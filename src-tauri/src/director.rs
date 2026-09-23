//! 导演调度（M3.4 · 设计 §10.5）：群聊的发言权打分与发言计划。
//!
//! 导演是**元层**：它只决定「这一轮谁说话、按什么顺序」，输出是调度动作，
//! 绝不进入任何角色的上下文（设计 §10.5）。打分是宿主侧**确定性算法**——
//! 同样的输入必然得到同样的发言计划，LLM 导演只是未来的可选策略源（M3.6）。
//!
//! 打分信号（设计 §10.5「相关性打分」）：
//! - **最近提及**：本轮输入点到名（强信号）或最近窗口里被谈到；
//! - **场景黑板关联**：在黑板的在场名单上；
//! - **活跃线关联**：是某条活跃剧情线的 actor（线正被谈到时再加码）；
//! - **`want_to_speak` 投票**：角色「想说话」的程度（M3.4 来自意图强度，
//!   M3.5 的 `api.schedule_say` 主动队列接进来后同槽加权）；
//! - **冷却**（防独占）：刚说过话的扣分——三信号都平平时让没说过话的人接话，
//!   这是「不冷场、不打架」的机制保证，不是提示词恳求。
//!
//! 纪律与 scene.rs 相同：纯 Rust 数据与算法，不碰文件不碰网络，单测直接钉住。

/// 打分权重与窗口（集中成常量，调参不改逻辑；调用方按窗口预切片）
pub mod weights {
    /// 本轮输入直接点名（提到名字）。
    pub const MENTION: f32 = 3.0;
    /// 最近窗口里被谈到（弱一档的提及）。
    pub const MENTION_RECENT: f32 = 1.0;
    /// 最近提及往前数几条消息。
    pub const RECENT_WINDOW: usize = 4;
    /// 在黑板在场名单上。
    pub const STAGE: f32 = 1.0;
    /// 是活跃线的 actor。
    pub const THREAD: f32 = 0.8;
    /// 线正被本轮输入谈到（线 actor 且线的关键词命中）。
    pub const THREAD_HIT: f32 = 1.5;
    /// want_to_speak 投票的满额加成（实际 = VOTE × 票数 0..1）。
    pub const VOTE: f32 = 2.0;
    /// 冷却：最近窗口里每说过一次话扣这么多。
    pub const COOLDOWN: f32 = 1.2;
    /// 冷却往前数几条角色回复。
    pub const COOLDOWN_SPAN: usize = 3;
}

/// 一名候选发言人：目录名（会话内唯一）+ 展示名（提及匹配与理由都用它）。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub dir: String,
    pub name: String,
}

/// 发言权打分里用到的剧情线切片（从投影的线快照解析，commands.rs 负责）。
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRef {
    pub title: String,
    pub actors: Vec<String>,
    pub importance: f32,
    /// 线的话题窗口词（resurface 的 mention 窗口；空 = 没有话题词可命中）。
    pub mention_words: Vec<String>,
}

impl ThreadRef {
    /// 本轮输入是否正谈到这条线（actor 视角之外的「线被谈到」判据）。
    fn hit_by(&self, text: &str) -> bool {
        let t = normalize(text);
        self.mention_words
            .iter()
            .any(|w| !w.trim().is_empty() && t.contains(&normalize(w)))
    }
}

/// 一名候选的打分结果：分数 + 逐条理由（导演面板「为何轮到她」的数据源）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Pick {
    pub dir: String,
    pub name: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

/// 一次发言权调度的全部输入（只读快照，与 ThreadQuery 同风格）。
pub struct SpeechQuery<'a> {
    /// 全阵容（会话顺序 = 同分的稳定次序）。
    pub candidates: &'a [Candidate],
    /// 本场景在场者（目录名）。冻结场景的成员没有发言权——不在名单里就轮不到。
    pub present: &'a [String],
    /// 本轮用户输入（点名提及的主要来源）。
    pub content: &'a str,
    /// 本场景的消息内容（旧→新；「最近被谈到」在内部只看最近 [`weights::RECENT_WINDOW`] 条）。
    pub recent: &'a [String],
    /// 场景黑板的在场名单（展示名或目录名皆可，与黑板编辑习惯一致）。
    pub board_actors: &'a [String],
    /// 活跃剧情线（已按 state=active 过滤）。
    pub threads: &'a [ThreadRef],
    /// want_to_speak 投票（目录名 → 0..=1，超出按 1 封顶）。
    pub votes: &'a [(String, f32)],
    /// char 发言人（目录名，新→旧；冷却在内部只看最近 [`weights::COOLDOWN_SPAN`] 条）。
    pub recent_speakers: &'a [String],
    /// 本轮最多几人发言（每轮发言数可配，默认 1–2）。
    pub max_speakers: usize,
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 名字是否出现在文本里（拉丁大小写不敏感；CJK 按子串精确匹配）。
fn mentions(text: &str, name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && normalize(text).contains(&normalize(name))
}

