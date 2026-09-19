//! 事件日志：会话的唯一事实来源（M2.0 · 设计 §7.3「可回放」/ §6.9 / §12）
//!
//! **为什么要有它**：M1 里钩子对 state 的改动是「快照式」落盘——编辑或删除历史消息
//! 不会回滚它（把带「谢谢」的那句改成别的，好感度仍然停在 +1 之后的值）。设计 §7.3
//! 要求「转移是 (事件流, 黑板, state, 设定揭示状态, 剧情线状态) 的纯函数」，§6.9 要求
//! 确认/否决动作记入事件流，§5.3 要求总结批次可从 jsonl 回放重算。三处指向同一份设施。
//!
//! 于是把 messages.jsonl 升级为**类型化事件流**：消息、钩子副作用、黑板、状态树转移、
//! 剧情线、设定确认都是事件；state.json / blackboard.json / palace.jsonl 一律由事件流
//! 的**投影**（[project_over]）写出——落盘只有一条路径，回放必然一致。
//!
//! **向后兼容**：M1 写的行没有 kind 字段，按消息读（见 [LogBody::from_value]）；
//! 旧会话的派生文件（state.json / blackboard.json / palace.jsonl）充当起始基线
//! （[Base]），事件在其上继续折叠，不做破坏性迁移。
//!
//! **重放语义**（调用方按此实现重建，见 commands.rs 的 rebuild_from）：
//! 消息级操作（编辑/删除/重roll）保留消息事件、丢弃**派生事件**（[LogRecord::is_derived]），
//! 再按卡顺序重跑这些消息的钩子，追加新派生事件——钩子是 (消息, 黑板, state) 的纯函数，
//! 重放得到同一状态。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::card::KvSet;
use crate::store::{Blackboard, MemRecord, Message};

/// 事件序号（从 1 起，等于它在流中的位置；重写时整体重排）
pub type Seq = u64;

// ---------- 事件体 ----------

/// 一次钩子运行的副作用（设计 §7.3：任务/进入钩子写记忆、改黑板、揭示设定）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectEvent {
    pub turn: u64,
    /// 触发者：on_load / on_context / on_message:user / on_message:char
    pub trigger: String,
    /// 角色（characters/ 下的目录名）
    pub character: String,
    /// state 顶层键补丁：值 = 该键的完整新值，JSON null 表示删除该键
    #[serde(default)]
    pub state_set: Vec<KvSet>,
    /// api.blackboard.set 的写入（白名单四字段，折叠时后写覆盖）
    #[serde(default)]
    pub blackboard: Vec<KvSet>,
    /// api.memory.set 的写入（M2.1 起由记忆宫殿消费）
    #[serde(default)]
    pub memory: Vec<KvSet>,
    pub ts: u64,
}

/// 黑板变更（全量快照，折叠时后写覆盖）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlackboardEvent {
    pub turn: u64,
    /// init（建会话基线）/ manual（界面手改）/ clock（轮末时钟步进）/ hook（钩子触发）
    pub reason: String,
    pub board: Blackboard,
    pub ts: u64,
}

/// 状态树转移（M2.3 · 设计 §7.3-3：转移作为事件追加）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionEvent {
    pub turn: u64,
    /// 转移前的活跃路径（根→叶）
    pub from: Vec<String>,
    /// 转移后的活跃路径
    pub to: Vec<String>,
    /// 命中的转移说明（哪个状态的哪条 when）
    pub reason: String,
    pub ts: u64,
}

/// 剧情线事件（M2.4 · 设计 §8.3：开线/推进/升格/收线/放弃）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadEvent {
    pub turn: u64,
    /// open | progress | escalate | resolve | abandon
    pub op: String,
    pub thread_id: String,
    /// 线全量快照（折叠时同 id 后写覆盖；None = 只记一笔不改变投影）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<serde_json::Value>,
    /// manual（玩家手动）/ tree（状态树任务）/ pipeline（总结管线提案）
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub ts: u64,
}

