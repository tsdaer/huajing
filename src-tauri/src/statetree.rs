//! 状态树（M2.3 · 设计 §7）：剧情状态机的**纯数据与算法**。
//!
//! 分工（M2 决断 3：Lua 只在 card.rs 的沙箱里跑）：卡内 `state_tree` 的解析（含 `when` 函数）
//! 与转移求值在 card.rs 的沙箱里做（`card::eval_state_tree`）；本模块只承载「宿主与界面
//! 都要的那份结构」——层级、directive、recall/reveal、声明式转移（to/priority/when 字符串）——
//! 不碰 Lua、不碰文件、不碰网络，因而可被单测直接钉住（M2.3 验收：转移是事件的纯函数）。
//!
//! 与设计 §7.3 的对应：
//! - §7.3-1 事件落地 → 收集当前叶状态自身的转移 + 沿 parent 链从祖先继承的转移
//!   （[`StateTree::transition_targets`]）；
//! - §7.3-2 `priority` 升序求值、首个命中即转移、一轮只转移一次（求值入口在 card.rs）；
//! - §7.3-4 directive 按 根→叶 拼装注入 B2 槽（[`StateTree::directive_of`]）；
//! - §7.3-5 可回放：本模块全部是纯函数，同一输入必得同一输出（[`StateTree::validate`] 的诊断
//!   顺序也按状态 id 排序固定）。
//!
//! **求值顺序规则**（两条入口共用，改一处必须两边同步）：
//! 候选转移 = 叶状态自己的 + 沿 `parent` 链继承的；叶的转移先于祖先的；
//! 按 `priority` **升序稳定排序**——同 `priority` 时保持「叶先、祖先后，各自按声明顺序」。
//! 未声明 `priority` 视为 0。
//!
//! 纯数据的边界：Lua 函数（`when` / `on_enter` / `on_exit`）不可能进到本模块。结构里只留
//! 「有没有」：`has_enter` / `has_exit` / `when_is_fn`。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// 一条转移的声明式部分（设计 §7.2 的 `{ to, when, priority }`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateTransition {
    /// 目标状态 id。可以是未声明的 id——引擎只如实转述，越界由 [`StateTree::validate`] 诊断，
    /// 求值入口（card.rs）也照转不误（宿主拿 [`StateTree::active_path`] 校验后再决定是否落转移）
    pub to: String,
    /// 求值顺序：小的先求值。未声明视为 0；同值按「叶先于祖先、各自声明顺序」
    pub priority: i64,
    /// 字符串简写 when（`"event:提及过去伤疤"`）；函数式 when 为 `None`
    pub when: Option<String>,
    /// when 是 Lua 函数（具体逻辑在卡里，纯数据只能记「有」）
    pub when_is_fn: bool,
}

/// 一个剧情状态节点（设计 §7.1 的 States 一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateNode {
    pub id: String,
    /// 父状态（层级：继承父状态未覆盖的转移与 directive）。无父 = 树根一侧的孤立节点
    pub parent: Option<String>,
    /// 该状态的导演指令（注入 B2 槽；子状态的指令排在父之后 = 子覆盖父）
    pub directive: Option<String>,
    /// 提升相关记忆召回权重（设计 §5.4）
    pub recall: Vec<String>,
    /// 揭示设定（设计 §6.4）
    pub reveal: Vec<String>,
    pub has_enter: bool,
    pub has_exit: bool,
    /// 声明式转移（函数式 when 只留 [`StateTransition::when_is_fn`] 一个布尔位）
    pub transitions: Vec<StateTransition>,
}

/// 一整棵状态树。`from_value` 吃的是 card::state_tree_shape 产出的 JSON 形态
/// （也可直接吃卡内 `state_tree` 的 JSON 化结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateTree {
    pub root: String,
    pub states: BTreeMap<String, StateNode>,
    /// 解析时输入里重复出现的状态 id（BTreeMap 表达不了重复，单独记一笔供 `validate` 诊断；
    /// 只有 `states` 写成数组形态时才可能出现）
    pub duplicate_ids: Vec<String>,
}