/// 发言权打分与发言计划（设计 §10.5）。
///
/// 返回按发言顺序排列的 [`Pick`]，至多 `max_speakers` 人、互不重复，
/// 且全部在 `present` 里——「不打架」（不会同人占两条）与「不越界」
/// （冻结场景的人不说话）都是结构保证。
/// **不冷场**：即使所有信号平平（全员 0 分），照样按稳定次序产出计划——
/// 导演永不沉默；冷却扣分让刚说完话的人自然排到没说过话的人后面。
pub fn plan_speakers(q: &SpeechQuery) -> Vec<Pick> {
    let content = q.content;
    let mut scored: Vec<Pick> = q
        .candidates
        .iter()
        .filter(|c| q.present.iter().any(|p| p == &c.dir))
        .map(|c| {
            let mut score = 0.0;
            let mut reasons: Vec<String> = Vec::new();

            // 最近提及：本轮点名 > 最近窗口被谈到
            if mentions(content, &c.name) || mentions(content, &c.dir) {
                score += weights::MENTION;
                reasons.push("被点名提及".into());
            } else if q
                .recent
                .iter()
                .rev()
                .take(weights::RECENT_WINDOW)
                .any(|m| mentions(m, &c.name) || mentions(m, &c.dir))
            {
                score += weights::MENTION_RECENT;
                reasons.push("最近被谈到".into());
            }

            // 场景黑板关联
            if q
                .board_actors
                .iter()
                .any(|a| a == &c.name || a == &c.dir)
            {
                score += weights::STAGE;
                reasons.push("在黑板的在场名单".into());
            }

            // 活跃线关联：取关联最强的一条（不叠多条线，避免堆分打架）
            let mut best_thread: Option<(f32, String)> = None;
            for t in q.threads {
                if !t.actors.iter().any(|a| a == &c.dir || a == &c.name) {
                    continue;
                }
                let mut s = weights::THREAD;
                let mut why = format!("剧情线「{}」涉及", t.title);
                if t.hit_by(content) {
                    s += weights::THREAD_HIT + t.importance;
                    why = format!("剧情线「{}」正被谈到", t.title);
                }
                if best_thread.as_ref().map(|(b, _)| s > *b).unwrap_or(true) {
                    best_thread = Some((s, why));
                }
            }
            if let Some((s, why)) = best_thread {
                score += s;
                reasons.push(why);
            }

            // want_to_speak 投票
            let vote = q
                .votes
                .iter()
                .filter(|(d, _)| d == &c.dir)
                .map(|(_, v)| v.clamp(0.0, 1.0))
                .fold(0.0f32, f32::max);
            if vote > 0.0 {
                score += weights::VOTE * vote;
                reasons.push("想说话（投票）".into());
            }

            // 冷却（防独占）：最近窗口里每说过一次扣一档
            let spoke = q
                .recent_speakers
                .iter()
                .take(weights::COOLDOWN_SPAN)
                .filter(|d| *d == &c.dir)
                .count();
            if spoke > 0 {
                score -= weights::COOLDOWN * spoke as f32;
                reasons.push("刚说过话（冷却）".into());
            }

            Pick {
                dir: c.dir.clone(),
                name: c.name.clone(),
                score,
                reasons,
            }
        })
        .collect();

    // 排序：分高在前；同分按「更久没说话」在前；再同按阵容顺序（确定性收尾）
    let order = |dir: &str| q.candidates.iter().position(|c| c.dir == dir).unwrap_or(0);
    let spoke_rank = |dir: &str| {
        q.recent_speakers
            .iter()
            .take(weights::COOLDOWN_SPAN)
            .position(|d| d == dir)
            .unwrap_or(usize::MAX)
    };
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(spoke_rank(&a.dir).cmp(&spoke_rank(&b.dir)))
            .then(order(&a.dir).cmp(&order(&b.dir)))
    });
    scored.truncate(q.max_speakers.max(1));
    scored
}

