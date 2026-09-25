//! 主演模型的工具调用快通道（增强计划 · 包 A · 设计决断 1–7）。
//!
//! **定位**：工具是模型对「我刚演出的内容」的结构化旁注，不是新发的特权——
//! 直写档全过 hook 增量同款校验（黑板白名单、字符串/深度上限、槽位封顶），
//! 提案档复用收件箱链路（`capture_grade` 分级 + `anchors_conflict` 驳回）。
//! 工具与剧情文本同报文（决断 2），效果是消息的衍生事件（决断 3）：
//! 本模块只做「tool_calls + 当前投影 → 确定性事件」这一步，落盘由调用方执行，
//! 编辑/删除/重roll 后重放重跑同一段代码即自动正确。
//!
//! **确定性**：除 `ts` 与 diag 外，同样的输入（tool_calls、state、设定集、
//! 已有提案 id 集）产出同样的事件序列——重放路径（rebuild_from）与生成路径共用。

use serde::Serialize;
use serde_json::json;

use crate::card::{BLACKBOARD_KEYS, KvSet, ToolPolicy};
use crate::codex;
use crate::event::{self, LogBody};
use crate::llm::{ToolCall, UiEmit};
use crate::psyche::Psyche;
use crate::threads;

// ---------- 工具 schema 常量表（A1：≤10 个、紧凑中文描述、总 token ≤1200）----------

/// 一条工具的声明（名字 + 一句话语义 + 最小化 JSON Schema）
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// 直写档六件 + 提案档四件（A3/A4）。语义与 Lua 沙箱 API 一份心智模型（决断 4）；
/// 模型侧不区分档位——名字即语义，直写/提案由宿主确定性执行方式决定。
pub static TOOL_DEFS: std::sync::LazyLock<Vec<ToolDef>> = std::sync::LazyLock::new(|| {
    vec![
    ToolDef {
        name: "psyche_feel",
        description: "自报情绪事件（本场演出中产生的情绪）。intensity 取 0–1。",
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "情绪名，如 后怕"},
                "intensity": {"type": "number", "description": "强度 0–1"},
                "source": {"type": "string", "description": "来源，一句话"}
            },
            "required": ["name", "intensity"]
        }),
    },
    ToolDef {
        name: "psyche_intent",
        description: "自报意图的新建或增减（刚形成的念头）。delta 取 -1–1，正增负减。",
        parameters: json!({
            "type": "object",
            "properties": {
                "goal": {"type": "string", "description": "意图名，一句话"},
                "delta": {"type": "number", "description": "强度增减 -1–1；新建给正值"}
            },
            "required": ["goal", "delta"]
        }),
    },
    ToolDef {
        name: "blackboard_set",
        description: "更新场上的公开事实：时间/地点/在场者。只用于剧情确实发生的变化。",
        parameters: json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "enum": ["day", "clock", "place", "actors"]},
                "value": {}
            },
            "required": ["key", "value"]
        }),
    },
    ToolDef {
        name: "memory_set",
        description: "记一条亲身经历的键值事实（如 周三休息）。只写剧情里坐实的事。",
        parameters: json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "事实键，如 还书日期"},
                "value": {}
            },
            "required": ["key", "value"]
        }),
    },
    ToolDef {
        name: "schedule_say",
        description: "把一句没说出口的心里话记下来，下轮时机合适时主动开口。",
        parameters: json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }),
    },
    ToolDef {
        name: "ui_emit",
        description: "推送界面事件（表情等）。kind 如 emotion；value 用一个词。",
        parameters: json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string"},
                "value": {"type": "string"}
            },
            "required": ["kind", "value"]
        }),
    },
    ToolDef {
        name: "propose_fact",
        description: "给既有设定实体提案一条事实（进收件箱，人工确认后生效）。",
        parameters: json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "description": "实体 id，如 char.小雨"},
                "facet": {"type": "string", "description": "facts 路径，如 schedule"},
                "value": {"type": "string"},
                "reason": {"type": "string", "description": "剧情依据，一句话"}
            },
            "required": ["target", "facet", "value", "reason"]
        }),
    },
    ToolDef {
        name: "propose_entity",
        description: "提案一个剧情里新出现的设定实体（进收件箱）。",
        parameters: json!({
            "type": "object",
            "properties": {
                "type": {"type": "string", "enum": ["char", "place", "item", "event", "org", "rule", "concept", "note"]},
                "name": {"type": "string"},
                "skeleton": {"type": "object", "description": "可选实体骨架：facts/one_liner 等"},
                "reason": {"type": "string"}
            },
            "required": ["type", "name", "reason"]
        }),
    },
    ToolDef {
        name: "propose_relation",
        description: "提案两个既有实体间的新关系（进收件箱）。",
        parameters: json!({
            "type": "object",
            "properties": {
                "from": {"type": "string", "description": "主体实体 id"},
                "to": {"type": "string", "description": "客体实体 id"},
                "kind": {"type": "string", "description": "关系名，如 师徒"},
                "note": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["from", "to", "kind", "reason"]
        }),
    },
    ToolDef {
        name: "propose_thread",
        description: "提案开一条剧情线（进收件箱）。只开剧情真正立起来的线。",
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "线标题，短语"},
                "cause": {"type": "string", "description": "起因，一句话"},
                "actors": {"type": "array", "items": {"type": "string"}},
                "importance": {"type": "number", "description": "0–1"},
                "resurface": {"type": "object", "description": "可选，提起时机窗口对象"},
                "framing": {"type": "string", "description": "为什么值得挂心，一句话"}
            },
            "required": ["title", "cause", "actors"]
        }),
    },
    ]
});

