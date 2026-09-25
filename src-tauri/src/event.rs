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
use crate::scene::{self, Scene};
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
    /// 钩子运行时所在的场景（M3.2 · 设计 §10.3）：黑板写入按它路由到场景分区。
    /// None = 无场景会话（世界层，兼容旧事件流）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    pub ts: u64,
}

/// 黑板变更（全量快照，折叠时后写覆盖）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlackboardEvent {
    pub turn: u64,
    /// init（建会话基线）/ manual（界面手改）/ clock（轮末时钟步进）/ hook（钩子触发）
    pub reason: String,
    pub board: Blackboard,
    /// 场景分区快照（M3.2）：Some(id) 时 board 的地点/在场者/时间/extra（=场景 flags）
    /// 是**该场景分区**的值，不碰世界层；None = 世界层快照（旧语义，兼容旧事件流）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
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
    /// 谁的状态树转移了（M3.1 按角色隔离；缺省 = 旧会话的单角色）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
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
    /// 见证者（M3.1 · 设计 §10.4）：reveal 只对名单里的角色生效——「某些人知道的事」。
    /// 空 = 全局知情（旧事件与公开设定的兼容语义）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witnesses: Vec<String>,
    pub ts: u64,
}

/// 滚动摘要增量（M2.6 · 设计 §5.3：总结管线产出 L1 摘要，编年史体）
///
/// 注意：LLM 产物**不是派生事件**（重放不会重新调用模型），因此编辑历史时它不会被丢弃——
/// 摘要描述的是「当时总结出来的东西」，可回放性承诺针对的是状态/转移，不是模型输出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryEvent {
    pub turn: u64,
    /// 本次增量（并入 summary.md）
    pub delta: String,
    /// 总结覆盖的批次范围（溯源用）
    #[serde(default)]
    pub from_turn: u64,
    #[serde(default)]
    pub to_turn: u64,
    /// 摘要分卷（M3.2 · 设计 §10.3）：Some(id) = 该场景的分卷；None = 世界层大事记
    /// （仅公开事件）。None 同时兼容旧事件流（全部落世界层）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    pub ts: u64,
}

/// 设定收件箱的一条提案（M2.6 · 设计 §6.9：草稿→确认，确认/否决动作也进事件流）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalEvent {
    pub turn: u64,
    pub id: String,
    /// propose | accept | reject
    pub op: String,
    /// new_entity | new_fact | fact_change | relation | episode | thread | psyche。
    /// **线上键是 `proposal_kind`**：事件流行已有判别键 `kind`（"proposal"），
    /// M3.11 真机验收发现同名互相覆盖——落盘后 codex kind 全变成 "proposal"，
    /// 收件箱确认因此拿不到类型、从未物化过 codex 类提案（补全路径直调
    /// materialize 才幸存）。
    #[serde(default, rename = "proposal_kind")]
    pub kind: String,
    #[serde(default = "default_origin")]
    pub origin: String,
    /// 提案正文（propose 时必带；accept/reject 可省）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub ts: u64,
}

/// 一条记忆对象（M2.6 · 设计 §5.2：情景记忆 episode / 转述 hearsay 等）。
///
/// 与 api.memory.set 的键值事实不同，这是**结构化记忆对象**（带见证者、显著度、关联）。
/// 与摘要/提案同理：它是模型产物，重放不重新生成，故永不丢弃。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub turn: u64,
    /// pipeline（总结管线）/ hook（卡内写入的情景记忆）/ manual
    #[serde(default = "default_origin")]
    pub origin: String,
    /// MemObject 的 JSON 形态（palace::MemObject 可直接反序列化）
    pub object: serde_json::Value,
    pub ts: u64,
}

/// 场景生命周期事件（M3.2 · 设计 §10.3：切场/分场/合场/冻结进事件流，回放可重现）。
///
/// 与摘要/提案同理：场景的创建与切换是**玩家/导演的动作**，不是消息的派生结果，
/// 消息级重建永远保留它们（否则编辑一句台词就会把场景史抹掉）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneEvent {
    pub turn: u64,
    /// create（落地新场景，含缺省场景补落地）/ switch（切场）/ split（分场）/
    /// merge（合场）/ freeze / resume
    pub op: String,
    /// 事件主角场景：create/split = 新场景；switch/freeze/resume = 目标场景；merge = 合入目标
    pub scene_id: String,
    /// 场景全量快照（create/split/merge 必带；折叠时按 id 后写覆盖）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<Scene>,
    /// split = 分场来源（其在场者要扣掉移出名单）；merge = 被并入的场景（置为 merged）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<String>,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub ts: u64,
}

fn default_origin() -> String {
    "manual".into()
}

/// 导演调度里的一名发言人（M3.4 · 设计 §10.5）：谁说话 + 为何轮到她。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorPick {
    /// 角色目录名（会话内唯一键）
    pub dir: String,
    /// 展示名（卡名；与消息署名一致）
    pub name: String,
    /// 打分（调度依据的一部分；点名直通时无意义）
    pub score: f32,
    /// 逐条理由（「被点名提及」「剧情线「X」正被谈到」…）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// 导演调度事件（M3.4 · 设计 §10.5）：发言权调度落流，回放可重现整场调度史。