/// 设定事件（M2.2 · 设计 §6.9：确立/揭示/retcon 进事件流）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexEvent {
    pub turn: u64,
    /// reveal（揭示）/ confirm（草稿转正史）/ retract（废止）/ retcon（显式改写）
    pub op: String,
    /// 目标：实体 id 或 实体.秘密 路径
    pub target: String,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub ts: u64,
}

fn default_origin() -> String {
    "manual".into()
}

/// 事件体（messages.jsonl 一行去掉 seq 与 kind 之后的部分）
#[derive(Debug, Clone, PartialEq)]
pub enum LogBody {
    Message(Message),
    Effect(EffectEvent),
    Blackboard(BlackboardEvent),
    Transition(TransitionEvent),
    Thread(ThreadEvent),
    Codex(CodexEvent),
}

impl From<Message> for LogBody {
    fn from(m: Message) -> Self {
        LogBody::Message(m)
    }
}

impl From<&Message> for LogBody {
    fn from(m: &Message) -> Self {
        LogBody::Message(m.clone())
    }
}

impl LogBody {
    pub fn kind(&self) -> &'static str {
        match self {
            LogBody::Message(_) => "message",
            LogBody::Effect(_) => "effect",
            LogBody::Blackboard(_) => "blackboard",
            LogBody::Transition(_) => "transition",
            LogBody::Thread(_) => "thread",
            LogBody::Codex(_) => "codex",
        }
    }

    /// 归属轮次（消息级重放按它切分）
    pub fn turn(&self) -> u64 {
        match self {
            LogBody::Message(m) => m.turn,
            LogBody::Effect(e) => e.turn,
            LogBody::Blackboard(b) => b.turn,
            LogBody::Transition(t) => t.turn,
            LogBody::Thread(t) => t.turn,
            LogBody::Codex(c) => c.turn,
        }
    }

    /// 序列化为带 kind 判别字段的对象
    pub fn to_value(&self) -> Result<serde_json::Value, String> {
        let mut v = match self {
            LogBody::Message(m) => serde_json::to_value(m),
            LogBody::Effect(e) => serde_json::to_value(e),
            LogBody::Blackboard(b) => serde_json::to_value(b),
            LogBody::Transition(t) => serde_json::to_value(t),
            LogBody::Thread(t) => serde_json::to_value(t),
            LogBody::Codex(c) => serde_json::to_value(c),
        }
        .map_err(|e| format!("事件序列化失败：{e}"))?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("kind".into(), serde_json::Value::String(self.kind().into()));
        }
        Ok(v)
    }

    /// 从对象还原。**没有 kind 字段 = M1 写下的消息行**（向后兼容的关键）。
    pub fn from_value(v: serde_json::Value) -> Result<LogBody, String> {
        let kind = v
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("message")
            .to_string();
        let bad = |e: serde_json::Error| format!("事件解析失败（kind={kind}）：{e}");
        match kind.as_str() {
            "message" => serde_json::from_value(v).map(LogBody::Message).map_err(bad),
            "effect" => serde_json::from_value(v).map(LogBody::Effect).map_err(bad),
            "blackboard" => serde_json::from_value(v)
                .map(LogBody::Blackboard)
                .map_err(bad),
            "transition" => serde_json::from_value(v)
                .map(LogBody::Transition)
                .map_err(bad),
            "thread" => serde_json::from_value(v).map(LogBody::Thread).map_err(bad),
            "codex" => serde_json::from_value(v).map(LogBody::Codex).map_err(bad),
            other => Err(format!("未知事件类型：{other}")),
        }
    }
}

// ---------- 记录 ----------

/// 事件流的一行
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub seq: Seq,
    pub body: LogBody,
}

impl LogRecord {
    pub fn new(seq: Seq, body: LogBody) -> Self {
        LogRecord { seq, body }
    }

    pub fn message(seq: Seq, msg: Message) -> Self {
        LogRecord::new(seq, LogBody::Message(msg))
    }