/// 单条工具的请求体（调用方按卡策略过滤后拼请求）
pub fn schema_of(d: &ToolDef) -> serde_json::Value {
    json!({
        "type": "function",
        "function": {
            "name": d.name,
            "description": d.description,
            "parameters": d.parameters,
        },
    })
}

/// 全量工具请求体（无卡策略过滤的形态；生产路径用 [`schema_of`] 按策略拼装，
/// 本函数由测试与调试消费）
#[allow(dead_code)]
pub fn tool_schemas() -> Vec<serde_json::Value> {
    TOOL_DEFS.iter().map(schema_of).collect()
}

/// A1 契约区尾部的注入段（预算层 T，≤3%）：纪律句（决断 1/2）+ 工具清单。
/// 工具 schema 同时随请求体 `tools` 参数下发，这里是人读的行为契约。
pub fn contract_text() -> String {
    let mut s = String::from(
        "【工具旁注】\n\
         - 工具是旁路的结构化旁注，不是叙事的一部分：正文里不得描述工具使用；\
         只为写进剧情的持久变化调用工具，瞬时状态留在正文里。\n\
         - 每轮至多 6 次；没有值得持久化的变化就一次都不调。可用工具：",
    );
    for (i, d) in TOOL_DEFS.iter().enumerate() {
        if i > 0 {
            s.push('；');
        }
        s.push_str(&format!("{}（{}）", d.name, short_purpose(d.description)));
    }
    s.push('。');
    s
}

/// 描述的短用途（括注用）：取第一分句、去括注
fn short_purpose(desc: &str) -> &str {
    desc.split(['（', '。']).next().unwrap_or(desc)
}

/// 每轮工具调用上限的缺省值（A5；settings.tool_calls_per_turn 可配）
pub const DEFAULT_PER_TURN: usize = 6;

// ---------- 校验边界（A5）----------

/// 单个字符串参数值的字节上限（2KB）
const MAX_STRING_BYTES: usize = 2048;
/// 参数值的嵌套深度上限（照搬 hook 加固口径取工具档更严值）
const MAX_VALUE_DEPTH: usize = 8;

// ---------- 应用器 ----------

/// 一次应用的输入现场。`state` = 该角色当前 state（调用方从投影取，含 default_state 降级）。
pub struct ToolCtx<'a> {
    /// 角色目录名（作用域归属：psyche_* / schedule_say / memory 只落自己）
    pub character: &'a str,
    /// 展示名（群聊的 actors 自查、verdict 文案用）
    pub display_name: &'a str,
    pub turn: u64,
    /// 本轮所在场景（黑板写入按它路由到场景分区）
    pub scene: Option<&'a str>,
    pub state: serde_json::Value,
    /// 群聊/隔离模式（A5 作用域裁剪：propose_thread 的 actors 必须含自己）
    pub multi_cast: bool,
    pub policy: &'a ToolPolicy,
    pub limit: usize,
    pub cx: &'a codex::Codex,
    /// 设置：运行期自动接受既有实体的小事实（与总结管线同一开关）
    pub auto_minor: bool,
    /// 已在流里的提案 id（重放幂等：撞上即跳过，保留物化正史——决断 3）
    pub existing_proposals: &'a std::collections::BTreeSet<String>,
    pub ts: u64,
}

/// 一次调用的裁决（检查器/报告/诊断三处共用）
#[derive(Debug, Clone, Serialize)]
pub struct ToolVerdict {
    /// 工具名
    pub name: String,
    /// 参数摘要（一行，检查器显示）
    pub summary: String,
    /// applied | rejected | proposed | kept | dropped
    pub verdict: String,
    /// 结果/驳回原因（人读）
    pub reason: String,
}

/// 一次应用的全部产出：事件（调用方 commit）、界面事件（调用方转推）、
/// 待物化的自动接受提案（仅生成路径执行 grown.json 落盘）。
pub struct ToolApplication {
    pub bodies: Vec<LogBody>,
    pub verdicts: Vec<ToolVerdict>,
    pub state_after: serde_json::Value,
    pub ui_events: Vec<UiEmit>,
    /// (kind, payload) —— auto_minor 的小事实，调用方在**生成路径**物化
    pub auto_accepts: Vec<(String, serde_json::Value)>,
    /// 因超出每轮上限被弃的调用数
    pub overflow: usize,
}

/// 待落事件的一条提案（先校验收集、后统一分级落事件：anchors 驳回要在
/// propose 之前发生，事件顺序才与总结管线同形）
struct PendingProposal {
    tool: String,
    summary: String,
    id: String,
    kind: String,
    payload: serde_json::Value,
    note: Option<String>,
}

