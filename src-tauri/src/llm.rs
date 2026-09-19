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

/// 对话消息（OpenAI 格式：system | user | assistant）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
    Delta { text: String },
    Done {
        full: String,
        cancelled: bool,
        /// 本轮 on_message 钩子的报告（无 on_message 卡为 None）
        report: Option<HookReport>,
    },
    Error { message: String },
    /// 卡片经 `api.ui.emit` 推来的界面事件（紧跟产生它的那一步发生）
    HookEvent { kind: String, value: String },
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
    extra_proxy: Option<&str>,
) -> Result<StreamOutcome, String> {
    let (client, _proxy) = build_client(extra_proxy).await?;
    let url = endpoint(provider);
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
        .map_err(|e| format!("请求失败（{}）：{}", provider.name, error_chain(&e)))?;
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
    value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "响应里没有 choices[0].message.content：{}",
                truncate(&value.to_string(), 300)
            )
        })
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
    match proxy_from_system() {
        Some((url, source)) if proxy_reachable(&url).await => Some((url, source)),
        _ => None,
    }
}

/// 建一个 HTTP 客户端（连同一个会话的多次请求共用一份；Tauri State 持有）
pub async fn build_client(extra_proxy: Option<&str>) -> Result<(reqwest::Client, Option<String>), String> {
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