///
/// 与场景事件同理：调度是**元层的动作**，不是消息的派生结果——消息级重建
/// 永远保留它们（否则编辑一句台词就会把调度史抹掉）。调度只决定谁说话，
/// 不产生任何投影副作用（fold 对它是 no-op）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorEvent {
    pub turn: u64,
    /// schedule（发言权调度）；后续：cut（切场）/ advance（时间推进）/ open·close（开收线）
    pub op: String,
    /// 选中的发言人（按发言顺序）
    pub picks: Vec<DirectorPick>,
    /// 用户点名的发言人（直通，不经打分）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub ts: u64,
}

/// 导演树阶段转移事件（M3.6 · 设计 §8.5/§10.5）：起承转合的推进落流，回放可重现。
///
/// 与调度事件同理：阶段转移是**元层的动作**，不是消息的派生结果——消息级重建
/// 永远保留它们。折叠进 `Projection.director_tree`（按序追加），当前活跃路径 =
/// 最后一条的 `to`（没有事件时 = 树根）；转移历史即整棵树的走位记录，
/// 供「进度指示」与导演面板回放。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorTreeEvent {
    pub turn: u64,
    /// 转移前的活跃路径（根→叶）；剧场开场播种时为空
    pub from: Vec<String>,
    /// 转移后的活跃路径（根→叶）
    pub to: Vec<String>,
    /// 转移理由（求值命中的 when 描述，给人读）
    pub reason: String,
    pub ts: u64,
}