impl StateTree {
    /// 解析状态树。缺 `root` / 结构坏一律 `Err`（中文诊断，不 panic）。
    ///
    /// `states` 接受两种形态：对象 `{ "日常": {...} }`（正常）与数组
    /// `[ { "id": "日常", ... } ]`（Lua 侧 dump 常见；数组里的重名会记进
    /// [`StateTree::duplicate_ids`]，后写覆盖前写）。
    pub fn from_value(v: &Value) -> Result<StateTree, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "状态树不是 JSON 对象".to_string())?;
        let root = match obj.get("root") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => return Err("状态树缺少非空的 root".to_string()),
        };
        let raw = obj
            .get("states")
            .ok_or_else(|| "状态树缺少 states 表".to_string())?;
        let mut states = BTreeMap::new();
        let mut duplicate_ids = Vec::new();
        match raw {
            Value::Object(map) => {
                for (id, sv) in map {
                    let id = id.trim();
                    if id.is_empty() {
                        return Err("状态树里有一个空 id 的状态".to_string());
                    }
                    states.insert(id.to_string(), node_from_value(id, sv)?);
                }
            }
            Value::Array(items) => {
                for item in items {
                    let id = match item.get("id") {
                        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                        _ => return Err("states 数组里的状态缺少非空 id".to_string()),
                    };
                    let node = node_from_value(&id, item)?;
                    if states.insert(id.clone(), node).is_some() {
                        duplicate_ids.push(id);
                    }
                }
            }
            _ => return Err("状态树的 states 既不是对象也不是数组".to_string()),
        }
        duplicate_ids.sort();
        duplicate_ids.dedup();
        Ok(StateTree {
            root,
            states,
            duplicate_ids,
        })
    }

    /// 活跃路径：从叶一路沿 `parent` 往上，返回 **根→叶**（含父链）。
    ///
    /// - 叶没在 `states` 里声明 → 空 vec（宿主据此跳过注入/转移，不 panic）；
    /// - 父链断在未声明的父节点上 → 就地截断（从断点往下仍是一条合法路径）；
    /// - 父链有环（坏树）→ 截断，不死循环（诊断走 [`StateTree::validate`]）。
    pub fn active_path(&self, leaf: &str) -> Vec<String> {
        if !self.states.contains_key(leaf) {
            return Vec::new();
        }
        let mut chain: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut cur = Some(leaf.to_string());
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                break; // 环：截断
            }
            chain.push(id.clone());
            cur = match self.states.get(&id).and_then(|n| n.parent.clone()) {
                Some(p) if self.states.contains_key(&p) => Some(p),
                _ => None, // 无父 / 父未声明：截断
            };
        }
        chain.reverse();
        chain
    }

    /// 活跃路径的 directive：按 **根→叶** 拼接，用换行分隔（子覆盖父：子在前更靠后）。
    /// 没写 directive 的状态直接跳过；整条路径都没写 → 空串。
    pub fn directive_of(&self, path: &[String]) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for id in path {
            let Some(node) = self.states.get(id) else {
                continue;
            };
            let Some(d) = node.directive.as_deref() else {
                continue;
            };
            let d = d.trim();
            if !d.is_empty() {
                parts.push(d);
            }
        }
        parts.join("\n")
    }

    /// 活跃路径要提升召回的键（根→叶，去重保序：更靠根的先出现者排在前）
    pub fn recall_of(&self, path: &[String]) -> Vec<String> {
        dedupe_in_order(
            path.iter()
                .filter_map(|id| self.states.get(id))
                .flat_map(|n| n.recall.iter().cloned())
                .collect(),
        )
    }

    /// 活跃路径要揭示的设定键（根→叶，去重保序）
    pub fn reveal_of(&self, path: &[String]) -> Vec<String> {
        dedupe_in_order(
            path.iter()
                .filter_map(|id| self.states.get(id))
                .flat_map(|n| n.reveal.iter().cloned())
                .collect(),
        )
    }

    /// 结构校验：悬空父 / 父链环 / 自环 / 重名 / 转移目标越界 / root 缺失。
    /// 返回中文诊断（空 vec = 没问题），**不 panic**；顺序固定（root → 各状态按 id 升序 →
    /// 环 → 重名），便于快照与面板稳定展示。
    pub fn validate(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.root.trim().is_empty() {
            out.push("状态树没有 root".to_string());
        } else if !self.states.contains_key(&self.root) {
            out.push(format!("根状态「{}」没有在 states 里声明", self.root));
        }
        for (id, node) in &self.states {
            match &node.parent {
                Some(p) if p == id => out.push(format!("状态「{id}」的父状态是它自己（自环）")),
                Some(p) if !self.states.contains_key(p) => {
                    out.push(format!("状态「{id}」的父状态「{p}」不存在（悬空父）"));
                }
                _ => {}
            }
            for t in &node.transitions {
                if !self.states.contains_key(&t.to) {
                    out.push(format!("状态「{id}」的转移指向未声明的状态「{}」", t.to));
                }
            }
        }
        // 父链环：逐节点往上走，撞见走过的节点即闭环；同一环只报一次（以环成员排序当键去重）
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for start in self.states.keys() {
            let mut seen: BTreeMap<String, usize> = BTreeMap::new();
            let mut path: Vec<String> = Vec::new();
            let mut cur = Some(start.clone());
            while let Some(id) = cur {
                if let Some(&at) = seen.get(&id) {
                    let members: Vec<String> = path[at..].to_vec();
                    let mut key_src = members.clone();
                    key_src.sort();
                    if reported.insert(key_src.join(" → ")) {
                        out.push(format!("父链存在环：{} → {}", members.join(" → "), members[0]));
                    }
                    break;
                }
                seen.insert(id.clone(), path.len());
                path.push(id.clone());
                cur = match self.states.get(&id).and_then(|n| n.parent.clone()) {
                    Some(p) if p != id && self.states.contains_key(&p) => Some(p),
                    _ => None,
                };
            }
        }
        for id in &self.duplicate_ids {
            out.push(format!("状态「{id}」在 states 里重复声明"));
        }
        out
    }

    /// 从 `active`（当前活跃叶）出发可用的转移：`(to, priority, 来源状态)`，
    /// 按 priority 升序（设计 §7.3-2）；同 priority 时**叶的转移在前**，各自保持声明顺序。
    ///
    /// `active` 未声明 / 树上没有任何转移 → 空 vec（不 panic）；目标越界也如实给出
    /// （越界与否由 [`StateTree::validate`] 与宿主判断）。
    pub fn transition_targets(&self, active: &str) -> Vec<(String, i64, String)> {
        if !self.states.contains_key(active) {
            return Vec::new();
        }
        let mut out: Vec<(String, i64, String)> = Vec::new();
        // active_path 是 根→叶，倒过来即「叶先、祖先后」
        for id in self.active_path(active).iter().rev() {
            let Some(node) = self.states.get(id) else {
                continue;
            };
            for t in &node.transitions {
                out.push((t.to.clone(), t.priority, id.clone()));
            }
        }
        out.sort_by_key(|(_, p, _)| *p); // 稳定排序：同 priority 保持叶先
        out
    }
}

