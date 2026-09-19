// 自动总结管线（M2.6 · 设计 §5.3）：滑窗淘汰批次的六类产物
//
// 消息滑出 L0 窗口时，轮末**异步**触发一次总结调用（不阻塞对话；失败重试，批次可从事件流
// 回放重算）。本模块只负责这条管线的**引擎侧纯逻辑**——拼提示词、解析模型回复、校验与
// 夹紧、产出提案对象：不读文件、不联网、不跑 Lua（m2.md 决断 3）。
//
// 六类产物与设计 §5.3 那张图逐条对应（也是 build_prompt 逐条要账的清单）：
//
//   1. L1 摘要增量   summary_delta —— 编年史体、第三人称，并入 summary.md（§5.1）
//   2. 情景记忆      episodes      —— episode 形态入宫殿（§5.2：salience/emotion/links/thread）
//   3. L3 事实键值   facts         —— 跨会话持久的事实（§5.1）
//   4. 剧情线提案    threads       —— 开线 + **提及时机起草**（§8.3）：grade（克制梯度）/
//                                    windows（可提及窗口，直接写成 §8.2 的 JSON 形态）/
//                                    deadline / cooldown / framing；宿主喂
//                                    threads::ResurfaceWindow::from_value 即可用
//   5. 心理评价提案  psyche        —— 需要满足/受挫 → 情绪与意图增减（§9.2 评价闭环）
//   6. 设定提案      codex         —— 新实体/新事实/事实变更/新关系 → 设定收件箱（§6.8）
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
/// 单批剧情线提案条数上限（§8.3；开线是大事，不能一轮开一堆）。
pub const MAX_THREADS: usize = 2;
/// 单批设定提案条数上限（§6.8；收件箱要人审，给太多等于没给）。
pub const MAX_CODEX_DRAFTS: usize = 6;

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

/// 输出骨架（写进提示词，也是 parse_outcome 的契约，见模块头注释）。
///
/// 刻意写成**合法 JSON**：测试会把它解一遍，保证骨架与解析器不脱节。里面的示例值只是
/// 格式示范，模型要按本批消息重写（骨架里的字面值不该被照抄进产物）。
pub const OUTCOME_SCHEMA_HINT: &str = r#"{
  "summary_delta": "第三人称编年史体，2–6 句；没有进展给空串",
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
    }
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
    /// 故事时钟（如 「第3天 23:40」；宿主从黑板取）。
    pub story_clock: &'a str,
    /// 已有 L1 滚动摘要（本批之前的梗概；首轮为空串）。
    pub rolling_summary: &'a str,
    /// 活跃剧情线（C1 未决事项的全量只读投影，§8.5）：给 id 或「id（标题）」都行。
    pub active_threads: &'a [String],
    /// 角色的需要清单（§9.2「评价之源」；codex char 的 needs/values）。
    pub needs: &'a [String],
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

/// 一次总结的全部产物（§5.3 六类）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SummaryOutcome {
    /// L1 滚动摘要增量（编年史体、第三人称；空串 = 本批无进展）。
    pub summary_delta: String,
    /// 情景记忆草稿（入宫殿，§5.2）。
    pub episodes: Vec<EpisodeDraft>,
    /// L3 事实键值草稿（§5.1）。
    pub facts: Vec<FactDraft>,
    /// 剧情线提案（含提及时机起草，§8.3）。
    pub threads: Vec<ThreadDraft>,
    /// 心理评价提案（§9.2）。
    pub psyche: Vec<PsycheDraft>,
    /// 设定提案（进收件箱，§6.8）。
    pub codex: Vec<CodexDraft>,
}

