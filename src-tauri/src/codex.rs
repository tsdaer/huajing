//! 设定集（Codex）：实体化的世界知识（设计 §6）。
//!
//! 把世界知识组织为**带类型的实体节点 + 关系边**，注入不再是"关键词抽奖"而是
//! **引用解析**（§6.1）：谁被提到、谁在场、剧情揭示到哪，谁就出现。
//!
//! 本模块是**纯数据与算法**（M2 决断 3）：不碰文件、不碰网络、不跑 Lua。
//! .lua 实体文件由宿主在 card.rs 的沙箱里解析成 serde_json::Value 后交给
//! CodexEntity::from_value；时间（故事天/时钟）与黑板一律以参数传入。
//!
//! 覆盖的设计条款：
//! - §6.2 实体 schema（char/place/item/event/org/rule/concept/note 与 facts/secrets/live/relations）；
//! - §6.3 五个激活源（提及/在场/揭示/关系牵引/常驻）+ 三级注入深度 + 滞回
//!   + 恒定辨识点 anchors + 预算降级（anchors 行最后被裁）；
//! - §6.4 与状态树/黑板的三方联动：live 从黑板取 ▸当前，reveal 决定秘密是否进深卡；
//! - §6.5 三个时间层：瞬态 live、周期 variants、史变 versions（按故事天解析）；
//! - §6.8 一致性校验：anchors 最高保护级（提案冲突即驳回）；
//! - §6.9 只注入 canon（draft/retired 保留给界面，不参与激活）；
//! - §6.10 兼容层：note 类型 + 实体级 when 门控；
//! - §6.12 别名倒排索引（自建 trie，不引依赖）。
//!
//! 两个刻意的实现取舍，宿主需知道：
//! 1. **卡片层级精简卡（Stage::CardLean）**：为落实"anchors 行最后被裁"，降级阶梯是
//!    Deep → Card → 精简卡（one_liner + 辨识点）→ Line → 裁撤。精简卡在
//!    Activated::depth 里仍报 Depth::Card（辨识点保底仍是卡片级），text 是实际
//!    注入内容，reasons 里带「降级:…」说明；
//! 2. **实体级 when（§6.10）承载在 facts["__when"]**：公开结构体字段与任务签名
//!    一一对应、不加字段，宿主用 from_value 解析即可获得门控行为。
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

use crate::prompt::estimate_tokens;
use crate::store;

// ---------- 常量（设计 §6）----------

/// 实体类型顺序（输出次序用，§6.2 的类型表）
pub const ENTITY_TYPES: [&str; 8] = [
    "char", "place", "item", "event", "org", "rule", "concept", "note",
];

/// 实体状态（§6.9）：草稿待审 / 正史 / 废止留档
pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_CANON: &str = "canon";
pub const STATUS_RETIRED: &str = "retired";

/// 激活源权重（§6.3 五源；排序与降级都用它，"越强越晚被裁"）
pub const W_REVEAL: u8 = 5;
pub const W_PRESENCE: u8 = 4;
pub const W_CONSTANT: u8 = 3;
pub const W_TRACTION: u8 = 2;
pub const W_MENTION: u8 = 1;
/// 滞回（上一轮激活的保底），最弱：新证据优先于旧余温
pub const W_HYSTERESIS: u8 = 0;

/// 实体级 when 门控（§6.10）在 facts 里的承载键（见模块头注释取舍 2）
pub const WHEN_FACT_KEY: &str = "__when";

/// known_by 里的通配：该秘密不设知情限制（谁都能看到深卡）
pub const SECRET_KNOWN_BY_ANY: &str = "*";

/// 卡片里 facts 的取值表：越靠前越重要（渲染时"择要"，不倾倒全量 JSON，§6.3）
const FACT_TABLE: &[(&str, &str)] = &[
    ("look.impression", "外貌"),
    ("speech.style", "言语"),
    ("mannerisms.habits", "动作"),
    ("motivation", "动机"),
    ("schedule", "作息"),
    ("speech.tics", "口癖"),
    ("needs", "需要"),
    ("values", "看重"),
    ("interests", "兴趣"),
    ("temperament.summary", "气质"),
    ("scene", "景象"),
    ("rules", "规则"),
    ("exits", "出口"),
    ("appearance", "外观"),
    ("cause", "起因"),
    ("course", "经过"),
    ("outcome", "结果"),
    ("premise", "前提"),
    ("abilities", "能力"),
];

/// 卡片里 facts 行数上限（"择要"，不是倾倒）
const MAX_FACT_LINES: usize = 5;
/// 情绪变体行数上限（§6.3 状态词表按需注入）
const MAX_AFFECT_LINES: usize = 2;
/// 单行文本的字数上限（超出以 … 收尾）
const MAX_FACT_CHARS: usize = 60;
/// 单实体最多记几条"提及"原因（其余折叠成"另 N 处"）
const MENTION_REASONS_MAX: usize = 3;

// ---------- 注入深度（§6.3 三级）----------

/// 注入深度（§6.3）：1 行 / 卡片 / 深卡。Line < Card < Deep，可直接比较大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// 仅被提及：one_liner
    Line,
    /// 激活（在场/揭示/牵引/常驻）：one_liner + facts + 当前 + 辨识点
    Card,
    /// 激活且当前视角知情：卡片 + 对应 secrets
    Deep,
}

impl Depth {
    /// 中文短名（记忆检查器逐层可见用）
    pub fn label(self) -> &'static str {
        match self {
            Depth::Line => "1 行",
            Depth::Card => "卡片",
            Depth::Deep => "深卡",
        }
    }

    /// 卡片级（卡片/深卡）恒带 anchors 行（§6.3）
    pub fn is_card_level(self) -> bool {
        self >= Depth::Card
    }
}

/// 渲染阶段：比 Depth 多一个"精简卡"中间态（anchors 最后被裁的落点，见模块头注释）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Line,
    /// 只留 one_liner + 辨识点（facts/当前/秘密已裁，辨识点仍在）
    CardLean,
    Card,
    Deep,
}

impl Stage {
    fn of(depth: Depth) -> Stage {
        match depth {
            Depth::Line => Stage::Line,
            Depth::Card => Stage::Card,
            Depth::Deep => Stage::Deep,
        }
    }

    /// 对外报告的深度：精简卡仍算卡片级（辨识点保底）
    fn depth(self) -> Depth {
        match self {
            Stage::Line => Depth::Line,
            Stage::Card | Stage::CardLean => Depth::Card,
            Stage::Deep => Depth::Deep,
        }
    }

    fn demote(self) -> Stage {
        match self {
            Stage::Deep => Stage::Card,
            Stage::Card => Stage::CardLean,
            Stage::CardLean => Stage::Line,
            Stage::Line => Stage::Line,
        }
    }
}

/// 降级原因文案（进 Activated::reasons；按"降到的目标阶段"给，检查器可解释）
fn demote_reason(target: Stage) -> &'static str {
    match target {
        Stage::Deep => "",
        Stage::Card => "降级:深卡→卡片（秘密先裁）",
        Stage::CardLean => "降级:卡片→精简（细目先裁，辨识点保底）",
        Stage::Line => "降级:精简→1 行（辨识点最后裁）",
    }
}

// ---------- 实体 schema（§6.2）----------

/// 秘密（§6.2 secrets）：known_by 是知情者集合，revealed_by 是声明式揭示来源。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Secret {
    pub content: String,
    /// 知情者集合（§10.4 揭示时按场景见证者加入）；含 "*" 表示不设限
    pub known_by: Vec<String>,
    /// 声明式揭示来源，如 "state:日常.夜谈"（§6.4）
    pub revealed_by: Option<String>,
}

/// 关系边（§6.2 relations）：always_with 参与"关系牵引"（§6.3 源 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation {
    pub to: String,
    pub kind: String,
    pub always_with: bool,
}

/// 实体生命周期（§6.5）：departed / dead 带生效故事时刻；死亡是正史变更不是删除。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lifecycle {
    /// active | departed | dead
    pub status: String,
    /// 生效故事天（0 / 负数 = 未指定，立即生效）
    pub at_day: i64,
    pub note: Option<String>,
}

impl Lifecycle {
    pub const ACTIVE: &'static str = "active";
    pub const DEPARTED: &'static str = "departed";
    pub const DEAD: &'static str = "dead";

    /// 该生命周期此刻是否已经生效（at_day 未指定视为立即生效）
    pub fn in_effect_at(&self, day: i64) -> bool {
        self.at_day <= 0 || day >= self.at_day
    }

    /// 此刻是否还算"在场"：active 恒是；departed/dead 只有生效前（flashback /
    /// 更早的 canon point，§6.5/§6.7）才算在场——离场后不再由在场源激活。
    pub fn present_at(&self, day: i64) -> bool {
        match self.status.as_str() {
            Lifecycle::DEPARTED | Lifecycle::DEAD => !self.in_effect_at(day),
            _ => true,
        }
    }
}

/// 周期变体（§6.5）：when 命中即用 value 覆盖 facet（声明序优先）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variant {
    pub when: Value,
    pub facet: String,
    pub value: Value,
}

/// 史变版本（§6.5）：事实变更追加不覆盖，from_day 起生效（按故事钟解析）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Version {
    pub from_day: i64,
    pub facet: String,
    pub value: Value,
    pub note: Option<String>,
}

/// 设定集实体（§6.2 schema；.lua / .json 双格式由宿主统一解析成 Value 后进来）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexEntity {
    pub id: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub one_liner: String,
    pub facts: BTreeMap<String, Value>,
    pub secrets: BTreeMap<String, Secret>,
    /// "现在时"字段名：注入时取黑板值拼 ▸当前（§6.4）
    pub live: Vec<String>,
    pub relations: Vec<Relation>,
    /// draft | canon | retired（§6.9）
    pub status: String,
    /// 常驻注入标记（与 ty == "rule" 等价，§6.3 源 5）
    pub constant: bool,
    /// 宫殿已召回强关联记忆时降级或跳过（§6.3 去重）——判定需要召回结果，
    /// 而 ActivationContext 里没有召回输入，故本模块只解析保存、由宿主在组装
    /// 前自行降级/跳过（已知未做项）。
    pub skip_if_remembered: bool,
    pub lifecycle: Option<Lifecycle>,
    pub variants: Vec<Variant>,
    pub versions: Vec<Version>,
}

impl CodexEntity {
    /// 从宿主解析出的 Value（Lua table / JSON object）构造实体。
    /// 缺 id / name / type 报错；其余字段给默认值（缺 status → canon，
    /// 与 DataHub 下的示例实体一致）。
    pub fn from_value(v: &Value) -> Result<CodexEntity, String> {
        let o = v
            .as_object()
            .ok_or_else(|| "实体必须是对象（Lua table / JSON object）".to_string())?;
        let id = get_text(o, "id").ok_or_else(|| "实体缺少 id".to_string())?;
        let name = get_text(o, "name").ok_or_else(|| format!("实体「{id}」缺少 name"))?;
        let ty = get_text(o, "type").ok_or_else(|| format!("实体「{id}」缺少 type"))?;

        // aliases：trim、去空、去重、去掉与 name 重复项（引用解析用的别名表）
        let mut aliases: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        if let Some(v) = field(o, "aliases") {
            for a in get_text_list(v) {
                if eq_fold(&a, &name) {
                    continue;
                }
                if seen.insert(fold_str(&a)) {
                    aliases.push(a);
                }
            }
        }

        // facts：任意 facet 的自由结构（BTreeMap 保证遍历次序确定）
        let mut facts: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(Value::Object(m)) = field(o, "facts") {
            for (k, val) in m {
                if !val.is_null() {
                    facts.insert(k.clone(), val.clone());
                }
            }
        }
        // 实体级 when 门控（§6.10）：承载在 facts 保留键里（不改公开结构，见模块头注释）
        if let Some(w) = field(o, "when") {
            facts.entry(WHEN_FACT_KEY.to_string())
                .or_insert_with(|| w.clone());
        }

        // one_liner：兼容旧世界书条目（content / text 兜底）
        let one_liner = get_text(o, "one_liner")
            .or_else(|| get_text(o, "content"))
            .or_else(|| get_text(o, "text"))
            .or_else(|| facts.get("content").and_then(as_text))
            .or_else(|| facts.get("text").and_then(as_text))
            .unwrap_or_default();

        // secrets：支持完整对象，也支持 "秘密 = 一句话" 的简写
        let mut secrets: BTreeMap<String, Secret> = BTreeMap::new();
        if let Some(Value::Object(m)) = field(o, "secrets") {
            for (k, val) in m {
                let key = k.trim();
                if key.is_empty() {
                    continue;
                }
                let s = match val {
                    Value::String(_) | Value::Number(_) => Secret {
                        content: as_text(val).unwrap_or_default(),
                        ..Secret::default()
                    },
                    Value::Object(so) => Secret {
                        content: get_text(so, "content")
                            .or_else(|| get_text(so, "text"))
                            .or_else(|| get_text(so, "desc"))
                            .unwrap_or_default(),
                        known_by: field(so, "known_by")
                            .map(get_text_list)
                            .unwrap_or_default(),
                        revealed_by: field(so, "revealed_by")
                            .and_then(|v| get_text_list(v).into_iter().next()),
                    },
                    _ => continue,
                };
                if s.content.is_empty() && s.known_by.is_empty() && s.revealed_by.is_none() {
                    continue;
                }
                secrets.insert(key.to_string(), s);
            }
        }

        // live："现在时"字段名（数组或单串）
        let mut live: Vec<String> = Vec::new();
        let mut live_seen: BTreeSet<String> = BTreeSet::new();
        if let Some(v) = field(o, "live") {
            for f in get_text_list(v) {
                if live_seen.insert(fold_str(&f)) {
                    live.push(f);
                }
            }
        }

        // relations：一跳关系边（to/kind/always_with）
        let mut relations: Vec<Relation> = Vec::new();
        if let Some(Value::Array(a)) = field(o, "relations") {
            for item in a {
                let to = match item {
                    Value::Object(ro) => get_text(ro, "to").or_else(|| get_text(ro, "id")),
                    Value::String(_) => as_text(item),
                    _ => None,
                };
                let Some(to) = to else { continue };
                let (kind, always_with) = match item {
                    Value::Object(ro) => (
                        get_text(ro, "kind")
                            .or_else(|| get_text(ro, "rel"))
                            .unwrap_or_else(|| "related".to_string()),
                        field(ro, "always_with").and_then(get_bool).unwrap_or(false),
                    ),
                    _ => ("related".to_string(), false),
                };
                relations.push(Relation {
                    to,
                    kind,
                    always_with,
                });
            }
        }

        // status：缺省 canon（示例实体均不写 status）
        let status = get_text(o, "status")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| STATUS_CANON.to_string());

        let lifecycle = match field(o, "lifecycle") {
            Some(Value::Object(lo)) => Some(Lifecycle {
                status: get_text(lo, "status")
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_else(|| Lifecycle::ACTIVE.to_string()),
                at_day: field(lo, "at_day").and_then(get_i64).unwrap_or(0),
                note: get_text(lo, "note"),
            }),
            Some(v) => as_text(v).map(|status| Lifecycle {
                status: status.to_ascii_lowercase(),
                at_day: 0,
                note: None,
            }),
            None => None,
        };

