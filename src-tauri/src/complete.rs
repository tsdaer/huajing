//! 设定补全管线（M3.8 · 设计 §6.8）：模板驱动手动补全、一致性校验、即兴模式。
//!
//! 模块纪律与 scene.rs/director.rs 相同：纯数据与算法，不碰文件不碰网络（补全/即兴的
//! LLM 调用由宿主 commands 侧拿 prompt 去跑），单测直接钉住。
//!
//! 四机制（设计 §6.8）的落点：
//! - ① 模板驱动手动补全：类型模板知道每个实体「缺什么」（`missing_facets`），提示词
//!   内嵌写作论约束（anchors 宁少而精、口癖能落进示例对话、tells 覆盖高频情绪、
//!   by_affect 与气质一致）；
//! - ② 运行期捕获分级：瞬时状态 → 黑板、既有实体小事实 → 按配置自动、全新实体 → 人工
//!   （分级判据 `CaptureGrade`，宿主在 apply_summary_outcome 执行）；
//! - ③ 一致性校验：确定性先行（id 重复 / 字段冲突 / 悬空关系 / anchors 驳回），
//!   冲突带正史现值供收件箱双源呈现；
//! - ④ 即兴模式：薄实体现场补「设定·暂定」一条（`thinness` 判薄 + `build_improv_prompt`）。

use crate::codex::{Codex, CodexEntity};
use serde_json::Value;
use std::collections::BTreeMap;

// ---------- 类型模板（设计 §6.2 的模板 facet 清单）----------

/// 模板里的一个 facet 槽位：path 是 facts 里的点分路径（`one_liner` 特指实体顶层字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetSpec {
    pub path: &'static str,
    /// 人读名（UI 高亮与补全 diff 卡片用）
    pub label: &'static str,
    /// 生成提示（「补什么」的一句话说明，进提示词）
    pub hint: &'static str,
}

const CHAR_TEMPLATE: &[FacetSpec] = &[
    FacetSpec { path: "one_liner", label: "一句话", hint: "一句含最强辨识点的整体介绍（one_liner 内含辨识点）" },
    FacetSpec { path: "look.impression", label: "整体印象", hint: "传神而非全貌的外貌整体印象（识别度 > 细节量）" },
    FacetSpec { path: "look.anchors", label: "恒定辨识点", hint: "2–4 个恒定辨识点：求传神不求全貌，宁少而精（anchors 是最高保护级，之后禁改）" },
    FacetSpec { path: "speech.style", label: "说话方式", hint: "句式、语气、节奏的整体描述" },
    FacetSpec { path: "speech.tics", label: "口癖", hint: "2–4 个口癖/语气词，每条都必须能自然落进示例对话" },
    FacetSpec { path: "speech.by_affect", label: "情绪变体（语言）", hint: "情绪 → 说话方式变化（如 shy/upset/happy）；要与气质参数一致——胆汁质的「生气」不该是安静内敛" },
    FacetSpec { path: "mannerisms.habits", label: "标志小动作", hint: "1–3 个标志性的习惯动作" },
    FacetSpec { path: "mannerisms.by_affect", label: "状态动作", hint: "情绪 → 可见的小动作（与语言变体互补，不重复）" },
    FacetSpec { path: "tells", label: "心理外化词典", hint: "情绪 → 可见线索；至少覆盖高频情绪（喜/怒/哀/惧），供心理运行时外化（设计 §9）" },
    FacetSpec { path: "motivation", label: "动机", hint: "她为什么动——最深一层想要什么" },
    FacetSpec { path: "schedule", label: "作息", hint: "日常作息/出没规律（可省的场景类实体才空着）" },
];

const PLACE_TEMPLATE: &[FacetSpec] = &[
    FacetSpec { path: "one_liner", label: "一句话", hint: "一句含最强辨识点的整体介绍" },
    FacetSpec { path: "scene", label: "场景描写", hint: "可按时段写多套；先给一套当下时段的氛围描写" },
    FacetSpec { path: "rules", label: "场所规则", hint: "这里的规矩/惯例（谁能在什么时候做什么）" },
    FacetSpec { path: "exits", label: "出入口", hint: "能去哪、怎么走" },
];

