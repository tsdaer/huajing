//! 世界主线与世界时钟（M3.7 · 设计 §6.6）：世界作用域的阶段弧与跨会话持久时钟。
//!
//! 有的世界自带"大势"：图书馆月底拆除、七天后祭典——它不属于任何角色，却压着所有角色。
//! 设计上**不造新引擎**：世界主线 = 世界作用域的状态树（§7 引擎，与 M3.6 导演树同一套
//! 机制，[`card::worldline_shape`] 把设计 §6.6 的 `stages` 列表糖归一成 `state_tree`）+
//! 世界级剧情线（§8 模型，scope=world）。本模块只承载纯数据与算法：
//!
//! - [`Worldline`]：一条已加载的世界主线（树 + 元信息 + 归一化后的 Lua 源）；
//! - [`WorldState`]：`codex/<世界>/world.json` 的正文——世界时钟、主线进度、世界级线。
//!   它是世界级单例（跨会话持久），**不属于任何会话的事件流**：会话流只记「本会话
//!   见证的走位」（[`event::WorldlineEvent`]），轮末由宿主把 max 语义回写进世界；
//! - [`sync`]：会话 → 世界的纯合并（时钟不回退、进度只前进、线按 id upsert），
//!   「多线并行不回退、flashback 不拉低」是这条合并规则的结构性质，不是提示词恳求。
//!
//! 纪律与 scene.rs/director.rs 相同：不碰文件不碰网络，单测直接钉住；
//! 文件读写归 store.rs（load_world/save_world），Lua 归 card.rs 沙箱。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::statetree::StateTree;

/// era 行里 directive 摘要的最大长度（B1 是 80 token 的紧凑现状卡，时代行只留一句）
const ERA_CLIP: usize = 48;

/// 一条已加载的世界主线（设计 §6.6 的 Lua 声明 + 归一化产物）。
///
/// `source` 是**归一化后**的 Lua（stages 糖 → state_tree，见 [`card::worldline_shape`]）——
/// 转移求值（`eval_state_tree`）与阶段钩子（`run_worldline_hook`）共用它，
/// 与导演树的「加载一次、处处求值」同构。
#[derive(Debug, Clone)]
pub struct Worldline {
    pub id: String,
    /// 世界级起因（「老图书馆月底拆除，所有人都在倒数」）——新会话三问的世界版
    pub premise: String,
    pub tree: StateTree,
    /// 声明里的 world_threads（世界级线的缺省标题清单；实际线以事件流为准）
    pub world_threads: Vec<String>,
    pub source: String,
}

impl Worldline {
    /// 从 [`card::worldline_shape`] 的产物构造。树解析失败报错（调用方回落「无主线」）。
    pub fn from_shape(shape: &serde_json::Value, source: String) -> Result<Worldline, String> {
        let obj = shape
            .as_object()
            .ok_or_else(|| "worldline 形状必须是对象".to_string())?;
        let tree_v = obj
            .get("state_tree")
            .ok_or_else(|| "worldline 缺少阶段弧（stages / state_tree）".to_string())?;
        let tree = StateTree::from_value(tree_v)?;
        Ok(Worldline {
            id: obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("worldline")
                .to_string(),
            premise: obj
                .get("premise")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            world_threads: obj
                .get("world_threads")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            tree,
            source,
        })
    }

    /// 当前阶段（活跃路径的叶；空路径回树根）
    pub fn stage_of(&self, path: &[String]) -> String {
        path.last()
            .cloned()
            .unwrap_or_else(|| self.tree.root.clone())
    }
}

/// B1 时代行（设计 §6.6「注入」）：「公告期——公告已贴出，空气里有告别的味道」。
/// 取**叶阶段**自己的 directive（最贴近现状的一段），截断到紧凑现状卡承受得起的长度；
/// 叶没写 directive 时只报阶段名。
pub fn era_line(tree: &StateTree, path: &[String]) -> Option<String> {
    let leaf = path.last()?;
    let directive = tree.states.get(leaf).and_then(|n| n.directive.as_deref());
    let summary = directive.map(str::trim).filter(|l| !l.is_empty());
    match summary {
        Some(line) => Some(format!("{leaf}——{}", clip(line, ERA_CLIP))),
        None => Some(leaf.clone()),
    }
}

