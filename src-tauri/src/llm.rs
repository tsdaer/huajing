//! LLM 接入（设计 §11）：OpenAI 兼容协议 + SSE 流式。
//!
//! 流解析抽象为独立的 [`SseParser`]（各家 SSE 实现差异集中于此，设计
//! 风险表：至少两家实测）。补全入口 [`chat_stream`] 逐段回调增量文本，
//! 支持取消与逐块超时。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

/// providers.json 中的一条接入点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// 用途档位：chat 主对话 / util 总结捕获（设计 §11，util 走便宜档）
    #[serde(default = "default_role")]
    pub role: String,
    /// 工具调用能力位（增强 A1 · 决断 4）：`"off" | "on"`，缺省 off。
    /// `"on"` 的接入点发请求带工具 schema；provider 不支持（400 类错误）时
    /// 本次请求自动去 tools 重试一次并留诊断（仿 C3 空正文棘轮的降级先例）。
    #[serde(default = "default_tools")]
    pub tools: String,
}

fn default_temperature() -> f32 {
    0.8
}

fn default_role() -> String {
    "chat".into()
}

fn default_tools() -> String {
    "off".into()
}

impl Provider {
    /// 工具调用是否开启（设置页按接入点开关；缺省 off = 全链路与现状一致）
    pub fn tools_enabled(&self) -> bool {
        self.tools.trim().eq_ignore_ascii_case("on")
    }
}

/// 模型返回的一次工具调用（OpenAI `tool_calls`；增强 A·决断 2/3）。
/// 原始调用随 assistant 消息存档进事件流（`Message.tool_calls`），
/// 效果不在落盘时直接写 state，由重放按「tool_calls + 确定性校验器」推导。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// 参数对象。流式增量按 index 键串接 arguments 后在收尾解析；
    /// 解析失败记 Null（应用层按坏调用弃置 + diag）。
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// 真正会被请求的补全地址（自检与真实发送共用，避免两处规则漂移）
pub fn endpoint(provider: &Provider) -> String {
    format!("{}/chat/completions", normalize_base_url(&provider.base_url))
}

/// 兼容地址里漏写 `/v1` 的情况：DeepSeek / OpenAI 这类云服务的 OpenAI 兼容路径
/// 挂在 `/v1` 下，用户从文档里复制时常只抄到域名。只对**已知主机**补，
/// 自建网关一律不猜（补错了反而更难查）。
fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    let cloud = [
        "api.deepseek.com",
        "api.openai.com",
        "api.moonshot.cn",
        "open.bigmodel.cn",
    ];
    if path.is_empty() && cloud.contains(&host) {
        return format!("{trimmed}/v1");
    }
    trimmed.to_string()
}

/// 对话消息（OpenAI 格式：system | user | assistant）。
/// `tool_calls` 两侧共用：请求侧随 assistant 历史序列化（本设计为捆绑式、不回填
/// tool result，正常不发），响应侧由流式/非流式解析填出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// 纯文本消息（绝大多数构造点用这个，省得逐处写 tool_calls: None）
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        ChatMessage {
            role: role.into(),
            content: content.into(),
            tool_calls: None,
        }
    }
}

/// `api.ui.emit` 转推给前端的一条界面事件（M1.6：表情/立绘位占位）。
/// 与 [`crate::card::UiEvent`] 同义，此处单独定义是为了让流事件不依赖卡片模块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiEmit {
    pub kind: String,
    pub value: String,
}

/// 发送给前端的流事件
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    /// 一段增量文本；`name` = 生成它的角色署名（M3.4 群聊一轮多人发言：
    /// 前端据此切换流式气泡——每个发言人一个气泡，按发言顺序排列）
    Delta {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Done {
        full: String,
        cancelled: bool,
        /// 本轮 on_message 钩子的报告（无 on_message 卡为 None）
        report: Option<HookReport>,
    },
    Error { message: String },
    /// 卡片经 `api.ui.emit` 推来的界面事件（紧跟产生它的那一步发生）；
    /// turn = 产生它的钩子轮次（M3.0 修界面事件「第 -1 轮」：前端不再从乐观消息推轮次）
    HookEvent { kind: String, value: String, turn: u64 },
    /// 导演的调度指示（M3.4 · 设计 §10.5）：这一轮谁接话、为何轮到她。
    /// 生成开始前发出——「谁在说话」在第一个字出现前就有答案
    Director {
        /// 发言人署名（按发言顺序）
        names: Vec<String>,
        /// 人读的调度依据（逐人：「小雨：被点名提及 · 剧情线「X」正被谈到」）
        brief: String,
    },
}

/// 一轮 `on_message` 钩子的执行报告（前端据此显示卡内状态与事件）
#[derive(Debug, Clone, Default, Serialize)]
pub struct HookReport {
    pub turn: u64,
    /// 卡片是否定义了该 hook（未定义时后面几项都为空）
    pub ran: bool,
    pub card_state: serde_json::Value,
    /// `api.memory.set` 的写入（已落 palace.jsonl）
    pub memory: Vec<crate::card::KvSet>,
    pub ui_events: Vec<UiEmit>,
    /// 卡内错误（沙箱错误边界捕获；只在面板里展示，不打断对话）
    pub logs: Vec<String>,
}

// ---------- SSE 解析 ----------

/// 字节级 SSE 解析器：按 `data:` 行产出事件负载。
///
/// 按 `\n` 字节切行对 UTF-8 是安全的（多字节字符的续字节不含 `\n`），
/// 因此跨块截断的多字节字符不会损坏；`\r\n` 与 `data: x`/`data:x` 均兼容；
/// 空行、注释与其它字段（`event:`/`id:`/`retry:`）忽略。
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser::default()
    }

    /// 喂入一段响应字节，返回其中完整 `data:` 行的负载（可能为空）。
    /// `[DONE]` 也作为负载返回，由调用方判断。
    /// 加固 C5：残余缓冲（无换行尾部）超过上限时报错——调用方断开连接；
    /// 已消费行一次性 drain（单 chunk 多行不再从头部逐行搬移）。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut consumed = 0usize;
        let mut scan = 0usize;
        while let Some(rel) = self.buf[scan..].iter().position(|&b| b == b'\n') {
            let pos = scan + rel;
            let line = &self.buf[consumed..pos];
            consumed = pos + 1;
            scan = consumed;
            let line = String::from_utf8_lossy(line);
            let line = line.trim_end_matches('\r');
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                if !data.is_empty() {
                    events.push(data.to_string());
                }
            }
        }
        if consumed > 0 {
            self.buf.drain(..consumed);
        }
        if self.buf.len() > SSE_BUF_LIMIT {
            return Err(format!(
                "SSE 缓冲超过 {} MB 无有效换行——响应不像 SSE 流，主动断开",
                SSE_BUF_LIMIT / 1024 / 1024
            ));
        }
        Ok(events)
    }
}