/// 去重保序（先出现的留下；空白项丢弃）
fn dedupe_in_order(items: Vec<String>) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::with_capacity(items.len());
    for s in items {
        if s.trim().is_empty() {
            continue;
        }
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// 解析一个状态节点
fn node_from_value(id: &str, v: &Value) -> Result<StateNode, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("状态「{id}」不是 JSON 对象"))?;
    let parent = match obj.get("parent") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Some(_) => return Err(format!("状态「{id}」的 parent 不是字符串")),
    };
    let directive = match obj.get("directive") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(format!("状态「{id}」的 directive 不是字符串")),
    };
    let recall = string_list(id, "recall", obj.get("recall"))?;
    let reveal = string_list(id, "reveal", obj.get("reveal"))?;
    let has_enter = bool_flag(id, "has_enter", obj.get("has_enter"))?;
    let has_exit = bool_flag(id, "has_exit", obj.get("has_exit"))?;
    let transitions = match obj.get("transitions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(transition_from_value(id, i, item)?);
            }
            out
        }
        Some(_) => return Err(format!("状态「{id}」的 transitions 不是数组")),
    };
    Ok(StateNode {
        id: id.to_string(),
        parent,
        directive,
        recall,
        reveal,
        has_enter,
        has_exit,
        transitions,
    })
}

fn transition_from_value(state_id: &str, index: usize, v: &Value) -> Result<StateTransition, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("状态「{state_id}」的第 {} 条转移不是 JSON 对象", index + 1))?;
    let to = match obj.get("to") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return Err(format!(
                "状态「{state_id}」的第 {} 条转移缺少非空的 to",
                index + 1
            ))
        }
    };
    let priority = match obj.get("priority") {
        None | Some(Value::Null) => 0,
        Some(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| {
                format!(
                    "状态「{state_id}」的第 {} 条转移的 priority 不是数字",
                    index + 1
                )
            })?,
    };
    let when = match obj.get("when") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(format!(
                "状态「{state_id}」的第 {} 条转移的 when 不是字符串（函数式 when 只存在于 Lua 侧）",
                index + 1
            ))
        }
    };
    let when_is_fn = bool_flag(state_id, "when_is_fn", obj.get("when_is_fn"))?;
    Ok(StateTransition {
        to,
        priority,
        when,
        when_is_fn,
    })
}

