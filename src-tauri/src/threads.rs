// 剧情线（Thread）：起因与结果的一等公民（设计 §8）
//
// 六要素审计里，时间/地点/人物/经过早就是一等公民（黑板 + 现状卡 + 状态树 + 摘要），
// 起因与结果此前只以「未决事项清单」隐式存在（§8.1）。剧情线把它们做成显式、可操作、
// 可注入的结构化数据：一条线 = 起因（cause）+ 经过（progress，可挂宫殿记忆）
// + 结果（resolution）+ **提及时机**（resurface）。
//
// 反过来还有第二个隐患（§8.1）：**每轮无条件强调所有悬置的线，模型会把欠账清单当必谈
// 话题、角色沦为任务 NPC**。所以本模块的另一半核心是**克制**（§8.4），落在三个机制上：
//
//   1. grade 克制梯度：dormant（深埋——仅玩家明确提起才相关，**绝不进 B1**）
//      → natural（默认——仅在可提及窗口内出现）→ eager（很想找机会说，framing 变强）。
//      升格只由 deadline 到期或提案驱动（escalate），不会自动升级。
//   2. windows 可提及窗口：黑板键值 / mention 词表 / actors_with 在场者 / 状态路径，
//      **任一命中**即进入窗口，宿主每轮确定性求值。窗口未命中 → 该线绝不进 B1，
//      哪怕 deadline 已到期（到期只提档位与 framing 强度，不豁免窗口）。
//   3. cooldown：被提及后 N 轮内不再进窗口，防止同一条线轮轮霸屏（「防反复横跳」）。
//
// 「六要素完备性不牺牲」靠分工实现：**全量在 C1（低注意力区、仅标题，pending_lines）、
// 相关进 B1（高注意力区、带 framing，select_resurface）**。
//
// 与相邻概念的边界（§8.6）：线是会话内、可了结的**运行时叙事状态**；event 实体是跨会话、
// 已定格的**正史**（重要线收线后可经收件箱晋升）。落盘由事件流负责（m2.md 决断 1）：
// 宿主把 to_value() 的**线全量快照**写进 event::ThreadEvent.thread，投影按同 id 后写覆盖折叠；
// 本模块**纯数据与算法**——不碰文件、不碰网络、不跑 Lua（m2.md 决断 3），只回答三件事：
// **线是什么、现在能不能提、该不该升格**。
//
// 确定性（§7.3「同一事件流重放状态路径一致」）：窗口求值、冷却判定、排序全程无随机、
// 无哈希迭代顺序参与决策；选择排序键固定为
// **档位（deadline 升格 > 黑板/在场 > 状态路径 > 话题擦边）→ importance 降序
// → opened.turn 升序 → id 升序**。
//
// 落点一览（宿主集成按这些签名调用）：
//   开线/推进/收线   Thread::open / touch / resolve / abandon
//   事件快照         Thread::to_value / from_value（往返一致）
//   提及时机         Thread::window_hit / cooldown_ready / due / escalate / mark_mentioned
//   B1「心里有事」    select_resurface + render_concerns（仅窗口内、非 dormant、不在冷却）
//   C1 未决事项       pending_lines（全部活跃线的只读投影）
//   B1「了结未远」    recent_resolutions
//   状态树判据        active / resolved；thread:due 事件 due_threads
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

// ---------- 常量：状态 / 梯度 / 作用域（设计 §8.2 §8.4）----------

/// 生命周期：进行中。
pub const STATE_ACTIVE: &str = "active";
/// 生命周期：已收线（结果落定，resolution 非空）。
pub const STATE_RESOLVED: &str = "resolved";
/// 生命周期：已放弃（「不了了之」也是结果，§8.3；说明挂在经过末节点上）。
pub const STATE_ABANDONED: &str = "abandoned";

/// 克制梯度：深埋——仅玩家明确提起才相关，**不进现状卡**（§8.4）。
pub const GRADE_DORMANT: &str = "dormant";
/// 克制梯度：默认——仅在可提及窗口内出现。
pub const GRADE_NATURAL: &str = "natural";
/// 克制梯度：角色很想找机会说，framing 变强，导演/树可加速铺垫。
pub const GRADE_EAGER: &str = "eager";

/// 作用域：会话级（M2 唯一形态，由事件流投影出 threads.json）。
pub const SCOPE_SESSION: &str = "session";
/// 作用域：世界级（M3 世界线；M2 只做字段预留与透传）。
pub const SCOPE_WORLD: &str = "world";

/// 缺省重要度（管线未给权重时的中位值）。
pub const DEFAULT_IMPORTANCE: f32 = 0.5;
/// 缺省冷却轮数（被提及后 N 轮不再进窗口，§8.2 示例值）。
pub const DEFAULT_COOLDOWN: u32 = 5;
/// deadline 未写 escalate 时的升格目标（§8.3「grade 升至 eager」）。
pub const DEFAULT_ESCALATE: &str = GRADE_EAGER;
/// B1「心里有事」的推荐条数上限（§8.5：按重要度取 top 2–3）。
pub const RESURFACE_TOP: usize = 3;

/// 事件 op：开线（与 event::ThreadEvent.op 对应，宿主引用常量避免拼错）。
pub const OP_OPEN: &str = "open";
/// 事件 op：推进（挂经过节点）。
pub const OP_PROGRESS: &str = "progress";
/// 事件 op：升格（deadline 到期）。
pub const OP_ESCALATE: &str = "escalate";
/// 事件 op：收线。
pub const OP_RESOLVE: &str = "resolve";
/// 事件 op：放弃。
pub const OP_ABANDON: &str = "abandon";

/// 事件来源：玩家手动。
pub const ORIGIN_MANUAL: &str = "manual";
/// 事件来源：状态树任务。
pub const ORIGIN_TREE: &str = "tree";
/// 事件来源：总结管线提案。
pub const ORIGIN_PIPELINE: &str = "pipeline";
/// 事件来源：心理外化（M3.5 · 设计 §9.2：意图说出口，意志外化为剧情线）。
pub const ORIGIN_PSYCHE: &str = "psyche";

// ---------- 常量：命中档位与展示分权重（设计 §8.4 排序口径）----------

/// 档位 0：deadline 到期——她已经在等这件事了，最高优先。
const TIER_DEADLINE: u8 = 0;
/// 档位 1：情境在位——黑板时间/地点命中，或指定的人就在场。
const TIER_SITUATION: u8 = 1;
/// 档位 2：状态路径命中——走到了「图书馆 / 夜谈」这样的阶段。
const TIER_STATE: u8 = 2;
/// 档位 3：话题擦边——窗口词被提到（最低，不抢戏）。
const TIER_MENTION: u8 = 3;

/// 档位分（只进 ResurfacePick.score 供面板展示，不参与排序）。
const TIER_SCORE: [f32; 4] = [0.55, 0.40, 0.25, 0.10];
/// 重要度权重（B1「按重要度 × 新近取 top 2–3」，§8.5）。
const IMPORTANCE_WEIGHT: f32 = 0.40;
/// 久置权重：悬得越久越该被想起（方向与 opened.turn 升序一致）。
const PENDING_WEIGHT: f32 = 0.05;
/// 久置度半程轮数：久置度 = pending / (pending + PENDING_HALF) ∈ [0,1)。
const PENDING_HALF: f32 = 8.0;

// ---------- 数据结构（设计 §8.2）----------

/// 故事时间戳（opened / 收线判定都用它）。
///
/// 设计 §8.2 的示例只写 `{ "turn": 14, "story_clock": "第3天 23:10" }`；story_day 缺失时
/// 从 story_clock 的「第N天」解出（与 palace 的时间戳口径一致）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadStamp {
    /// 产生该戳的轮次。
    pub turn: u64,
    /// 故事天（第 N 天）。
    pub story_day: i64,
    /// 故事时钟，如 `23:10`；也接受已含天数的 `第3天 23:10`。
    pub story_clock: String,
}

/// 经过节点（§8.2 progress）：关键节点的说明，可挂一条宫殿记忆。
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressNode {
    pub turn: u64,
    pub note: String,
    /// 关联的宫殿记忆 id（`mem_0192`），让「这件事的来龙去脉」能在召回里聚合（§8.5）。
    pub memory: Option<String>,
}

/// 期限（§8.2 deadline）：到期未提 → grade 升至 escalate 并抛 thread:due 事件。
#[derive(Debug, Clone, PartialEq)]
pub struct Deadline {
    /// 到期故事天（判定：story_day >= day）。
    pub day: i64,
    /// 升格目标梯度（缺省 eager）。
    pub escalate: String,
}

/// 结果（§8.3 收线时填）：宿主写完「高显著结果记忆」后可回填 memory。
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub turn: u64,
    pub story_day: i64,
    pub story_clock: String,
    /// 结果正文（「玩家如约还书，小雨送了张画着太阳的便签。」）。
    pub outcome: String,
    /// 结果记忆 id（宫殿）。
    pub memory: Option<String>,
}

/// 可提及窗口（§8.2 windows，**任一命中**即进入窗口）。
///
/// 求值全确定性（§8.4）：
/// - Blackboard：键 `day` 按阈值（`story_day >= day`），其余键按等值（字符串 trim +
///   拉丁大小写不敏感；数字按数值；类型不符不命中）。黑板里没有该键 → 不命中。
/// - Mention：窗口词命中「最近窗口里出现的词/实体」——等值，或 mention 抽出的是带上下文
///   的长词时按包含（「话题擦边」）；拉丁大小写不敏感，CJK 精确（不做模糊）。
/// - ActorsWith：名单里的人**全都在场**才算命中（空名单不构成窗口）。
/// - StatePath：活跃路径的**任一段**与给定串相等。
/// - All：合取（设计 §8.2 第三个示例 `{"blackboard":{...},"actors_with":[...]}` 的形态）；
///   from_value 遇到多键对象会自动包成它，to_value 合并回一个对象。
#[derive(Debug, Clone, PartialEq)]
pub enum ResurfaceWindow {
    /// 黑板键值窗口：`{"blackboard": {"day": 5}}`。
    Blackboard(BTreeMap<String, Value>),
    /// 话题窗口：`{"mention": ["还书", "借书卡"]}`。
    Mention(Vec<String>),
    /// 在场窗口：`{"actors_with": ["小雨"]}`。
    ActorsWith(Vec<String>),
    /// 状态路径窗口：`{"state_path": ["图书馆"]}`。
    StatePath(Vec<String>),
    /// 合取窗口（设计 §8.2 的组合写法；单条件会被归一掉，不出现在结果里）。
    All(Vec<ResurfaceWindow>),
}

/// 提及时机（§8.2 resurface）：开线时由 LLM 起草（写入时的智能），运行时由宿主确定性
/// 执行（运行时的克制）。
#[derive(Debug, Clone, PartialEq)]
pub struct Resurface {
    /// 克制梯度：dormant | natural | eager（见 GRADE_*）。
    pub grade: String,
    /// 可提及窗口；**空 = 永不进 B1**（只有玩家主动提起才由 C1 兜底）。
    pub windows: Vec<ResurfaceWindow>,
    /// 期限（可选）。
    pub deadline: Option<Deadline>,
    /// 被提及后 N 轮不再进窗口。
    pub cooldown: u32,
    /// 提起时的表演指引（「她在意但不好意思催」）。
    pub framing: String,
    /// 上次被提及的轮次（进冷却的锚点）。
    pub last_mentioned_turn: Option<u64>,
}

