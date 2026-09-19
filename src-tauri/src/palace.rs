// 记忆宫殿：情景记忆的空间化组织（设计 `5）
//
// 一条记忆 = `palace.jsonl` 追加流里的一个对象（`5.2）：情景本身（content）+
// 故事时间（story_day / story_clock）+ 空间（place）+ 人（actors / witnesses）+
// 权重（salience / emotion / rehearsals）+ 关联（links / thread）。
// 「房间 / 人物厅 / 时间线走廊」只是组织隐喻（`5.2），物理上就是本模块这套带索引的对象库；
// 面板三视图（`5.5）由 rooms / timeline / link_graph 供数。
//
// 本模块是**纯数据与算法**：不碰文件、不碰网络、不跑 Lua（m2.md 决断 3），
// 因而召回打分能被单测直接钉死。打分**必须完全确定**：同样的 (记忆集合, 查询) 必然给出
// 同样的次序——同分时按 (story_day, turn, id) 升序。这是设计 `7.3「同一事件流重放状态
// 路径一致」在召回侧的前提，也让 B4 槽的降级顺序可回放。
//
// 召回打分（设计 `5.4，常量具名）：
//
//   score = salience × 时间衰减 × 关联加成 × 再提及加成
//     时间衰减 = 0.5f64.powf(Δ故事天 / HALF_LIFE_DAYS)，Δ = now_day - story_day（负数按 0）
//     关联加成 = 1.0 + Σ 命中权重，上限 RELEVANCE_CAP = 3.0：
//                 hints 命中 link +HINT_LINK_WEIGHT · place 命中 place: link +PLACE_LINK_WEIGHT
//                 · 在场者命中 person: link +PRESENT_LINK_WEIGHT · mentions 命中 link +MENTION_LINK_WEIGHT
//                 · thread ∈ active_threads +ACTIVE_THREAD_WEIGHT · actors ∩ 在场者 +CO_ACTOR_WEIGHT
//     再提及加成 = (1.0 + REMENTION_STEP × rehearsals)，上限 REMENTION_CAP = 1.5
//
// 流程：**视角过滤（硬约束，`10.4）→ 打分 → 排序 → top-K → token 预算截断**。
// 视角过滤只放行 viewer ∈ witnesses_or_actors() 的记忆——她只记得她经历过或被告知的；
// witnesses 为空视为默认 = actors（`5.2）。转述（kind = hearsay）是信息跨视角流动的
// 唯一通道，渲染时标注来源（`10.4）。
//
// 读侧兼容：M1 的 `palace.jsonl` 里只有 kind=fact 的键值记录（store::MemRecord），
// 由 from_legacy_fact 并入宫殿；反序列化也直接吃那种行（content 由 key/value 合成，
// 并补一条 topic:<key> 关联，好让状态树 recall 提示仍能命中旧事实）。设计 `5.2 的
// 嵌套 time 形态（{"time":{"turn":14,"story_clock":"第3天 23:40"}}）同样可读，
// 缺 story_day 时从 story_clock 的「第N天」里解出来。
//
// 两条时间路径的取舍：recall 的衰减是**即时计算**（相对查询的 now_day，不写回），
// decay_all 是**时钟步进/睡眠整理时把衰减固化进 salience**（不挪 story_day——它是事件
// 时间戳，时间线视图要靠它）。宿主只应选其一：常态用 recall 即时衰减，decay_all 留给
// 批量整理，二者叠加会让旧事衰减过快。
#![allow(dead_code)]

use serde::{Deserialize, Deserializer, Serialize};

use crate::prompt::estimate_tokens;

// ---------- 常量：打分规则具名化（设计 `5.4）----------

/// 故事时间半衰期（天）：每过 7 个故事天，召回权重减半。
pub const HALF_LIFE_DAYS: f64 = 7.0;
/// 关联加成上限：防止「到处都关联得上」的记忆淹没视角内真正相关的事。
pub const RELEVANCE_CAP: f64 = 3.0;
/// 再提及加成上限（rehearsals ≥ 5 即封顶）。
pub const REMENTION_CAP: f64 = 1.5;
/// 每次再提及的加成步长。
pub const REMENTION_STEP: f64 = 0.1;
/// 状态树 recall 提示命中 link 的权重。
pub const HINT_LINK_WEIGHT: f64 = 0.6;
/// 当前地点命中 place: link（或 place 字段）的权重。
pub const PLACE_LINK_WEIGHT: f64 = 0.5;
/// 在场者命中 person: link 的权重。
pub const PRESENT_LINK_WEIGHT: f64 = 0.4;
/// 最近提及命中 link 的权重。
pub const MENTION_LINK_WEIGHT: f64 = 0.3;
/// 记忆所属剧情线 ∈ 活跃线的权重。
pub const ACTIVE_THREAD_WEIGHT: f64 = 0.5;
/// actors 与在场者有交集的权重（有交集记一次，不按人数叠加）。
pub const CO_ACTOR_WEIGHT: f64 = 0.2;

/// 记忆类型：情景（管线总结出的亲身经历，`5.3）。
pub const KIND_EPISODE: &str = "episode";
/// 记忆类型：事实（L3 键值，含 M1 旧记录，`5.1）。
pub const KIND_FACT: &str = "fact";
/// 记忆类型：转述（A 告诉 B，salience 折半、links 继承，`10.4）。
pub const KIND_HEARSAY: &str = "hearsay";

/// 缺省显著度（管线未给权重时的中位值）。
pub const DEFAULT_SALIENCE: f32 = 0.5;
/// M1 旧 fact 记录的显著度：卡内写定的长期事实，比情景记忆稳定但偏弱。
pub const LEGACY_FACT_SALIENCE: f32 = 0.5;
/// 房间视图 / 时间线视图每个桶最多带几条摘要（面板数据，`5.5）。
pub const VIEW_TOP_N: usize = 5;

// ---------- 记忆对象 ----------

/// 记忆对象（设计 `5.2：`palace.jsonl` 追加流中的一条）。
///
/// 字段与设计文档一一对应，扁平存放（turn / story_day / story_clock 三件套在读侧也
/// 接受设计示例的嵌套 time 形态，见模块头注释）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemObject {
    /// 记忆 id，形如 `mem_0001`（宿主用 next_id 分配）。
    pub id: String,
    /// 类型：episode / fact / hearsay（见 KIND_*）。
    pub kind: String,
    /// 情景正文（第三人称一句话）。
    pub content: String,
    /// 产生该记忆的轮次（溯源用，UI 可一键跳回原文）。
    pub turn: u64,
    /// 故事天（第 N 天）。
    pub story_day: i64,
    /// 故事时钟，如 `23:40`；也接受已含天数的 `第3天 23:40`。
    pub story_clock: String,
    /// 故事地点（房间 = 地点，`5.2）。
    pub place: Option<String>,
    /// 参与者。
    pub actors: Vec<String>,
    /// 见证者（视角召回的过滤依据，`10.4）；为空时默认 = actors。
    pub witnesses: Vec<String>,
    /// 显著度 0–1：召回权重与衰减的核心（`5.2）。
    pub salience: f32,
    /// 情绪标签（渲染在括号里，让模型分得清「温暖」的旧事）。
    pub emotion: Option<String>,
    /// 关联标签，形如 `topic:便签` / `person:小雨` / `place:图书馆`。
    pub links: Vec<String>,
    /// 所属剧情线，如 `thread.周五还书`（可选）。
    pub thread: Option<String>,
    /// 来源：hearsay 时为转述人（`10.4）；写入侧另有 `hook.on_message` 等。
    pub source: String,
    /// 写入时间戳（unix 秒）。
    pub ts: u64,
    /// 再提及次数：被召回后由 rehearse 累加，构成再提及加成。
    pub rehearsals: u32,
}