/// 一段增量内容（OpenAI 兼容流式 chunk）
#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    /// 工具调用增量分片（增强 A1）：同一 `index` 的分片串接成一次完整调用
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolDelta>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamToolDelta {
    /// 分片序号（OpenAI 约定：arguments 按它追加到对应调用上）
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamToolFn>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamToolFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// tool_calls 增量累积器：按 index 键收集、arguments 字符串追加，
/// finish 时整块交出（arguments 解析失败记 Null，由应用层按坏调用弃置）。
/// 边界（单独单测）：跨 chunk 的 arguments 分片、UTF-8 多字节截断、部分 JSON。
#[derive(Default)]
pub struct ToolCallAccum {
    calls: std::collections::BTreeMap<usize, PartialToolCall>,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    args: String,
}

impl ToolCallAccum {
    pub fn push(&mut self, deltas: &[StreamToolDelta]) {
        for d in deltas {
            let part = self.calls.entry(d.index).or_default();
            if let Some(id) = d.id.as_deref() {
                part.id = id.to_string();
            }
            if let Some(f) = &d.function {
                if let Some(name) = f.name.as_deref() {
                    part.name = name.to_string();
                }
                if let Some(args) = f.arguments.as_deref() {
                    part.args.push_str(args);
                }
            }
        }
    }

    /// 收尾：按 index 序交出完整调用。arguments 非法 JSON 时记 Null（不 panic、不丢弃
    /// 整批——坏一个调用不该连坐别的调用）。
    pub fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .map(|(_, part)| ToolCall {
                id: part.id,
                name: part.name,
                arguments: serde_json::from_str(&part.args).unwrap_or(serde_json::Value::Null),
            })
            .collect()
    }
}

/// 流式补全结果
#[derive(Debug)]
pub struct StreamOutcome {
    pub text: String,
    /// 被用户中断（保留已生成的部分文本）
    pub cancelled: bool,
    /// 末 chunk 的 finish_reason（C3 空正文棘轮的判据；服务端没带时为 None）
    pub finish_reason: Option<String>,
    /// 与正文同报文的工具调用（增强 A1 · 决断 2）；无工具轮为空
    pub tool_calls: Vec<ToolCall>,
    /// provider 不支持工具（400 类错误）→ 本次已自动去 tools 重试（界面提示用）
    pub tools_fallback: bool,
}

/// 流式中途失败：错误信息 + **已生成的部分文本**（加固 C2）。
/// 网络抖动/超时不该让已经显示出来的文字凭空消失——调用方按取消语义落盘部分文本。
#[derive(Debug, Clone)]
pub struct StreamFailure {
    pub message: String,
    pub partial: String,
    /// HTTP 状态码（A1 工具降级棘轮的判据：400 类 = provider 不支持 tools）
    pub status: Option<u16>,
}

impl StreamFailure {
    pub fn from_message(message: String) -> Self {
        StreamFailure { message, partial: String::new(), status: None }
    }
}

impl From<String> for StreamFailure {
    fn from(message: String) -> Self {
        StreamFailure::from_message(message)
    }
}

impl std::fmt::Display for StreamFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 每块之间的最长等待（无整体超时：长生成不误杀）
const CHUNK_TIMEOUT: Duration = Duration::from_secs(180);

/// 非流式调用（send + 读 body 全程）的整体超时（加固 C1）：服务端回 200 后挂住
/// 不回 body 会让总结/补全管线永久 await——不报错不重试，批次卡死到重启。
/// 流式不受它管（[`chat_stream`] 逐块超时，长生成不误杀）。
const NON_STREAM_TIMEOUT: Duration = Duration::from_secs(120);

/// SSE 缓冲上限（加固 C5）：坏网关回 200 后持续输出不含换行的字节时，
/// 缓冲无界增长直到内存耗尽——超限报错断开，而不是陪它烧内存。
const SSE_BUF_LIMIT: usize = 4 * 1024 * 1024;

/// OpenAI 兼容 `/chat/completions` 流式补全。
/// `on_delta` 逐段回调增量文本；`cancel` 置 true 后尽快返回（保留部分文本）。
/// 失败时返回 [`StreamFailure`]——已生成的增量在 `partial` 里（加固 C2）。
pub async fn chat_stream(
    provider: &Provider,
    messages: &[ChatMessage],
    on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
    extra_proxy: Option<&str>,
) -> Result<StreamOutcome, StreamFailure> {
    chat_stream_bounded(provider, messages, on_delta, cancel, extra_proxy, None, CHUNK_TIMEOUT, None).await
}

/// 带请求参数的流式补全（加固 C3 的重试入口）：
/// - `max_tokens`：Some 时随请求带上（None = 服务端默认，现状行为）；
/// - `chunk_timeout`：单块最长等待（生产用 [`CHUNK_TIMEOUT`]，测试注入小值）；
/// - `tools`：Some 时随请求带工具 schema（增强 A1；空数组等价 None）。
pub(crate) async fn chat_stream_bounded(
    provider: &Provider,
    messages: &[ChatMessage],
    mut on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
    extra_proxy: Option<&str>,
    max_tokens: Option<u32>,
    chunk_timeout: Duration,
    tools: Option<&[serde_json::Value]>,
) -> Result<StreamOutcome, StreamFailure> {
    let (client, _proxy) = build_client(extra_proxy).await.map_err(StreamFailure::from)?;
    let url = endpoint(provider);
    let mut body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": provider.temperature,
        "stream": true,
    });
    if let Some(mt) = max_tokens {
        body["max_tokens"] = serde_json::json!(mt);
    }
    if let Some(list) = tools.filter(|t| !t.is_empty()) {
        body["tools"] = serde_json::Value::Array(list.to_vec());
    }
    let resp = client
        .post(&url)
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| StreamFailure::from(format!("请求失败（{}）：{}", provider.name, error_chain(&e))))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(StreamFailure {
            message: format!(
                "{} 返回 {}：{}",
                provider.name,
                status,
                truncate(&detail, 500)
            ),
            partial: String::new(),
            status: Some(status.as_u16()),
        });
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut tool_acc = ToolCallAccum::default();
    let mut full = String::new();
    let mut cancelled = false;
    let mut finish_reason: Option<String> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        // 失败路径带出已生成的增量（C2）：流中途断掉时，已显示的文字不能凭空消失
        let chunk = tokio::time::timeout(chunk_timeout, stream.next())
            .await
            .map_err(|_| StreamFailure {
                message: format!("{} 响应超时（{} 秒无数据）", provider.name, chunk_timeout.as_secs()),
                partial: full.clone(),
                status: None,
            })?
            .transpose()
            .map_err(|e| StreamFailure {
                message: format!("{} 流读取失败：{e}", provider.name),
                partial: full.clone(),
                status: None,
            })?;
        let Some(bytes) = chunk else { break };

        let events = parser.feed(&bytes).map_err(|e| StreamFailure {
            message: format!("{} {e}", provider.name),
            partial: full.clone(),
            status: None,
        })?;
        for data in events {
            if data == "[DONE]" {
                return Ok(StreamOutcome {
                    text: full,
                    cancelled: false,
                    finish_reason,
                    tool_calls: tool_acc.finish(),
                    tools_fallback: false,
                });
            }
            let Ok(parsed) = serde_json::from_str::<StreamChunk>(&data) else {
                continue; // 非 JSON 负载（心跳等）跳过
            };
            for choice in parsed.choices {
                if choice.finish_reason.is_some() {
                    finish_reason = choice.finish_reason;
                }
                if let Some(delta) = choice.delta.content {
                    if !delta.is_empty() {
                        on_delta(&delta);
                        full.push_str(&delta);
                    }
                }
                // 工具分片与正文交错到达（决断 2 的同报文形态）：各自累积，互不挤占
                if let Some(deltas) = choice.delta.tool_calls {
                    tool_acc.push(&deltas);
                }
            }
        }
    }
    // 流结束但未见 [DONE]：按已收内容收尾
    Ok(StreamOutcome {
        text: full,
        cancelled,
        finish_reason,
        tool_calls: tool_acc.finish(),
        tools_fallback: false,
    })
}