/// 一条剧情线（§8.2；会话级运行时叙事状态，落盘经事件流）。
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    /// 线 id，如 `thread.周五还书`（宿主用 id_from_title 兜一个默认写法）。
    pub id: String,
    pub title: String,
    /// 起因（为什么会有这条线）。
    pub cause: String,
    /// 涉及的人。
    pub actors: Vec<String>,
    /// 重要度 0–1（收敛后存放；B1 排序的第一权重）。
    pub importance: f32,
    /// 开线戳。
    pub opened: ThreadStamp,
    /// 生命周期：active | resolved | abandoned（见 STATE_*）。
    pub state: String,
    /// 经过：关键节点，按发生顺序追加。
    pub progress: Vec<ProgressNode>,
    /// 提及时机。
    pub resurface: Resurface,
    /// 结果（收线时填）。
    pub resolution: Option<Resolution>,
    /// 作用域：session | world（M3 世界级线）。
    pub scope: String,
    /// 心理运行时外化的意图 id（M2.5；`state.psyche` 的 intent 与线互为表里，§9.1）。
    pub linked_intent: Option<String>,
}

/// 一次提及窗口求值的输入（宿主每轮交给剧情线的一切）。
///
/// 全部字段都是只读快照：黑板（运行时状态属主，§2.1）、最近窗口里出现的词/实体、
/// 在场者、状态树活跃路径（根→叶）、当前轮次与故事时间。
pub struct ThreadQuery<'a> {
    pub turn: u64,
    pub story_day: i64,
    pub story_clock: &'a str,
    pub blackboard: &'a BTreeMap<String, serde_json::Value>,
    /// 最近窗口里出现的词/实体。
    pub mentions: &'a [String],
    /// 在场者。
    pub present: &'a [String],
    /// 状态树活跃路径（根→叶）。
    pub state_path: &'a [String],
}

impl<'a> ThreadQuery<'a> {
    /// 常用构造：只给时间与黑板，三个词表/路径留空（宿主按需再填字段）。
    pub fn new(
        turn: u64,
        story_day: i64,
        story_clock: &'a str,
        blackboard: &'a BTreeMap<String, Value>,
    ) -> ThreadQuery<'a> {
        ThreadQuery {
            turn,
            story_day,
            story_clock,
            blackboard,
            mentions: &[],
            present: &[],
            state_path: &[],
        }
    }
}

/// 一条「此刻可以提起」的线（B1「心里有事」的条目，§8.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResurfacePick {
    pub id: String,
    pub title: String,
    /// 提起时的表演指引（原样给出，空串表示没有特别指引）。
    pub framing: String,
    /// 归一化后的克制梯度（dormant 永远不会出现在结果里）。
    pub grade: String,
    /// 展示分 0–1 = `档位分 + 0.40 × importance + 0.05 × 久置度`。
    ///
    /// **不参与排序**：排序权威是「档位 → importance 降序 → opened.turn 升序 → id 升序」
    /// （久置度加成与 opened.turn 升序同向；宿主不要按 score 重排）。
    pub score: f32,
    /// 激活原因（界面直接显示，如 `黑板:day≥5+在场:小雨`）——DoD 第 7 项。
    pub reason: String,
}

// ---------- 线：生命周期（设计 §8.3）----------

impl Thread {
    /// 开线（起因落锚 + 时机起草）。
    ///
    /// 默认策略按 §8.3：**手动/树开线默认 natural + 仅 mention 窗口**（窗口词取标题——
    /// 谁提到这条线就让它浮上来），deadline 留空由管线/面板补。管线提案开线时用
    /// from_value 读提案里 LLM 起草的完整 resurface。
    ///
    /// id 为空时用 id_from_title 兜一个（确定性）；importance 收敛到 [0,1]（NaN → 0.5）。
    pub fn open(
        id: &str,
        title: &str,
        cause: &str,
        actors: &[String],
        importance: f32,
        stamp: ThreadStamp,
    ) -> Thread {
        let title = title.trim().to_string();
        let id = match id.trim() {
            "" => id_from_title(&title),
            given => given.to_string(),
        };
        Thread {
            id,
            resurface: Resurface::mentioning(&title),
            title,
            cause: cause.trim().to_string(),
            actors: actors
                .iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect(),
            importance: sanitize_importance(importance),
            opened: stamp,
            state: STATE_ACTIVE.to_string(),
            progress: Vec::new(),
            resolution: None,
            scope: SCOPE_SESSION.to_string(),
            linked_intent: None,
        }
    }

    /// 推进：追加一个经过节点（管线把新情景记忆挂上来，§8.3）。
    ///
    /// 说明与记忆都为空时不落节点（避免空推进把「经过」冲淡）；不改 state——
    /// 收线后再推进通常是宿主该新开一条线。
    pub fn touch(&mut self, turn: u64, note: &str, memory: Option<String>) {
        let note = note.trim();
        let memory = memory
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        if note.is_empty() && memory.is_none() {
            return;
        }
        self.progress.push(ProgressNode {
            turn,
            note: note.to_string(),
            memory,
        });
    }

    /// 收线（结果落定）：state → resolved，写入 resolution 并**返回一份给宿主**——
    /// 宿主据此做收线三件事（写高显著结果记忆入宫殿、发 `thread:<id>:resolved` 事件、
    /// 移出现状卡；§8.3）。结果记忆 id 由宿主写完后用 attach_resolution_memory 回填。
    ///
    /// 重复收线 = 结果覆盖（后写为准）；放弃过的线也可收线。
    pub fn resolve(
        &mut self,
        turn: u64,
        story_day: i64,
        story_clock: &str,
        outcome: &str,
    ) -> Resolution {
        let resolution = Resolution {
            turn,
            story_day,
            story_clock: story_clock.trim().to_string(),
            outcome: outcome.trim().to_string(),
            memory: None,
        };
        self.state = STATE_RESOLVED.to_string();
        self.resolution = Some(resolution.clone());
        resolution
    }

    /// 放弃（§8.3：「不了了之」也是结果）。
    ///
    /// state → abandoned，**不写 resolution**（放弃不是收线：没有高显著结果记忆、
    /// 不发 resolved 事件，宿主按 abandoned 事件处理）。说明落在经过末节点上——
    /// 数据模型里没有独立的放弃说明字段，经过节点是最贴近的落点；轮次取最近已知轮次
    /// （末节点轮次，没节点则开线轮次），说明为空时记为「不了了之」。
    pub fn abandon(&mut self, note: &str) {
        self.state = STATE_ABANDONED.to_string();
        let turn = self
            .progress
            .last()
            .map(|p| p.turn)
            .unwrap_or(self.opened.turn);
        let note = match note.trim() {
            "" => "不了了之",
            given => given,
        };
        self.progress.push(ProgressNode {
            turn,
            note: note.to_string(),
            memory: None,
        });
    }

    /// 是否活跃（state 归一后比较；未知 state 在读入时已归一为 active）。
    pub fn is_active(&self) -> bool {
        same_state(&self.state, STATE_ACTIVE)
    }

    /// 窗口求值：`Some(命中说明)` 即进入「可提及窗口」（界面显示为激活原因）。
    ///
    /// 只回答「窗口命中没有」，**不判断** grade 与 cooldown——那两层由 select_resurface
    /// 把关（克制梯度与冷却抑制是选择期的规则）。
    pub fn window_hit(&self, q: &ThreadQuery<'_>) -> Option<String> {
        self.resurface.window_hit(q)
    }

    /// 冷却是否就绪：无提及记录，或 `turn - last_mentioned_turn >= cooldown`。
    /// 轮次倒退（重放/乱序事件）按「刚提过」处理，不放行。
    pub fn cooldown_ready(&self, turn: u64) -> bool {
        match self.resurface.last_mentioned_turn {
            None => true,
            Some(last) => turn.saturating_sub(last) >= self.resurface.cooldown as u64,
        }
    }

    /// deadline 是否到期：活跃线且 `story_day >= deadline.day`。
    /// 到期是状态判据（到期后一直为真），事件只抛一次由 due_threads 保证。
    pub fn due(&self, story_day: i64) -> bool {
        self.is_active()
            && self
                .resurface
                .deadline
                .as_ref()
                .is_some_and(|d| story_day >= d.day)
    }

    /// 到期升格：把 grade 抬到 deadline.escalate（缺省 eager），返回是否变化（幂等）。
    ///
    /// 调用方在 due() 命中时调用（本函数不重复判到期——签名不带故事天）。升格**只提
    /// 档位**：framing 的收紧由管线提案（§8.3），窗口不会因此豁免。
    pub fn escalate(&mut self) -> bool {
        let Some(target) = self.escalation_target() else {
            return false;
        };
        if grade_rank(&self.resurface.grade) >= grade_rank(&target) {
            return false;
        }
        self.resurface.grade = target;
        true
    }

    /// 记「被提及」：进冷却，N 轮内不再进窗口（§8.3「防止同一条线轮轮霸屏」）。
    ///
    /// 只动冷却锚点；把「被提及」记成经过节点由管线用 touch 完成（宿主可两者都调）。
    /// 轮次只前进不回退，重放不会把冷却窗口拉回去。
    pub fn mark_mentioned(&mut self, turn: u64) {
        self.resurface.last_mentioned_turn = Some(match self.resurface.last_mentioned_turn {
            Some(prev) if prev > turn => prev,
            _ => turn,
        });
    }

    /// 回填结果记忆 id（宿主写完「高显著结果记忆」后调用）；没有 resolution 时返回 false。
    pub fn attach_resolution_memory(&mut self, memory_id: &str) -> bool {
        let id = memory_id.trim();
        match self.resolution.as_mut() {
            Some(r) if !id.is_empty() => {
                r.memory = Some(id.to_string());
                true
            }
            _ => false,
        }
    }

    /// deadline 的升格目标（无 deadline → None；escalate 写坏时退回 eager）。
    fn escalation_target(&self) -> Option<String> {
        self.resurface
            .deadline
            .as_ref()
            .map(|d| normalize_escalate(&d.escalate))
    }

    /// 到期且尚未达到升格目标——due_threads 用它保证 `thread:due` 只抛一次。
    fn escalation_pending(&self) -> bool {
        self.escalation_target()
            .is_some_and(|t| grade_rank(&self.resurface.grade) < grade_rank(&t))
    }

    /// 事件流快照形态（写进 event::ThreadEvent.thread；投影按同 id 后写覆盖折叠）。
    pub fn to_value(&self) -> Value {
        obj(vec![
            ("id", Value::String(self.id.clone())),
            ("title", Value::String(self.title.clone())),
            ("cause", Value::String(self.cause.clone())),
            ("actors", text_list(&self.actors)),
            ("importance", f32_value(self.importance)),
            ("opened", self.opened.to_value()),
            ("state", Value::String(self.state.clone())),
            (
                "progress",
                Value::Array(self.progress.iter().map(ProgressNode::to_value).collect()),
            ),
            ("resurface", self.resurface.to_value()),
            (
                "resolution",
                match &self.resolution {
                    Some(r) => r.to_value(),
                    None => Value::Null,
                },
            ),
            ("scope", Value::String(self.scope.clone())),
            (
                "linked_intent",
                match &self.linked_intent {
                    Some(i) => Value::String(i.clone()),
                    None => Value::Null,
                },
            ),
        ])
    }

    /// 读侧：宽松解析（缺字段给默认，单字段写坏不让整条线报废），缺 id/title 报错。
    pub fn from_value(v: &Value) -> Result<Thread, String> {
        Self::parse(v)
    }