const ITEM_TEMPLATE: &[FacetSpec] = &[
    FacetSpec { path: "one_liner", label: "一句话", hint: "一句含最强辨识点的整体介绍" },
    FacetSpec { path: "appearance", label: "外观", hint: "看起来是什么样（识别度优先）" },
    FacetSpec { path: "owner", label: "归属", hint: "属于谁/在哪，与谁有关" },
    FacetSpec { path: "role", label: "作用", hint: "它能干什么、对剧情意味着什么" },
];

const EVENT_TEMPLATE: &[FacetSpec] = &[
    FacetSpec { path: "one_liner", label: "一句话", hint: "一句说清这是一件什么事" },
    FacetSpec { path: "cause", label: "起因", hint: "事件因何而起" },
    FacetSpec { path: "development", label: "经过", hint: "正史中如何展开" },
    FacetSpec { path: "outcome", label: "结果", hint: "落到了什么结局、留下什么后果" },
];

const CONCEPT_TEMPLATE: &[FacetSpec] = &[
    FacetSpec { path: "one_liner", label: "一句话", hint: "一句说清这是什么概念" },
    FacetSpec { path: "definition", label: "界定", hint: "边界与要点：它不是什么也值得写" },
];

/// 类型 → 模板；未知类型回落通用概念模板（one_liner 起步，宁缺勿滥）。
pub fn template_of(ty: &str) -> &'static [FacetSpec] {
    match ty {
        "char" => CHAR_TEMPLATE,
        "place" => PLACE_TEMPLATE,
        "item" => ITEM_TEMPLATE,
        "event" => EVENT_TEMPLATE,
        _ => CONCEPT_TEMPLATE,
    }
}

/// 槽位是否空着：one_liner 看顶层字段，其余看 facts 点分路径。
fn slot_filled(entity: &CodexEntity, path: &str) -> bool {
    let v = if path == "one_liner" {
        return !entity.one_liner.trim().is_empty();
    } else {
        crate::codex::static_fact(entity, path)
    };
    match v {
        None => false,
        Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// 缺失的模板 facet（补全按钮与 UI 高亮的数据源；即兴判薄同源）。
pub fn missing_facets(entity: &CodexEntity) -> Vec<&'static FacetSpec> {
    template_of(&entity.ty)
        .iter()
        .filter(|spec| !slot_filled(entity, spec.path))
        .collect()
}

/// 缺失 facet 的路径清单（UI 直接可用）。
pub fn missing_paths(entity: &CodexEntity) -> Vec<String> {
    missing_facets(entity)
        .into_iter()
        .map(|s| s.path.to_string())
        .collect()
}

/// 实体薄度 = 缺失模板 facet 数（设计 §6.8-4「激活实体过薄」的判据）。
pub fn thinness(entity: &CodexEntity) -> usize {
    missing_facets(entity).len()
}

/// 即兴触发阈值：缺这么多模板槽位才算「过薄」——满配实体不值得为一轮即兴多花一次请求。
pub const THIN_THRESHOLD: usize = 3;

/// 实体清单里被文本提及且过薄的候选（按薄度降序、同分按 id；宿主取第一个去生成）。
/// 提及判定用 name/aliases 的朴素包含——即兴模式的候选入口不需要 trie 精度。
pub fn improv_candidates<'a>(cx: &'a Codex, text: &str) -> Vec<&'a CodexEntity> {
    let hay = text.to_lowercase();
    let mut out: Vec<&CodexEntity> = cx
        .entities()
        .iter()
        .filter(|e| e.status != "retired")
        .filter(|e| {
            // 加固 C7：反向 contains（实体名包含整段对话文本）是写反的一半——短文本
            //（「猫」）会误激活一片实体，空名实体的 hay.contains("") 恒真全入候选；
            // 提及判定只该是「文本里出现了实体名/别名」。
            let name = e.name.trim().to_lowercase();
            !name.is_empty() && hay.contains(&name)
                || e.aliases.iter().any(|a| {
                    let a = a.trim().to_lowercase();
                    !a.is_empty() && hay.contains(&a)
                })
        })
        .filter(|e| thinness(e) >= THIN_THRESHOLD)
        .collect();
    out.sort_by(|a, b| thinness(b).cmp(&thinness(a)).then_with(|| a.id.cmp(&b.id)));
    out
}