/// 世界主线阶段转移（M3.7 · 设计 §6.6）：世界作用域状态树的走位史。
/// 与 [`DirectorTreeEvent`] 同构——世界大势压着所有角色，转移史按序追加，
/// 会话里只记「本会话见证的走位」；跨会话的持久进度在 `codex/<世界>/world.json`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldlineEvent {
    pub turn: u64,
    /// 转移前的活跃路径（根→叶）；开局承袭时为空
    pub from: Vec<String>,
    /// 转移后的活跃路径（根→叶）
    pub to: Vec<String>,
    /// 转移理由（求值命中的 when 描述，给人读）
    pub reason: String,
    pub ts: u64,
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
    Summary(SummaryEvent),
    Proposal(ProposalEvent),
    Memory(MemoryEvent),
    Scene(SceneEvent),
    Director(DirectorEvent),
    DirectorTree(DirectorTreeEvent),
    Worldline(WorldlineEvent),
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
            LogBody::Summary(_) => "summary",
            LogBody::Proposal(_) => "proposal",
            LogBody::Memory(_) => "memory",
            LogBody::Scene(_) => "scene",
            LogBody::Director(_) => "director",
            LogBody::DirectorTree(_) => "director_tree",
            LogBody::Worldline(_) => "worldline",
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
            LogBody::Summary(s) => s.turn,
            LogBody::Proposal(p) => p.turn,
            LogBody::Memory(m) => m.turn,
            LogBody::Scene(s) => s.turn,
            LogBody::Director(d) => d.turn,
            LogBody::DirectorTree(d) => d.turn,
            LogBody::Worldline(d) => d.turn,
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
            LogBody::Summary(s) => serde_json::to_value(s),
            LogBody::Proposal(p) => serde_json::to_value(p),
            LogBody::Memory(m) => serde_json::to_value(m),
            LogBody::Scene(s) => serde_json::to_value(s),
            LogBody::Director(d) => serde_json::to_value(d),
            LogBody::DirectorTree(d) => serde_json::to_value(d),
            LogBody::Worldline(d) => serde_json::to_value(d),
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
            "summary" => serde_json::from_value(v).map(LogBody::Summary).map_err(bad),
            "proposal" => serde_json::from_value(v).map(LogBody::Proposal).map_err(bad),
            "memory" => serde_json::from_value(v).map(LogBody::Memory).map_err(bad),
            "scene" => serde_json::from_value(v).map(LogBody::Scene).map_err(bad),
            "director" => serde_json::from_value(v).map(LogBody::Director).map_err(bad),
            "director_tree" => {
                serde_json::from_value(v).map(LogBody::DirectorTree).map_err(bad)
            }
            "worldline" => serde_json::from_value(v).map(LogBody::Worldline).map_err(bad),
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
            // 线事件：manual 是玩家手动动作；director 是导演树的调度动作（M3.6）——
            // 两者都是元层动作，重建不重跑导演，必须原样保留。
            // pipeline / psyche 由重建时的同一份管线代码重演（转述/心里话消费走重放路径）。
            LogBody::Thread(t) => t.origin != "manual" && t.origin != "director",
            // 只有**状态树**的揭示算派生（重建时会由 advance_state_tree 重导）；
            // 总结管线转述带来的揭示（M3.3 · §10.4，origin = pipeline）是模型产物——
            // 与它伴随写入的转述记忆同理，重放不重新调用模型，编辑历史不得丢弃。
            LogBody::Codex(c) => c.origin == "tree",
            // 摘要与提案是**模型产物**，不是确定性派生：重放不重新调用模型，
            // 所以它们永远保留（编辑历史只重算状态/转移/心理，设计 §7.3-5 的承诺范围）。
            // 场景事件同理：切场/分场/合场是玩家/导演的动作，不从消息派生。
            // 导演调度同理（M3.4）：调度史是元层的动作记录，重建不得抹掉。
            // 导演树阶段转移同理（M3.6）：起承转合的走位史，重建保留。
            // 世界主线转移同理（M3.7）：世界大势的走位史，重建保留。
            LogBody::Summary(_)
            | LogBody::Proposal(_)
            | LogBody::Memory(_)
            | LogBody::Scene(_)
            | LogBody::Director(_)
            | LogBody::DirectorTree(_)
            | LogBody::Worldline(_) => false,
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
    /// 秘密揭示集（"实体.秘密" 路径）——全局知情（无见证者的 reveal，兼容旧事件流）
    pub known: std::collections::BTreeSet<String>,
    /// 见证者视角的揭示集（M3.1 · 设计 §10.4）：角色 → 只对她揭示的路径。
    /// 组装视角的有效知情集 = known ∪ known_of[视角]（[Projection::known_for]）
    pub known_of: BTreeMap<String, std::collections::BTreeSet<String>>,
    pub codex_log: Vec<CodexEvent>,
    /// L1 滚动摘要（summary 事件按序拼接，设计 §5.3）
    pub summary: String,
    /// 摘要已覆盖到哪一轮（批次从它之后取；0 = 还没总结过）
    pub summary_upto: u64,
    /// 设定收件箱：提案 id → 当前状态（propose/accept/reject 后写覆盖）
    pub proposals: BTreeMap<String, serde_json::Value>,
    /// 结构化记忆对象（M2.6 管线写入的情景记忆；键值事实仍在 memory 里）
    pub episodes: Vec<serde_json::Value>,
    /// 场景表（M3.2 · 设计 §10.3）：场景本体含黑板分区（地点/在场者/局部时钟/flags）。
    /// 空 = 无场景会话（世界层黑板单场景，M2 行为）。
    pub scenes: BTreeMap<String, Scene>,
    /// 当前聚焦的场景（切场的落点）；Some 时必是 scenes 里的 active 场景
    pub active_scene: Option<String>,
    /// 摘要分卷：场景 id → 该场景的滚动摘要（scene_id 落值的 summary 事件折叠于此）
    pub scene_summaries: BTreeMap<String, String>,
    /// 摘要水位（场景维度）：场景 id → 已总结到哪一轮（批次按场景取）
    pub summary_upto_of: BTreeMap<String, u64>,
    /// 导演树走位史（M3.6 · 设计 §8.5）：起承转合的阶段转移按序追加；
    /// 当前活跃路径 = 最后一条的 to（空 = 还没开场，用树根）
    pub director_tree: Vec<DirectorTreeEvent>,
    /// 世界主线走位史（M3.7 · 设计 §6.6）：本会话见证的世界阶段转移按序追加；
    /// 当前活跃路径 = 最后一条的 to（空 = 开局承袭 world.json 的世界进度）
    pub worldline: Vec<WorldlineEvent>,
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

    /// 某个组装视角的有效知情集：全局揭示 ∪ 只对她的揭示（M3.1 视角化）
    pub fn known_for(&self, character: &str) -> std::collections::BTreeSet<String> {
        let mut set = self.known.clone();
        if let Some(extra) = self.known_of.get(character) {
            set.extend(extra.iter().cloned());
        }
        set
    }

    /// 消息归属的场景（读侧归一：None = 缺省场景）
    pub fn scene_of_message(&self, m: &Message) -> String {
        scene::normalize(m.scene_id.as_deref()).to_string()
    }

    /// 某场景的黑板分区（地点/在场者/局部时钟/flags）；场景不存在返回 None
    pub fn scene_partition(&self, scene_id: Option<&str>) -> Option<&Scene> {
        let id = scene_id?;
        self.scenes.get(id)
    }

    /// 当前聚焦场景 id（多场景会话才有；单场景/老会话 = None，读侧退化为世界层）
    pub fn active_scene_id(&self) -> Option<&str> {
        self.active_scene.as_deref()
    }

    /// 组装用的**有效黑板**（M3.2 · 设计 §10.3）：世界层（时间基准/实体键/世界 flags）
    /// ∪ 该场景分区（地点/在场者/局部时钟/场景 flags）。场景分区有值就以它为准；
    /// 场景不存在（老会话/单场景）= 世界层黑板原样（M2 行为不变）。
    pub fn effective_board(&self, scene_id: Option<&str>) -> Blackboard {
        let mut board = self.blackboard.clone().unwrap_or_else(Blackboard::default_board);
        let Some(part) = self.scene_partition(scene_id) else {
            return board;
        };
        board.day = part.day;
        board.clock = part.clock.clone();
        board.place = part.place.clone();
        if !part.actors.is_empty() {
            board.actors = part.actors.clone();
        }
        for (k, v) in &part.flags {
            board.extra.insert(k.clone(), v.clone());
        }
        board
    }

    /// 组装注入用的滚动摘要：世界层大事记 + 该场景分卷（摘要分卷防跨视角泄漏，
    /// 设计 §10.4）。场景无分卷时只有世界层；两者皆空返回 None。
    pub fn summary_for(&self, scene_id: Option<&str>) -> Option<String> {
        let world = self.summary.trim();
        let scene_text = scene_id
            .and_then(|id| self.scene_summaries.get(id))
            .map(|s| s.trim())
            .unwrap_or("");
        let mut parts: Vec<String> = Vec::new();
        if !world.is_empty() {
            parts.push(world.to_string());
        }
        if !scene_text.is_empty() {
            parts.push(scene_text.to_string());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    /// 某场景的摘要水位（批次从它之后取）。场景建立时水位即落 0（各场景独立记账）；
    /// 没有条目的场景（老会话的归一缺省场景）退回全局水位——旧语义按全局轮次取批次。
    pub fn summary_upto_for(&self, scene_id: &str) -> u64 {
        self.summary_upto_of
            .get(scene_id)
            .copied()
            .unwrap_or(self.summary_upto)
    }

    /// 会话是否已有显式场景（多场景会话的判据；单场景/老会话 = false）
    pub fn has_scenes(&self) -> bool {
        !self.scenes.is_empty()
    }
}

/// 全量投影计数（加固 D1 的验收仪表）：一轮对话（构造用例）的全量 fold 应 ≤2，
/// 增量投影生效后通常为 0——测试据此断言「投影次数不再随会话长度线性恶化」。
pub static FULL_FOLD_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 在基线上折叠事件流
pub fn project_over(records: &[LogRecord], base: &Base) -> Projection {
    FULL_FOLD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

/// 把一条场景事件折叠进投影（场景生命周期的不变量在这里维持：
/// 至多一个活跃聚焦场景；被切走的场景冻结；被合并的场景归档）。
pub fn fold_scene(p: &mut Projection, s: &SceneEvent) {
    match s.op.as_str() {
        "create" | "split" => {
            if let Some(sc) = &s.scene {
                let mut sc = sc.clone();
                if !sc.is_active() {
                    sc.status = scene::STATUS_ACTIVE.into();
                }
                // 场景建立即开一本自己的摘要账（水位从 0 起算，与全局水位脱钩）
                p.summary_upto_of.entry(sc.id.clone()).or_insert(0);
                p.scenes.insert(sc.id.clone(), sc);
            }
            if s.op == "split" {
                // 来源场景扣掉移出的在场者并冻结（others[0] = 分场来源）
                let moved = p
                    .scenes
                    .get(&s.scene_id)
                    .map(|sc| sc.actors.clone())
                    .unwrap_or_default();
                if let Some(parent) = s.others.first().and_then(|id| p.scenes.get_mut(id)) {
                    parent.actors.retain(|a| !moved.contains(a));
                    parent.status = scene::STATUS_FROZEN.into();
                }
            }
            // 视角带到新场景：首个落地 = 缺省场景接管；分场/显式新建 = 直接去新舞台
            p.active_scene = Some(s.scene_id.clone());
        }
        "switch" => {
            // 被切走的场景冻结（设计 §10.3：冻结 ≠ 删除，切回来原地继续）
            if let Some(prev) = p.active_scene.clone() {
                if prev != s.scene_id {
                    if let Some(sc) = p.scenes.get_mut(&prev) {
                        sc.status = scene::STATUS_FROZEN.into();
                    }
                }
            }
            if let Some(sc) = p.scenes.get_mut(&s.scene_id) {
                sc.status = scene::STATUS_ACTIVE.into();
                // 世界层时间基准跟随视角（世界时钟=「现在」；回忆场景靠 versions，M3.7）
                let (day, clock) = (sc.day, sc.clock.clone());
                if let Some(world) = p.blackboard.as_mut() {
                    world.day = day;
                    world.clock = clock;
                }
            }
            p.active_scene = Some(s.scene_id.clone());
        }
        "merge" => {
            if let Some(sc) = &s.scene {
                p.scenes.insert(s.scene_id.clone(), sc.clone());
            }
            // 被并入的场景归档：留档可查，不再推进
            for id in &s.others {
                if let Some(sc) = p.scenes.get_mut(id) {
                    sc.status = scene::STATUS_MERGED.into();
                }
                if p.active_scene.as_deref() == Some(id.as_str()) {
                    p.active_scene = Some(s.scene_id.clone());
                }
            }
            p.active_scene = Some(s.scene_id.clone());
        }
        "freeze" => {
            // 冻结活跃场景没有意义（没有聚焦的会话无法推进），防御性忽略
            if p.active_scene.as_deref() != Some(s.scene_id.as_str()) {
                if let Some(sc) = p.scenes.get_mut(&s.scene_id) {
                    sc.status = scene::STATUS_FROZEN.into();
                }
            }
        }
        "resume" => {
            if let Some(sc) = p.scenes.get_mut(&s.scene_id) {
                if sc.status == scene::STATUS_FROZEN {
                    sc.status = scene::STATUS_ACTIVE.into();
                }
            }
        }
        // 手动编辑分区（地点/在场者/局部时钟/标题）：快照整体替换
        "update" => {
            if let Some(sc) = &s.scene {
                p.scenes.insert(s.scene_id.clone(), sc.clone());
            }
        }
        _ => {}
    }
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
                // 黑板写入按钩子所在场景路由（M3.2）：地点/在场者/时钟进场景分区，
                // 实体键与世界 flags 归世界层；无场景 = 世界层（M2 语义）
                let scene_part = e.scene_id.as_deref().and_then(|id| p.scenes.get_mut(id));
                let bb = p.blackboard.get_or_insert_with(Blackboard::default_board);
                apply_blackboard_sets_scoped(bb, scene_part, &e.blackboard);
                mirror_active_scene(p);
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
        LogBody::Blackboard(b) => {
            match b.scene_id.as_deref() {
                None => {
                    // 世界层快照（init/manual/clock 的旧语义）：活跃场景分区保持同步
                    p.blackboard = Some(b.board.clone());
                    if let Some(id) = p.active_scene.clone() {
                        if let Some(sc) = p.scenes.get_mut(&id) {
                            sync_scene_from_board(sc, &b.board);
                        }
                    }
                }
                Some(id) => {
                    // 场景分区快照（M3.2）：只动该场景；extra 整体 = 场景 flags
                    if let Some(sc) = p.scenes.get_mut(id) {
                        sync_scene_from_board(sc, &b.board);
                        sc.flags = b.board.extra.clone();
                    }
                }
            }
            mirror_active_scene(p);
        }
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
                    if c.witnesses.is_empty() {
                        p.known.insert(c.target.clone());
                    } else {
                        // 见证者视角的揭示（M3.1）：只有名单里的角色知道——串台隔离的数据基础
                        for w in &c.witnesses {
                            p.known_of
                                .entry(w.clone())
                                .or_default()
                                .insert(c.target.clone());
                        }
                    }
                }
                "retract" => {
                    p.known.remove(&c.target);
                    for set in p.known_of.values_mut() {
                        set.remove(&c.target);
                    }
                }
                _ => {}
            }
            p.codex_log.push(c.clone());
        }
        LogBody::Summary(s) => {
            let delta = s.delta.trim();
            if !delta.is_empty() {
                match s.scene_id.as_deref() {
                    // 场景分卷（M3.2 · 设计 §10.4：摘要分卷防跨视角泄漏）
                    Some(id) => {
                        let volume = p.scene_summaries.entry(id.to_string()).or_default();
                        if !volume.is_empty() {
                            volume.push('\n');
                        }
                        volume.push_str(delta);
                    }
                    // 世界层大事记（None 兼容旧事件流：全部落世界层）
                    None => {
                        if !p.summary.is_empty() {
                            p.summary.push('\n');
                        }
                        p.summary.push_str(delta);
                    }
                }
            }
            // 水位按场景记账（None 归一为缺省场景，保底进全局水位供旧读取方使用）
            let scene_key = scene::normalize(s.scene_id.as_deref()).to_string();
            let upto = p.summary_upto_of.entry(scene_key).or_insert(0);
            *upto = (*upto).max(s.to_turn);
            p.summary_upto = p.summary_upto.max(s.to_turn);
        }
        LogBody::Memory(m) => p.episodes.push(m.object.clone()),
        LogBody::Proposal(pr) => {
            // 提案是状态机：propose 落条目，accept/reject 改状态（payload 缺失时保留原提案正文）
            let entry = p
                .proposals
                .entry(pr.id.clone())
                .or_insert_with(|| serde_json::json!({}));
            if !entry.is_object() {
                *entry = serde_json::json!({});
            }
            let obj = entry.as_object_mut().expect("上面刚保证是对象");
            obj.insert("id".into(), serde_json::json!(pr.id));
            obj.insert("status".into(), serde_json::json!(pr.op));
            if !pr.kind.is_empty() {
                obj.insert("kind".into(), serde_json::json!(pr.kind));
            }
            // 来源（M3.8）：pipeline / complete / improv / manual——收件箱的来源徽标与
            // 「设定·暂定」注入回读（B2 improv 行）都靠它
            if !pr.origin.is_empty() {
                obj.insert("origin".into(), serde_json::json!(pr.origin));
            }
            obj.insert("turn".into(), serde_json::json!(pr.turn));
            if let Some(payload) = &pr.payload {
                obj.insert("payload".into(), payload.clone());
            }
            if let Some(note) = &pr.note {
                obj.insert("note".into(), serde_json::json!(note));
            }
        }
        LogBody::Scene(s) => fold_scene(p, s),
        // 导演调度只决定谁说话，不产生任何会话状态（设计 §10.5「输出只有调度动作」）
        LogBody::Director(_) => {}
        // 导演树阶段转移（M3.6）：按序追加——当前活跃路径 = 最后一条的 to，
        // 转移历史即起承转合的走位记录（进度指示与导演面板的数据源）
        LogBody::DirectorTree(t) => p.director_tree.push(t.clone()),
        // 世界主线阶段转移（M3.7）：按序追加——当前活跃路径 = 最后一条的 to，
        // 走位史即世界大势的演变记录（B1 时代行 / B2 世界 directive 的数据源）
        LogBody::Worldline(t) => p.worldline.push(t.clone()),
    }
}