    fn parse(v: &Value) -> Result<Thread, String> {
        let Some(map) = v.as_object() else {
            return Err("剧情线快照不是对象".to_string());
        };
        let id = required_text(map, "id")?;
        let title = required_text(map, "title")?;
        let state = get_text(map, "state")
            .map(|s| normalize_state(&s))
            .unwrap_or_else(|| STATE_ACTIVE.to_string());
        let scope = get_text(map, "scope")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| SCOPE_SESSION.to_string());
        let progress = match map.get("progress") {
            Some(Value::Array(items)) => {
                items.iter().filter_map(ProgressNode::from_value).collect()
            }
            _ => Vec::new(),
        };
        Ok(Thread {
            id,
            title,
            cause: get_text(map, "cause").unwrap_or_default(),
            actors: get_str_list(map, "actors"),
            importance: sanitize_importance(
                get_f32(map, "importance").unwrap_or(DEFAULT_IMPORTANCE),
            ),
            opened: map
                .get("opened")
                .map(ThreadStamp::from_value)
                .unwrap_or_default(),
            state,
            progress,
            resurface: map
                .get("resurface")
                .map(Resurface::from_value)
                .unwrap_or_default(),
            resolution: map.get("resolution").and_then(Resolution::from_value),
            scope,
            linked_intent: get_text(map, "linked_intent")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        })
    }
}

// ---------- 提及时机：窗口求值与一次选择（设计 §8.4）----------

impl Default for Resurface {
    /// 缺省 = 保守：natural + **无窗口**（永不进 B1，只有玩家主动提起才由 C1 兜底）。
    fn default() -> Resurface {
        Resurface {
            grade: GRADE_NATURAL.to_string(),
            windows: Vec::new(),
            deadline: None,
            cooldown: DEFAULT_COOLDOWN,
            framing: String::new(),
            last_mentioned_turn: None,
        }
    }
}

impl Resurface {
    /// 手动/树开线的默认时机（§8.3）：natural + 仅 mention 窗口（窗口词 = 标题）。
    pub fn mentioning(title: &str) -> Resurface {
        let title = title.trim();
        let mut r = Resurface::default();
        if !title.is_empty() {
            r.windows = vec![ResurfaceWindow::Mention(vec![title.to_string()])];
        }
        r
    }

    /// 窗口求值：命中则给出 `(档位, 激活原因)`；窗口按声明顺序求值，首个命中即返回。
    fn hit(&self, q: &ThreadQuery<'_>) -> Option<(u8, String)> {
        self.windows.iter().find_map(|w| hit_of(w, q))
    }

    /// 窗口是否命中（命中则给出激活原因，界面直接显示）。
    pub fn window_hit(&self, q: &ThreadQuery<'_>) -> Option<String> {
        self.hit(q).map(|(_, reason)| reason)
    }

    pub fn to_value(&self) -> Value {
        obj(vec![
            ("grade", Value::String(normalize_grade(&self.grade))),
            (
                "windows",
                Value::Array(self.windows.iter().map(ResurfaceWindow::to_value).collect()),
            ),
            (
                "deadline",
                match &self.deadline {
                    Some(d) => d.to_value(),
                    None => Value::Null,
                },
            ),
            ("cooldown", Value::from(self.cooldown)),
            ("framing", Value::String(self.framing.clone())),
            (
                "last_mentioned_turn",
                match self.last_mentioned_turn {
                    Some(t) => Value::from(t),
                    None => Value::Null,
                },
            ),
        ])
    }

    /// 读侧：缺字段给默认（grade natural / cooldown 5），坏窗口丢弃（宁可少提不可乱提）。
    pub fn from_value(v: &Value) -> Resurface {
        Self::parse(v).unwrap_or_default()
    }

    fn parse(v: &Value) -> Result<Resurface, String> {
        let Some(map) = v.as_object() else {
            return Ok(Resurface::default());
        };
        let windows = match map.get("windows") {
            // 单对象也接受（"windows": {"mention": "还书"}）
            Some(Value::Object(_)) => map
                .get("windows")
                .and_then(ResurfaceWindow::from_value)
                .into_iter()
                .collect(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(ResurfaceWindow::from_value)
                .collect(),
            _ => Vec::new(),
        };
        Ok(Resurface {
            grade: get_text(map, "grade")
                .map(|g| normalize_grade(&g))
                .unwrap_or_else(|| GRADE_NATURAL.to_string()),
            windows,
            deadline: map.get("deadline").and_then(Deadline::from_value),
            cooldown: get_u64(map, "cooldown")
                .map(|c| c.min(u32::MAX as u64) as u32)
                .unwrap_or(DEFAULT_COOLDOWN),
            framing: get_text(map, "framing").unwrap_or_default(),
            last_mentioned_turn: get_u64(map, "last_mentioned_turn"),
        })
    }
}

impl ResurfaceWindow {
    /// 窗口求值：`Some((档位, 激活原因))`。
    pub fn hit(&self, q: &ThreadQuery<'_>) -> Option<(u8, String)> {
        hit_of(self, q)
    }

    /// 设计 §8.2 的 JSON 形态；All 合并回一个多键对象。
    pub fn to_value(&self) -> Value {
        match self {
            ResurfaceWindow::Blackboard(pairs) => {
                let mut inner = Map::new();
                for (k, v) in pairs {
                    inner.insert(k.clone(), v.clone());
                }
                obj(vec![("blackboard", Value::Object(inner))])
            }
            ResurfaceWindow::Mention(words) => obj(vec![("mention", text_list(words))]),
            ResurfaceWindow::ActorsWith(names) => obj(vec![("actors_with", text_list(names))]),
            ResurfaceWindow::StatePath(segs) => obj(vec![("state_path", text_list(segs))]),
            ResurfaceWindow::All(parts) => {
                let mut merged = Map::new();
                for p in parts {
                    if let Value::Object(child) = p.to_value() {
                        for (k, v) in child {
                            merged.insert(k, v);
                        }
                    }
                }
                Value::Object(merged)
            }
        }
    }

    /// 读侧：无法识别的窗口 → None（调用方丢弃它，保守）。
    pub fn from_value(v: &Value) -> Option<ResurfaceWindow> {
        Self::parse(v).ok()
    }

    fn parse(v: &Value) -> Result<ResurfaceWindow, String> {
        let Some(map) = v.as_object() else {
            return Err("窗口不是对象".to_string());
        };
        // 固定键序（与 to_value 的写法、激活原因的拼接顺序一致）：
        // blackboard → mention → actors_with → state_path。不依赖 serde_json 的 Map 迭代顺序。
        let mut parts: Vec<ResurfaceWindow> = Vec::new();
        if let Some(bb) = first_key(map, &["blackboard", "bb"]) {
            if let Some(inner) = bb.as_object() {
                let mut pairs = BTreeMap::new();
                for (k, val) in inner {
                    let key = k.trim();
                    if !key.is_empty() {
                        pairs.insert(key.to_string(), val.clone());
                    }
                }
                if !pairs.is_empty() {
                    parts.push(ResurfaceWindow::Blackboard(pairs));
                }
            }
        }
        for key in ["mention", "mentions", "words"] {
            if let Some(words) = words_of(map.get(key)) {
                parts.push(ResurfaceWindow::Mention(words));
                break;
            }
        }
        for key in ["actors_with", "actors"] {
            if let Some(names) = words_of(map.get(key)) {
                parts.push(ResurfaceWindow::ActorsWith(names));
                break;
            }
        }
        for key in ["state_path", "path"] {
            if let Some(segs) = words_of(map.get(key)) {
                parts.push(ResurfaceWindow::StatePath(segs));
                break;
            }
        }
        match parts.len() {
            0 => Err("窗口里没有可识别的条件".to_string()),
            1 => Ok(parts.pop().unwrap()),
            _ => Ok(ResurfaceWindow::All(parts)),
        }
    }
}

impl ThreadStamp {
    pub fn to_value(&self) -> Value {
        obj(vec![
            ("turn", Value::from(self.turn)),
            ("story_day", Value::from(self.story_day)),
            ("story_clock", Value::String(self.story_clock.clone())),
        ])
    }

    /// 读侧：非对象 → 默认戳（全 0）；缺 story_day 时从 story_clock 的「第N天」解出。
    pub fn from_value(v: &Value) -> ThreadStamp {
        Self::parse(v).unwrap_or_default()
    }

    fn parse(v: &Value) -> Result<ThreadStamp, String> {
        let Some(map) = v.as_object() else {
            return Ok(ThreadStamp::default());
        };
        let story_clock = get_text(map, "story_clock").unwrap_or_default();
        Ok(ThreadStamp {
            turn: get_u64(map, "turn").unwrap_or(0),
            story_day: get_i64(map, "story_day")
                .or_else(|| parse_day_from_clock(&story_clock))
                .unwrap_or(0),
            story_clock,
        })
    }
}

impl ProgressNode {
    pub fn to_value(&self) -> Value {
        obj(vec![
            ("turn", Value::from(self.turn)),
            ("note", Value::String(self.note.clone())),
            (
                "memory",
                match &self.memory {
                    Some(m) => Value::String(m.clone()),
                    None => Value::Null,
                },
            ),
        ])
    }

    pub fn from_value(v: &Value) -> Option<ProgressNode> {
        Self::parse(v).ok()
    }

    fn parse(v: &Value) -> Result<ProgressNode, String> {
        match v {
            Value::String(note) => Ok(ProgressNode {
                turn: 0,
                note: note.trim().to_string(),
                memory: None,
            }),
            Value::Object(map) => Ok(ProgressNode {
                turn: get_u64(map, "turn").unwrap_or(0),
                note: get_text(map, "note").unwrap_or_default(),
                memory: get_text(map, "memory")
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty()),
            }),
            _ => Err("经过节点既不是字符串也不是对象".to_string()),
        }
    }
}

impl Deadline {
    pub fn to_value(&self) -> Value {
        obj(vec![
            ("day", Value::from(self.day)),
            ("escalate", Value::String(normalize_escalate(&self.escalate))),
        ])
    }

    pub fn from_value(v: &Value) -> Option<Deadline> {
        Self::parse(v).ok()
    }

    fn parse(v: &Value) -> Result<Deadline, String> {
        let Some(map) = v.as_object() else {
            return Err("deadline 不是对象".to_string());
        };
        let day = get_i64(map, "day")
            .or_else(|| get_i64(map, "story_day"))
            .ok_or_else(|| "deadline 缺 day".to_string())?;
        Ok(Deadline {
            day,
            escalate: get_text(map, "escalate")
                .map(|s| normalize_escalate(&s))
                .unwrap_or_else(|| DEFAULT_ESCALATE.to_string()),
        })
    }
}

impl Resolution {
    pub fn to_value(&self) -> Value {
        obj(vec![
            ("turn", Value::from(self.turn)),
            ("story_day", Value::from(self.story_day)),
            ("story_clock", Value::String(self.story_clock.clone())),
            ("outcome", Value::String(self.outcome.clone())),
            (
                "memory",
                match &self.memory {
                    Some(m) => Value::String(m.clone()),
                    None => Value::Null,
                },
            ),
        ])
    }

    pub fn from_value(v: &Value) -> Option<Resolution> {
        Self::parse(v).ok()
    }

    fn parse(v: &Value) -> Result<Resolution, String> {
        let Some(map) = v.as_object() else {
            return Err("resolution 不是对象".to_string());
        };
        let story_clock = get_text(map, "story_clock").unwrap_or_default();
        Ok(Resolution {
            turn: get_u64(map, "turn").unwrap_or(0),
            story_day: get_i64(map, "story_day")
                .or_else(|| parse_day_from_clock(&story_clock))
                .unwrap_or(0),
            story_clock,
            outcome: get_text(map, "outcome").unwrap_or_default(),
            memory: get_text(map, "memory")
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
        })
    }
}

// ---------- 窗口求值（全确定性，§8.4）----------