// ---------- 提示词（手动补全 ① / 即兴 ④）----------

/// 手动补全的输入上下文（宿主负责从 codex/会话里取好喂进来）。
pub struct CompletionContext<'a> {
    pub entity: &'a CodexEntity,
    /// 一跳关系邻体的「id 类型 名字——one_liner」行
    pub neighbors: &'a [String],
    /// 世界概览行（全部实体的「id 类型 名字」简表）
    pub world_lines: &'a [String],
    /// 会话种子/前提（风格基调）
    pub premise: Option<&'a str>,
    /// 时代基调（worldline 叶阶段 directive 摘要，M3.7；可空）
    pub era: Option<&'a str>,
}

/// 写作论约束（设计 §6.8-1）：生成规范内嵌进补全与即兴两类提示词。
const CRAFT_RULES: &str = "\
【写作论约束】\
1. 辨识点宁少而精：anchors 只给 2–4 个恒定辨识点，求传神不求全貌；\
2. 口癖必须能落进示例对话：每条 tics 都要是一句她真会说出口的话或动作；\
3. tells 覆盖高频情绪（喜/怒/哀/惧至少各一条），每条都是旁观者可见的线索，不写心理独白；\
4. by_affect 与气质一致：脾气急的人生气的样子不能是安静内敛；\
5. 与既有设定严格一致：不与世界概览、一跳关系、既有事实矛盾；拿不准就不写；\
6. 全部用中文；不写机制数值，不复述已有内容。";

/// 为缺失 facet 生成草稿的提示词（宿主拿去调 LLM；回复应是一个 JSON 对象）。
pub fn build_completion_prompt(ctx: &CompletionContext<'_>) -> String {
    let e = ctx.entity;
    let missing = missing_facets(e);
    let mut p = String::new();
    p.push_str("你在为互动小说的设定集补全一个角色/事物。只输出一个 JSON 对象，不要解释、不要代码围栏。\n\n");
    if let Some(premise) = ctx.premise {
        p.push_str(&format!("【故事前提】{premise}\n"));
    }
    if let Some(era) = ctx.era {
        p.push_str(&format!("【时代基调】{era}\n"));
    }
    p.push_str(&format!(
        "\n【目标实体】\n{}\n",
        serde_json::json!({
            "id": e.id, "type": e.ty, "name": e.name,
            "aliases": e.aliases, "one_liner": e.one_liner,
            "facts": e.facts,
        })
    ));
    if !ctx.neighbors.is_empty() {
        p.push_str("\n【一跳关系】\n");
        for line in ctx.neighbors {
            p.push_str(line);
            p.push('\n');
        }
    }
    if !ctx.world_lines.is_empty() {
        p.push_str("\n【世界概览】\n");
        for line in ctx.world_lines {
            p.push_str(line);
            p.push('\n');
        }
    }
    p.push_str("\n【缺失 facets】只补下面这些，不要动其他内容：\n");
    for spec in &missing {
        p.push_str(&format!("- {}（{}）：{}\n", spec.path, spec.label, spec.hint));
    }
    p.push('\n');
    p.push_str(CRAFT_RULES);
    p.push_str(
        "\n\n【输出格式】一个 JSON 对象：{\"facets\": {\"<路径>\": <值>…}, \"note\": \"一句话说明取舍\"}。\
键是缺失 facet 的路径，值类型与 facet 语义相符（anchors/tics/habits 是字符串数组，tells/by_affect 是对象，其余是字符串）。",
    );
    p
}

/// 即兴模式的一条暂定事实草稿。
#[derive(Debug, Clone, PartialEq)]
pub struct ImprovDraft {
    pub facet: String,
    pub value: Value,
    /// 可直接注入的一句话（「设定·暂定」前缀由宿主加）
    pub text: String,
}