/// 带空正文棘轮的流式补全（加固 C3）：推理模型在服务端默认预算内把 token 耗在
/// 思考上时，正文为空但流正常 [DONE] 结束——非流式的 self_or_retry 专门处理了
/// 这个场景，流式此前没有等价物。判据与非流式一致（finish_reason=length），
/// 预算复用 [`retry_budget`]（8192 起翻倍，封顶 32768）；取消/有正文不重试。
pub async fn chat_stream_ratcheted(
    provider: &Provider,
    messages: &[ChatMessage],
    mut on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
    extra_proxy: Option<&str>,
) -> Result<StreamOutcome, StreamFailure> {
    chat_stream_ratcheted_inner(provider, messages, &mut on_delta, cancel, extra_proxy, None).await
}

/// [`chat_stream_ratcheted`] 的工具版内核：`tools` 随每梯重试一起带上
/// （空正文重试与工具无交集，同一轮的请求形态保持一致）。
pub(crate) async fn chat_stream_ratcheted_inner(
    provider: &Provider,
    messages: &[ChatMessage],
    mut on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
    extra_proxy: Option<&str>,
    tools: Option<&[serde_json::Value]>,
) -> Result<StreamOutcome, StreamFailure> {
    let mut budget: Option<u32> = None;
    loop {
        let outcome = chat_stream_bounded(
            provider,
            messages,
            &mut on_delta,
            cancel,
            extra_proxy,
            budget,
            CHUNK_TIMEOUT,
            tools,
        )
        .await?;
        let empty = outcome.text.trim().is_empty();
        if !outcome.cancelled && empty && outcome.finish_reason.as_deref() == Some("length") {
            let last = budget.unwrap_or(4096);
            match retry_budget(last) {
                Some(next) => {
                    budget = Some(next);
                    continue;
                }
                None => {
                    return Err(StreamFailure {
                        message: format!(
                            "{} 的思考耗尽了流式预算（{}），加大预算后仍无正文",
                            provider.name, last
                        ),
                        partial: outcome.text,
                        status: None,
                    })
                }
            }
        }
        return Ok(outcome);
    }
}

/// 主对话的流式入口（增强 A1）：`tools` 为 Some 且非空时随请求带工具 schema
/// （工具与剧情同报文，决断 2）。provider 对 tools 回 400 类错误（不支持功能
/// 调用）→ 本次请求自动去 tools 重试一次，`tools_fallback` 置 true + diag 留痕
/// （仿 C3 空正文棘轮的降级先例）。400 前不会有任何增量回调，重试不产生重复文本。
pub async fn chat_stream_auto(
    provider: &Provider,
    messages: &[ChatMessage],
    tools: Option<&[serde_json::Value]>,
    mut on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
    extra_proxy: Option<&str>,
) -> Result<StreamOutcome, StreamFailure> {
    let enabled = tools.map(|t| !t.is_empty()).unwrap_or(false);
    if !enabled {
        return chat_stream_ratcheted(provider, messages, on_delta, cancel, extra_proxy).await;
    }
    match chat_stream_ratcheted_inner(provider, messages, &mut on_delta, cancel, extra_proxy, Some(tools.unwrap()))
        .await
    {
        Ok(outcome) => Ok(outcome),
        Err(f) if f.status.is_some_and(|s| (400..500).contains(&s)) => {
            crate::diag::record(
                "tools",
                format!(
                    "接入点「{}」拒绝了工具请求（{}），本次已自动去工具重试；建议在设置页关闭该接入点的工具开关",
                    provider.name, f.message
                ),
            );
            let mut outcome =
                chat_stream_ratcheted_inner(provider, messages, &mut on_delta, cancel, extra_proxy, None).await?;
            outcome.tools_fallback = true;
            Ok(outcome)
        }
        Err(f) => Err(f),
    }
}

/// 把 reqwest 的错误链摊平成一行：只印顶层 `Display` 会丢掉真正的原因
/// （DNS 失败 / 连接被拒 / TLS 握手失败 / 证书不受信 都在 source 链里）。
/// 这是「请求失败」类问题的唯一线索来源，务必保留。
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut cursor = err.source();
    while let Some(cause) = cursor {
        let text = cause.to_string();
        // 相邻层级偶有重复文案，去重后更易读
        if parts.last().map(|p| p != &text).unwrap_or(true) {
            parts.push(text);
        }
        cursor = cause.source();
    }
    parts.join(" ← ")
}

/// 非流式补全（连通性自检用；正式对话一律走 [`chat_stream`]）
pub async fn chat_once(
    provider: &Provider,
    messages: &[ChatMessage],
    extra_proxy: Option<&str>,
) -> Result<String, String> {
    chat_once_bounded(provider, messages, extra_proxy, NON_STREAM_TIMEOUT).await
}

/// [`chat_once`] 的可注入超时版（加固 C1 的测试入口；生产用 [`NON_STREAM_TIMEOUT`]）
async fn chat_once_bounded(
    provider: &Provider,
    messages: &[ChatMessage],
    extra_proxy: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    with_non_stream_timeout(&provider.name, timeout, async {
    let (client, _proxy) = build_client(extra_proxy).await?;
    let url = format!("{}/chat/completions", normalize_base_url(&provider.base_url));
    let body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": 16,
        "stream": false,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败（{}）：{}", provider.name, error_chain(&e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{} 返回 {}：{}",
            provider.name,
            status,
            truncate(&detail, 300)
        ));
    }
    Ok(truncate(&resp.text().await.unwrap_or_default(), 200))
    })
    .await
}

/// 通用**非流式**补全：自动总结 / 设定捕获 / 补全 / 分类这类实用档调用走这里
/// （设计 §11：不同用途可用不同档位；正式对话一律走 chat_stream）。
///
/// 与 chat_once（连通性自检）的区别：max_tokens 与 temperature 由调用方给，
/// 返回值是模型正文而不是截断的原始响应。
pub async fn chat_complete(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
) -> Result<String, String> {
    // 加固 C4：开头的 client/url/body 是建了不用的死代码（真正发送在 self_or_retry 里
    // 完整重来一遍），删除；client 由 build_client 的缓存兜住重复探测
    self_or_retry(provider, messages, max_tokens, temperature, extra_proxy, 0).await
}

/// 非流式调用的预算棘轮：推理型模型的思考 token 计入 max_tokens（M2.6 的教训），
/// 思考太长会「finish_reason=length、正文为空」。内容为空且被长度截断时，
/// 预算翻倍重试（×2、封顶 32768、至多四梯）——非推理模型永远一次成功，不多花一分钱。
/// M3.11 真机验收实锤：deepseek-flash 在 2048 下思考耗掉 7692 字符，素材管线的
/// 分类/归纳全数返回空。
fn retry_budget(max_tokens: u32) -> Option<u32> {
    let next = max_tokens.saturating_mul(2);
    if next == max_tokens || next > 32768 {
        None
    } else {
        Some(next)
    }
}

/// 非流式补全的完整结果：正文 + 工具调用（增强 A1；工具轮正文可能为空）。
/// 当前由单测消费；剧场/推理型路径接入时即为生产入口。
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct CompleteOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// provider 不支持工具 → 本次已自动去 tools 重试
    pub tools_fallback: bool,
}