// ---------- 剧场模式与导演树（M3.6 · 设计 §8.5/§10.5）----------

/// 导演树钩子（on_enter / on_exit）产出的**调度动作**（设计 §10.5「输出只产生调度动作」）。
///
/// 动作由 card.rs 的导演沙箱 api 收集，宿主（commands.rs 的 advance_theater）逐条执行并
/// 落成事件：开/收线 → ThreadEvent（origin=director）、resurface → ThreadEvent（op=retune）、
/// 合场 → SceneEvent（origin=director）。全部是元层动作，绝不进入任何角色的上下文。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum DirectorAction {
    /// 开线（起 / 转）。字段全部可选——缺省由宿主补：标题取在场者最强的未外化意图，
    /// 没有意图用「主线」；在场者 = 当前场景成员；重要度缺省 0.7
    OpenThread {
        title: Option<String>,
        cause: Option<String>,
        actors: Option<Vec<String>>,
        importance: Option<f32>,
    },
    /// 收线（合）。`thread_id` 为空 = 收束**导演自己开的**全部活跃线
    /// （不动管线/心理外化的线——那些有自己的生命周期）
    ResolveThreads {
        thread_id: Option<String>,
        outcome: Option<String>,
    },
    /// 窗口调度权（设计 §8.5「提前/延后 resurface 作为节奏工具」）：
    /// earlier = grade 升到 eager（角色很想找机会说），later = 降到 dormant（暂不进窗口）
    Resurface { thread_id: String, direction: String },
    /// 合场裁决：其余活跃场景并入聚焦场景（交叉剪辑的收束拍）
    MergeScenes,
}

/// 默认导演树（M3.6 内置）：起承转合四段控一条完整的开线→收线弧。
///
/// 会话目录没有 director.lua 时用它——剧场模式开箱即跑。判据全部来自宿主注入的
/// 合成 state 表：`st.turns_left`（预算余量）/ `st.stage_turns`（本段已走轮数）/
/// `st.threads_active`（活跃线 id 列表）。预算压力（priority 90）保证小预算也能走完弧：
/// 目标函数「限定轮数内完成完整的开线→收线弧」由 `turns_left` 阈值兑现。
pub const DEFAULT_DIRECTOR_LUA: &str = r#"
-- 默认导演树：起承转合（M3.6 内置；会话目录放 director.lua 可整树替换）
return {
  state_tree = {
    root = "起",
    states = {
      ["起"] = {
        directive = "起：铺陈日常与人物，让张力自然登场。",
        has_enter = true,
        on_enter = function(api)
          -- 开一条主线：标题缺省取在场者最强的未外化意图
          api.open_thread {}
        end,
        transitions = {
          { to = "承", priority = 10,
            when = function(ev, bb, st)
              return st.stage_turns >= 3 and #st.threads_active > 0
            end },
          { to = "承", priority = 90,
            when = function(ev, bb, st) return st.turns_left <= 6 end },
        },
      },
      ["承"] = {
        directive = "承：让线在对话里生长，铺垫但不急收。",
        transitions = {
          { to = "转", priority = 10,
            when = function(ev, bb, st) return st.stage_turns >= 5 end },
          { to = "转", priority = 90,
            when = function(ev, bb, st) return st.turns_left <= 4 end },
        },
      },
      ["转"] = {
        directive = "转：主动制造反转，一条新线搅进局面。",
        has_enter = true,
        on_enter = function(api)
          api.open_thread {
            title = "意外",
            cause = "剧场转段：突如其来的变数搅进局面",
            importance = 0.8,
          }
        end,
        transitions = {
          { to = "合", priority = 10,
            when = function(ev, bb, st) return st.stage_turns >= 3 end },
          { to = "合", priority = 90,
            when = function(ev, bb, st) return st.turns_left <= 3 end },
        },
      },
      ["合"] = {
        directive = "合：收束各线，场景归一，余韵收尾。",
        has_enter = true,
        on_enter = function(api)
          -- 先并场再收线：大家回到同一舞台把话说完
          api.merge_scenes()
          api.resolve_threads(nil, "剧场收束：剧情走到了合的段落，各条线有了交代。")
        end,
      },
    },
  },
}
"#;

/// 交叉剪辑的节奏常量：同一场景连续推进 [`INTERCUT_CADENCE`] 轮后换下一路（设计 §10.5）。
/// 多场景轮换让「与此同时」的两路都有戏份；进入「合」段合场后只剩一路，轮换自然停止。
pub const INTERCUT_CADENCE: u32 = 3;

