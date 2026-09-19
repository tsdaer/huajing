//! LLM 接入（设计 §11）：OpenAI 兼容协议 + SSE 流式。

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

// TODO(M1): llm.rs — reqwest + eventsource 流式；按 role 选 provider；
//   增量事件转发前端（打字机效果）；中断与重试。