fn clip(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

/// 阶段进度比较：路径 `a` 是否比 `b` 走得更远。
///
/// 世界主线的本意是**线性阶段弧**（传闻期 → 公告期 → 最后一夜），路径更深 = 更靠后；
/// 等深不前进（同一天的两次推进不构成「更进一步」）。自定义树若开了支线，
/// 深度仍是稳定口径——宁可少进一格，不让世界进度来回横跳。
pub fn progress_deeper(a: &[String], b: &[String]) -> bool {
    a.len() > b.len()
}

/// 世界主线的持久进度（world.json 的 `worldline` 槽）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldlineProgress {
    /// 主线 id（换主线后旧进度作废，见 [`sync`]）
    pub id: String,
    /// 活跃路径（根→叶）
    pub path: Vec<String>,
    /// 最近一次推进发生的会话轮次（溯源用）
    pub advanced_turn: u64,
    /// 最近一次推进的会话（溯源用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_in: Option<String>,
}

/// 世界状态：`codex/<世界>/world.json` 的正文（设计 §12「世界元信息」的运行时槽位）。
///
/// 世界时钟 `day` 是世界级单例：各会话开局读取为基准、会话内自行推进、
/// 轮末回写 `max(世界, 本会话)`——多线并行不回退，flashback 会话不拉低它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    /// 世界时钟（故事天）。缺省 1（与建会话缺省一致——没有 world.json 时行为不变）。
    /// 必须带 default：仓库示例与玩家手写的 world.json 可以只有元信息没有时钟，
    /// 解析失败会让 load_world 整体退回缺省，轮末回写把元信息一并抹掉
    #[serde(default = "default_day")]
    pub day: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// 主线进度（没有主线 / 主线未推进 = None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worldline: Option<WorldlineProgress>,
    /// 世界级线快照（scope=world 的 Thread；任何会话可推进，按 id upsert）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// 未知键原样保留（玩家手写的元信息不因回写丢失）
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_day() -> i64 {
    1
}

impl Default for WorldState {
    fn default() -> Self {
        WorldState {
            day: default_day(),
            tone: None,
            style: None,
            worldline: None,
            threads: Vec::new(),
            updated_at: None,
            updated_by: None,
            extra: BTreeMap::new(),
        }
    }
}

