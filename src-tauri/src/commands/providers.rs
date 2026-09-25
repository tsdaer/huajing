//! Tauri 命令 · 接入点与全局设置（设计 §11 / §12）：设置页的数据通道。

use super::*;
// ---------- providers（设计 §11）----------

#[tauri::command]
pub fn list_providers() -> Result<Vec<Provider>, String> {
    store::load_providers(&root()).map_err(|e| e.to_string())
}

/// 按名称 upsert，返回更新后的全量列表
#[tauri::command]
pub fn save_provider(provider: Provider) -> Result<Vec<Provider>, String> {
    store::upsert_provider(&root(), provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_provider(name: String) -> Result<Vec<Provider>, String> {
    store::delete_provider(&root(), &name).map_err(|e| e.to_string())
}

/// 一次连通性自检的结果（设置页「测试」按钮）
#[derive(Debug, Clone, Serialize)]
pub struct ProviderTest {
    pub ok: bool,
    /// 人类可读的结论或错误（错误含 reqwest 的完整原因链）
    pub message: String,
    /// 实际请求的 URL（base_url 少写 /v1 之类一眼可见）
    pub url: String,
    pub model: String,
    /// 服务端原样返回的响应体摘要（成功时用于确认模型确实回了话）
    pub detail: String,
    pub elapsed_ms: u64,
    /// 这台机器上配了的代理（出网失败时的第一条线索）
    pub proxy: Vec<String>,
    /// 本次实际采用的代理（含来源）；None = 直连
    pub proxy_used: Option<String>,
}

/// 测试一个接入点是否真的能用：发一条最小请求（非流式，60s 上限）。
/// 覆盖三类常见故障：key 无效 / 地址写错（404 或连不上）/ 出网被拦（TLS 与代理）。
#[tauri::command]
pub async fn test_provider(provider: Provider) -> Result<ProviderTest, String> {
    // 与真实发送走同一份地址与代理规则，自检结果才对得上真实请求
    let url = llm::endpoint(&provider);
    let proxy = proxy_of(&root());
    let proxy_used = llm::build_client(proxy.as_deref())
        .await
        .map(|(_, used)| used)
        .unwrap_or(None);
    let messages = vec![llm::ChatMessage {
        role: "user".into(),
        content: "说「好」一个字即可。".into(),
    }];
    let started = std::time::Instant::now();
    let outcome = llm::chat_once(&provider, &messages, proxy.as_deref()).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(ProviderTest {
        ok: outcome.is_ok(),
        detail: outcome.clone().unwrap_or_default(),
        message: match &outcome {
            Ok(_) => format!("连接成功（{} ms）", elapsed_ms),
            Err(e) => e.clone(),
        },
        url,
        model: provider.model.clone(),
        elapsed_ms,
        proxy: llm::proxy_env(),
        proxy_used,
    })
}

// ---------- settings ----------

#[tauri::command]
pub fn get_settings() -> Result<Settings, String> {
    store::load_settings(&root()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(settings: Settings) -> Result<Settings, String> {
    store::save_settings(&root(), &settings).map_err(|e| e.to_string())?;
    Ok(settings)
}