impl MemObject {
    /// 视角可见者：witnesses 为空时取 actors（设计 `5.2「默认 = actors」）。
    ///
    /// 两者都为空表示「无见证信息」，此时**没有**任何具名视角能通过过滤
    /// （M1 的 fact 旧记录就属于这种，它们走 C2 事实槽与检索/视图，不进 B4 召回）。
    pub fn witnesses_or_actors(&self) -> Vec<String> {
        if self.witnesses.is_empty() {
            self.actors.clone()
        } else {
            self.witnesses.clone()
        }
    }

    /// link 命中判定：`topic:便签` / `topic` / `便签` 三种写法都能命中 `topic:便签`。
    ///
    /// 比较前 trim + 大小写不敏感 + 全角冒号归一；带命名空间的 needle（含 `:`）
    /// 要求整体相等，不带命名空间的 needle 则允许命中 link 的键或值。
    pub fn links_match(&self, needle: &str) -> bool {
        let needle = normalize_tag(needle);
        if needle.is_empty() {
            return false;
        }
        let needle_has_ns = needle.contains(':');
        self.links.iter().any(|raw| {
            let link = normalize_tag(raw);
            if link == needle {
                return true;
            }
            if needle_has_ns {
                return false; // 带命名空间必须整体相等，避免 `topic:便签` 命中 `topic:便签旧`
            }
            let (key, value) = split_link(&link);
            key == needle || value == Some(needle.as_str())
        })
    }
}

/// 反序列化用的宽松形态：字段全可选 + 嵌套 time + M1 fact 的 key/value。
#[derive(Deserialize)]
struct MemObjectWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<serde_json::Value>,
    #[serde(default)]
    turn: Option<u64>,
    #[serde(default)]
    story_day: Option<i64>,
    #[serde(default)]
    story_clock: Option<String>,
    #[serde(default)]
    time: Option<MemTimeWire>,
    #[serde(default)]
    place: Option<String>,
    #[serde(default)]
    actors: Option<Vec<String>>,
    #[serde(default)]
    witnesses: Option<Vec<String>>,
    #[serde(default)]
    salience: Option<f32>,
    #[serde(default)]
    emotion: Option<String>,
    #[serde(default)]
    links: Option<Vec<String>>,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    ts: Option<u64>,
    #[serde(default)]
    rehearsals: Option<u32>,
}

/// 设计 `5.2 示例里的嵌套时间形态。
#[derive(Deserialize)]
struct MemTimeWire {
    #[serde(default)]
    turn: Option<u64>,
    #[serde(default)]
    story_day: Option<i64>,
    #[serde(default)]
    story_clock: Option<String>,
}

impl From<MemObjectWire> for MemObject {
    fn from(w: MemObjectWire) -> Self {
        let kind = match w.kind.as_deref().map(str::trim) {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => KIND_EPISODE.to_string(),
        };
        let content_given = w
            .content
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        // M1 的 fact 记录：没有 content，只有 key/value。
        let legacy = !content_given && w.key.is_some();
        let key = w.key.as_deref().unwrap_or("");
        let story_clock = w
            .story_clock
            .clone()
            .or_else(|| w.time.as_ref().and_then(|t| t.story_clock.clone()))
            .unwrap_or_default();
        MemObject {
            id: w.id.unwrap_or_default(),
            kind,
            content: if legacy {
                legacy_content(key, w.value.as_ref().unwrap_or(&serde_json::Value::Null))
            } else {
                w.content.unwrap_or_default()
            },
            turn: w
                .turn
                .or_else(|| w.time.as_ref().and_then(|t| t.turn))
                .unwrap_or(0),
            story_day: w
                .story_day
                .or_else(|| w.time.as_ref().and_then(|t| t.story_day))
                .or_else(|| parse_day_from_clock(&story_clock))
                .unwrap_or(0),
            story_clock,
            place: w.place,
            actors: w.actors.unwrap_or_default(),
            witnesses: w.witnesses.unwrap_or_default(),
            salience: w.salience.unwrap_or(if legacy {
                LEGACY_FACT_SALIENCE
            } else {
                DEFAULT_SALIENCE
            }),
            emotion: w.emotion,
            links: if legacy {
                legacy_links(key)
            } else {
                w.links.unwrap_or_default()
            },
            thread: w.thread,
            source: w.source.unwrap_or_default(),
            ts: w.ts.unwrap_or(0),
            rehearsals: w.rehearsals.unwrap_or(0),
        }
    }
}

impl<'de> Deserialize<'de> for MemObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(MemObjectWire::deserialize(d)?.into())
    }
}

/// 下一条记忆 id：`mem_0001`（n 从 1 起；不足四位左补零，超过四位自然增长）。
pub fn next_id(n: usize) -> String {
    format!("mem_{n:04}")
}

/// M1 旧 fact 记录 → 记忆对象（读侧兼容，不破坏旧 `palace.jsonl`）。
///
/// - kind = fact，content = `key：value`（字符串值原样，其余取 JSON 紧凑文本）；
/// - links 补一条 `topic:<key>`，让状态树 recall 提示与最近提及仍能命中旧事实；
/// - story_day = 0 / story_clock 为空：旧记录只记轮次，不知道故事天，
///   宿主若能把该 turn 换算成故事天，可自行覆写这两个字段。
pub fn from_legacy_fact(
    key: &str,
    value: &serde_json::Value,
    source: &str,
    turn: u64,
    ts: u64,
) -> MemObject {
    MemObject {
        id: String::new(),
        kind: KIND_FACT.to_string(),
        content: legacy_content(key, value),
        turn,
        story_day: 0,
        story_clock: String::new(),
        place: None,
        actors: Vec::new(),
        witnesses: Vec::new(),
        salience: LEGACY_FACT_SALIENCE,
        emotion: None,
        links: legacy_links(key),
        thread: None,
        source: source.to_string(),
        ts,
        rehearsals: 0,
    }
}

/// 旧 fact 的正文文本：字符串值去引号，其余取 JSON 紧凑文本。
fn legacy_content(key: &str, value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let key = key.trim();
    if key.is_empty() {
        text
    } else {
        format!("{key}：{text}")
    }
}

/// 旧 fact 的合成关联：`topic:<key>`。
fn legacy_links(key: &str) -> Vec<String> {
    let key = key.trim();
    if key.is_empty() {
        Vec::new()
    } else {
        vec![format!("topic:{key}")]
    }
}

// ---------- 召回查询与打分 ----------

/// 一次召回请求：宿主每轮把「视角 + 最近窗口提及 + 当前地点/在场者 + 状态树 recall
/// 提示 + 活跃剧情线」交给召回（设计 `5.4）。
///
/// Default 即「全视角、不限量、不限预算」，方便面板/调试直接查全部。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallQuery {
    /// 当前组装视角（角色名）；空串 = 不作视角过滤（面板/调试用）。
    pub viewer: String,
    /// 当前故事天，用于时间衰减。
    pub now_day: i64,
    /// 当前地点（命中 place: link 或 place 字段即加成）。
    pub place: Option<String>,
    /// 在场者（命中 person: link 加成；与 actors 有交集再加成）。
    pub present: Vec<String>,
    /// 最近窗口提及的实体/话题（命中 link 加成）。
    pub mentions: Vec<String>,
    /// 状态树 recall 提示，如 `room:图书馆` / `topic:过去`（命中 link 加成，权重最高）。
    pub hints: Vec<String>,
    /// 活跃剧情线 id（记忆的 thread 命中即加成）。
    pub active_threads: Vec<String>,
    /// 取前 K 条；0 = 不限量。
    pub top_k: usize,
    /// 注入预算（token）；按序累积 estimate_tokens(渲染行)，超预算即停，
    /// 但至少保留 1 条。0 = 不限预算。
    pub budget_tokens: usize,
}

/// 一条召回结果：记忆 + 分数 + 中文激活原因（界面「激活原因」直接显示）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    pub mem: MemObject,
    pub score: f32,
    pub reasons: Vec<String>,
}

impl RecallHit {
    /// 该条注入后占用的 B4 预算（与 render_memory_block 的实际行一致）。
    pub fn tokens(&self) -> usize {
        estimate_tokens(&render_memory_line(&self.mem))
    }
}

