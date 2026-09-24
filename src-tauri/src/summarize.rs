// 自动总结管线（M2.6 · 设计 §5.3）：滑窗淘汰批次的六类产物
//
// 消息滑出 L0 窗口时，轮末**异步**触发一次总结调用（不阻塞对话；失败重试，批次可从事件流
// 回放重算）。本模块只负责这条管线的**引擎侧纯逻辑**——拼提示词、解析模型回复、校验与
// 夹紧、产出提案对象：不读文件、不联网、不跑 Lua（m2.md 决断 3）。
//
// 六类产物与设计 §5.3 那张图逐条对应（也是 build_prompt 逐条要账的清单）：
//
//   1. L1 摘要增量   summary_delta —— 编年史体、第三人称，并入 summary.md（§5.1）
//      世界层大事记  chronicle    —— 只记公开事件，跨场景共享（M3.2 · §10.4 摘要分卷）
//   2. 情景记忆      episodes      —— episode 形态入宫殿（§5.2：salience/emotion/links/thread）
//      转述记忆      hearsays      —— episodes 的伴生清单（M3.3 · §10.4）：本批剧情里
//                                    A 把某事告诉了 B → 宿主为每个听众各写一条 hearsay
//                                    （content=转述内容、source=告知者、salience 折半、
//                                    links 继承）——信息跨视角流动的唯一通道
//   3. L3 事实键值   facts         —— 跨会话持久的事实（§5.1）
//   4. 剧情线提案    threads       —— 开线 + **提及时机起草**（§8.3）：grade（克制梯度）/
//                                    windows（可提及窗口，直接写成 §8.2 的 JSON 形态）/
//                                    deadline / cooldown / framing；宿主喂
//                                    threads::ResurfaceWindow::from_value 即可用
//   5. 心理评价提案  psyche        —— 需要满足/受挫 → 情绪与意图增减（§9.2 评价闭环）
//   6. 设定提案      codex         —— 新实体/新事实/事实变更/新关系 → 设定收件箱（§6.8）
//   7. 关联审计      audit         —— 对照「实体清单 + 当轮激活记录」报告疑似漏激活 /
//                                    缺失 facet / 该立的新事实（M3.10 · §6.13）：
//                                    漏激活与缺失 facet 是给收件箱的提示条目，新事实
//                                    经宿主并入 codex 提案走同一条确认链路
//
// 分工：宿主做三件事——① 拼批次与上下文交给 build_prompt；② 用便宜档 provider（util 角色，
// §11 / m2.md M2.6）调模型；③ 把 parse_outcome + sanitize 的结果落成事件（记忆对象 / 线 /
// 设定提案 / 摘要增量）。本模块**刻意不做**的事：不分配 mem_ 前缀 id（宿主用 palace::next_id）、
// 不读故事天与时钟（宿主从黑板给）、不判 anchors 冲突（写入前用 codex::anchors_conflict 校验，
// §6.8「最高保护级」）——那些都需要本模块拿不到的世界状态。
//
// 草稿纪律（§6.9 / m2.md 风险 1）：一切 LLM 产物**先落草稿**——codex 提案进设定收件箱、
// 线经收件箱确认后再开、情景记忆与事实由宿主按配置决定自动或询问；未经确认的不进注入。
//
// 确定性与容错：同输入必然同输出（无随机、无哈希迭代顺序参与输出）；parse_outcome 对模型
// 回复极宽容（围栏 / 前后杂文 / 缺字段 / 类型不符都能活），但**解析失败给 Err 且信息可读**，
// 由宿主决定重试或丢弃；任何输入都不 panic。
//
// 与相邻模块的口径统一：显著度缺省取 palace::DEFAULT_SALIENCE、重要度与冷却取
// threads::DEFAULT_IMPORTANCE / threads::DEFAULT_COOLDOWN、情绪与意图强度缺省取
// psyche::DEFAULT_AFFECT_INTENSITY / psyche::DEFAULT_INTENT_STRENGTH——同一份缺省只写一次，
// 免得管线与读侧漂移。
#![allow(dead_code)]

use serde_json::{Map, Value};

use crate::palace;
use crate::psyche;
use crate::store::Message;
use crate::threads;

// ---------- 常量：条数上限（防「产物洪水」把收件箱与宫殿冲垮）----------

/// 单批情景记忆条数上限（§5.3；宁少勿滥）。
pub const MAX_EPISODES: usize = 6;
/// 单批转述提案条数上限（§10.4；与情景记忆同档——转述是「被人讲起的事」，不该比亲历多）。
pub const MAX_HEARSAYS: usize = 6;
/// 单批剧情线提案条数上限（§8.3；开线是大事，不能一轮开一堆）。
pub const MAX_THREADS: usize = 2;
/// 单批设定提案条数上限（§6.8；收件箱要人审，给太多等于没给）。
pub const MAX_CODEX_DRAFTS: usize = 6;
/// 单批关联审计条数上限（M3.10 · §6.13；审计是提示不是任务清单，宁少勿滥）。
pub const MAX_AUDIT: usize = 6;

/// 审计发现类型（M3.10 · §6.13）：疑似被涉及但未激活的实体（trie/语义都漏了）。
pub const AUDIT_MISSED: &str = "missed";
/// 审计发现类型：既有实体缺了剧情正在用的 facet。
pub const AUDIT_FACET: &str = "facet";
/// 审计发现类型：该立的新事实（宿主并入 codex 提案走收件箱确认链路）。
pub const AUDIT_FACT: &str = "fact";

/// 心理评价类型（§9.2）：情绪事件。
pub const PSYCHE_FEEL: &str = "feel";
/// 心理评价类型（§9.2）：意图增减（值可为负：受挫削弱意图）。
pub const PSYCHE_INTEND: &str = "intend";

/// 设定提案类型（§6.8）：全新实体（必须人工确认）。
pub const CODEX_NEW_ENTITY: &str = "new_entity";
/// 设定提案类型（§6.8）：给既有实体加一条事实。
pub const CODEX_NEW_FACT: &str = "new_fact";
/// 设定提案类型（§6.8）：改写既有事实。
pub const CODEX_FACT_CHANGE: &str = "fact_change";
/// 设定提案类型（§6.8）：新关系。
pub const CODEX_RELATION: &str = "relation";
/// 设定提案类型（M3.8 · §6.8-2）：瞬时状态（「现在下雨了」）——宿主直接写黑板。
pub const CODEX_TRANSIENT: &str = "transient";

/// 输出骨架（写进提示词，也是 parse_outcome 的契约，见模块头注释）。
///
/// 刻意写成**合法 JSON**：测试会把它解一遍，保证骨架与解析器不脱节。里面的示例值只是
/// 格式示范，模型要按本批消息重写（骨架里的字面值不该被照抄进产物）。
pub const OUTCOME_SCHEMA_HINT: &str = r#"{
  "summary_delta": "第三人称编年史体，2–6 句；没有进展给空串",
  "chronicle": "世界层大事记，只记公开发生、别的场景也该知道的大事，1–2 句；没有给空串",
  "episodes": [
    {
      "content": "一句话：谁在哪里做了什么、结果如何",
      "salience": 0.8,
      "emotion": "温暖",
      "place": "图书馆",
      "actors": ["小雨", "玩家"],
      "witnesses": ["小雨"],
      "links": ["topic:便签", "person:小雨", "place:图书馆"],
      "thread": "thread.周五还书",
      "turns": [14]
    }
  ],
  "hearsays": [
    {
      "content": "小雨听说图书馆要拆了，转告了玩家",
      "source": "小雨",
      "listeners": ["玩家"],
      "salience": 0.8,
      "emotion": "怅然",
      "place": "图书馆",
      "links": ["topic:拆迁", "person:小雨"],
      "thread": "thread.周五还书",
      "turns": [15],
      "reveals": []
    }
  ],
  "facts": [{ "key": "玩家名字", "value": "阿澈" }],
  "threads": [
    {
      "title": "周五还书的约定",
      "cause": "玩家忘带借书卡，小雨破例让他先把书带走，约定周五来还。",
      "actors": ["小雨", "玩家"],
      "importance": 0.7,
      "grade": "natural",
      "windows": [
        { "blackboard": { "day": 5 } },
        { "mention": ["还书", "借书卡"] },
        { "blackboard": { "place": "图书馆" }, "actors_with": ["小雨"] }
      ],
      "deadline_day": 6,
      "cooldown": 5,
      "framing": "她在意但不好意思催；若对方主动提起，会松一口气。"
    }
  ],
  "psyche": [
    { "kind": "feel", "name": "忐忑", "intensity": 0.6, "source": "需要「被信任」受挫" },
    { "kind": "intend", "name": "想解释", "intensity": 0.4, "source": "被误解" }
  ],
  "codex": [
    {
      "kind": "new_entity",
      "target": "char.墨墨",
      "value": { "type": "char", "name": "墨墨", "facts": { "look.impression": "一只黑猫" } },
      "reason": "第 14 轮即兴提到她养了一只叫墨墨的猫"
    },
    { "kind": "new_fact", "target": "char.小雨", "value": { "facet": "schedule", "value": "周三休息" }, "reason": "第 15 轮提到" },
    { "kind": "transient", "target": "", "value": { "key": "天气", "value": "雨渐大" }, "reason": "第 15 轮" }
  ],
  "audit": [
    { "finding": "missed", "target": "char.小雨", "evidence": "第 15 轮「她今晚不在」用代词指小雨，但激活记录里没有她" },
    { "finding": "facet", "target": "char.小雨", "facet": "schedule", "evidence": "多轮提到夜班，设定里没有作息 facet" },
    { "finding": "fact", "target": "char.小雨", "facet": "look.hair", "value": "齐肩短发", "evidence": "第 16 轮她剪了短发，明天还成立" }
  ]
}"#;

// ---------- 上下文与批次（宿主拼给 build_prompt 的输入）----------

/// 拼提示词的上下文：宿主从卡片 / 会话 / 黑板 / 剧情线 / 设定集里取到的**只读快照**。
///
/// 全部字段都是引用（拼提示词不改宿主状态）；没有对应数据的可选字段给 None / 空切片。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SummaryContext<'a> {
    /// 角色卡名（如 「小雨」）。
    pub card_name: &'a str,
    /// 玩家角色（persona）名；未设定时 None。
    pub persona_name: Option<&'a str>,
    /// 会话起因 premise（§4.1 新会话向导三问；也是 §8.3 开线来源之一）。
    pub premise: Option<&'a str>,
    /// 本批所属场景的标题（M3.2 摘要分卷；None = 未分场景/世界层批次）。
    pub scene_label: Option<&'a str>,
    /// 故事时钟（如 「第3天 23:40」；宿主从黑板取）。
    pub story_clock: &'a str,
    /// 已有 L1 滚动摘要（本批之前的梗概；首轮为空串）。
    pub rolling_summary: &'a str,
    /// 活跃剧情线（C1 未决事项的全量只读投影，§8.5）：给 id 或「id（标题）」都行。
    pub active_threads: &'a [String],
    /// 角色的需要清单（§9.2「评价之源」；codex char 的 needs/values）。
    pub needs: &'a [String],
    /// 设定集实体清单（M3.10 关联审计的对照表）：「id（名）一句话」逐行。
    /// 空切片 = 世界没有设定集，审计段落整体省略（不逼模型对着空表编）。
    pub entity_catalog: &'a [String],
    /// 当轮激活记录（M3.10 关联审计的对照表）：最近一次组装实际激活的实体 id。
    /// 空切片 = 没有记录（手动触发总结时可能没有），提示词里写明。
    pub active_entities: &'a [String],
}

/// 批次里的一条消息（滑出 L0 窗口的那批；字段对齐 store::Message 的读侧子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchMessage {
    /// 轮次（与 store::Message.turn 同口径，从 1 起）。
    pub turn: u64,
    /// user | char | system。
    pub role: String,
    pub content: String,
}

impl BatchMessage {
    /// 从事件流投影出的消息转一条（宿主拼批次的最短路径）。
    ///
    /// OOC 导演指令（/ooc …）与 system 消息**不进入剧情记忆**（§4.1），由宿主在拼批次前
    /// 过滤——本模块不猜消息语义，role 原样保留在提示词里。
    pub fn from_message(m: &Message) -> BatchMessage {
        BatchMessage {
            turn: m.turn,
            role: m.role.clone(),
            content: m.content.clone(),
        }
    }
}

// ---------- 产物对象（宿主直接落事件 / 收件箱）----------