/// 工具应用主入口（决断 3：tool_calls + 确定性校验器 → 事件序列）。
///
/// 事件顺序：直写档聚合为**一条** EffectEvent（trigger="tool"）→
/// 提案档按调用序各一条 ProposalEvent（origin="model"）。
pub fn apply_tool_calls(calls: &[ToolCall], ctx: &ToolCtx<'_>) -> ToolApplication {
    let mut out = ToolApplication {
        bodies: Vec::new(),
        verdicts: Vec::new(),
        state_after: ctx.state.clone(),
        ui_events: Vec::new(),
        auto_accepts: Vec::new(),
        overflow: 0,
    };
    if calls.is_empty() {
        return out;
    }

    let mut blackboard: Vec<KvSet> = Vec::new();
    let mut memory: Vec<KvSet> = Vec::new();
    let mut psyche = Psyche::from_state(&ctx.state);
    let mut psyche_touched = false;
    let mut proposals: Vec<PendingProposal> = Vec::new();

    for (i, call) in calls.iter().enumerate() {
        let summary = args_summary(&call.arguments);
        // A5：每轮上限——超出的全部弃置，函数尾记一条汇总 diag
        if i >= ctx.limit {
            out.overflow += 1;
            out.verdicts.push(ToolVerdict {
                name: call.name.clone(),
                summary,
                verdict: "dropped".into(),
                reason: format!("超出每轮工具上限（{}）", ctx.limit),
            });
            continue;
        }
        // A7：卡策略（作者只做减法；平台审计工具面）
        if !ctx.policy.permits(&call.name) {
            out.verdicts.push(ToolVerdict {
                name: call.name.clone(),
                summary,
                verdict: "rejected".into(),
                reason: "卡策略禁用了这个工具".into(),
            });
            continue;
        }
        match call.name.as_str() {
            // ---------- 直写档（A3）：全过 hook 同款校验 ----------
            "psyche_feel" => {
                let Some(name) = str_arg(&call.arguments, "name") else {
                    reject(&mut out.verdicts, call, &summary, "缺 name 参数");
                    continue;
                };
                let Some(intensity) = num_arg(&call.arguments, "intensity") else {
                    reject(&mut out.verdicts, call, &summary, "缺 intensity 参数");
                    continue;
                };
                if name.is_empty() {
                    reject(&mut out.verdicts, call, &summary, "情绪名为空");
                    continue;
                }
                let source = str_arg(&call.arguments, "source").unwrap_or_default();
                let outcome = psyche.feel(&name, intensity as f32, &source, ctx.turn);
                psyche_touched = true;
                out.verdicts.push(ToolVerdict {
                    name: call.name.clone(),
                    summary,
                    verdict: verdict_str(outcome.accepted).into(),
                    reason: outcome.reason,
                });
            }
            "psyche_intent" => {
                let Some(goal) = str_arg(&call.arguments, "goal") else {
                    reject(&mut out.verdicts, call, &summary, "缺 goal 参数");
                    continue;
                };
                let Some(delta) = num_arg(&call.arguments, "delta") else {
                    reject(&mut out.verdicts, call, &summary, "缺 delta 参数");
                    continue;
                };
                if goal.is_empty() {
                    reject(&mut out.verdicts, call, &summary, "意图名为空");
                    continue;
                }
                match psyche.intend(&goal, delta as f32, ctx.turn) {
                    Some(it) => {
                        psyche_touched = true;
                        out.verdicts.push(ToolVerdict {
                            name: call.name.clone(),
                            summary,
                            verdict: "applied".into(),
                            reason: format!("意图「{}」强度 {:.2}", it.name, it.strength),
                        });
                    }
                    None => {
                        // 负增量削不存在的意图、或削到归零：无变化可落
                        out.verdicts.push(ToolVerdict {
                            name: call.name.clone(),
                            summary,
                            verdict: "rejected".into(),
                            reason: "意图不存在或已归零，无变化".into(),
                        });
                    }
                }
            }
            "blackboard_set" => {
                let Some(key) = str_arg(&call.arguments, "key") else {
                    reject(&mut out.verdicts, call, &summary, "缺 key 参数");
                    continue;
                };
                if !BLACKBOARD_KEYS.contains(&key.as_str()) {
                    reject(
                        &mut out.verdicts,
                        call,
                        &summary,
                        format!("黑板只收公开字段（{}）", BLACKBOARD_KEYS.join("/")),
                    );
                    continue;
                }
                let Some(value) = call.arguments.get("value") else {
                    reject(&mut out.verdicts, call, &summary, "缺 value 参数");
                    continue;
                };
                if let Some(bad) = value_violation(value) {
                    reject(&mut out.verdicts, call, &summary, bad);
                    continue;
                }
                blackboard.retain(|kv| kv.key != key);
                blackboard.push(KvSet {
                    key,
                    value: value.clone(),
                });
                out.verdicts.push(ToolVerdict {
                    name: call.name.clone(),
                    summary,
                    verdict: "applied".into(),
                    reason: "黑板公开字段已更新".into(),
                });
            }
            "memory_set" => {
                let Some(key) = str_arg(&call.arguments, "key").filter(|k| !k.is_empty()) else {
                    reject(&mut out.verdicts, call, &summary, "缺非空 key 参数");
                    continue;
                };
                let Some(value) = call.arguments.get("value") else {
                    reject(&mut out.verdicts, call, &summary, "缺 value 参数");
                    continue;
                };
                if let Some(bad) = value_violation(value) {
                    reject(&mut out.verdicts, call, &summary, bad);
                    continue;
                }
                memory.retain(|kv| kv.key != key);
                memory.push(KvSet {
                    key,
                    value: value.clone(),
                });
                out.verdicts.push(ToolVerdict {
                    name: call.name.clone(),
                    summary,
                    verdict: "applied".into(),
                    reason: "事实已记入记忆".into(),
                });
            }
            "schedule_say" => {
                let Some(text) = str_arg(&call.arguments, "text") else {
                    reject(&mut out.verdicts, call, &summary, "缺 text 参数");
                    continue;
                };
                if psyche.schedule(&text, ctx.turn) {
                    psyche_touched = true;
                    out.verdicts.push(ToolVerdict {
                        name: call.name.clone(),
                        summary,
                        verdict: "applied".into(),
                        reason: "已记下，下轮时机合适时主动开口".into(),
                    });
                } else {
                    out.verdicts.push(ToolVerdict {
                        name: call.name.clone(),
                        summary,
                        verdict: "rejected".into(),
                        reason: "空话、重复或队列已满，未收下".into(),
                    });
                }
            }
            "ui_emit" => {
                // hook 的 ui.emit 无 kind 白名单（界面按 kind 自行解释），工具同口径
                // （决断 4：与沙箱 API 一份心智模型）——只挡空值
                let Some(kind) = str_arg(&call.arguments, "kind") else {
                    reject(&mut out.verdicts, call, &summary, "缺 kind 参数");
                    continue;
                };
                let Some(value) = str_arg(&call.arguments, "value") else {
                    reject(&mut out.verdicts, call, &summary, "缺 value 参数");
                    continue;
                };
                if kind.is_empty() || value.is_empty() {
                    reject(&mut out.verdicts, call, &summary, "kind/value 不能为空");
                    continue;
                }
                out.ui_events.push(UiEmit { kind, value });
                out.verdicts.push(ToolVerdict {
                    name: call.name.clone(),
                    summary,
                    verdict: "applied".into(),
                    reason: "界面事件已推送".into(),
                });
            }
            // ---------- 提案档（A4）：复用收件箱链路 ----------
            "propose_fact" => {
                let Some(target) = nonempty_arg(&call.arguments, "target") else {
                    reject(&mut out.verdicts, call, &summary, "缺 target 参数");
                    continue;
                };
                let Some(facet) = nonempty_arg(&call.arguments, "facet") else {
                    reject(&mut out.verdicts, call, &summary, "缺 facet 参数");
                    continue;
                };
                let Some(value) = nonempty_arg(&call.arguments, "value") else {
                    reject(&mut out.verdicts, call, &summary, "缺 value 参数");
                    continue;
                };
                let reason = str_arg(&call.arguments, "reason").unwrap_or_default();
                // 实体已有这个 facet = 改写（收件箱措辞更准），否则新事实
                let kind = match ctx.cx.get(&target).and_then(|e| e.facts.get(&facet)) {
                    Some(_) => "fact_change",
                    None => "new_fact",
                };
                let payload = json!({
                    "target": target,
                    "value": {"facet": facet, "value": value},
                    "reason": reason,
                });
                queue_proposal(
                    &mut out,
                    &mut proposals,
                    ctx,
                    i,
                    call,
                    &summary,
                    kind,
                    payload,
                    None,
                );
            }
            "propose_entity" => {
                let Some(ty) = nonempty_arg(&call.arguments, "type") else {
                    reject(&mut out.verdicts, call, &summary, "缺 type 参数");
                    continue;
                };
                if !codex::ENTITY_TYPES.contains(&ty.as_str()) {
                    reject(&mut out.verdicts, call, &summary, format!("未知实体类型 {ty}"));
                    continue;
                }
                let Some(name) = nonempty_arg(&call.arguments, "name") else {
                    reject(&mut out.verdicts, call, &summary, "缺 name 参数");
                    continue;
                };
                let reason = str_arg(&call.arguments, "reason").unwrap_or_default();
                let skeleton = call
                    .arguments
                    .get("skeleton")
                    .and_then(|s| s.as_object())
                    .cloned()
                    .unwrap_or_default();
                if let Some(bad) = value_violation(&serde_json::Value::Object(skeleton.clone())) {
                    reject(&mut out.verdicts, call, &summary, bad);
                    continue;
                }
                // id 以提案为准（管线 new_entity 同形）：skeleton.id 优先，否则 type.name
                let target = skeleton
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{ty}.{name}"));
                let mut value = serde_json::Map::new();
                value.insert("type".into(), json!(ty));
                value.insert("name".into(), json!(name));
                for key in ["one_liner", "facts", "anchors", "look", "persona", "secrets", "notes"] {
                    if let Some(v) = skeleton.get(key) {
                        value.insert(key.into(), v.clone());
                    }
                }
                let payload = json!({"target": target, "value": value, "reason": reason});
                queue_proposal(
                    &mut out,
                    &mut proposals,
                    ctx,
                    i,
                    call,
                    &summary,
                    "new_entity",
                    payload,
                    None,
                );
            }
            "propose_relation" => {
                let Some(from) = nonempty_arg(&call.arguments, "from") else {
                    reject(&mut out.verdicts, call, &summary, "缺 from 参数");
                    continue;
                };
                let Some(to) = nonempty_arg(&call.arguments, "to") else {
                    reject(&mut out.verdicts, call, &summary, "缺 to 参数");
                    continue;
                };
                let Some(kind) = nonempty_arg(&call.arguments, "kind") else {
                    reject(&mut out.verdicts, call, &summary, "缺 kind 参数");
                    continue;
                };
                let reason = str_arg(&call.arguments, "reason").unwrap_or_default();
                let note = str_arg(&call.arguments, "note").unwrap_or_default();
                // 悬空关系确定性校验：两端都必须是设定集里已存在的实体
                'endpoints: for (label, id) in [("from", &from), ("to", &to)] {
                    if ctx.cx.get(id).is_none() {
                        reject(
                            &mut out.verdicts,
                            call,
                            &summary,
                            format!("关系端点 {label}（{id}）不在设定集里"),
                        );
                        break 'endpoints;
                    }
                }
                if out.verdicts.last().is_some_and(|v| v.verdict == "rejected") {
                    continue;
                }
                let mut rel = serde_json::Map::new();
                rel.insert("to".into(), json!(to));
                rel.insert("kind".into(), json!(kind));
                if !note.is_empty() {
                    rel.insert("note".into(), json!(note));
                }
                let payload = json!({"target": from, "value": rel, "reason": reason});
                queue_proposal(
                    &mut out,
                    &mut proposals,
                    ctx,
                    i,
                    call,
                    &summary,
                    "relation",
                    payload,
                    None,
                );
            }
            "propose_thread" => {
                let Some(title) = nonempty_arg(&call.arguments, "title") else {
                    reject(&mut out.verdicts, call, &summary, "缺 title 参数");
                    continue;
                };
                let Some(cause) = nonempty_arg(&call.arguments, "cause") else {
                    reject(&mut out.verdicts, call, &summary, "缺 cause 参数");
                    continue;
                };
                let actors: Vec<String> = call
                    .arguments
                    .get("actors")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if actors.is_empty() {
                    reject(&mut out.verdicts, call, &summary, "缺 actors 参数");
                    continue;
                }
                // A5 作用域裁剪：群聊/隔离下，开线提案必须把自己算进去
                if ctx.multi_cast && !actors.iter().any(|a| a == ctx.display_name || a == ctx.character)
                {
                    reject(
                        &mut out.verdicts,
                        call,
                        &summary,
                        "群聊里开线提案的 actors 必须包含自己",
                    );
                    continue;
                }
                let importance = num_arg(&call.arguments, "importance").unwrap_or(0.5);
                let framing = str_arg(&call.arguments, "framing").unwrap_or_default();
                // 坏窗口弃窗口保线体（A4）：时机对象宿主认不出就不带
                let resurface = call
                    .arguments
                    .get("resurface")
                    .filter(|w| w.is_object() && threads::ResurfaceWindow::from_value(w).is_some());
                let mut payload = json!({
                    "title": title,
                    "cause": cause,
                    "actors": actors,
                    "importance": importance.clamp(0.0, 1.0),
                });
                if let Some(w) = resurface {
                    payload["resurface"] = w.clone();
                }
                queue_proposal(
                    &mut out,
                    &mut proposals,
                    ctx,
                    i,
                    call,
                    &summary,
                    "thread",
                    payload,
                    (!framing.is_empty()).then_some(framing),
                );
            }
            other => {
                // A5：未知工具名静默弃 + diag（不炸轮）
                reject(&mut out.verdicts, call, &summary, "未知工具名");
                crate::diag::record("tools", format!("未知工具「{other}」已弃置"));
            }
        }
    }

    // 直写档聚合落一条 EffectEvent（事件顺序固定：效果 → 提案，重放由此确定）
    if psyche_touched {
        let mut next = ctx.state.clone();
        psyche.write_into(&mut next);
        out.state_after = next.clone();
        let patch = event::state_patch(&ctx.state, &next);
        out.bodies.push(LogBody::Effect(event::EffectEvent {
            turn: ctx.turn,
            trigger: "tool".into(),
            character: ctx.character.to_string(),
            state_set: patch,
            blackboard: blackboard.clone(),
            memory: memory.clone(),
            scene_id: ctx.scene.map(str::to_string),
            ts: ctx.ts,
        }));
    } else if !blackboard.is_empty() || !memory.is_empty() {
        out.bodies.push(LogBody::Effect(event::EffectEvent {
            turn: ctx.turn,
            trigger: "tool".into(),
            character: ctx.character.to_string(),
            state_set: Vec::new(),
            blackboard: blackboard.clone(),
            memory: memory.clone(),
            scene_id: ctx.scene.map(str::to_string),
            ts: ctx.ts,
        }));
    }

    // 提案档统一落事件：anchors 预检（驳回同管线：落 reject 事件，收件箱可见）→
    // 小事实自动接受（propose+accept 双事件，管线同形）→ 默认 propose
    for p in proposals {
        let payload = Some(p.payload.clone());
        if p.kind != "thread" {
            let conflict = ctx
                .cx
                .get(
                    p.payload
                        .get("target")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default(),
                )
                .and_then(|e| {
                    codex::anchors_conflict(e, p.payload.get("value").unwrap_or(&serde_json::Value::Null))
                });
            if let Some(reason) = conflict {
                crate::diag::record(
                    "tools",
                    format!("工具提案与辨识点冲突，已驳回：{}（{reason}）", p.id),
                );
                out.bodies.push(proposal_body(
                    ctx,
                    p.id,
                    "reject",
                    &p.kind,
                    payload,
                    Some(format!("与辨识点冲突，自动驳回：{reason}")),
                ));
                out.verdicts.push(ToolVerdict {
                    name: p.tool,
                    summary: p.summary,
                    verdict: "rejected".into(),
                    reason: format!("anchors 冲突：{reason}"),
                });
                continue;
            }
            // 小事实 + 用户开了自动接受：连落 propose 与 accept 两条事件（动作可溯源）
            if p.kind == "new_fact" && ctx.auto_minor {
                out.bodies.push(proposal_body(ctx, p.id.clone(), "propose", &p.kind, payload.clone(), None));
                out.bodies.push(proposal_body(
                    ctx,
                    p.id,
                    "accept",
                    &p.kind,
                    None,
                    Some("小事实自动接受（设置：运行期自动接受）".into()),
                ));
                out.auto_accepts.push((p.kind, p.payload));
                continue;
            }
        }
        out.bodies.push(proposal_body(ctx, p.id, "propose", &p.kind, payload, p.note));
        // 主 verdict 已在 queue_proposal 时记过（proposed）
    }

    // A5：限流汇总 + 驳回留痕（diag 溯源）
    if out.overflow > 0 {
        crate::diag::record(
            "tools",
            format!(
                "第 {} 轮 {}：{} 次工具调用超出每轮上限 {}，已全部弃置",
                ctx.turn, ctx.character, out.overflow, ctx.limit
            ),
        );
    }
    for v in &out.verdicts {
        if v.verdict == "rejected" || v.verdict == "dropped" {
            crate::diag::record(
                "tools",
                format!(
                    "第 {} 轮 {}：{} —— {}",
                    ctx.turn, ctx.character, v.summary, v.reason
                ),
            );
        }
    }
    out
}