/// 即兴补一条暂定事实的提示词（便宜档、单条、快）。
pub fn build_improv_prompt(
    entity: &CodexEntity,
    world_lines: &[String],
    seed_dialogue: &str,
) -> String {
    let mut p = String::new();
    p.push_str("你在为互动小说即兴补一条设定。只输出一个 JSON 对象，不要解释、不要代码围栏。\n");
    p.push_str(&format!(
        "\n【目标实体】{}（{}）——{}\n",
        entity.name, entity.id, entity.one_liner
    ));
    if !world_lines.is_empty() {
        p.push_str("\n【世界概览】\n");
        for line in world_lines.iter().take(20) {
            p.push_str(line);
            p.push('\n');
        }
    }
    p.push_str("\n【最近对话（会话种子）】\n");
    p.push_str(seed_dialogue);
    p.push_str("\n\n补一条：从这段对话的自然延伸里，为该实体补一个此刻成立、以后也可能被提到的细节。");
    p.push_str("不要写转瞬即逝的状态（天气/此刻在做什么），不要与既有设定矛盾，不许碰 anchors（恒定辨识点）。");
    p.push_str(CRAFT_RULES);
    p.push_str(
        "\n\n【输出格式】{\"facet\": \"<facts 路径>\", \"value\": <值>, \"text\": \"<一句可注入的中文陈述，主语用实体名>\"}。",
    );
    p
}

/// 取回复里第一个 `{` 到最后一个 `}` 的片段（剥围栏与前后杂文；与 summarize 同纪律）。
/// pub(crate)：素材管线（M3.9）的各阶段解析共用同一宽容口径。
pub(crate) fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

/// 解析补全回复：`{"facets": {...}, "note": "…"}`（宽容：facets 缺失或不是对象 → 报错）。
pub fn parse_completion(raw: &str) -> Result<(BTreeMap<String, Value>, String), String> {
    let frag = extract_json_object(raw).ok_or_else(|| "回复里没有 JSON 对象".to_string())?;
    let v: Value =
        serde_json::from_str(frag).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let obj = v.as_object().ok_or_else(|| "回复不是 JSON 对象".to_string())?;
    let facets = obj
        .get("facets")
        .and_then(Value::as_object)
        .ok_or_else(|| "回复缺少 facets 对象".to_string())?;
    let mut out = BTreeMap::new();
    for (k, v) in facets {
        let key = k.trim();
        if key.is_empty() || v.is_null() {
            continue;
        }
        // 空对象/空数组与缺失等价（补一个空没有意义，还会挡住「缺失」判定）
        match v {
            Value::Object(o) if o.is_empty() => continue,
            Value::Array(a) if a.is_empty() => continue,
            Value::String(s) if s.trim().is_empty() => continue,
            _ => {}
        }
        out.insert(key.to_string(), v.clone());
    }
    if out.is_empty() {
        return Err("facets 里没有有效条目".to_string());
    }
    let note = obj
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((out, note))
}

/// 解析即兴回复：无有效正文给 None（即兴是锦上添花，失败静默跳过）。
pub fn parse_improv(raw: &str) -> Result<Option<ImprovDraft>, String> {
    let frag = extract_json_object(raw).ok_or_else(|| "回复里没有 JSON 对象".to_string())?;
    let v: Value =
        serde_json::from_str(frag).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let obj = v.as_object().ok_or_else(|| "回复不是 JSON 对象".to_string())?;
    let facet = obj
        .get("facet")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = obj
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let value = obj.get("value").cloned().unwrap_or(Value::Null);
    if facet.is_empty() || text.is_empty() || value.is_null() {
        return Ok(None);
    }
    Ok(Some(ImprovDraft { facet, value, text }))
}

// ---------- 一致性校验（③：确定性先行，冲突带双源）----------