/// 一次总结的全部产物（§5.3 六类 + M3.10 关联审计）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SummaryOutcome {
    /// L1 滚动摘要增量（编年史体、第三人称；空串 = 本批无进展）。
    pub summary_delta: String,
    /// 世界层大事记增量（M3.2 · 设计 §10.4）：只记公开事件，跨场景可见；空串 = 没有。
    pub chronicle: String,
    /// 情景记忆草稿（入宫殿，§5.2）。
    pub episodes: Vec<EpisodeDraft>,
    /// 转述提案（M3.3 · §10.4）：本批剧情里 A 把某事告诉了 B——宿主为每个听众
    /// 各写一条 hearsay 记忆（salience 折半、links 继承），顺带揭示的秘密进知情集。
    pub hearsays: Vec<HearsayDraft>,
    /// L3 事实键值草稿（§5.1）。
    pub facts: Vec<FactDraft>,
    /// 剧情线提案（含提及时机起草，§8.3）。
    pub threads: Vec<ThreadDraft>,
    /// 心理评价提案（§9.2）。
    pub psyche: Vec<PsycheDraft>,
    /// 设定提案（进收件箱，§6.8）。
    pub codex: Vec<CodexDraft>,
    /// 关联审计发现（M3.10 · §6.13）：漏激活 / 缺失 facet / 该立的新事实。
    pub audit: Vec<AuditDraft>,
}

impl SummaryOutcome {
    /// 各类产物是否全空（宿主据此跳过落盘与事件；空产物算成功，不算失败）。
    pub fn is_empty(&self) -> bool {
        self.summary_delta.trim().is_empty()
            && self.chronicle.trim().is_empty()
            && self.episodes.is_empty()
            && self.hearsays.is_empty()
            && self.facts.is_empty()
            && self.threads.is_empty()
            && self.psyche.is_empty()
            && self.codex.is_empty()
            && self.audit.is_empty()
    }
}

/// 情景记忆草稿（§5.2 记忆对象的管线侧形态；id / story_day / story_clock / ts 由宿主补）。
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeDraft {
    /// 一句话正文（第三人称）。
    pub content: String,
    /// 显著度 0–1（情绪强度与稀有度决定初值，§5.2）。
    pub salience: f32,
    /// 情绪标签（渲染在括号里，§5.2）。
    pub emotion: Option<String>,
    /// 故事地点（房间 = 地点）。
    pub place: Option<String>,
    /// 参与者。
    pub actors: Vec<String>,
    /// 见证者（视角召回的过滤依据，§10.4）；空 = 默认 actors。
    pub witnesses: Vec<String>,
    /// 关联标签（topic: / person: / place:，§5.2）。
    pub links: Vec<String>,
    /// 所属剧情线 id（如 thread.周五还书）。
    pub thread: Option<String>,
    /// 这件事发生的轮次（升序；宿主取首个作 MemObject.turn 溯源）。
    pub turns: Vec<u64>,
}

/// 转述提案（M3.3 · 设计 §10.4「hearsay」）：本批剧情里 A 把某事告诉了 B。
///
/// id / story_day / story_clock / ts 由宿主补；**salience 语义与情景记忆不同**——
/// 它填的是**原事件（亲历）**的显著度，宿主写入时按 [`palace::HEARSAY_SALIENCE_FACTOR`]
/// 折半（听来的事不如亲历的刻骨）。每个听众各得一条自己的转述记忆（witnesses = 她自己），
/// 没在 listeners 里的人不知道（设计 §10.1：信息跨视角流动的唯一通道）。
#[derive(Debug, Clone, PartialEq)]
pub struct HearsayDraft {
    /// 转述内容：B 现在知道的那件事本身（一句话、第三人称、从 B 的视角）。
    pub content: String,
    /// 告知者（她是从谁那听来的；渲染成「转述自X」）。
    pub source: String,
    /// 听到的人（每人各得一条转述记忆；告知者本人不在此列——她的是亲历）。
    pub listeners: Vec<String>,
    /// **原事件（亲历）**的显著度 0–1；宿主写入时折半。
    pub salience: f32,
    /// 情绪标签（可选）。
    pub emotion: Option<String>,
    /// 事发地点（可选）。
    pub place: Option<String>,
    /// 关联标签：从原事件继承（topic: / person: / place:）。
    pub links: Vec<String>,
    /// 所属剧情线 id（可选）。
    pub thread: Option<String>,
    /// 告知发生的轮次（升序；宿主取首个作溯源与故事时刻锚点）。
    pub turns: Vec<u64>,
    /// 这番话顺带揭示的秘密路径（实体.secrets.秘密，可选）：宿主落 reveal 事件、
    /// 见证者 = listeners——听众的视角知情集因此增项，M3.1 的深卡判定随之闭环。
    pub reveals: Vec<String>,
}

/// L3 事实草稿（§5.1 键值：玩家的名字、生日、约定、关键事件）。
#[derive(Debug, Clone, PartialEq)]
pub struct FactDraft {
    pub key: String,
    pub value: serde_json::Value,
}

/// 剧情线提案（§8.2 / §8.3：起因落锚 + 提及时机起草；id 与 opened 戳由宿主补）。
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadDraft {
    pub title: String,
    /// 起因（为什么会有这条线）。
    pub cause: String,
    pub actors: Vec<String>,
    /// 重要度 0–1。
    pub importance: f32,
    /// 克制梯度：dormant | natural | eager（§8.4）。
    pub grade: String,
    /// 可提及窗口，**设计 §8.2 的 JSON 对象形态**（见 sanitize）。
    pub windows: Vec<serde_json::Value>,
    /// 期限（故事天）：到期未提 → 升格（§8.3）。
    pub deadline_day: Option<i64>,
    /// 被提及后 N 轮不再进窗口（§8.4「防反复横跳」）。
    pub cooldown: u32,
    /// 提起时的表演指引（§8.2）。
    pub framing: String,
}

impl ThreadDraft {
    /// 组装 §8.2 的 resurface 对象，宿主直接喂 threads::Resurface::from_value。
    ///
    /// grade 归一、cooldown 缺省 5（见 normalize_cooldown）、deadline 只出 {day, escalate}
    /// ——escalate 取 threads::DEFAULT_ESCALATE，即 §8.3 的「grade 升至 eager」。无期限时
    /// deadline 给 null（读侧按「没有期限」处理）。windows 原样数组（sanitize 已把不可解析的
    /// 窗口滤掉）。
    pub fn resurface_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("grade".into(), Value::String(normalize_grade(&self.grade)));
        map.insert("windows".into(), Value::Array(self.windows.clone()));
        map.insert(
            "deadline".into(),
            match self.deadline_day {
                Some(day) => {
                    let mut d = Map::new();
                    d.insert("day".into(), Value::from(day));
                    d.insert(
                        "escalate".into(),
                        Value::String(threads::DEFAULT_ESCALATE.to_string()),
                    );
                    Value::Object(d)
                }
                None => Value::Null,
            },
        );
        map.insert(
            "cooldown".into(),
            Value::from(normalize_cooldown(self.cooldown)),
        );
        map.insert("framing".into(), Value::String(self.framing.clone()));
        Value::Object(map)
    }
}

/// 心理评价提案（§9.2：需要满足 / 受挫 → 情绪与意图）。
#[derive(Debug, Clone, PartialEq)]
pub struct PsycheDraft {
    /// feel（情绪）或 intend（意图增减）；见 PSYCHE_*。
    pub kind: String,
    /// 情绪名 / 意图名。
    pub name: String,
    /// 情绪强度 0–1；意图增减 −1–1（负数 = 削弱，§9.2「受挫可削弱」）。
    pub intensity: f32,
    /// 来源：哪个需要被满足 / 受挫，或这次增减的原因。
    pub source: String,
}

/// 设定提案（§6.8 收件箱；未经确认不进注入，§6.9）。
#[derive(Debug, Clone, PartialEq)]
pub struct CodexDraft {
    /// new_entity | new_fact | fact_change | relation | transient；见 CODEX_*。
    /// transient（M3.8 · §6.8-2）= 瞬时状态，宿主直接写黑板、不入收件箱。
    pub kind: String,
    /// 目标：既有实体 id（如 char.小雨）或新实体的预分配 id。
    pub target: String,
    /// 提案内容（facet 值 / 新关系对象 / 新实体骨架）；宿主写入前先过
    /// codex::anchors_conflict（§6.8 anchors 最高保护级）。
    pub value: serde_json::Value,
    /// 出处说明（「第 14 轮即兴提到」），收件箱双源呈现用（§6.8）。
    pub reason: String,
}

/// 关联审计发现（M3.10 · §6.13）：对照实体清单与当轮激活记录复查本批消息后，
/// 报告检索层（trie/语义）可能漏掉的东西。三类见 AUDIT_*。
#[derive(Debug, Clone, PartialEq)]
pub struct AuditDraft {
    /// missed | facet | fact；见 AUDIT_*。
    pub finding: String,
    /// 涉及的实体 id（missed/facet）或提案目标（fact）。
    pub target: String,
    /// fact = 要立的 facts 路径；facet = 缺失的 facet 名；missed 不用。
    pub facet: String,
    /// fact = 要写的事实内容；其余不用。
    pub value: String,
    /// 引源：本批哪一轮哪句话让你起了疑心（收件箱可点验的依据）。
    pub evidence: String,
}

// ---------- 拼提示词（宿主 ① 的入口）----------

/// 角色简介（提示词第一段）：告诉模型它是谁、不许做什么。
const ROLE_BRIEF: &str =
    "你是《化境》的「自动总结管线」引擎（设计 §5.3）：一批消息滑出最近窗口后，\
由你把已经发生的剧情整理成长期记忆与提案。\
你不续写剧情、不扮演角色、不替角色做决定；只做归纳与抽取。\
宁可少写，不要编造——下面这批消息里没有的东西，一个字也不要补。";

/// 六类产物的逐条要求（§5.3 那张图的展开；build_prompt 的核心段落）。
const PRODUCT_SPEC: &str = r#"【必须逐条产出的产物】（设计 §5.3 + §10.4 摘要分卷；哪一类都没有就给空，但字段不要省）

1. summary_delta —— L1 滚动摘要增量（设计 §5.1，**本场景分卷**）
   把本批消息推进的剧情并入长期摘要：编年史体、第三人称，只写「发生了什么、结果如何」，
   2–6 句。不抄台词、不写文采、不揣测内心；起因与结果比过程重要。本批没有值得记的进展就给空串 ""。
   这一卷只有本场景的角色会读到，可以放心写本场景私下发生的事。

2. chronicle —— 世界层大事记（设计 §10.4，跨场景共享）
   只记**公开发生**、别的场景也该知道的大事（某人离开了小镇、店铺倒闭、世界级事件），
   1–2 句；私下对话、只有本场景角色知道的细节绝对不写。没有就给空串 ""。

3. episodes —— 情景记忆（设计 §5.2，入记忆宫殿）
   一条 = 角色亲身经历的一件值得记住的事。只记值得记住的事，宁少勿滥：日常寒暄、重复的
   状态描写不要记；承诺、冲突、秘密、转折、亲密、失去才值得记。最多 6 条，都不值得就给 []。
   每条字段：
   - content：一句话（第三人称：谁在哪里做了什么、结果如何）；
   - salience：0–1 显著度（由情绪强度与稀有度决定：平淡 0.2–0.4，承诺/转折/亲密 0.7 以上）；
   - emotion：当时的情绪词（温暖 / 忐忑 / 失落），没有就省略；
   - place：发生地点（房间 = 地点）；
   - actors：参与者（写名字，如 小雨 / 玩家）；
   - witnesses：知道这件事的人（默认 = actors）。只讲给某人听的事不要把不在场的人写进来
     ——视角召回按它过滤（设计 §10.4），写错会让角色「记得」她不该知道的事；
   - links：关联标签，用命名空间写法 topic:<话题> / person:<人名> / place:<地点>；
   - thread：属于哪条剧情线（填线 id，如 thread.周五还书），不属于就省略；
   - turns：这件事发生的轮次（本批消息的 turn，升序）。

4. hearsays —— 转述提案（设计 §10.4「转述是信息跨视角流动的唯一通道」）
   本批剧情里**有人把某件事讲给了别人听**（转告、坦白、透露、道听途说）时记录：谁讲的、
   听的人现在知道了什么。最多 6 条，没有就给 []。每条字段：
   - content：听的人现在知道的那件事本身（一句话、第三人称、从听者的视角，
     如「小雨告诉玩家，图书馆要拆了」）；
   - source：告知者（讲的人）；
   - listeners：听到的人（写名字；讲的人自己不算——她的记忆是亲历，进 episodes）；
     只讲给某一个人听的事，绝不要把别人写进来——没听到的人不会知道这件事；
   - salience：**原事件（被讲述的那件事）**的显著度 0–1；宿主写入时会自动折半
     （听来的不如亲历的刻骨），这里不要自己折；
   - emotion / place / links / thread：可选，语义同 episodes（links 从原事件继承）；
   - turns：这番话发生的轮次（升序）；
   - reveals：可选。这番话顺带**揭示了设定集里的秘密**时，填秘密路径
     （如 char.小雨.secrets.工作牌）——听的人从此算「知情」，深卡才会对她展开；
     没有就给 []。