// ---------- 帮手（模块内私有）----------

fn reject(
    verdicts: &mut Vec<ToolVerdict>,
    call: &ToolCall,
    summary: &str,
    reason: impl Into<String>,
) {
    verdicts.push(ToolVerdict {
        name: call.name.clone(),
        summary: summary.to_string(),
        verdict: "rejected".into(),
        reason: reason.into(),
    });
}

fn verdict_str(applied: bool) -> &'static str {
    if applied {
        "applied"
    } else {
        "rejected"
    }
}

/// 提案的统一前置：id 幂等（重放撞上已在流里的提案不再落事件——决断 3 的
/// 「人工确认不因改历史蒸发」）+ id 的确定性格式（`<类>.<target>.<轮>.m<调用序>`；
/// 管线提案的序号是裸 i，工具提案用 m 前缀，两套 id 永不撞号）。
fn queue_proposal(
    out: &mut ToolApplication,
    proposals: &mut Vec<PendingProposal>,
    ctx: &ToolCtx<'_>,
    i: usize,
    call: &ToolCall,
    summary: &str,
    kind: &str,
    payload: serde_json::Value,
    note: Option<String>,
) {
    let target = payload
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    let class = if kind == "thread" { "thread" } else { "codex" };
    let id = format!("{class}.{target}.{}.m{i}", ctx.turn);
    if ctx.existing_proposals.contains(&id) {
        out.verdicts.push(ToolVerdict {
            name: call.name.clone(),
            summary: summary.to_string(),
            verdict: "kept".into(),
            reason: "提案已在流里（保留物化正史，不重复入箱）".into(),
        });
        return;
    }
    out.verdicts.push(ToolVerdict {
        name: call.name.clone(),
        summary: summary.to_string(),
        verdict: "proposed".into(),
        reason: format!("已进收件箱（{id}）"),
    });
    proposals.push(PendingProposal {
        tool: call.name.clone(),
        summary: summary.to_string(),
        id,
        kind: kind.to_string(),
        payload,
        note,
    });
}