/// 交叉剪辑（intercut）的打分输入（设计 §10.5「多场景间自动切换推进」）。
pub struct IntercutQuery<'a> {
    /// 可推进的场景（调用方已过滤归档；冻结的分路算——切回即解冻），按折叠序
    pub scenes: &'a [String],
    /// 当前聚焦场景
    pub current: &'a str,
    /// 当前场景已连续推进的剧场轮数
    pub rounds_in_current: u32,
    /// 连续推进多少轮后切场（会话级节奏旋钮；传 0 = 用缺省 [`INTERCUT_CADENCE`]）
    pub cadence: u32,
}

/// 交叉剪辑的一步决策：留在当前场景，或切到下一路。
///
/// 纯函数（与 plan_speakers 同纪律）：同样的输入必得同样的决策。轮换按可推进场景的
/// 折叠序循环（A→B→A→B…），保证每一路都有固定戏份；合场不由这里决定——
/// 它是导演树「合」段的显式动作（[`DirectorAction::MergeScenes`]），时机由树掌管。
pub fn plan_cut(q: &IntercutQuery) -> CutDecision {
    let cadence = if q.cadence == 0 { INTERCUT_CADENCE } else { q.cadence };
    if q.scenes.len() <= 1 || q.rounds_in_current < cadence {
        return CutDecision::Stay;
    }
    // 轮换：当前场景之后的第一路（循环）；找不到（异常输入）就留守
    let at = q.scenes.iter().position(|s| s == q.current);
    let n = q.scenes.len();
    for step in 1..=n {
        if let Some(to) = at.map(|i| q.scenes[(i + step) % n].clone()) {
            if to != q.current {
                return CutDecision::Cut { to };
            }
        }
    }
    CutDecision::Stay
}