/// 召回：视角过滤 → 打分 → 排序（score 降序，同分按 (story_day, turn, id) 升序）
/// → top-K → token 预算截断（至少保留 1 条）。
///
/// 全程无哈希迭代顺序参与决策，同样的输入必然得到同样的输出（`7.3）。
pub fn recall(objs: &[MemObject], q: &RecallQuery) -> Vec<RecallHit> {
    let viewer = normalize_tag(&q.viewer);
    let mut hits: Vec<RecallHit> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    for m in objs {
        // 视角过滤是硬约束（`10.4）：只召回 witnesses 含当前角色的记忆。
        if !viewer.is_empty() && !contains_tag(&m.witnesses_or_actors(), &viewer) {
            continue;
        }
        // B4 内部条目互斥去重（`4.1）：同一条记忆（非空 id）只出现一次。
        let id = m.id.trim();
        if !id.is_empty() {
            if seen_ids.iter().any(|s| s == id) {
                continue;
            }
            seen_ids.push(id.to_string());
        }
        let (score, reasons) = score_of(m, q, &viewer);
        hits.push(RecallHit {
            mem: m.clone(),
            score,
            reasons,
        });
    }

    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.mem.story_day.cmp(&b.mem.story_day))
            .then_with(|| a.mem.turn.cmp(&b.mem.turn))
            .then_with(|| a.mem.id.cmp(&b.mem.id))
    });

    if q.top_k > 0 {
        hits.truncate(q.top_k);
    }
    if q.budget_tokens > 0 {
        let mut used = 0usize;
        let mut keep = 0usize;
        for h in &hits {
            let cost = h.tokens();
            if keep > 0 && used + cost > q.budget_tokens {
                break;
            }
            used += cost;
            keep += 1;
        }
        hits.truncate(keep);
    }
    hits
}

/// 单条记忆的打分与激活原因。viewer 已归一化（空串 = 无视角）。
fn score_of(m: &MemObject, q: &RecallQuery, viewer: &str) -> (f32, Vec<String>) {
    let decay = decay_factor((q.now_day - m.story_day) as f64);

    let hints = dedup_tags(&q.hints);
    let present = dedup_tags(&q.present);
    let mentions = dedup_tags(&q.mentions);
    let threads = dedup_tags(&q.active_threads);
    let place_norm = normalize_tag(q.place.as_deref().unwrap_or(""));

    let mut relevance = 1.0f64;
    let mut reasons: Vec<String> = Vec::new();
    if !viewer.is_empty() {
        reasons.push(format!("视角:{}", q.viewer.trim()));
    }

    // 状态树 recall 提示（「回到事发地点才想起那件事」，`5.4）。
    for (norm, display) in &hints {
        if m.links_match(norm) {
            relevance += HINT_LINK_WEIGHT;
            reasons.push(format!("提示:{display}"));
        }
    }
    // 当前地点：place 字段或 place: link 命中都算（记忆自带的 place 是权威值）。
    if !place_norm.is_empty()
        && (normalize_tag(m.place.as_deref().unwrap_or("")) == place_norm
            || m.links_match(&format!("place:{place_norm}")))
    {
        relevance += PLACE_LINK_WEIGHT;
        reasons.push(format!("关联地点:{}", q.place.as_deref().unwrap_or("").trim()));
    }
    // 在场者：person: link。
    for (norm, display) in &present {
        if m.links_match(&format!("person:{norm}")) {
            relevance += PRESENT_LINK_WEIGHT;
            reasons.push(format!("在场者:{display}"));
        }
    }
    // 最近窗口提及的实体/话题。
    for (norm, display) in &mentions {
        if m.links_match(norm) {
            relevance += MENTION_LINK_WEIGHT;
            reasons.push(format!("提及:{display}"));
        }
    }
    // 活跃剧情线关联。
    if let Some(t) = m.thread.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let tn = normalize_tag(t);
        if threads.iter().any(|(n, _)| n == &tn) || m.links_match(&format!("thread:{tn}")) {
            relevance += ACTIVE_THREAD_WEIGHT;
            reasons.push(format!("活跃线:{t}"));
        }
    }
    // actors 与在场者有交集（记一次）。
    let co: Vec<&str> = m
        .actors
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty() && present.iter().any(|(n, _)| normalize_tag(a) == n.as_str()))
        .collect();
    if !co.is_empty() {
        relevance += CO_ACTOR_WEIGHT;
        reasons.push(format!("同场:{}", co.join(",")));
    }
    let relevance = relevance.min(RELEVANCE_CAP);

    let remention = if m.rehearsals > 0 {
        let boosted = (1.0 + REMENTION_STEP * m.rehearsals as f64).min(REMENTION_CAP);
        reasons.push(format!("再提及×{}", m.rehearsals));
        boosted
    } else {
        1.0
    };

    let score = (sanitize_salience(m.salience) as f64 * decay * relevance * remention) as f32;
    (score, reasons)
}

/// 故事时间衰减系数：0.5^(Δ/HALF_LIFE_DAYS)，Δ 为负按 0（未来时间不倒扣）。
/// 打分层与 decay_all 共用这一处公式，避免两条路径漂移。
fn decay_factor(delta_days: f64) -> f64 {
    0.5f64.powf(delta_days.max(0.0) / HALF_LIFE_DAYS)
}

/// 时钟步进 / 睡眠整理时的批量衰减（设计 `5.4）：salience 乘上
/// 0.5^(elapsed/HALF_LIFE_DAYS)，返回被衰减的条数。
///
/// - elapsed_story_days <= 0（或 NaN）不动任何记忆，返回 0；
/// - 只缩 salience，**不挪 story_day**：story_day 是事件时间戳，时间线视图要靠它；
/// - 与 recall 的即时衰减不要叠加使用（见模块头注释）。
pub fn decay_all(objs: &mut [MemObject], elapsed_story_days: f64) -> usize {
    if !(elapsed_story_days > 0.0) {
        return 0;
    }
    let factor = decay_factor(elapsed_story_days) as f32;
    let mut decayed = 0usize;
    for m in objs.iter_mut() {
        let before = sanitize_salience(m.salience);
        let after = (before * factor).clamp(0.0, 1.0);
        if after < before || !m.salience.is_finite() {
            m.salience = after;
            decayed += 1;
        }
    }
    decayed
}

/// 再提及增强（设计 `5.4「被再次提及则回升」）：把命中记忆的 rehearsals 各 +1，
/// 返回被增强的条数（同一条记忆在一次调用里最多 +1，即使 id 在 hit_ids 里重复）。
///
/// 只加计数不直接改 salience——回升通过打分层里的再提及加成体现，
/// 这样 salience 始终是「写入侧给的初值 × 衰减」，可回放、可解释。
pub fn rehearse(objs: &mut [MemObject], hit_ids: &[String]) -> usize {
    let mut wanted: Vec<&str> = hit_ids
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut boosted = 0usize;
    for m in objs.iter_mut() {
        let id = m.id.trim();
        if id.is_empty() || wanted.binary_search(&id).is_err() {
            continue;
        }
        if m.rehearsals == u32::MAX {
            continue;
        }
        m.rehearsals += 1;
        boosted += 1;
    }
    boosted
}

// ---------- B4 槽渲染（设计 `4.1）----------