    pub fn turn(&self) -> u64 {
        self.body.turn()
    }

    pub fn as_message(&self) -> Option<&Message> {
        match &self.body {
            LogBody::Message(m) => Some(m),
            _ => None,
        }
    }

    /// **派生事件**：由重放重新生成的事件。消息级操作（编辑/删除/重roll）丢弃它们再重算；
    /// 玩家手动产生的黑板/线/设定事件（origin=manual、reason=init|manual）永远保留。
    pub fn is_derived(&self) -> bool {
        match &self.body {
            LogBody::Message(_) => false,
            LogBody::Effect(_) | LogBody::Transition(_) => true,
            LogBody::Blackboard(b) => b.reason == "clock" || b.reason == "hook",
            LogBody::Thread(t) => t.origin != "manual",
            LogBody::Codex(c) => c.origin != "manual",
        }
    }

    pub fn to_json(&self) -> Result<serde_json::Value, String> {
        let mut v = self.body.to_value()?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("seq".into(), serde_json::Value::Number(self.seq.into()));
        }
        Ok(v)
    }

    pub fn to_line(&self) -> Result<String, String> {
        Ok(serde_json::to_string(&self.to_json()?).map_err(|e| e.to_string())? + "\n")
    }

    /// 从一行还原（seq 缺省 0，调用方按位置补）
    pub fn from_line(line: &str) -> Result<LogRecord, String> {
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("事件行不是合法 JSON：{e}"))?;
        let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
        Ok(LogRecord {
            seq,
            body: LogBody::from_value(v)?,
        })
    }
}

/// 解析缓冲里的完整行（返回记录与消费的字节数）。
///
/// 只吃到最后一个换行符为止——结尾半行（崩溃残留）留待补全；坏行跳过。
/// 字节级切行对 UTF-8 安全（多字节字符的续字节不含换行）。
/// 序号按位置重新编号（next_seq 起），因此外部手改文件也不会让序号错乱。
pub fn parse_lines(buf: &[u8], next_seq: Seq) -> (Vec<LogRecord>, usize) {
    let complete = match buf.iter().rposition(|&b| b == b'\n') {
        Some(p) => p + 1,
        None => return (Vec::new(), 0),
    };
    let mut out = Vec::new();
    let mut seq = next_seq;
    for line in buf[..complete].split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if let Ok(mut rec) = std::str::from_utf8(line)
            .map_err(|e| e.to_string())
            .and_then(LogRecord::from_line)
        {
            rec.seq = seq;
            seq += 1;
            out.push(rec);
        }
    }
    (out, complete)
}

/// 把记录列表渲染成文件内容（写盘用；序号按位置重排）
pub fn render_lines(records: &[LogRecord]) -> Result<String, String> {
    let mut buf = String::new();
    for (i, rec) in records.iter().enumerate() {
        let mut rec = rec.clone();
        rec.seq = i as Seq + 1;
        buf.push_str(&rec.to_line()?);
    }
    Ok(buf)
}

/// 事件流里的消息视图（顺序即对话顺序）
pub fn messages(records: &[LogRecord]) -> Vec<Message> {
    records
        .iter()
        .filter_map(|r| r.as_message().cloned())
        .collect()
}

/// 事件流是否已带 genesis（M2 建会话时写入的初始黑板事件）。
/// 有 genesis 说明 state/黑板/宫殿都能从事件流完整重建，派生文件不再充当基线
/// （否则宫殿会把自己的输出再当成基线读一次，导致记忆重复）。
pub fn has_genesis(records: &[LogRecord]) -> bool {
    records
        .iter()
        .any(|r| matches!(&r.body, LogBody::Blackboard(b) if b.reason == "init"))
}

// ---------- 投影 ----------