        // variants：{ when, facet, value }；兼容 §6.5 文字形态 { when, facets = {…} }
        let mut variants: Vec<Variant> = Vec::new();
        if let Some(Value::Array(a)) = field(o, "variants") {
            for item in a {
                let Value::Object(vo) = item else { continue };
                let when = field(vo, "when").cloned().unwrap_or(Value::Null);
                if let Some(facet) = get_text(vo, "facet") {
                    variants.push(Variant {
                        when,
                        facet,
                        value: field(vo, "value").cloned().unwrap_or(Value::Null),
                    });
                } else if let Some(Value::Object(m)) = field(vo, "facets") {
                    for (f, val) in m {
                        variants.push(Variant {
                            when: when.clone(),
                            facet: f.clone(),
                            value: val.clone(),
                        });
                    }
                }
            }
        }

        // versions：{ from_day, facet, value, note }；day/from 是宽容别名
        let mut versions: Vec<Version> = Vec::new();
        if let Some(Value::Array(a)) = field(o, "versions") {
            for item in a {
                let Value::Object(vo) = item else { continue };
                let Some(facet) = get_text(vo, "facet") else {
                    continue;
                };
                let from_day = field(vo, "from_day")
                    .or_else(|| field(vo, "day"))
                    .or_else(|| field(vo, "from"))
                    .and_then(get_i64)
                    .unwrap_or(0);
                versions.push(Version {
                    from_day,
                    facet,
                    value: field(vo, "value").cloned().unwrap_or(Value::Null),
                    note: get_text(vo, "note"),
                });
            }
        }

        Ok(CodexEntity {
            id,
            ty,
            name,
            aliases,
            one_liner,
            facts,
            secrets,
            live,
            relations,
            status,
            constant: field(o, "constant").and_then(get_bool).unwrap_or(false),
            skip_if_remembered: field(o, "skip_if_remembered")
                .and_then(get_bool)
                .unwrap_or(false),
            lifecycle,
            variants,
            versions,
        })
    }

    /// 正史实体才参与注入（§6.9）；draft / retired 留给界面与收件箱。
    pub fn is_canon(&self) -> bool {
        self.status == STATUS_CANON
    }

    /// 恒定辨识点（§6.3 anchors）：facts.look.anchors，卡片/深卡恒注入。
    pub fn anchors(&self) -> Vec<String> {
        let Some(look) = self.facts.get("look") else {
            return Vec::new();
        };
        let Some(v) = look.get("anchors").or_else(|| look.get("anchor")) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for a in get_text_list(v) {
            if !out.iter().any(|x| eq_fold(x, &a)) {
                out.push(a);
            }
        }
        out
    }

    /// facet 在指定故事时刻的取值（§6.5 三个时间层）。
    ///
    /// 解析顺序：versions（from_day ≤ day 的最大者，含当日）→ variants
    /// （when 命中的第一个，声明序）→ facts（点分路径）。返回 None 表示该
    /// facet 此刻无值。
    pub fn facet_at(
        &self,
        facet: &str,
        day: i64,
        clock: &str,
        bb: &BTreeMap<String, Value>,
    ) -> Option<Value> {
        // ① 史变：不覆盖、追加；同 from_day 取后声明者（事件溯源的"最后写入生效"）
        let mut best: Option<(i64, Value)> = None;
        for ver in &self.versions {
            if ver.from_day > day {
                continue;
            }
            let Some(val) = facet_override(&ver.facet, facet, &ver.value) else {
                continue;
            };
            match &best {
                Some((best_day, _)) if *best_day > ver.from_day => {}
                _ => best = Some((ver.from_day, val)),
            }
        }
        if let Some((_, val)) = best {
            return Some(val);
        }

        // ② 周期变体：when 命中即覆盖（黑板/时钟，确定性）
        let place = bb.get("place").and_then(|v| v.as_str());
        for var in &self.variants {
            if !when_matches(&var.when, bb, day, clock, place) {
                continue;
            }
            if let Some(val) = facet_override(&var.facet, facet, &var.value) {
                return Some(val);
            }
        }

        // ③ 静态正史：先整体键（允许 facet 本身就是带点的键），再逐层点分
        if let Some(v) = self.facts.get(facet) {
            return Some(v.clone());
        }
        fact_path(self, facet).cloned()
    }
}

/// 声明 facet 是否覆盖请求 facet：完整相等，或请求的是它的下级
/// （look 覆盖 look.impression）。
fn facet_override(declared: &str, requested: &str, value: &Value) -> Option<Value> {
    let declared = declared.trim();
    if declared.is_empty() {
        return None;
    }
    if declared == requested {
        return Some(value.clone());
    }
    let rest = requested.strip_prefix(declared)?.strip_prefix('.')?;
    if rest.is_empty() {
        return Some(value.clone());
    }
    let mut cur = value;
    for p in rest.split('.') {
        cur = cur.get(p)?;
    }
    Some(cur.clone())
}

/// facts 的点分路径取值（look.anchors、speech.by_affect.害羞）。
fn fact_path<'a>(e: &'a CodexEntity, path: &str) -> Option<&'a Value> {
    if let Some(v) = e.facts.get(path) {
        return Some(v);
    }
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut cur = e.facts.get(first)?;
    for p in parts {
        cur = cur.get(p)?;
    }
    Some(cur)
}

/// 静态正史取值（点分路径；不含 variants/versions 的时间层）。
///
/// 一致性校验的「正史现值」一侧用静态 canon：冲突呈现关心的是写定的设定，
/// 「第 N 天取旧版」的解析（facet_at）需要时钟上下文，那里不适合做校验依据。
pub fn static_fact<'a>(e: &'a CodexEntity, path: &str) -> Option<&'a Value> {
    fact_path(e, path)
}

/// 把收件箱确认的正史增量（grown.json，M3.8 · 设计 §6.9）应用到实体列表上。
///
/// 合并语义（确定性、幂等——同一补丁应用两次与一次结果一致）：
/// - 补丁 id 不存在 → 追加为新实体（补丁即全量骨架，缺 id 时用键补）；
/// - 补丁 id 已存在 → **深合并**：facts 递归合并（对象并集、标量后写覆盖）、
///   aliases 并入去重、relations 按 to+kind 去重追加、secrets 按 key 覆盖合并；
///   one_liner/status/type 等标量仅在补丁给出非空值时覆盖。
///
/// 玩家手写的实体文件永不被机器改写——增量住在 grown.json 里，撤掉一行即回滚。
pub fn apply_grown(mut entities: Vec<CodexEntity>, grown: &store::GrownFile) -> Vec<CodexEntity> {
    for (id, patch) in &grown.entities {
        let mut patch = patch.clone();
        if let Some(obj) = patch.as_object_mut() {
            obj.entry("id").or_insert_with(|| Value::String(id.clone()));
        }
        match entities.iter().position(|e| &e.id == id) {
            Some(idx) => {
                // 部分补丁缺 name/type 必填字段：借既有实体的补齐再解析
                //（补丁只带增量是常态，不能因此整个丢弃）
                if let Err(_) = CodexEntity::from_value(&patch) {
                    let base = &entities[idx];
                    if let Some(obj) = patch.as_object_mut() {
                        obj.entry("name").or_insert_with(|| Value::String(base.name.clone()));
                        obj.entry("type").or_insert_with(|| Value::String(base.ty.clone()));
                    }
                }
                match CodexEntity::from_value(&patch) {
                    Ok(parsed) => merge_entity(&mut entities[idx], parsed),
                    Err(e) => {
                        crate::diag::record("codex", format!("正史增量 {id} 解析失败，已跳过：{e}"))
                    }
                }
            }
            None => match CodexEntity::from_value(&patch) {
                Ok(parsed) => entities.push(parsed),
                Err(e) => {
                    crate::diag::record("codex", format!("正史增量 {id} 解析失败，已跳过：{e}"))
                }
            },
        }
    }
    entities
}

/// 把补丁实体并入既有实体（幂等：并集语义，标量后写覆盖）。
/// type 不覆盖——改既有实体的类型会换模板、改注入语义，撤增量重写是正道。
fn merge_entity(base: &mut CodexEntity, patch: CodexEntity) {
    if !patch.one_liner.is_empty() {
        base.one_liner = patch.one_liner;
    }
    for a in patch.aliases {
        if !base.aliases.iter().any(|x| x == &a) {
            base.aliases.push(a);
        }
    }
    merge_facts(&mut base.facts, patch.facts);
    for (key, secret) in patch.secrets {
        base.secrets.insert(key, secret);
    }
    for r in patch.relations {
        if !base.relations.iter().any(|x| x.to == r.to && x.kind == r.kind) {
            base.relations.push(r);
        }
    }
    if !patch.live.is_empty() {
        for l in patch.live {
            if !base.live.contains(&l) {
                base.live.push(l);
            }
        }
    }
}

/// facts 递归合并：两边都是对象 → 并集递归；否则补丁值覆盖
fn merge_facts(base: &mut BTreeMap<String, Value>, patch: BTreeMap<String, Value>) {
    for (key, value) in patch {
        match base.get_mut(&key) {
            Some(Value::Object(prev_obj)) if value.is_object() => {
                let patch_map: BTreeMap<String, Value> =
                    value.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let mut prev_map: BTreeMap<String, Value> =
                    prev_obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                merge_facts(&mut prev_map, patch_map);
                *prev_obj = prev_map.into_iter().collect();
            }
            Some(slot) => *slot = value,
            None => {
                base.insert(key, value);
            }
        }
    }
}

// ---------- 字段读取小工具（Lua/JSON 宽容解析）----------

fn field<'a>(o: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn get_text(o: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    field(o, key).and_then(as_text)
}

/// 字符串列表：接受单串、数字/布尔或数组（元素同样宽容）
fn get_text_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(as_text).collect(),
        other => as_text(other).into_iter().collect(),
    }
}

fn get_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn get_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

// ---------- 拉丁大小写不敏感（CJK 精确）----------

fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

fn fold_str(s: &str) -> String {
    s.trim().chars().map(fold).collect()
}

/// 两侧 trim 后比较：拉丁字母大小写不敏感，CJK 精确（§6.3 提及/在场匹配口径）
fn eq_fold(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.len() != b.len() {
        return false;
    }
    a.chars().zip(b.chars()).all(|(x, y)| fold(x) == fold(y))
}

// ---------- when 条件（§6.5 / §6.6 / §6.10）----------

/// 声明式条件求值（宿主侧确定性）。
///
/// - {}（或非对象）恒命中；
/// - day：**阈值**语义——day ≥ 给定值即命中（§6.6 的 when = { day = 20 }）；
/// - clock：与黑板时钟精确匹配，另支持时段区间 "18:00-24:00"（~ / – / — / -
///   皆可，起 > 止 视为跨夜，如 "22:00-02:00"）；
/// - place：与黑板地点精确匹配；
/// - actors：**含关系**——黑板 actors 里出现该值即命中；
/// - 其余键：与黑板同名键等值匹配（数字跨 int/float 比较，字符串大小写不敏感）；
/// - 数组一律是"任一命中"（或）；黑板里没有该键即不命中。
pub fn when_matches(
    when: &Value,
    bb: &BTreeMap<String, Value>,
    day: i64,
    clock: &str,
    place: Option<&str>,
) -> bool {
    let Value::Object(cond) = when else {
        return true; // 非对象视为无约束
    };
    let bb_place: Option<String> = place
        .map(|s| s.to_string())
        .or_else(|| bb.get("place").and_then(|v| v.as_str()).map(|s| s.to_string()));

    for (key, expect) in cond {
        match key.as_str() {
            "day" => {
                let hit = match expect {
                    Value::Array(a) => a.iter().any(|v| get_i64(v).is_some_and(|d| day >= d)),
                    other => get_i64(other).is_some_and(|d| day >= d),
                };
                if !hit {
                    return false;
                }
            }
            "clock" => {
                let hit = match expect {
                    Value::Array(a) => a.iter().any(|v| clock_hit(v, clock)),
                    other => clock_hit(other, clock),
                };
                if !hit {
                    return false;
                }
            }
            "place" => {
                let hit = match expect {
                    Value::Array(a) => a.iter().any(|v| {
                        v.as_str()
                            .is_some_and(|s| bb_place.as_deref().is_some_and(|p| eq_fold(s, p)))
                    }),
                    other => other
                        .as_str()
                        .is_some_and(|s| bb_place.as_deref().is_some_and(|p| eq_fold(s, p))),
                };
                if !hit {
                    return false;
                }
            }
            "actors" => {
                let present: Vec<String> = bb
                    .get("actors")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let hit = |want: &str| present.iter().any(|x| eq_fold(x, want));
                let ok = match expect {
                    Value::Array(a) => a.iter().any(|v| v.as_str().is_some_and(hit)),
                    other => other.as_str().is_some_and(hit),
                };
                if !ok {
                    return false;
                }
            }
            _ => {
                let Some(actual) = bb.get(key) else {
                    return false;
                };
                if !values_equal(expect, actual) {
                    return false;
                }
            }
        }
    }
    true
}

fn clock_hit(expect: &Value, clock: &str) -> bool {
    let Some(want) = expect.as_str() else {
        return false;
    };
    let want = want.trim();
    if want.is_empty() {
        return true;
    }
    if eq_fold(want, clock) {
        return true;
    }
    for sep in ["~", "–", "—", "-"] {
        let Some((a, b)) = want.split_once(sep) else {
            continue;
        };
        let (Some(from), Some(to)) = (parse_hhmm(a), parse_hhmm(b)) else {
            continue;
        };
        let Some(now) = parse_hhmm(clock) else {
            return false;
        };
        return if from <= to {
            now >= from && now <= to
        } else {
            now >= from || now <= to
        };
    }
    false
}

/// HH:MM → 分钟数；允许 24:00 作区间终点
fn parse_hhmm(s: &str) -> Option<i64> {
    let (h, m) = s.trim().split_once(':')?;
    let h: i64 = h.trim().parse().ok()?;
    let m: i64 = m.trim().parse().ok()?;
    if !(0..=24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    if h == 24 && m != 0 {
        return None;
    }
    Some(h * 60 + m)
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => eq_fold(x, y),
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(p), Some(q)) => p == q,
            _ => false,
        },
        (Value::String(x), Value::Number(y)) | (Value::Number(y), Value::String(x)) => {
            x.trim().parse::<f64>().is_ok_and(|p| y.as_f64().is_some_and(|q| p == q))
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Bool(x), Value::String(y)) | (Value::String(y), Value::Bool(x)) => {
            y.trim().eq_ignore_ascii_case(&x.to_string())
        }
        (Value::Null, Value::Null) => true,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| values_equal(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w)))
        }
        _ => false,
    }
}

// ---------- anchors 一致性校验（§6.8 最高保护级）----------