/// 语义矛盾检测的提示词（§6.8-3「可选 LLM 语义矛盾检测」）：确定性校验拦不住的
/// 「前文黑发后文棕发」类矛盾交给便宜模型对照判断；结论只做双源呈现，不自动裁决。
/// `fact_brief` = 目标实体的现状 JSON；`proposal_brief` = 提案内容的 JSON。
pub fn build_semantic_check_prompt(fact_brief: &str, proposal_brief: &str) -> String {
    let mut p = String::new();
    p.push_str("你在审查互动小说设定集的一条写入提案。只判断**语义矛盾**，不评价文笔、不补充建议。\n");
    p.push_str("只输出一个 JSON 对象，不要解释、不要代码围栏。\n\n");
    p.push_str("【实体现状】\n");
    p.push_str(fact_brief);
    p.push_str("\n\n【提案内容】\n");
    p.push_str(proposal_brief);
    p.push_str("\n\n判断标准：提案与现状能否同时为真？时间演进（剪发/搬家/关系变化）不是矛盾；\
恒定辨识点（anchors）被替换、同一事实两种说法、互斥的特征才是矛盾。拿不准按无矛盾处理。");
    p.push_str("\n\n【输出格式】{\"contradictions\": [\"<每条矛盾一句话，引用双方原文>\"]}；无矛盾给 {\"contradictions\": []}。");
    p
}