fn string_list(state_id: &str, key: &str, v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) if !s.trim().is_empty() => out.push(s.clone()),
                    Value::String(_) => {}
                    _ => {
                        return Err(format!("状态「{state_id}」的 {key} 里有非字符串项"));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(format!("状态「{state_id}」的 {key} 不是数组")),
    }
}

fn bool_flag(state_id: &str, key: &str, v: Option<&Value>) -> Result<bool, String> {
    match v {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("状态「{state_id}」的 {key} 不是布尔值")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 与设计 §7.2 同构的树（Lua 函数位折成 `has_enter` / `when_is_fn` 布尔位）
    fn design_value() -> Value {
        json!({
            "root": "日常",
            "states": {
                "日常": {
                    "directive": "保持轻松日常的氛围。",
                    "transitions": [
                        { "to": "日常.夜谈", "priority": 10, "when_is_fn": true },
                        { "to": "疏远", "priority": 20, "when": "event:提及过去伤疤" }
                    ]
                },
                "日常.夜谈": {
                    "parent": "日常",
                    "directive": "夜深人静，两人独处。",
                    "recall": ["room:图书馆", "topic:过去"],
                    "reveal": ["char.小雨.secrets.工作牌"],
                    "has_enter": true
                },
                "疏远": {
                    "directive": "她突然变得客气而疏离。",
                    "has_enter": true,
                    "has_exit": true
                }
            }
        })
    }

    fn design() -> StateTree {
        StateTree::from_value(&design_value()).expect("示例树应能解析")
    }

    #[test]
    fn from_value_reads_design_tree() {
        let t = design();
        assert_eq!(t.root, "日常");
        assert_eq!(t.states.len(), 3);
        assert!(t.duplicate_ids.is_empty());

        let leaf = t.states.get("日常.夜谈").unwrap();
        assert_eq!(leaf.parent.as_deref(), Some("日常"));
        assert_eq!(leaf.directive.as_deref(), Some("夜深人静，两人独处。"));
        assert_eq!(leaf.recall, vec!["room:图书馆", "topic:过去"]);
        assert_eq!(leaf.reveal, vec!["char.小雨.secrets.工作牌"]);
        assert!(leaf.has_enter);
        assert!(!leaf.has_exit);
        assert!(leaf.transitions.is_empty());

        let root = t.states.get("日常").unwrap();
        assert_eq!(root.parent, None);
        assert!(!root.has_enter);
        assert_eq!(root.transitions.len(), 2);
        // 函数式 when：纯数据只留布尔位
        assert!(root.transitions[0].when_is_fn && root.transitions[0].when.is_none());
        assert_eq!(root.transitions[0].to, "日常.夜谈");
        assert_eq!(root.transitions[0].priority, 10);
        // 字符串简写 when 原样保留
        assert_eq!(root.transitions[1].when.as_deref(), Some("event:提及过去伤疤"));
        assert!(!root.transitions[1].when_is_fn);

        let distant = t.states.get("疏远").unwrap();
        assert!(distant.has_enter && distant.has_exit);
    }

    #[test]
    fn from_value_rejects_missing_root_and_broken_structure() {
        let cases: Vec<(Value, &str)> = vec![
            (json!([]), "不是 JSON 对象"),
            (json!({}), "缺少非空的 root"),
            (json!({ "root": "  " }), "缺少非空的 root"),
            (json!({ "root": "日常" }), "缺少 states"),
            (json!({ "root": "日常", "states": 3 }), "既不是对象也不是数组"),
            (json!({ "root": "日常", "states": { "": {} } }), "空 id"),
            (json!({ "root": "日常", "states": { "日常": 1 } }), "不是 JSON 对象"),
            (
                json!({ "root": "日常", "states": { "日常": { "parent": 1 } } }),
                "parent 不是字符串",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "directive": [] } } }),
                "directive 不是字符串",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "recall": "room:图书馆" } } }),
                "recall 不是数组",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "reveal": [1] } } }),
                "reveal 里有非字符串项",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "has_enter": "yes" } } }),
                "has_enter 不是布尔值",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "transitions": {} } } }),
                "transitions 不是数组",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "transitions": [{ "priority": 1 }] } } }),
                "缺少非空的 to",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "transitions": [{ "to": "x", "priority": "high" }] } } }),
                "priority 不是数字",
            ),
            (
                json!({ "root": "日常", "states": { "日常": { "transitions": [{ "to": "x", "when": {} }] } } }),
                "when 不是字符串",
            ),
            (json!({ "root": "日常", "states": [{ "parent": "日常" }] }), "缺少非空 id"),
        ];
        for (v, needle) in cases {
            let err = StateTree::from_value(&v).expect_err(&format!("应报错：{v}"));
            assert!(err.contains(needle), "「{err}」应含「{needle}」");
        }
    }

    #[test]
    fn from_value_accepts_array_states_and_flags_duplicates() {
        let v = json!({
            "root": "A",
            "states": [
                { "id": "A", "directive": "第一版" },
                { "id": "A", "directive": "第二版" },
                { "id": "B", "parent": "A" }
            ]
        });
        let t = StateTree::from_value(&v).expect("数组形态应能解析");
        assert_eq!(t.states.len(), 2);
        assert_eq!(t.duplicate_ids, vec!["A".to_string()]);
        assert_eq!(t.states["A"].directive.as_deref(), Some("第二版"));
        assert!(t
            .validate()
            .iter()
            .any(|d| d.contains("状态「A」在 states 里重复声明")));
    }

    #[test]
    fn active_path_walks_up_and_truncates() {
        let v = json!({
            "root": "日常",
            "states": {
                "日常": {},
                "日常.夜谈": { "parent": "日常" },
                "日常.夜谈.深": { "parent": "日常.夜谈" },
                "孤儿": { "parent": "不存在的父" },
                "自环": { "parent": "自环" }
            }
        });
        let t = StateTree::from_value(&v).unwrap();
        assert_eq!(
            t.active_path("日常.夜谈.深"),
            vec!["日常", "日常.夜谈", "日常.夜谈.深"]
        );
        assert_eq!(t.active_path("日常"), vec!["日常"]);
        // 缺失父节点：就地截断（不是空 vec）
        assert_eq!(t.active_path("孤儿"), vec!["孤儿"]);
        // 自环：不死循环，截断
        assert_eq!(t.active_path("自环"), vec!["自环"]);
        // 未声明的叶：空 vec
        assert!(t.active_path("没这个状态").is_empty());
        assert!(t.active_path("").is_empty());
    }

    #[test]
    fn directive_of_joins_root_to_leaf_and_skips_empty() {
        let v = json!({
            "root": "日常",
            "states": {
                "日常": { "directive": " 保持轻松 " },
                "日常.夜谈": { "parent": "日常", "directive": "夜深人静。" },
                "日常.夜谈.深": { "parent": "日常.夜谈" }
            }
        });
        let t = StateTree::from_value(&v).unwrap();
        let path = t.active_path("日常.夜谈.深");
        assert_eq!(t.directive_of(&path), "保持轻松\n夜深人静。");
        // 只有子状态写了 directive
        assert_eq!(t.directive_of(&["日常.夜谈".to_string()]), "夜深人静。");
        // 路径里的未声明 id 被忽略；完全没写的路径是空串
        assert_eq!(t.directive_of(&["没这个状态".to_string()]), "");
        assert_eq!(t.directive_of(&[]), "");
    }

    #[test]
    fn recall_and_reveal_dedupe_preserving_root_to_leaf_order() {
        let v = json!({
            "root": "A",
            "states": {
                "A": { "recall": ["room:图书馆", "topic:过去"], "reveal": ["x"] },
                "B": { "parent": "A", "recall": ["topic:过去", "room:天台"], "reveal": ["x", "y"] }
            }
        });
        let t = StateTree::from_value(&v).unwrap();
        let path = t.active_path("B");
        assert_eq!(t.recall_of(&path), vec!["room:图书馆", "topic:过去", "room:天台"]);
        assert_eq!(t.reveal_of(&path), vec!["x", "y"]);
        assert!(t.recall_of(&["没这个状态".to_string()]).is_empty());
    }

    #[test]
    fn validate_reports_root_self_loop_dangling_parent_and_cycle() {
        // 根没声明
        let t = StateTree::from_value(&json!({ "root": "幽灵", "states": { "A": {} } })).unwrap();
        assert_eq!(t.validate(), vec!["根状态「幽灵」没有在 states 里声明"]);

        // 自环（父是自己）
        let t = StateTree::from_value(&json!({
            "root": "A",
            "states": { "A": { "parent": "A" } }
        }))
        .unwrap();
        let d = t.validate();
        assert!(d.iter().any(|s| s.contains("父状态是它自己（自环）")), "{d:?}");
        assert!(!d.iter().any(|s| s.contains("父链存在环")), "自环不与环重复报：{d:?}");

        // 悬空父
        let t = StateTree::from_value(&json!({
            "root": "A",
            "states": { "A": {}, "B": { "parent": "没有这个状态" } }
        }))
        .unwrap();
        let d = t.validate();
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("状态「B」的父状态「没有这个状态」不存在（悬空父）"));

        // 环（两个节点互指；每个节点都扫一遍，但只报一次）
        let t = StateTree::from_value(&json!({
            "root": "A",
            "states": { "A": { "parent": "B" }, "B": { "parent": "A" } }
        }))
        .unwrap();
        let d = t.validate();
        let cycles: Vec<&String> = d.iter().filter(|s| s.contains("父链存在环")).collect();
        assert_eq!(cycles.len(), 1, "同一个环只报一次：{d:?}");
        assert!(
            cycles[0].contains("A → B → A") || cycles[0].contains("B → A → B"),
            "{:?}",
            cycles[0]
        );
    }

    #[test]
    fn validate_reports_transition_target_out_of_range() {
        let t = StateTree::from_value(&json!({
            "root": "A",
            "states": {
                "A": { "transitions": [{ "to": "B", "priority": 1 }, { "to": "不存在", "priority": 2 }] },
                "B": { "parent": "A" }
            }
        }))
        .unwrap();
        let d = t.validate();
        assert_eq!(d, vec!["状态「A」的转移指向未声明的状态「不存在」"]);
    }

    #[test]
    fn transition_targets_sorted_by_priority_leaf_first_and_deterministic() {
        let v = json!({
            "root": "塔",
            "states": {
                "塔": {
                    "transitions": [
                        { "to": "祖先后", "priority": 5 },
                        { "to": "祖先同", "priority": 30 }
                    ]
                },
                "塔.中": { "parent": "塔", "transitions": [{ "to": "中先", "priority": 5 }] },
                "塔.中.叶": {
                    "parent": "塔.中",
                    "transitions": [
                        { "to": "叶A", "priority": 30 },
                        { "to": "叶B", "priority": 10 }
                    ]
                }
            }
        });
        let t = StateTree::from_value(&v).unwrap();
        // 叶 → 中 → 塔；priority 升序；同 priority 叶在前
        assert_eq!(
            t.transition_targets("塔.中.叶"),
            vec![
                ("中先".to_string(), 5, "塔.中".to_string()),
                ("祖先后".to_string(), 5, "塔".to_string()),
                ("叶B".to_string(), 10, "塔.中.叶".to_string()),
                ("叶A".to_string(), 30, "塔.中.叶".to_string()),
                ("祖先同".to_string(), 30, "塔".to_string()),
            ]
        );
        // 纯函数：同一输入两次结果一致（§7.3-5）
        assert_eq!(t.transition_targets("塔.中.叶"), t.transition_targets("塔.中.叶"));
        // 叶子自己的转移只算自己的 + 祖先的
        assert_eq!(
            t.transition_targets("塔.中"),
            vec![
                ("中先".to_string(), 5, "塔.中".to_string()),
                ("祖先后".to_string(), 5, "塔".to_string()),
                ("祖先同".to_string(), 30, "塔".to_string()),
            ]
        );
    }

    #[test]
    fn transition_targets_unknown_active_and_out_of_range_target_do_not_panic() {
        let v = json!({
            "root": "A",
            "states": { "A": { "transitions": [{ "to": "越界目标" }] } }
        });
        let t = StateTree::from_value(&v).unwrap();
        // 未声明的活跃叶 → 空 vec，不 panic
        assert!(t.transition_targets("没这个状态").is_empty());
        // 目标越界：如实给出，交给 validate 诊断
        assert_eq!(
            t.transition_targets("A"),
            vec![("越界目标".to_string(), 0, "A".to_string())]
        );
        assert!(t.validate().iter().any(|d| d.contains("越界目标")));
    }
}