5. facts —— L3 事实键值（设计 §5.1）
   跨会话仍然要记得的稳定事实：玩家叫什么、生日、约定、关键事件、稳定偏好。
   key 用简短中文或点分路径（如 玩家名字 / 约定.还书），value 是 JSON 标量或短数组。
   一次性的、会变的、拿不准的不要写；没有就给 []。

6. threads —— 剧情线提案（设计 §8.3「管线提案」，必须一并起草提及时机）
   只有当剧情里出现了新的承诺 / 冲突 / 悬念（有起因、将来要了结的事）才提，最多 2 条。
   上文已列出的活跃线不要重复开；没有就给 []。每条字段：
   - title / cause（起因，一句话讲清为什么欠着）/ actors / importance（0–1 重要度）；
   - grade —— 克制梯度（设计 §8.4）：dormant 深埋（只有玩家明确提起才相关）|
     natural 默认（时机到了才可能被提起）| eager 角色很想找机会说。拿不准就 natural，
     不要动不动 eager——每轮都强调悬置的线会让角色变成讨债的任务 NPC；
   - windows —— 可提及窗口（设计 §8.2 的 JSON 形态，满足任一即进入窗口）。只允许这几种写法：
       {"blackboard": {"day": 5}}                     黑板键值（时间 / 地点 / 任意黑板键）
       {"mention": ["还书", "借书卡"]}                 话题擦边（1–3 个词）
       {"actors_with": ["小雨"]}                       指定的人在场
       {"state_path": ["图书馆"]}                      走到某个剧情阶段
     也可以把黑板与在场合成一个对象：
       {"blackboard": {"place": "图书馆"}, "actors_with": ["小雨"]}
     窗口要克制：写的是「什么时候才该被想起来」，不是「什么时候都可以提」；宁窄勿宽。
     确实想不出时机就给 []（那条线就只会在玩家主动提起时由未决事项清单兜底）。
   - deadline_day：期限（故事天，整数）。到那天还没被提起，宿主会自动升格这条线；没有期限就省略；
   - cooldown：被提及后多少轮内不再进窗口（缺省 5，防止同一条线轮轮霸屏）；
   - framing：提起时的表演指引一句话（她打算怎么开这个口、被问到会怎样），例如
     「她在意但不好意思催；若对方主动提起，会松一口气」。没有特别指引给 ""。

7. psyche —— 心理评价提案（设计 §9.2：情绪是对「需要是否被满足」的态度体验）
   对照上文的「需要（needs）」清单评价本批消息——某个需要被满足或受挫时：
   - 情绪：{"kind": "feel", "name": "<情绪名>", "intensity": <0–1>, "source": "<哪个需要被满足/受挫>"}；
   - 意图增减：{"kind": "intend", "name": "<意图名>", "intensity": <-1–1>, "source": "<原因>"}
     （正数增强、负数削弱；例如 -0.3 表示「想解释」的冲动被削掉三成）。
   没有明显评价就给 []；不要为了凑数造情绪，也不要写角色的台词倾向（那是生成时的职责）。

8. codex —— 设定提案（设计 §6.8，进设定收件箱；未经确认不进注入，§6.9）
   本批消息里即兴发明且值得留下的世界事实（例如「她养了一只叫墨墨的猫」）。kind 五选一：
   - "new_entity"  全新实体（必须人工确认）：{"kind":"new_entity","target":"char.墨墨",
       "value":{"type":"char","name":"墨墨","facts":{"look.impression":"一只黑猫"}},
       "reason":"第 14 轮即兴提到"}；
   - "new_fact"    给既有实体加一条事实：target = 实体 id，
       value = {"facet":"<facts 路径，如 schedule>","value":"<要写入的内容>"}；
   - "fact_change" 改写既有事实：value 同 new_fact 的形态，reason 说明出自第几轮；
   - "relation"    新关系：value 形如 {"to":"char.小雨","kind":"宠物","always_with":false}；
   - "transient"   瞬时状态（M3.8 分级：「现在下雨了」这类转瞬即逝、不构成长期设定）：
       target 给 ""，value = {"key":"<黑板键，如 天气>","value":"雨"}——宿主直接写黑板。
   判断标准：一条事实「明天还成立吗？」——成立才写 new_fact / new_entity，不成立就 transient。
   绝不允许改动任何实体的恒定辨识点 anchors（设计 §6.8：anchors 是最高保护级，与之冲突的提案
   会被直接驳回）；只是气氛描写、拿不准的，不要写。最多 6 条，没有就给 []。

9. audit —— 关联审计（设计 §6.13：复查检索层有没有漏掉本该在场的世界知识）
   对照上文的「设定集实体清单」与「当轮激活记录」重读本批消息：剧情明明涉及某个实体、
   但它不在激活记录里（代词指称、描述性指称、转述都可能漏），或设定里缺了剧情正在用的
   信息时报告。最多 6 条，没有疑点就给 []；宁可少报，不要对着清单硬凑。每条字段：
   - finding：三选一——
       "missed"  疑似被涉及但未被激活的实体：target = 实体 id；
       "facet"   既有实体缺了剧情正在用的 facet：target = 实体 id，facet = 缺的 facet 名；
       "fact"    该立的新事实：target = 实体 id，facet = facts 路径，value = 要写的内容
                 （与 codex 的 new_fact 同一判断标准：「明天还成立吗？」）；
   - evidence：引源，写清本批哪一轮的哪句话让你起疑（如「第 15 轮『她今晚不在』用代词，
     结合上下文指 char.小雨」）——没有引源的疑心不要报。"#;

/// 输出格式要求：只输出一个 JSON 对象 + 骨架（骨架由 build_prompt 拼在最后）。
const OUTPUT_SPEC: &str = "【输出格式】\
只输出一个 JSON 对象：不要 Markdown 代码围栏、不要任何解释、不要在对象前后写别的字。\
字段名照抄下面的骨架；缺的数组给 []，缺的字符串给 \"\"，缺失字段按空处理。骨架：";

/// 空批次的说明（§5.3：批次为空时不该编内容出来）。
const EMPTY_BATCH_NOTE: &str =
    "（本批没有消息。summary_delta 与 chronicle 给空串，episodes / facts / threads / psyche / codex / audit 全给 []。）";

/// 拼一次总结调用的提示词（宿主 ①：批次与上下文进，提示词出）。
///
/// 结构（顺序固定，因而同输入同输出）：
///   角色简介 → 【当前上下文】→【本批消息】→【必须逐条产出的六类产物】→【输出格式 + 骨架】。
/// 空批次也照常拼（明确写「本批没有消息」），绝不产出「让模型自己编」的提示词。
/// 上下文里空着的位置写「（未给）」占位，免得模型把「没给」当成「没有」。
pub fn build_prompt(ctx: &SummaryContext<'_>, batch: &[BatchMessage]) -> String {
    let mut out = String::with_capacity(4096);

    out.push_str(ROLE_BRIEF);
    out.push_str("\n\n");

    // ---- 当前上下文 ----
    out.push_str("【当前上下文】\n");
    push_line(&mut out, "角色卡", ctx.card_name);
    if let Some(p) = ctx.persona_name {
        push_line(&mut out, "玩家角色", p);
    }
    if let Some(p) = ctx.premise {
        push_line(&mut out, "起因（premise）", p);
    }
    if let Some(sc) = ctx.scene_label {
        push_line(&mut out, "本批所属场景（分卷）", sc);
    }
    push_line(&mut out, "故事时钟", ctx.story_clock);
    let summary = ctx.rolling_summary.trim();
    if summary.is_empty() {
        out.push_str("已有滚动摘要（L1）：（还没有，这是开头）\n");
    } else {
        out.push_str("已有滚动摘要（L1，本批之前的梗概）：\n");
        out.push_str(summary);
        out.push('\n');
    }
    push_bullets(
        &mut out,
        "活跃剧情线（C1 未决事项，全量；已在此列出的线不要重复开）",
        ctx.active_threads,
    );
    push_join(
        &mut out,
        "角色的需要（needs，第 7 条评价的对照表）",
        ctx.needs,
    );
    // 关联审计的对照表（M3.10 · §6.13）：实体清单 + 当轮激活记录。
    // 没有设定集或没有激活记录时明说——不逼模型对着空表编疑点。
    if ctx.entity_catalog.is_empty() {
        out.push_str("设定集实体清单：（本世界没有设定集实体——audit 一律给 []）\n");
    } else {
        out.push_str("设定集实体清单（第 9 条审计的对照表）：\n");
        for line in ctx.entity_catalog {
            if !line.trim().is_empty() {
                out.push_str("- ");
                out.push_str(line.trim());
                out.push('\n');
            }
        }
    }
    if ctx.active_entities.is_empty() {
        out.push_str("当轮激活记录：（无记录——audit 只报有引源的 fact，missed 不要猜）\n");
    } else {
        out.push_str("当轮激活记录（最近一次组装实际激活的实体，第 9 条审计据此找漏网）：");
        out.push_str(&ctx.active_entities.join("、"));
        out.push('\n');
    }

    // ---- 本批消息 ----
    out.push('\n');
    if batch.is_empty() {
        out.push_str("【本批消息】（空批次：没有消息滑出最近窗口）\n");
        out.push_str(EMPTY_BATCH_NOTE);
        out.push('\n');
    } else {
        let first = batch[0].turn;
        let last = batch[batch.len() - 1].turn;
        out.push_str(&format!(
            "【本批消息】（共 {} 条，turn {}–{}）\n",
            batch.len(),
            first,
            last
        ));
        for m in batch {
            out.push_str(&format!("[turn {} · {}] ", m.turn, m.role.trim()));
            out.push_str(m.content.trim());
            out.push('\n');
        }
    }

    // ---- 六类产物 + 输出格式 ----
    out.push('\n');
    out.push_str(PRODUCT_SPEC);
    out.push_str("\n\n");
    out.push_str(OUTPUT_SPEC);
    out.push('\n');
    out.push_str(OUTCOME_SCHEMA_HINT);
    out.push_str("\n\n再次强调：整个回复就是这一个 JSON 对象本身，不要围栏、不要旁白。");
    out
}

/// 一行「标签：值」（值空则写「（未给）」占位）。
fn push_line(out: &mut String, label: &str, value: &str) {
    let v = value.trim();
    out.push_str(label);
    out.push('：');
    out.push_str(if v.is_empty() { "（未给）" } else { v });
    out.push('\n');
}

/// 一行「标签：a、b、c」（全空则整行省略）。
fn push_join(out: &mut String, label: &str, items: &[String]) {
    let cleaned: Vec<&str> = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if cleaned.is_empty() {
        return;
    }
    out.push_str(label);
    out.push('：');
    out.push_str(&cleaned.join("、"));
    out.push('\n');
}