/// 事件流的起始基线：M1 会话已有的派生文件（事件流里没有对应事件时用它们兜底）
#[derive(Debug, Clone, Default)]
pub struct Base {
    pub blackboard: Option<Blackboard>,
    /// 角色目录名 → state
    pub states: BTreeMap<String, serde_json::Value>,
    /// 旧会话的宫殿记录（palace.jsonl）
    pub memory: Vec<MemRecord>,
    /// 事件流里已有 genesis：派生文件不作为基线
    pub genesis: bool,
}

/// 事件流折叠出的会话现状（一切派生文件的唯一来源）
#[derive(Debug, Clone, Default)]
pub struct Projection {
    pub messages: Vec<Message>,
    /// 角色目录名 → state（M2 仍是 1v1 单元素；M3 群聊按角色分）
    pub states: BTreeMap<String, serde_json::Value>,
    pub blackboard: Option<Blackboard>,
    pub memory: Vec<MemRecord>,
    pub transitions: Vec<TransitionEvent>,
    /// 线 id → 当前快照（后写覆盖）
    pub threads: BTreeMap<String, serde_json::Value>,
    pub thread_log: Vec<ThreadEvent>,
    /// 秘密揭示集（"实体.秘密" 路径）
    pub known: std::collections::BTreeSet<String>,
    pub codex_log: Vec<CodexEvent>,
    pub last_seq: Seq,
}

impl Projection {
    /// 取某角色的 state（没有则 None；调用方按卡上 default_state 降级）
    pub fn state_of(&self, character: &str) -> Option<&serde_json::Value> {
        self.states.get(character)
    }

    /// 最近一条消息
    pub fn last_message(&self) -> Option<&Message> {
        self.messages.last()
    }
}

/// 在基线上折叠事件流
pub fn project_over(records: &[LogRecord], base: &Base) -> Projection {
    let mut p = Projection {
        states: base.states.clone(),
        blackboard: base.blackboard.clone(),
        memory: if base.genesis {
            Vec::new()
        } else {
            base.memory.clone()
        },
        ..Default::default()
    };
    for rec in records {
        fold(&mut p, rec);
    }
    p
}

/// 把一条事件折叠进投影（增量折叠与整体投影走同一份代码，防两处语义漂移）
pub fn fold(p: &mut Projection, rec: &LogRecord) {
    p.last_seq = p.last_seq.max(rec.seq);
    match &rec.body {
        LogBody::Message(m) => p.messages.push(m.clone()),
        LogBody::Effect(e) => {
            if !e.state_set.is_empty() {
                let entry = p
                    .states
                    .entry(e.character.clone())
                    .or_insert_with(|| serde_json::json!({}));
                if !entry.is_object() {
                    *entry = serde_json::json!({});
                }
                let obj = entry.as_object_mut().expect("上面刚保证是对象");
                for kv in &e.state_set {
                    if kv.value.is_null() {
                        obj.remove(&kv.key);
                    } else {
                        obj.insert(kv.key.clone(), kv.value.clone());
                    }
                }
            }
            if !e.blackboard.is_empty() {
                let bb = p
                    .blackboard
                    .get_or_insert_with(|| Blackboard::default_board());
                apply_blackboard_sets(bb, &e.blackboard);
            }
            for kv in &e.memory {
                p.memory.push(MemRecord {
                    kind: "fact".into(),
                    key: kv.key.clone(),
                    value: kv.value.clone(),
                    source: e.trigger.clone(),
                    turn: e.turn,
                    ts: e.ts,
                });
            }
        }
        LogBody::Blackboard(b) => p.blackboard = Some(b.board.clone()),
        LogBody::Transition(t) => p.transitions.push(t.clone()),
        LogBody::Thread(t) => {
            if let Some(snapshot) = &t.thread {
                p.threads.insert(t.thread_id.clone(), snapshot.clone());
            }
            p.thread_log.push(t.clone());
        }
        LogBody::Codex(c) => {
            match c.op.as_str() {
                "reveal" => {
                    p.known.insert(c.target.clone());
                }
                "retract" => {
                    p.known.remove(&c.target);
                }
                _ => {}
            }
            p.codex_log.push(c.clone());
        }
    }
}