/// 通用**非流式**补全（带工具版，增强 A1）：`tools` 为 Some 且非空时随请求带
/// 工具 schema；provider 回 400 类错误 → 自动去 tools 重试一次 + diag。
/// 空正文预算棘轮照常生效（工具调用不算正文——纯工具轮仍可能因思考耗预算而空）。
/// 当前由单测消费；剧场/推理型路径接入时即为生产入口。
#[allow(dead_code)]
pub async fn chat_complete_auto(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
    tools: Option<&[serde_json::Value]>,
) -> Result<CompleteOutcome, String> {
    let enabled = tools.map(|t| !t.is_empty()).unwrap_or(false);
    if !enabled {
        return chat_complete(provider, messages, max_tokens, temperature, extra_proxy)
            .await
            .map(|text| CompleteOutcome { text, ..Default::default() });
    }
    let list = tools.unwrap();
    match complete_with_tools(provider, messages, max_tokens, temperature, extra_proxy, list).await {
        Ok(outcome) => Ok(outcome),
        Err(ErrWithStatus { message, status: Some(s), .. }) if (400..500).contains(&s) => {
            crate::diag::record(
                "tools",
                format!(
                    "接入点「{}」拒绝了工具请求（{message}），本次已自动去工具重试",
                    provider.name
                ),
            );
            // 去 tools 重试：与流式降级同款，整轮重来一遍
            let mut outcome = complete_with_tools(provider, messages, max_tokens, temperature, extra_proxy, &[])
                .await
                .map_err(|e| e.message)?;
            outcome.tools_fallback = true;
            Ok(outcome)
        }
        Err(e) => Err(e.message),
    }
}

/// 带状态码的错误（工具降级棘轮要分清「400 类 = 不支持」与其它失败）
struct ErrWithStatus {
    message: String,
    status: Option<u16>,
    /// 400 类错误的响应体摘要（诊断用）
    partial: Option<String>,
}

async fn complete_with_tools(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
    tools: &[serde_json::Value],
) -> Result<CompleteOutcome, ErrWithStatus> {
    // 空正文预算棘轮（C3 同款）：纯工具轮正文为空属正常，棘轮只在
    // finish=length 且无工具调用时触发，避免给工具轮白烧一遍预算
    let mut budget = max_tokens;
    loop {
        let (outcome, finish_reason) =
            chat_once_tools_bounded(provider, messages, budget, temperature, extra_proxy, tools).await?;
        let empty = outcome.text.trim().is_empty();
        if empty
            && outcome.tool_calls.is_empty()
            && finish_reason.as_deref() == Some("length")
        {
            match retry_budget(budget) {
                Some(next) => {
                    budget = next;
                    continue;
                }
                None => {
                    return Err(ErrWithStatus {
                        message: format!(
                            "{} 的思考耗尽了 max_tokens（{budget}），加大预算后仍无正文",
                            provider.name
                        ),
                        status: None,
                        partial: None,
                    })
                }
            }
        }
        return Ok(outcome);
    }
}

/// 一次带工具的非流式请求（整体超时沿用 NON_STREAM_TIMEOUT）
async fn chat_once_tools_bounded(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
    tools: &[serde_json::Value],
) -> Result<(CompleteOutcome, Option<String>), ErrWithStatus> {
    let future = async {
        let (client, _proxy) = build_client(extra_proxy).await.map_err(|e| ErrWithStatus {
            message: e,
            status: None,
            partial: None,
        })?;
        let url = format!("{}/chat/completions", normalize_base_url(&provider.base_url));
        let mut body = serde_json::json!({
            "model": provider.model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools.to_vec());
        }
        let resp = client
            .post(&url)
            .bearer_auth(&provider.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ErrWithStatus {
                message: format!("请求失败（{}）：{}", provider.name, error_chain(&e)),
                status: None,
                partial: None,
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            return Err(ErrWithStatus {
                message: format!("{} 返回 {}：{}", provider.name, status, truncate(&detail, 300)),
                status: Some(status.as_u16()),
                partial: Some(truncate(&detail, 300)),
            });
        }
        let value: serde_json::Value = resp.json().await.map_err(|e| ErrWithStatus {
            message: format!("响应解析失败（{}）：{e}", provider.name),
            status: None,
            partial: None,
        })?;
        let choice = value.get("choices").and_then(|c| c.get(0));
        let message = choice.and_then(|c| c.get("message"));
        let text = message
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_string)
            .ok_or_else(|| ErrWithStatus {
                message: format!(
                    "响应里没有 choices[0].message.content：{}",
                    truncate(&value.to_string(), 300)
                ),
                status: None,
                partial: None,
            })?;
        let tool_calls = message
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let name = c
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())?
                            .to_string();
                        let id = c
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let args_raw = c
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("");
                        Some(ToolCall {
                            id,
                            name,
                            arguments: serde_json::from_str(args_raw)
                                .unwrap_or(serde_json::Value::Null),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let finish_reason = choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(|c| c.as_str())
            .map(str::to_string);
        Ok((CompleteOutcome { text, tool_calls, tools_fallback: false }, finish_reason))
    };
    match tokio::time::timeout(NON_STREAM_TIMEOUT, future).await {
        Ok(r) => r,
        Err(_) => Err(ErrWithStatus {
            message: format!(
                "{} 非流式响应超时（{} 秒内无完整响应）",
                provider.name,
                NON_STREAM_TIMEOUT.as_secs()
            ),
            status: None,
            partial: None,
        }),
    }
}

fn self_or_retry<'a>(
    provider: &'a Provider,
    messages: &'a [ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&'a str>,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>> {
    Box::pin(async move {
        let (content, finish_reason) = chat_once_full(provider, messages, max_tokens, temperature, extra_proxy).await?;
        let empty = content.trim().is_empty();
        if empty && finish_reason.as_deref() == Some("length") {
            if depth < 4 {
                if let Some(next) = retry_budget(max_tokens) {
                    return self_or_retry(provider, messages, next, temperature, extra_proxy, depth + 1).await;
                }
            }
            return Err(format!(
                "{} 的思考耗尽了 max_tokens（{}），加大预算后仍无正文",
                provider.name, max_tokens
            ));
        }
        Ok(content)
    })
}

/// 一次非流式请求的完整结果：正文 + finish_reason（预算棘轮靠它判断空正文的原因）
async fn chat_once_full(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
) -> Result<(String, Option<String>), String> {
    chat_once_full_bounded(provider, messages, max_tokens, temperature, extra_proxy, NON_STREAM_TIMEOUT).await
}

/// [`chat_once_full`] 的可注入超时版（加固 C1 的测试入口）
async fn chat_once_full_bounded(
    provider: &Provider,
    messages: &[ChatMessage],
    max_tokens: u32,
    temperature: f32,
    extra_proxy: Option<&str>,
    timeout: Duration,
) -> Result<(String, Option<String>), String> {
    with_non_stream_timeout(&provider.name, timeout, async {
    let (client, _proxy) = build_client(extra_proxy).await?;
    let url = format!("{}/chat/completions", normalize_base_url(&provider.base_url));
    let body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "stream": false,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败（{}）：{}", provider.name, error_chain(&e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{} 返回 {}：{}",
            provider.name,
            status,
            truncate(&detail, 300)
        ));
    }
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("响应解析失败（{}）：{e}", provider.name))?;
    let choice = value.get("choices").and_then(|c| c.get(0));
    let content = choice
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "响应里没有 choices[0].message.content：{}",
                truncate(&value.to_string(), 300)
            )
        })?;
    let finish_reason = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|c| c.as_str())
        .map(str::to_string);
    Ok((content, finish_reason))
    })
    .await
}

/// 非流式调用的整体超时包装（加固 C1）：超时映射成可读错误，管线不再永久挂死
async fn with_non_stream_timeout<T, F>(
    provider_name: &str,
    timeout: Duration,
    fut: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "{} 非流式响应超时（{} 秒内无完整响应）",
            provider_name,
            timeout.as_secs()
        )),
    }
}

/// embeddings 地址（自检与真实调用共用，避免两处规则漂移）
pub fn embeddings_endpoint(provider: &Provider) -> String {
    format!("{}/embeddings", normalize_base_url(&provider.base_url))
}