impl WorldState {
    /// 世界级线里有没有这条（按 id）
    pub fn has_thread(&self, id: &str) -> bool {
        self.threads
            .iter()
            .any(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
    }
}

/// 会话 → 世界的纯合并（轮末回写的内核，`max` 语义全在这里）：
///
/// - **时钟不回退**：`day` 只在更大时采纳——flashback 会话拉不低世界；
/// - **进度只前进**：主线 id 一致且路径更深才替换（换主线 = 旧进度作废，重新起弧）；
/// - **线按 id upsert**：会话侧的 scope=world 线快照合并进世界（新的追加、已有的
///   以会话侧为准——它刚被这个会话推进过）；返回是否发生了任何变化。
pub fn sync(
    world: &mut WorldState,
    session_id: &str,
    day: i64,
    progress: Option<WorldlineProgress>,
    world_threads: &[serde_json::Value],
    now: u64,
) -> bool {
    let mut changed = false;
    if day > world.day {
        world.day = day;
        changed = true;
    }
    if let Some(prog) = progress {
        let ahead = match &world.worldline {
            // 同一条主线且走得更远才前进；换主线（id 不同）直接重置
            Some(cur) => cur.id != prog.id || progress_deeper(&prog.path, &cur.path),
            None => true,
        };
        if ahead {
            world.worldline = Some(prog);
            changed = true;
        }
    }
    for t in world_threads {
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        match world.threads.iter().position(|x| {
            x.get("id").and_then(|v| v.as_str()) == Some(id)
        }) {
            Some(at) => {
                if &world.threads[at] != t {
                    world.threads[at] = t.clone();
                    changed = true;
                }
            }
            None => {
                world.threads.push(t.clone());
                changed = true;
            }
        }
    }
    if changed {
        world.updated_at = Some(now);
        world.updated_by = Some(session_id.to_string());
    }
    changed
}

/// 世界主线阶段钩子产出的动作（设计 §6.6「阶段转移可 reveal 世界设定 + 开世界级线」）。
/// 由 card.rs 的 worldline 沙箱 api 收集，宿主逐条执行落事件——
/// 只有这两个动作（没有 state/memory/blackboard）：主线是世界层，不碰任何角色的私有状态。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum WorldlineAction {
    /// 揭示世界设定（无见证者 = 全局知情——大势对所有人可见）
    Reveal { targets: Vec<String> },
    /// 开世界级线（scope=world；id 缺省由标题生成，title 缺省取阶段名）
    OpenThread {
        id: Option<String>,
        title: Option<String>,
        cause: Option<String>,
        importance: Option<f32>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree_with(stages: &[&str]) -> StateTree {
        // 线性阶段弧的最小树：root=stages[0]，逐级 parent 链
        let states: serde_json::Map<String, serde_json::Value> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut node = serde_json::Map::new();
                if i > 0 {
                    node.insert("parent".into(), json!(stages[i - 1]));
                }
                node.insert("directive".into(), json!(format!("{s}的调子。")));
                (s.to_string(), serde_json::Value::Object(node))
            })
            .collect();
        StateTree::from_value(&json!({ "root": stages[0], "states": states })).expect("测试树应能解析")
    }

    #[test]
    fn era_line_joins_stage_with_directive_summary() {
        let tree = tree_with(&["传闻期", "公告期"]);
        let path = vec!["传闻期".to_string(), "公告期".to_string()];
        let era = era_line(&tree, &path).expect("有路径就有时代行");
        assert!(era.starts_with("公告期——"), "时代行以阶段名开头：{era}");
        assert!(era.contains("公告期的调子"), "带 directive 摘要：{era}");

        // 没有 directive 的状态只报阶段名
        let bare = StateTree::from_value(&json!({
            "root": "日常",
            "states": { "日常": {} }
        }))
        .unwrap();
        assert_eq!(era_line(&bare, &["日常".to_string()]).as_deref(), Some("日常"));
        // 空路径 = 没有时代行（调用方回落为无 worldline 的形态）
        assert_eq!(era_line(&tree, &[]), None);
    }

    #[test]
    fn era_line_clips_long_directives() {
        let long = "很".repeat(120);
        let tree = StateTree::from_value(&json!({
            "root": "公告期",
            "states": { "公告期": { "directive": long } }
        }))
        .unwrap();
        let era = era_line(&tree, &["公告期".to_string()]).unwrap();
        assert!(era.chars().count() < 60, "超长 directive 被截断：{}", era.chars().count());
        assert!(era.ends_with("…"));
    }

    #[test]
    fn progress_deeper_means_a_longer_path() {
        let a = vec!["传闻期".to_string()];
        let b = vec!["传闻期".to_string(), "公告期".to_string()];
        assert!(progress_deeper(&b, &a));
        assert!(!progress_deeper(&a, &b));
        assert!(!progress_deeper(&a, &a), "等深不前进");
    }

    #[test]
    fn sync_never_regresses_the_world_clock() {
        let mut world = WorldState::default();
        // 会话推进到第 15 天
        assert!(sync(&mut world, "s1", 15, None, &[], 100));
        assert_eq!(world.day, 15);
        // flashback 会话从第 3 天结束——世界不被拉低
        assert!(!sync(&mut world, "s2", 3, None, &[], 101));
        assert_eq!(world.day, 15);
        assert_eq!(world.updated_by.as_deref(), Some("s1"), "无变化不盖章");
    }

    /// 仓库示例与玩家手写的 world.json 只有元信息没有时钟字段（day 缺省）——
    /// 解析失败会让 load_world 静默退回缺省，轮末回写把元信息一并抹掉
    /// （实锤过 world.json 被写剩三个时钟字段的缺陷）。day 必须带 serde default。
    #[test]
    fn hand_written_world_metadata_survives_round_end_write_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("codex").join("default")).unwrap();
        crate::store::atomic_write(
            &crate::store::world_path(dir.path(), "default"),
            r#"{ "spec": "world/1.0", "name": "default", "tone": "现代都市 · 温柔日常" }"#.as_bytes(),
        )
        .unwrap();
        let mut world = crate::store::load_world(dir.path(), "default");
        assert_eq!(
            world.tone.as_deref(),
            Some("现代都市 · 温柔日常"),
            "缺 day 的手写文件要读得进来"
        );
        // 轮末回写（sync + save）后元信息原样保留，时钟字段补上
        assert!(sync(&mut world, "s1", 5, None, &[], 100));
        crate::store::save_world(dir.path(), "default", &world).unwrap();
        let reread = crate::store::load_world(dir.path(), "default");
        assert_eq!(reread.day, 5);
        assert_eq!(reread.tone.as_deref(), Some("现代都市 · 温柔日常"));
        assert_eq!(
            reread.extra.get("spec").and_then(|v| v.as_str()),
            Some("world/1.0"),
            "未知键经 flatten 原样保留"
        );
        assert_eq!(reread.extra.get("name").and_then(|v| v.as_str()), Some("default"));
    }

    #[test]
    fn sync_advances_progress_only_forward_and_resets_on_new_worldline() {
        let mut world = WorldState::default();
        let early = WorldlineProgress {
            id: "wl".into(),
            path: vec!["传闻期".into()],
            advanced_turn: 3,
            advanced_in: Some("s1".into()),
        };
        let later = WorldlineProgress {
            id: "wl".into(),
            path: vec!["传闻期".into(), "公告期".into()],
            advanced_turn: 9,
            advanced_in: Some("s1".into()),
        };
        assert!(sync(&mut world, "s1", 1, Some(early.clone()), &[], 100));
        // 更浅的进度（另一个并行会话还在传闻期、也没把世界时钟往前推）不覆盖
        assert!(!sync(&mut world, "s2", 1, Some(early), &[], 101));
        assert_eq!(world.worldline.as_ref().unwrap().path, vec!["传闻期".to_string()]);
        // 更深的进度前进
        assert!(sync(&mut world, "s1", 2, Some(later.clone()), &[], 102));
        assert_eq!(world.worldline.as_ref().unwrap().path, later.path);
        // 换主线：旧进度作废重新起弧
        let fresh = WorldlineProgress {
            id: "wl2".into(),
            path: vec!["序幕".into()],
            advanced_turn: 1,
            advanced_in: Some("s3".into()),
        };
        assert!(sync(&mut world, "s3", 2, Some(fresh.clone()), &[], 103));
        assert_eq!(world.worldline.as_ref().unwrap().id, "wl2");
    }

    #[test]
    fn sync_upserts_world_threads_by_id() {
        let mut world = WorldState::default();
        let t1 = json!({ "id": "thread.最后一个月", "title": "最后一个月", "state": "active" });
        assert!(sync(&mut world, "s1", 1, None, std::slice::from_ref(&t1), 100));
        // 同 id 推进（收线）以会话侧为准
        let t1_resolved = json!({ "id": "thread.最后一个月", "title": "最后一个月", "state": "resolved" });
        assert!(sync(&mut world, "s1", 1, None, std::slice::from_ref(&t1_resolved), 101));
        assert_eq!(world.threads.len(), 1);
        assert_eq!(world.threads[0]["state"], json!("resolved"));
        // 另一条线追加
        let t2 = json!({ "id": "thread.祭典", "title": "祭典", "state": "active" });
        assert!(sync(&mut world, "s2", 1, None, &[t1_resolved.clone(), t2.clone()], 102));
        assert_eq!(world.threads.len(), 2);
        // 无变化不重写
        assert!(!sync(&mut world, "s2", 1, None, &[t1_resolved, t2], 103));
    }

    #[test]
    fn world_state_roundtrips_and_keeps_unknown_keys() {
        let v = json!({
            "day": 20,
            "tone": "日常与告别",
            "worldline": { "id": "wl", "path": ["传闻期", "公告期"], "advanced_turn": 4 },
            "threads": [{ "id": "thread.最后一个月" }],
            "作者备注": "手写字段不丢"
        });
        let mut world: WorldState = serde_json::from_value(v).unwrap();
        assert_eq!(world.day, 20);
        assert!(world.has_thread("thread.最后一个月"));
        assert!(!world.has_thread("thread.别的"));
        assert!(sync(&mut world, "s9", 25, None, &[], 999));
        let out: serde_json::Value = serde_json::to_value(&world).unwrap();
        assert_eq!(out["作者备注"], json!("手写字段不丢"), "flatten 保留未知键");
        assert_eq!(out["day"], json!(25));
    }

    #[test]
    fn worldline_parses_shape_with_meta_and_tree() {
        let shape = json!({
            "id": "worldline.图书馆拆迁",
            "premise": "老图书馆月底拆除。",
            "world_threads": ["thread.最后一个月"],
            "state_tree": {
                "root": "传闻期",
                "states": {
                    "传闻期": { "directive": "日常氛围。" },
                    "公告期": { "parent": "传闻期", "directive": "公告已贴出。" }
                }
            }
        });
        let wl = Worldline::from_shape(&shape, "return {}".into()).expect("应能解析");
        assert_eq!(wl.id, "worldline.图书馆拆迁");
        assert_eq!(wl.premise, "老图书馆月底拆除。");
        assert_eq!(wl.world_threads, vec!["thread.最后一个月".to_string()]);
        assert_eq!(wl.stage_of(&[]), "传闻期", "空路径回树根");
        assert_eq!(wl.stage_of(&["传闻期".into(), "公告期".into()]), "公告期");
    }
}