/// api.blackboard.set 的写入合并进黑板（只认黑板 v0 字段；类型不符即忽略）。
/// 返回是否有实际改动。宿主与投影共用，避免两处白名单漂移。
pub fn apply_blackboard_sets(bb: &mut Blackboard, sets: &[KvSet]) -> bool {
    let before = format!("{bb:?}");
    for kv in sets {
        match (kv.key.as_str(), &kv.value) {
            ("day", v) => {
                if let Some(n) = v.as_i64() {
                    bb.day = n;
                }
            }
            ("clock", v) => {
                if let Some(s) = v.as_str() {
                    bb.clock = s.to_string();
                }
            }
            ("place", v) => {
                if let Some(s) = v.as_str() {
                    bb.place = s.to_string();
                }
            }
            ("actors", v) => {
                if let Some(arr) = v.as_array() {
                    bb.actors = arr
                        .iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect();
                }
            }
            // 实体作用域键（设计 §6.4：`char.小雨.status`）——带点的键一律进 extra
            (key, v) if key.contains('.') => {
                if v.is_null() {
                    bb.extra.remove(key);
                } else {
                    bb.extra.insert(key.to_string(), v.clone());
                }
            }
            _ => {} // 其余未知键兜底忽略（沙箱侧还有一层白名单）
        }
    }
    format!("{bb:?}") != before
}