/// 提案是否与实体的恒定辨识点冲突（§6.8）：冲突即返回驳回原因。
///
/// 确定性检查三类改动：
/// 1. 提案改写了 anchors 集合（新增/移除/清空）——辨识点改变应当是刻意的
///    叙事事件，需显式 retcon 并留档；
/// 2. 提案整体替换 look 却没把 anchors 带过去（应用后会静默丢掉辨识点）；
/// 3. 提案显式声明撤回某个辨识点（retract_anchors / remove_anchors 等）。
///
/// 无 anchors 的实体没有受保护对象，返回 None（新增 anchors 走确认流程）。
/// 语义级矛盾（"前文黑发后文棕发"）留给 §6.8 ③ 的可选 LLM 校验。
pub fn anchors_conflict(entity: &CodexEntity, proposal: &Value) -> Option<String> {
    let existing = entity.anchors();
    if existing.is_empty() {
        return None;
    }

    // ① 显式撤回
    let mut retracted: Vec<String> = Vec::new();
    collect_keyed_strings(
        proposal,
        &[
            "retract_anchors",
            "remove_anchors",
            "drop_anchors",
            "delete_anchors",
        ],
        &mut retracted,
    );
    if let Some(hit) = retracted
        .iter()
        .find(|r| existing.iter().any(|a| eq_fold(a, r)))
    {
        return Some(conflict_reason(&format!("提案显式撤回辨识点「{hit}」")));
    }

    // ② 提案整体替换 look（却未带 anchors）
    if let Value::Object(m) = proposal {
        if let Some(facet) = get_text(m, "facet").or_else(|| get_text(m, "field")) {
            let f = facet.trim();
            if (f == "look" || f == "facts.look") && !f.ends_with("anchors") {
                if let Some(Value::Object(look)) = field(m, "value") {
                    if !look.contains_key("anchors") && !look.contains_key("anchor") {
                        return Some(conflict_reason("提案整体替换 look 却未保留辨识点"));
                    }
                }
            }
        }
    }

    // ③ 提案里的 anchors 列表与现状不一致（含 facet = "look.anchors" 的直给形态）
    let mut proposed: Vec<(String, Vec<String>)> = Vec::new();
    collect_anchor_lists(proposal, &mut proposed);
    for (path, list) in proposed {
        let same = list.len() == existing.len()
            && existing.iter().all(|a| list.iter().any(|b| eq_fold(a, b)));
        if same {
            continue;
        }
        if list.is_empty() {
            return Some(conflict_reason(&format!("提案清空了 {path} 的全部辨识点")));
        }
        let removed: Vec<&String> = existing
            .iter()
            .filter(|a| !list.iter().any(|b| eq_fold(a, b)))
            .collect();
        let added: Vec<&String> = list
            .iter()
            .filter(|b| !existing.iter().any(|a| eq_fold(a, b)))
            .collect();
        let mut detail = String::new();
        if !removed.is_empty() {
            detail.push_str(&format!(
                "提案移除「{}」",
                removed
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        if !added.is_empty() {
            if !detail.is_empty() {
                detail.push('，');
            }
            detail.push_str(&format!(
                "新增「{}」",
                added
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        return Some(conflict_reason(&format!("{path}：{detail}")));
    }
    None
}

fn conflict_reason(detail: &str) -> String {
    format!("与恒定辨识点冲突：{detail}（§6.8 anchors 最高保护级，改动需显式 retcon 并留档）")
}

/// 递归收集提案里所有 anchors 列表（含 {"facet":"look.anchors","value":[…]} 形态）
fn collect_anchor_lists(v: &Value, out: &mut Vec<(String, Vec<String>)>) {
    match v {
        Value::Object(m) => {
            let facetish = get_text(m, "facet").or_else(|| get_text(m, "field"));
            if let Some(f) = &facetish {
                let f = f.trim();
                if f == "anchors" || f.ends_with(".anchors") {
                    let list = field(m, "value")
                        .or_else(|| field(m, "anchors"))
                        .map(get_text_list)
                        .unwrap_or_default();
                    out.push((f.to_string(), list));
                }
            }
            for (k, val) in m {
                if k == "anchors" || k == "anchor" {
                    out.push((k.clone(), get_text_list(val)));
                } else if Some(k.as_str()) != facetish.as_deref() {
                    collect_anchor_lists(val, out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_anchor_lists(x, out);
            }
        }
        _ => {}
    }
}

/// 递归收集指定键名下的字符串（用于显式撤回标记）
fn collect_keyed_strings(v: &Value, keys: &[&str], out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if keys.contains(&k.as_str()) {
                    out.extend(get_text_list(val));
                } else {
                    collect_keyed_strings(val, keys, out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_keyed_strings(x, keys, out);
            }
        }
        _ => {}
    }
}

// ---------- 激活上下文与产物 ----------

/// 别名/名字命中的一次提及
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mention {
    pub id: String,
    /// 命中的声明形式（别名或 name）
    pub alias: String,
    /// 命中起点在 window_text 里的**字节偏移**（可直接 text[at..at+alias.len()] 切片高亮）
    pub at: usize,
}

/// 一轮激活的输入（宿主拼好后传入；本模块不读文件、不查库）
#[derive(Debug)]
pub struct ActivationContext<'a> {
    /// 扫描窗口（宿主拼最近 N 条消息，§6.3 默认 16 条）
    pub window_text: &'a str,
    /// 黑板地点（实体 id 或名字/别名均可命中；None 时退回 blackboard["place"]）
    pub place: Option<&'a str>,
    /// 在场者（黑板 actors）
    pub actors: &'a [String],
    /// 状态树 reveal：实体 id、实体.秘密，或秘密的 revealed_by 声明值
    pub reveals: &'a [String],
    /// 当前视角已知的秘密路径（实体.secrets.秘密 或 实体.秘密）
    pub known: &'a BTreeSet<String>,
    /// 上一轮激活集合（滞回；宿主只传上一轮）
    pub previously_active: &'a BTreeSet<String>,
    /// 滞回轮数（宿主传 3；0 = 关闭滞回）。本实现按"上一轮激活者保底卡片"落地，
    /// 轮数窗口由宿主维护的 previously_active 集合表达。
    pub hold_rounds: u32,
    pub day: i64,
    pub clock: &'a str,
    /// 黑板快照：live 取值 + variants/when 条件 + {{bb.*}} 占位符
    pub blackboard: &'a BTreeMap<String, Value>,
    /// 组装视角（M3.1 · 设计 §10.4）：深卡秘密按「她是否知情」判定——
    /// known_by 名单含她（她自己一直知道）、或她的揭示集里有该路径（经历过 reveal）
    pub viewer: &'a str,
}

/// 一张注入卡（检查器逐层可见：激活原因、深度、token）
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activated {
    pub id: String,
    pub name: String,
    pub ty: String,
    pub depth: Depth,
    /// 激活原因（揭示/在场/常驻/牵引/提及/滞回/降级），确定性次序
    pub reasons: Vec<String>,
    /// 实际注入文本（与 render_block 中该项一致）
    pub text: String,
    pub tokens: usize,
}

impl Activated {
    /// 是否被预算降级过（text 比该深度的完整形态更短）
    pub fn degraded(&self) -> bool {
        self.reasons.iter().any(|r| r.starts_with("降级:"))
    }
}

/// B3 层预算（§4.2 实体卡 ~12%；超限先降级再裁撤）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CodexBudget {
    /// 卡片合计 token 上限（0 = 不注入）
    pub tokens: usize,
    /// 条数上限（0 = 不注入）
    pub max_cards: usize,
}

impl Default for CodexBudget {
    /// v0 占位默认值（宿主按 §4.2 预算表覆盖）
    fn default() -> Self {
        CodexBudget {
            tokens: 1000,
            max_cards: 12,
        }
    }
}

// ---------- 别名 trie（自建，不引依赖；§6.12）----------

#[derive(Debug, Default)]
struct TrieNode {
    /// 子节点（BTreeMap 保证遍历次序确定）
    children: BTreeMap<char, usize>,
    /// 终止于此的（实体下标, 声明别名）
    terminal: Vec<(usize, String)>,
}

/// 别名倒排索引：name + aliases 全部入树，拉丁大小写不敏感
#[derive(Debug, Default)]
struct AliasTrie {
    nodes: Vec<TrieNode>,
}

/// trie 的一次原始命中（未做同实体去重）
#[derive(Debug, Clone)]
struct RawHit {
    start: usize,
    end: usize,
    entity: usize,
    alias: String,
}

impl AliasTrie {
    fn new() -> AliasTrie {
        AliasTrie {
            nodes: vec![TrieNode::default()],
        }
    }

    fn insert(&mut self, key: &str, entity: usize) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        let mut node = 0usize;
        for c in key.chars().map(fold) {
            let existing = self.nodes[node].children.get(&c).copied();
            node = match existing {
                Some(n) => n,
                None => {
                    self.nodes.push(TrieNode::default());
                    let n = self.nodes.len() - 1;
                    self.nodes[node].children.insert(c, n);
                    n
                }
            };
        }
        let term = &mut self.nodes[node].terminal;
        if !term.iter().any(|(e, a)| *e == entity && a == key) {
            term.push((entity, key.to_string()));
        }
    }

    /// 最长优先扫描：每个起点走到最深，沿途每个终结点都算命中
    fn scan(&self, text: &str) -> Vec<RawHit> {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut hits: Vec<RawHit> = Vec::new();
        for s in 0..chars.len() {
            let mut node = 0usize;
            for i in s..chars.len() {
                let Some(&next) = self.nodes[node].children.get(&fold(chars[i].1)) else {
                    break;
                };
                node = next;
                if !self.nodes[node].terminal.is_empty() {
                    let end = chars.get(i + 1).map(|(b, _)| *b).unwrap_or(text.len());
                    for (e, alias) in &self.nodes[node].terminal {
                        hits.push(RawHit {
                            start: chars[s].0,
                            end,
                            entity: *e,
                            alias: alias.clone(),
                        });
                    }
                }
            }
        }
        hits
    }
}

// ---------- 设定集 ----------

/// 设定集：实体表 + 别名 trie + id 索引 + 一跳关系图（§6.12 加载时构建）
#[derive(Debug)]
pub struct Codex {
    entities: Vec<CodexEntity>,
    by_id: HashMap<String, usize>,
    trie: AliasTrie,
    /// 无向一跳 always_with 邻接表。设计 §6.2 的注释要求"提到便签 → 连带激活小雨"
    /// （边声明在小雨侧），§6.3 的措辞是"已激活实体的一跳关系连带激活"——取两者
    /// 并集即无向边，且**只走一跳**（牵引来的实体不再向外牵引）。
    always_with: HashMap<usize, Vec<usize>>,
}

impl Codex {
    /// 全量收录（含 draft/retired，界面与收件箱要用），由 activate 过滤。
    /// 重复 id 以首个为准（一致性校验在 §6.8 管线里做）。
    pub fn build(entities: Vec<CodexEntity>) -> Codex {
        let mut by_id: HashMap<String, usize> = HashMap::new();
        let mut trie = AliasTrie::new();
        for (i, e) in entities.iter().enumerate() {
            by_id.entry(e.id.clone()).or_insert(i);
            trie.insert(&e.name, i);
            for a in &e.aliases {
                trie.insert(a, i);
            }
        }
        let mut always_with: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, e) in entities.iter().enumerate() {
            for r in e.relations.iter().filter(|r| r.always_with) {
                if let Some(&j) = by_id.get(r.to.trim()) {
                    if i == j {
                        continue;
                    }
                    always_with.entry(i).or_default().push(j);
                    always_with.entry(j).or_default().push(i);
                }
            }
        }
        for v in always_with.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Codex {
            entities,
            by_id,
            trie,
            always_with,
        }
    }

    pub fn entities(&self) -> &[CodexEntity] {
        &self.entities
    }

    pub fn get(&self, id: &str) -> Option<&CodexEntity> {
        self.by_id.get(id.trim()).map(|&i| &self.entities[i])
    }

    /// 别名扫描（含 draft/retired：解析预览要用全量）。trie 最长优先；同一实体的
    /// 重叠命中只留最长；不同实体的重叠命中都保留。结果按 (起点, id) 升序确定。
    pub fn scan_mentions(&self, text: &str) -> Vec<Mention> {
        let mut hits = self.trie.scan(text);
        // 同实体：长命中优先（"夜班管理员" 赢过其中的 "管理员"）
        hits.sort_by(|a, b| {
            a.entity
                .cmp(&b.entity)
                .then_with(|| (b.end - b.start).cmp(&(a.end - a.start)))
                .then_with(|| a.start.cmp(&b.start))
        });
        let mut kept: Vec<RawHit> = Vec::new();
        for h in hits {
            let overlaps = kept
                .iter()
                .any(|k| k.entity == h.entity && h.start < k.end && h.end > k.start);
            if !overlaps {
                kept.push(h);
            }
        }
        kept.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then_with(|| self.entities[a.entity].id.cmp(&self.entities[b.entity].id))
                .then_with(|| b.end.cmp(&a.end))
        });
        kept.into_iter()
            .map(|h| Mention {
                id: self.entities[h.entity].id.clone(),
                alias: h.alias,
                at: h.start,
            })
            .collect()
    }

    /// "实体.秘密" 路径解析：实体 id 自带点（char.小雨），故从最长前缀往下试。
    fn split_secret_path(&self, path: &str) -> Option<(usize, String)> {
        let mut cut = path.len();
        while let Some(pos) = path[..cut].rfind('.') {
            let (head, tail) = (&path[..pos], &path[pos + 1..]);
            if let Some(&i) = self.by_id.get(head.trim()) {
                let secret = tail.trim().strip_prefix("secrets.").unwrap_or(tail.trim());
                return Some((i, secret.to_string()));
            }
            cut = pos;
        }
        None
    }

    /// id / name / 别名精确命中（拉丁大小写不敏感）
    fn find_by_ref(&self, s: &str) -> Option<usize> {
        self.entities.iter().position(|e| ref_matches(e, s))
    }
}

/// 一轮激活的中间态
struct Hit {
    strength: u8,
    depth: Depth,
    reasons: Vec<String>,
}

impl Default for Hit {
    fn default() -> Self {
        Hit {
            strength: 0,
            depth: Depth::Line,
            reasons: Vec::new(),
        }
    }
}

/// 参与预算裁剪的候选
struct Item {
    idx: usize,
    strength: u8,
    stage: Stage,
    reasons: Vec<String>,
    text: String,
    tokens: usize,
}

fn bump(hits: &mut BTreeMap<usize, Hit>, i: usize, strength: u8, depth: Depth, reason: String) {
    let h = hits.entry(i).or_default();
    h.strength = h.strength.max(strength);
    h.depth = h.depth.max(depth);
    if !h.reasons.iter().any(|r| r == &reason) {
        h.reasons.push(reason);
    }
}

/// 原因按激活源权重降序（稳定排序：滞回/降级保持追加序），输出确定
fn sorted_reasons(mut reasons: Vec<String>) -> Vec<String> {
    reasons.sort_by_key(|r| std::cmp::Reverse(reason_weight(r)));
    reasons
}

fn reason_weight(reason: &str) -> u8 {
    if reason.starts_with("揭示") {
        W_REVEAL
    } else if reason.starts_with("在场") {
        W_PRESENCE
    } else if reason.starts_with("常驻") {
        W_CONSTANT
    } else if reason.starts_with("牵引") {
        W_TRACTION
    } else if reason.starts_with("提及") {
        W_MENTION
    } else {
        W_HYSTERESIS
    }
}

/// 输出次序的类型位（§6.3 规则 6）：char/place/item/event/org/rule/concept/note，未知类型排最后
fn type_rank(ty: &str) -> usize {
    ENTITY_TYPES
        .iter()
        .position(|t| *t == ty)
        .unwrap_or(ENTITY_TYPES.len())
}

/// 实体类型中文名（卡片头 【小雨·人】）
pub fn type_cn(ty: &str) -> &'static str {
    match ty {
        "char" => "人",
        "place" => "地",
        "item" => "物",
        "event" => "事",
        "org" => "组织",
        "rule" => "法则",
        "concept" => "概念",
        "note" => "笔记",
        _ => "设定",
    }
}

/// id / name / 别名精确命中（trim + 拉丁大小写不敏感）
fn ref_matches(e: &CodexEntity, s: &str) -> bool {
    eq_fold(&e.id, s) || eq_fold(&e.name, s) || e.aliases.iter().any(|a| eq_fold(a, s))
}

/// 地点实体专用宽容匹配：黑板地点是自由文本（"图书馆自习区"），
/// 含地点名/别名（≥2 字）即算在场。
fn place_contains(e: &CodexEntity, place: &str) -> bool {
    let hay = fold_str(place);
    let mut keys: Vec<&str> = vec![e.name.as_str()];
    keys.extend(e.aliases.iter().map(|a| a.as_str()));
    keys.into_iter().any(|k| {
        let k = fold_str(k);
        k.chars().count() >= 2 && hay.contains(&k)
    })
}