fn proposal_body(
    ctx: &ToolCtx<'_>,
    id: String,
    op: &str,
    kind: &str,
    payload: Option<serde_json::Value>,
    note: Option<String>,
) -> LogBody {
    LogBody::Proposal(event::ProposalEvent {
        turn: ctx.turn,
        id,
        op: op.into(),
        kind: kind.into(),
        origin: "model".into(),
        payload,
        note,
        ts: ctx.ts,
    })
}

/// 去空白字符串参数；缺失或纯空白返回 None
fn str_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
}

fn nonempty_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    str_arg(args, key).filter(|s| !s.is_empty())
}

fn num_arg(args: &serde_json::Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

/// 参数值的两道硬边界（A5）：字符串 ≤2KB、嵌套 ≤8。返回违规说明。
fn value_violation(v: &serde_json::Value) -> Option<String> {
    value_violation_at(v, 1)
}

fn value_violation_at(v: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > MAX_VALUE_DEPTH {
        return Some(format!("参数嵌套超过 {MAX_VALUE_DEPTH} 层"));
    }
    match v {
        serde_json::Value::String(s) => {
            if s.len() > MAX_STRING_BYTES {
                return Some("字符串参数超过 2 KB 上限".into());
            }
            None
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|it| value_violation_at(it, depth + 1)),
        serde_json::Value::Object(map) => map
            .values()
            .find_map(|it| value_violation_at(it, depth + 1)),
        _ => None,
    }
}

/// 参数摘要（verdict/检查器/诊断共用的一行）：`key=value` 截到 24 字
fn args_summary(args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    let parts: Vec<String> = obj
        .iter()
        .map(|(k, v)| {
            let rendered = match v {
                serde_json::Value::String(s) => truncate_chars(s, 24),
                other => truncate_chars(&other.to_string(), 24),
            };
            format!("{k}={rendered}")
        })
        .collect();
    parts.join(", ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: String::new(),
            name: name.into(),
            arguments: args,
        }
    }

    fn ctx<'a>(
        state: serde_json::Value,
        policy: &'a ToolPolicy,
        existing: &'a std::collections::BTreeSet<String>,
        cx: &'a codex::Codex,
    ) -> ToolCtx<'a> {
        ToolCtx {
            character: "xiaoyu",
            display_name: "小雨",
            turn: 4,
            scene: None,
            state,
            multi_cast: false,
            policy,
            limit: DEFAULT_PER_TURN,
            cx,
            auto_minor: false,
            existing_proposals: existing,
            ts: 100,
        }
    }

    #[test]
    fn direct_write_aggregates_into_one_effect_and_applies_psyche() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let state = json!({"psyche": {"affects": [], "intents": []}});
        let ctx = ctx(state, &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[
                call("psyche_feel", json!({"name": "后怕", "intensity": 0.7, "source": "刚才的巨响"})),
                call("memory_set", json!({"key": "巨响时刻", "value": "第4天20:10"})),
                call("ui_emit", json!({"kind": "emotion", "value": "后怕"})),
            ],
            &ctx,
        );
        // 一条聚合 Effect（psyche 补丁 + memory）；ui 事件只走报告不落事件
        assert_eq!(out.bodies.len(), 1, "{:?}", out.verdicts);
        match &out.bodies[0] {
            LogBody::Effect(e) => {
                assert_eq!(e.trigger, "tool");
                assert_eq!(e.character, "xiaoyu");
                assert_eq!(e.memory.len(), 1);
                assert_eq!(e.memory[0].key, "巨响时刻");
                assert!(e.state_set[0].key.starts_with("psyche"));
            }
            other => panic!("应是 Effect 事件：{other:?}"),
        }
        assert_eq!(out.ui_events.len(), 1);
        assert_eq!(out.ui_events[0].value, "后怕");
        assert!(
            out.verdicts.iter().all(|v| v.verdict == "applied"),
            "{:?}",
            out.verdicts
        );
        // 情绪真的进了槽位（应用器接共用心理运行时的证据）
        let after = Psyche::from_state(&out.state_after);
        assert_eq!(after.affect("后怕").map(|a| a.intensity), Some(0.7));
    }

    #[test]
    fn bad_calls_do_not_break_the_turn() {
        // A5 DoD：一串坏调用夹着好的——本轮正常完成，好的照常生效
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let mut c = ctx(json!({}), &policy, &existing, &codex);
        c.limit = 16; // 本测试专测校验边界；限流另有用例
        let ctx = c;
        let deep = {
            let mut v = json!("x");
            for _ in 0..9 {
                v = json!([v]);
            }
            v
        };
        let out = apply_tool_calls(
            &[
                call("time_travel", json!({"year": 3024})),                  // 未知工具
                call("psyche_feel", json!({"intensity": 0.5})),               // 缺 name
                call("blackboard_set", json!({"key": "secret", "value": 1})), // 白名单外
                call("blackboard_set", json!({"key": "place", "value": "天台"})), // ✓
                call("schedule_say", json!({"text": ""})),                    // 空话
                call("ui_emit", json!({"kind": ""})),                         // 空 kind
                call("memory_set", json!({"key": "k", "value": deep})),       // 嵌套超限
                call("psyche_intent", json!({"goal": " ", "delta": 0.5})),    // 空意图名
            ],
            &ctx,
        );
        let applied: Vec<_> = out.verdicts.iter().filter(|v| v.verdict == "applied").collect();
        assert_eq!(applied.len(), 1, "只有 place 写入生效：{:?}", out.verdicts);
        assert_eq!(out.overflow, 0);
        match &out.bodies[0] {
            LogBody::Effect(e) => {
                assert_eq!(e.blackboard[0].key, "place");
                assert!(e.state_set.is_empty(), "无 psyche 调用不产生 state 补丁");
            }
            other => panic!("应是 Effect：{other:?}"),
        }
    }

    #[test]
    fn per_turn_limit_drops_excess() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let mut c = ctx(json!({}), &policy, &existing, &codex);
        c.limit = 2;
        let calls: Vec<ToolCall> = (0..4)
            .map(|i| call("ui_emit", json!({"kind": "emotion", "value": format!("v{i}")})))
            .collect();
        let out = apply_tool_calls(&calls, &c);
        assert_eq!(out.ui_events.len(), 2, "上限内的生效");
        assert_eq!(out.overflow, 2);
        assert_eq!(out.verdicts.iter().filter(|v| v.verdict == "dropped").count(), 2);
    }

    #[test]
    fn card_policy_subtracts_tools() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy {
            allow: Vec::new(),
            deny: vec!["propose_thread".into()],
        };
        let existing = std::collections::BTreeSet::new();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[
                call("propose_thread", json!({"title": "线", "cause": "因", "actors": ["小雨"]})),
                call("psyche_feel", json!({"name": "平静", "intensity": 0.3})),
            ],
            &ctx,
        );
        assert_eq!(out.verdicts[0].verdict, "rejected", "deny 名单生效");
        assert_eq!(out.verdicts[1].verdict, "applied", "其余工具照常");
    }

    #[test]
    fn proposals_get_deterministic_ids_and_model_origin() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[
                call("propose_fact", json!({"target": "char.小雨", "facet": "schedule", "value": "周三休息", "reason": "第4轮说漏嘴"})),
                call("propose_thread", json!({"title": "周五还书", "cause": "借了书", "actors": ["小雨"], "importance": 0.6, "framing": "到期要还"})),
            ],
            &ctx,
        );
        assert_eq!(out.bodies.len(), 2, "{:?}", out.verdicts);
        assert_eq!(out.bodies[0].turn(), 4);
        match &out.bodies[0] {
            LogBody::Proposal(p) => {
                assert_eq!(p.origin, "model");
                assert_eq!(p.op, "propose");
                assert_eq!(p.id, "codex.char.小雨.4.m0", "id 沿用确定性格式（m 调用序）");
                assert_eq!(p.payload.as_ref().unwrap()["value"]["facet"], "schedule");
            }
            other => panic!("应是提案事件：{other:?}"),
        }
        match &out.bodies[1] {
            LogBody::Proposal(p) => {
                assert_eq!(p.kind, "thread");
                assert_eq!(p.note.as_deref(), Some("到期要还"), "framing 走 note（管线同形）");
                assert!(p.id.starts_with("thread."), "id: {}", p.id);
            }
            other => panic!("应是线提案：{other:?}"),
        }
    }

    #[test]
    fn replay_hits_existing_proposal_id_and_keeps_history() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing: std::collections::BTreeSet<String> = ["codex.char.小雨.4.m0".to_string()]
            .into_iter()
            .collect();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[call("propose_fact", json!({"target": "char.小雨", "facet": "schedule", "value": "周三休息", "reason": "同一条消息重放"}))],
            &ctx,
        );
        assert!(out.bodies.is_empty(), "撞上已有 id 不再落提案事件（保留正史）");
        assert_eq!(out.verdicts[0].verdict, "kept");
    }

    #[test]
    fn dangling_relation_is_rejected_deterministically() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[call("propose_relation", json!({"from": "char.不存在", "to": "char.小雨", "kind": "邻居", "reason": "编的"}))],
            &ctx,
        );
        assert!(out.bodies.is_empty());
        assert_eq!(out.verdicts[0].verdict, "rejected");
        assert!(out.verdicts[0].reason.contains("不在设定集"));
    }

    #[test]
    fn bad_resurface_window_is_dropped_but_thread_body_survives() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let out = apply_tool_calls(
            &[call("propose_thread", json!({"title": "还书", "cause": "借书", "actors": ["小雨"], "resurface": "三天后"}))],
            &ctx,
        );
        match &out.bodies[0] {
            LogBody::Proposal(p) => {
                let payload = p.payload.as_ref().unwrap();
                assert!(payload.get("resurface").is_none(), "坏窗口被弃：{payload}");
            }
            other => panic!("应是提案：{other:?}"),
        }
    }

    #[test]
    fn multi_cast_thread_requires_self_in_actors() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let mut c = ctx(json!({}), &policy, &existing, &codex);
        c.multi_cast = true;
        let out = apply_tool_calls(
            &[
                call("propose_thread", json!({"title": "别人的线", "cause": "因", "actors": ["阿澈"]})),
                call("propose_thread", json!({"title": "自己的线", "cause": "因", "actors": ["阿澈", "小雨"]})),
            ],
            &c,
        );
        assert_eq!(out.verdicts[0].verdict, "rejected", "actors 不含自己被裁");
        assert_eq!(out.verdicts[1].verdict, "proposed", "含自己（展示名）放行");
    }

    #[test]
    fn oversized_string_param_is_rejected() {
        let codex = codex::Codex::build(Vec::new());
        let policy = ToolPolicy::default();
        let existing = std::collections::BTreeSet::new();
        let ctx = ctx(json!({}), &policy, &existing, &codex);
        let big = "啊".repeat(3000);
        let out = apply_tool_calls(&[call("memory_set", json!({"key": "k", "value": big}))], &ctx);
        assert_eq!(out.verdicts[0].verdict, "rejected", "超 2KB 的字符串值被拒");
        assert!(out.verdicts[0].reason.contains("2 KB"));
    }

    #[test]
    fn contract_text_mentions_discipline_and_stays_small() {
        let text = contract_text();
        assert!(text.contains("不得描述工具使用"), "契约纪律句要在场");
        assert!(text.contains("psyche_feel"));
        assert!(text.contains("propose_thread"));
        // ≤1200 token 预算（CJK≈1 字 1 token 的保守口径：字符数即上界）
        assert!(text.chars().count() < 1200, "契约区膨胀到 {} 字符", text.chars().count());
    }

    #[test]
    fn tool_schemas_cover_exactly_the_whitelist() {
        assert_eq!(TOOL_DEFS.len(), 10, "十件：直写六 + 提案四");
        let names: Vec<_> = TOOL_DEFS.iter().map(|d| d.name).collect();
        for expected in [
            "psyche_feel",
            "psyche_intent",
            "blackboard_set",
            "memory_set",
            "schedule_say",
            "ui_emit",
            "propose_fact",
            "propose_entity",
            "propose_relation",
            "propose_thread",
        ] {
            assert!(names.contains(&expected), "缺工具 {expected}");
        }
        let schemas = tool_schemas();
        assert_eq!(schemas.len(), 10);
        assert_eq!(schemas[0]["type"], "function");
    }
}