/// 渲染 B4 回忆块：每行一条 `【回忆·第3天 23:40】正文（情绪·显著度）`，
/// hearsay 追加「转述自X」。整体不加外层标签（宿主 prompt.rs 负责包 <memory>）；
/// 无命中返回空串，不产生空标签（`4.3）。
pub fn render_memory_block(hits: &[RecallHit]) -> String {
    hits.iter()
        .map(|h| render_memory_line(&h.mem))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 单条回忆行（recall 的预算也按它计 token，保证「预算 → 实际注入」口径一致）。
fn render_memory_line(m: &MemObject) -> String {
    let salience = sanitize_salience(m.salience);
    let emotion = m.emotion.as_deref().map(str::trim).filter(|e| !e.is_empty());
    let mut meta = match emotion {
        Some(e) => format!("{e}·{salience:.2}"),
        None => format!("显著度{salience:.2}"),
    };
    if m.kind.trim().eq_ignore_ascii_case(KIND_HEARSAY) {
        let src = m.source.trim();
        if src.is_empty() {
            meta.push_str("；转述");
        } else {
            meta.push_str(&format!("；转述自{src}"));
        }
    }
    // 正文里的换行压成空格，保证「一行一条」的格式不被破坏。
    let content = m.content.replace("\r\n", " ").replace('\n', " ").replace('\r', " ");
    let content = content.trim();
    format!("【回忆·{}】{content}（{meta}）", story_stamp(m))
}

/// 故事时间戳：story_clock 已含「第N天」时原样用，否则拼上 story_day。
fn story_stamp(m: &MemObject) -> String {
    let clock = m.story_clock.trim();
    if clock.is_empty() {
        return format!("第{}天", m.story_day);
    }
    if clock.starts_with('第') {
        return clock.to_string();
    }
    format!("第{}天 {}", m.story_day, clock)
}

// ---------- 面板视图数据（设计 `5.5：房间图 / 时间线 / 关联图）----------

/// 面板用的记忆摘要：只带画一张卡片需要的字段（正文 + 时间 + 权重 + 溯源）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemBrief {
    pub id: String,
    pub content: String,
    pub turn: u64,
    pub story_day: i64,
    pub story_clock: String,
    pub salience: f32,
    pub emotion: Option<String>,
    pub place: Option<String>,
    pub source: String,
}

/// 记忆 → 面板摘要。
pub fn brief(m: &MemObject) -> MemBrief {
    MemBrief {
        id: m.id.clone(),
        content: m.content.clone(),
        turn: m.turn,
        story_day: m.story_day,
        story_clock: m.story_clock.clone(),
        salience: sanitize_salience(m.salience),
        emotion: m.emotion.clone(),
        place: m.place.clone(),
        source: m.source.clone(),
    }
}

/// 房间视图：一个故事地点 = 一间房（设计 `5.2 / `5.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomView {
    pub place: String,
    /// 该房间的记忆条数。
    pub count: usize,
    /// 房间里最显眼的几条（salience 降序，同分按 (story_day, turn, id) 升序）。
    pub top: Vec<MemBrief>,
}

/// 时间线视图：一条走廊按故事天分桶。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineBucket {
    /// 桶标签，如 `第3天`。
    pub label: String,
    pub count: usize,
    pub top: Vec<MemBrief>,
}

/// 关联图：节点是 link 标签，边是同一条记忆里的共现（权重 = 共现次数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkGraph {
    /// 节点（展示用原文写法）：按出现频次降序，同频按名字升序。
    pub nodes: Vec<String>,
    /// 边 (a, b, 共现次数)：a < b（归一化升序，无向边只有一种写法）；
    /// 整体按次数降序，同次按 (a, b) 升序。
    pub edges: Vec<(String, String, usize)>,
}