// ---------- 秘密可见性（§6.3 深卡 / §6.4 reveal）----------

/// 当前视角是否知情（决定深卡里是否出现该秘密）：
/// 1. known_by 含 "*" → 不设限；含**本视角** → 她一直知道（自己的秘密无需 reveal）；
/// 2. ctx.known 命中路径（实体.secrets.秘密 或 实体.秘密）——视角化的揭示集；
/// 3. 本轮 reveal 命中该秘密路径 / 命中其 revealed_by 声明值；
/// 4. 本轮 reveal 命中实体本身且该秘密 known_by 为空（公开秘密）。
fn secret_visible(e: &CodexEntity, name: &str, s: &Secret, ctx: &ActivationContext<'_>) -> bool {
    if s
        .known_by
        .iter()
        .any(|k| k.trim() == SECRET_KNOWN_BY_ANY || eq_fold(k, ctx.viewer))
    {
        return true;
    }
    let p1 = format!("{}.secrets.{}", e.id, name);
    let p2 = format!("{}.{}", e.id, name);
    if ctx.known.contains(&p1) || ctx.known.contains(&p2) {
        return true;
    }
    for r in ctx.reveals {
        let r = r.trim();
        if r == p1 || r == p2 {
            return true;
        }
        if s.revealed_by.as_deref().is_some_and(|rb| eq_fold(rb, r)) {
            return true;
        }
        if eq_fold(r, &e.id) && s.known_by.is_empty() {
            return true;
        }
    }
    false
}

fn known_secrets(e: &CodexEntity, ctx: &ActivationContext<'_>) -> Vec<String> {
    e.secrets
        .iter()
        .filter(|(name, s)| secret_visible(e, name, s, ctx))
        .map(|(name, _)| name.clone())
        .collect()
}

/// 该实体的秘密是否在**当前视角的揭示集**里（她经历过那次 reveal——
/// 区别于先天的 known_by 名单：见证过的要升深卡，天生的只做门控）
fn witnessed_secret(e: &CodexEntity, ctx: &ActivationContext<'_>) -> bool {
    e.secrets.keys().any(|name| {
        let p1 = format!("{}.secrets.{}", e.id, name);
        let p2 = format!("{}.{}", e.id, name);
        ctx.known.contains(&p1) || ctx.known.contains(&p2)
    })
}

impl Codex {
    /// 五激活源 → 分级深度 → 预算降级裁撤 → 确定性排序的注入列表（§6.3）。
    pub fn activate(&self, ctx: &ActivationContext<'_>, budget: &CodexBudget) -> Vec<Activated> {
        // 地点：显式参数优先，其次黑板；并注入本轮视图供变体/门控/占位符共用
        let place: Option<&str> = ctx
            .place
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                ctx.blackboard
                    .get("place")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });
        let mut bb = ctx.blackboard.clone();
        if let Some(p) = place {
            bb.insert("place".to_string(), Value::String(p.to_string()));
        }
        let affects = affects_from_bb(&bb);

        let gated = |i: usize| -> bool {
            match self.entities[i].facts.get(WHEN_FACT_KEY) {
                Some(w) => !when_matches(w, &bb, ctx.day, ctx.clock, place),
                None => false,
            }
        };
        // 只有正史实体参与注入（§6.9），且要过实体级 when 门控（§6.10）
        let injectable = |i: usize| self.entities[i].is_canon() && !gated(i);
        // 在场类来源还要过生命周期（§6.5：离场/故去者不再在场激活）
        let presentable = |i: usize| {
            injectable(i)
                && self.entities[i]
                    .lifecycle
                    .as_ref()
                    .map(|l| l.present_at(ctx.day))
                    .unwrap_or(true)
        };

        let mut hits: BTreeMap<usize, Hit> = BTreeMap::new();

        // ---- 源 3：揭示（权重 5）→ 深卡 ----
        for r in ctx.reveals {
            let r = r.trim();
            if r.is_empty() {
                continue;
            }
            let mut targets: Vec<usize> = Vec::new();
            if let Some(&i) = self.by_id.get(r) {
                targets.push(i);
            } else if let Some((i, _)) = self.split_secret_path(r) {
                targets.push(i);
            } else if let Some(i) = self.find_by_ref(r) {
                targets.push(i);
            }
            // 声明式揭示：secrets.revealed_by 命中（如 "state:日常.夜谈"，§6.2/§6.4）
            for (i, e) in self.entities.iter().enumerate() {
                if e.secrets
                    .values()
                    .any(|s| s.revealed_by.as_deref().is_some_and(|x| eq_fold(x, r)))
                {
                    targets.push(i);
                }
            }
            targets.sort_unstable();
            targets.dedup();
            for i in targets {
                if injectable(i) {
                    bump(&mut hits, i, W_REVEAL, Depth::Deep, format!("揭示:{r}"));
                }
            }
        }

        // ---- 源 2：在场（权重 4）→ 卡片 ----
        if let Some(p) = place {
            for (i, e) in self.entities.iter().enumerate() {
                if !presentable(i) {
                    continue;
                }
                if ref_matches(e, p) || (e.ty == "place" && place_contains(e, p)) {
                    bump(
                        &mut hits,
                        i,
                        W_PRESENCE,
                        Depth::Card,
                        format!("在场:地点「{p}」"),
                    );
                }
            }
        }
        for a in ctx.actors {
            let a = a.trim();
            if a.is_empty() {
                continue;
            }
            for (i, e) in self.entities.iter().enumerate() {
                if presentable(i) && ref_matches(e, a) {
                    bump(&mut hits, i, W_PRESENCE, Depth::Card, format!("在场:{a}"));
                }
            }
        }

        // ---- 源 5：常驻（权重 3）→ 卡片起 ----
        for (i, e) in self.entities.iter().enumerate() {
            if !(e.constant || e.ty == "rule") || !injectable(i) {
                continue;
            }
            bump(&mut hits, i, W_CONSTANT, Depth::Card, "常驻".to_string());
        }

        // ---- 源 3b：已知秘密升级（M3.1 · 设计 §6.3 深卡）：实体已被剧情激活、
        //      且**当前视角的揭示集**里有它的秘密路径（她经历过那次 reveal）→ 升深卡。
        //      只升级、不激活——知道秘密不等于每轮都要想起它；先天的 known_by 名单
        //      （她一直知道自己的秘密）不触发升级，仍只做深卡门控（M2 的预算语义）；
        //      预算挤占由既有降级阶梯兜底（深卡→卡片→1 行→裁撤）。
        for (&i, h) in hits.iter_mut() {
            let e = &self.entities[i];
            if e.secrets.is_empty() {
                continue;
            }
            if h.depth < Depth::Deep && witnessed_secret(e, ctx) {
                h.depth = Depth::Deep;
                h.reasons.push("已知:该视角经历过揭示".to_string());
            }
        }

        // ---- 源 1：提及（权重 1）→ 1 行 ----
        let mut by_entity: BTreeMap<usize, Vec<Mention>> = BTreeMap::new();
        for m in self.scan_mentions(ctx.window_text) {
            if let Some(&i) = self.by_id.get(&m.id) {
                by_entity.entry(i).or_default().push(m);
            }
        }
        for (i, ms) in by_entity {
            if !injectable(i) {
                continue;
            }
            let h = hits.entry(i).or_default();
            h.strength = h.strength.max(W_MENTION);
            h.depth = h.depth.max(Depth::Line);
            for m in ms.iter().take(MENTION_REASONS_MAX) {
                let reason = format!("提及:「{}」@{}", m.alias, m.at);
                if !h.reasons.iter().any(|r| r == &reason) {
                    h.reasons.push(reason);
                }
            }
            if ms.len() > MENTION_REASONS_MAX {
                h.reasons
                    .push(format!("提及:另{}处", ms.len() - MENTION_REASONS_MAX));
            }
        }

        // ---- 源 4：关系牵引（权重 2，只走一跳）----
        let seeds: Vec<usize> = hits.keys().copied().collect();
        for s in seeds {
            let Some(neighbors) = self.always_with.get(&s) else {
                continue;
            };
            for &n in neighbors {
                if presentable(n) {
                    bump(
                        &mut hits,
                        n,
                        W_TRACTION,
                        Depth::Card,
                        format!("牵引:由「{}」带出", self.entities[s].id),
                    );
                }
            }
        }

        // ---- 滞回（权重 0）：上一轮激活者保底卡片，消除"提一次闪一下"（§6.3）----
        if ctx.hold_rounds > 0 {
            for id in ctx.previously_active {
                let Some(&i) = self.by_id.get(id.trim()) else {
                    continue;
                };
                if !presentable(i) {
                    continue;
                }
                bump(
                    &mut hits,
                    i,
                    W_HYSTERESIS,
                    Depth::Card,
                    "滞回:上一轮激活（保底卡片）".to_string(),
                );
            }
        }

        // ---- 渲染 + 确定性排序（§6.3：强度降序, 类型序, id 升序）----
        let render = |idx: usize, stage: Stage| -> (String, usize) {
            let e = &self.entities[idx];
            let known = known_secrets(e, ctx);
            let cx = RenderCtx {
                bb: &bb,
                day: ctx.day,
                clock: ctx.clock,
                affects: &affects,
                known: &known,
            };
            let text = render_entity(e, stage, &cx);
            let tokens = estimate_tokens(&text);
            (text, tokens)
        };

        let mut items: Vec<Item> = Vec::with_capacity(hits.len());
        for (&idx, h) in &hits {
            let stage = Stage::of(h.depth);
            let (text, tokens) = render(idx, stage);
            items.push(Item {
                idx,
                strength: h.strength,
                stage,
                reasons: sorted_reasons(h.reasons.clone()),
                text,
                tokens,
            });
        }
        items.sort_by(|a, b| {
            b.strength
                .cmp(&a.strength)
                .then_with(|| {
                    type_rank(&self.entities[a.idx].ty).cmp(&type_rank(&self.entities[b.idx].ty))
                })
                .then_with(|| self.entities[a.idx].id.cmp(&self.entities[b.idx].id))
        });

        // 条数上限：先裁条数（排序靠后者先走）
        if budget.max_cards < items.len() {
            items.truncate(budget.max_cards);
        }

        // token 预算：先降级（深度高 → 激活弱 → 输出序靠后），再裁撤（激活弱先走）
        loop {
            let total: usize = items.iter().map(|it| it.tokens).sum();
            if total <= budget.tokens {
                break;
            }
            let mut demote: Option<usize> = None;
            for (i, it) in items.iter().enumerate() {
                if it.stage <= Stage::Line {
                    continue;
                }
                let better = match demote {
                    None => true,
                    Some(j) => {
                        let cur = &items[j];
                        (it.stage, std::cmp::Reverse(it.strength), i)
                            > (cur.stage, std::cmp::Reverse(cur.strength), j)
                    }
                };
                if better {
                    demote = Some(i);
                }
            }
            if let Some(i) = demote {
                let next = items[i].stage.demote();
                items[i].stage = next;
                let reason = demote_reason(next);
                if !reason.is_empty() && !items[i].reasons.iter().any(|r| r == reason) {
                    items[i].reasons.push(reason.to_string());
                }
                let (text, tokens) = render(items[i].idx, next);
                items[i].text = text;
                items[i].tokens = tokens;
                continue;
            }
            let mut drop: Option<usize> = None;
            for (i, it) in items.iter().enumerate() {
                let better = match drop {
                    None => true,
                    Some(j) => {
                        let cur = &items[j];
                        (std::cmp::Reverse(it.strength), i)
                            > (std::cmp::Reverse(cur.strength), j)
                    }
                };
                if better {
                    drop = Some(i);
                }
            }
            match drop {
                Some(i) => {
                    items.remove(i);
                }
                None => break,
            }
        }

        items
            .into_iter()
            .map(|it| {
                let e = &self.entities[it.idx];
                Activated {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    ty: e.ty.clone(),
                    depth: it.stage.depth(),
                    reasons: it.reasons,
                    text: it.text,
                    tokens: it.tokens,
                }
            })
            .collect()
    }
}

// ---------- 渲染 ----------

struct RenderCtx<'a> {
    bb: &'a BTreeMap<String, Value>,
    day: i64,
    clock: &'a str,
    affects: &'a [String],
    known: &'a [String],
}

/// 单实体卡片文本（Activated::text 与 render_block 共用同一函数）
fn render_entity(e: &CodexEntity, stage: Stage, cx: &RenderCtx<'_>) -> String {
    // one_liner 也可被 variants/versions 覆盖（周期/史变都可能改一句话简介）
    let one = e
        .facet_at("one_liner", cx.day, cx.clock, cx.bb)
        .map(|v| value_text(&v))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| e.one_liner.clone());
    let mut out = format!("【{}·{}】{}", e.name, type_cn(&e.ty), fill_bb(&one, cx.bb));

    if stage >= Stage::Card {
        if let Some(cur) = live_line(e, cx.bb) {
            out.push(' ');
            out.push_str(&cur);
        }
        for (label, text) in fact_lines(e, cx) {
            out.push('\n');
            out.push_str(&label);
            out.push(':');
            out.push_str(&text);
        }
    }

    if stage == Stage::Deep {
        for name in cx.known {
            let Some(s) = e.secrets.get(name) else {
                continue;
            };
            let content = fill_bb(&s.content, cx.bb);
            if content.is_empty() {
                continue;
            }
            out.push('\n');
            out.push_str(&format!(
                "秘密({name}):{}",
                truncate(&content, MAX_FACT_CHARS)
            ));
        }
    }

    // 卡片与深卡恒带 anchors 一行（§6.3）；精简卡保留它，直到降为 1 行才随卡退役
    if stage >= Stage::CardLean {
        let anchors = e.anchors();
        if !anchors.is_empty() {
            out.push('\n');
            out.push_str("辨识点:");
            out.push_str(
                &anchors
                    .iter()
                    .map(|a| fill_bb(a, cx.bb))
                    .collect::<Vec<_>>()
                    .join("；"),
            );
        }
    }
    out
}

/// ▸当前：live 字段从黑板取值（嵌套 bb[实体id][字段] → 平铺 bb["实体id.字段"]
/// → 全局 bb[字段]），多字段以逗号连接（§6.3 的 ▸当前:当班,情绪平静）。
fn live_line(e: &CodexEntity, bb: &BTreeMap<String, Value>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for field_name in &e.live {
        let v = bb
            .get(&e.id)
            .and_then(|m| m.get(field_name.as_str()))
            .or_else(|| bb.get(&format!("{}.{}", e.id, field_name)))
            .or_else(|| bb.get(field_name.as_str()));
        let Some(v) = v else { continue };
        let t = placeholder_text(v);
        if !t.is_empty() {
            parts.push(t);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("▸当前:{}", parts.join(",")))
    }
}