/// 「标签：」+ 逐条 `- ` 列表（全空则整行省略）。
fn push_bullets(out: &mut String, label: &str, items: &[String]) {
    let cleaned: Vec<&str> = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if cleaned.is_empty() {
        return;
    }
    out.push_str(label);
    out.push_str("：\n");
    for item in cleaned {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
}

// ---------- 解析模型回复（宿主 ③ 的第一道关）----------

/// 解析一次总结的模型回复。
///
/// 容错口径（对应 m2.md M2.6 的「LLM 产物不稳定」风险）：
/// - **围栏与杂文**：先取回复里第一个 `{` 到最后一个 `}` 之间的片段——Markdown 代码围栏、
///   「好的，结果如下：」这类前后缀都不影响解析；
/// - **缺字段**：给空（字符串 ""、数组 []、数值取缺省，见各 *Draft 的字段说明）；
/// - **类型不符**：按缺处理。数值不认字符串（`"salience": "0.8"` 当没给，避免脏串被当权威），
///   数组位置放对象/字符串也当没给；单条产物不是对象就跳过；
/// - **解析失败**：返回 Err 且信息可读（带出错片段），宿主据此重试或丢弃；
/// - **绝不 panic**：任何输入（空串、半个 JSON、全角括号、超长噪声）都只会走进 Ok/Err。
///
/// 解析只管「形状」，归一（夹紧、去重、限额）交给 [`sanitize`]——两步分开，方便宿主先看
/// 原始解析结果再决定是否清洗，也方便单测各自钉死。
pub fn parse_outcome(raw: &str) -> Result<SummaryOutcome, String> {
    let Some(text) = extract_json_object(raw) else {
        return Err(format!(
            "总结结果里找不到 JSON 对象（共 {} 字，没有成对的 {{ 与 }}；片段：{}）",
            raw.chars().count(),
            excerpt(raw)
        ));
    };
    let value: Value = serde_json::from_str(text).map_err(|e| {
        format!(
            "总结结果不是合法 JSON：{e}（已截取第 1 个 {{ 到最后一个 }}，共 {} 字；片段：{}）",
            text.chars().count(),
            excerpt(text)
        )
    })?;
    let Some(map) = value.as_object() else {
        return Err(format!(
            "总结结果的最外层不是 JSON 对象（片段：{}）",
            excerpt(text)
        ));
    };

    Ok(SummaryOutcome {
        summary_delta: text_of(map.get("summary_delta")),
        chronicle: text_of(map.get("chronicle")),
        episodes: objects_of(map.get("episodes"))
            .iter()
            .map(|m| episode_of(m))
            .collect(),
        hearsays: objects_of(map.get("hearsays"))
            .iter()
            .map(|m| hearsay_of(m))
            .collect(),
        facts: objects_of(map.get("facts"))
            .iter()
            .map(|m| fact_of(m))
            .collect(),
        threads: objects_of(map.get("threads"))
            .iter()
            .map(|m| thread_of(m))
            .collect(),
        psyche: objects_of(map.get("psyche"))
            .iter()
            .map(|m| psyche_of(m))
            .collect(),
        codex: objects_of(map.get("codex"))
            .iter()
            .map(|m| codex_of(m))
            .collect(),
        audit: objects_of(map.get("audit"))
            .iter()
            .map(|m| audit_of(m))
            .collect(),
    })
}

/// 取回复里第一个 `{` 到最后一个 `}` 的片段（剥围栏与前后杂文）。
///
/// 全角括号、空串、只有一个括号的情况都返回 None（交给调用方报错）。
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

/// 错误信息里的片段：最多 80 字，按字符切（不切坏 UTF-8）。
fn excerpt(text: &str) -> String {
    let mut out: String = text.trim().chars().take(80).collect();
    if text.trim().chars().count() > 80 {
        out.push('…');
    }
    out
}

// ---------- 单条产物：形状解析（缺字段给空，类型不符按缺）----------

/// 一条情景记忆草稿（缺 salience 取 palace::DEFAULT_SALIENCE；turns 收数字，浮点整值也认）。
fn episode_of(map: &Map<String, Value>) -> EpisodeDraft {
    EpisodeDraft {
        content: text_of(map.get("content")),
        salience: f32_of(map.get("salience"), palace::DEFAULT_SALIENCE),
        emotion: opt_text_of(map.get("emotion")),
        place: opt_text_of(map.get("place")),
        actors: list_of_text(map.get("actors")),
        witnesses: list_of_text(map.get("witnesses")),
        links: list_of_text(map.get("links")),
        thread: opt_text_of(map.get("thread")),
        turns: turns_of(map.get("turns")),
    }
}

/// 一条转述提案（salience 缺省取 palace::DEFAULT_SALIENCE；reveals 只收非空文本）。
fn hearsay_of(map: &Map<String, Value>) -> HearsayDraft {
    HearsayDraft {
        content: text_of(map.get("content")),
        source: text_of(map.get("source")),
        listeners: list_of_text(map.get("listeners")),
        salience: f32_of(map.get("salience"), palace::DEFAULT_SALIENCE),
        emotion: opt_text_of(map.get("emotion")),
        place: opt_text_of(map.get("place")),
        links: list_of_text(map.get("links")),
        thread: opt_text_of(map.get("thread")),
        turns: turns_of(map.get("turns")),
        reveals: list_of_text(map.get("reveals")),
    }
}

/// 一条 L3 事实草稿（value 缺失给 null，字符串值 trim）。
fn fact_of(map: &Map<String, Value>) -> FactDraft {
    FactDraft {
        key: text_of(map.get("key")),
        value: map
            .get("value")
            .cloned()
            .map(trim_value)
            .unwrap_or(Value::Null),
    }
}

/// 一条剧情线提案（缺 importance 取 threads::DEFAULT_IMPORTANCE；windows 只收对象）。
fn thread_of(map: &Map<String, Value>) -> ThreadDraft {
    ThreadDraft {
        title: text_of(map.get("title")),
        cause: text_of(map.get("cause")),
        actors: list_of_text(map.get("actors")),
        importance: f32_of(map.get("importance"), threads::DEFAULT_IMPORTANCE),
        grade: text_of(map.get("grade")),
        windows: windows_of(map.get("windows")),
        deadline_day: map.get("deadline_day").and_then(int_of),
        cooldown: map.get("cooldown").and_then(u32_of).unwrap_or(0),
        framing: text_of(map.get("framing")),
    }
}

/// 一条心理评价提案（缺 intensity 按 kind 取 psyche 侧的缺省）。
fn psyche_of(map: &Map<String, Value>) -> PsycheDraft {
    let kind = normalize_psyche_kind(&text_of(map.get("kind")));
    let fallback = if kind == PSYCHE_INTEND {
        psyche::DEFAULT_INTENT_STRENGTH
    } else {
        psyche::DEFAULT_AFFECT_INTENSITY
    };
    PsycheDraft {
        kind,
        name: text_of(map.get("name")),
        intensity: f32_of(map.get("intensity"), fallback),
        source: text_of(map.get("source")),
    }
}

/// 一条设定提案（value 缺失给 null）。
fn codex_of(map: &Map<String, Value>) -> CodexDraft {
    CodexDraft {
        kind: normalize_codex_kind(&text_of(map.get("kind"))),
        target: text_of(map.get("target")),
        value: map
            .get("value")
            .cloned()
            .map(trim_value)
            .unwrap_or(Value::Null),
        reason: text_of(map.get("reason")),
    }
}

/// 一条关联审计发现（finding 归一在 sanitize：未知值在那里丢弃）。
fn audit_of(map: &Map<String, Value>) -> AuditDraft {
    AuditDraft {
        finding: text_of(map.get("finding")),
        target: text_of(map.get("target")),
        facet: text_of(map.get("facet")),
        value: text_of(map.get("value")),
        evidence: text_of(map.get("evidence")),
    }
}

// ---------- 取值小工具（类型不符一律按缺）----------

/// 数组取值：不是数组（缺字段 / 类型不符）给空切片。
fn array_of(v: Option<&Value>) -> &[Value] {
    match v.and_then(Value::as_array) {
        Some(items) => items.as_slice(),
        None => &[],
    }
}

/// 数组里的对象（字符串 / 数字 / null 元素跳过）。
fn objects_of(v: Option<&Value>) -> Vec<&Map<String, Value>> {
    array_of(v).iter().filter_map(Value::as_object).collect()
}

/// 文本取值：非字符串按空串；顺带 trim。
fn text_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// 可选文本：空串 / 空白 / 类型不符都给 None。
fn opt_text_of(v: Option<&Value>) -> Option<String> {
    let t = text_of(v);
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 数值取值：非数字（含数字字符串）按缺省；超范围由 sanitize 夹紧。
fn f32_of(v: Option<&Value>, fallback: f32) -> f32 {
    match v.and_then(Value::as_f64) {
        Some(n) => n as f32,
        None => fallback,
    }
}

/// 字符串数组：非字符串元素跳过，空白项丢弃。
fn list_of_text(v: Option<&Value>) -> Vec<String> {
    array_of(v)
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 轮次数组：只收数字（浮点整值如 14.0 也认，负数 / 非数字跳过）。
fn turns_of(v: Option<&Value>) -> Vec<u64> {
    array_of(v).iter().filter_map(turn_of).collect()
}

/// 单个轮次。
fn turn_of(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let n = v.as_f64()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n as u64)
}

/// 整数（deadline_day）：整数与浮点整值都收。
fn int_of(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return Some(n.min(i64::MAX as u64) as i64);
    }
    let n = v.as_f64()?;
    if !n.is_finite() {
        return None;
    }
    Some(n as i64)
}

/// 无符号数（cooldown）：负数 / 非数字给 None，超大值封顶。
fn u32_of(v: &Value) -> Option<u32> {
    if let Some(n) = v.as_u64() {
        return Some(n.min(u32::MAX as u64) as u32);
    }
    let n = v.as_f64()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n.min(u32::MAX as f64) as u32)
}

/// 可提及窗口：设计 §8.2 的**对象**形态才收（数组元素非对象丢弃；单个对象也接受，
/// 与 threads::Resurface::from_value 的读侧宽容度一致）。
fn windows_of(v: Option<&Value>) -> Vec<Value> {
    let Some(v) = v else {
        return Vec::new();
    };
    match v {
        Value::Object(_) => vec![v.clone()],
        Value::Array(items) => items.iter().filter(|w| w.is_object()).cloned().collect(),
        _ => Vec::new(),
    }
}

/// 字符串值 trim（只动顶层：对象 / 数组里的内容原样，免得改坏结构化数据）。
fn trim_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(s.trim().to_string()),
        other => other,
    }
}

// ---------- 归一与夹紧（宿主 ③ 的第二道关）----------

/// 清洗一次解析结果：丢垃圾、夹紧、去重、限额。**幂等**（sanitize(sanitize(x)) == sanitize(x)）。
///
/// 口径（逐条可测）：
/// - 文本字段一律 trim；content / key / title / target / name 为空的条目**丢弃**（空记忆、
///   空键、无标题的线、无目标的提案都是噪声）；
/// - salience / importance 夹到 0–1（NaN / ∞ → 0.5，与 palace / threads 的缺省同值）；
/// - psyche.intensity 夹到 −1–1（意图增减可为负，§9.2「受挫可削弱」；NaN → 0.5）；
/// - actors / witnesses / links 去重（trim + 拉丁大小写不敏感，保留首次出现的写法），
///   turns 去重并升序；
/// - grade 归一为 dormant | natural | eager，未知 / 空 → natural（§8.4 的默认档）；
/// - cooldown 为 0 视为没给 → threads::DEFAULT_COOLDOWN（§8.2 示例值 5；「被提及后立刻可再进
///   窗口」与 §8.4「防反复横跳」相悖，管线不产出无冷却的线）；
/// - windows 只保留 threads::ResurfaceWindow::from_value 认得的对象（宿主能直接喂进去），
///   并按 JSON 文本去重；
/// - episodes / hearsays / threads / codex / audit 截断到 MAX_*（保留顺序，先到先得）；
/// - facts / psyche 不设上限——它们没有「一条顶十条」的破坏力，且都要过收件箱分级与情绪
///   槽位互斥；未识别的 kind 归一为最普通的形态（psyche → feel、codex → new_fact）。
pub fn sanitize(outcome: SummaryOutcome) -> SummaryOutcome {
    let mut episodes: Vec<EpisodeDraft> = outcome
        .episodes
        .into_iter()
        .filter_map(normalize_episode)
        .collect();
    episodes.truncate(MAX_EPISODES);

    let mut hearsays: Vec<HearsayDraft> = outcome
        .hearsays
        .into_iter()
        .filter_map(normalize_hearsay)
        .collect();
    hearsays.truncate(MAX_HEARSAYS);

    let facts: Vec<FactDraft> = outcome
        .facts
        .into_iter()
        .filter_map(normalize_fact)
        .collect();

    let mut threads: Vec<ThreadDraft> = outcome
        .threads
        .into_iter()
        .filter_map(normalize_thread)
        .collect();
    threads.truncate(MAX_THREADS);

    let psyche: Vec<PsycheDraft> = outcome
        .psyche
        .into_iter()
        .filter_map(normalize_psyche)
        .collect();

    let mut codex: Vec<CodexDraft> = outcome
        .codex
        .into_iter()
        .filter_map(normalize_codex)
        .collect();
    codex.truncate(MAX_CODEX_DRAFTS);

    let mut audit: Vec<AuditDraft> = outcome
        .audit
        .into_iter()
        .filter_map(normalize_audit)
        .collect();
    audit.truncate(MAX_AUDIT);

    SummaryOutcome {
        summary_delta: outcome.summary_delta.trim().to_string(),
        chronicle: outcome.chronicle.trim().to_string(),
        episodes,
        hearsays,
        facts,
        threads,
        psyche,
        codex,
        audit,
    }
}

/// 一条情景记忆（空正文丢弃）。
fn normalize_episode(e: EpisodeDraft) -> Option<EpisodeDraft> {
    let content = e.content.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(EpisodeDraft {
        content,
        salience: clamp_unit(e.salience, palace::DEFAULT_SALIENCE),
        emotion: clean_opt(e.emotion),
        place: clean_opt(e.place),
        actors: dedup_words(e.actors),
        witnesses: dedup_words(e.witnesses),
        links: dedup_words(e.links),
        thread: clean_opt(e.thread),
        turns: sorted_turns(e.turns),
    })
}