/// 窗口求值：命中则返回 `(档位, 激活原因)`。All 是合取，档位取各部分里最高的一档。
fn hit_of(w: &ResurfaceWindow, q: &ThreadQuery<'_>) -> Option<(u8, String)> {
    match w {
        ResurfaceWindow::Blackboard(pairs) => {
            blackboard_hit(pairs, q).map(|r| (TIER_SITUATION, r))
        }
        ResurfaceWindow::Mention(words) => mention_hit(words, q).map(|r| (TIER_MENTION, r)),
        ResurfaceWindow::ActorsWith(names) => actors_hit(names, q).map(|r| (TIER_SITUATION, r)),
        ResurfaceWindow::StatePath(segs) => state_hit(segs, q).map(|r| (TIER_STATE, r)),
        ResurfaceWindow::All(parts) => {
            if parts.is_empty() {
                return None;
            }
            let mut tier = u8::MAX;
            let mut reasons: Vec<String> = Vec::new();
            for p in parts {
                let (t, r) = hit_of(p, q)?;
                tier = tier.min(t);
                reasons.push(r);
            }
            Some((tier, reasons.join("+")))
        }
    }
}

/// 黑板窗口：day 按阈值（story_day >= day），其余键按等值；任一条件不满足即不命中。
fn blackboard_hit(pairs: &BTreeMap<String, Value>, q: &ThreadQuery<'_>) -> Option<String> {
    if pairs.is_empty() {
        return None;
    }
    let mut reasons: Vec<String> = Vec::new();
    for (key, expected) in pairs {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        if key.eq_ignore_ascii_case("day") {
            // 非数字的 day 窗口视为写坏 → 不命中（保守：宁可少提不可乱提）。
            let threshold = as_i64(expected)?;
            if blackboard_day(q) < threshold {
                return None;
            }
            reasons.push(format!("黑板:day≥{threshold}"));
            continue;
        }
        let actual = blackboard_get(q.blackboard, key)?;
        if !value_eq(actual, expected) {
            return None;
        }
        reasons.push(format!("黑板:{key}={}", value_text(expected)));
    }
    Some(reasons.join("+"))
}

/// 黑板时钟：day 键优先（黑板是运行时状态属主，§2.1），缺了才用 query.story_day。
fn blackboard_day(q: &ThreadQuery<'_>) -> i64 {
    blackboard_get(q.blackboard, "day")
        .and_then(as_i64)
        .unwrap_or(q.story_day)
}

/// 黑板取值：先精确键，再 trim + 拉丁大小写不敏感（BTreeMap 顺序，确定性）。
fn blackboard_get<'a>(bb: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a Value> {
    if let Some(v) = bb.get(key) {
        return Some(v);
    }
    let want = normalize_word(key);
    bb.iter()
        .find(|(k, _)| normalize_word(k) == want)
        .map(|(_, v)| v)
}

/// 黑板等值：字符串 trim + 拉丁大小写不敏感，数字按数值，布尔按相等，Null 只配 Null；
/// 类型不符不命中（不做跨类型猜测）。
fn value_eq(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::String(a), Value::String(b)) => normalize_word(a) == normalize_word(b),
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(x, y)| x == y),
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// 话题窗口：窗口词命中「最近窗口里出现的词/实体」——等值，或 mention 是带上下文的长词时
/// 按包含（「话题擦边」）；拉丁大小写不敏感、CJK 精确。命中即返回词表里第一个命中的词。
fn mention_hit(words: &[String], q: &ThreadQuery<'_>) -> Option<String> {
    for w in words {
        let needle = normalize_word(w);
        if needle.is_empty() {
            continue;
        }
        for m in q.mentions {
            let hay = normalize_word(m);
            if hay.is_empty() {
                continue;
            }
            if hay == needle || hay.contains(&needle) {
                return Some(format!("提及:{}", w.trim()));
            }
        }
    }
    None
}

/// 在场窗口：名单里的人全都在场才算命中（空名单不构成窗口）。
fn actors_hit(names: &[String], q: &ThreadQuery<'_>) -> Option<String> {
    let mut need: Vec<(String, String)> = Vec::new(); // (归一化, 展示原文)
    for n in names {
        let norm = normalize_word(n);
        if norm.is_empty() || need.iter().any(|(k, _)| *k == norm) {
            continue;
        }
        need.push((norm, n.trim().to_string()));
    }
    if need.is_empty() {
        return None;
    }
    need.sort_by(|a, b| a.0.cmp(&b.0));
    let present: Vec<String> = q
        .present
        .iter()
        .map(|p| normalize_word(p))
        .filter(|p| !p.is_empty())
        .collect();
    if !need.iter().all(|(norm, _)| present.iter().any(|p| p == norm)) {
        return None;
    }
    let display: Vec<String> = need.into_iter().map(|(_, d)| d).collect();
    Some(format!("在场:{}", display.join(",")))
}

/// 状态路径窗口：活跃路径（根→叶）的任一段与给定串相等即命中（整段相等，不做包含）。
fn state_hit(segs: &[String], q: &ThreadQuery<'_>) -> Option<String> {
    for s in segs {
        let want = normalize_word(s);
        if want.is_empty() {
            continue;
        }
        if q.state_path.iter().any(|p| normalize_word(p) == want) {
            return Some(format!("状态:{}", s.trim()));
        }
    }
    None
}

// ---------- B1「心里有事」（设计 §4.1 §8.4 §8.5）----------

/// 选出此刻可以提起的线（B1「心里有事」，§8.5：按重要度取 top 2–3，各附一句 framing）。
///
/// 三道闸门，缺一不可（克制是核心，§8.4）：
/// 1. **窗口命中**——未命中绝不进 B1（deadline 到期也不豁免，到期只提档位）；
/// 2. **非 dormant**——dormant 只由 C1 兜底，玩家主动提起时才相关；
/// 3. **冷却就绪**——被提及后 N 轮内不再进窗口。
///
/// 排序：档位（deadline 升格 > 黑板/在场 > 状态路径 > 话题擦边）→ importance 降序
/// → opened.turn 升序 → id 升序。`top = 0` 视为不限量（宿主传 RESURFACE_TOP 取 2–3 条）。
pub fn select_resurface(
    threads: &[Thread],
    q: &ThreadQuery<'_>,
    top: usize,
) -> Vec<ResurfacePick> {
    let mut rows: Vec<(u8, &Thread, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for t in threads {
        if !t.is_active() {
            continue;
        }
        // 同一条线（非空 id）只出现一次。
        let id = normalize_word(&t.id);
        if !id.is_empty() {
            if seen.iter().any(|s| *s == id) {
                continue;
            }
            seen.push(id);
        }
        if grade_rank(&t.resurface.grade) == grade_rank(GRADE_DORMANT) {
            continue; // dormant 即使窗口命中也不进 B1（§8.4）
        }
        if !t.cooldown_ready(q.turn) {
            continue; // 冷却抑制高频复现
        }
        let Some((tier, reason)) = t.resurface.hit(q) else {
            continue; // 窗口未命中 → 绝不进 B1
        };
        let tier = if t.due(q.story_day) {
            TIER_DEADLINE // 到期升格：最高档（仍以窗口命中为前提）
        } else {
            tier
        };
        rows.push((tier, t, reason));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| rank_cmp(a.1, b.1)));
    if top > 0 {
        rows.truncate(top);
    }
    rows.into_iter()
        .map(|(tier, t, reason)| ResurfacePick {
            id: t.id.clone(),
            title: t.title.clone(),
            framing: t.resurface.framing.trim().to_string(),
            grade: normalize_grade(&t.resurface.grade),
            score: resurface_score(tier, t, q),
            reason,
        })
        .collect()
}

/// 展示分（只给面板看，不参与排序；见 ResurfacePick.score）。
fn resurface_score(tier: u8, t: &Thread, q: &ThreadQuery<'_>) -> f32 {
    let pending = q.turn.saturating_sub(t.opened.turn) as f32;
    let stale = pending / (pending + PENDING_HALF);
    TIER_SCORE[(tier as usize).min(TIER_SCORE.len() - 1)]
        + IMPORTANCE_WEIGHT * sanitize_importance(t.importance)
        + PENDING_WEIGHT * stale
}

/// 渲染 B1「心里有事」条目（§4.1 的 `①标题——framing` 形态；无 framing 时只给标题）。
pub fn render_concerns(picks: &[ResurfacePick]) -> Vec<String> {
    picks
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mark = circled(i + 1);
            let framing = p.framing.trim();
            if framing.is_empty() {
                format!("{mark}{}", p.title.trim())
            } else {
                format!("{mark}{}——{framing}", p.title.trim())
            }
        })
        .collect()
}

// ---------- C1 未决事项 / 了结未远 / 判据（设计 §2.1 规则 4 §4.1 §8.5）----------

/// C1 未决事项清单：**全部活跃线**的只读投影（§2.1 规则 4：玩家与树操作的是线，不是清单）。
///
/// 仅标题与状态，不带 framing/progress——低注意力区负责「查得到全部欠账」，
/// 高注意力区（B1）负责「此刻该提的」。dormant 的线在这里也能被查到。
pub fn pending_lines(threads: &[Thread]) -> Vec<String> {
    let mut rows: Vec<&Thread> = threads.iter().filter(|t| t.is_active()).collect();
    rows.sort_by(|a, b| rank_cmp(a, b));
    rows.into_iter()
        .map(|t| format!("{}（{}）", t.title.trim(), state_label(&t.state)))
        .collect()
}

/// B1「了结未远」：近期收线的重要结果（§8.5）。
///
/// within_turns = 最近多少轮内（0 = 不限窗口）；只看 state = resolved 的线
/// （放弃不算收线），未来轮次的结果不注入（重放/编辑历史后不至于穿越）。
/// 返回形如 `工作牌之事已坦白（第6天 18:02）`，最近的在前。
pub fn recent_resolutions(threads: &[Thread], within_turns: u64, q_turn: u64) -> Vec<String> {
    let mut rows: Vec<(&Thread, &Resolution)> = Vec::new();
    for t in threads {
        if !same_state(&t.state, STATE_RESOLVED) {
            continue;
        }
        let Some(r) = t.resolution.as_ref() else {
            continue;
        };
        if r.turn > q_turn {
            continue;
        }
        if within_turns > 0 && q_turn.saturating_sub(r.turn) > within_turns {
            continue;
        }
        rows.push((t, r));
    }
    rows.sort_by(|a, b| b.1.turn.cmp(&a.1.turn).then_with(|| a.0.id.cmp(&b.0.id)));
    rows.into_iter()
        .filter_map(|(t, r)| {
            let text = match r.outcome.trim() {
                "" => t.title.trim(),
                given => given,
            };
            if text.is_empty() {
                return None;
            }
            Some(format!("{text}（{}）", stamp_text(r.story_day, &r.story_clock)))
        })
        .collect()
}

/// 状态树判据 `threads.active(id)`：该线活跃（id trim + 拉丁大小写不敏感；空 id 不命中）。
pub fn active(threads: &[Thread], id: &str) -> bool {
    find_thread(threads, id).is_some_and(Thread::is_active)
}

/// 状态树判据 `threads.resolved(id)`：该线已收线（放弃既不 active 也不 resolved）。
pub fn resolved(threads: &[Thread], id: &str) -> bool {
    find_thread(threads, id).is_some_and(|t| same_state(&t.state, STATE_RESOLVED))
}