impl SummaryOutcome {
    /// 六类产物是否全空（宿主据此跳过落盘与事件；空产物算成功，不算失败）。
    pub fn is_empty(&self) -> bool {
        self.summary_delta.trim().is_empty()
            && self.episodes.is_empty()
            && self.facts.is_empty()
            && self.threads.is_empty()
            && self.psyche.is_empty()
            && self.codex.is_empty()
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
    /// new_entity | new_fact | fact_change | relation；见 CODEX_*。
    pub kind: String,
    /// 目标：既有实体 id（如 char.小雨）或新实体的预分配 id。
    pub target: String,
    /// 提案内容（facet 值 / 新关系对象 / 新实体骨架）；宿主写入前先过
    /// codex::anchors_conflict（§6.8 anchors 最高保护级）。
    pub value: serde_json::Value,
    /// 出处说明（「第 14 轮即兴提到」），收件箱双源呈现用（§6.8）。
    pub reason: String,
}

// ---------- 拼提示词（宿主 ① 的入口）----------

/// 角色简介（提示词第一段）：告诉模型它是谁、不许做什么。
const ROLE_BRIEF: &str = "你是《化境》的「自动总结管线」引擎（设计 §5.3）：一批消息滑出最近窗口后，\
由你把已经发生的剧情整理成长期记忆与提案。\
你不续写剧情、不扮演角色、不替角色做决定；只做归纳与抽取。\
宁可少写，不要编造——下面这批消息里没有的东西，一个字也不要补。";

/// 六类产物的逐条要求（§5.3 那张图的展开；build_prompt 的核心段落）。
const PRODUCT_SPEC: &str = r#"【必须逐条产出的六类产物】（设计 §5.3；哪一类都没有就给空，但字段不要省）

1. summary_delta —— L1 滚动摘要增量（设计 §5.1）
   把本批消息推进的剧情并入长期摘要：编年史体、第三人称，只写「发生了什么、结果如何」，
   2–6 句。不抄台词、不写文采、不揣测内心；起因与结果比过程重要。本批没有值得记的进展就给空串 ""。

2. episodes —— 情景记忆（设计 §5.2，入记忆宫殿）
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


3. facts —— L3 事实键值（设计 §5.1）
   跨会话仍然要记得的稳定事实：玩家叫什么、生日、约定、关键事件、稳定偏好。
   key 用简短中文或点分路径（如 玩家名字 / 约定.还书），value 是 JSON 标量或短数组。
   一次性的、会变的、拿不准的不要写；没有就给 []。

4. threads —— 剧情线提案（设计 §8.3「管线提案」，必须一并起草提及时机）
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


5. psyche —— 心理评价提案（设计 §9.2：情绪是对「需要是否被满足」的态度体验）
   对照上文的「需要（needs）」清单评价本批消息——某个需要被满足或受挫时：
   - 情绪：{"kind": "feel", "name": "<情绪名>", "intensity": <0–1>, "source": "<哪个需要被满足/受挫>"}；
   - 意图增减：{"kind": "intend", "name": "<意图名>", "intensity": <-1–1>, "source": "<原因>"}
     （正数增强、负数削弱；例如 -0.3 表示「想解释」的冲动被削掉三成）。
   没有明显评价就给 []；不要为了凑数造情绪，也不要写角色的台词倾向（那是生成时的职责）。

6. codex —— 设定提案（设计 §6.8，进设定收件箱；未经确认不进注入，§6.9）
   本批消息里即兴发明且值得留下的世界事实（例如「她养了一只叫墨墨的猫」）。kind 四选一：
   - "new_entity"  全新实体（必须人工确认）：{"kind":"new_entity","target":"char.墨墨",
       "value":{"type":"char","name":"墨墨","facts":{"look.impression":"一只黑猫"}},
       "reason":"第 14 轮即兴提到"}；
   - "new_fact"    给既有实体加一条事实：target = 实体 id，value = 要写入的 facet 内容；
   - "fact_change" 改写既有事实：value 写新值，reason 说明出自第几轮；
   - "relation"    新关系：value 形如 {"to":"char.小雨","kind":"宠物","always_with":false}。
   绝不允许改动任何实体的恒定辨识点 anchors（设计 §6.8：anchors 是最高保护级，与之冲突的提案
   会被直接驳回）；只是气氛描写、拿不准的，不要写。最多 6 条，没有就给 []。"#;

// @@TAIL@@