/// state 的**顶层键补丁**：钩子原地改的是嵌套表，顶层键比对足以完整表达变化
/// （改了 psyche.affects 就整段记下 psyche），且事件流不会随状态膨胀。
pub fn state_patch(before: &serde_json::Value, after: &serde_json::Value) -> Vec<KvSet> {
    let empty = serde_json::Map::new();
    let before = before.as_object().unwrap_or(&empty);
    let after = after.as_object().unwrap_or(&empty);
    let mut out = Vec::new();
    for (k, v) in after {
        if before.get(k) != Some(v) {
            out.push(KvSet {
                key: k.clone(),
                value: v.clone(),
            });
        }
    }
    for k in before.keys() {
        if !after.contains_key(k) {
            out.push(KvSet {
                key: k.clone(),
                value: serde_json::Value::Null, // null = 删除该键
            });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key)); // 确定性输出
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(turn: u64, role: &str, content: &str) -> Message {
        Message {
            turn,
            role: role.into(),
            content: content.into(),
            ts: 100 + turn,
            scene_id: None,
        }
    }

    fn effect(turn: u64, key: &str, value: serde_json::Value) -> LogBody {
        LogBody::Effect(EffectEvent {
            turn,
            trigger: "hook.on_message".into(),
            character: "小雨".into(),
            state_set: vec![KvSet {
                key: key.into(),
                value,
            }],
            blackboard: vec![],
            memory: vec![],
            ts: 1,
        })
    }

    fn board(day: i64, clock: &str) -> Blackboard {
        Blackboard {
            day,
            clock: clock.into(),
            place: "图书馆".into(),
            actors: vec!["小雨".into()],
            extra: Default::default(),
        }
    }

    /// 六条事件的样例流（序号按位置）。内容对应「开场 → 用户道谢 → 回复」的一轮。
    fn sample_log() -> Vec<LogRecord> {
        let mut records = vec![
            LogRecord::new(
                0,
                LogBody::Blackboard(BlackboardEvent {
                    turn: 0,
                    reason: "init".into(),
                    board: board(1, "20:00"),
                    ts: 1,
                }),
            ),
            LogRecord::message(0, msg(0, "char", "开场")),
            LogRecord::new(0, effect(0, "favorability", serde_json::json!(50))),
            LogRecord::message(0, msg(1, "user", "谢谢")),
            LogRecord::new(0, effect(1, "favorability", serde_json::json!(51))),
            LogRecord::message(0, msg(1, "char", "……嗯")),
        ];
        for (i, rec) in records.iter_mut().enumerate() {
            rec.seq = i as Seq + 1;
        }
        records
    }

    #[test]
    fn legacy_lines_without_kind_read_as_messages() {
        // M1 写下的行没有 kind —— 必须照旧读成消息（否则所有老会话都读不出来）
        let line = r#"{"turn":3,"role":"user","content":"你好","ts":1700000000}"#;
        let rec = LogRecord::from_line(line).unwrap();
        assert_eq!(rec.body.kind(), "message");
        assert_eq!(rec.as_message().unwrap().content, "你好");
        assert_eq!(rec.turn(), 3);
    }

    #[test]
    fn roundtrip_through_json_keeps_kind_and_seq() {
        let bodies = vec![
            LogBody::Message(msg(2, "user", "在吗")),
            effect(2, "favorability", serde_json::json!(52)),
            LogBody::Blackboard(BlackboardEvent {
                turn: 2,
                reason: "clock".into(),
                board: board(2, "09:10"),
                ts: 9,
            }),
            LogBody::Transition(TransitionEvent {
                turn: 2,
                from: vec!["日常".into()],
                to: vec!["日常".into(), "日常.夜谈".into()],
                reason: "clock>=23".into(),
                ts: 9,
            }),
            LogBody::Thread(ThreadEvent {
                turn: 2,
                op: "open".into(),
                thread_id: "thread.还书".into(),
                thread: Some(serde_json::json!({"id":"thread.还书"})),
                origin: "manual".into(),
                note: None,
                ts: 9,
            }),
            LogBody::Codex(CodexEvent {
                turn: 2,
                op: "reveal".into(),
                target: "char.小雨.secrets.工作牌".into(),
                origin: "tree".into(),
                value: None,
                note: Some("夜谈".into()),
                ts: 9,
            }),
        ];
        for (i, body) in bodies.into_iter().enumerate() {
            let rec = LogRecord::new(i as Seq + 1, body);
            let line = rec.to_line().unwrap();
            assert!(line.contains(&format!("\"kind\":\"{}\"", rec.body.kind())));
            let back = LogRecord::from_line(line.trim()).unwrap();
            assert_eq!(back.body, rec.body, "事件往返应完全一致");
            assert_eq!(back.seq, rec.seq);
        }
    }

    #[test]
    fn unknown_kind_is_rejected_not_silently_dropped() {
        let line = r#"{"kind":"nonsense","turn":1}"#;
        assert!(LogRecord::from_line(line).is_err());
    }

    #[test]
    fn projection_folds_state_blackboard_and_memory() {
        let mut records = sample_log();
        records.push(LogRecord::new(
            0,
            LogBody::Effect(EffectEvent {
                turn: 1,
                trigger: "hook.on_message".into(),
                character: "小雨".into(),
                state_set: vec![],
                blackboard: vec![KvSet {
                    key: "place".into(),
                    value: serde_json::json!("天台"),
                }],
                memory: vec![KvSet {
                    key: "last_thanked".into(),
                    value: serde_json::json!(1),
                }],
                ts: 2,
            }),
        ));
        let p = project_over(&records, &Base::default());
        assert_eq!(p.messages.len(), 3);
        assert_eq!(p.state_of("小雨").unwrap()["favorability"], 51);
        assert_eq!(p.blackboard.as_ref().unwrap().place, "天台");
        assert_eq!(p.blackboard.as_ref().unwrap().day, 1);
        assert_eq!(p.memory.len(), 1);
        assert_eq!(p.memory[0].key, "last_thanked");
        assert_eq!(p.last_seq, 6);
    }

    #[test]
    fn projection_is_deterministic_and_replay_identical() {
        // 设计 §7.3-5：同一事件流重放必然一致
        let records = sample_log();
        let a = project_over(&records, &Base::default());
        let b = project_over(&records, &Base::default());
        assert_eq!(a.messages, b.messages);
        assert_eq!(a.states, b.states);
        assert_eq!(a.blackboard.as_ref().unwrap().clock, "20:00");
    }

    #[test]
    fn state_patch_records_changes_and_deletions() {
        let before = serde_json::json!({"favorability": 50, "mood": "平静", "nested": {"a": 1}});
        let after = serde_json::json!({"favorability": 51, "nested": {"a": 2}});
        let patch = state_patch(&before, &after);
        assert_eq!(patch.len(), 3);
        let keys: Vec<&str> = patch.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(keys, vec!["favorability", "mood", "nested"], "补丁按键名排序（确定性）");
        assert!(patch[1].value.is_null(), "被删掉的键记为 null");
        // 折叠回去应与 after 等价
        let mut p = Projection::default();
        fold(
            &mut p,
            &LogRecord::new(
                1,
                LogBody::Effect(EffectEvent {
                    turn: 1,
                    trigger: "hook.on_message".into(),
                    character: "小雨".into(),
                    state_set: patch,
                    blackboard: vec![],
                    memory: vec![],
                    ts: 0,
                }),
            ),
        );
        assert_eq!(p.state_of("小雨").unwrap(), &after);
    }

    #[test]
    fn genesis_decides_whether_derived_files_are_the_baseline() {
        // 有 genesis（M2 会话）：派生文件不作为基线，否则宫殿会把自己的输出再读一遍
        let records = sample_log();
        assert!(has_genesis(&records));
        let base = Base {
            blackboard: Some(board(9, "23:00")),
            memory: vec![MemRecord {
                kind: "fact".into(),
                key: "old".into(),
                value: serde_json::json!(1),
                source: "hook.on_message".into(),
                turn: 1,
                ts: 1,
            }],
            genesis: true,
            ..Default::default()
        };
        let p = project_over(&records, &base);
        assert_eq!(p.memory.len(), 0, "genesis 会话的记忆只来自事件流");
        assert_eq!(p.blackboard.as_ref().unwrap().clock, "20:00", "init 事件覆盖基线");

        // 无 genesis（M1 老会话）：文件基线 + 新事件继续折叠
        let legacy: Vec<LogRecord> = vec![LogRecord::message(0, msg(1, "user", "你好"))];
        assert!(!has_genesis(&legacy));
        let p = project_over(
            &legacy,
            &Base {
                memory: vec![MemRecord {
                    kind: "fact".into(),
                    key: "old".into(),
                    value: serde_json::json!(1),
                    source: "hook.on_message".into(),
                    turn: 1,
                    ts: 1,
                }],
                ..Default::default()
            },
        );
        assert_eq!(p.memory.len(), 1, "老会话的旧记忆照旧可读");
    }

    #[test]
    fn manual_events_survive_rebuild_derived_ones_do_not() {
        let manual_board = LogRecord::new(
            1,
            LogBody::Blackboard(BlackboardEvent {
                turn: 3,
                reason: "manual".into(),
                board: board(3, "10:00"),
                ts: 1,
            }),
        );
        let clock_board = LogRecord::new(
            2,
            LogBody::Blackboard(BlackboardEvent {
                turn: 3,
                reason: "clock".into(),
                board: board(3, "10:10"),
                ts: 1,
            }),
        );
        let manual_thread = LogRecord::new(
            3,
            LogBody::Thread(ThreadEvent {
                turn: 3,
                op: "open".into(),
                thread_id: "thread.还书".into(),
                thread: None,
                origin: "manual".into(),
                note: None,
                ts: 1,
            }),
        );
        let pipeline_thread = LogRecord::new(
            4,
            LogBody::Thread(ThreadEvent {
                turn: 3,
                op: "open".into(),
                thread_id: "thread.提案".into(),
                thread: None,
                origin: "pipeline".into(),
                note: None,
                ts: 1,
            }),
        );
        assert!(!manual_board.is_derived());
        assert!(clock_board.is_derived());
        assert!(!manual_thread.is_derived());
        assert!(pipeline_thread.is_derived());
        assert!(LogRecord::new(5, effect(1, "k", serde_json::json!(1))).is_derived());
        assert!(!LogRecord::message(6, msg(1, "user", "x")).is_derived());
    }

    #[test]
    fn parse_lines_skips_bad_lines_and_waits_for_half_line() {
        let good = LogRecord::message(0, msg(1, "user", "你好")).to_line().unwrap();
        let mut buf = good.clone().into_bytes();
        buf.extend_from_slice(b"{ not json }\n");
        buf.extend_from_slice(br#"{"kind":"effect""#); // 半行：崩溃残留
        let (records, consumed) = parse_lines(&buf, 1);
        assert_eq!(records.len(), 1, "坏行跳过、半行等待");
        assert_eq!(records[0].seq, 1);
        assert_eq!(consumed, good.len() + "{ not json }\n".len());

        // 续上后半行后能读出来（尾部字段直接拼源码，注意 format! 里 }} 是转义的花括号）
        let mut full = buf.clone();
        full.extend_from_slice(format!(",\"turn\":1,\"trigger\":\"hook.on_message\",\"character\":\"小雨\",\"ts\":0}}\n").as_bytes());
        let (records, _) = parse_lines(&full, 1);
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].seq, 2, "序号按位置重排");
    }

    #[test]
    fn render_lines_renumbers_and_is_stable() {
        let records = vec![
            LogRecord::message(7, msg(1, "user", "你好")),
            LogRecord::new(9, effect(1, "favorability", serde_json::json!(51))),
        ];
        let text = render_lines(&records).unwrap();
        let parsed: Vec<LogRecord> = text
            .lines()
            .map(|l| LogRecord::from_line(l).unwrap())
            .collect();
        assert_eq!(parsed[0].seq, 1);
        assert_eq!(parsed[1].seq, 2);
        assert_eq!(render_lines(&records).unwrap(), text, "渲染是确定的");
    }

    #[test]
    fn messages_view_keeps_only_message_records() {
        let msgs = messages(&sample_log());
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "char");
        assert_eq!(msgs[2].content, "……嗯");
    }

    #[test]
    fn codex_reveal_moves_into_known_set_and_retract_removes_it() {
        let mut records = sample_log();
        records.push(LogRecord::new(
            0,
            LogBody::Codex(CodexEvent {
                turn: 1,
                op: "reveal".into(),
                target: "char.小雨.secrets.工作牌".into(),
                origin: "tree".into(),
                value: None,
                note: None,
                ts: 0,
            }),
        ));
        let p = project_over(&records, &Base::default());
        assert!(p.known.contains("char.小雨.secrets.工作牌"));

        records.push(LogRecord::new(
            0,
            LogBody::Codex(CodexEvent {
                turn: 2,
                op: "retract".into(),
                target: "char.小雨.secrets.工作牌".into(),
                origin: "manual".into(),
                value: None,
                note: None,
                ts: 0,
            }),
        ));
        let p = project_over(&records, &Base::default());
        assert!(p.known.is_empty());
    }

    #[test]
    fn thread_snapshot_folds_last_write_wins() {
        let mut records = vec![LogRecord::new(
            1,
            LogBody::Thread(ThreadEvent {
                turn: 1,
                op: "open".into(),
                thread_id: "thread.还书".into(),
                thread: Some(serde_json::json!({"id":"thread.还书","state":"active"})),
                origin: "pipeline".into(),
                note: None,
                ts: 0,
            }),
        )];
        records.push(LogRecord::new(
            2,
            LogBody::Thread(ThreadEvent {
                turn: 3,
                op: "resolve".into(),
                thread_id: "thread.还书".into(),
                thread: Some(serde_json::json!({"id":"thread.还书","state":"resolved"})),
                origin: "manual".into(),
                note: Some("如约还书".into()),
                ts: 0,
            }),
        ));
        let p = project_over(&records, &Base::default());
        assert_eq!(p.threads["thread.还书"]["state"], "resolved");
        assert_eq!(p.thread_log.len(), 2, "事件本身全部留痕");
    }
}