/// 房间图：按 place 分组（place 为空的记忆不进房间图，仍在时间线与检索里）。
/// 房间按条数降序、同条数按地点名升序；每间最多带 VIEW_TOP_N 条摘要。
pub fn rooms(objs: &[MemObject]) -> Vec<RoomView> {
    let mut map: Vec<(String, String, Vec<&MemObject>)> = Vec::new(); // (norm, display, mems)
    for m in objs {
        let place = m.place.as_deref().map(str::trim).filter(|p| !p.is_empty());
        let Some(place) = place else { continue };
        let norm = normalize_tag(place);
        match map.iter_mut().find(|(n, _, _)| n == &norm) {
            Some(slot) => slot.2.push(m),
            None => map.push((norm, place.to_string(), vec![m])),
        }
    }
    let mut out: Vec<RoomView> = map
        .into_iter()
        .map(|(_, place, mut mems)| {
            let count = mems.len();
            sort_by_rank(&mut mems);
            RoomView {
                place,
                count,
                top: mems.into_iter().take(VIEW_TOP_N).map(brief).collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.place.cmp(&b.place)));
    out
}

/// 时间线走廊：按 story_day 升序分桶（第1天 → 第N天），每桶最多 VIEW_TOP_N 条摘要。
pub fn timeline(objs: &[MemObject]) -> Vec<TimelineBucket> {
    let mut days: Vec<(i64, Vec<&MemObject>)> = Vec::new();
    for m in objs {
        match days.iter_mut().find(|(d, _)| *d == m.story_day) {
            Some((_, bucket)) => bucket.push(m),
            None => days.push((m.story_day, vec![m])),
        }
    }
    days.sort_by_key(|(d, _)| *d);
    days.into_iter()
        .map(|(day, mut mems)| {
            let count = mems.len();
            sort_by_rank(&mut mems);
            TimelineBucket {
                label: format!("第{day}天"),
                count,
                top: mems.into_iter().take(VIEW_TOP_N).map(brief).collect(),
            }
        })
        .collect()
}

/// 关联图：links 的共现图（设计 `5.2「links 构成关联图」）。
///
/// 节点按频次降序（同频按归一化名字升序），边按共现次数降序（同次按 (a, b) 升序）；
/// 展示用首次出现的原文写法，比较用归一化写法（大小写 / 全角冒号不敏感）。
pub fn link_graph(objs: &[MemObject]) -> LinkGraph {
    let mut nodes: Vec<(String, String, usize)> = Vec::new(); // (norm, display, freq)
    let mut edges: Vec<(String, String, usize)> = Vec::new();
    for m in objs {
        let tags = dedup_tags(&m.links);
        for (norm, display) in &tags {
            match nodes.iter_mut().find(|(n, _, _)| n == norm) {
                Some(slot) => slot.2 += 1,
                None => nodes.push((norm.clone(), display.clone(), 1)),
            }
        }
        for i in 0..tags.len() {
            for j in (i + 1)..tags.len() {
                let (a, b) = if tags[i].0 <= tags[j].0 {
                    (&tags[i].0, &tags[j].0)
                } else {
                    (&tags[j].0, &tags[i].0)
                };
                match edges.iter_mut().find(|(x, y, _)| x == a && y == b) {
                    Some(slot) => slot.2 += 1,
                    None => edges.push((a.clone(), b.clone(), 1)),
                }
            }
        }
    }
    nodes.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    let display_of = |norm: &str| -> String {
        nodes
            .iter()
            .find(|(n, _, _)| n == norm)
            .map(|(_, d, _)| d.clone())
            .unwrap_or_else(|| norm.to_string())
    };
    let mut out_edges: Vec<(String, String, usize)> = edges
        .iter()
        .map(|(a, b, c)| (display_of(a), display_of(b), *c))
        .collect();
    out_edges.sort_by(|x, y| {
        y.2.cmp(&x.2)
            .then_with(|| x.0.cmp(&y.0))
            .then_with(|| x.1.cmp(&y.1))
    });

    LinkGraph {
        nodes: nodes.into_iter().map(|(_, d, _)| d).collect(),
        edges: out_edges,
    }
}

/// 检索（面板搜索框）：在正文 / id / 关联 / 剧情线 / 地点 / 人物 / 情绪里做
/// 大小写不敏感的子串匹配，按显眼程度排序（salience 降序，同分按 (story_day, turn, id) 升序）。
/// 空 needle 返回空列表。
pub fn search(objs: &[MemObject], needle: &str) -> Vec<MemBrief> {
    let needle = normalize_tag(needle);
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<&MemObject> = objs
        .iter()
        .filter(|m| {
            m.links_match(&needle)
                || m.links.iter().any(|l| normalize_tag(l).contains(&needle))
                || normalize_tag(&m.content).contains(&needle)
                || normalize_tag(&m.id).contains(&needle)
                || normalize_tag(&m.story_clock).contains(&needle)
                || m.place
                    .as_deref()
                    .is_some_and(|p| normalize_tag(p).contains(&needle))
                || m.thread
                    .as_deref()
                    .is_some_and(|t| normalize_tag(t).contains(&needle))
                || m.emotion
                    .as_deref()
                    .is_some_and(|e| normalize_tag(e).contains(&needle))
                || m.actors.iter().any(|a| normalize_tag(a).contains(&needle))
                || m.witnesses.iter().any(|w| normalize_tag(w).contains(&needle))
        })
        .collect();
    sort_by_rank(&mut found);
    found.into_iter().map(brief).collect()
}

// ---------- 内部工具 ----------

/// 归一化标签：trim + 全角冒号 → 半角 + 小写（中文不受影响，ASCII 标签大小写不敏感）。
fn normalize_tag(raw: &str) -> String {
    raw.trim().replace('：', ":").to_lowercase()
}

/// 拆分「命名空间:值」（只按第一个冒号拆）。
fn split_link(link: &str) -> (&str, Option<&str>) {
    match link.split_once(':') {
        Some((k, v)) => (k, Some(v)),
        None => (link, None),
    }
}

/// 标签列表去重：返回 (归一化, 展示原文) 对，保持输入次序。
fn dedup_tags(items: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in items {
        let display = raw.trim();
        if display.is_empty() {
            continue;
        }
        let norm = normalize_tag(display);
        if norm.is_empty() || out.iter().any(|(n, _)| n == &norm) {
            continue;
        }
        out.push((norm, display.to_string()));
    }
    out
}

/// 归一化后是否命中标签列表。
fn contains_tag(items: &[String], norm: &str) -> bool {
    items.iter().any(|i| normalize_tag(i) == norm)
}

/// 显著度清洗：非有限值按 0，并夹到 0–1。
fn sanitize_salience(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 视图排序：salience 降序，同分按 (story_day, turn, id) 升序（与召回同一套确定性口径）。
fn sort_by_rank(mems: &mut [&MemObject]) {
    mems.sort_by(|a, b| {
        sanitize_salience(b.salience)
            .total_cmp(&sanitize_salience(a.salience))
            .then_with(|| a.story_day.cmp(&b.story_day))
            .then_with(|| a.turn.cmp(&b.turn))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// 从 `第3天 23:40` 这类故事时钟里解出故事天（读侧兼容设计 `5.2 的嵌套 time 形态）。
fn parse_day_from_clock(clock: &str) -> Option<i64> {
    let s = clock.trim();
    let start = s.find('第')? + '第'.len_utf8();
    let rest = &s[start..];
    let end = rest.find('天')?;
    rest[..end].trim().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 测试用记忆：其余字段给中位数默认值，各用例只改自己关心的。
    fn mem(id: &str, day: i64, turn: u64, content: &str) -> MemObject {
        MemObject {
            id: id.to_string(),
            kind: KIND_EPISODE.to_string(),
            content: content.to_string(),
            turn,
            story_day: day,
            story_clock: String::new(),
            place: None,
            actors: Vec::new(),
            witnesses: Vec::new(),
            salience: 0.8,
            emotion: None,
            links: Vec::new(),
            thread: None,
            source: String::new(),
            ts: 0,
            rehearsals: 0,
        }
    }

    fn query(viewer: &str, now_day: i64) -> RecallQuery {
        RecallQuery {
            viewer: viewer.to_string(),
            now_day,
            ..Default::default()
        }
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "期望 {b}，实际 {a}");
    }

    fn ids(hits: &[RecallHit]) -> Vec<String> {
        hits.iter().map(|h| h.mem.id.clone()).collect()
    }

    // ---------- 时间衰减与半衰期 ----------

    #[test]
    fn decay_follows_half_life() {
        let mut m = mem("mem_0001", 1, 1, "深夜送便签");
        m.salience = 0.8;
        m.witnesses = vec!["小雨".into()];

        // Δ = 7 天 → 权重减半
        let hits = recall(&[m.clone()], &query("小雨", 8));
        approx(hits[0].score, 0.4);

        // Δ = 3 天 → 0.5^(3/7)
        let hits = recall(&[m.clone()], &query("小雨", 4));
        approx(hits[0].score, (0.8f64 * 0.5f64.powf(3.0 / 7.0)) as f32);

        // Δ = 0 → 完全不衰减
        let hits = recall(&[m], &query("小雨", 1));
        approx(hits[0].score, 0.8);

        assert_eq!(decay_factor(7.0), 0.5);
        approx(decay_factor(3.0) as f32, (0.5f64.powf(3.0 / 7.0)) as f32);
    }

    #[test]
    fn decay_clamps_negative_delta() {
        // now_day 早于 story_day（未来记忆）不倒扣，按 Δ = 0 处理
        let m = mem("mem_0001", 9, 1, "还没发生的事");
        let hits = recall(&[m], &query("", 3));
        approx(hits[0].score, 0.8);
        assert_eq!(decay_factor(-5.0), 1.0);
    }

    #[test]
    fn decay_all_scales_salience_and_counts() {
        let mut objs = vec![
            mem("mem_0001", 1, 1, "a"),
            mem("mem_0002", 2, 2, "b"),
            mem("mem_0003", 3, 3, "c"),
        ];
        for o in objs.iter_mut() {
            o.salience = 0.8;
        }
        assert_eq!(decay_all(&mut objs, 0.0), 0, "0 天不衰减");
        assert_eq!(decay_all(&mut objs, -3.0), 0, "负数不衰减");
        assert_eq!(decay_all(&mut objs, 7.0), 3, "7 天 = 半衰期");
        for o in &objs {
            approx(o.salience, 0.4);
        }
        // story_day 是事件时间戳，不随时钟挪动（时间线视图要靠它）
        assert_eq!(objs[0].story_day, 1);
        assert_eq!(decay_all(&mut objs, 7.0), 3);
        approx(objs[0].salience, 0.2);
    }

    // ---------- 视角过滤（`10.4 硬约束）----------

    #[test]
    fn viewpoint_filter_hides_other_people_memories() {
        let mut own = mem("mem_0001", 1, 1, "我经历过");
        own.witnesses = vec!["小雨".into()];
        let mut others = mem("mem_0002", 1, 2, "只有阿澈知道");
        others.witnesses = vec!["阿澈".into()];
        let mut via_actors = mem("mem_0003", 1, 3, "witnesses 为空 → 取 actors");
        via_actors.actors = vec!["小雨".into(), "玩家".into()];
        let mut actor_only_other = mem("mem_0004", 1, 4, "阿澈的独处");
        actor_only_other.actors = vec!["阿澈".into()];
        let objs = vec![own, others, via_actors, actor_only_other];

        let hits = recall(&objs, &query("小雨", 10));
        assert_eq!(ids(&hits), vec!["mem_0001", "mem_0003"]);
        assert!(hits[0].reasons.contains(&"视角:小雨".to_string()));

        // 阿澈视角反过来看不到小雨的经历；玩家视角同理
        let hits = recall(&objs, &query("阿澈", 10));
        assert_eq!(ids(&hits), vec!["mem_0002", "mem_0004"]);
        // 玩家只亲身经历过 witnesses 为空的第三条（witnesses → actors 兜底）
        assert_eq!(ids(&recall(&objs, &query("玩家", 10))), vec!["mem_0003"]);
    }

    #[test]
    fn viewpoint_compare_ignores_case_and_spaces() {
        let mut m = mem("mem_0001", 1, 1, "x");
        m.witnesses = vec!["  XiaoYu ".into()];
        assert_eq!(recall(&[m.clone()], &query("xiaoyu", 1)).len(), 1);
        assert_eq!(recall(&[m.clone()], &query(" XIAOYU ", 1)).len(), 1);
        assert_eq!(recall(&[m], &query("小雨", 1)).len(), 0);
    }

    #[test]
    fn empty_viewer_sees_everything() {
        let mut a = mem("mem_0001", 1, 1, "a");
        a.witnesses = vec!["小雨".into()];
        let b = mem("mem_0002", 1, 2, "b");
        let hits = recall(&[a, b], &query("", 1));
        assert_eq!(hits.len(), 2, "空 viewer = 面板/调试的全视角");
        assert!(hits
            .iter()
            .all(|h| !h.reasons.iter().any(|r| r.starts_with("视角:"))));
    }

    // ---------- 四类关联命中（`5.4）----------

    #[test]
    fn relevance_hint_hit() {
        let mut m = mem("mem_0001", 1, 1, "便签的事");
        m.links = vec!["topic:便签".into()];
        m.witnesses = vec!["小雨".into()];
        let q = RecallQuery {
            viewer: "小雨".into(),
            now_day: 1,
            hints: vec!["topic:便签".into()],
            ..Default::default()
        };
        // 0.8 × 1.0 × (1 + 0.6)
        let hits = recall(&[m], &q);
        approx(hits[0].score, 1.28);
        assert!(hits[0].reasons.contains(&"提示:topic:便签".to_string()));
    }

    #[test]
    fn relevance_place_hit() {
        let mut by_field = mem("mem_0001", 1, 1, "闭馆");
        by_field.place = Some("图书馆".into());
        let mut by_link = mem("mem_0002", 1, 2, "门前");
        by_link.links = vec!["place:图书馆".into()];
        let q = RecallQuery {
            viewer: String::new(),
            now_day: 1,
            place: Some("图书馆".into()),
            ..Default::default()
        };
        // 0.8 × 1.0 × (1 + 0.5)
        let hits = recall(&[by_field, by_link], &q);
        assert_eq!(hits.len(), 2);
        for h in &hits {
            approx(h.score, 1.2);
            assert!(h.reasons.contains(&"关联地点:图书馆".to_string()));
        }
        // 不在同一地点 → 无加成
        let elsewhere = RecallQuery {
            place: Some("操场".into()),
            ..q
        };
        let hits = recall(&[mem("mem_0003", 1, 3, "x")], &elsewhere);
        approx(hits[0].score, 0.8);
        assert!(hits[0].reasons.is_empty());
    }

    #[test]
    fn relevance_present_person_hit() {
        let mut m = mem("mem_0001", 1, 1, "她递来便签");
        m.links = vec!["person:小雨".into()];
        let q = RecallQuery {
            now_day: 1,
            present: vec!["小雨".into(), "玩家".into()],
            ..Default::default()
        };
        // 0.8 × 1.0 × (1 + 0.4)
        let hits = recall(&[m], &q);
        approx(hits[0].score, 1.12);
        assert!(hits[0].reasons.contains(&"在场者:小雨".to_string()));
        assert!(!hits[0].reasons.iter().any(|r| r.contains("玩家")));
    }

    #[test]
    fn relevance_mention_hit() {
        let mut m = mem("mem_0001", 1, 1, "便签的事");
        m.links = vec!["topic:便签".into()];
        let q = RecallQuery {
            now_day: 1,
            mentions: vec!["便签".into()],
            ..Default::default()
        };
        // 0.8 × 1.0 × (1 + 0.3)
        let hits = recall(&[m], &q);
        approx(hits[0].score, 1.04);
        assert!(hits[0].reasons.contains(&"提及:便签".to_string()));
    }

    #[test]
    fn relevance_thread_and_co_actors() {
        let mut m = mem("mem_0001", 1, 1, "还书之约");
        m.thread = Some("thread.周五还书".into());
        m.actors = vec!["小雨".into(), "玩家".into()];
        let q = RecallQuery {
            now_day: 1,
            present: vec!["小雨".into()],
            active_threads: vec!["thread.周五还书".into()],
            ..Default::default()
        };
        // 0.8 × 1.0 × (1 + 0.5 活跃线 + 0.2 同场)
        let hits = recall(&[m], &q);
        approx(hits[0].score, 1.36);
        assert!(hits[0]
            .reasons
            .contains(&"活跃线:thread.周五还书".to_string()));
        assert!(hits[0].reasons.contains(&"同场:小雨".to_string()));
    }

    #[test]
    fn relevance_bonus_is_capped() {
        let mut m = mem("mem_0001", 1, 1, "到处都关联得上");
        m.links = vec![
            "topic:便签".into(),
            "person:小雨".into(),
            "place:图书馆".into(),
        ];
        m.actors = vec!["小雨".into()];
        m.thread = Some("thread.周五还书".into());
        let q = RecallQuery {
            viewer: "小雨".into(),
            now_day: 1,
            place: Some("图书馆".into()),
            present: vec!["小雨".into()],
            mentions: vec!["便签".into(), "便签".into()],
            hints: vec!["topic:便签".into()],
            active_threads: vec!["thread.周五还书".into()],
            ..Default::default()
        };
        // Σ 权重 = 0.6 + 0.5 + 0.4 + 0.3 + 0.5 + 0.2 = 2.5 → 3.5，被 RELEVANCE_CAP=3.0 截住
        let hits = recall(&[m], &q);
        assert_eq!(hits[0].score, 0.8 * RELEVANCE_CAP as f32);
        // 重复的 mention 不重复计权
        assert_eq!(
            hits[0]
                .reasons
                .iter()
                .filter(|r| r.starts_with("提及:"))
                .count(),
            1
        );
    }

    // ---------- 再提及增强 ----------

    #[test]
    fn remention_bonus_and_cap() {
        let mut m = mem("mem_0001", 1, 1, "被提过的事");
        assert_eq!(recall(&[m.clone()], &query("", 1))[0].score, 0.8);

        m.rehearsals = 3;
        let hits = recall(&[m.clone()], &query("", 1));
        approx(hits[0].score, (0.8f64 * 1.3) as f32);
        assert!(hits[0].reasons.contains(&"再提及×3".to_string()));

        m.rehearsals = 20; // 1 + 0.1×20 = 3.0 → 封顶 1.5
        approx(recall(&[m], &query("", 1))[0].score, 1.2);
    }

    #[test]
    fn rehearse_increments_once_per_memory() {
        let mut objs = vec![
            mem("mem_0001", 1, 1, "a"),
            mem("mem_0002", 1, 2, "b"),
            mem("mem_0003", 1, 3, "c"),
        ];
        let ids = vec![
            "mem_0001".to_string(),
            " mem_0001 ".to_string(),
            "mem_0003".to_string(),
        ];
        assert_eq!(rehearse(&mut objs, &ids), 2);
        assert_eq!(objs[0].rehearsals, 1, "同一次调用里重复 id 只 +1");
        assert_eq!(objs[1].rehearsals, 0);
        assert_eq!(objs[2].rehearsals, 1);

        // 召回结果可以直接喂回 rehearse（宿主「这轮真提了」的回路）
        let hits = recall(&objs, &query("", 1));
        let hit_ids: Vec<String> = hits.iter().map(|h| h.mem.id.clone()).collect();
        assert_eq!(rehearse(&mut objs, &hit_ids), 3);
        assert_eq!(objs[1].rehearsals, 1);
        assert_eq!(rehearse(&mut objs, &["mem_9999".to_string()]), 0);
    }

    // ---------- 读侧兼容（M1 fact 与设计 `5.2 的 time 形态）----------

    #[test]
    fn legacy_fact_from_parts() {
        let m = from_legacy_fact("last_thanked", &json!(3), "hook.on_message", 7, 1712345678);
        assert_eq!(m.kind, KIND_FACT);
        assert_eq!(m.content, "last_thanked：3");
        assert_eq!(m.source, "hook.on_message");
        assert_eq!(m.turn, 7);
        assert_eq!(m.ts, 1712345678);
        assert_eq!(m.salience, LEGACY_FACT_SALIENCE);
        assert_eq!(m.links, vec!["topic:last_thanked".to_string()]);
        assert!(m.links_match("last_thanked"), "旧事实仍能被状态树提示/提及命中");
        assert!(m.id.is_empty(), "id 由宿主用 next_id 分配");
        assert!(m.witnesses_or_actors().is_empty());

        let s = from_legacy_fact("玩家名", &json!("阿澈"), "", 1, 0);
        assert_eq!(s.content, "玩家名：阿澈");
        let o = from_legacy_fact("约定", &json!({"还书": "周五"}), "", 2, 0);
        assert_eq!(o.content, "约定：{\"还书\":\"周五\"}");
    }

    #[test]
    fn legacy_fact_line_deserializes_to_memory_object() {
        let line = r#"{"kind":"fact","key":"last_thanked","value":3,"source":"hook.on_message","turn":7,"ts":1712345678}"#;
        let m: MemObject = serde_json::from_str(line).unwrap();
        assert_eq!(
            m,
            from_legacy_fact("last_thanked", &json!(3), "hook.on_message", 7, 1712345678)
        );
        assert_eq!(m.story_day, 0);
        assert!(m.story_clock.is_empty());
    }

    #[test]
    fn design_time_shape_deserializes() {
        let line = r#"{"id":"mem_0002","kind":"episode","content":"深夜闭馆","time":{"turn":14,"story_clock":"第3天 23:40"},"place":"图书馆","actors":["小雨"],"salience":0.82,"emotion":"温暖","links":["topic:便签"]}"#;
        let m: MemObject = serde_json::from_str(line).unwrap();
        assert_eq!(m.turn, 14);
        assert_eq!(m.story_day, 3, "从 story_clock 的「第N天」解出");
        assert_eq!(m.story_clock, "第3天 23:40");
        assert_eq!(m.place.as_deref(), Some("图书馆"));
        assert_eq!(m.actors, vec!["小雨".to_string()]);
        assert_eq!(m.salience, 0.82);
        assert_eq!(m.emotion.as_deref(), Some("温暖"));
        assert_eq!(m.witnesses_or_actors(), vec!["小雨".to_string()]);
    }

    #[test]
    fn memory_object_round_trips_through_json() {
        let mut m = mem("mem_0007", 3, 14, "深夜闭馆时她把便签送给玩家");
        m.story_clock = "23:40".into();
        m.place = Some("图书馆".into());
        m.actors = vec!["小雨".into(), "玩家".into()];
        m.emotion = Some("温暖".into());
        m.links = vec!["topic:便签".into()];
        m.thread = Some("thread.周五还书".into());
        m.rehearsals = 2;
        let text = serde_json::to_string(&m).unwrap();
        let back: MemObject = serde_json::from_str(&text).unwrap();
        assert_eq!(m, back);
    }

    // ---------- B4 渲染（`4.1）----------

    #[test]
    fn render_block_has_story_stamp_and_hearsay_source() {
        let mut ep = mem("mem_0001", 3, 14, "深夜闭馆时她把画着猫的便签送给了玩家");
        ep.story_clock = "23:40".into();
        ep.salience = 0.82;
        ep.emotion = Some("温暖".into());

        let mut hs = mem("mem_0002", 5, 40, "阿澈说图书馆要拆了");
        hs.story_clock = "23:40".into();
        hs.kind = KIND_HEARSAY.into();
        hs.source = "小雨".into();
        hs.salience = 0.41;
        hs.emotion = Some("怅然".into());

        let hits = vec![
            RecallHit {
                mem: ep,
                score: 0.82,
                reasons: vec![],
            },
            RecallHit {
                mem: hs,
                score: 0.21,
                reasons: vec![],
            },
        ];
        assert_eq!(
            render_memory_block(&hits),
            "【回忆·第3天 23:40】深夜闭馆时她把画着猫的便签送给了玩家（温暖·0.82）\n\
             【回忆·第5天 23:40】阿澈说图书馆要拆了（怅然·0.41；转述自小雨）"
        );
        // 整体不加外层标签，宿主 prompt.rs 负责包 <memory>
        assert!(!render_memory_block(&hits).contains("<memory>"));
        assert_eq!(render_memory_block(&[]), "", "空命中不产生空标签（`4.3）");
    }

    #[test]
    fn render_line_shapes() {
        // story_clock 已含天数 → 原样使用
        let mut m = mem("mem_0001", 3, 1, "正文");
        m.story_clock = "第3天 23:40".into();
        assert_eq!(render_memory_line(&m), "【回忆·第3天 23:40】正文（显著度0.80）");

        // 无时钟 → 只给天数；无情绪 → 显著度；正文换行压成空格
        let m = mem("mem_0002", 2, 1, "上句\n下句");
        assert_eq!(render_memory_line(&m), "【回忆·第2天】上句 下句（显著度0.80）");

        // 转述但来源缺失
        let mut m = mem("mem_0003", 1, 1, "听说");
        m.kind = KIND_HEARSAY.into();
        assert_eq!(
            render_memory_line(&m),
            "【回忆·第1天】听说（显著度0.80；转述）"
        );
    }

    // ---------- 确定性 ----------

    #[test]
    fn ties_are_broken_deterministically() {
        // 三条同分 0.5：a2/a1 同天同轮（比 id），b 天更晚（比 story_day）
        let mut a1 = mem("mem_0002", 1, 5, "a1");
        a1.salience = 1.0;
        let mut a2 = mem("mem_0001", 1, 5, "a2");
        a2.salience = 1.0;
        let mut b = mem("mem_0003", 8, 1, "b");
        b.salience = 0.5;
        let objs = vec![a1, a2, b];

        let hits = recall(&objs, &query("", 8));
        for h in &hits {
            approx(h.score, 0.5);
        }
        assert_eq!(ids(&hits), vec!["mem_0001", "mem_0002", "mem_0003"]);

        // 输入次序变了，输出次序不变（可回放）
        let shuffled = vec![objs[2].clone(), objs[1].clone(), objs[0].clone()];
        assert_eq!(ids(&recall(&shuffled, &query("", 8))), ids(&hits));
    }

    // ---------- top-K 与预算截断 ----------

    #[test]
    fn top_k_and_budget_truncate() {
        let mut objs = Vec::new();
        for i in 1..=4u64 {
            let mut m = mem(
                &format!("mem_{i:04}"),
                1,
                i,
                &format!("第{i}条回忆，内容长短一致。"),
            );
            m.salience = 0.8 - (i as f32) * 0.1;
            objs.push(m);
        }
        let unlimited = recall(&objs, &query("", 1));
        assert_eq!(unlimited.len(), 4, "top_k = 0 视为不限量");

        let q = RecallQuery {
            top_k: 2,
            ..query("", 1)
        };
        assert_eq!(ids(&recall(&objs, &q)), vec!["mem_0001", "mem_0002"]);

        // 预算刚好装下前两条
        let first_two: usize = unlimited.iter().take(2).map(|h| h.tokens()).sum();
        let q = RecallQuery {
            top_k: 10,
            budget_tokens: first_two,
            ..query("", 1)
        };
        assert_eq!(ids(&recall(&objs, &q)), vec!["mem_0001", "mem_0002"]);
        assert_eq!(
            recall(&objs, &q).iter().map(|h| h.tokens()).sum::<usize>(),
            first_two
        );

        // 预算小到装不下任何一条 → 仍保留最靠前的一条
        let q = RecallQuery {
            budget_tokens: 1,
            ..query("", 1)
        };
        assert_eq!(ids(&recall(&objs, &q)), vec!["mem_0001"]);
        // budget_tokens = 0 → 不限预算
        let q = RecallQuery {
            budget_tokens: 0,
            ..query("", 1)
        };
        assert_eq!(recall(&objs, &q).len(), 4);
    }

    // ---------- 三视图与检索 ----------

    #[test]
    fn rooms_group_by_place() {
        let mut a = mem("mem_0001", 1, 1, "图书馆的夜");
        a.place = Some("图书馆".into());
        a.salience = 0.9;
        let mut b = mem("mem_0002", 2, 2, "图书馆的门");
        b.place = Some(" 图书馆 ".into());
        b.salience = 0.3;
        let mut c = mem("mem_0003", 3, 3, "操场");
        c.place = Some("操场".into());
        let d = mem("mem_0004", 4, 4, "地点不详");
        let objs = vec![a, b, c, d];

        let views = rooms(&objs);
        assert_eq!(views.len(), 2, "place 为空的记忆不进房间图");
        assert_eq!(views[0].place, "图书馆");
        assert_eq!(views[0].count, 2);
        assert_eq!(views[0].top[0].id, "mem_0001", "房间内按 salience 降序");
        assert_eq!(views[0].top[1].id, "mem_0002");
        assert_eq!(views[1].place, "操场");
        assert_eq!(views[1].count, 1);

        assert!(rooms(&[]).is_empty(), "宫殿为空 → 面板无房间（`4.3）");
    }

    #[test]
    fn timeline_buckets_by_story_day() {
        let mut a = mem("mem_0001", 1, 1, "第一天");
        a.salience = 0.2;
        let mut b = mem("mem_0002", 1, 2, "第一天晚些");
        b.salience = 0.9;
        let c = mem("mem_0003", 3, 5, "第三天");
        let objs = vec![b, c, a];

        let tl = timeline(&objs);
        assert_eq!(tl.len(), 2);
        assert_eq!(tl[0].label, "第1天", "按故事天升序（时间线走廊）");
        assert_eq!(tl[0].count, 2);
        assert_eq!(tl[0].top[0].id, "mem_0002", "桶内按 salience 降序");
        assert_eq!(tl[1].label, "第3天");
        assert_eq!(tl[1].count, 1);
        assert_eq!(tl[1].top[0].turn, 5, "摘要带溯源轮次");

        // 每桶最多 VIEW_TOP_N 条摘要，但 count 是全量
        let many: Vec<MemObject> = (0..VIEW_TOP_N + 3)
            .map(|i| mem(&format!("mem_{:04}", i + 1), 2, i as u64, "x"))
            .collect();
        let tl = timeline(&many);
        assert_eq!(tl[0].count, VIEW_TOP_N + 3);
        assert_eq!(tl[0].top.len(), VIEW_TOP_N);
    }

    #[test]
    fn link_graph_nodes_and_edges() {
        let mut a = mem("mem_0001", 1, 1, "a");
        a.links = vec!["topic:便签".into(), "person:小雨".into()];
        let mut b = mem("mem_0002", 1, 2, "b");
        b.links = vec!["topic:便签".into(), "place:图书馆".into()];
        let mut c = mem("mem_0003", 1, 3, "c");
        c.links = vec!["topic:便签".into(), "TOPIC:便签".into()]; // 归一化后同一节点，不产生自环
        let g = link_graph(&[a, b, c]);

        assert_eq!(
            g.nodes,
            vec![
                "topic:便签".to_string(),
                "person:小雨".to_string(),
                "place:图书馆".to_string()
            ],
            "频次降序，同频按名字升序"
        );
        assert_eq!(
            g.edges,
            vec![
                ("person:小雨".to_string(), "topic:便签".to_string(), 1),
                ("place:图书馆".to_string(), "topic:便签".to_string(), 1),
            ]
        );

        // 两条记忆里共现 → 边权累加
        let mut d = mem("mem_0004", 2, 4, "d");
        d.links = vec!["topic:便签".into(), "person:小雨".into()];
        let mut e = mem("mem_0005", 3, 5, "e");
        e.links = vec!["topic:便签".into(), "person:小雨".into()];
        let g = link_graph(&[d, e]);
        assert_eq!(
            g.edges[0],
            ("person:小雨".to_string(), "topic:便签".to_string(), 2)
        );

        assert!(link_graph(&[]).nodes.is_empty());
    }

    #[test]
    fn links_match_accepts_three_forms() {
        let mut m = mem("mem_0001", 1, 1, "x");
        m.links = vec!["topic:便签".into(), "place:图书馆".into()];
        assert!(m.links_match("topic:便签"));
        assert!(m.links_match("topic"));
        assert!(m.links_match("便签"));
        assert!(m.links_match("  TOPIC:便签 "), "trim + 大小写不敏感");
        assert!(m.links_match("topic：便签"), "全角冒号归一");
        assert!(m.links_match("图书馆"));
        assert!(!m.links_match("person:小雨"));
        assert!(!m.links_match("topic:便签旧"), "带命名空间必须整体相等");
        assert!(!m.links_match(""));
        assert!(!m.links_match("   "));
    }

    #[test]
    fn search_hits_content_links_people_and_place() {
        let mut a = mem("mem_0001", 1, 1, "深夜闭馆时她把画着猫的便签送给了玩家");
        a.links = vec!["topic:便签".into()];
        a.place = Some("图书馆".into());
        a.actors = vec!["小雨".into()];
        a.emotion = Some("温暖".into());
        let mut b = mem("mem_0002", 2, 2, "操场上的约定");
        b.thread = Some("thread.周五还书".into());
        let objs = vec![a, b];

        assert_eq!(search(&objs, "便签")[0].id, "mem_0001");
        assert_eq!(search(&objs, "topic:便签")[0].id, "mem_0001");
        assert_eq!(search(&objs, "图书馆")[0].id, "mem_0001");
        assert_eq!(search(&objs, "小雨")[0].id, "mem_0001");
        assert_eq!(search(&objs, "温暖")[0].id, "mem_0001");
        assert_eq!(search(&objs, "还书")[0].id, "mem_0002");
        assert_eq!(search(&objs, "mem_0002")[0].id, "mem_0002");
        assert!(search(&objs, "不存在的东西").is_empty());
        assert!(search(&objs, "  ").is_empty());

        // 命中多条时按显眼程度排序
        let mut many = vec![
            mem("mem_0010", 1, 1, "便签 小事"),
            mem("mem_0011", 1, 1, "便签 大事"),
        ];
        many[1].salience = 0.95;
        assert_eq!(search(&many, "便签")[0].id, "mem_0011");
    }

    // ---------- 零散契约 ----------

    #[test]
    fn next_id_is_four_digit_padded() {
        assert_eq!(next_id(1), "mem_0001");
        assert_eq!(next_id(42), "mem_0042");
        assert_eq!(next_id(9999), "mem_9999");
        assert_eq!(next_id(10000), "mem_10000", "超过四位自然增长");
    }

    #[test]
    fn salience_is_sanitized() {
        let mut bad = mem("mem_0001", 1, 1, "脏数据");
        bad.salience = f32::NAN;
        assert_eq!(recall(&[bad], &query("", 1))[0].score, 0.0);
        let mut big = mem("mem_0002", 1, 2, "超界");
        big.salience = 3.0;
        approx(recall(&[big], &query("", 1))[0].score, 1.0);
    }

    #[test]
    fn recall_dedupes_by_id() {
        let a = mem("mem_0001", 1, 1, "同一条");
        let b = mem("mem_0001", 1, 1, "同一条");
        let c = mem("", 1, 2, "没有 id 的旧记录不参与去重");
        let d = mem("", 1, 3, "没有 id 的旧记录不参与去重");
        let hits = recall(&[a, b, c, d], &query("", 1));
        assert_eq!(ids(&hits), vec!["mem_0001", "", ""]);
    }

    #[test]
    fn brief_copies_display_fields() {
        let mut m = mem("mem_0001", 3, 14, "便签");
        m.story_clock = "23:40".into();
        m.emotion = Some("温暖".into());
        m.place = Some("图书馆".into());
        m.source = "hook.on_message".into();
        let b = brief(&m);
        assert_eq!(b.id, "mem_0001");
        assert_eq!(b.content, "便签");
        assert_eq!(b.turn, 14);
        assert_eq!(b.story_day, 3);
        assert_eq!(b.story_clock, "23:40");
        approx(b.salience, 0.8);
        assert_eq!(b.emotion.as_deref(), Some("温暖"));
        assert_eq!(b.place.as_deref(), Some("图书馆"));
        assert_eq!(b.source, "hook.on_message");
    }
}