/// 场景分区 ← 黑板形态的快照（时间/地点/在场者；extra 由调用方决定是否当 flags 整体替换）
fn sync_scene_from_board(sc: &mut Scene, board: &Blackboard) {
    sc.day = board.day;
    sc.clock = board.clock.clone();
    sc.place = board.place.clone();
    if !board.actors.is_empty() {
        sc.actors = board.actors.clone();
    }
}

/// 世界层黑板跟随活跃场景（世界时钟 = 「现在」的镜像；读侧有效黑板以分区为准，
/// 这份镜像服务于派生文件 blackboard.json 与无场景读侧路径的可见性）
fn mirror_active_scene(p: &mut Projection) {
    let Some(id) = p.active_scene.clone() else {
        return;
    };
    let Some(sc) = p.scenes.get(&id) else {
        return;
    };
    let Some(world) = p.blackboard.as_mut() else {
        return;
    };
    world.day = sc.day;
    world.clock = sc.clock.clone();
    world.place = sc.place.clone();
    if !sc.actors.is_empty() {
        world.actors = sc.actors.clone();
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

/// 场景感知的黑板写入路由（M3.2 · 设计 §10.3）：
///
/// - `day`/`clock` → 世界层时间基准，并同步场景局部时钟（时间是「世界的时间」，
///   但被冻结的场景不能被别的场景推着走，所以分区里各留一份）；
/// - `place`/`actors` → 场景分区（地点与在场者是**这个舞台**的属性）；无场景 = 世界层（M2 语义）；
/// - 其余键（实体作用域键、世界 flags）→ 世界层 extra——设定集是世界的知识，不随场景走。
///
/// `scene` = 写入者所在场景的分区（折叠时来自 EffectEvent.scene_id）。
pub fn apply_blackboard_sets_scoped(
    world: &mut Blackboard,
    mut scene: Option<&mut Scene>,
    sets: &[KvSet],
) -> bool {
    let before = format!("{world:?}");
    for kv in sets {
        let value = &kv.value;
        match (kv.key.as_str(), value, scene.as_deref_mut()) {
            ("day", serde_json::Value::Number(n), sc) => {
                if let Some(day) = n.as_i64() {
                    world.day = day;
                    if let Some(sc) = sc {
                        sc.day = day;
                    }
                }
            }
            ("clock", serde_json::Value::String(s), sc) => {
                world.clock = s.clone();
                if let Some(sc) = sc {
                    sc.clock = s.clone();
                }
            }
            ("place", serde_json::Value::String(s), sc) => match sc {
                Some(sc) => sc.place = s.clone(),
                None => world.place = s.clone(),
            },
            ("actors", serde_json::Value::Array(arr), sc) => {
                let list: Vec<String> = arr
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect();
                match sc {
                    Some(sc) => sc.actors = list,
                    None => world.actors = list,
                }
            }
            // 实体作用域键与世界 flags：世界的知识，不随场景走
            (key, v, _) if key.contains('.') => {
                if v.is_null() {
                    world.extra.remove(key);
                } else {
                    world.extra.insert(key.to_string(), v.clone());
                }
            }
            _ => {}
        }
    }
    format!("{world:?}") != before
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

    /// M3.11 真机验收发现的缺陷：ProposalEvent 自身的 codex kind 与事件流行的
    /// 判别键 `kind` 同名，to_value 的判别插入把它覆盖成 "proposal"——落盘即失真，
    /// 收件箱确认读不到类型、codex 类提案从未物化进 grown.json。
    /// 线上键改 `proposal_kind` 后往返必须保真。
    #[test]
    fn proposal_kind_survives_the_log_roundtrip() {
        let ev = ProposalEvent {
            turn: 4,
            id: "codex.char.小雨.4.0".into(),
            op: "propose".into(),
            kind: "new_fact".into(),
            origin: "pipeline".into(),
            payload: Some(serde_json::json!({
                "target": "char.小雨",
                "value": { "facet": "schedule", "value": "周三休息" },
                "reason": "第 4 轮提到",
            })),
            note: None,
            ts: 0,
        };
        let v = LogBody::Proposal(ev.clone()).to_value().unwrap();
        // 行判别键仍是 "proposal"（from_value 靠它分派）
        assert_eq!(v["kind"], "proposal");
        // codex kind 活在 proposal_kind 里，不再被覆盖
        assert_eq!(v["proposal_kind"], "new_fact");
        let back = match LogBody::from_value(v).unwrap() {
            LogBody::Proposal(p) => p,
            other => panic!("应还原为提案事件：{other:?}"),
        };
        assert_eq!(back.kind, "new_fact");
        assert_eq!(back, ev);
        // 投影折叠后收件箱条目带 codex kind（materialize 靠它分流）
        let mut proj = Projection::default();
        fold(
            &mut proj,
            &LogRecord {
                seq: 1,
                body: LogBody::Proposal(back),
            },
        );
        assert_eq!(
            proj.proposals["codex.char.小雨.4.0"]["kind"], "new_fact",
            "收件箱条目的 kind 应是 codex 类型"
        );
    }


    fn msg(turn: u64, role: &str, content: &str) -> Message {
        Message {
        name: None,
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
            scene_id: None,
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
                    scene_id: None,
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
                scene_id: None,
                ts: 9,
            }),
            LogBody::Transition(TransitionEvent {
            character: None,
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
            witnesses: Vec::new(),
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
                scene_id: None,
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
                    scene_id: None,
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
                scene_id: None,
                ts: 1,
            }),
        );
        let clock_board = LogRecord::new(
            2,
            LogBody::Blackboard(BlackboardEvent {
                turn: 3,
                reason: "clock".into(),
                board: board(3, "10:10"),
                scene_id: None,
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

        // 设定揭示按来源分道（M3.3）：状态树的会随重建重导（派生），
        // 总结管线转述带来的不会（模型产物，重放不重调模型）
        let codex = |origin: &str| {
            LogRecord::new(
                7,
                LogBody::Codex(CodexEvent {
                    turn: 3,
                    op: "reveal".into(),
                    target: "char.小雨.secrets.工作牌".into(),
                    origin: origin.into(),
                    value: None,
                    note: None,
                    witnesses: vec!["阿澈".into()],
                    ts: 1,
                }),
            )
        };
        assert!(codex("tree").is_derived());
        assert!(!codex("pipeline").is_derived());
        assert!(!codex("manual").is_derived());
    }

    #[test]
    fn director_event_roundtrips_and_is_never_derived() {
        // 调度史是元层动作（M3.4 · §10.5）：永远保留、回放不产生副作用
        let ev = DirectorEvent {
            turn: 7,
            op: "schedule".into(),
            picks: vec![DirectorPick {
                dir: "xiaoyu".into(),
                name: "小雨".into(),
                score: 4.5,
                reasons: vec!["被点名提及".into(), "剧情线「周五还书」正被谈到".into()],
            }],
            direct: None,
            note: None,
            ts: 12,
        };
        assert!(!LogRecord::new(0, LogBody::Director(ev.clone())).is_derived());

        let rec = LogRecord::new(3, LogBody::Director(ev));
        let line = rec.to_line().unwrap();
        assert!(line.contains(r#""kind":"director""#), "{line}");
        assert!(line.contains(r#""reasons":["被点名提及""#), "{line}");
        let back = LogRecord::from_line(&line).unwrap();
        assert!(matches!(back.body, LogBody::Director(ref d) if d.turn == 7 && d.picks.len() == 1));
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
            witnesses: Vec::new(),
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
            witnesses: Vec::new(),
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


#[cfg(test)]
mod scene_tests {
    //! M3.2 验收用例（设计 §10.3）：双场景互不污染、分场合场记忆不合并、重放一致。

    use super::*;
    use crate::scene::{self, Scene};

    fn scene(id: &str, title: &str, place: &str, actors: &[&str], day: i64, clock: &str) -> Scene {
        Scene {
            id: id.into(),
            title: title.into(),
            place: place.into(),
            actors: actors.iter().map(|s| s.to_string()).collect(),
            day,
            clock: clock.into(),
            flags: Default::default(),
            created_turn: 0,
            origin: "manual".into(),
            parent: None,
            status: scene::STATUS_ACTIVE.into(),
            ts: 1,
        }
    }

    fn create(sc: &Scene, turn: u64) -> LogRecord {
        LogRecord::new(
            0,
            LogBody::Scene(SceneEvent {
                turn,
                op: "create".into(),
                scene_id: sc.id.clone(),
                scene: Some(sc.clone()),
                others: Vec::new(),
                origin: "manual".into(),
                note: None,
                ts: 1,
            }),
        )
    }

    fn msg_in(turn: u64, role: &str, content: &str, scene_id: Option<&str>) -> LogRecord {
        LogRecord::message(
            0,
            Message {
                turn,
                role: role.into(),
                content: content.into(),
                ts: 100 + turn,
                scene_id: scene_id.map(str::to_string),
                name: None,
            },
        )
    }

    /// 场景内钩子写黑板（effect 带 scene_id）
    fn effect_in(turn: u64, scene_id: &str, key: &str, value: serde_json::Value) -> LogRecord {
        LogRecord::new(
            0,
            LogBody::Effect(EffectEvent {
                turn,
                trigger: "hook.on_message".into(),
                character: "阿澈".into(),
                state_set: vec![],
                blackboard: vec![KvSet {
                    key: key.into(),
                    value,
                }],
                memory: vec![],
                scene_id: Some(scene_id.into()),
                ts: 1,
            }),
        )
    }

    fn two_scene_stream() -> Vec<LogRecord> {
        let mut records = vec![
            LogRecord::new(
                0,
                LogBody::Blackboard(BlackboardEvent {
                    turn: 0,
                    reason: "init".into(),
                    board: Blackboard {
                        day: 1,
                        clock: "20:00".into(),
                        place: "图书馆".into(),
                        actors: vec!["阿澈".into(), "小雨".into()],
                        extra: Default::default(),
                    },
                    scene_id: None,
                    ts: 1,
                }),
            ),
            create(
                &scene("scene.main", "开场", "图书馆", &["阿澈", "小雨"], 1, "20:00"),
                0,
            ),
            // A 场景推进：阿澈写地点「天台」
            msg_in(1, "user", "我们去天台吧", Some("scene.main")),
            effect_in(1, "scene.main", "place", serde_json::json!("天台")),
        ];
        // 「与此同时」——B 场景开线（小雨在书店）
        let b = scene("scene.b", "书店", "旧书店", &["小雨"], 1, "20:10");
        records.push(create(&b, 1));
        records.push(msg_in(2, "user", "小雨在书店翻书", Some("scene.b")));
        records
    }

    #[test]
    fn two_scenes_never_leak_into_each_other() {
        // DoD 第 4 项的场景版：仅 A 场景发生的事，不进 B 场景的任何注入层
        let p = project_over(&two_scene_stream(), &Base::default());
        assert_eq!(p.active_scene.as_deref(), Some("scene.b"), "视角跟到新场景");

        // 黑板分区：A 场景的钩子写了 place=天台，B 场景分区仍是书店
        let a = p.effective_board(Some("scene.main"));
        let b = p.effective_board(Some("scene.b"));
        assert_eq!(a.place, "天台");
        assert_eq!(b.place, "旧书店", "B 场景分区不被 A 场景写入污染");
        assert_eq!(b.actors, vec!["小雨"], "B 场景在场者独立");

        // 消息流分段：B 场景组装的历史只有 B 场景的消息
        let b_history = p
            .messages
            .iter()
            .filter(|m| scene::normalize(m.scene_id.as_deref()) == "scene.b")
            .count();
        assert_eq!(b_history, 1, "B 场景看不到 A 场景的台词");

        // 摘要分卷：A 卷 B 卷互不可见
        let mut p2 = p.clone();
        fold(
            &mut p2,
            &LogRecord::new(
                0,
                LogBody::Summary(SummaryEvent {
                    turn: 2,
                    delta: "天台上的告白".into(),
                    from_turn: 1,
                    to_turn: 2,
                    scene_id: Some("scene.main".into()),
                    ts: 1,
                }),
            ),
        );
        let b_summary = p2.summary_for(Some("scene.b")).unwrap_or_default();
        assert!(
            !b_summary.contains("天台上的告白"),
            "B 场景摘要里没有 A 场景的分卷"
        );
        let a_summary = p2.summary_for(Some("scene.main")).unwrap_or_default();
        assert!(a_summary.contains("天台上的告白"), "A 场景读得到自己的分卷");
    }

    #[test]
    fn split_then_merge_keeps_memory_per_viewer_and_replay_identical() {
        let mut records = two_scene_stream();
        // 分场：小雨从 main 移出另立 scene.c（纯函数算快照，命令层同款）
        let main = project_over(&records, &Base::default())
            .scenes
            .get("scene.main")
            .cloned()
            .unwrap();
        let (_next_main, c) = main
            .split_from("scene.c", "天台一角", "天台", &["小雨".to_string()], 9)
            .unwrap();
        records.push(LogRecord::new(
            0,
            LogBody::Scene(SceneEvent {
                turn: 2,
                op: "split".into(),
                scene_id: "scene.c".into(),
                scene: Some(c),
                others: vec!["scene.main".into()],
                origin: "manual".into(),
                note: None,
                ts: 9,
            }),
        ));
        // 合场：c 并回 main（先切回 main 再合）
        records.push(LogRecord::new(
            0,
            LogBody::Scene(SceneEvent {
                turn: 3,
                op: "switch".into(),
                scene_id: "scene.main".into(),
                scene: None,
                others: Vec::new(),
                origin: "manual".into(),
                note: None,
                ts: 10,
            }),
        ));
        let mut p = project_over(&records, &Base::default());
        let sources = vec![p.scenes.get("scene.c").cloned().unwrap()];
        let merged = p
            .scenes
            .get("scene.main")
            .cloned()
            .unwrap()
            .merge_into(&sources, 11);
        records.push(LogRecord::new(
            0,
            LogBody::Scene(SceneEvent {
                turn: 3,
                op: "merge".into(),
                scene_id: "scene.main".into(),
                scene: Some(merged),
                others: vec!["scene.c".into()],
                origin: "manual".into(),
                note: None,
                ts: 11,
            }),
        ));

        let p = project_over(&records, &Base::default());
        // 分场扣人、合场归档与并集
        assert_eq!(
            p.scenes["scene.main"].actors,
            vec!["阿澈", "小雨"],
            "合场 = 在场者并集"
        );
        assert_eq!(
            p.scenes["scene.c"].status,
            scene::STATUS_MERGED,
            "被并入的场景归档"
        );
        assert_eq!(p.active_scene.as_deref(), Some("scene.main"));

        // 记忆不合并：合场不改记忆层（各自记得自己线里的事）
        let before_mem = project_over(&records[..records.len() - 1], &Base::default()).memory.len();
        assert_eq!(p.memory.len(), before_mem, "合场零记忆写入");

        // 重放一致（设计 §7.3-5 的场景版）
        let again = project_over(&records, &Base::default());
        assert_eq!(p.scenes, again.scenes);
        assert_eq!(p.active_scene, again.active_scene);
        assert_eq!(p.messages, again.messages);
    }

    #[test]
    fn scene_events_survive_message_level_rebuild_semantics() {
        // 场景事件不是派生事件：编辑历史（丢弃派生）后场景表原样保留
        let records = two_scene_stream();
        let derived_dropped: Vec<LogRecord> = records
            .iter()
            .filter(|r| !r.is_derived())
            .cloned()
            .collect();
        let p = project_over(&derived_dropped, &Base::default());
        assert_eq!(p.scenes.len(), 2, "场景事件全保留");
        assert_eq!(p.active_scene.as_deref(), Some("scene.b"));
        // 而 A 场景的钩子写入（派生）被丢弃：分区不再含「天台」
        assert_eq!(p.effective_board(Some("scene.main")).place, "图书馆");
    }

    #[test]
    fn scene_scoped_clock_events_advance_only_their_scene() {
        let mut records = two_scene_stream();
        // B 场景的时钟步进：不动 A 场景
        records.push(LogRecord::new(
            0,
            LogBody::Blackboard(BlackboardEvent {
                turn: 2,
                reason: "clock".into(),
                scene_id: Some("scene.b".into()),
                board: Blackboard {
                    day: 1,
                    clock: "20:20".into(),
                    place: "旧书店".into(),
                    actors: vec!["小雨".into()],
                    extra: Default::default(),
                },
                ts: 2,
            }),
        ));
        let p = project_over(&records, &Base::default());
        assert_eq!(p.scenes["scene.b"].clock, "20:20");
        assert_eq!(
            p.scenes["scene.main"].clock, "20:00",
            "A 场景时间停在被切走那一刻"
        );
        // 世界层镜像跟随活跃场景（B）
        assert_eq!(p.blackboard.as_ref().unwrap().clock, "20:20");
    }

    #[test]
    fn scene_event_roundtrips_through_json() {
        let rec = create(&scene("scene.x", "标题", "地点", &["阿澈"], 2, "21:00"), 3);
        let line = rec.to_line().unwrap();
        assert!(line.contains("\"kind\":\"scene\""));
        let back = LogRecord::from_line(line.trim()).unwrap();
        assert_eq!(back.body, rec.body);
    }
}