/// 从 `/v1/embeddings` 的响应 JSON 里按 `index` 排序抽出向量（OpenAI 兼容形态，
/// Ollama 同协议）。条目缺 `embedding` / 索引不齐 / 维度为零都算脏响应，由调用方
/// 报错——向量对错实体是静默串台，宁可拒绝。
fn parse_embeddings_response(value: &serde_json::Value) -> Result<Vec<Vec<f32>>, String> {
    let data = value
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| {
            format!(
                "embeddings 响应里没有 data 数组：{}",
                truncate(&value.to_string(), 300)
            )
        })?;
    let mut rows: Vec<(usize, Vec<f32>)> = data
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let embedding = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .ok_or_else(|| format!("embeddings 响应第 {i} 条缺 embedding 数组"))?;
            if embedding.is_empty() {
                return Err(format!("embeddings 响应第 {i} 条是空向量"));
            }
            let index = item.get("index").and_then(|x| x.as_u64()).unwrap_or(i as u64) as usize;
            Ok((index, embedding))
        })
        .collect::<Result<Vec<_>, String>>()?;
    rows.sort_by_key(|(i, _)| *i);
    // 索引必须恰好是 0..n（缺位/重复意味着向量与输入的对应关系已不可信）
    if rows.iter().enumerate().any(|(i, (idx, _))| *idx != i) {
        return Err("embeddings 响应的 index 不连续（部分输入被服务端丢弃？）".to_string());
    }
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

/// OpenAI 兼容 `/v1/embeddings` 批量嵌入（M3.10 · 设计 §6.13 语义关联层；
/// 非流式，Ollama 同协议）。返回向量顺序与 `inputs` 一一对应。
pub async fn embeddings(
    provider: &Provider,
    inputs: &[String],
    extra_proxy: Option<&str>,
) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    with_non_stream_timeout(&provider.name, NON_STREAM_TIMEOUT, async {
    let (client, _proxy) = build_client(extra_proxy).await?;
    let body = serde_json::json!({
        "model": provider.model,
        "input": inputs,
    });
    let resp = client
        .post(embeddings_endpoint(provider))
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败（{}）：{}", provider.name, error_chain(&e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{} 返回 {}：{}",
            provider.name,
            status,
            truncate(&detail, 300)
        ));
    }
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("响应解析失败（{}）：{e}", provider.name))?;
    parse_embeddings_response(&value)
    })
    .await
}

/// 环境变量里的代理（reqwest 的 system-proxy 也会读它们）
fn proxy_from_env() -> Option<(String, String)> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some((value.to_string(), format!("环境变量 {key}")));
            }
        }
    }
    None
}

/// Windows 的「系统代理」（Internet 选项）。它不落环境变量，reqwest 的
/// system-proxy 读不到——而国内用户恰恰常在这里开着本地代理（Clash/v2ray 等），
/// 于是直连必然超时。这里读注册表把它捡回来。
#[cfg(windows)]
fn proxy_from_system() -> Option<(String, String)> {
    use std::process::Command;
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let out = Command::new("reg").args(["query", key]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let enabled = text
        .lines()
        .find(|l| l.contains("ProxyEnable"))
        .and_then(|l| l.split_whitespace().last())
        .map(|v| v.trim() == "0x1")
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let server = text
        .lines()
        .find(|l| l.contains("ProxyServer") && !l.contains("ProxyServer."))
        .and_then(|l| l.split_once("REG_SZ"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    // 形如 `127.0.0.1:7890`（也可能带协议或 `http=...;https=...` 分号写法）
    let first = server.split(';').next().unwrap_or("").trim();
    let hostport = first.rsplit('=').next().unwrap_or(first).trim();
    if hostport.is_empty() {
        return None;
    }
    let url = if hostport.starts_with("http://") || hostport.starts_with("socks") {
        hostport.to_string()
    } else {
        format!("http://{hostport}")
    };
    Some((url, "Windows 系统代理".into()))
}

#[cfg(not(windows))]
fn proxy_from_system() -> Option<(String, String)> {
    None
}

/// 本地代理是否真的在监听：没开代理软件时直接连（否则会把本来能通的请求也弄断）
async fn proxy_reachable(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("socks5://"))
        .or_else(|| url.strip_prefix("socks5h://"))
        .or_else(|| url.strip_prefix("socks4://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (authority.to_string(), "8080".to_string()),
    };
    // 先解析成 SocketAddr 再连：避免为 tuple 形式引入 ToSocketAddrs 的额外类型约束
    let Ok(mut addrs) = tokio::net::lookup_host((host.as_str(), 0u16)).await else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let socket = match port.parse::<u16>() {
        Ok(p) => std::net::SocketAddr::new(addr.ip(), p),
        Err(_) => return false,
    };
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(300),
            tokio::net::TcpStream::connect(socket)
        )
        .await,
        Ok(Ok(_))
    )
}

/// 这台机器上配了的代理（自检与界面展示用；不含凭据）
pub fn proxy_env() -> Vec<String> {
    let mut out = Vec::new();
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                out.push(format!("{key}={}", scrub_credentials(&value)));
            }
        }
    }
    if let Some((url, _)) = proxy_from_system() {
        out.push(format!("系统代理={}", scrub_credentials(&url)));
    }
    out.sort();
    out.dedup();
    out
}

/// 最终采用的代理：显式设置 > 环境变量 > 系统代理（且必须真的在监听）。
/// 返回 `(代理 URL, 来源说明)`；`extra` 是用户在设置页手填的代理。
async fn resolve_proxy(extra: Option<&str>) -> Option<(String, String)> {
    if let Some(raw) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        return Some((raw.to_string(), "设置页手填".into()));
    }
    if let Some(found) = proxy_from_env() {
        return Some(found); // 环境变量是用户显式意图，即使代理没开也照用（报错更直白）
    }
    // 加固 C4：reg 子进程查询是阻塞调用，别在 async 上下文里直接跑
    let system = tokio::task::spawn_blocking(proxy_from_system)
        .await
        .unwrap_or(None);
    match system {
        Some((url, source)) if proxy_reachable(&url).await => Some((url, source)),
        _ => None,
    }
}

/// 按「用户手填代理」缓存的客户端池（加固 C4）：build_client 每次调用都会做
/// 代理探测——Windows 上 spawn 一个 reg 子进程 + 300ms TCP 探测，预算棘轮一次
/// 重试最多 5 遍完整探测。键是归一化后的手填代理（None 与空串等价）；
/// 系统代理在运行期变化不在缓存失效范围内（重启后重新探测，行为与设置页手填一致）。
fn client_cache() -> &'static std::sync::Mutex<HashMap<Option<String>, (reqwest::Client, Option<String>)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Option<String>, (reqwest::Client, Option<String>)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 建一个 HTTP 客户端（同参数跨调用复用缓存实例）
pub async fn build_client(extra_proxy: Option<&str>) -> Result<(reqwest::Client, Option<String>), String> {
    let key: Option<String> = extra_proxy
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Ok(cache) = client_cache().lock() {
        if let Some(hit) = cache.get(&key) {
            return Ok(hit.clone());
        }
    }
    let built = build_client_uncached(extra_proxy).await?;
    if let Ok(mut cache) = client_cache().lock() {
        cache.insert(key, built.clone());
    }
    Ok(built)
}

async fn build_client_uncached(extra_proxy: Option<&str>) -> Result<(reqwest::Client, Option<String>), String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("huajing/", env!("CARGO_PKG_VERSION")));
    let mut used = None;
    if let Some((url, source)) = resolve_proxy(extra_proxy).await {
        let proxy = reqwest::Proxy::all(&url)
            .map_err(|e| format!("代理地址不可用（{source}：{}）：{e}", scrub_credentials(&url)))?;
        builder = builder.proxy(proxy);
        used = Some(format!("{}（{source}）", scrub_credentials(&url)));
    }
    let client = builder
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败：{e}"))?;
    Ok((client, used))
}