/// 一条转述提案（正文或告知者为空丢弃；没有听众的转述没人听见，同样丢弃）。
///
/// 听众里的告知者本人剔除（她的是亲历，不是转述）；salience 是**原事件**的显著度，
/// 这里只夹紧不折半——折半是宿主写入侧的规则（见 palace::HEARSAY_SALIENCE_FACTOR）。
fn normalize_hearsay(h: HearsayDraft) -> Option<HearsayDraft> {
    let content = h.content.trim().to_string();
    let source = h.source.trim().to_string();
    if content.is_empty() || source.is_empty() {
        return None;
    }
    let source_key = source.to_ascii_lowercase();
    let listeners: Vec<String> = dedup_words(h.listeners)
        .into_iter()
        .filter(|l| l.to_ascii_lowercase() != source_key)
        .collect();
    if listeners.is_empty() {
        return None;
    }
    Some(HearsayDraft {
        content,
        source,
        listeners,
        salience: clamp_unit(h.salience, palace::DEFAULT_SALIENCE),
        emotion: clean_opt(h.emotion),
        place: clean_opt(h.place),
        links: dedup_words(h.links),
        thread: clean_opt(h.thread),
        turns: sorted_turns(h.turns),
        reveals: dedup_words(h.reveals),
    })
}

/// 一条事实（空 key 丢弃）。
fn normalize_fact(f: FactDraft) -> Option<FactDraft> {
    let key = f.key.trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some(FactDraft {
        key,
        value: trim_value(f.value),
    })
}

/// 一条剧情线提案（空标题丢弃；windows 过一遍可解析性）。
fn normalize_thread(t: ThreadDraft) -> Option<ThreadDraft> {
    let title = t.title.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some(ThreadDraft {
        title,
        cause: t.cause.trim().to_string(),
        actors: dedup_words(t.actors),
        importance: clamp_unit(t.importance, threads::DEFAULT_IMPORTANCE),
        grade: normalize_grade(&t.grade),
        windows: clean_windows(t.windows),
        deadline_day: t.deadline_day,
        cooldown: normalize_cooldown(t.cooldown),
        framing: t.framing.trim().to_string(),
    })
}

/// 一条心理评价（空名字丢弃；kind 归一）。
fn normalize_psyche(p: PsycheDraft) -> Option<PsycheDraft> {
    let name = p.name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let kind = normalize_psyche_kind(&p.kind);
    let fallback = if kind == PSYCHE_INTEND {
        psyche::DEFAULT_INTENT_STRENGTH
    } else {
        psyche::DEFAULT_AFFECT_INTENSITY
    };
    Some(PsycheDraft {
        kind,
        name,
        intensity: clamp_signed(p.intensity, fallback),
        source: p.source.trim().to_string(),
    })
}

/// 一条设定提案（空 target 丢弃；kind 归一）。
fn normalize_codex(c: CodexDraft) -> Option<CodexDraft> {
    let target = c.target.trim().to_string();
    // 瞬时状态没有实体目标（「现在下雨了」是世界的事）——只有它允许空 target
    if target.is_empty() && normalize_codex_kind(&c.kind) != CODEX_TRANSIENT {
        return None;
    }
    Some(CodexDraft {
        kind: normalize_codex_kind(&c.kind),
        target,
        value: trim_value(c.value),
        reason: c.reason.trim().to_string(),
    })
}

/// 一条关联审计发现（空 target / 缺引源的丢弃；未知 finding 丢弃——审计是
/// 提示性产物，不猜模型想说什么）。missed 不需要 facet/value，facet 需要名字，
/// fact 三者（facet/value/target）都要。
fn normalize_audit(a: AuditDraft) -> Option<AuditDraft> {
    let finding = match a.finding.trim().to_ascii_lowercase().as_str() {
        AUDIT_MISSED => AUDIT_MISSED.to_string(),
        AUDIT_FACET => AUDIT_FACET.to_string(),
        AUDIT_FACT => AUDIT_FACT.to_string(),
        _ => return None,
    };
    let target = a.target.trim().to_string();
    let evidence = a.evidence.trim().to_string();
    if target.is_empty() || evidence.is_empty() {
        return None;
    }
    let facet = a.facet.trim().to_string();
    let value = a.value.trim().to_string();
    if finding == AUDIT_FACET && facet.is_empty() {
        return None;
    }
    if finding == AUDIT_FACT && (facet.is_empty() || value.is_empty()) {
        return None;
    }
    Some(AuditDraft {
        finding,
        target,
        facet,
        value,
        evidence,
    })
}

// ---------- 归一细则：夹紧 / 去重 / 归一（全模块一个口径）----------

/// 夹到 0–1（NaN / ±∞ → fallback）。
fn clamp_unit(v: f32, fallback: f32) -> f32 {
    if !v.is_finite() {
        return fallback;
    }
    v.clamp(0.0, 1.0)
}

/// 夹到 −1–1（意图增减可为负，§9.2）。
fn clamp_signed(v: f32, fallback: f32) -> f32 {
    if !v.is_finite() {
        return fallback;
    }
    v.clamp(-1.0, 1.0)
}

/// 可选文本归一：trim 后为空给 None。
fn clean_opt(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 词表去重：trim、丢空、比较用拉丁大小写不敏感（与 threads 的 normalize_word 同口径，
/// CJK 因此是精确比较），保留首次出现的写法。
fn dedup_words(items: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for raw in items {
        let word = raw.trim();
        if word.is_empty() {
            continue;
        }
        let key = word.to_ascii_lowercase();
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push(word.to_string());
    }
    out
}

/// 轮次去重并升序（宿主取首个做 MemObject.turn 的溯源锚点）。
fn sorted_turns(mut turns: Vec<u64>) -> Vec<u64> {
    turns.sort_unstable();
    turns.dedup();
    turns
}

/// 克制梯度归一（§8.4）：dormant | natural | eager；未知 / 空 → natural。
fn normalize_grade(raw: &str) -> String {
    let g = raw.trim().to_ascii_lowercase();
    if g == threads::GRADE_DORMANT {
        return threads::GRADE_DORMANT.to_string();
    }
    if g == threads::GRADE_EAGER {
        return threads::GRADE_EAGER.to_string();
    }
    threads::GRADE_NATURAL.to_string()
}

/// cooldown 归一：0 视为没给 → threads::DEFAULT_COOLDOWN（见 sanitize 的口径说明）。
fn normalize_cooldown(c: u32) -> u32 {
    if c == 0 {
        threads::DEFAULT_COOLDOWN
    } else {
        c
    }
}

/// 心理评价类型归一：只认 feel / intend；未知 / 空 → feel（情绪是瞬时的，比猜「她想要什么」保守）。
fn normalize_psyche_kind(raw: &str) -> String {
    if raw.trim().eq_ignore_ascii_case(PSYCHE_INTEND) {
        PSYCHE_INTEND.to_string()
    } else {
        PSYCHE_FEEL.to_string()
    }
}

/// 设定提案类型归一：只认 §6.8 的四种 + transient（M3.8 分级）；未知 / 空 → new_fact（最普通的形态，仍要过收件箱分级）。
fn normalize_codex_kind(raw: &str) -> String {
    let kind = raw.trim().to_ascii_lowercase();
    for known in [
        CODEX_NEW_ENTITY,
        CODEX_NEW_FACT,
        CODEX_FACT_CHANGE,
        CODEX_RELATION,
        CODEX_TRANSIENT,
    ] {
        if kind == known {
            return known.to_string();
        }
    }
    CODEX_NEW_FACT.to_string()
}

/// 窗口清洗：必须是**对象**，且必须能被 threads::ResurfaceWindow::from_value 认出
/// （认不出的窗口宿主也没法用，留着只会让「可提及窗口」变成一句空话）；按 JSON 文本去重
/// （serde_json 默认 Map 是 BTreeMap，键序稳定 → 判重确定）。
fn clean_windows(windows: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for w in windows {
        if !w.is_object() || threads::ResurfaceWindow::from_value(&w).is_none() {
            continue;
        }
        let key = w.to_string();
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push(w);
    }
    out
}

// ---------- 单测（覆盖 m2.md M2.6 的引擎侧验收口径）----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads;
    use serde_json::json;

    // ---------- 脚手架 ----------

    /// 浮点近似相等（LLM 来的数值走 f64 → f32，别用 == 较真）。
    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-6, "期望 {b}，实际 {a}");
    }

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn ctx_of<'a>(
        card: &'a str,
        persona: Option<&'a str>,
        premise: Option<&'a str>,
        clock: &'a str,
        summary: &'a str,
        active_threads: &'a [String],
        needs: &'a [String],
    ) -> SummaryContext<'a> {
        SummaryContext {
            card_name: card,
            persona_name: persona,
            premise,
            scene_label: None,
            story_clock: clock,
            rolling_summary: summary,
            active_threads,
            needs,
            entity_catalog: &[],
            active_entities: &[],
        }
    }

    fn prompt_with(threads_list: &[String], needs: &[String], batch: &[BatchMessage]) -> String {
        build_prompt(
            &ctx_of(
                "小雨",
                Some("阿澈"),
                Some("雨夜躲进图书馆"),
                "第3天 23:40",
                "前两天两人在图书馆认识。",
                threads_list,
                needs,
            ),
            batch,
        )
    }

    fn batch() -> Vec<BatchMessage> {
        vec![
            BatchMessage {
                turn: 14,
                role: "user".into(),
                content: "我把借书卡忘在家里了。".into(),
            },
            BatchMessage {
                turn: 15,
                role: "char".into(),
                content: "那我先给你记着，周五记得来还。".into(),
            },
        ]
    }

    fn draft_with(content: &str, salience: f32) -> EpisodeDraft {
        EpisodeDraft {
            content: content.to_string(),
            salience,
            emotion: None,
            place: None,
            actors: Vec::new(),
            witnesses: Vec::new(),
            links: Vec::new(),
            thread: None,
            turns: Vec::new(),
        }
    }

    fn thread_draft(title: &str) -> ThreadDraft {
        ThreadDraft {
            title: title.to_string(),
            cause: String::new(),
            actors: Vec::new(),
            importance: threads::DEFAULT_IMPORTANCE,
            grade: String::new(),
            windows: Vec::new(),
            deadline_day: None,
            cooldown: 0,
            framing: String::new(),
        }
    }

    fn codex_draft(kind: &str, target: &str) -> CodexDraft {
        CodexDraft {
            kind: kind.to_string(),
            target: target.to_string(),
            value: Value::Null,
            reason: String::new(),
        }
    }

    fn audit_draft(finding: &str, target: &str) -> AuditDraft {
        AuditDraft {
            finding: finding.to_string(),
            target: target.to_string(),
            facet: String::new(),
            value: String::new(),
            evidence: "第 3 轮提到".to_string(),
        }
    }

    /// 一份「像模型真的会吐出来」的回复：前后有杂文、外面有围栏、六类产物齐全。
    fn sample_reply() -> &'static str {
        r#"好的，本批的总结如下：
