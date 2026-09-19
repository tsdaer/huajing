//! LLM 接入（设计 §11）：OpenAI 兼容协议 + SSE 流式。
//!
//! 流解析抽象为独立的 [`SseParser`]（各家 SSE 实现差异集中于此，设计
//! 风险表：至少两家实测）。补全入口 [`chat_stream`] 逐段回调增量文本，
//! 支持取消与逐块超时。

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
}

fn default_temperature() -> f32 {
    0.8
}

fn default_role() -> String {
    "chat".into()
}

/// 对话消息（OpenAI 格式：system | user | assistant）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 发送给前端的流事件
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Delta { text: String },
    Done { full: String, cancelled: bool },
    Error { message: String },
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
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            let line = line.trim_end_matches('\r');
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                if !data.is_empty() {
                    events.push(data.to_string());
                }
            }
        }
        events
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
}

#[derive(Debug, Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

/// 流式补全结果
pub struct StreamOutcome {
    pub text: String,
    /// 被用户中断（保留已生成的部分文本）
    pub cancelled: bool,
}

/// 每块之间的最长等待（无整体超时：长生成不误杀）
const CHUNK_TIMEOUT: Duration = Duration::from_secs(180);

/// OpenAI 兼容 `/chat/completions` 流式补全。
/// `on_delta` 逐段回调增量文本；`cancel` 置 true 后尽快返回（保留部分文本）。
pub async fn chat_stream(
    provider: &Provider,
    messages: &[ChatMessage],
    mut on_delta: impl FnMut(&str),
    cancel: &AtomicBool,
) -> Result<StreamOutcome, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败：{e}"))?;
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": provider.temperature,
        "stream": true,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败（{}）：{e}", provider.name))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("{} 返回 {}：{}", provider.name, status, truncate(&detail, 500)));
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut full = String::new();
    let mut cancelled = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let chunk = tokio::time::timeout(CHUNK_TIMEOUT, stream.next())
            .await
            .map_err(|_| format!("{} 响应超时（{} 秒无数据）", provider.name, CHUNK_TIMEOUT.as_secs()))?
            .transpose()
            .map_err(|e| format!("{} 流读取失败：{e}", provider.name))?;
        let Some(bytes) = chunk else { break };

        for data in parser.feed(&bytes) {
            if data == "[DONE]" {
                return Ok(StreamOutcome { text: full, cancelled: false });
            }
            let Ok(parsed) = serde_json::from_str::<StreamChunk>(&data) else {
                continue; // 非 JSON 负载（心跳等）跳过
            };
            if let Some(delta) = parsed.choices.into_iter().find_map(|c| c.delta.content) {
                if !delta.is_empty() {
                    on_delta(&delta);
                    full.push_str(&delta);
                }
            }
        }
    }
    // 流结束但未见 [DONE]：按已收内容收尾
    Ok(StreamOutcome { text: full, cancelled })
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
mod tests {
    use super::*;

    #[test]
    fn sse_single_chunk_multiple_events() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\n");
        assert_eq!(events, vec![r#"{"a":1}"#, r#"{"a":2}"#]);
    }

    #[test]
    fn sse_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"a\"").is_empty());
        assert_eq!(p.feed(b":1}\n"), vec![r#"{"a":1}"#]);

        let mut q = SseParser::new();
        assert!(q.feed(b"data: [DO").is_empty());
        assert_eq!(q.feed(b"NE]\n"), vec!["[DONE]"]);
    }

    #[test]
    fn sse_crlf_and_no_space_after_colon() {
        let mut p = SseParser::new();
        let events = p.feed(b"data:{\"x\":1}\r\ndata: {\"x\":2}\r\n");
        assert_eq!(events, vec![r#"{"x":1}"#, r#"{"x":2}"#]);
    }

    #[test]
    fn sse_ignores_non_data_lines() {
        let mut p = SseParser::new();
        let events = p.feed(b": keepalive\nevent: message\nid: 7\nretry: 1000\ndata: hi\n\n");
        assert_eq!(events, vec!["hi"]);
    }

    #[test]
    fn sse_multibyte_utf8_split_across_chunks() {
        let mut p = SseParser::new();
        let full = "data: 你好，世界\n\n".as_bytes().to_vec();
        let (a, b) = full.split_at(9); // 切在多字节字符中间
        assert!(p.feed(a).is_empty());
        assert_eq!(p.feed(b), vec!["你好，世界"]);
    }

    #[test]
    fn sse_empty_and_blank_data_ignored() {
        // data: 后仅去一个可选空格；空负载忽略（不 trim，负载可能以空格结尾）
        let mut p = SseParser::new();
        let events = p.feed(b"data:\ndata: \ndata: ok\n");
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
}