/// facts 择要成行：优先级表取前 MAX_FACT_LINES 条，再按当前情绪补变体行（§6.3）
fn fact_lines(e: &CodexEntity, cx: &RenderCtx<'_>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (path, label) in FACT_TABLE {
        if out.len() >= MAX_FACT_LINES {
            break;
        }
        let Some(v) = e.facet_at(path, cx.day, cx.clock, cx.bb) else {
            continue;
        };
        let text = value_text(&v);
        if text.is_empty() {
            continue;
        }
        out.push((
            (*label).to_string(),
            truncate(&fill_bb(&text, cx.bb), MAX_FACT_CHARS),
        ));
    }

    // 状态词表按需注入：情绪未激活时零开销
    let mut affect_lines = 0usize;
    for a in cx.affects {
        if affect_lines >= MAX_AFFECT_LINES {
            break;
        }
        let mut seg: Vec<String> = Vec::new();
        for path in [
            format!("speech.by_affect.{a}"),
            format!("mannerisms.by_affect.{a}"),
            format!("tells.{a}"),
        ] {
            if let Some(v) = e.facet_at(&path, cx.day, cx.clock, cx.bb) {
                let t = value_text(&v);
                if !t.is_empty() {
                    seg.push(t);
                }
            }
        }
        if seg.is_empty() {
            continue;
        }
        out.push((
            format!("情绪({a})"),
            truncate(&fill_bb(&seg.join("；"), cx.bb), MAX_FACT_CHARS),
        ));
        affect_lines += 1;
    }
    out
}

/// 当前情绪（§6.3 状态词表命中）：黑板可选键 affect / affects / emotion
/// 或 psyche 下的情绪槽，值可为情绪名、名字数组，或 {name=…} 对象数组。
/// 心理运行时（M2.5）写出的是 psyche.affects（复数），单数 affect 是 M1 遗留形态——两侧都认。
fn affects_from_bb(bb: &BTreeMap<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["affect", "affects", "emotion"] {
        if let Some(v) = bb.get(key) {
            push_affects(v, &mut out);
        }
    }
    for key in ["affects", "affect"] {
        if let Some(v) = bb.get("psyche").and_then(|p| p.get(key)) {
            push_affects(v, &mut out);
        }
    }
    out
}

fn push_affects(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Array(a) => {
            for x in a {
                push_affects(x, out);
            }
        }
        Value::Object(m) => {
            if let Some(n) = m.get("name").and_then(|n| n.as_str()) {
                let n = n.trim();
                if !n.is_empty() && !out.iter().any(|x| eq_fold(x, n)) {
                    out.push(n.to_string());
                }
            }
        }
        Value::String(s) => {
            let s = s.trim();
            if !s.is_empty() && !out.iter().any(|x| eq_fold(x, s)) {
                out.push(s.to_string());
            }
        }
        _ => {}
    }
}

/// 值 → 单行文本（对象不倾倒成 JSON，§6.3「择要」）
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => {
            if *b {
                "是".to_string()
            } else {
                "否".to_string()
            }
        }
        Value::Array(a) => a
            .iter()
            .map(value_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("、"),
        Value::Object(_) | Value::Null => String::new(),
    }
}