/// 到期该抛 `thread:due` 的线 id：活跃 + deadline 到期 + **尚未升格到目标**。
///
/// 第三项保证事件只抛一次（升格后不再列出），宿主可以放心地「对本列表逐条抛事件 +
/// escalate()」；想查「所有过了期限的线」用 Thread::due。
pub fn due_threads(threads: &[Thread], story_day: i64) -> Vec<String> {
    let mut rows: Vec<&Thread> = threads
        .iter()
        .filter(|t| t.due(story_day) && t.escalation_pending())
        .collect();
    rows.sort_by(|a, b| rank_cmp(a, b));
    rows.into_iter().map(|t| t.id.clone()).collect()
}

fn find_thread<'a>(threads: &'a [Thread], id: &str) -> Option<&'a Thread> {
    let want = normalize_word(id);
    if want.is_empty() {
        return None;
    }
    threads.iter().find(|t| normalize_word(&t.id) == want)
}

/// 线 id 的兜底写法（`周五还书的约定` → `thread.周五还书的约定`）：确定性、不做 slug 化
/// （CJK 保持可读，面板/日志里一眼认得出）；空白压成单空格，空标题给 `thread.未命名`。
pub fn id_from_title(title: &str) -> String {
    let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        "thread.未命名".to_string()
    } else {
        format!("thread.{t}")
    }
}

// ---------- 小工具 ----------

/// 线的静态排序键：importance 降序 → opened.turn 升序 → id 升序。
/// select_resurface / pending_lines / due_threads 共用，全模块一个次序口径。
fn rank_cmp(a: &Thread, b: &Thread) -> std::cmp::Ordering {
    sanitize_importance(b.importance)
        .total_cmp(&sanitize_importance(a.importance))
        .then_with(|| a.opened.turn.cmp(&b.opened.turn))
        .then_with(|| a.id.cmp(&b.id))
}

/// 词/名/段/键的比较口径：trim + 拉丁大小写不敏感（CJK 因此是精确比较）。
fn normalize_word(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// state 归一：未知写法一律当 active（行为可预期，不让脏数据把线变成幽灵）。
fn normalize_state(s: &str) -> String {
    match normalize_word(s).as_str() {
        STATE_RESOLVED => STATE_RESOLVED.to_string(),
        STATE_ABANDONED => STATE_ABANDONED.to_string(),
        _ => STATE_ACTIVE.to_string(),
    }
}

/// grade 归一：未知写法一律当 natural。
fn normalize_grade(s: &str) -> String {
    match normalize_word(s).as_str() {
        GRADE_DORMANT => GRADE_DORMANT.to_string(),
        GRADE_EAGER => GRADE_EAGER.to_string(),
        _ => GRADE_NATURAL.to_string(),
    }
}

/// 升格目标归一：不是三档之一就退回 eager（§8.3 的默认升格目标）。
fn normalize_escalate(s: &str) -> String {
    match normalize_word(s).as_str() {
        GRADE_DORMANT => GRADE_DORMANT.to_string(),
        GRADE_NATURAL => GRADE_NATURAL.to_string(),
        _ => DEFAULT_ESCALATE.to_string(),
    }
}

/// 梯度序：dormant(0) < natural(1) < eager(2)（未知写法按 natural）。
fn grade_rank(s: &str) -> u8 {
    match normalize_word(s).as_str() {
        GRADE_DORMANT => 0,
        GRADE_EAGER => 2,
        _ => 1,
    }
}

fn same_state(s: &str, want: &str) -> bool {
    normalize_word(s) == want
}

/// 重要度收敛到 [0,1]；非有限值退回缺省 0.5（脏数据不让排序与分数抖）。
fn sanitize_importance(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        DEFAULT_IMPORTANCE
    }
}

/// 结果/经过的故事时间戳：clock 已含「第N天」时原样用，否则拼上 story_day
/// （与 palace 的回忆行时间戳同一口径）。
fn stamp_text(story_day: i64, story_clock: &str) -> String {
    let clock = story_clock.trim();
    if clock.is_empty() {
        return format!("第{story_day}天");
    }
    if clock.starts_with('第') {
        return clock.to_string();
    }
    format!("第{story_day}天 {clock}")
}

/// 从 `第3天 23:10` 里解出故事天（设计 §8.2 的 opened 只有 turn + story_clock）。
fn parse_day_from_clock(clock: &str) -> Option<i64> {
    let idx = clock.find('第')?;
    let rest = &clock[idx + '第'.len_utf8()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok()
}

/// C1 条目里的状态词（给人/模型看的中文）。
fn state_label(state: &str) -> &'static str {
    if same_state(state, STATE_RESOLVED) {
        "已了结"
    } else if same_state(state, STATE_ABANDONED) {
        "已放弃"
    } else {
        "进行中"
    }
}

/// B1 条目的序号（①②③…；超过 10 条退回 `(n)`）。
fn circled(n: usize) -> String {
    const MARKS: [char; 10] = ['①', '②', '③', '④', '⑤', '⑥', '⑦', '⑧', '⑨', '⑩'];
    match n {
        1..=10 => MARKS[n - 1].to_string(),
        other => format!("({other})"),
    }
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

fn text_list(list: &[String]) -> Value {
    Value::Array(list.iter().map(|s| Value::String(s.clone())).collect())
}

/// f32 → JSON 数字：用最短往返表示（0.7 而不是 0.699999988079071），非有限值写 null。
/// threads.json 快照由 to_value 直接落盘，短表示让它与设计 §8.2 的写法一致。
fn f32_value(x: f32) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    serde_json::from_str::<Value>(&x.to_string()).unwrap_or(Value::Null)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

// ---------- 宽松取字段（单个字段类型不对不让整条线报废）----------

fn required_text(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    match get_text(map, key) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(format!("剧情线缺 {key}")),
    }
}

fn get_text(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(as_text)
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// 字符串列表：数组逐项取字符串，单个值当单元素列表（"actors": "小雨" 也认）。
fn get_str_list(map: &Map<String, Value>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(as_text)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(other) => as_text(other)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}

/// 词/名/段列表：数组或单值；空白项丢弃；全空返回 None（调用方据此丢弃该条件）。
fn words_of(v: Option<&Value>) -> Option<Vec<String>> {
    let list = match v? {
        Value::Array(items) => items
            .iter()
            .filter_map(as_text)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<String>>(),
        other => as_text(other)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
    };
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

/// 多别名取第一个存在的键（固定顺序，确定性）。
fn first_key<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| map.get(*k))
}

fn get_u64(map: &Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key).and_then(as_u64)
}

fn get_i64(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key).and_then(as_i64)
}

fn get_f32(map: &Map<String, Value>, key: &str) -> Option<f32> {
    map.get(key).and_then(as_f32)
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().and_then(f64_as_u64)),
        Value::String(s) => s
            .trim()
            .parse::<u64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().and_then(f64_as_u64)),
        _ => None,
    }
}

fn f64_as_u64(f: f64) -> Option<u64> {
    if f.is_finite() && f >= 0.0 {
        Some(f.floor().min(u64::MAX as f64) as u64)
    } else {
        None
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().and_then(f64_as_i64)),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().and_then(f64_as_i64)),
        _ => None,
    }
}

fn f64_as_i64(f: f64) -> Option<i64> {
    if f.is_finite() && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Some(f.floor() as i64)
    } else {
        None
    }
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.trim().parse::<f32>().ok(),
        _ => None,
    }
}

// ---------- serde 桥（to_value/from_value 是主形态，serde 与它逐字一致）----------

macro_rules! value_codec {
    ($($t:ty),* $(,)?) => { $(
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.to_value().serialize(s)
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let v = Value::deserialize(d)?;
                <$t>::parse(&v).map_err(serde::de::Error::custom)
            }
        }
    )* };
}