```json
{
  "summary_delta": "  小雨把画着猫的便签交给玩家，两人约好周五还书。  ",
  "episodes": [
    {
      "content": "深夜闭馆时，小雨把画着猫的便签递给了玩家",
      "salience": 0.82,
      "emotion": "温暖",
      "place": "图书馆",
      "actors": ["小雨", "玩家", "小雨"],
      "witnesses": ["小雨", "玩家"],
      "links": ["topic:便签", "person:小雨", "place:图书馆"],
      "thread": "thread.周五还书",
      "turns": [15, 14]
    }
  ],
  "hearsays": [
    {
      "content": "小雨告诉玩家，图书馆下个月要拆了",
      "source": "小雨",
      "listeners": ["玩家", " 小雨 "],
      "salience": 0.8,
      "emotion": "怅然",
      "links": ["topic:拆迁", "topic:拆迁"],
      "turns": [15]
    }
  ],
  "facts": [
    { "key": "玩家名字", "value": "阿澈" },
    { "key": "约定.还书", "value": "周五" }
  ],
  "threads": [
    {
      "title": "周五还书的约定",
      "cause": "玩家忘带借书卡，小雨破例让他先把书带走，约定周五来还。",
      "actors": ["小雨", "玩家"],
      "importance": 0.7,
      "grade": "natural",
      "windows": [
        { "blackboard": { "day": 5 } },
        { "mention": ["还书", "借书卡"] }
      ],
      "deadline_day": 6,
      "cooldown": 5,
      "framing": "她在意但不好意思催；若对方主动提起，会松一口气。"
    }
  ],
  "psyche": [
    { "kind": "feel", "name": "忐忑", "intensity": 0.6, "source": "需要「被信任」受挫" },
    { "kind": "intend", "name": "想解释", "intensity": 0.4, "source": "被误解" }
  ],
  "codex": [
    {
      "kind": "new_entity",
      "target": "char.墨墨",
      "value": { "type": "char", "name": "墨墨" },
      "reason": "第 14 轮即兴提到"
    }
  ]
}
```
以上。"#
    }

    fn sample_outcome() -> SummaryOutcome {
        sanitize(parse_outcome(sample_reply()).expect("样例回复可解析"))
    }

    // ---------- 提示词（§5.3 逐条要账）----------

    #[test]
    fn prompt_lists_all_products() {
        let p = prompt_with(&[], &[], &batch());
        for key in [
            "summary_delta",
            "episodes",
            "hearsays",
            "facts",
            "threads",
            "psyche",
            "codex",
            "audit",
        ] {
            assert!(p.contains(key), "提示词漏了产物 {key}");
        }
        for must in [
            "编年史体",
            "第三人称",
            "宁少勿滥",
            "salience",
            "emotion",
            "links",
            "提及时机",
            "克制梯度",
            "windows",
            "framing",
            "needs",
            "受挫",
            "new_entity",
            "new_fact",
            "fact_change",
            "relation",
            "anchors",
            "关联审计",
            "missed",
        ] {
            assert!(p.contains(must), "提示词漏了要求 {must}");
        }
    }

    /// 关联审计的对照表（M3.10 · §6.13）：实体清单与激活记录进提示词；
    /// 空清单时明说「没有设定集」，不逼模型对着空表编疑点。
    #[test]
    fn prompt_embeds_audit_reference_tables() {
        let ctx = ctx_of("小雨", None, None, "23:40", "", &[], &[]);
        let batch = vec![BatchMessage {
            turn: 3,
            role: "user".into(),
            content: "她今晚也在吗？".into(),
        }];
        let catalog = vec!["char.小雨（小雨）大学图书馆夜班管理员。".to_string()];
        let empty = Vec::new();
        let with_catalog = SummaryContext {
            entity_catalog: &catalog,
            active_entities: &["char.小雨".to_string(), "place.图书馆".to_string()],
            ..ctx
        };
        let p = build_prompt(&with_catalog, &batch);
        assert!(p.contains("设定集实体清单（第 9 条审计的对照表）"));
        assert!(p.contains("char.小雨（小雨）大学图书馆夜班管理员。"));
        assert!(p.contains("当轮激活记录"));
        assert!(p.contains("char.小雨、place.图书馆"));

        let without = SummaryContext {
            entity_catalog: &empty,
            active_entities: &empty,
            ..ctx
        };
        let p = build_prompt(&without, &batch);
        assert!(p.contains("本世界没有设定集实体——audit 一律给 []"));
        assert!(p.contains("（无记录——audit 只报有引源的 fact，missed 不要猜）"));
    }

    #[test]
    fn prompt_embeds_batch_and_context() {
        let threads_list = words(&["thread.周五还书（周五还书的约定）"]);
        let needs = words(&["被信任", "不再孤单"]);
        let p = prompt_with(&threads_list, &needs, &batch());
        assert!(p.contains("角色卡：小雨"));
        assert!(p.contains("玩家角色：阿澈"));
        assert!(p.contains("起因（premise）：雨夜躲进图书馆"));
        assert!(p.contains("故事时钟：第3天 23:40"));
        assert!(p.contains("前两天两人在图书馆认识。"));
        assert!(p.contains("- thread.周五还书（周五还书的约定）"));
        assert!(p.contains("被信任、不再孤单"));
        assert!(p.contains("【本批消息】（共 2 条，turn 14–15）"));
        assert!(p.contains("[turn 14 · user] 我把借书卡忘在家里了。"));
        assert!(p.contains("[turn 15 · char] 那我先给你记着，周五记得来还。"));
    }

    #[test]
    fn prompt_demands_one_json_object_with_skeleton() {
        let p = prompt_with(&[], &[], &batch());
        assert!(p.contains("只输出一个 JSON 对象"));
        assert!(p.contains(OUTCOME_SCHEMA_HINT));
        // 骨架自己是合法 JSON，且各类产物一个不少（提示词与解析器的契约）
        let parsed: Value = serde_json::from_str(OUTCOME_SCHEMA_HINT).expect("骨架是合法 JSON");
        let map = parsed.as_object().expect("骨架是对象");
        for key in [
            "summary_delta",
            "chronicle",
            "episodes",
            "hearsays",
            "facts",
            "threads",
            "psyche",
            "codex",
        ] {
            assert!(map.contains_key(key), "骨架缺少 {key}");
        }
        // 骨架还能被自己的解析器吃下去 —— 提示词与 parse_outcome 不脱节
        assert!(!parse_outcome(OUTCOME_SCHEMA_HINT)
            .expect("骨架可解析")
            .is_empty());
    }

    #[test]
    fn prompt_handles_empty_batch() {
        let p = prompt_with(&[], &[], &[]);
        assert!(p.contains("【本批消息】（空批次：没有消息滑出最近窗口）"));
        assert!(p.contains(EMPTY_BATCH_NOTE));
        assert!(p.contains(OUTCOME_SCHEMA_HINT));
        assert!(!p.contains("【本批消息】（共"));
    }

    #[test]
    fn prompt_marks_missing_context_fields() {
        let p = build_prompt(&ctx_of("", None, None, "", "", &[], &[]), &[]);
        assert!(p.contains("角色卡：（未给）"));
        assert!(p.contains("故事时钟：（未给）"));
        assert!(p.contains("已有滚动摘要（L1）：（还没有，这是开头）"));
        assert!(!p.contains("玩家角色："));
        assert!(!p.contains("起因（premise）："));
        assert!(!p.contains("活跃剧情线"));
        assert!(!p.contains("角色的需要"));
    }

    #[test]
    fn prompt_is_deterministic() {
        let threads_list = words(&["thread.周五还书"]);
        let needs = words(&["被信任"]);
        let a = prompt_with(&threads_list, &needs, &batch());
        let b = prompt_with(&threads_list, &needs, &batch());
        assert_eq!(a, b);
        // 同一次调用里连拼两遍也一样（没有时间戳 / 随机 / 迭代顺序参与）
        assert_eq!(
            build_prompt(&ctx_of("小雨", None, None, "第3天", "", &[], &[]), &batch()),
            build_prompt(&ctx_of("小雨", None, None, "第3天", "", &[], &[]), &batch())
        );
    }

    // ---------- 解析：形状与容错 ----------

    #[test]
    fn parse_reads_a_clean_object() {
        let raw = r#"{
          "summary_delta": "小雨把便签给了玩家。",
          "episodes": [{
            "content": "深夜闭馆时她把画着猫的便签递给玩家",
            "salience": 0.82,
            "emotion": "温暖",
            "place": "图书馆",
            "actors": ["小雨", "玩家"],
            "witnesses": ["小雨", "玩家"],
            "links": ["topic:便签"],
            "thread": "thread.周五还书",
            "turns": [14]
          }],
          "facts": [{ "key": "玩家名字", "value": "阿澈" }],
          "threads": [{
            "title": "周五还书的约定",
            "cause": "忘带借书卡",
            "actors": ["小雨", "玩家"],
            "importance": 0.7,
            "grade": "natural",
            "windows": [{ "blackboard": { "day": 5 } }],
            "deadline_day": 6,
            "cooldown": 5,
            "framing": "她在意但不好意思催"
          }],
          "psyche": [{ "kind": "feel", "name": "忐忑", "intensity": 0.6, "source": "被信任受挫" }],
          "codex": [{ "kind": "new_entity", "target": "char.墨墨", "value": { "type": "char" }, "reason": "第 14 轮提到" }]
        }"#;
        let o = parse_outcome(raw).expect("干净对象应当解析成功");
        assert_eq!(o.summary_delta, "小雨把便签给了玩家。");
        assert_eq!(o.episodes.len(), 1);
        approx(o.episodes[0].salience, 0.82);
        assert_eq!(o.episodes[0].emotion.as_deref(), Some("温暖"));
        assert_eq!(o.episodes[0].place.as_deref(), Some("图书馆"));
        assert_eq!(o.episodes[0].thread.as_deref(), Some("thread.周五还书"));
        assert_eq!(o.episodes[0].turns, vec![14]);
        assert_eq!(o.facts[0].key, "玩家名字");
        assert_eq!(o.facts[0].value, json!("阿澈"));
        assert_eq!(o.threads[0].deadline_day, Some(6));
        assert_eq!(o.threads[0].cooldown, 5);
        assert_eq!(
            o.threads[0].windows,
            vec![json!({"blackboard": {"day": 5}})]
        );
        assert_eq!(o.psyche[0].kind, PSYCHE_FEEL);
        assert_eq!(o.codex[0].kind, CODEX_NEW_ENTITY);
        assert_eq!(o.codex[0].value, json!({"type": "char"}));
    }

    #[test]
    fn parse_strips_json_fence() {
        let raw = "```json\n{\"summary_delta\":\"她哭了。\"}\n```";
        let o = parse_outcome(raw).unwrap();
        assert_eq!(o.summary_delta, "她哭了。");
        assert!(o.episodes.is_empty() && o.threads.is_empty() && o.codex.is_empty());
    }

    #[test]
    fn parse_strips_prose_around_object() {
        let raw =
            "好的，这是本批结果：\n{\"summary_delta\":\"两人和好。\",\"facts\":[]}\n以上，请查收。";
        let o = parse_outcome(raw).unwrap();
        assert_eq!(o.summary_delta, "两人和好。");
        assert!(o.facts.is_empty());
    }

    #[test]
    fn parse_tolerates_braces_inside_strings() {
        let o = parse_outcome(r#"{"summary_delta":"她说：{好}。"}"#).unwrap();
        assert_eq!(o.summary_delta, "她说：{好}。");
    }

    #[test]
    fn parse_accepts_array_wrapper() {
        // 模型偶尔把结果包成数组：第一个 { 到最后一个 } 的片段恰好是那个对象
        let o = parse_outcome(r#"[{"summary_delta":"两人和好。"}]"#).unwrap();
        assert_eq!(o.summary_delta, "两人和好。");
    }

    #[test]
    fn parse_missing_fields_give_empty() {
        let o = parse_outcome("{}").unwrap();
        assert!(o.is_empty());
        assert_eq!(o.summary_delta, "");
        assert!(o.episodes.is_empty() && o.facts.is_empty() && o.psyche.is_empty());

        // 缺数值字段取缺省（与 palace / threads / psyche 同口径）
        let raw = r#"{
          "episodes": [{ "content": "x" }],
          "threads": [{ "title": "t" }],
          "psyche": [{ "name": "忐忑" }],
          "codex": [{ "target": "char.小雨" }]
        }"#;
        let o = parse_outcome(raw).unwrap();
        approx(o.episodes[0].salience, palace::DEFAULT_SALIENCE);
        assert_eq!(o.episodes[0].emotion, None);
        assert_eq!(o.episodes[0].thread, None);
        assert!(o.episodes[0].turns.is_empty());
        approx(o.threads[0].importance, threads::DEFAULT_IMPORTANCE);
        assert_eq!(o.threads[0].grade, "");
        // 解析侧给 0（= 模型没给），缺省成 5 是 sanitize 的事
        assert_eq!(o.threads[0].cooldown, 0);
        assert_eq!(o.threads[0].deadline_day, None);
        assert_eq!(o.psyche[0].kind, PSYCHE_FEEL);
        approx(o.psyche[0].intensity, psyche::DEFAULT_AFFECT_INTENSITY);
        assert_eq!(o.codex[0].kind, CODEX_NEW_FACT);
        assert_eq!(o.codex[0].value, Value::Null);
    }

    #[test]
    fn parse_type_mismatch_treated_as_missing() {
        let raw = r#"{
          "summary_delta": 42,
          "episodes": "无",
          "facts": {},
          "threads": [{ "title": 7, "windows": "x", "cooldown": "5" }],
          "psyche": [{ "kind": [], "name": {}, "intensity": "0.9" }],
          "codex": [{ "target": false }]
        }"#;
        let o = parse_outcome(raw).unwrap();
        assert_eq!(o.summary_delta, "");
        assert!(o.episodes.is_empty());
        assert!(o.facts.is_empty());
        assert_eq!(o.threads.len(), 1);
        assert_eq!(o.threads[0].title, ""); // 类型不符 → 空（sanitize 会丢掉它）
        assert!(o.threads[0].windows.is_empty());
        assert_eq!(o.threads[0].cooldown, 0); // 数字字符串不认
        assert_eq!(o.psyche[0].name, "");
        assert_eq!(o.psyche[0].kind, PSYCHE_FEEL); // kind 类型不符 → 默认 feel
        approx(o.psyche[0].intensity, psyche::DEFAULT_AFFECT_INTENSITY);
        assert_eq!(o.codex[0].target, "");
    }

    #[test]
    fn parse_accepts_float_turns_and_single_object_windows() {
        let raw = r#"{
          "episodes": [{ "content": "x", "turns": [15.0, 14, -1, "15", null] }],
          "threads": [{ "title": "t", "windows": { "mention": ["还书"] } }]
        }"#;
        let o = parse_outcome(raw).unwrap();
        // 只收数字（浮点整值认，负数 / 字符串 / null 跳过），排序归一留给 sanitize
        assert_eq!(o.episodes[0].turns, vec![15, 14]);
        assert_eq!(o.threads[0].windows.len(), 1);
        assert!(o.threads[0].windows[0].is_object());
    }

    #[test]
    fn parse_non_json_is_a_readable_error() {
        let err = parse_outcome("模型今天不想说话").unwrap_err();
        assert!(err.contains("JSON"), "错误信息应当说明 JSON 的问题：{err}");
        assert!(
            err.contains("模型今天不想说话"),
            "错误信息应当带上片段：{err}"
        );

        let err = parse_outcome("{ \"summary_delta\": \"没关引号 }").unwrap_err();
        assert!(err.contains("不是合法 JSON"), "{err}");
        assert!(err.contains("片段"), "{err}");
    }

    #[test]
    fn parse_never_panics_on_garbage() {
        let mut cases: Vec<String> = vec![
            "".into(),
            "   ".into(),
            "{".into(),
            "}".into(),
            "{]".into(),
            "{}".into(),
            "[]".into(),
            "{{{{".into(),
            "{}{}".into(),
            "｛全角括号｝".into(),
            "```json".into(),
            r#"{"a":1} 尾巴 {"b":2}"#.into(),
            "第 14 轮：她笑了（但没说话）。".into(),
        ];
        cases.push("很长的噪声".repeat(2_000));
        for raw in &cases {
            // 解析的成败都不重要，只要不 panic 且错误信息可读
            if let Err(e) = parse_outcome(raw) {
                assert!(!e.is_empty());
            }
        }
    }

    // ---------- 归一：夹紧 / 去重 / 限额 ----------

    #[test]
    fn sanitize_clamps_numbers() {
        let out = sanitize(SummaryOutcome {
            summary_delta: "  她哭了。  ".into(),
            chronicle: String::new(),
            episodes: vec![
                draft_with("a", 1.7),
                draft_with("b", -3.0),
                draft_with("c", f32::NAN),
                draft_with("d", f32::INFINITY),
            ],
            threads: vec![ThreadDraft {
                importance: f32::NAN,
                ..thread_draft("t")
            }],
            psyche: vec![
                PsycheDraft {
                    kind: PSYCHE_FEEL.into(),
                    name: "忐忑".into(),
                    intensity: 9.0,
                    source: String::new(),
                },
                PsycheDraft {
                    kind: PSYCHE_INTEND.into(),
                    name: "想解释".into(),
                    intensity: -9.0,
                    source: String::new(),
                },
                PsycheDraft {
                    kind: PSYCHE_INTEND.into(),
                    name: "想逃".into(),
                    intensity: f32::NAN,
                    source: String::new(),
                },
            ],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.summary_delta, "她哭了。");
        approx(out.episodes[0].salience, 1.0);
        approx(out.episodes[1].salience, 0.0);
        approx(out.episodes[2].salience, 0.5); // NaN → 0.5（§5.2 的中位值）
        approx(out.episodes[3].salience, 0.5);
        approx(out.threads[0].importance, threads::DEFAULT_IMPORTANCE);
        approx(out.psyche[0].intensity, 1.0);
        approx(out.psyche[1].intensity, -1.0); // 意图增减可为负（§9.2）
        approx(out.psyche[2].intensity, 0.5);
    }

    #[test]
    fn sanitize_drops_empty_identity_fields() {
        let out = sanitize(SummaryOutcome {
            summary_delta: "   ".into(),
            chronicle: String::new(),
            episodes: vec![draft_with("   ", 0.5), draft_with("有效记忆", 0.5)],
            hearsays: vec![HearsayDraft {
                content: "  ".into(),
                ..hearsay_draft("占位")
            }, hearsay_draft("有效转述")],
            facts: vec![
                FactDraft {
                    key: "  ".into(),
                    value: json!(1),
                },
                FactDraft {
                    key: "ok".into(),
                    value: json!(1),
                },
            ],
            threads: vec![thread_draft("  "), thread_draft("有效线")],
            psyche: vec![
                PsycheDraft {
                    kind: PSYCHE_FEEL.into(),
                    name: " ".into(),
                    intensity: 0.5,
                    source: String::new(),
                },
                PsycheDraft {
                    kind: PSYCHE_FEEL.into(),
                    name: "忐忑".into(),
                    intensity: 0.5,
                    source: String::new(),
                },
            ],
            codex: vec![
                codex_draft(CODEX_NEW_FACT, " "),
                codex_draft(CODEX_NEW_FACT, "char.小雨"),
            ],
            audit: vec![audit_draft(AUDIT_MISSED, " ")],
        });
        assert_eq!(out.summary_delta, "");
        assert!(!out.is_empty());
        assert_eq!(out.episodes.len(), 1);
        assert_eq!(out.episodes[0].content, "有效记忆");
        assert_eq!(out.facts.len(), 1);
        assert_eq!(out.threads.len(), 1);
        assert_eq!(out.threads[0].title, "有效线");
        assert_eq!(out.hearsays.len(), 1);
        assert_eq!(out.hearsays[0].content, "有效转述");
        assert_eq!(out.psyche.len(), 1);
        assert_eq!(out.codex.len(), 1);
    }

    #[test]
    fn sanitize_dedups_words_and_sorts_turns() {
        let mut e = draft_with("她把便签递给他", 0.8);
        e.actors = words(&["小雨", " 小雨 ", "玩家", "小雨"]);
        e.witnesses = words(&["Alice", "alice", "玩家"]); // 拉丁大小写不敏感
        e.links = words(&["topic:便签", "topic:便签", " person:小雨 "]);
        e.turns = vec![15, 14, 15, 14, 3];
        let out = sanitize(SummaryOutcome {
            episodes: vec![e],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.episodes[0].actors, words(&["小雨", "玩家"]));
        assert_eq!(out.episodes[0].witnesses, words(&["Alice", "玩家"]));
        assert_eq!(out.episodes[0].links, words(&["topic:便签", "person:小雨"]));
        assert_eq!(out.episodes[0].turns, vec![3, 14, 15]);
    }

    #[test]
    fn sanitize_truncates_keeping_order() {
        let episodes: Vec<EpisodeDraft> = (0..MAX_EPISODES + 3)
            .map(|i| draft_with(&format!("记忆{i}"), 0.5))
            .collect();
        let threads: Vec<ThreadDraft> = (0..MAX_THREADS + 3)
            .map(|i| thread_draft(&format!("线{i}")))
            .collect();
        let codex: Vec<CodexDraft> = (0..MAX_CODEX_DRAFTS + 3)
            .map(|i| codex_draft(CODEX_NEW_FACT, &format!("char.{i}")))
            .collect();
        let out = sanitize(SummaryOutcome {
            episodes,
            threads,
            codex,
            ..SummaryOutcome::default()
        });
        assert_eq!(out.episodes.len(), MAX_EPISODES);
        assert_eq!(out.episodes[0].content, "记忆0");
        assert_eq!(
            out.episodes[MAX_EPISODES - 1].content,
            format!("记忆{}", MAX_EPISODES - 1)
        );
        assert_eq!(out.threads.len(), MAX_THREADS);
        assert_eq!(out.threads[0].title, "线0");
        assert_eq!(out.codex.len(), MAX_CODEX_DRAFTS);
        assert_eq!(out.codex[0].target, "char.0");
    }

    #[test]
    fn sanitize_normalizes_grade_and_cooldown() {
        let out = sanitize(SummaryOutcome {
            threads: vec![
                ThreadDraft {
                    grade: " EAGER ".into(),
                    cooldown: 0,
                    ..thread_draft("a")
                },
                ThreadDraft {
                    grade: "urgent".into(),
                    cooldown: 3,
                    ..thread_draft("b")
                },
            ],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.threads[0].grade, threads::GRADE_EAGER);
        assert_eq!(out.threads[0].cooldown, threads::DEFAULT_COOLDOWN); // 0 = 没给 → 5
        assert_eq!(out.threads[1].grade, threads::GRADE_NATURAL); // 未知 → natural
        assert_eq!(out.threads[1].cooldown, 3); // 显式冷却原样保留
    }

    #[test]
    fn sanitize_keeps_only_usable_windows() {
        let t = ThreadDraft {
            windows: vec![
                json!({"mention": ["还书", "借书卡"]}),
                json!({}),        // 空对象：没有任何可识别条件
                json!("mention"), // 不是对象
                json!({"state_path": ["图书馆"]}),
                json!({"无关键": 1}),                   // 认不出的键
                json!({"mention": ["还书", "借书卡"]}), // 与首条重复
            ],
            ..thread_draft("周五还书的约定")
        };
        let out = sanitize(SummaryOutcome {
            threads: vec![t],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.threads[0].windows.len(), 2);
        assert_eq!(
            out.threads[0].windows[0],
            json!({"mention": ["还书", "借书卡"]})
        );
        assert_eq!(out.threads[0].windows[1], json!({"state_path": ["图书馆"]}));
    }

    #[test]
    fn sanitize_trims_text_and_blanks_optionals() {
        let mut e = draft_with("  她把便签给他  ", 0.8);
        e.emotion = Some("  温暖 ".into());
        e.place = Some("   ".into());
        e.thread = Some("".into());
        let out = sanitize(SummaryOutcome {
            summary_delta: "  梗概  ".into(),
            chronicle: String::new(),
            episodes: vec![e],
            facts: vec![FactDraft {
                key: "  玩家名字 ".into(),
                value: json!("  阿澈  "),
            }],
            threads: vec![ThreadDraft {
                cause: "  起因 ".into(),
                framing: "  指引 ".into(),
                ..thread_draft(" 线 ")
            }],
            codex: vec![CodexDraft {
                kind: " relation ".into(),
                target: " char.小雨 ".into(),
                value: json!({"to": "char.墨墨"}),
                reason: " 理由 ".into(),
            }],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.summary_delta, "梗概");
        assert_eq!(out.episodes[0].content, "她把便签给他");
        assert_eq!(out.episodes[0].emotion.as_deref(), Some("温暖"));
        assert_eq!(out.episodes[0].place, None);
        assert_eq!(out.episodes[0].thread, None);
        assert_eq!(out.facts[0].key, "玩家名字");
        assert_eq!(out.facts[0].value, json!("阿澈"));
        assert_eq!(out.threads[0].title, "线");
        assert_eq!(out.threads[0].cause, "起因");
        assert_eq!(out.threads[0].framing, "指引");
        assert_eq!(out.codex[0].kind, CODEX_RELATION);
        assert_eq!(out.codex[0].target, "char.小雨");
        assert_eq!(out.codex[0].reason, "理由");
    }

    #[test]
    fn sanitize_normalizes_unknown_kinds() {
        let out = sanitize(SummaryOutcome {
            psyche: vec![PsycheDraft {
                kind: "心情".into(),
                name: "忐忑".into(),
                intensity: 0.5,
                source: String::new(),
            }],
            codex: vec![codex_draft("随便写的", "char.小雨")],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.psyche[0].kind, PSYCHE_FEEL);
        assert_eq!(out.codex[0].kind, CODEX_NEW_FACT);
    }

    // ---------- 转述（M3.3 · §10.4）：解析与归一 ----------

    fn hearsay_draft(content: &str) -> HearsayDraft {
        HearsayDraft {
            content: content.to_string(),
            source: "小雨".into(),
            listeners: words(&["玩家"]),
            salience: 0.8,
            emotion: None,
            place: None,
            links: Vec::new(),
            thread: None,
            turns: Vec::new(),
            reveals: Vec::new(),
        }
    }

    #[test]
    fn parse_reads_hearsay_fields_and_defaults() {
        let raw = r#"{
          "hearsays": [
            {
              "content": "小雨告诉玩家，图书馆要拆了",
              "source": "小雨",
              "listeners": ["玩家"],
              "salience": 0.9,
              "emotion": "怅然",
              "place": "图书馆",
              "links": ["topic:拆迁"],
              "thread": "thread.周五还书",
              "turns": [15, 14],
              "reveals": ["char.图书馆.secrets.拆迁"]
            },
            { "content": "缺字段的最简形态", "source": "小雨", "listeners": ["玩家"] }
          ]
        }"#;
        let o = parse_outcome(raw).unwrap();
        assert_eq!(o.hearsays.len(), 2);
        let hs = &o.hearsays[0];
        approx(hs.salience, 0.9);
        assert_eq!(hs.emotion.as_deref(), Some("怅然"));
        assert_eq!(hs.place.as_deref(), Some("图书馆"));
        assert_eq!(hs.thread.as_deref(), Some("thread.周五还书"));
        assert_eq!(hs.turns, vec![15, 14]);
        assert_eq!(hs.reveals, words(&["char.图书馆.secrets.拆迁"]));
        // 缺省字段：salience 取宫殿缺省、可选全空
        let bare = &o.hearsays[1];
        approx(bare.salience, palace::DEFAULT_SALIENCE);
        assert!(bare.reveals.is_empty() && bare.turns.is_empty() && bare.links.is_empty());

        // 缺 hearsays 键 = 空清单（不带任何产物进来）
        assert!(parse_outcome("{}").unwrap().hearsays.is_empty());
    }

    #[test]
    fn sanitize_drops_unhearable_or_empty_hearsays() {
        let out = sanitize(SummaryOutcome {
            hearsays: vec![
                hearsay_draft("有效的转述"),
                hearsay_draft("   "), // 空正文
                HearsayDraft {
                    source: "  ".into(),
                    ..hearsay_draft("没有告知者")
                },
                HearsayDraft {
                    listeners: Vec::new(), // 没有听众 = 没人听见
                    ..hearsay_draft("自言自语")
                },
                HearsayDraft {
                    listeners: words(&["小雨"]), // 听众只有告知者本人 = 没人听见
                    ..hearsay_draft("讲给自己听")
                },
            ],
            ..SummaryOutcome::default()
        });
        assert_eq!(out.hearsays.len(), 1);
        assert_eq!(out.hearsays[0].content, "有效的转述");
    }

    #[test]
    fn sanitize_hearsay_clamps_and_dedups() {
        let mut h = hearsay_draft("夹紧与去重");
        h.salience = 7.0;
        h.listeners = words(&["玩家", " 玩家 ", "阿澈", " 小雨 "]);
        h.links = words(&["topic:拆迁", "topic:拆迁", " person:小雨 "]);
        h.turns = vec![15, 14, 15];
        h.reveals = words(&["char.小雨.secrets.工作牌", " char.小雨.secrets.工作牌 ", ""]);
        let out = sanitize(SummaryOutcome {
            hearsays: vec![h],
            ..SummaryOutcome::default()
        });
        let hs = &out.hearsays[0];
        approx(hs.salience, 1.0);
        assert_eq!(hs.listeners, words(&["玩家", "阿澈"]), "去重，告知者本人剔除");
        assert_eq!(hs.links, words(&["topic:拆迁", "person:小雨"]));
        assert_eq!(hs.turns, vec![14, 15]);
        assert_eq!(hs.reveals, words(&["char.小雨.secrets.工作牌"]));
    }

    #[test]
    fn sanitize_truncates_hearsays_keeping_order() {
        let hearsays: Vec<HearsayDraft> = (0..MAX_HEARSAYS + 3)
            .map(|i| hearsay_draft(&format!("转述{i}")))
            .collect();
        let out = sanitize(SummaryOutcome {
            hearsays,
            ..SummaryOutcome::default()
        });
        assert_eq!(out.hearsays.len(), MAX_HEARSAYS);
        assert_eq!(out.hearsays[0].content, "转述0");
    }

    #[test]
    fn hearsay_draft_feeds_a_palace_memory() {
        let mut h = hearsay_draft("小雨告诉玩家，图书馆要拆了");
        h.salience = 0.8;
        h.links = words(&["topic:拆迁", "person:小雨"]);
        h.turns = vec![15];
        // 宿主写入侧的规则（apply_summary_outcome）：salience 折半、每个听众各一条
        let listener = &h.listeners[0];
        let mem = palace::MemObject {
            id: palace::next_id(7),
            kind: palace::KIND_HEARSAY.to_string(),
            content: h.content.clone(),
            turn: h.turns.first().copied().unwrap_or(0),
            story_day: 3,
            story_clock: "第3天 21:05".to_string(),
            place: h.place.clone(),
            actors: vec![h.source.clone(), listener.clone()],
            witnesses: vec![listener.clone()],
            salience: h.salience * palace::HEARSAY_SALIENCE_FACTOR,
            emotion: h.emotion.clone(),
            links: h.links.clone(),
            thread: h.thread.clone(),
            source: h.source.clone(),
            ts: 0,
            rehearsals: 0,
        };
        assert_eq!(mem.kind, palace::KIND_HEARSAY);
        assert!((mem.salience - 0.4).abs() < 1e-6, "听来的事显著度折半（§10.4）");
        assert_eq!(mem.witnesses_or_actors(), vec!["玩家".to_string()]);
        // 渲染行带来源标注（召回时翻旧账有据可查）
        assert!(palace::render_memory_block(&[palace::RecallHit {
            mem: mem.clone(),
            score: 0.4,
            reasons: vec![],
        }])
        .contains("转述自小雨"));
        // 关联继承：原事件的 links 在召回里照样命中
        assert!(mem.links_match("topic:拆迁"));
    }

    #[test]
    fn sanitize_is_idempotent() {
        let once = sample_outcome();
        let twice = sanitize(once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn sanitize_empty_outcome_stays_empty() {
        let out = sanitize(SummaryOutcome::default());
        assert!(out.is_empty());
        assert_eq!(out, SummaryOutcome::default());
    }

    // ---------- 跨模块：提案形态必须能直接喂给 threads / palace ----------

    #[test]
    fn windows_feed_threads_resurface_window_directly() {
        let out = sample_outcome();
        let draft = &out.threads[0];
        assert!(draft.windows.len() >= 2);
        let mut blackboard = std::collections::BTreeMap::new();
        blackboard.insert("day".to_string(), json!(5));
        let q = threads::ThreadQuery::new(40, 5, "第5天 18:00", &blackboard);

        let mut live = 0;
        for w in &draft.windows {
            let window = threads::ResurfaceWindow::from_value(w).expect("宿主能直接解析这个窗口");
            if window.hit(&q).is_some() {
                live += 1;
            }
        }
        assert!(live >= 1, "黑板 day=5 的窗口应当在第 5 天命中");
    }

    #[test]
    fn thread_draft_resurface_value_round_trips_through_threads() {
        let draft = sample_outcome().threads[0].clone();
        let value = draft.resurface_value();
        let resurface = threads::Resurface::from_value(&value);
        assert_eq!(resurface.grade, threads::GRADE_NATURAL);
        assert_eq!(resurface.windows.len(), draft.windows.len());
        assert_eq!(resurface.cooldown, threads::DEFAULT_COOLDOWN);
        assert_eq!(resurface.framing, draft.framing);
        assert_eq!(resurface.deadline.as_ref().map(|d| d.day), Some(6));
        assert_eq!(
            resurface.deadline.as_ref().map(|d| d.escalate.as_str()),
            Some(threads::DEFAULT_ESCALATE)
        );
        // 宿主落事件用 to_value，读回来仍一致
        assert_eq!(
            threads::Resurface::from_value(&resurface.to_value()),
            resurface
        );

        // 开线：把提案起草的时机装进线，窗口该中时中、不该中时不中（§8.4 的克制）
        let mut t = threads::Thread::open(
            "",
            &draft.title,
            &draft.cause,
            &draft.actors,
            draft.importance,
            threads::ThreadStamp {
                turn: 15,
                story_day: 3,
                story_clock: "第3天 23:40".into(),
            },
        );
        assert_eq!(t.id, "thread.周五还书的约定");
        t.resurface = resurface;

        let mut bb5 = std::collections::BTreeMap::new();
        bb5.insert("day".to_string(), json!(5));
        let at_friday = threads::ThreadQuery::new(40, 5, "第5天 09:00", &bb5);
        assert!(t.window_hit(&at_friday).is_some(), "约定期限当天应当进窗口");

        let mut bb4 = std::collections::BTreeMap::new();
        bb4.insert("day".to_string(), json!(4));
        let before = threads::ThreadQuery::new(38, 4, "第4天 09:00", &bb4);
        assert!(t.window_hit(&before).is_none(), "时机未到就不该进现状卡");
    }

    #[test]
    fn episode_draft_feeds_a_palace_memory() {
        let e = sample_outcome().episodes[0].clone();
        let mem = palace::MemObject {
            id: palace::next_id(192),
            kind: palace::KIND_EPISODE.to_string(),
            content: e.content.clone(),
            turn: e.turns.first().copied().unwrap_or(0),
            story_day: 3,
            story_clock: "第3天 23:40".to_string(),
            place: e.place.clone(),
            actors: e.actors.clone(),
            witnesses: e.witnesses.clone(),
            salience: e.salience,
            emotion: e.emotion.clone(),
            links: e.links.clone(),
            thread: e.thread.clone(),
            source: "pipeline".to_string(),
            ts: 0,
            rehearsals: 0,
        };
        assert_eq!(mem.id, "mem_0192");
        assert_eq!(mem.turn, 14); // turns 升序 → 首个是最早的那一轮
        assert_eq!(mem.kind, palace::KIND_EPISODE);
        assert!(
            mem.links_match("topic:便签"),
            "管线给的 links 要能被宫殿召回命中"
        );
        assert!(mem.links_match("便签"));
        assert_eq!(mem.witnesses_or_actors(), e.witnesses);

        // witnesses 空 = 默认 actors（§5.2）
        let mut bare = mem.clone();
        bare.witnesses.clear();
        assert_eq!(bare.witnesses_or_actors(), bare.actors);
    }

    #[test]
    fn batch_message_converts_from_store_message() {
        let m = crate::store::Message {
        name: None,
            turn: 7,
            role: "char".into(),
            content: "她笑了笑。".into(),
            ts: 0,
            scene_id: None,
        };
        let b = BatchMessage::from_message(&m);
        assert_eq!(b.turn, 7);
        assert_eq!(b.role, "char");
        assert_eq!(b.content, "她笑了笑。");
    }

    // ---------- 端到端：从模型回复到可落库的产物 ----------

    #[test]
    fn end_to_end_parse_and_sanitize() {
        let parsed = parse_outcome(sample_reply()).expect("带围栏与前后杂文的回复应当能解析");
        assert_eq!(parsed.episodes[0].turns, vec![15, 14]); // 解析只管形状
        assert_eq!(parsed.episodes[0].actors.len(), 3);

        let out = sanitize(parsed);
        assert_eq!(
            out.summary_delta,
            "小雨把画着猫的便签交给玩家，两人约好周五还书。"
        );
        assert_eq!(out.episodes[0].turns, vec![14, 15]); // 归一后升序
        assert_eq!(out.episodes[0].actors, words(&["小雨", "玩家"]));
        assert_eq!(out.facts.len(), 2);
        assert_eq!(out.threads.len(), 1);
        assert_eq!(out.psyche.len(), 2);
        assert_eq!(out.codex.len(), 1);

        // 转述：听众里的告知者被剔除、links 去重、salience 保留原值（折半是宿主的事）
        assert_eq!(out.hearsays.len(), 1);
        let hs = &out.hearsays[0];
        assert_eq!(hs.source, "小雨");
        assert_eq!(hs.listeners, words(&["玩家"]));
        assert_eq!(hs.links, words(&["topic:拆迁"]));
        approx(hs.salience, 0.8);
    }

    #[test]
    fn parse_and_sanitize_are_deterministic() {
        let a = sanitize(parse_outcome(sample_reply()).expect("可解析"));
        let b = sanitize(parse_outcome(sample_reply()).expect("可解析"));
        assert_eq!(a, b);
        // 空批次同样确定（提示词一模一样）
        let threads_list = words(&["thread.周五还书"]);
        let needs = words(&["被信任"]);
        assert_eq!(
            prompt_with(&threads_list, &needs, &[]),
            prompt_with(&threads_list, &needs, &[])
        );
    }

    #[test]
    fn outcome_is_empty_only_when_nothing_came_back() {
        assert!(SummaryOutcome::default().is_empty());
        let only_summary = SummaryOutcome {
            summary_delta: "  有进展  ".to_string(),
            chronicle: String::new(),
            ..SummaryOutcome::default()
        };
        assert!(!sanitize(only_summary).is_empty());
    }
}