/// 交叉剪辑的一步决策结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutDecision {
    Stay,
    Cut { to: String },
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(dir: &str, name: &str) -> Candidate {
        Candidate {
            dir: dir.into(),
            name: name.into(),
        }
    }

    fn thread(title: &str, actors: &[&str], words: &[&str]) -> ThreadRef {
        ThreadRef {
            title: title.into(),
            actors: actors.iter().map(|s| s.to_string()).collect(),
            importance: 0.8,
            mention_words: words.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn plan(areas: &SpeechQuery) -> Vec<String> {
        plan_speakers(areas).iter().map(|p| p.dir.clone()).collect()
    }

    /// 三人阵容的公共底座
    fn trio() -> (Vec<Candidate>, Vec<String>) {
        (
            vec![cand("ayaka", "绫"), cand("xiaoyu", "小雨"), cand("ache", "阿澈")],
            vec!["ayaka".into(), "xiaoyu".into(), "ache".into()],
        )
    }

    #[test]
    fn mention_beats_cooldown_and_rotates_the_rest() {
        let (cands, present) = trio();
        // 绫刚连说过话（冷却），但这轮用户点了她的名——点名是强信号，压过冷却
        let q = SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "绫，你怎么看？",
            recent: &[],
            board_actors: &[],
            threads: &[],
            votes: &[],
            recent_speakers: &["ayaka".into(), "ayaka".into()],
            max_speakers: 2,
        };
        let picks = plan_speakers(&q);
        assert_eq!(picks[0].dir, "ayaka");
        assert!(picks[0].reasons.iter().any(|r| r.contains("点名")));
        assert!(picks[0].reasons.iter().any(|r| r.contains("冷却")));
        // 第二个发言权给到没被冷却的小雨/阿澈（不打架：只取一人，且不是绫）
        assert_eq!(picks.len(), 2);
        assert_ne!(picks[1].dir, "ayaka");
        assert!(picks[1].score >= 0.0);
    }

    #[test]
    fn all_calm_floor_rotates_to_the_quiet_ones() {
        let (cands, present) = trio();
        // 没有任何信号、绫刚说过话 → 不冷场：仍有人接话，且不是刚说过的绫
        let q = SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "（大家继续）",
            recent: &[],
            board_actors: &[],
            threads: &[],
            votes: &[],
            recent_speakers: &["ayaka".into()],
            max_speakers: 1,
        };
        assert_eq!(plan(&q), vec!["xiaoyu".to_string()]);
    }

    #[test]
    fn thread_relevance_and_votes_rank_the_floor() {
        let (cands, present) = trio();
        let threads = vec![thread("周五还书", &["xiaoyu"], &["还书", "借书卡"])];
        // 阿澈有意向投票，小雨被线关联 + 线正被谈到 → 小雨压过阿澈
        let q = SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "说到还书这件事……",
            recent: &[],
            board_actors: &[],
            threads: &threads,
            votes: &[("ache".into(), 1.0)],
            recent_speakers: &[],
            max_speakers: 2,
        };
        let picks = plan_speakers(&q);
        assert_eq!(picks[0].dir, "xiaoyu");
        assert!(picks[0]
            .reasons
            .iter()
            .any(|r| r.contains("正被谈到")));
        assert_eq!(picks[1].dir, "ache");
        assert!(picks[1].reasons.iter().any(|r| r.contains("想说话")));
        // 线 actor 但线没被谈到：只有基础关联分，压不过投票
        let q2 = SpeechQuery {
            content: "（换个话题）",
            threads: &threads,
            votes: &[("ache".into(), 1.0)],
            recent_speakers: &[],
            max_speakers: 1,
            ..q
        };
        assert_eq!(plan(&q2), vec!["ache".to_string()]);
    }

    #[test]
    fn frozen_scene_members_never_get_the_floor() {
        let (cands, _) = trio();
        // 阿澈被切走的场景冻结（不在 present）——分数再高也不给发言权
        let present = vec!["ayaka".to_string(), "xiaoyu".to_string()];
        let q = SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "阿澈，来聊聊？",
            recent: &[],
            board_actors: &[],
            threads: &[],
            votes: &[],
            recent_speakers: &[],
            max_speakers: 3,
        };
        let picks = plan_speakers(&q);
        assert!(picks.iter().all(|p| p.dir != "ache"));
        assert_eq!(picks.len(), 2);
    }

    #[test]
    fn plan_is_deterministic_capped_and_distinct() {
        let (cands, present) = trio();
        let recent = vec!["小雨说起了图书馆".to_string()];
        let board_actors = vec!["小雨".to_string()];
        let threads = vec![thread("夜谈", &["ache", "ayaka"], &["夜谈"])];
        let votes = vec![
            ("ayaka".to_string(), 0.6),
            ("ache".to_string(), 0.4),
        ];
        let recent_speakers = vec!["xiaoyu".to_string()];
        let mk = || SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "小雨 阿澈 绫 都说说",
            recent: &recent,
            board_actors: &board_actors,
            threads: &threads,
            votes: &votes,
            recent_speakers: &recent_speakers,
            max_speakers: 2,
        };
        let a = plan(&mk());
        let b = plan(&mk());
        assert_eq!(a, b, "同样的输入必然得到同样的计划");
        assert_eq!(a.len(), 2, "发言数按配置截断");
        let unique: std::collections::HashSet<&String> = a.iter().collect();
        assert_eq!(unique.len(), a.len(), "不会两人同抢一条发言权");
    }

    #[test]
    fn empty_signals_still_yield_a_speaker() {
        let (cands, present) = trio();
        let q = SpeechQuery {
            candidates: &cands,
            present: &present,
            content: "",
            recent: &[],
            board_actors: &[],
            threads: &[],
            votes: &[],
            recent_speakers: &[],
            max_speakers: 0, // 退化配置也至少给一人（不冷场的下限）
        };
        assert_eq!(plan(&q).len(), 1);
    }

    // ---------- M3.6 剧场模式与导演树 ----------

    use crate::statetree::StateTree;

    fn active_of(source: &str) -> Vec<String> {
        let tree = StateTree::from_value(&crate::card::state_tree_shape(source).unwrap())
            .expect("默认导演树应能解析");
        tree.active_path(&tree.root)
    }

    /// 剧场一步：在给定路径与环境下求值默认树，返回命中的转移目标（无则 None）
    fn step(path: &[&str], turns_left: i64, stage_turns: i64, threads: &[&str]) -> Option<String> {
        let active: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        let env = crate::card::TreeEnv {
            event: "theater:turn_end".into(),
            state: serde_json::json!({
                "turn": 10,
                "turns_left": turns_left,
                "stage_turns": stage_turns,
                "threads_active": threads,
            }),
            threads_active: threads.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        crate::card::eval_state_tree(DEFAULT_DIRECTOR_LUA, &active, &env)
            .expect("默认树求值不应失败")
            .map(|d| d.to)
    }

    #[test]
    fn default_director_tree_parses_and_walks_the_four_act_arc() {
        let tree = StateTree::from_value(&crate::card::state_tree_shape(DEFAULT_DIRECTOR_LUA).unwrap())
            .expect("默认导演树应能解析");
        assert!(tree.validate().is_empty(), "默认树不应有结构问题：{:?}", tree.validate());
        let path = tree.active_path(&tree.root);
        assert_eq!(path, vec!["起".to_string()]);

        // 起：线未开 / 轮数不足都留守；线已开且满三轮 → 承
        assert_eq!(step(&["起"], 20, 1, &[]), None);
        assert_eq!(step(&["起"], 20, 3, &["thread.主线"]), Some("承".into()));
        // 预算压力也能推（turns_left ≤ 6，即使线还没开）
        assert_eq!(step(&["起"], 6, 1, &[]), Some("承".into()));

        // 承：满五轮 → 转；预算紧 → 转
        assert_eq!(step(&["起", "承"], 20, 4, &["thread.主线"]), None);
        assert_eq!(step(&["起", "承"], 20, 5, &["thread.主线"]), Some("转".into()));
        assert_eq!(step(&["起", "承"], 4, 1, &[]), Some("转".into()));

        // 转：满三轮 → 合；预算见底 → 合
        assert_eq!(step(&["转"], 20, 2, &[]), None);
        assert_eq!(step(&["转"], 20, 3, &[]), Some("合".into()));
        assert_eq!(step(&["转"], 3, 1, &[]), Some("合".into()));

        // 合：终段，无转移（收束在 on_enter 里完成）
        assert_eq!(step(&["合"], 20, 9, &["thread.主线"]), None);
    }

    #[test]
    fn default_tree_hooks_declare_the_arc_actions() {
        // 起段进场开线、转段进场开反转线、合段进场并场收线——钩子的存在性是结构承诺
        assert!(crate::card::card_has_state_hook(DEFAULT_DIRECTOR_LUA, "起", "on_enter"));
        assert!(crate::card::card_has_state_hook(DEFAULT_DIRECTOR_LUA, "转", "on_enter"));
        assert!(crate::card::card_has_state_hook(DEFAULT_DIRECTOR_LUA, "合", "on_enter"));
        assert!(!crate::card::card_has_state_hook(DEFAULT_DIRECTOR_LUA, "承", "on_enter"));
    }

    #[test]
    fn intercut_rotates_scenes_after_cadence_and_stays_single_scene() {
        let scenes = vec!["scene.a".to_string(), "scene.b".to_string()];
        // 未满节奏轮数：留守
        let q = IntercutQuery {
            scenes: &scenes,
            current: "scene.a",
            rounds_in_current: INTERCUT_CADENCE - 1,
            cadence: 0,
        };
        assert_eq!(plan_cut(&q), CutDecision::Stay);
        // 满了：轮换到下一路
        let q = IntercutQuery { rounds_in_current: INTERCUT_CADENCE, ..q };
        assert_eq!(plan_cut(&q), CutDecision::Cut { to: "scene.b".into() });
        // 从 b 再轮换回 a（循环）
        let q = IntercutQuery { current: "scene.b", ..q };
        assert_eq!(plan_cut(&q), CutDecision::Cut { to: "scene.a".into() });
        // 单场景 / 空场景：无交叉剪辑可言
        let solo = vec!["scene.a".to_string()];
        let q = IntercutQuery { scenes: &solo, ..q };
        assert_eq!(plan_cut(&q), CutDecision::Stay);
        let none: Vec<String> = Vec::new();
        let q = IntercutQuery { scenes: &none, ..q };
        assert_eq!(plan_cut(&q), CutDecision::Stay);
        // 会话级节奏旋钮（cadence 覆盖）
        let q = IntercutQuery { rounds_in_current: 2, cadence: 2, ..q };
        let scenes2 = vec!["scene.a".to_string(), "scene.b".to_string()];
        let q = IntercutQuery { scenes: &scenes2, current: "scene.a", ..q };
        assert_eq!(plan_cut(&q), CutDecision::Cut { to: "scene.b".into() });
    }

    #[test]
    fn active_of_reads_the_default_tree_root() {
        assert_eq!(active_of(DEFAULT_DIRECTOR_LUA), vec!["起".to_string()]);
        // 自定义树（会话模板覆盖缺省）：同样的机制读 root
        let custom = r#"return { state_tree = { root = "开端", states = { ["开端"] = {} } } }"#;
        assert_eq!(active_of(custom), vec!["开端".to_string()]);
    }
}