/// 解析语义矛盾检测的回复：矛盾条目列表（宽容解析，异常按「检测失败」空表处理）
pub fn parse_semantic_check(raw: &str) -> Vec<String> {
    let Some(frag) = extract_json_object(raw) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return Vec::new();
    };
    v.get("contradictions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| c.as_str().map(str::trim))
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 校验结论级别：Reject = 自动驳回（anchors / id 重复）；Warn = 冲突双源呈现，人工裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueLevel {
    Reject,
    Warn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub level: IssueLevel,
    pub detail: String,
    /// Warn 时的正史现值（收件箱双源呈现的「现状」一侧）
    pub current: Option<Value>,
}

impl Issue {
    fn reject(detail: impl Into<String>) -> Issue {
        Issue { level: IssueLevel::Reject, detail: detail.into(), current: None }
    }
    fn warn(detail: impl Into<String>, current: Option<Value>) -> Issue {
        Issue { level: IssueLevel::Warn, detail: detail.into(), current }
    }
}

/// 提案 payload 里「目标 facet」的取值形态宽容解析：
/// `{"facet": 路径, "value": 内容}` 优先，其次顶层是单键对象（键即路径），标量给 None。
fn proposed_facet(payload: &Value) -> Option<(String, &Value)> {
    let obj = payload.as_object()?;
    if let Some(f) = obj.get("facet").or_else(|| obj.get("path")).and_then(Value::as_str) {
        let v = obj.get("value")?;
        return Some((f.trim().to_string(), v));
    }
    // value 字段里也可能包一层 {facet, value}
    if let Some(inner) = obj.get("value").and_then(Value::as_object) {
        if let Some(f) = inner.get("facet").and_then(Value::as_str) {
            return Some((f.trim().to_string(), inner.get("value")?));
        }
    }
    None
}

/// 确定性校验（设计 §6.8-3）：任何写入先过这里；anchors 最高保护级。
///
/// - `new_entity`：id 与既有实体重复 → 驳回（重复 id 会让引用解析歧义）；
/// - `relation`：to 指向不存在的实体 → 悬空关系 Warn（可能对方还没入集，人工裁决）；
/// - `new_fact`/`fact_change`：与正史现值不同 → 字段冲突 Warn，带现值供双源呈现；
/// - 一律：anchors 冲突 → 驳回（`codex::anchors_conflict`）。
pub fn validate_proposal(cx: &Codex, target: &str, kind: &str, payload: &Value) -> Vec<Issue> {
    let mut out = Vec::new();
    let existing = cx.get(target);
    match kind {
        "new_entity" => {
            if existing.is_some() {
                out.push(Issue::reject(format!(
                    "实体 {target} 已存在——新实体提案的 id 与既有实体重复"
                )));
            }
        }
        "relation" => {
            if let Some(to) = payload.get("to").or_else(|| {
                payload.get("value").and_then(|v| v.get("to"))
            }) {
                let to = to.as_str().unwrap_or_default().trim();
                if !to.is_empty() && cx.get(to).is_none() {
                    out.push(Issue::warn(
                        format!("悬空关系：to 指向的实体 {to} 不在设定集里"),
                        None,
                    ));
                }
            }
        }
        "new_fact" | "fact_change" => {
            if let Some(entity) = existing {
                if let Some((facet, proposed)) = proposed_facet(payload) {
                    if facet != "look.anchors" {
                        if let Some(current) = crate::codex::static_fact(entity, &facet) {
                            if current != proposed {
                                out.push(Issue::warn(
                                    format!(
                                        "字段冲突：{facet} 已有正史值，提案另写一词（双源见下）"
                                    ),
                                    Some(current.clone()),
                                ));
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    if let Some(entity) = existing {
        if let Some(reason) = crate::codex::anchors_conflict(entity, payload) {
            out.push(Issue::reject(reason));
        }
    }
    out
}

// ---------- 运行期捕获分级（②）----------

/// 一条设定提案的捕获分级（设计 §6.8-2「按提案类型分级自动接受」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureGrade {
    /// 瞬时状态（「现在下雨了」）：直接写黑板，不入收件箱
    Transient,
    /// 既有实体的小事实：按用户配置自动接受或询问
    MinorFact,
    /// 全新实体：必须人工确认
    NewEntity,
    /// 其他（关系/改写）：照旧人工
    Manual,
}

pub fn capture_grade(kind: &str, target: &str, cx: &Codex) -> CaptureGrade {
    match kind {
        "transient" => CaptureGrade::Transient,
        "new_entity" => CaptureGrade::NewEntity,
        "new_fact" => {
            if target.trim().is_empty() || cx.get(target).is_none() {
                // 目标实体不存在的事实没有落点，仍走人工（收件箱里看得见原因）
                CaptureGrade::Manual
            } else {
                CaptureGrade::MinorFact
            }
        }
        _ => CaptureGrade::Manual,
    }
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entity(ty: &str, facts: Value) -> CodexEntity {
        let mut v = json!({
            "id": format!("{ty}.测试"),
            "type": ty,
            "name": "测试",
            "one_liner": "",
        });
        if let Some(f) = facts.as_object() {
            v["facts"] = Value::Object(f.clone());
        }
        CodexEntity::from_value(&v).unwrap()
    }

    #[test]
    fn char_template_reports_missing_facets_and_highlights() {
        let mut e = entity("char", json!({
            "look": { "impression": "旧毛衣", "anchors": ["泪痣"] },
            "speech": { "style": "短句", "tics": ["……嗯。"] },
        }));
        e.one_liner = "图书馆夜班管理员。".into();
        let missing = missing_paths(&e);
        // 已填 one_liner/look.impression/look.anchors/speech.style/speech.tics
        assert!(!missing.contains(&"one_liner".to_string()));
        assert!(!missing.contains(&"look.anchors".to_string()));
        assert!(missing.contains(&"speech.by_affect".to_string()), "{missing:?}");
        assert!(missing.contains(&"tells".to_string()), "{missing:?}");
        assert!(missing.contains(&"motivation".to_string()), "{missing:?}");
        assert_eq!(thinness(&e), missing.len());
        // 空字符串与空数组都算缺失
        let bare = entity("char", json!({ "tells": {} }));
        assert!(missing_paths(&bare).contains(&"tells".to_string()));
    }

    #[test]
    fn improv_mention_requires_name_to_appear_in_text() {
        // C7：反向 contains 曾让短文本（「猫」）激活一切名字含猫的实体；
        // 修正后只有「文本里出现实体名/别名」才入候选。
        let mut cat = entity("char", json!({})); // 空模板 = 过薄（thinness >= 3）
        cat.name = "猫娘".into();
        cat.id = "char.猫娘".into();
        let mut dog = entity("char", json!({}));
        dog.name = "犬耳娘".into();
        dog.id = "char.犬耳娘".into();
        dog.aliases = vec!["猫见愁".into()];
        let cx = Codex::build(vec![cat, dog]);

        assert!(
            improv_candidates(&cx, "猫").is_empty(),
            "短文本不得反向激活名字含猫的实体"
        );
        let hits = improv_candidates(&cx, "想撸猫娘");
        assert_eq!(hits.len(), 1, "真提及才入候选：{:?}", hits.iter().map(|e| &e.id).collect::<Vec<_>>());
        assert_eq!(hits[0].id, "char.猫娘");
        // 别名路径：文本提到别名
        let hits = improv_candidates(&cx, "猫见愁来了");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "char.犬耳娘");
        assert!(improv_candidates(&cx, "谁都没提").is_empty());
    }

    #[test]
    fn unknown_type_falls_back_to_concept_template() {
        let e = entity("macguffin", json!({}));
        let missing = missing_paths(&e);
        assert_eq!(missing, vec!["one_liner".to_string(), "definition".to_string()]);
    }

    #[test]
    fn completion_prompt_carries_constraints_and_missing_list() {
        let e = entity("char", json!({
            "look": { "anchors": ["泪痣"] },
        }));
        let ctx = CompletionContext {
            entity: &e,
            neighbors: &["place.图书馆 place 图书馆——夜色里的书架之间".to_string()],
            world_lines: &["char.测试 char 测试".to_string()],
            premise: Some("雨夜图书馆"),
            era: Some("公告期——公告已贴出"),
        };
        let p = build_completion_prompt(&ctx);
        assert!(p.contains("雨夜图书馆"));
        assert!(p.contains("公告期"));
        assert!(p.contains("place.图书馆"));
        assert!(p.contains("2–4 个恒定辨识点"), "anchors 约束要在提示词里");
        assert!(p.contains("落进示例对话"), "口癖约束要在提示词里");
        assert!(p.contains("高频情绪"), "tells 约束要在提示词里");
        assert!(p.contains("气质一致"), "by_affect 约束要在提示词里");
        assert!(p.contains("- look.impression（整体印象）"), "缺失清单带说明：{p}");
        assert!(!p.contains("- look.anchors"), "只列缺失的：{p}");
    }

    #[test]
    fn completion_parse_tolerates_fences_and_drops_empty() {
        let raw = "好的，这是结果：\n```json\n{\"facets\": {\"motivation\": \"守着夜班\", \"tells\": {}, \"look.anchors\": null}, \"note\": \"宁少而精\"}\n```";
        let (facets, note) = parse_completion(raw).unwrap();
        assert_eq!(facets.get("motivation").unwrap(), "守着夜班");
        assert!(!facets.contains_key("tells"), "空对象条目应丢弃");
        assert!(!facets.contains_key("look.anchors"), "null 条目应丢弃");
        assert_eq!(note, "宁少而精");
        assert!(parse_completion("没有对象").is_err());
        assert!(parse_completion("{\"note\":\"只有说明\"}").is_err());
    }

    #[test]
    fn improv_parse_and_candidates() {
        let draft = parse_improv(
            "{\"facet\":\"facts.日常\",\"value\":\"养了一只叫墨墨的猫\",\"text\":\"她养了一只叫墨墨的猫。\"}",
        )
        .unwrap()
        .unwrap();
        assert_eq!(draft.facet, "facts.日常");
        assert!(draft.text.contains("墨墨"));
        assert!(parse_improv("{\"facet\":\"\",\"value\":1,\"text\":\"x\"}")
            .unwrap()
            .is_none());

        let mut cx_e = entity("char", json!({}));
        cx_e.id = "char.小雨".into();
        cx_e.name = "小雨".into();
        let full = entity("char", json!({
            "look": { "impression": "旧毛衣", "anchors": ["泪痣"] },
            "speech": { "style": "短句", "tics": ["……嗯。"], "by_affect": { "shy": "声音变小" } },
            "mannerisms": { "habits": ["绕头发"], "by_affect": { "nervous": "擦胸牌" } },
            "tells": { "忐忑": "敲桌面" },
            "motivation": "替母亲读完书",
            "schedule": "夜班",
        }));
        let mut full = full;
        full.id = "char.阿澈".into();
        full.name = "阿澈".into();
        let cx = Codex::build(vec![cx_e.clone(), full]);
        let hits = improv_candidates(&cx, "小雨把借书卡忘在家里了");
        assert_eq!(hits.len(), 1, "满配的阿澈不该入选：{:?}", hits.iter().map(|e| &e.id).collect::<Vec<_>>());
        assert_eq!(hits[0].id, "char.小雨");
        assert!(improv_candidates(&cx, "阿澈在看书").is_empty(), "满配实体不触发即兴");
        assert!(improv_candidates(&cx, "毫无关系的文本").is_empty());
    }

    #[test]
    fn validate_rejects_duplicate_ids_and_anchor_conflicts() {
        let e = entity("char", json!({
            "look": { "anchors": ["左眼角一颗泪痣"] },
        }));
        let cx = Codex::build(vec![e]);
        // id 重复
        let issues = validate_proposal(&cx, "char.测试", "new_entity", &json!({}));
        assert!(issues.iter().any(|i| i.level == IssueLevel::Reject && i.detail.contains("已存在")));
        // anchors 冲突
        let issues = validate_proposal(
            &cx,
            "char.测试",
            "new_fact",
            &json!({ "facet": "look.anchors", "value": ["黑色短发"] }),
        );
        assert!(issues.iter().any(|i| i.level == IssueLevel::Reject), "{issues:?}");
        // 全新 id 的 new_entity 不报重复
        assert!(
            !validate_proposal(&cx, "char.新人", "new_entity", &json!({}))
                .iter()
                .any(|i| i.level == IssueLevel::Reject)
        );
    }

    #[test]
    fn validate_flags_dangling_relations_and_field_conflicts_with_current() {
        let e = entity("char", json!({
            "schedule": "18:00–24:00 值班",
        }));
        let mut e = e;
        e.id = "char.小雨".into();
        let place = entity("place", json!({}));
        let cx = Codex::build(vec![e, place]);
        // 悬空关系
        let issues = validate_proposal(
            &cx,
            "char.小雨",
            "relation",
            &json!({ "to": "char.不存在", "kind": "宠物" }),
        );
        assert!(issues.iter().any(|i| i.level == IssueLevel::Warn && i.detail.contains("悬空")));
        // 字段冲突带正史现值（双源呈现的「现状」一侧）
        let issues = validate_proposal(
            &cx,
            "char.小雨",
            "fact_change",
            &json!({ "facet": "schedule", "value": "全天值班" }),
        );
        let conflict = issues
            .iter()
            .find(|i| i.detail.contains("字段冲突"))
            .expect("应报字段冲突");
        assert_eq!(conflict.level, IssueLevel::Warn);
        assert_eq!(conflict.current.as_ref(), Some(&json!("18:00–24:00 值班")));
        // 同值不报
        assert!(validate_proposal(
            &cx,
            "char.小雨",
            "fact_change",
            &json!({ "facet": "schedule", "value": "18:00–24:00 值班" })
        )
        .is_empty());
    }

    #[test]
    fn semantic_check_parse_tolerates_noise_and_keeps_items() {
        let p = build_semantic_check_prompt("{\"one_liner\":\"黑发\"}", "{\"facet\":\"look.impression\",\"value\":\"棕发少女\"}");
        assert!(p.contains("黑发"));
        assert!(p.contains("棕发少女"));
        assert!(p.contains("拿不准按无矛盾处理"), "保守倾向要写进提示词");
        let out = parse_semantic_check(
            "结论：```json\n{\"contradictions\": [\"现状写黑发，提案写棕发\"]}\n``` 以上。",
        );
        assert_eq!(out, vec!["现状写黑发，提案写棕发"]);
        assert!(parse_semantic_check("{\"contradictions\": []}").is_empty());
        assert!(parse_semantic_check("模型抽风了").is_empty(), "解析失败 = 无结论，不误报");
    }

    #[test]
    fn capture_grade_splits_transient_minor_fact_and_new_entity() {
        let e = entity("char", json!({}));
        let mut e = e;
        e.id = "char.小雨".into();
        let cx = Codex::build(vec![e]);
        assert_eq!(capture_grade("transient", "", &cx), CaptureGrade::Transient);
        assert_eq!(
            capture_grade("new_fact", "char.小雨", &cx),
            CaptureGrade::MinorFact
        );
        assert_eq!(
            capture_grade("new_fact", "char.陌生人", &cx),
            CaptureGrade::Manual,
            "目标不存在的事实没有落点，走人工"
        );
        assert_eq!(capture_grade("new_entity", "char.墨墨", &cx), CaptureGrade::NewEntity);
        assert_eq!(capture_grade("relation", "char.小雨", &cx), CaptureGrade::Manual);
    }
}