/// 代理地址里的账号密码不进界面/日志
fn scrub_credentials(raw: &str) -> String {
    match (raw.find("//"), raw.rfind('@')) {
        (Some(start), Some(at)) if at > start + 2 => {
            format!("{}//***@{}", &raw[..start], &raw[at + 1..])
        }
        _ => raw.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}


#[cfg(test)]
mod ratchet_tests {
    use super::*;

    /// M3.11 真机验收的预算棘轮：推理型模型的思考耗尽 max_tokens 时 ×2 重试
    /// （至多四梯），封顶 32768、到顶不再重试（宁可报错也不无限烧钱）。
    #[test]
    fn retry_budget_doubles_until_cap() {
        assert_eq!(retry_budget(1024), Some(2048));
        assert_eq!(retry_budget(2048), Some(4096));
        assert_eq!(retry_budget(4096), Some(8192));
        assert_eq!(retry_budget(8192), Some(16384));
        assert_eq!(retry_budget(16384), Some(32768));
        assert_eq!(retry_budget(32768), None, "32768 是封顶：再往上翻倍即不重试");
    }

    // ---------- 加固 C1-C5 的钉子用例（本地 mock 服务端，不打真实网络） ----------

    fn test_provider(base_url: &str) -> Provider {
        Provider {
            name: "mock".into(),
            base_url: base_url.into(),
            api_key: "k".into(),
            model: "m".into(),
            temperature: 0.0,
            role: "chat".into(),
            tools: "off".into(),
        }
    }

    /// mock 服务端读一整条 HTTP 请求（头 + Content-Length 的 body）——
    /// 单次 read 只会拿到一个 TCP 段，头和体经常分离
    async fn read_http_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let text = String::from_utf8_lossy(&buf);
            if let Some(pos) = text.find("\r\n\r\n") {
                let clen = text[..pos]
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + clen {
                    return text.into_owned();
                }
            }
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return text.into_owned();
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// C1：服务端回 200 后装死不回 body——非流式调用按整体超时报错，而不是永久挂死
    #[test]
    fn non_stream_call_times_out_when_server_hangs() {
        rt().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{\"partial\":",
                    )
                    .await;
                // 装死：不再发数据也不关连接
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let err = chat_once_full_bounded(
                &provider,
                &[ChatMessage::text("user", "hi")],
                16,
                0.0,
                None,
                Duration::from_millis(300),
            )
            .await
            .unwrap_err();
            assert!(err.contains("超时"), "{err}");
            // chat_once（自检路径）同样受整体超时保护
            let err = chat_once_bounded(
                &provider,
                &[ChatMessage::text("user", "hi")],
                None,
                Duration::from_millis(300),
            )
            .await
            .unwrap_err();
            assert!(err.contains("超时"), "{err}");
        });
    }

    /// C2：流中途失败（块间超时）时，已生成的增量必须随错误带回
    #[test]
    fn stream_failure_carries_partial_text() {
        rt().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"早\"}}]}\n\n";
                let _ = sock.write_all(format!("{head}{body}").as_bytes()).await;
                // 装死：不发新块也不关流 → 块间超时走错误路径
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let cancel = AtomicBool::new(false);
            let err = chat_stream_bounded(
                &provider,
                &[ChatMessage::text("user", "hi")],
                |_| {},
                &cancel,
                None,
                None,
                Duration::from_millis(300),
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(err.partial, "早", "失败要带回已生成的增量：{err:?}");
            assert!(err.message.contains("超时"), "{err}");
        });
    }

    /// C3：空正文 + finish_reason=length（思考耗尽预算）触发带预算重试
    #[test]
    fn stream_ratchet_retries_empty_length_with_bigger_budget() {
        rt().block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let sse = |delta: &str, finish: Option<&str>| {
                    let mut payload = String::new();
                    if delta.is_empty() && finish.is_some() {
                        payload.push_str(
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
                        );
                    } else {
                        payload.push_str(&format!(
                            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{delta}\"}}}}]}}\n\n"
                        ));
                    }
                    payload.push_str("data: [DONE]\n\n");
                    payload
                };
                // 第一条连接：空正文 + length；断言请求里没带 max_tokens（服务端默认预算）
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let first_req = read_http_request(&mut sock).await;
                assert!(!first_req.contains("max_tokens"), "首请求不带预算：{first_req}");
                let body = sse("", Some("length"));
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
                // 第二条连接：带 max_tokens=8192 的重试；正常正文
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let retry_req = read_http_request(&mut sock).await;
                assert!(
                    retry_req.contains("\"max_tokens\":8192"),
                    "重试要带翻倍预算：{retry_req}"
                );
                let body = sse("正文来了", None);
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let cancel = AtomicBool::new(false);
            let mut seen = String::new();
            let out = chat_stream_ratcheted(
                &provider,
                &[ChatMessage::text("user", "hi")],
                |d| seen.push_str(d),
                &cancel,
                None,
            )
            .await
            .unwrap();
            assert_eq!(out.text, "正文来了", "重试后的正文非空");
            assert_eq!(seen, "正文来了");
        });
    }

    /// C4：build_client 同参数复用缓存实例（代理探测不再每次重跑）
    #[test]
    fn build_client_caches_by_proxy_key() {
        rt().block_on(async {
            let _ = build_client(None).await.unwrap();
            assert!(
                client_cache().lock().unwrap().contains_key(&None),
                "None 键要进缓存"
            );
            // 不同键各建各的（语法合法的代理地址不会在 build 阶段拨号）
            let _ = build_client(Some("http://127.0.0.1:1")).await.unwrap();
            assert!(client_cache().lock().unwrap().contains_key(&Some("http://127.0.0.1:1".into())));
        });
    }

    /// C5：SSE 缓冲超限报错；单 chunk 多行与逐字节喂的解析结果一致
    #[test]
    fn sse_buf_capped_and_multiline_chunk_matches_streaming() {
        let mut p = SseParser::new();
        let big = vec![b'x'; SSE_BUF_LIMIT + 1];
        assert!(p.feed(&big).is_err(), "超限要报错断开");

        let lines: &[u8] = b"data: a\ndata: b\n\ndata: c\n";
        let mut whole = SseParser::new();
        let a = whole.feed(lines).unwrap();
        let mut drip = SseParser::new();
        let mut b = Vec::new();
        for byte in lines {
            b.extend(drip.feed(&[*byte]).unwrap());
        }
        assert_eq!(a, b, "整段喂与逐字节喂解析一致");
        assert_eq!(a, vec!["a", "b", "c"]);
    }

    // ---------- 增强 A1 · tool_calls 增量解析与降级棘轮 ----------

    /// 同一 index 的 arguments 分片串接；不同 index 各自成一次调用
    #[test]
    fn tool_call_accum_merges_split_arguments_by_index() {
        let mut acc = ToolCallAccum::default();
        acc.push(&[StreamToolDelta {
            index: 0,
            id: Some("call_1".into()),
            function: Some(StreamToolFn { name: Some("psyche_feel".into()), arguments: Some("{\"na".into()) }),
        }]);
        acc.push(&[StreamToolDelta {
            index: 0,
            id: None,
            function: Some(StreamToolFn { name: None, arguments: Some("me\":\"开心\"}".into()) }),
        }]);
        acc.push(&[StreamToolDelta {
            index: 1,
            id: Some("call_2".into()),
            function: Some(StreamToolFn { name: Some("ui_emit".into()), arguments: Some("{}".into()) }),
        }]);
        let calls = acc.finish();
        assert_eq!(calls.len(), 2, "两个 index = 两次调用");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "psyche_feel");
        assert_eq!(calls[0].arguments["name"], "开心", "跨 chunk 的 arguments 串接成完整 JSON");
        assert_eq!(calls[1].name, "ui_emit");
    }

    /// 参数截断（中断/长度截断）：坏 JSON 记 Null，不连坐同批的其它调用
    #[test]
    fn tool_call_accum_survives_partial_json() {
        let mut acc = ToolCallAccum::default();
        acc.push(&[StreamToolDelta {
            index: 0,
            id: Some("a".into()),
            function: Some(StreamToolFn { name: Some("memory_set".into()), arguments: Some("{\"key\":".into()) }),
        }]);
        acc.push(&[StreamToolDelta {
            index: 1,
            id: Some("b".into()),
            function: Some(StreamToolFn { name: Some("ui_emit".into()), arguments: Some("{\"kind\":\"emotion\"}".into()) }),
        }]);
        let calls = acc.finish();
        assert!(calls[0].arguments.is_null(), "截断参数记 Null");
        assert_eq!(calls[1].arguments["kind"], "emotion", "坏调用不连坐别的调用");
    }

    #[test]
    fn stream_chunk_parses_tool_call_deltas_alongside_content() {
        let chunk: StreamChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"content":"（她笑）","tool_calls":[{"index":0,"id":"c1","function":{"name":"psyche_feel","arguments":"{\"name\":\"开心\"}"}}]}}]}"#,
        )
        .unwrap();
        let c = &chunk.choices[0];
        assert_eq!(c.delta.content.as_deref(), Some("（她笑）"), "正文与工具分片同 chunk 交错");
        let d = c.delta.tool_calls.as_ref().unwrap();
        assert_eq!(d[0].index, 0);
        assert_eq!(d[0].function.as_ref().unwrap().name.as_deref(), Some("psyche_feel"));
        // 旧形态（无 tool_calls 键）照常解析
        let plain: StreamChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).unwrap();
        assert!(plain.choices[0].delta.tool_calls.is_none());
    }

    /// 全链路（字节 → SSE 行 → chunk → 累积器）在**任意字节处**切开都与整段一致：
    /// 行缓冲保证多字节字符不被撕裂；工具分片与正文交错各自归位
    #[test]
    fn tool_stream_end_to_end_identical_at_every_byte_split() {
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"早\"}}]}\n\n".to_string()
            + "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"blackboard_set\",\"arguments\":\"{\\\"place\\\": \\\"天台\\\"}\"}}]}}]}\n\n"
            + "data: [DONE]\n\n";
        let drain = |bytes: &[u8]| {
            let mut p = SseParser::new();
            let mut acc = ToolCallAccum::default();
            let mut text = String::new();
            for data in p.feed(bytes).unwrap() {
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str::<StreamChunk>(&data) {
                    for choice in parsed.choices {
                        if let Some(d) = choice.delta.content {
                            text.push_str(&d);
                        }
                        if let Some(t) = choice.delta.tool_calls {
                            acc.push(&t);
                        }
                    }
                }
            }
            (text, acc.finish())
        };
        let (text, calls) = drain(payload.as_bytes());
        assert_eq!(text, "早");
        assert_eq!(calls[0].arguments["place"], "天台");
        for split in 0..payload.len() {
            let (a, b) = payload.as_bytes().split_at(split);
            let (t2, c2) = drain(&[a, b].concat());
            assert_eq!(t2, text, "切点 {split} 的正文");
            assert_eq!(format!("{c2:?}"), format!("{calls:?}"), "切点 {split} 的工具调用");
        }
    }

    /// A1 降级棘轮：带 tools 的请求被 400 拒 → 自动去 tools 重试一次，
    /// `tools_fallback` 置位；重试请求里不能再出现 "tools" 键
    #[test]
    fn chat_stream_auto_falls_back_when_tools_rejected_with_400() {
        rt().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let first = read_http_request(&mut sock).await;
                assert!(first.contains("\"tools\""), "首请求要带工具 schema：{first}");
                let _ = sock
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"tools not found\"}")
                    .await;
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let retry = read_http_request(&mut sock).await;
                assert!(!retry.contains("\"tools\""), "重试请求已去 tools：{retry}");
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"降级成功\"}}]}\n\ndata: [DONE]\n\n";
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let cancel = AtomicBool::new(false);
            let schemas = vec![serde_json::json!({"type": "function", "function": {"name": "ui_emit"}})];
            let mut seen = String::new();
            let out = chat_stream_auto(
                &provider,
                &[ChatMessage::text("user", "hi")],
                Some(&schemas),
                |d| seen.push_str(d),
                &cancel,
                None,
            )
            .await
            .unwrap();
            assert_eq!(out.text, "降级成功");
            assert_eq!(seen, "降级成功");
            assert!(out.tools_fallback, "降级标记要在结果里，供界面提示");
            assert!(out.tool_calls.is_empty());
        });
    }

    /// 工具开关关闭（tools=None）时与 chat_stream_ratcheted 完全同路：请求里没有 tools 键，
    /// 也没有降级标记——「不支持工具的接入点全链路与现状一致」的钉子
    #[test]
    fn chat_stream_auto_without_tools_matches_legacy_path() {
        rt().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let req = read_http_request(&mut sock).await;
                assert!(!req.contains("\"tools\""), "关开关的请求不带工具 schema：{req}");
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"照旧\"}}]}\n\ndata: [DONE]\n\n";
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let cancel = AtomicBool::new(false);
            let out = chat_stream_auto(
                &provider,
                &[ChatMessage::text("user", "hi")],
                None,
                |_| {},
                &cancel,
                None,
            )
            .await
            .unwrap();
            assert_eq!(out.text, "照旧");
            assert!(!out.tools_fallback);
        });
    }

    /// 非流式带工具：message.tool_calls 解析成参数对象（A1「非流式同步支持」的钉子）
    #[test]
    fn chat_complete_auto_parses_tool_calls() {
        rt().block_on(async {
            use tokio::io::AsyncWriteExt;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let _req = read_http_request(&mut sock).await;
                let body = r#"{"choices":[{"finish_reason":"tool_calls","message":{"content":"","tool_calls":[{"id":"c9","type":"function","function":{"name":"ui_emit","arguments":"{\"kind\":\"emotion\",\"value\":\"开心\"}"}}]}}]}"#;
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
            });
            let provider = test_provider(&format!("http://{addr}"));
            let schemas = vec![serde_json::json!({"type": "function"})];
            let out = chat_complete_auto(
                &provider,
                &[ChatMessage::text("user", "hi")],
                256,
                0.0,
                None,
                Some(&schemas),
            )
            .await
            .unwrap();
            assert_eq!(out.tool_calls.len(), 1);
            assert_eq!(out.tool_calls[0].name, "ui_emit");
            assert_eq!(out.tool_calls[0].arguments["value"], "开心");
            assert!(!out.tools_fallback);
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_v1_is_normalized_only_for_known_hosts() {
        let p = |base: &str| Provider {
            name: "t".into(),
            base_url: base.into(),
            api_key: "k".into(),
            model: "m".into(),
            temperature: 0.0,
            role: "chat".into(),
            tools: "off".into(),
        };
        // 漏写 /v1 的云服务地址：补上
        assert_eq!(
            endpoint(&p("https://api.deepseek.com")),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint(&p("https://api.deepseek.com/")),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint(&p("https://API.OpenAI.com")),
            "https://API.OpenAI.com/v1/chat/completions",
            "主机名大小写不敏感"
        );
        // 已经写了路径：原样拼接
        assert_eq!(
            endpoint(&p("https://api.deepseek.com/v1")),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // 自建网关：不猜，原样使用（Ollama 的 /v1 是必填的，用户自己写）
        assert_eq!(
            endpoint(&p("http://localhost:11434/v1")),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            endpoint(&p("http://192.168.1.9:8000")),
            "http://192.168.1.9:8000/chat/completions"
        );
    }

    #[test]
    fn scrub_credentials_hides_password() {
        assert_eq!(
            scrub_credentials("http://user:pass@127.0.0.1:7890"),
            "http://***@127.0.0.1:7890"
        );
        assert_eq!(scrub_credentials("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
    }

    #[test]
    fn error_chain_flattens_sources() {
        #[derive(Debug)]
        struct Layer(String, Option<Box<Layer>>);
        impl std::fmt::Display for Layer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|e| e as &(dyn std::error::Error + 'static))
            }
        }
        let err = Layer(
            "error sending request".into(),
            Some(Box::new(Layer(
                "invalid peer certificate".into(),
                Some(Box::new(Layer("unknown issuer".into(), None))),
            ))),
        );
        assert_eq!(
            error_chain(&err),
            "error sending request ← invalid peer certificate ← unknown issuer"
        );
    }

    /// 真实网络请求（默认跳过：`HUAJING_NET_TEST=1 cargo test -- --ignored`）。
    /// 存在的意义是钉住「HTTPS 传输层被真的编译进来了」——reqwest 一旦被关掉
    /// default-tls，任何 https 请求都会在发送阶段失败，而这个回归只能靠真实请求发现。
    /// 端到端：按真实链路（设置 → 环境变量 → 系统代理）建客户端并发一次真实请求。
    /// 这是「发不出去」类问题的回归网。
    #[test]
    #[ignore = "需要出网：HUAJING_NET_TEST=1 cargo test -- --ignored"]
    fn client_factory_end_to_end() {
        if std::env::var("HUAJING_NET_TEST").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (client, used) = build_client(None).await.expect("建客户端");
            println!("采用代理：{used:?}");
            match client.get("https://www.example.com/").send().await {
                Ok(r) => println!("状态：{}", r.status()),
                Err(e) => panic!("请求失败：{}", error_chain(&e)),
            }
        });
    }

    /// 系统代理探测（Windows 注册表 + 健康检查）。没有开系统代理的环境下自动跳过。
    #[test]
    #[ignore = "依赖本机系统代理：HUAJING_NET_TEST=1 cargo test -- --ignored"]
    fn system_proxy_detection_smoke() {
        if std::env::var("HUAJING_NET_TEST").is_err() {
            return;
        }
        match proxy_from_system() {
            Some((url, source)) => {
                println!("系统代理：{url}（{source}）");
                assert!(url.starts_with("http://") || url.starts_with("socks"), "{url}");
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                println!("在监听：{}", rt.block_on(proxy_reachable(&url)));
            }
            None => println!("本机未启用系统代理（跳过）"),
        }
    }

    #[test]
    #[ignore = "需要出网：HUAJING_NET_TEST=1 cargo test -- --ignored"]
    fn https_transport_is_wired() {
        if std::env::var("HUAJING_NET_TEST").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("建 tokio 运行时");
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("建客户端");
            // 只要能拿到状态码就说明 TCP + TLS + 证书校验都通了（不需要打到某个 API）
            match client.get("https://www.example.com/").send().await {
                Ok(r) => assert!(r.status().is_success() || r.status().is_redirection()),
                Err(e) => panic!("HTTPS 传输层不可用：{}", error_chain(&e)),
            }
        });
    }

    #[test]
    fn sse_single_chunk_multiple_events() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\n").unwrap();
        assert_eq!(events, vec![r#"{"a":1}"#, r#"{"a":2}"#]);
    }

    #[test]
    fn sse_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"a\"").unwrap().is_empty());
        assert_eq!(p.feed(b":1}\n").unwrap(), vec![r#"{"a":1}"#]);

        let mut q = SseParser::new();
        assert!(q.feed(b"data: [DO").unwrap().is_empty());
        assert_eq!(q.feed(b"NE]\n").unwrap(), vec!["[DONE]"]);
    }

    #[test]
    fn sse_crlf_and_no_space_after_colon() {
        let mut p = SseParser::new();
        let events = p.feed(b"data:{\"x\":1}\r\ndata: {\"x\":2}\r\n").unwrap();
        assert_eq!(events, vec![r#"{"x":1}"#, r#"{"x":2}"#]);
    }

    #[test]
    fn sse_ignores_non_data_lines() {
        let mut p = SseParser::new();
        let events = p.feed(b": keepalive\nevent: message\nid: 7\nretry: 1000\ndata: hi\n\n").unwrap();
        assert_eq!(events, vec!["hi"]);
    }

    #[test]
    fn sse_multibyte_utf8_split_across_chunks() {
        let mut p = SseParser::new();
        let full = "data: 你好，世界\n\n".as_bytes().to_vec();
        let (a, b) = full.split_at(9); // 切在多字节字符中间
        assert!(p.feed(a).unwrap().is_empty());
        assert_eq!(p.feed(b).unwrap(), vec!["你好，世界"]);
    }

    #[test]
    fn sse_empty_and_blank_data_ignored() {
        // data: 后仅去一个可选空格；空负载忽略（不 trim，负载可能以空格结尾）
        let mut p = SseParser::new();
        let events = p.feed(b"data:\ndata: \ndata: ok\n").unwrap();
        assert_eq!(events, vec!["ok"]);
    }

    #[test]
    fn stream_chunk_parses_delta() {
        let chunk: StreamChunk = serde_json::from_str(
            r#"{"choices":[{"delta":{"content":"你好"}}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("你好"));

        let no_delta: StreamChunk =
            serde_json::from_str(r#"{"choices":[{"delta":{}}]}"#).unwrap();
        assert!(no_delta.choices[0].delta.content.is_none());
    }

    #[test]
    fn embeddings_response_sorts_by_index() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"data":[{"index":1,"embedding":[3.0,4.0]},{"index":0,"embedding":[1.0,2.0]}]}"#,
        )
        .unwrap();
        let rows = parse_embeddings_response(&value).unwrap();
        assert_eq!(rows, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn embeddings_response_rejects_dirty_payloads() {
        let no_data: serde_json::Value = serde_json::from_str(r#"{"error":"boom"}"#).unwrap();
        assert!(parse_embeddings_response(&no_data).is_err());

        let missing: serde_json::Value =
            serde_json::from_str(r#"{"data":[{"index":0}]}"#).unwrap();
        assert!(parse_embeddings_response(&missing).is_err(), "缺 embedding 报错");

        let empty_vec: serde_json::Value =
            serde_json::from_str(r#"{"data":[{"index":0,"embedding":[]}]}"#).unwrap();
        assert!(parse_embeddings_response(&empty_vec).is_err(), "空向量报错");

        let gapped: serde_json::Value = serde_json::from_str(
            r#"{"data":[{"index":0,"embedding":[1.0]},{"index":2,"embedding":[2.0]}]}"#,
        )
        .unwrap();
        assert!(
            parse_embeddings_response(&gapped).is_err(),
            "索引断档 = 对应关系不可信"
        );
    }

    #[test]
    fn embeddings_endpoint_uses_the_same_url_rules() {
        let p = Provider {
            name: "t".into(),
            base_url: "https://api.deepseek.com".into(),
            api_key: "k".into(),
            model: "m".into(),
            temperature: 0.0,
            role: "embed".into(),
            tools: "off".into(),
        };
        assert_eq!(
            embeddings_endpoint(&p),
            "https://api.deepseek.com/v1/embeddings"
        );
    }
}