value_codec!(
    ThreadStamp,
    ProgressNode,
    Deadline,
    Resolution,
    ResurfaceWindow,
    Resurface,
    Thread,
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- 测试工具 ----------

    /// 一次提及窗口求值的可控环境（宿主每轮交给剧情线的东西的测试替身）。
    struct Env {
        turn: u64,
        story_day: i64,
        clock: String,
        bb: BTreeMap<String, Value>,
        mentions: Vec<String>,
        present: Vec<String>,
        path: Vec<String>,
    }

    impl Env {
        fn new(turn: u64, story_day: i64) -> Env {
            Env {
                turn,
                story_day,
                clock: format!("第{story_day}天 18:02"),
                bb: BTreeMap::new(),
                mentions: Vec::new(),
                present: Vec::new(),
                path: Vec::new(),
            }
        }

        fn bb(mut self, k: &str, v: Value) -> Env {
            self.bb.insert(k.to_string(), v);
            self
        }

        fn mentions(mut self, list: &[&str]) -> Env {
            self.mentions = list.iter().map(|s| s.to_string()).collect();
            self
        }

        fn present(mut self, list: &[&str]) -> Env {
            self.present = list.iter().map(|s| s.to_string()).collect();
            self
        }

        fn path(mut self, list: &[&str]) -> Env {
            self.path = list.iter().map(|s| s.to_string()).collect();
            self
        }

        fn q(&self) -> ThreadQuery<'_> {
            ThreadQuery {
                turn: self.turn,
                story_day: self.story_day,
                story_clock: &self.clock,
                blackboard: &self.bb,
                mentions: &self.mentions,
                present: &self.present,
                state_path: &self.path,
            }
        }
    }

    fn stamp(turn: u64, day: i64) -> ThreadStamp {
        ThreadStamp {
            turn,
            story_day: day,
            story_clock: format!("第{day}天 18:02"),
        }
    }

    fn mk(
        id: &str,
        title: &str,
        importance: f32,
        opened_turn: u64,
        windows: Vec<ResurfaceWindow>,
    ) -> Thread {
        let mut t = Thread::open(
            id,
            title,
            "起因：说好了的事",
            &["小雨".into(), "玩家".into()],
            importance,
            stamp(opened_turn, 1),
        );
        t.resurface.windows = windows;
        t
    }

    fn bb_window(pairs: &[(&str, Value)]) -> ResurfaceWindow {
        ResurfaceWindow::Blackboard(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn mention(words: &[&str]) -> ResurfaceWindow {
        ResurfaceWindow::Mention(words.iter().map(|s| s.to_string()).collect())
    }

    fn actors(names: &[&str]) -> ResurfaceWindow {
        ResurfaceWindow::ActorsWith(names.iter().map(|s| s.to_string()).collect())
    }

    fn state_path(segs: &[&str]) -> ResurfaceWindow {
        ResurfaceWindow::StatePath(segs.iter().map(|s| s.to_string()).collect())
    }

    fn ids(picks: &[ResurfacePick]) -> Vec<String> {
        picks.iter().map(|p| p.id.clone()).collect()
    }

    // ---------- 开线、往返与默认值 ----------

    #[test]
    fn open_uses_conservative_defaults() {
        let t = Thread::open(
            "   ",
            "  周五还书的约定  ",
            "  起因  ",
            &[" 小雨 ".into(), "".into(), "玩家".into()],
            3.0,
            stamp(14, 3),
        );
        assert_eq!(t.id, "thread.周五还书的约定", "空 id 用标题兜一个");
        assert_eq!(t.title, "周五还书的约定");
        assert_eq!(t.cause, "起因");
        assert_eq!(t.actors, vec!["小雨".to_string(), "玩家".to_string()]);
        assert_eq!(t.importance, 1.0, "importance 收敛到 [0,1]");
        assert_eq!(t.state, STATE_ACTIVE);
        assert_eq!(t.scope, SCOPE_SESSION);
        assert!(t.progress.is_empty(), "开线只落起因，经过由 touch 追加");
        assert!(t.resolution.is_none());
        assert!(t.linked_intent.is_none());
        assert_eq!(t.resurface.grade, GRADE_NATURAL);
        assert_eq!(t.resurface.cooldown, DEFAULT_COOLDOWN);
        assert!(t.resurface.deadline.is_none());
        assert_eq!(
            t.resurface.windows,
            vec![mention(&["周五还书的约定"])],
            "§8.3：手动/树开线默认 natural + 仅 mention 窗口"
        );
    }

    #[test]
    fn thread_round_trips_through_value_and_json() {
        let mut t = mk(
            "thread.还书",
            "周五还书的约定",
            0.7,
            14,
            vec![
                bb_window(&[("day", json!(5))]),
                mention(&["还书", "借书卡"]),
                ResurfaceWindow::All(vec![
                    bb_window(&[("place", json!("图书馆"))]),
                    actors(&["小雨"]),
                ]),
            ],
        );
        t.touch(14, "立约", Some("mem_0192".into()));
        t.resurface.deadline = Some(Deadline {
            day: 6,
            escalate: GRADE_EAGER.into(),
        });
        t.resurface.framing = "她在意但不好意思催。".into();
        t.mark_mentioned(20);
        t.scope = SCOPE_WORLD.into();
        t.linked_intent = Some("intent.说出工作牌".into());
        t.resolve(30, 5, "第5天 18:02", "玩家如约还书");

        let v = t.to_value();
        assert_eq!(v["id"], json!("thread.还书"));
        assert_eq!(v["importance"], json!(0.7), "f32 用最短往返表示");
        assert_eq!(v["resolution"]["story_day"], json!(5));
        let back = Thread::from_value(&v).unwrap();
        assert_eq!(back, t, "to_value → from_value 往返一致");

        let text = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Thread>(&text).unwrap(), t);
        assert!(text.contains("\"resolved\""));
        assert!(text.contains("\"resurface\""));
    }

    #[test]
    fn from_value_fills_defaults_and_rejects_missing_identity() {
        let t = Thread::from_value(&json!({"id": "thread.a", "title": "A"})).unwrap();
        assert_eq!(t.state, STATE_ACTIVE);
        assert_eq!(t.scope, SCOPE_SESSION);
        assert_eq!(t.importance, DEFAULT_IMPORTANCE, "缺 importance → 0.5");
        assert_eq!(t.resurface.grade, GRADE_NATURAL, "缺 grade → natural");
        assert_eq!(t.resurface.cooldown, DEFAULT_COOLDOWN, "缺 cooldown → 5");
        assert!(t.resurface.windows.is_empty(), "无窗口 = 永不进 B1（保守默认）");
        assert_eq!(t.opened, ThreadStamp::default());

        assert!(Thread::from_value(&json!({"title": "A"})).is_err(), "缺 id 报错");
        assert!(Thread::from_value(&json!({"id": "thread.a"})).is_err(), "缺 title 报错");
        assert!(
            Thread::from_value(&json!({"id": "  ", "title": "A"})).is_err(),
            "空白 id 等同缺失"
        );
        assert!(Thread::from_value(&json!([])).is_err(), "非对象不是线");

        let t = Thread::from_value(&json!({
            "id": "thread.b",
            "title": "B",
            "state": "weird",
            "resurface": {"grade": "WEIRD", "cooldown": 0}
        }))
        .unwrap();
        assert!(t.is_active(), "未知 state 归一为 active");
        assert_eq!(t.resurface.grade, GRADE_NATURAL);
        assert_eq!(t.resurface.cooldown, 0, "显式 0 = 无冷却");
    }

    #[test]
    fn design_sample_snapshot_parses() {
        let v = json!({
          "id": "thread.周五还书",
          "title": "周五还书的约定",
          "cause": "玩家忘带借书卡，小雨破例让他先把书拿走，约定周五来还。",
          "actors": ["小雨", "玩家"],
          "importance": 0.7,
          "opened": { "turn": 14, "story_clock": "第3天 23:10" },
          "state": "active",
          "progress": [ { "turn": 14, "note": "立约", "memory": "mem_0192" } ],
          "resurface": {
            "grade": "natural",
            "windows": [
              { "blackboard": { "day": 5 } },
              { "mention": ["还书", "借书卡"] },
              { "blackboard": { "place": "图书馆" }, "actors_with": ["小雨"] }
            ],
            "deadline": { "day": 6, "escalate": "eager" },
            "cooldown": 5,
            "framing": "她在意但不好意思催；若对方主动提起，会松一口气。"
          }
        });
        let t = Thread::from_value(&v).unwrap();
        assert_eq!(t.opened.story_day, 3, "缺 story_day 时从「第N天」解出");
        assert_eq!(t.opened.story_clock, "第3天 23:10");
        assert_eq!(
            t.progress[0],
            ProgressNode {
                turn: 14,
                note: "立约".into(),
                memory: Some("mem_0192".into())
            }
        );
        assert_eq!(t.resurface.windows.len(), 3);
        assert_eq!(
            t.resurface.windows[2],
            ResurfaceWindow::All(vec![
                bb_window(&[("place", json!("图书馆"))]),
                actors(&["小雨"])
            ]),
            "§8.2 第三个示例是合取窗口"
        );
        assert_eq!(
            t.resurface.deadline,
            Some(Deadline {
                day: 6,
                escalate: GRADE_EAGER.into()
            })
        );
        assert!(t.resolution.is_none());

        let back = Thread::from_value(&t.to_value()).unwrap();
        assert_eq!(back, t, "设计示例形态往返一致");
        assert_eq!(
            back.to_value()["resurface"]["windows"][2],
            json!({"blackboard": {"place": "图书馆"}, "actors_with": ["小雨"]}),
            "合取窗口写回设计的合并对象形态"
        );
    }

    #[test]
    fn malformed_nested_fields_degrade_to_defaults() {
        let v = json!({
          "id": "thread.a",
          "title": "A",
          "actors": "小雨",
          "opened": "第3天 23:10",
          "progress": ["立约", 42, {"turn": 9, "note": "还书"}],
          "resurface": {
            "grade": "EAGER",
            "windows": {"mention": "还书"},
            "cooldown": "6",
            "last_mentioned_turn": 12
          },
          "resolution": {"outcome": "说开了", "turn": 30}
        });
        let t = Thread::from_value(&v).unwrap();
        assert_eq!(t.actors, vec!["小雨".to_string()], "单值当单元素列表");
        assert_eq!(t.opened, ThreadStamp::default(), "坏戳 → 默认戳");
        assert_eq!(t.progress.len(), 2, "数字节点被丢弃");
        assert_eq!(t.progress[0].note, "立约");
        assert_eq!(
            t.progress[1],
            ProgressNode {
                turn: 9,
                note: "还书".into(),
                memory: None
            }
        );
        assert_eq!(t.resurface.grade, GRADE_EAGER, "grade 归一为小写");
        assert_eq!(t.resurface.windows, vec![mention(&["还书"])], "单对象窗口也接受");
        assert_eq!(t.resurface.cooldown, 6, "数字字符串也接受");
        assert_eq!(t.resurface.last_mentioned_turn, Some(12));
        assert_eq!(t.resolution.as_ref().unwrap().outcome, "说开了");
    }

    #[test]
    fn query_new_leaves_word_lists_empty() {
        let bb = BTreeMap::new();
        let q = ThreadQuery::new(7, 3, "第3天 23:10", &bb);
        assert_eq!(q.turn, 7);
        assert_eq!(q.story_day, 3);
        assert_eq!(q.story_clock, "第3天 23:10");
        assert!(q.mentions.is_empty() && q.present.is_empty() && q.state_path.is_empty());
    }

    // ---------- 四种窗口各一例 ----------

    #[test]
    fn blackboard_day_window_is_a_threshold() {
        let t = mk("thread.a", "A", 0.5, 1, vec![bb_window(&[("day", json!(5))])]);
        assert!(t.window_hit(&Env::new(20, 4).q()).is_none(), "还没到那天");
        assert_eq!(
            t.window_hit(&Env::new(21, 5).q()).as_deref(),
            Some("黑板:day≥5")
        );
        assert_eq!(
            t.window_hit(&Env::new(22, 9).q()).as_deref(),
            Some("黑板:day≥5"),
            "过点不关窗，靠 cooldown 抑制复现"
        );
        let env = Env::new(21, 3).bb("day", json!(5));
        assert!(
            t.window_hit(&env.q()).is_some(),
            "黑板 day 与 query.story_day 不一致时以黑板为准（§2.1 属主）"
        );
    }

    #[test]
    fn blackboard_equality_window_matches_place() {
        let t = mk(
            "thread.a",
            "A",
            0.5,
            1,
            vec![bb_window(&[("place", json!("图书馆"))])],
        );
        assert!(t.window_hit(&Env::new(1, 1).q()).is_none(), "黑板没这个键 → 不命中");
        assert!(t
            .window_hit(&Env::new(1, 1).bb("place", json!("操场")).q())
            .is_none());
        assert_eq!(
            t.window_hit(&Env::new(1, 1).bb("place", json!(" 图书馆 ")).q())
                .as_deref(),
            Some("黑板:place=图书馆")
        );
        let w = mk(
            "thread.b",
            "B",
            0.5,
            1,
            vec![bb_window(&[("weather", json!("rainy"))])],
        );
        assert!(
            w.window_hit(&Env::new(1, 1).bb("WEATHER", json!("RAINY")).q())
                .is_some(),
            "键与拉丁值大小写不敏感"
        );
        assert!(
            w.window_hit(&Env::new(1, 1).bb("weather", json!(3)).q())
                .is_none(),
            "类型不符不命中"
        );
        let both = mk(
            "thread.c",
            "C",
            0.5,
            1,
            vec![bb_window(&[("place", json!("图书馆")), ("day", json!(5))])],
        );
        assert!(both
            .window_hit(&Env::new(5, 5).bb("place", json!("图书馆")).q())
            .is_some());
        assert!(both
            .window_hit(&Env::new(4, 4).bb("place", json!("图书馆")).q())
            .is_none());
    }

    #[test]
    fn mention_window_is_latin_case_insensitive_and_cjk_exact() {
        let t = mk("thread.a", "A", 0.5, 1, vec![mention(&["Library", "还书"])]);
        assert_eq!(
            t.window_hit(&Env::new(1, 1).mentions(&["library"]).q())
                .as_deref(),
            Some("提及:Library")
        );
        assert_eq!(
            t.window_hit(&Env::new(1, 1).mentions(&["明天得去还书"]).q())
                .as_deref(),
            Some("提及:还书"),
            "话题擦边：mention 是带上下文的长词时按包含"
        );
        assert!(
            t.window_hit(&Env::new(1, 1).mentions(&["书"]).q()).is_none(),
            "CJK 不模糊：mention 比窗口词短不算命中"
        );
        assert!(t.window_hit(&Env::new(1, 1).mentions(&["借书卡"]).q()).is_none());
        assert!(t.window_hit(&Env::new(1, 1).mentions(&[]).q()).is_none());
        assert!(
            t.window_hit(&Env::new(1, 1).mentions(&["   "]).q()).is_none(),
            "空词不命中"
        );
    }

    #[test]
    fn actors_with_window_requires_every_named_actor() {
        let one = mk("thread.a", "A", 0.5, 1, vec![actors(&["小雨"])]);
        assert_eq!(
            one.window_hit(&Env::new(1, 1).present(&["小雨", "玩家"]).q())
                .as_deref(),
            Some("在场:小雨")
        );
        assert!(one.window_hit(&Env::new(1, 1).present(&["玩家"]).q()).is_none());
        let two = mk("thread.b", "B", 0.5, 1, vec![actors(&["小雨", "玩家"])]);
        assert!(
            two.window_hit(&Env::new(1, 1).present(&["小雨"]).q())
                .is_none(),
            "列了两个人就得都在场"
        );
        assert!(two
            .window_hit(&Env::new(1, 1).present(&["玩家", "小雨"]).q())
            .is_some());
        assert!(
            mk("thread.c", "C", 0.5, 1, vec![actors(&[])])
                .window_hit(&Env::new(1, 1).present(&["小雨"]).q())
                .is_none(),
            "空名单不构成窗口"
        );
    }

    #[test]
    fn state_path_window_hits_any_active_segment() {
        let t = mk("thread.a", "A", 0.5, 1, vec![state_path(&["图书馆"])]);
        assert_eq!(
            t.window_hit(&Env::new(1, 1).path(&["日常", "图书馆"]).q())
                .as_deref(),
            Some("状态:图书馆")
        );
        assert!(t
            .window_hit(&Env::new(1, 1).path(&["日常", "操场"]).q())
            .is_none());
        assert!(t.window_hit(&Env::new(1, 1).q()).is_none(), "空路径不命中");
        assert!(
            t.window_hit(&Env::new(1, 1).path(&["图书馆夜"]).q())
                .is_none(),
            "整段相等，不做包含"
        );
    }

    #[test]
    fn combined_window_needs_all_conditions() {
        let t = mk(
            "thread.a",
            "A",
            0.5,
            1,
            vec![ResurfaceWindow::All(vec![
                bb_window(&[("place", json!("图书馆"))]),
                actors(&["小雨"]),
            ])],
        );
        let both = Env::new(1, 1)
            .bb("place", json!("图书馆"))
            .present(&["小雨", "玩家"]);
        assert_eq!(
            t.window_hit(&both.q()).as_deref(),
            Some("黑板:place=图书馆+在场:小雨")
        );
        assert!(
            t.window_hit(&Env::new(1, 1).bb("place", json!("图书馆")).q())
                .is_none(),
            "她不在场这一半不满足"
        );
        assert!(
            t.window_hit(&Env::new(1, 1).present(&["小雨"]).q())
                .is_none(),
            "地点这一半不满足"
        );
    }

    // ---------- 克制：窗口未命中 / dormant / 冷却 ----------

    #[test]
    fn window_miss_or_dormant_keeps_thread_out_of_b1() {
        let hit_by_day = bb_window(&[("day", json!(5))]);
        let mut dormant = mk("thread.dormant", "深埋的线", 0.9, 1, vec![hit_by_day.clone()]);
        dormant.resurface.grade = GRADE_DORMANT.into();
        let inside = mk("thread.hit", "窗口内的线", 0.5, 1, vec![hit_by_day.clone()]);
        let outside = mk(
            "thread.outside",
            "窗口外的线",
            0.9,
            1,
            vec![bb_window(&[("day", json!(30))])],
        );
        let threads = vec![dormant, inside, outside];
        let env = Env::new(20, 5);
        let picks = select_resurface(&threads, &env.q(), RESURFACE_TOP);
        assert_eq!(
            ids(&picks),
            vec!["thread.hit"],
            "窗口未命中绝不进 B1；dormant 即使命中也不进 B1（§8.4）"
        );
        assert_eq!(
            pending_lines(&threads).len(),
            3,
            "C1 兜底：含 dormant 的全部活跃线都查得到"
        );
    }

    #[test]
    fn cooldown_suppresses_resurface_until_ready() {
        let mut t = mk("thread.a", "A", 0.9, 1, vec![mention(&["还书"])]);
        assert!(t.cooldown_ready(1), "没被提过 → 就绪");
        t.mark_mentioned(10);
        assert_eq!(t.resurface.last_mentioned_turn, Some(10));
        assert!(!t.cooldown_ready(14));
        assert!(t.cooldown_ready(15), "turn - last >= cooldown");
        assert!(
            select_resurface(&[t.clone()], &Env::new(14, 1).mentions(&["还书"]).q(), 3).is_empty(),
            "冷却中不进 B1"
        );
        assert_eq!(
            select_resurface(&[t.clone()], &Env::new(15, 1).mentions(&["还书"]).q(), 3).len(),
            1
        );
        t.mark_mentioned(3);
        assert_eq!(t.resurface.last_mentioned_turn, Some(10), "轮次只前进不回退");
        t.resurface.cooldown = 0;
        assert!(t.cooldown_ready(10), "cooldown = 0 → 不抑制");
    }

    // ---------- 期限与升格 ----------

    #[test]
    fn deadline_due_escalates_once_and_ranks_first() {
        let mut overdue = mk("thread.due", "快到期的线", 0.2, 3, vec![mention(&["还书"])]);
        overdue.resurface.deadline = Some(Deadline {
            day: 6,
            escalate: GRADE_EAGER.into(),
        });
        assert!(!overdue.due(5));
        assert!(overdue.due(6) && overdue.due(9), "到期判据：story_day >= day");
        assert_eq!(due_threads(&[overdue.clone()], 6), vec!["thread.due".to_string()]);
        assert!(overdue.escalate(), "到期升格");
        assert_eq!(overdue.resurface.grade, GRADE_EAGER);
        assert!(!overdue.escalate(), "幂等：已达标不再变");
        assert!(overdue.due(6), "到期状态本身不因升格消失");
        assert!(
            due_threads(&[overdue.clone()], 6).is_empty(),
            "已升格的线不再重复抛 thread:due"
        );
        assert!(due_threads(&[overdue.clone()], 5).is_empty(), "没到期");
        assert!(
            !Thread::open("thread.x", "X", "", &[], 0.5, stamp(1, 1)).escalate(),
            "没有 deadline 谈不上升格"
        );

        let quiet = Env::new(20, 6);
        assert!(
            select_resurface(&[overdue.clone()], &quiet.q(), 3).is_empty(),
            "窗口未命中时，到期的线也不进 B1"
        );
        let talking = Env::new(20, 6).mentions(&["还书"]);
        let big = mk(
            "thread.big",
            "更重要的线",
            0.95,
            1,
            vec![bb_window(&[("day", json!(1))])],
        );
        let picks = select_resurface(&[big, overdue.clone()], &talking.q(), 3);
        assert_eq!(
            ids(&picks),
            vec!["thread.due", "thread.big"],
            "窗口命中时，deadline 档压过更高的 importance"
        );
    }

    // ---------- 收线 / 放弃 / 推进 ----------

    #[test]
    fn resolve_writes_resolution_and_leaves_selection() {
        let mut t = mk("thread.a", "A", 0.5, 1, vec![mention(&["还书"])]);
        let r = t.resolve(30, 5, "第5天 18:02", "玩家如约还书，小雨送了张便签");
        assert_eq!(
            r,
            Resolution {
                turn: 30,
                story_day: 5,
                story_clock: "第5天 18:02".into(),
                outcome: "玩家如约还书，小雨送了张便签".into(),
                memory: None,
            }
        );
        assert_eq!(t.state, STATE_RESOLVED);
        assert_eq!(t.resolution.as_ref(), Some(&r), "返回的 Resolution 供宿主写结果记忆");
        assert!(!t.is_active());
        assert!(!active(&[t.clone()], "thread.a") && resolved(&[t.clone()], "thread.a"));
        assert!(
            select_resurface(&[t.clone()], &Env::new(31, 5).mentions(&["还书"]).q(), 3).is_empty(),
            "收线后移出现状卡「心里有事」"
        );
        assert!(pending_lines(&[t.clone()]).is_empty());
        assert_eq!(recent_resolutions(&[t.clone()], 5, 31).len(), 1, "转进「了结未远」");
        assert!(t.attach_resolution_memory("mem_0240"));
        assert_eq!(t.resolution.as_ref().unwrap().memory.as_deref(), Some("mem_0240"));
        assert!(!t.attach_resolution_memory("  "), "空记忆 id 不接受");
    }

    #[test]
    fn abandon_records_note_and_is_neither_active_nor_resolved() {
        let mut t = mk("thread.a", "A", 0.5, 1, vec![mention(&["还书"])]);
        t.touch(2, "她提了一次", None);
        t.abandon("长期无进展，不了了之");
        assert_eq!(t.state, STATE_ABANDONED);
        assert!(
            t.resolution.is_none(),
            "放弃不是收线：没有结果记忆、不发 resolved 事件"
        );
        assert_eq!(
            t.progress.last().unwrap(),
            &ProgressNode {
                turn: 2,
                note: "长期无进展，不了了之".into(),
                memory: None,
            },
            "说明挂在最近已知轮次的经过节点上"
        );
        assert!(!active(&[t.clone()], "thread.a") && !resolved(&[t.clone()], "thread.a"));
        assert!(pending_lines(&[t.clone()]).is_empty());
        assert!(recent_resolutions(&[t.clone()], 0, 99).is_empty(), "了结未远只统计收线");

        let mut u = mk("thread.b", "B", 0.5, 7, vec![]);
        u.abandon("   ");
        let node = u.progress.last().unwrap();
        assert_eq!(node.note, "不了了之", "空说明也有可读的默认文本");
        assert_eq!(node.turn, 7, "没有经过节点时用开线轮次");
    }

    #[test]
    fn touch_appends_progress_and_skips_empty_nodes() {
        let mut t = mk("thread.a", "A", 0.5, 1, vec![]);
        t.touch(14, "  立约  ", Some(" mem_0192 ".into()));
        assert_eq!(
            t.progress[0],
            ProgressNode {
                turn: 14,
                note: "立约".into(),
                memory: Some("mem_0192".into()),
            }
        );
        t.touch(15, "", None);
        t.touch(16, "   ", Some("  ".into()));
        assert_eq!(t.progress.len(), 1, "无内容的节点不落");
        t.touch(17, "", Some("mem_0200".into()));
        assert_eq!(t.progress.len(), 2, "只挂记忆不写说明也是有效节点");
        assert!(t.is_active(), "touch 不改 state");
    }

    // ---------- 选择：排序与截断 ----------

    #[test]
    fn select_resurface_orders_by_tier_then_importance_then_open_turn_then_id() {
        let mut due = mk("thread.due", "due", 0.1, 1, vec![mention(&["a"])]);
        due.resurface.deadline = Some(Deadline {
            day: 1,
            escalate: GRADE_EAGER.into(),
        });
        let bb = mk("thread.bb", "bb", 0.1, 1, vec![bb_window(&[("day", json!(1))])]);
        let act = mk("thread.act", "act", 0.1, 1, vec![actors(&["小雨"])]);
        let st = mk("thread.state", "state", 1.0, 1, vec![state_path(&["日常"])]);
        let men = mk("thread.mention", "mention", 1.0, 1, vec![mention(&["a"])]);
        let env = Env::new(1, 1)
            .mentions(&["a"])
            .present(&["小雨"])
            .path(&["日常"]);
        let picks = select_resurface(
            &[men.clone(), st.clone(), act.clone(), bb.clone(), due.clone()],
            &env.q(),
            0,
        );
        assert_eq!(
            ids(&picks),
            vec![
                "thread.due",
                "thread.act",
                "thread.bb",
                "thread.state",
                "thread.mention"
            ],
            "档位：deadline 升格 > 黑板/在场 > 状态路径 > 话题擦边；同档按 id 升序"
        );
        assert_eq!(picks[0].reason, "提及:a");
        assert_eq!(
            picks[0].grade, GRADE_NATURAL,
            "到期只提档位：宿主还没 escalate 时 grade 仍是 natural"
        );
        let mut escalated = due.clone();
        assert!(escalated.escalate());
        let picks = select_resurface(&[escalated], &env.q(), 3);
        assert_eq!(picks[0].grade, GRADE_EAGER, "升格后的档位随快照可见");

        let low = mk("thread.z_low", "z", 0.2, 1, vec![mention(&["a"])]);
        let high_old = mk("thread.z_high_old", "z", 0.8, 2, vec![mention(&["a"])]);
        let high_new = mk("thread.z_high_new", "z", 0.8, 9, vec![mention(&["a"])]);
        let picks = select_resurface(
            &[low.clone(), high_new.clone(), high_old.clone()],
            &env.q(),
            0,
        );
        assert_eq!(
            ids(&picks),
            vec!["thread.z_high_old", "thread.z_high_new", "thread.z_low"],
            "同档：importance 降序 → opened.turn 升序"
        );
        let a = mk("thread.a", "a", 0.5, 1, vec![mention(&["a"])]);
        let b = mk("thread.b", "b", 0.5, 1, vec![mention(&["a"])]);
        assert_eq!(
            ids(&select_resurface(&[b.clone(), a.clone()], &env.q(), 0)),
            vec!["thread.a", "thread.b"],
            "同重要度同轮次 → id 升序"
        );
    }

    #[test]
    fn select_resurface_truncates_top() {
        let threads: Vec<Thread> = (0..5)
            .map(|i| {
                mk(
                    &format!("thread.{i}"),
                    "x",
                    1.0 - i as f32 * 0.1,
                    1,
                    vec![mention(&["a"])],
                )
            })
            .collect();
        let env = Env::new(1, 1).mentions(&["a"]);
        assert_eq!(select_resurface(&threads, &env.q(), 2).len(), 2);
        assert_eq!(select_resurface(&threads, &env.q(), 0).len(), 5, "top = 0 不限量");
        assert_eq!(
            ids(&select_resurface(&threads, &env.q(), RESURFACE_TOP)),
            vec!["thread.0", "thread.1", "thread.2"],
            "B1 默认取 top 3（§8.5 top 2–3）"
        );
    }

    #[test]
    fn selection_is_deterministic() {
        let mut a = mk(
            "thread.a",
            "a",
            0.5,
            1,
            vec![mention(&["x"]), bb_window(&[("day", json!(1))])],
        );
        a.resurface.framing = "  她在意  ".into();
        let b = mk("thread.b", "b", 0.5, 1, vec![mention(&["x"])]);
        let env = Env::new(1, 1).mentions(&["x"]);
        let first = select_resurface(&[a.clone(), b.clone()], &env.q(), 3);
        let second = select_resurface(&[a.clone(), b.clone()], &env.q(), 3);
        assert_eq!(first, second, "两次调用同结果");
        assert_eq!(
            first,
            select_resurface(&[b.clone(), a.clone()], &env.q(), 3),
            "输入次序不影响输出次序（可回放）"
        );
        assert_eq!(
            select_resurface(&[a.clone(), a.clone()], &env.q(), 3).len(),
            1,
            "同 id 的快照只取第一条"
        );
        assert!(
            first.iter().all(|p| (0.0..=1.0).contains(&p.score)),
            "展示分落在 0–1"
        );
        assert_eq!(
            first[0].score,
            select_resurface(&[a.clone(), b.clone()], &env.q(), 3)[0].score
        );
        assert_eq!(first[0].reason, "提及:x", "窗口按声明顺序求值，首个命中即理由");
    }

    #[test]
    fn render_concerns_matches_the_design_line() {
        let mut a = mk("thread.a", "周五还书之约", 0.9, 1, vec![mention(&["还书"])]);
        a.resurface.framing = "明天到期，她在意但不好意思催".into();
        let b = mk("thread.b", "工作牌之事", 0.5, 1, vec![mention(&["还书"])]);
        let env = Env::new(1, 1).mentions(&["还书"]);
        let picks = select_resurface(&[a, b], &env.q(), 2);
        assert_eq!(
            render_concerns(&picks),
            vec![
                "①周五还书之约——明天到期，她在意但不好意思催",
                "②工作牌之事"
            ],
            "§4.1 的「心里有事」条目形态：无 framing 只给标题"
        );
        assert!(render_concerns(&[]).is_empty());
    }

    // ---------- C1 / 了结未远 / 判据 ----------

    #[test]
    fn pending_lines_is_the_full_readonly_projection() {
        let mut dormant = mk("thread.dormant", "深埋的线", 0.3, 1, vec![]);
        dormant.resurface.grade = GRADE_DORMANT.into();
        let mut done = mk("thread.done", "已了结的线", 0.9, 1, vec![]);
        done.resolve(9, 2, "第2天 09:00", "说开了");
        let mut gone = mk("thread.gone", "已放弃的线", 0.9, 1, vec![]);
        gone.abandon("不了了之");
        let hot = mk("thread.hot", "要紧的线", 0.9, 3, vec![]);
        let cold = mk("thread.cold", "次要紧的线", 0.5, 2, vec![]);
        let lines = pending_lines(&[cold, dormant, gone, done, hot]);
        assert_eq!(
            lines,
            vec!["要紧的线（进行中）", "次要紧的线（进行中）", "深埋的线（进行中）"],
            "全部活跃线、重要度降序、仅标题与状态（§2.1 规则 4）"
        );
        assert!(pending_lines(&[]).is_empty());
    }

    #[test]
    fn recent_resolutions_is_a_recent_window_of_outcomes() {
        let mut old = mk("thread.old", "旧事", 0.5, 1, vec![]);
        old.resolve(10, 2, "第2天 20:00", "旧事说开了");
        let mut fresh = mk("thread.fresh", "新事", 0.5, 1, vec![]);
        fresh.resolve(50, 6, "第6天 18:02", "工作牌之事已坦白");
        let mut blank = mk("thread.blank", "空白结果", 0.5, 1, vec![]);
        blank.resolve(51, 6, "第6天 19:00", "   ");
        let mut future = mk("thread.future", "还没发生的收线", 0.5, 1, vec![]);
        future.resolve(60, 7, "第7天 08:00", "提前收线");
        let threads = vec![old, fresh, blank, future];

        assert_eq!(
            recent_resolutions(&threads, 5, 51),
            vec!["空白结果（第6天 19:00）", "工作牌之事已坦白（第6天 18:02）"],
            "最近的在前；outcome 为空回退标题；未来轮次不注入"
        );
        assert_eq!(
            recent_resolutions(&threads, 5, 56),
            vec!["空白结果（第6天 19:00）"],
            "窗口按 turn 差算"
        );
        assert_eq!(
            recent_resolutions(&threads, 0, 51),
            vec![
                "空白结果（第6天 19:00）",
                "工作牌之事已坦白（第6天 18:02）",
                "旧事说开了（第2天 20:00）"
            ],
            "within_turns = 0 不限窗口"
        );
    }

    #[test]
    fn active_and_resolved_match_normalized_ids() {
        let t = mk("thread.周五还书", "A", 0.5, 1, vec![]);
        assert!(active(&[t.clone()], "thread.周五还书"));
        assert!(active(&[t.clone()], "  thread.周五还书  "), "trim 后比较");
        assert!(!active(&[t.clone()], "thread.别的"));
        assert!(!active(&[t.clone()], ""), "空 id 不命中（树判据不能误命中）");
        assert!(!resolved(&[t.clone()], "thread.周五还书"));
        let mut r = t.clone();
        r.resolve(9, 3, "第3天 10:00", "还了");
        assert!(resolved(&[r.clone()], "thread.周五还书"));
        assert!(!active(&[r.clone()], "thread.周五还书"));
        let mut a = t.clone();
        a.abandon("算了");
        assert!(!active(&[a.clone()], "thread.周五还书") && !resolved(&[a], "thread.周五还书"));
        assert!(!active(&[], "thread.周五还书"));
    }

    #[test]
    fn due_threads_lists_pending_escalations_only() {
        let mut a = mk("thread.a", "A", 0.5, 1, vec![]);
        a.resurface.deadline = Some(Deadline {
            day: 4,
            escalate: GRADE_EAGER.into(),
        });
        let mut b = mk("thread.b", "B", 0.9, 1, vec![]);
        b.resurface.deadline = Some(Deadline {
            day: 4,
            escalate: GRADE_EAGER.into(),
        });
        let mut done = mk("thread.done", "D", 0.9, 1, vec![]);
        done.resurface.deadline = Some(Deadline {
            day: 4,
            escalate: GRADE_EAGER.into(),
        });
        done.resolve(5, 4, "第4天 10:00", "提前了结");
        let c = mk("thread.c", "C", 0.5, 1, vec![]);
        assert_eq!(
            due_threads(&[a.clone(), b.clone(), done, c], 4),
            vec!["thread.b", "thread.a"],
            "重要度降序；收线与无期限的线都不在内"
        );
        assert!(due_threads(&[a.clone(), b.clone()], 3).is_empty(), "没到期");
        let mut eager = mk("thread.eager", "E", 0.5, 1, vec![]);
        eager.resurface.grade = GRADE_EAGER.into();
        eager.resurface.deadline = Some(Deadline {
            day: 4,
            escalate: GRADE_EAGER.into(),
        });
        assert!(due_threads(&[eager], 4).is_empty(), "已 eager（提案升格）不再抛 due");
    }

    // ---------- 零散契约 ----------

    #[test]
    fn importance_is_sanitized() {
        let bad = Thread::from_value(&json!({
            "id": "thread.n",
            "title": "N",
            "importance": "不是数"
        }))
        .unwrap();
        assert_eq!(bad.importance, DEFAULT_IMPORTANCE);
        let big =
            Thread::from_value(&json!({"id": "thread.b", "title": "B", "importance": 3})).unwrap();
        assert_eq!(big.importance, 1.0);
        let neg = mk("thread.neg", "N", -1.0, 1, vec![]);
        assert_eq!(neg.importance, 0.0);
        // JSON 数字放不下 NaN/inf，用字符串喂进来（解析成 f32::NAN / INFINITY）
        let nan = Thread::from_value(&json!({
            "id": "thread.nan",
            "title": "NaN",
            "importance": "NaN"
        }))
        .unwrap();
        assert_eq!(nan.importance, DEFAULT_IMPORTANCE, "NaN → 缺省 0.5");
        let inf = Thread::from_value(&json!({
            "id": "thread.inf",
            "title": "inf",
            "importance": "1e400"
        }))
        .unwrap();
        assert_eq!(inf.importance, DEFAULT_IMPORTANCE, "溢出成 inf → 缺省 0.5");
        let serialized = nan.to_value();
        assert_eq!(serialized["importance"], json!(0.5));
    }

    #[test]
    fn id_from_title_is_stable_and_readable() {
        assert_eq!(id_from_title("周五还书的约定"), "thread.周五还书的约定");
        assert_eq!(id_from_title("  周五   还书  "), "thread.周五 还书");
        assert_eq!(id_from_title(""), "thread.未命名");
    }

    #[test]
    fn windows_drop_unrecognized_and_keep_the_rest() {
        let t = Thread::from_value(&json!({
            "id": "thread.a",
            "title": "A",
            "resurface": {
                "grade": "natural",
                "windows": [
                    {"chrome": 1},
                    {"blackboard": {}},
                    {"mention": []},
                    {"mention": ["还书"]},
                    {"state_path": "图书馆"}
                ]
            }
        }))
        .unwrap();
        assert_eq!(
            t.resurface.windows,
            vec![mention(&["还书"]), state_path(&["图书馆"])],
            "无法识别的窗口丢弃（宁可少提不可乱提），单值当单元素"
        );
        assert!(ResurfaceWindow::from_value(&json!("还书")).is_none());
        assert!(Resurface::default().window_hit(&Env::new(1, 1).q()).is_none());
    }
}