/// 占位符取值 → 文本（对象退化为紧凑 JSON，其余同 value_text）
fn placeholder_text(v: &Value) -> String {
    match v {
        Value::Object(_) => serde_json::to_string(v).unwrap_or_default(),
        other => value_text(other),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// 紧凑实体卡多行文本（B3 槽内容，不含外层 world 标签；§6.3）
pub fn render_block(list: &[Activated]) -> String {
    list.iter()
        .filter(|a| !a.text.is_empty())
        .map(|a| a.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------- 占位符填充（§6.2 content 类字段）----------

/// 填充 {{bb.key}} 与 {{persona.name}}（组装时调用；§6.2）。
///
/// - bb.* 支持点分路径（{{bb.char.小雨.status}}），先整体键再逐层；缺失填空串；
/// - persona 命名空间：{{persona.name}} / {{persona}} 取 persona，缺失填空串；
/// - 不属于这两个命名空间的 {{…}} 原样保留（可能是别层的模板）。
pub fn fill_placeholders(text: &str, bb: &BTreeMap<String, Value>, persona: &str) -> String {
    let with_bb = fill_named(text, &|k| k.starts_with("bb."), &|k| {
        lookup_bb(bb, k.trim_start_matches("bb.")).map(placeholder_text)
    });
    fill_named(
        &with_bb,
        &|k| k == "persona" || k.starts_with("persona."),
        &|k| {
            if k == "persona" || k == "persona.name" {
                Some(persona.trim().to_string())
            } else {
                None // persona 的其他字段本模块拿不到 → 空
            }
        },
    )
}

/// 只填 {{bb.*}}（激活时用：persona 由宿主在组装时补）
fn fill_bb(text: &str, bb: &BTreeMap<String, Value>) -> String {
    if !text.contains("{{") {
        return text.to_string();
    }
    fill_named(text, &|k| k.starts_with("bb."), &|k| {
        lookup_bb(bb, k.trim_start_matches("bb.")).map(placeholder_text)
    })
}

fn fill_named(
    text: &str,
    owned: &dyn Fn(&str) -> bool,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                if owned(key) {
                    if let Some(v) = lookup(key) {
                        out.push_str(&v);
                    }
                } else {
                    out.push_str(&rest[start..start + 2 + end + 2]);
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn lookup_bb<'a>(bb: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a Value> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    if let Some(v) = bb.get(key) {
        return Some(v);
    }
    // 实体 id 自带点（char.小雨.status）：从长到短找黑板里的顶层键，再逐层下钻
    let mut cut = key.len();
    while let Some(pos) = key[..cut].rfind('.') {
        let (head, tail) = (&key[..pos], &key[pos + 1..]);
        if let Some(v) = bb.get(head) {
            let mut cur = v;
            let mut ok = true;
            for p in tail.split('.') {
                match cur.get(p) {
                    Some(next) => cur = next,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return Some(cur);
            }
        }
        cut = pos;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- 测试脚手架 ----------

    fn big() -> CodexBudget {
        CodexBudget {
            tokens: 100_000,
            max_cards: 16,
        }
    }

    fn ent(id: &str, ty: &str, name: &str, one_liner: &str) -> CodexEntity {
        CodexEntity {
            id: id.into(),
            ty: ty.into(),
            name: name.into(),
            aliases: Vec::new(),
            one_liner: one_liner.into(),
            facts: BTreeMap::new(),
            secrets: BTreeMap::new(),
            live: Vec::new(),
            relations: Vec::new(),
            status: STATUS_CANON.into(),
            constant: false,
            skip_if_remembered: false,
            lifecycle: None,
            variants: Vec::new(),
            versions: Vec::new(),
        }
    }

    /// 拥有所有权的上下文（借用给 ActivationContext，免得测试里到处写生命周期）
    struct Cx {
        window: String,
        place: Option<String>,
        actors: Vec<String>,
        reveals: Vec<String>,
        known: BTreeSet<String>,
        prev: BTreeSet<String>,
        day: i64,
        clock: String,
        hold_rounds: u32,
        bb: BTreeMap<String, Value>,
        viewer: String,
    }

    impl Cx {
        fn new(window: &str) -> Cx {
            Cx {
                window: window.into(),
                place: None,
                actors: Vec::new(),
                reveals: Vec::new(),
                known: BTreeSet::new(),
                prev: BTreeSet::new(),
                day: 1,
                clock: "20:00".into(),
                hold_rounds: 3,
                bb: BTreeMap::new(),
                viewer: "小雨".into(),
            }
        }
        /// 切换组装视角（M3.1 视角化用例）
        fn as_viewer(mut self, v: &str) -> Cx {
            self.viewer = v.into();
            self
        }
        fn place(mut self, p: &str) -> Cx {
            self.place = Some(p.into());
            self
        }
        fn actor(mut self, a: &str) -> Cx {
            self.actors.push(a.into());
            self
        }
        fn reveal(mut self, r: &str) -> Cx {
            self.reveals.push(r.into());
            self
        }
        fn known(mut self, k: &str) -> Cx {
            self.known.insert(k.into());
            self
        }
        fn prev(mut self, k: &str) -> Cx {
            self.prev.insert(k.into());
            self
        }
        fn day(mut self, d: i64) -> Cx {
            self.day = d;
            self
        }
        fn clock(mut self, c: &str) -> Cx {
            self.clock = c.into();
            self
        }
        fn hold(mut self, h: u32) -> Cx {
            self.hold_rounds = h;
            self
        }
        fn bb(mut self, k: &str, v: Value) -> Cx {
            self.bb.insert(k.into(), v);
            self
        }
        fn ctx(&self) -> ActivationContext<'_> {
            ActivationContext {
                window_text: &self.window,
                place: self.place.as_deref(),
                actors: &self.actors,
                reveals: &self.reveals,
                known: &self.known,
                previously_active: &self.prev,
                hold_rounds: self.hold_rounds,
                day: self.day,
                clock: &self.clock,
                blackboard: &self.bb,
                viewer: &self.viewer,
            }
        }
    }

    fn find<'a>(out: &'a [Activated], id: &str) -> &'a Activated {
        out.iter().find(|a| a.id == id).unwrap_or_else(|| {
            panic!(
                "未激活：{id}；实际：{:?}",
                out.iter().map(|a| a.id.as_str()).collect::<Vec<_>>()
            )
        })
    }

    /// 小雨（设计 §6.2 的示例实体形状）+ 图书馆 + 便签 + 常驻法则 + 草稿 + retired
    fn sample_codex() -> Codex {
        let xiaoyu = CodexEntity::from_value(&json!({
            "spec": "codex/1.0",
            "id": "char.小雨",
            "type": "char",
            "name": "小雨",
            "aliases": ["管理员", "夜班管理员"],
            "one_liner": "大学图书馆夜班管理员——左眼角一颗泪痣。",
            "facts": {
                "look": {
                    "impression": "旧毛衣、袖口的铅笔灰。",
                    "anchors": ["左眼角一颗泪痣", "母亲留下的旧胸牌"]
                },
                "speech": { "style": "句子很短，常用省略号。", "tics": ["……嗯。"] },
                "mannerisms": { "habits": ["说话时指尖绕头发", "递东西永远双手"] },
                "tells": { "忐忑": "指尖轻敲桌面。" },
                "motivation": "替母亲看完她没读完的书。",
                "schedule": "18:00–24:00 值班，周三休息。"
            },
            "secrets": {
                "工作牌": {
                    "content": "旧胸牌其实是已故母亲的。",
                    "known_by": ["小雨"],
                    "revealed_by": "state:日常.夜谈"
                }
            },
            "live": ["status"],
            "relations": [
                { "to": "place.图书馆", "kind": "works_at" },
                { "to": "item.便签", "kind": "fond_of", "always_with": true }
            ]
        }))
        .expect("解析小雨");

        let lib = CodexEntity::from_value(&json!({
            "id": "place.图书馆", "type": "place", "name": "图书馆",
            "one_liner": "23:50 闭馆铃的大学图书馆。",
            "facts": { "scene": "东侧自习区还亮着灯。" }
        }))
        .expect("解析图书馆");

        let note_item = ent("item.便签", "item", "便签", "她画小动物的黄色便签。");

        let mut rule = ent("rule.闭馆铃", "rule", "闭馆铃", "23:50 闭馆铃，雷打不动。");
        rule.constant = true;

        let mut draft = ent("char.旧书商", "char", "旧书商", "角落里收旧书的老头。");
        draft.status = STATUS_DRAFT.into();

        let mut retired = ent("place.旧书店", "place", "旧书店", "已经关门的旧书店。");
        retired.status = STATUS_RETIRED.into();

        Codex::build(vec![xiaoyu, lib, note_item, rule, draft, retired])
    }

    // ---------- 解析（§6.2）----------

    #[test]
    fn from_value_parses_sample_entity_shape() {
        let v = json!({
            "spec": "codex/1.0",
            "id": "char.小雨",
            "type": "char",
            "name": "小雨",
            "aliases": ["管理员", "夜班管理员", " 管理员 "],
            "one_liner": "大学图书馆夜班管理员——左眼角一颗泪痣，安静得像书架的一部分。",
            "facts": {
                "look": {
                    "impression": "旧毛衣、袖口的铅笔灰、说话前先看人一眼。",
                    "anchors": ["左眼角一颗泪痣", "母亲留下的旧胸牌"]
                },
                "speech": {
                    "style": "句子很短，常用省略号；被夸时会突然沉默。",
                    "tics": ["……嗯。", "（把东西轻轻推过来）"],
                    "by_affect": { "shy": "省略号变多、声音变小" }
                },
                "tells": { "忐忑": "指尖轻敲桌面，视线落在书页上却不翻页" },
                "schedule": "18:00–24:00 值班，周三休息。",
                "motivation": "守着夜班是为了替母亲看完她没读完的书。"
            },
            "secrets": {
                "工作牌": {
                    "content": "她挂着的旧胸牌，其实是已故母亲的。",
                    "known_by": ["小雨"],
                    "revealed_by": "state:日常.夜谈"
                }
            },
            "live": ["status"],
            "relations": [
                { "to": "place.图书馆", "kind": "works_at" },
                { "to": "item.便签", "kind": "fond_of", "always_with": true }
            ]
        });
        let e = CodexEntity::from_value(&v).expect("示例实体应解析成功");
        assert_eq!(e.id, "char.小雨");
        assert_eq!(e.ty, "char");
        assert_eq!(e.name, "小雨");
        assert_eq!(e.aliases, vec!["管理员", "夜班管理员"], "别名 trim + 去重");
        assert!(e.one_liner.contains("左眼角一颗泪痣"));
        assert_eq!(e.anchors(), vec!["左眼角一颗泪痣", "母亲留下的旧胸牌"]);
        assert_eq!(e.live, vec!["status"]);
        assert_eq!(e.relations.len(), 2);
        assert_eq!(e.relations[0].kind, "works_at");
        assert!(!e.relations[0].always_with);
        assert!(e.relations[1].always_with, "always_with 关系（牵引）");
        let s = &e.secrets["工作牌"];
        assert_eq!(s.known_by, vec!["小雨"]);
        assert_eq!(s.revealed_by.as_deref(), Some("state:日常.夜谈"));
        assert_eq!(e.status, STATUS_CANON, "缺 status 默认正史");
        assert!(e.is_canon());

        // 爱莉希雅形态：live 为空、tells 用中文键、facts 含嵌套对象
        let e2 = CodexEntity::from_value(&json!({
            "id": "char.爱莉希雅", "type": "char", "name": "爱莉希雅",
            "aliases": ["爱莉"],
            "one_liner": "粉色妖精小姐。",
            "facts": { "temperament": { "rise": "快", "impulsiveness": 0.8, "inference": true } },
            "live": {},
            "relations": [{ "to": "org.逐火之蛾", "kind": "member_of" }]
        }))
        .unwrap();
        assert!(e2.live.is_empty());
        assert_eq!(e2.relations[0].kind, "member_of");
        assert!(!e2.relations[0].always_with);
    }

    #[test]
    fn from_value_requires_id_name_type() {
        let cases = [
            (json!({ "name": "小雨", "type": "char" }), "id"),
            (json!({ "id": "char.小雨", "type": "char" }), "name"),
            (json!({ "id": "char.小雨", "name": "小雨" }), "type"),
        ];
        for (v, missing) in cases {
            let err = CodexEntity::from_value(&v).expect_err("缺字段应报错");
            assert!(err.contains(missing), "错误信息应点名 {missing}：{err}");
        }
        assert!(CodexEntity::from_value(&json!("not an object")).is_err());
        assert!(
            CodexEntity::from_value(&json!({ "id": "  ", "name": "x", "type": "char" })).is_err(),
            "空白 id 视为缺失"
        );
    }

    #[test]
    fn from_value_defaults_and_shorthands() {
        let e = CodexEntity::from_value(&json!({
            "id": "note.拆迁", "type": "note", "name": "拆迁",
            "aliases": "传闻",
            "content": "听说月底就要拆了。",
            "status": "Draft",
            "constant": 1,
            "lifecycle": "dead",
            "secrets": { "真相": "其实是他自己签的字。" },
            "variants": [{ "when": { "day": 20 }, "facets": { "scene": "公告已贴出" } }],
            "versions": [{ "day": 15, "facet": "one_liner", "value": "新的说法", "note": "第15天" }]
        }))
        .unwrap();
        assert_eq!(e.aliases, vec!["传闻"], "单串别名兼容");
        assert_eq!(e.one_liner, "听说月底就要拆了。", "content 兜底");
        assert_eq!(e.status, STATUS_DRAFT, "status 大小写归一");
        assert!(e.constant, "数字 1 视为 true");
        assert_eq!(e.lifecycle.unwrap().status, "dead", "lifecycle 简写");
        assert_eq!(e.secrets["真相"].content, "其实是他自己签的字。");
        assert_eq!(e.variants.len(), 1, "facets 展开成多个 variant");
        assert_eq!(e.variants[0].facet, "scene");
        assert_eq!(e.versions[0].from_day, 15, "day 是 from_day 的别名");
        assert_eq!(e.versions[0].note.as_deref(), Some("第15天"));
    }

    #[test]
    fn anchors_read_from_look_and_survive_single_string() {
        let one = CodexEntity::from_value(&json!({
            "id": "char.甲", "type": "char", "name": "甲", "one_liner": "甲。",
            "facts": { "look": { "anchors": "只有一条辨识点" } }
        }))
        .unwrap();
        assert_eq!(one.anchors(), vec!["只有一条辨识点"]);
        let none = ent("char.乙", "char", "乙", "乙。");
        assert!(none.anchors().is_empty());
    }

    // ---------- trie 扫描（§6.12）----------

    #[test]
    fn trie_longest_first_and_same_entity_overlap_dropped() {
        let mut e = ent("char.小雨", "char", "小雨", "夜班管理员。");
        e.aliases = vec!["管理员".into(), "夜班管理员".into()];
        let codex = Codex::build(vec![e]);
        let ms = codex.scan_mentions("夜班管理员来了");
        assert_eq!(ms.len(), 1, "同实体重叠只留最长：{ms:?}");
        assert_eq!(ms[0].alias, "夜班管理员");
        assert_eq!(ms[0].at, 0);
        // 分离的两处命中都保留（起点不同、不重叠）
        let ms2 = codex.scan_mentions("管理员走了，夜班管理员来了");
        assert_eq!(ms2.len(), 2, "{ms2:?}");
        assert_eq!(ms2[0].at, 0);
        assert!(ms2[1].at > 0);
    }

    #[test]
    fn trie_cjk_byte_offsets_and_latin_case_insensitive() {
        let mut e = ent("char.小雨", "char", "小雨", "管理员。");
        e.aliases = vec!["Lily".into()];
        let codex = Codex::build(vec![e]);
        let text = "他说小雨来了";
        let ms = codex.scan_mentions(text);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].alias, "小雨");
        assert_eq!(ms[0].at, "他说".len(), "at 是字节偏移");
        assert_eq!(&text[ms[0].at..ms[0].at + ms[0].alias.len()], "小雨");
        for t in ["lily 在吗", "LILY 在吗", "Lily 在吗"] {
            assert_eq!(codex.scan_mentions(t).len(), 1, "拉丁大小写不敏感：{t}");
        }
        assert!(codex.scan_mentions("lili").is_empty(), "不因折叠而误命中");
        assert!(codex.scan_mentions("").is_empty());
    }

    #[test]
    fn trie_different_entities_overlap_both_kept() {
        let mut a = ent("char.小雨", "char", "小雨", "a");
        a.aliases = vec!["管理员".into()];
        let b = ent("char.管理员甲", "char", "管理员甲", "b");
        let codex = Codex::build(vec![a, b]);
        let ms = codex.scan_mentions("管理员甲来了");
        let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
        assert!(
            ids.contains(&"char.小雨") && ids.contains(&"char.管理员甲"),
            "{ms:?}"
        );
        assert_eq!(ids, vec!["char.小雨", "char.管理员甲"], "起点相同时按 id 升序");
    }

    // ---------- 五激活源（§6.3）----------

    #[test]
    fn five_activation_sources() {
        let codex = sample_codex();

        // ① 提及：别名命中 → 1 行
        let out = codex.activate(&Cx::new("管理员正在整理书架。").ctx(), &big());
        let xy = find(&out, "char.小雨");
        assert_eq!(xy.depth, Depth::Line);
        assert!(
            xy.reasons.iter().any(|r| r.starts_with("提及:「管理员」")),
            "{:?}",
            xy.reasons
        );
        assert_eq!(xy.text, "【小雨·人】大学图书馆夜班管理员——左眼角一颗泪痣。");

        // ② 在场：地点（id 命中）→ 卡片
        let out = codex.activate(&Cx::new("").place("place.图书馆").ctx(), &big());
        let lib = find(&out, "place.图书馆");
        assert_eq!(lib.depth, Depth::Card);
        assert!(
            lib.reasons.iter().any(|r| r.starts_with("在场:地点")),
            "{:?}",
            lib.reasons
        );
        assert!(lib.text.contains("景象:东侧自习区还亮着灯。"), "{}", lib.text);

        // ③ 在场：在场者（名字命中）→ 卡片
        let out = codex.activate(&Cx::new("").actor("小雨").ctx(), &big());
        let xy = find(&out, "char.小雨");
        assert_eq!(xy.depth, Depth::Card);
        assert!(xy.reasons.contains(&"在场:小雨".to_string()), "{:?}", xy.reasons);

        // ④ 揭示：id.secret 路径 → 深卡 + 已知秘密
        let out = codex.activate(
            &Cx::new("")
                .reveal("char.小雨.工作牌")
                .known("char.小雨.secrets.工作牌")
                .ctx(),
            &big(),
        );
        let xy = find(&out, "char.小雨");
        assert_eq!(xy.depth, Depth::Deep);
        assert!(
            xy.reasons.iter().any(|r| r == "揭示:char.小雨.工作牌"),
            "{:?}",
            xy.reasons
        );
        assert!(xy.text.contains("秘密(工作牌):旧胸牌其实是已故母亲的。"), "{}", xy.text);

        // ⑤ 常驻：rule 无条件注入（空窗口下只有它）
        let out = codex.activate(&Cx::new("").ctx(), &big());
        assert_eq!(
            out.len(),
            1,
            "空窗口只有常驻：{:?}",
            out.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
        assert_eq!(out[0].id, "rule.闭馆铃");
        assert!(out[0].reasons.contains(&"常驻".to_string()));

        // ⑥ 关系牵引：提"便签"带出"小雨"（边声明在小雨侧，无向一跳）
        let out = codex.activate(&Cx::new("桌上的便签被风吹起。").ctx(), &big());
        assert_eq!(find(&out, "item.便签").depth, Depth::Line);
        let xy = find(&out, "char.小雨");
        assert_eq!(xy.depth, Depth::Card);
        assert!(
            xy.reasons.iter().any(|r| r == "牵引:由「item.便签」带出"),
            "{:?}",
            xy.reasons
        );
    }

    #[test]
    fn traction_is_one_hop_only() {
        let a = ent("char.A", "char", "甲", "a");
        let mut b = ent("char.B", "char", "乙", "b");
        b.relations.push(Relation {
            to: "char.A".into(),
            kind: "friend".into(),
            always_with: true,
        });
        let mut c = ent("char.C", "char", "丙", "c");
        c.relations.push(Relation {
            to: "char.B".into(),
            kind: "friend".into(),
            always_with: true,
        });
        let codex = Codex::build(vec![a, b, c]);
        let out = codex.activate(&Cx::new("丙在说话。").ctx(), &big());
        let ids: Vec<&str> = out.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains(&"char.C"));
        assert!(ids.contains(&"char.B"), "一跳牵引：{ids:?}");
        assert!(!ids.contains(&"char.A"), "牵引不再向外扩散：{ids:?}");
    }

    #[test]
    fn constants_and_rule_type_always_inject() {
        let mut flag = ent("concept.时代", "concept", "时代", "拆迁倒计时。");
        flag.constant = true;
        let codex = Codex::build(vec![flag, ent("rule.世界法", "rule", "世界法", "夜晚不可出门。")]);
        let out = codex.activate(&Cx::new("毫无关联的一句话。").ctx(), &big());
        let ids: Vec<&str> = out.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["rule.世界法", "concept.时代"],
            "同权重按类型序（rule 在 concept 前）：{ids:?}"
        );
        assert!(out.iter().all(|a| a.depth == Depth::Card));
    }

    #[test]
    fn presence_matches_place_by_name_alias_and_free_text() {
        let mut lib = ent("place.图书馆", "place", "图书馆", "大学图书馆。");
        lib.aliases = vec!["馆里".into()];
        let codex = Codex::build(vec![lib]);
        for p in ["place.图书馆", "图书馆", "馆里", "图书馆自习区"] {
            let out = codex.activate(&Cx::new("").place(p).ctx(), &big());
            assert_eq!(out.len(), 1, "地点「{p}」应命中：{out:?}");
            assert!(out[0].reasons[0].starts_with("在场:地点"), "{:?}", out[0].reasons);
        }
        // 自由文本里不含地点名 → 不命中
        let out = codex.activate(&Cx::new("").place("天台").ctx(), &big());
        assert!(out.is_empty());
    }

    // ---------- 深度分级与 anchors（§6.3）----------

    #[test]
    fn line_has_no_anchors_card_and_deep_always_do() {
        let codex = sample_codex();
        // 1 行：只有 one_liner
        let out = codex.activate(&Cx::new("小雨在整理书架。").ctx(), &big());
        let line = find(&out, "char.小雨");
        assert_eq!(line.depth, Depth::Line);
        assert!(!line.text.contains("辨识点:"), "{}", line.text);
        assert!(!line.text.contains("外貌:"));
        assert_eq!(line.text.lines().count(), 1);

        // 卡片：facts + 当前 + anchors
        let out = codex.activate(
            &Cx::new("")
                .actor("小雨")
                .bb("char.小雨", json!({ "status": "当班" }))
                .ctx(),
            &big(),
        );
        let card = find(&out, "char.小雨");
        assert_eq!(card.depth, Depth::Card);
        assert!(card.text.contains("\n外貌:"), "{}", card.text);
        assert!(card.text.contains("▸当前:当班"), "{}", card.text);
        assert!(
            card.text.contains("辨识点:左眼角一颗泪痣；母亲留下的旧胸牌"),
            "{}",
            card.text
        );

        // 深卡：卡片 + 已知秘密，anchors 仍在
        let out = codex.activate(
            &Cx::new("").reveal("char.小雨").known("char.小雨.工作牌").ctx(),
            &big(),
        );
        let deep = find(&out, "char.小雨");
        assert_eq!(deep.depth, Depth::Deep);
        assert!(deep.text.contains("秘密(工作牌):"), "{}", deep.text);
        assert!(deep.text.contains("辨识点:"));
    }

    #[test]
    fn live_current_line_reads_blackboard_three_layouts() {
        let mut e = ent("char.小雨", "char", "小雨", "管理员。");
        e.live = vec!["status".into(), "mood".into()];
        let codex = Codex::build(vec![e]);
        let out = codex.activate(
            &Cx::new("").actor("小雨")
                .bb("char.小雨", json!({ "status": "当班", "mood": "平静" }))
                .ctx(),
            &big(),
        );
        assert!(out[0].text.contains("▸当前:当班,平静"), "{}", out[0].text);
        let out = codex.activate(
            &Cx::new("").actor("小雨")
                .bb("char.小雨.mood", json!("困倦"))
                .bb("status", json!("闭馆中"))
                .ctx(),
            &big(),
        );
        assert!(out[0].text.contains("▸当前:闭馆中,困倦"), "{}", out[0].text);
        let out = codex.activate(&Cx::new("").actor("小雨").ctx(), &big());
        assert!(!out[0].text.contains("▸当前"), "黑板无值则不出当前行：{}", out[0].text);
    }

    #[test]
    fn affect_vocabulary_injected_only_when_matched() {
        let codex = sample_codex();
        let quiet = codex.activate(&Cx::new("").actor("小雨").ctx(), &big());
        assert!(
            !quiet[0].text.contains("情绪("),
            "情绪未激活时零开销：{}",
            quiet[0].text
        );
        let nervous = codex.activate(
            &Cx::new("").actor("小雨").bb("affect", json!("忐忑")).ctx(),
            &big(),
        );
        assert!(
            find(&nervous, "char.小雨").text.contains("情绪(忐忑):指尖轻敲桌面。"),
            "{}",
            find(&nervous, "char.小雨").text
        );
        let happy = codex.activate(
            &Cx::new("").actor("小雨").bb("affect", json!(["愉悦"])).ctx(),
            &big(),
        );
        assert!(!find(&happy, "char.小雨").text.contains("情绪("));
    }

    // ---------- 生命周期 / 门控 / 草稿 ----------

    #[test]
    fn lifecycle_blocks_presence_but_not_mention_or_flashback() {
        let dead = CodexEntity::from_value(&json!({
            "id": "char.阿雪", "type": "char", "name": "阿雪",
            "one_liner": "图书馆的前任管理员。",
            "lifecycle": { "status": "dead", "at_day": 20, "note": "第20天病故" }
        }))
        .unwrap();
        let codex = Codex::build(vec![dead]);
        // 生效之后：不再由在场源激活
        let out = codex.activate(&Cx::new("").actor("阿雪").day(30).ctx(), &big());
        assert!(out.is_empty(), "离场/故去者不再在场激活");
        // 但被提及仍激活（记忆、正史里照常存在）
        let out = codex.activate(&Cx::new("阿雪以前也这样。").day(30).ctx(), &big());
        assert_eq!(out.len(), 1);
        // flashback：生效之前照常在场
        let out = codex.activate(&Cx::new("").actor("阿雪").day(10).ctx(), &big());
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn entity_level_when_gates_injection() {
        let note = CodexEntity::from_value(&json!({
            "id": "note.拆迁传闻", "type": "note", "name": "拆迁传闻",
            "aliases": ["拆迁"],
            "content": "听说月底就要拆了。",
            "when": { "day": 2 }
        }))
        .unwrap();
        assert_eq!(note.one_liner, "听说月底就要拆了。", "content 兜底");
        let codex = Codex::build(vec![note]);
        let out = codex.activate(&Cx::new("拆迁").day(1).ctx(), &big());
        assert!(out.is_empty(), "门控不过 → 提及也不注入");
        let out = codex.activate(&Cx::new("拆迁").day(2).ctx(), &big());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].depth, Depth::Line);
        assert!(
            out[0].text.contains("【拆迁传闻·笔记】听说月底就要拆了。"),
            "{}",
            out[0].text
        );
    }

    #[test]
    fn draft_and_retired_never_inject_but_stay_in_build() {
        let codex = sample_codex();
        assert_eq!(codex.entities().len(), 6, "build 保留全量供界面用");
        assert!(codex.get("char.旧书商").is_some());
        assert_eq!(codex.get("char.小雨").unwrap().name, "小雨");
        assert!(codex.get("不存在").is_none());

        let out = codex.activate(&Cx::new("旧书商在角落里数钱。").ctx(), &big());
        assert!(out.iter().all(|a| a.id != "char.旧书商"), "草稿不注入");
        let out = codex.activate(&Cx::new("").place("旧书店").ctx(), &big());
        assert!(out.iter().all(|a| a.id != "place.旧书店"), "retired 不注入");
        // 但别名扫描（解析预览）看得见全量
        let ms = codex.scan_mentions("旧书商");
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].id, "char.旧书商");
    }

    // ---------- 滞回（§6.3）----------

    #[test]
    fn hysteresis_keeps_previous_round_at_card() {
        let codex = sample_codex();
        let out = codex.activate(
            &Cx::new("与设定无关的一句话。").prev("char.小雨").ctx(),
            &big(),
        );
        let xy = find(&out, "char.小雨");
        assert_eq!(xy.depth, Depth::Card);
        assert!(
            xy.reasons.iter().any(|r| r.starts_with("滞回")),
            "{:?}",
            xy.reasons
        );
        // hold_rounds = 0 → 关闭滞回
        let out = codex.activate(
            &Cx::new("与设定无关的一句话。")
                .prev("char.小雨")
                .hold(0)
                .ctx(),
            &big(),
        );
        assert!(out.iter().all(|a| a.id != "char.小雨"));
        // 上一轮集合里的陌生 id 不炸
        let out = codex.activate(&Cx::new("").prev("char.不存在").ctx(), &big());
        assert!(out.iter().all(|a| a.id != "char.不存在"));
    }

    // ---------- 预算与降级（§6.3 / §4.2）----------

    #[test]
    fn budget_demotes_deep_card_lean_line_then_culls_anchors_last() {
        let codex = Codex::build(vec![CodexEntity::from_value(&json!({
            "id": "char.小雨", "type": "char", "name": "小雨",
            "one_liner": "大学图书馆夜班管理员——左眼角一颗泪痣。",
            "facts": {
                "look": { "impression": "旧毛衣、袖口的铅笔灰。", "anchors": ["左眼角一颗泪痣"] },
                "motivation": "替母亲看完她没读完的书。"
            },
            "secrets": { "工作牌": { "content": "旧胸牌其实是已故母亲的。", "known_by": ["小雨"] } },
            "live": ["status"]
        }))
        .unwrap()]);
        let cx = Cx::new("")
            .reveal("char.小雨.工作牌")
            .known("char.小雨.secrets.工作牌")
            .bb("char.小雨", json!({ "status": "当班" }));

        let deep = codex.activate(&cx.ctx(), &big());
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].depth, Depth::Deep);
        assert!(deep[0].text.contains("秘密(工作牌)"));
        assert!(deep[0].text.contains("辨识点:"));

        // Deep → Card：秘密先裁，facts 与锚点仍在
        let step1 = codex.activate(
            &cx.ctx(),
            &CodexBudget { tokens: deep[0].tokens - 1, max_cards: 8 },
        );
        assert_eq!(step1[0].depth, Depth::Card);
        assert!(!step1[0].text.contains("秘密("), "{}", step1[0].text);
        assert!(step1[0].text.contains("\n外貌:"), "{}", step1[0].text);
        assert!(step1[0].text.contains("辨识点:左眼角一颗泪痣"), "{}", step1[0].text);
        assert!(step1[0].reasons.iter().any(|r| r.starts_with("降级:深卡")));

        // Card → 精简：细目裁掉，辨识点仍在（anchors 最后被裁）
        let step2 = codex.activate(
            &cx.ctx(),
            &CodexBudget { tokens: step1[0].tokens - 1, max_cards: 8 },
        );
        assert_eq!(step2[0].depth, Depth::Card, "精简卡仍报卡片级（辨识点保底）");
        assert!(!step2[0].text.contains("外貌:"), "{}", step2[0].text);
        assert!(!step2[0].text.contains("▸当前"), "{}", step2[0].text);
        assert!(step2[0].text.contains("辨识点:左眼角一颗泪痣"), "{}", step2[0].text);
        assert!(step2[0].tokens < step1[0].tokens);
        assert!(step2[0].degraded());

        // 精简 → 1 行：辨识点最后退役，one_liner 兜底
        let step3 = codex.activate(
            &cx.ctx(),
            &CodexBudget { tokens: step2[0].tokens - 1, max_cards: 8 },
        );
        assert_eq!(step3[0].depth, Depth::Line);
        assert!(!step3[0].text.contains("辨识点:"), "{}", step3[0].text);
        assert!(step3[0].text.contains("图书馆夜班管理员"), "{}", step3[0].text);
        assert_eq!(step3[0].text.lines().count(), 1);

        // 1 行 → 裁撤
        let step4 = codex.activate(
            &cx.ctx(),
            &CodexBudget { tokens: step3[0].tokens - 1, max_cards: 8 },
        );
        assert!(step4.is_empty(), "{step4:?}");
    }

    #[test]
    fn budget_culls_weakest_activation_first() {
        // 揭示（5）与提及（1）各一；预算只够最强的那条一行
        let mut strong = ent("char.强", "char", "强", "被揭示的人。");
        strong.secrets.insert(
            "底细".into(),
            Secret {
                content: "他是谁。".into(),
                known_by: vec!["*".into()],
                revealed_by: None,
            },
        );
        let weak = ent("char.弱", "char", "弱", "只是被提到的人。");
        let codex = Codex::build(vec![strong, weak]);
        let cx = Cx::new("弱也在这里。").reveal("char.强");
        let full = codex.activate(&cx.ctx(), &big());
        assert_eq!(full.len(), 2);
        assert_eq!(find(&full, "char.强").depth, Depth::Deep);
        assert_eq!(find(&full, "char.弱").depth, Depth::Line);

        let line_tokens = estimate_tokens("【强·人】被揭示的人。");
        let tight = codex.activate(
            &cx.ctx(),
            &CodexBudget {
                tokens: line_tokens,
                max_cards: 8,
            },
        );
        let ids: Vec<&str> = tight.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"char.强"), "{ids:?}");
        assert!(!ids.contains(&"char.弱"), "激活弱者先被裁撤：{ids:?}");
    }

    #[test]
    fn max_cards_limits_count() {
        let codex = Codex::build(vec![
            ent("rule.A", "rule", "甲法", "a"),
            ent("rule.B", "rule", "乙法", "b"),
            ent("rule.C", "rule", "丙法", "c"),
        ]);
        let out = codex.activate(
            &Cx::new("").ctx(),
            &CodexBudget { tokens: 10_000, max_cards: 2 },
        );
        assert_eq!(out.len(), 2);
        let ids: Vec<&str> = out.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["rule.A", "rule.B"], "同强度按 id 升序取前 N");
        assert!(codex
            .activate(&Cx::new("").ctx(), &CodexBudget { tokens: 10_000, max_cards: 0 })
            .is_empty());
    }

    // ---------- 时间三层（§6.5）----------

    #[test]
    fn versions_resolve_by_story_day() {
        let e = CodexEntity::from_value(&json!({
            "id": "char.小雨", "type": "char", "name": "小雨",
            "one_liner": "图书馆夜班管理员。",
            "facts": { "look": { "impression": "及腰长发", "anchors": ["左眼角一颗泪痣"] } },
            "versions": [
                { "from_day": 15, "facet": "look.impression", "value": "剪了短发，露出后颈", "note": "第15天剪发" },
                { "from_day": 30, "facet": "look.impression", "value": "短发已经长到肩" }
            ]
        }))
        .unwrap();
        let bb = BTreeMap::new();
        assert_eq!(
            e.facet_at("look.impression", 10, "20:00", &bb).unwrap(),
            json!("及腰长发")
        );
        assert_eq!(
            e.facet_at("look.impression", 14, "20:00", &bb).unwrap(),
            json!("及腰长发")
        );
        assert_eq!(
            e.facet_at("look.impression", 15, "20:00", &bb).unwrap(),
            json!("剪了短发，露出后颈"),
            "生效当日即切换"
        );
        assert_eq!(
            e.facet_at("look.impression", 30, "20:00", &bb).unwrap(),
            json!("短发已经长到肩"),
            "取 from_day ≤ day 的最大者"
        );
        assert_eq!(
            e.facet_at("look.anchors", 30, "20:00", &bb).unwrap(),
            json!(["左眼角一颗泪痣"]),
            "未被版本覆盖的 facet 仍取静态正史"
        );
        assert!(e.facet_at("不存在", 30, "20:00", &bb).is_none());

        // 渲染层同样按故事天解析（第 10 天的回忆杀里她仍是长发）
        let codex = Codex::build(vec![e]);
        let day10 = codex.activate(&Cx::new("").actor("小雨").day(10).ctx(), &big());
        assert!(day10[0].text.contains("及腰长发"), "{}", day10[0].text);
        let day30 = codex.activate(&Cx::new("").actor("小雨").day(30).ctx(), &big());
        assert!(day30[0].text.contains("外貌:短发已经长到肩"), "{}", day30[0].text);
    }

    #[test]
    fn variants_match_clock_place_and_blackboard() {
        let e = CodexEntity::from_value(&json!({
            "id": "place.图书馆", "type": "place", "name": "图书馆",
            "one_liner": "大学图书馆。",
            "facts": { "scene": "白天，人不多。", "rules": "不可饮食。" },
            "variants": [
                { "when": { "clock": "18:00-24:00" }, "facet": "scene", "value": "夜班，东侧自习区亮着灯。" },
                { "when": { "place": "天台" }, "facet": "scene", "value": "从天台看下去，图书馆像一块发光的砖。" },
                { "when": { "weather": "雨" }, "facet": "rules", "value": "雨天门口会摆伞架。" }
            ]
        }))
        .unwrap();
        let bb = BTreeMap::new();
        assert_eq!(
            e.facet_at("scene", 3, "19:30", &bb).unwrap(),
            json!("夜班，东侧自习区亮着灯。"),
            "时段变体"
        );
        assert_eq!(e.facet_at("scene", 3, "09:00", &bb).unwrap(), json!("白天，人不多。"));
        let mut bb2 = BTreeMap::new();
        bb2.insert("weather".to_string(), json!("雨"));
        assert_eq!(
            e.facet_at("rules", 3, "09:00", &bb2).unwrap(),
            json!("雨天门口会摆伞架。"),
            "条件键在黑板上等值匹配"
        );
        // 地点条件走黑板 place（activate 会把 ctx.place 注入本轮视图）。
        // 地点实体本身只在"人在图书馆"时激活，故用常驻法则实体验证条件命中
        let watcher = CodexEntity::from_value(&json!({
            "id": "rule.图书馆守则", "type": "rule", "name": "图书馆守则",
            "one_liner": "图书馆守则。",
            "facts": { "rules": "不可饮食。" },
            "variants": [
                { "when": { "place": "天台" }, "facet": "rules", "value": "天台规则：不许翻越栏杆。" }
            ]
        }))
        .unwrap();
        let codex = Codex::build(vec![e, watcher]);
        let out = codex.activate(&Cx::new("").place("天台").ctx(), &big());
        let guard = find(&out, "rule.图书馆守则");
        assert!(
            guard.text.contains("规则:天台规则：不许翻越栏杆。"),
            "{}",
            guard.text
        );
        let out = codex
            .activate(&Cx::new("").place("图书馆").clock("09:00").ctx(), &big());
        assert!(
            out.iter().any(|a| a.text.contains("白天，人不多。")),
            "{:?}",
            out.iter().map(|a| (&a.id, &a.text)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn when_matches_day_threshold_clock_and_blackboard() {
        let mut bb = BTreeMap::new();
        bb.insert("weather".to_string(), json!("雨"));
        bb.insert("actors".to_string(), json!(["小雨", "玩家"]));
        bb.insert("place".to_string(), json!("图书馆"));
        bb.insert("favorability".to_string(), json!(50));

        assert!(when_matches(&json!({}), &bb, 1, "20:00", None), "空对象恒命中");
        assert!(when_matches(&Value::Null, &bb, 1, "20:00", None), "非对象视为无约束");
        assert!(when_matches(&json!({ "day": 20 }), &bb, 20, "20:00", None), "day 是阈值");
        assert!(!when_matches(&json!({ "day": 20 }), &bb, 19, "20:00", None));
        assert!(when_matches(&json!({ "day": ["第2天", 7] }), &bb, 8, "20:00", None));
        assert!(when_matches(&json!({ "clock": "18:00-23:00" }), &bb, 1, "19:30", None));
        assert!(!when_matches(&json!({ "clock": "18:00-23:00" }), &bb, 1, "02:00", None));
        assert!(when_matches(&json!({ "clock": "22:00-02:00" }), &bb, 1, "01:00", None), "跨夜区间");
        assert!(when_matches(&json!({ "clock": "20:00" }), &bb, 1, "20:00", None), "精确匹配");
        assert!(when_matches(&json!({ "place": "图书馆" }), &bb, 1, "20:00", None));
        assert!(when_matches(&json!({ "place": "图书馆" }), &bb, 1, "20:00", Some("图书馆")));
        assert!(!when_matches(&json!({ "place": "天台" }), &bb, 1, "20:00", None));
        assert!(when_matches(&json!({ "actors": "小雨" }), &bb, 1, "20:00", None), "actors 是含关系");
        assert!(!when_matches(&json!({ "actors": "阿雪" }), &bb, 1, "20:00", None));
        assert!(when_matches(&json!({ "weather": "雨" }), &bb, 1, "20:00", None));
        assert!(!when_matches(&json!({ "weather": "晴" }), &bb, 1, "20:00", None));
        assert!(when_matches(&json!({ "favorability": 50.0 }), &bb, 1, "20:00", None), "数字跨 int/float");
        assert!(!when_matches(&json!({ "missing": 1 }), &bb, 1, "20:00", None), "黑板没有该键即不命中");
        // 多条件是"与"
        assert!(when_matches(
            &json!({ "day": 2, "weather": "雨", "actors": "小雨" }),
            &bb,
            3,
            "20:00",
            None
        ));
        assert!(!when_matches(
            &json!({ "day": 2, "weather": "晴" }),
            &bb,
            3,
            "20:00",
            None
        ));
    }

    // ---------- known_by / 深卡秘密（§6.3 / §6.4）----------

    #[test]
    fn known_by_gates_deep_secrets() {
        let open = CodexEntity::from_value(&json!({
            "id": "char.甲", "type": "char", "name": "甲", "one_liner": "甲。",
            "secrets": { "公开的底细": { "content": "人人都知道的底细。", "known_by": ["*"] } }
        }))
        .unwrap();
        let closed = CodexEntity::from_value(&json!({
            "id": "char.乙", "type": "char", "name": "乙", "one_liner": "乙。",
            "secrets": { "私事": { "content": "只有她自己知道。", "known_by": ["乙"] } }
        }))
        .unwrap();
        let codex = Codex::build(vec![open, closed]);

        // 揭示两实体：known_by = "*" 的直接进深卡；仅本人的要视角已知背书
        let cx = Cx::new("").reveal("char.甲").reveal("char.乙");
        let out = codex.activate(&cx.ctx(), &big());
        assert!(
            find(&out, "char.甲").text.contains("秘密(公开的底细)"),
            "{}",
            find(&out, "char.甲").text
        );
        assert!(
            !find(&out, "char.乙").text.contains("秘密(私事)"),
            "视角不知情则不出秘密：{}",
            find(&out, "char.乙").text
        );
        assert_eq!(find(&out, "char.乙").depth, Depth::Deep, "揭示仍给深卡深度");

        // 状态树 reveal 翻转 known 之后（§6.4），秘密出现
        let cx = cx.known("char.乙.secrets.私事");
        let out = codex.activate(&cx.ctx(), &big());
        assert!(
            find(&out, "char.乙").text.contains("秘密(私事):只有她自己知道。"),
            "{}",
            find(&out, "char.乙").text
        );

        // 声明式揭示来源 revealed_by 命中 → 视为已知（§6.2/§6.4）
        let e = CodexEntity::from_value(&json!({
            "id": "char.丙", "type": "char", "name": "丙", "one_liner": "丙。",
            "secrets": { "身世": { "content": "她不是本地人。", "known_by": ["丙"], "revealed_by": "state:日常.夜谈" } }
        }))
        .unwrap();
        let codex2 = Codex::build(vec![e]);
        let out = codex2.activate(&Cx::new("").reveal("state:日常.夜谈").ctx(), &big());
        assert!(out[0].text.contains("秘密(身世):她不是本地人。"), "{}", out[0].text);
        // known_by 为空的秘密：实体级揭示即公开
        let p = CodexEntity::from_value(&json!({
            "id": "char.丁", "type": "char", "name": "丁", "one_liner": "丁。",
            "secrets": { "公开": { "content": "大家都知道。" } }
        }))
        .unwrap();
        let codex3 = Codex::build(vec![p]);
        let out = codex3.activate(&Cx::new("").reveal("char.丁").ctx(), &big());
        assert!(out[0].text.contains("秘密(公开):大家都知道。"), "{}", out[0].text);
    }

    // ---------- anchors 保护级（§6.8）----------

    /// 正史增量合并（M3.8 · §6.9）：既有实体深合并、新实体追加、幂等可重放
    #[test]
    fn apply_grown_merges_idempotently_and_appends_new_entities() {
        let base = CodexEntity::from_value(&json!({
            "id": "char.小雨", "type": "char", "name": "小雨", "one_liner": "夜班管理员。",
            "aliases": ["管理员"],
            "facts": {
                "schedule": "18:00–24:00 值班",
                "look": { "impression": "旧毛衣。", "anchors": ["泪痣"] }
            },
            "relations": [ { "to": "place.图书馆", "kind": "works_at" } ]
        }))
        .unwrap();
        let grown = store::GrownFile {
            entities: [
                ("char.小雨".to_string(), json!({
                    "facts": {
                        "schedule": "周三也休息",
                        "look": { "impression2": "袖口的铅笔灰" }
                    },
                    "aliases": ["夜班之星"],
                    "relations": [
                        { "to": "place.图书馆", "kind": "works_at" },
                        { "to": "item.便签", "kind": "fond_of" }
                    ]
                })),
                ("char.墨墨".to_string(), json!({
                    "type": "char", "name": "墨墨",
                    "facts": { "look": { "impression": "一只黑猫" } }
                })),
            ]
            .into_iter()
            .collect(),
        };
        let once = apply_grown(vec![base], &grown);
        assert_eq!(once.len(), 2, "新实体追加");
        let xiaoyu = once.iter().find(|e| e.id == "char.小雨").unwrap();
        // 深合并：同键覆盖、兄弟键保留、嵌套对象并集
        assert_eq!(
            codex_static(&once, "char.小雨", "schedule"),
            Some(&json!("周三也休息"))
        );
        assert_eq!(
            codex_static(&once, "char.小雨", "look.impression"),
            Some(&json!("旧毛衣。")),
            "补丁没写的兄弟键不能丢"
        );
        assert_eq!(
            codex_static(&once, "char.小雨", "look.impression2"),
            Some(&json!("袖口的铅笔灰"))
        );
        assert!(xiaoyu.anchors().contains(&"泪痣".to_string()));
        assert!(xiaoyu.aliases.contains(&"夜班之星".to_string()));
        assert_eq!(xiaoyu.relations.len(), 2, "重复关系去重、新关系追加");
        // 新实体补丁即全量骨架
        let momo = once.iter().find(|e| e.id == "char.墨墨").unwrap();
        assert_eq!(momo.name, "墨墨");
        assert_eq!(momo.status, "canon", "收件箱确认进来的就是正史");
        // 幂等：同一补丁再应用一次，结果不变（重放安全）
        let twice = apply_grown(once.clone(), &grown);
        assert_eq!(twice, once, "apply_grown 必须幂等");
    }

    /// 测试辅助：应用后按 id+路径取静态 fact
    fn codex_static<'a>(
        entities: &'a [CodexEntity],
        id: &str,
        path: &str,
    ) -> Option<&'a Value> {
        entities.iter().find(|e| e.id == id).and_then(|e| static_fact(e, path))
    }

    #[test]
    fn anchors_conflict_rejects_proposals() {
        let e = CodexEntity::from_value(&json!({
            "id": "char.小雨", "type": "char", "name": "小雨", "one_liner": "小雨。",
            "facts": { "look": { "impression": "旧毛衣。", "anchors": ["左眼角一颗泪痣", "母亲留下的旧胸牌"] } }
        }))
        .unwrap();

        // 改辨识点 → 驳回
        let c = anchors_conflict(&e, &json!({ "facet": "look.anchors", "value": ["黑色短发"] }))
            .expect("改 anchors 应冲突");
        assert!(c.contains("左眼角一颗泪痣"), "{c}");
        assert!(c.contains("最高保护级"), "{c}");
        assert!(c.contains("retcon"), "{c}");

        // 整体替换 look 却把 anchors 丢了 → 驳回（防静默漂移）
        assert!(
            anchors_conflict(&e, &json!({ "facet": "look", "value": { "impression": "短发少女" } }))
                .is_some(),
            "整体替换 look 未带 anchors 应冲突"
        );

        // 原样带上 anchors → 放行
        assert!(anchors_conflict(
            &e,
            &json!({ "facet": "look", "value": {
                "impression": "短发少女",
                "anchors": ["母亲留下的旧胸牌", "左眼角一颗泪痣"]
            } })
        )
        .is_none());

        // 与 anchors 无关的提案 → 放行
        assert!(anchors_conflict(&e, &json!({ "facet": "motivation", "value": "想去看海" })).is_none());
        assert!(anchors_conflict(&e, &json!({ "facet": "look.impression", "value": "换了新毛衣" })).is_none());

        // 显式撤回标记 → 驳回；撤回不存在的锚点 → 放行
        assert!(anchors_conflict(&e, &json!({ "retract_anchors": ["左眼角一颗泪痣"] })).is_some());
        assert!(anchors_conflict(&e, &json!({ "retract_anchors": ["根本不存在的点"] })).is_none());

        // 无 anchors 的实体没有受保护对象
        let plain = ent("note.杂记", "note", "杂记", "一句话。");
        assert!(
            anchors_conflict(&plain, &json!({ "facet": "look.anchors", "value": ["随便"] })).is_none()
        );
    }

    // ---------- 次序、渲染与占位符 ----------

    #[test]
    fn ordering_is_strength_then_type_then_id() {
        let mut a = ent("char.甲", "char", "甲", "被提及。");
        a.aliases = vec!["甲".into()];
        let rule = ent("rule.法", "rule", "法", "常驻。");
        let mut z = ent("place.乙地", "place", "乙地", "被揭示的地点。");
        z.secrets.insert(
            "秘密".into(),
            Secret {
                content: "地点有秘密。".into(),
                known_by: vec!["*".into()],
                revealed_by: None,
            },
        );
        let codex = Codex::build(vec![a, rule, z]);
        let out = codex.activate(&Cx::new("甲在这里。").reveal("place.乙地").ctx(), &big());
        let ids: Vec<&str> = out.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["place.乙地", "rule.法", "char.甲"],
            "揭示(5) > 常驻(3) > 提及(1)"
        );
        // 同强度按类型序：char < place
        let codex2 = Codex::build(vec![
            ent("place.丙", "place", "丙", "p"),
            ent("char.丁", "char", "丁", "c"),
        ]);
        let out = codex2.activate(&Cx::new("丙和丁都来了。").ctx(), &big());
        let ids: Vec<&str> = out.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["char.丁", "place.丙"], "同强度按类型序");
    }

    #[test]
    fn render_block_shape_and_text_consistency() {
        let codex = sample_codex();
        let cx = Cx::new("管理员拿着便签。")
            .actor("小雨")
            .place("图书馆")
            .bb("char.小雨", json!({ "status": "当班" }));
        let out = codex.activate(&cx.ctx(), &big());
        assert!(out.len() >= 3, "{:?}", out.iter().map(|a| &a.id).collect::<Vec<_>>());

        let block = render_block(&out);
        assert!(!block.contains("<world>"), "render_block 不含外层标签");
        assert!(block.starts_with("【"));
        for a in &out {
            assert!(block.contains(&a.text), "拼接结果包含每张卡：{}", a.text);
            assert_eq!(a.tokens, estimate_tokens(&a.text), "token 记账与文本一致");
            assert_eq!(a.name, codex.get(&a.id).unwrap().name);
            assert_eq!(a.ty, codex.get(&a.id).unwrap().ty);
        }
        let lines: usize = out.iter().map(|a| a.text.lines().count()).sum();
        assert_eq!(block.lines().count(), lines, "各行拼接不引入额外空行");
        assert!(block.contains("【小雨·人】"));
        assert!(block.contains("【图书馆·地】"));
        assert!(block.contains("【便签·物】"));
        assert!(block.contains("【闭馆铃·法则】"), "rule → 法则：{block}");
        assert!(block.contains("▸当前:当班"), "{block}");
        assert_eq!(render_block(&[]), "");
    }

    #[test]
    fn activation_is_deterministic_across_calls_and_rebuilds() {
        let cx = Cx::new("管理员拿着便签，图书馆里很安静。")
            .actor("小雨")
            .place("图书馆")
            .reveal("char.小雨.工作牌")
            .known("char.小雨.secrets.工作牌")
            .prev("item.便签")
            .bb("char.小雨", json!({ "status": "当班" }))
            .bb("affect", json!("忐忑"));
        let a = sample_codex().activate(&cx.ctx(), &big());
        let b = sample_codex().activate(&cx.ctx(), &big());
        assert_eq!(a, b, "同样输入两次调用应完全一致");
        let budget = CodexBudget {
            tokens: 120,
            max_cards: 4,
        };
        let x = sample_codex().activate(&cx.ctx(), &budget);
        let y = sample_codex().activate(&cx.ctx(), &budget);
        assert_eq!(x, y, "预算裁剪路径同样确定");
        assert!(render_block(&x) == render_block(&y));
        // 次序稳定：激活强度降序
        let strengths: Vec<usize> = a
            .iter()
            .map(|it| {
                if it.reasons.iter().any(|r| r.starts_with("揭示")) {
                    5
                } else if it.reasons.iter().any(|r| r.starts_with("在场")) {
                    4
                } else if it.reasons.iter().any(|r| r.starts_with("常驻")) {
                    3
                } else if it.reasons.iter().any(|r| r.starts_with("牵引")) {
                    2
                } else if it.reasons.iter().any(|r| r.starts_with("提及")) {
                    1
                } else {
                    0
                }
            })
            .collect();
        let mut sorted = strengths.clone();
        sorted.sort_by(|p, q| q.cmp(p));
        assert_eq!(strengths, sorted, "输出序按激活强度降序");
    }

    #[test]
    fn fill_placeholders_bb_and_persona() {
        let mut bb = BTreeMap::new();
        bb.insert("weather".to_string(), json!("雨"));
        bb.insert("char.小雨".to_string(), json!({ "status": "当班" }));
        let text = "今天{{bb.weather}}，她{{bb.char.小雨.status}}。{{persona.name}}看着{{ missing }}。";
        assert_eq!(
            fill_placeholders(text, &bb, "夜读者"),
            "今天雨，她当班。夜读者看着{{ missing }}。"
        );
        assert_eq!(fill_placeholders("{{bb.none}}x", &bb, ""), "x", "缺失的 bb 键 → 空串");
        assert_eq!(fill_placeholders("{{other.k}}", &bb, "p"), "{{other.k}}", "别层的模板原样保留");
        assert_eq!(fill_placeholders("{{persona}}", &bb, "夜读者"), "夜读者");
        assert_eq!(fill_placeholders("{{bb.weather", &bb, ""), "{{bb.weather", "未闭合不吞文本");

        // 实体文本里的占位符在激活时就被填（persona 由宿主组装时补）
        let e = CodexEntity::from_value(&json!({
            "id": "note.天气", "type": "note", "name": "天气",
            "one_liner": "窗外正下着{{bb.weather}}。"
        }))
        .unwrap();
        let codex = Codex::build(vec![e]);
        let out = codex.activate(&Cx::new("天气").bb("weather", json!("雨")).ctx(), &big());
        assert!(out[0].text.contains("窗外正下着雨。"), "{}", out[0].text);
    }

    #[test]
    fn depth_labels_and_type_names() {
        assert!(Depth::Line < Depth::Card && Depth::Card < Depth::Deep);
        assert_eq!(Depth::Line.label(), "1 行");
        assert!(!Depth::Line.is_card_level());
        assert!(Depth::Card.is_card_level() && Depth::Deep.is_card_level());
        assert_eq!(type_cn("char"), "人");
        assert_eq!(type_cn("place"), "地");
        assert_eq!(type_cn("item"), "物");
        assert_eq!(type_cn("event"), "事");
        assert_eq!(type_cn("org"), "组织");
        assert_eq!(type_cn("rule"), "法则");
        assert_eq!(type_cn("concept"), "概念");
        assert_eq!(type_cn("note"), "笔记");
        assert_eq!(type_cn("奇物"), "设定");
        assert_eq!(type_rank("char"), 0);
        assert!(type_rank("note") < type_rank("未知类型"));
        assert_eq!(CodexBudget::default().max_cards, 12);
    }

    #[test]
    fn lua_shaped_entity_renders_full_card() {
        // Lua 的 ["键"] = {...} / 数组 / 空表 三种形态经 serde 转换后的典型形状
        let e = CodexEntity::from_value(&json!({
            "id": "char.爱莉希雅", "type": "char", "name": "爱莉希雅",
            "aliases": ["爱莉", "粉色妖精小姐", "真我"],
            "one_liner": "粉色妖精小姐。",
            "facts": {
                "look": { "impression": "粉发蓝瞳。", "anchors": ["粉色长发与蓝色瞳仁"] },
                "speech": { "by_affect": { "愉悦": "句尾「♪」变多。" } },
                "needs": ["被爱、被需要。", "有趣的人与有趣的事。"],
                "temperament": { "rise": "快", "impulsiveness": 0.8 }
            },
            "secrets": {
                "人之律者": { "content": "她是最早诞生的律者。", "known_by": ["爱莉希雅"], "revealed_by": "state:第三十一章" }
            },
            "live": [],
            "relations": [{ "to": "char.凯文·卡斯兰娜", "kind": "comrade" }]
        }))
        .unwrap();
        assert_eq!(e.anchors(), vec!["粉色长发与蓝色瞳仁"]);
        assert_eq!(e.secrets.len(), 1);
        assert_eq!(e.secrets["人之律者"].known_by, vec!["爱莉希雅"]);
        let codex = Codex::build(vec![e]);
        let out = codex.activate(
            &Cx::new("")
                .reveal("char.爱莉希雅")
                .known("char.爱莉希雅.人之律者")
                .bb("affect", json!("愉悦"))
                .ctx(),
            &big(),
        );
        let t = &out[0].text;
        assert!(t.starts_with("【爱莉希雅·人】粉色妖精小姐。"), "{t}");
        assert!(t.contains("外貌:粉发蓝瞳。"), "{t}");
        assert!(t.contains("需要:被爱、被需要。、有趣的人与有趣的事。"), "{t}");
        assert!(t.contains("情绪(愉悦):句尾「♪」变多。"), "{t}");
        assert!(t.contains("秘密(人之律者):她是最早诞生的律者。"), "{t}");
        assert!(t.contains("辨识点:粉色长发与蓝色瞳仁"), "{t}");
        // 别名都能触发提及（引用解析，不用作者想关键词）
        for alias in ["爱莉", "粉色妖精小姐", "真我"] {
            let out = codex.activate(&Cx::new(&format!("{alias}在看着你。")).ctx(), &big());
            assert_eq!(out.len(), 1, "别名 {alias} 应命中");
            assert_eq!(out[0].depth, Depth::Line);
        }
    }
}














