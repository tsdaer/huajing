//! Tauri 命令 · 签名自动更新（M4.2 · 设计 §13 · 决断 1：更新无服务器）。
//!
//! 客户端能力全在 Rust 侧（tauri-plugin-updater 内置 minisign 签名校验）：
//! endpoint 运行时从 settings.toml 的 `[updater]` 读（决断 1 的「端点可配缺省关」），
//! 清单托管只写在发布流程文档里（docs/release.md），客户端不做任何动态协商。
//! 前端只拿三样东西：未启用态（不报错、按钮置灰）、检查结论、下载进度与安装通知。

use super::*;
use tauri::Url;
use tauri_plugin_updater::{Error as UpdaterError, UpdaterExt};

/// 更新检查的结论（设置页「关于与更新」的展示数据源）。
/// state 四态：disabled（未启用，UI 不报错）/ up_to_date / available / error。
#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub state: String,
    pub current_version: String,
    /// 可用的新版本号（available 时有值）
    pub version: Option<String>,
    /// 发布说明（清单 notes 原文）
    pub notes: Option<String>,
    /// 人类可读结论：成功给版本关系，失败给分类原因
    pub message: String,
}

/// 下载进度事件（Channel 推送，与流式对话同一机制）。
/// Windows 上 `Installing` 之后进程被安装器接管退出，本命令不再返回。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum UpdateProgress {
    Started { total: Option<u64> },
    Progress { downloaded: u64, total: Option<u64> },
    /// 下载完成且签名校验通过（篡改包在这里被拒绝，不会走到安装）
    Downloaded { bytes: u64 },
    /// 安装器已启动，应用即将退出
    Installing,
}

/// 更新器启用判据（决断 1 的唯一权威）：enabled 且 endpoint 非空才算配置了。
/// Err = 人类可读的未启用原因，设置页原样展示且不报错。
pub fn updater_endpoint_of(settings: &Settings) -> Result<String, String> {
    if !settings.updater.enabled {
        return Err("自动更新未启用".into());
    }
    let endpoint = settings.updater.endpoint.as_deref().unwrap_or("").trim();
    if endpoint.is_empty() {
        return Err("自动更新已启用，但未配置更新清单地址".into());
    }
    Ok(endpoint.to_string())
}

/// 更新错误的人类可读分类（决断 1「错误逐类可读」）：网络类给排查线索，
/// 签名类明确「已拒绝安装」，清单/配置类指回端点或发布侧。未知类型原样透出。
pub fn classify_update_error(raw: &str) -> &'static str {
    // 签名族（minisign_verify 文案 + 签名解码/版本盖章），篡改包必落在这一族
    const SIGNATURE_MARKS: &[&str] = &[
        "The signature verification failed",
        "Invalid encoding in minisign data",
        "Unexpected signature algorithm",
        "Unexpected key id",
        "could not be decoded, please check if it is a valid base64 string",
        "signed for version",
    ];
    // 网络族（reqwest/下载中断/服务端非成功状态码）
    const NETWORK_MARKS: &[&str] = &[
        "error sending request",
        "Download request failed with status",
        "operation was canceled",
        "TimedOut",
        "connection closed before message completed",
        "invalid peer certificate",
        "dns error",
    ];
    // 清单/配置族（JSON 结构、semver、URL、平台条目、端点协议）
    const MANIFEST_MARKS: &[&str] = &[
        "missing field",
        "invalid type",
        "expected value",
        "EOF while parsing",
        "relative URL without a base",
        "empty host",
        "was not found in the response `platforms` object",
        "were not found in the response `platforms` object",
        "None of the fallback platforms",
        "does not have any endpoints set",
        "must use a secure protocol",
        "Unsupported application architecture",
    ];
    if SIGNATURE_MARKS.iter().any(|m| raw.contains(m)) {
        return "签名校验失败，已拒绝安装";
    }
    if NETWORK_MARKS.iter().any(|m| raw.contains(m)) {
        return "网络错误";
    }
    if MANIFEST_MARKS.iter().any(|m| raw.contains(m)) {
        return "更新清单或端点配置有误";
    }
    "更新失败"
}

/// 组装人类可读的错误：分类前缀 + 插件完整原因链（排查时原文不丢）。
pub fn readable_update_error(e: &UpdaterError) -> String {
    let raw = e.to_string();
    match classify_update_error(&raw) {
        "签名校验失败，已拒绝安装" => format!("签名校验失败，已拒绝安装（{raw}）"),
        "网络错误" => format!("网络错误：{raw}（检查网络或出网代理后重试）"),
        "更新清单或端点配置有误" => format!("更新清单或端点配置有误：{raw}"),
        _ => format!("更新失败：{raw}"),
    }
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn load_updater_settings() -> Result<Settings, String> {
    store::load_settings(&root()).map_err(|e| e.to_string())
}

/// 从 settings 组一个带动态 endpoint 的 Updater（检查与下载共用，口径一致）。
/// 顺带把 settings.toml 的出网代理带上——更新通道与对话通道走同一套出网规则。
fn build_updater(
    app: &AppHandle,
    settings: &Settings,
) -> Result<tauri_plugin_updater::Updater, String> {
    let endpoint = updater_endpoint_of(settings)?;
    let url: Url = endpoint
        .parse()
        .map_err(|e| format!("更新清单或端点配置有误：{endpoint}（{e}）"))?;
    let mut builder = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| readable_update_error(&e))?;
    // 手填代理非空才接管；空串/未填 = 交给 reqwest 的默认行为（环境/系统代理→直连），
    // 与对话通道 llm::build_client 的自动档同口径
    if let Some(proxy) = settings
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let proxy_url: Url = proxy
            .parse()
            .map_err(|e| format!("出网代理地址无效：{proxy}（{e}）"))?;
        builder = builder.proxy(proxy_url);
    }
    builder.build().map_err(|e| readable_update_error(&e))
}

fn status(state: &str, message: impl Into<String>) -> UpdateStatus {
    UpdateStatus {
        state: state.into(),
        current_version: current_version(),
        version: None,
        notes: None,
        message: message.into(),
    }
}

/// 检查更新（不下载）。未启用 = `disabled` 结论（不是错误）；
/// 服务端可达但已是最新 = `up_to_date`；有新版 = `available` 带版本与说明；
/// 网络/签名/清单失败 = `error` 带分类原因（diag 留档）。
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateStatus, String> {
    let settings = load_updater_settings()?;
    let outcome = run_check(&app, &settings).await;
    if outcome.state == "error" {
        crate::diag::record("error", format!("检查更新失败：{}", outcome.message));
    } else {
        crate::diag::record("update", format!("检查更新：{}", outcome.message));
    }
    Ok(outcome)
}

async fn run_check(app: &AppHandle, settings: &Settings) -> UpdateStatus {
    if let Err(reason) = updater_endpoint_of(settings) {
        return status("disabled", reason);
    }
    let updater = match build_updater(app, settings) {
        Ok(u) => u,
        Err(message) => return status("error", message),
    };
    match updater.check().await {
        Ok(None) => status(
            "up_to_date",
            format!("已是最新版本（v{}）", current_version()),
        ),
        Ok(Some(update)) => UpdateStatus {
            state: "available".into(),
            current_version: update.current_version.clone(),
            version: Some(update.version.clone()),
            notes: update.body.clone(),
            message: format!(
                "发现新版本 v{}（当前 v{}）",
                update.version, update.current_version
            ),
        },
        Err(e) => status("error", readable_update_error(&e)),
    }
}

/// 下载并安装：下载（签名校验内建于下载完成后）→ 启动安装器 → Windows 上退出进程，
/// 安装器（NSIS `/P /R`，passive + 装完自动重启）接管后续。进度经 Channel 推送。
/// 清单在检查与安装之间可能变化：这里重新 check 一次拿最新的 Update 对象。
#[tauri::command]
pub async fn download_and_install(
    app: AppHandle,
    on_event: Channel<UpdateProgress>,
) -> Result<(), String> {
    let settings = load_updater_settings()?;
    let updater = build_updater(&app, &settings)?;
    let Some(update) = updater.check().await.map_err(|e| readable_update_error(&e))? else {
        return Err("已没有可安装的更新".into());
    };

    let fetched = std::sync::atomic::AtomicU64::new(0);
    let fetched = std::sync::Arc::new(fetched);
    let channel = on_event.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                let done = fetched.fetch_add(chunk as u64, Ordering::Relaxed) + chunk as u64;
                let _ = channel.send(UpdateProgress::Progress {
                    downloaded: done,
                    total,
                });
            },
            || {},
        )
        .await
        .map_err(|e| {
            let message = readable_update_error(&e);
            crate::diag::record("error", format!("更新包下载失败：{message}"));
            message
        })?;

    let _ = on_event.send(UpdateProgress::Downloaded {
        bytes: bytes.len() as u64,
    });
    crate::diag::record(
        "update",
        format!(
            "更新包 v{} 下载完成（{} 字节）且签名校验通过，启动安装器",
            update.version,
            bytes.len()
        ),
    );
    let _ = on_event.send(UpdateProgress::Installing);
    // Windows：启动安装器（passive 模式 + 装完重启）后 std::process::exit(0)——
    // 本命令从这一刻起不再返回；macOS/Linux 需自行重启（v1 不做移动端更新）
    update
        .install(bytes)
        .map_err(|e| readable_update_error(&e))?;
    #[cfg(not(windows))]
    {
        use tauri::Manager;
        let _ = app.restart();
    }
    #[cfg(windows)]
    Ok(())
}

/// 更新器现状（设置页「关于与更新」的静态部分 + 开关可用性判定）
#[tauri::command]
pub fn updater_info() -> UpdateStatus {
    let settings = load_updater_settings().unwrap_or_default();
    match updater_endpoint_of(&settings) {
        Ok(endpoint) => status("configured", endpoint),
        Err(reason) => status("disabled", reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(enabled: bool, endpoint: Option<&str>) -> Settings {
        Settings {
            updater: crate::store::UpdaterConfig {
                enabled,
                endpoint: endpoint.map(str::to_string),
            },
            ..Settings::default()
        }
    }

    #[test]
    fn endpoint_gate_requires_enabled_and_non_blank_endpoint() {
        assert_eq!(updater_endpoint_of(&settings_with(false, None)), Err("自动更新未启用".into()));
        // 开了但没填地址：给出可读原因而不是把空串当端点用
        assert_eq!(
            updater_endpoint_of(&settings_with(true, None)),
            Err("自动更新已启用，但未配置更新清单地址".into())
        );
        assert_eq!(
            updater_endpoint_of(&settings_with(true, Some("   "))),
            Err("自动更新已启用，但未配置更新清单地址".into())
        );
        assert_eq!(
            updater_endpoint_of(&settings_with(true, Some("  https://releases.example.com/latest.json "))),
            Ok("https://releases.example.com/latest.json".into()),
            "端点两侧空白被修剪"
        );
    }

    /// 分类用例的文案与插件 2.12 错误链逐字对齐（error.rs / minisign-verify 0.2.5）。
    /// 篡改包的实锤落在签名族——它必须在下载完成时被拒绝，不能进安装器。
    #[test]
    fn classify_covers_signature_network_and_manifest_families() {
        let signature_cases = [
            "The signature verification failed".to_string(),
            "The signature xxx could not be decoded, please check if it is a valid base64 string. The signature must be the contents of the `.sig` file generated by the Tauri bundler, as a string.".to_string(),
            "The update was signed for version 0.3.1 but the update endpoint announced version 0.3.2. The endpoint response may have been tampered with to force installing a different release.".to_string(),
            "Invalid encoding in minisign data".to_string(),
        ];
        for case in &signature_cases {
            assert_eq!(classify_update_error(case), "签名校验失败，已拒绝安装", "{case}");
        }

        let network_cases = [
            "error sending request for url (http://127.0.0.1:8080/latest.json)".to_string(),
            "Download request failed with status: 404 Not Found".to_string(),
            "invalid peer certificate contents".to_string(),
        ];
        for case in &network_cases {
            assert_eq!(classify_update_error(case), "网络错误", "{case}");
        }

        let manifest_cases = [
            "missing field `version` at line 1 column 40".to_string(),
            "the platform `windows-x86_64` was not found in the response `platforms` object".to_string(),
            "None of the fallback platforms `[\"windows-x86_64-nsis\", \"windows-x86_64\"]` were found in the response `platforms` object".to_string(),
            "Updater does not have any endpoints set.".to_string(),
            "The configured updater endpoint must use a secure protocol like `https`.".to_string(),
            "relative URL without a base".to_string(),
        ];
        for case in &manifest_cases {
            assert_eq!(classify_update_error(case), "更新清单或端点配置有误", "{case}");
        }

        // 未认识的错误：兜底前缀，不吞原因链
        assert_eq!(classify_update_error("某种全新错误"), "更新失败");
        assert_eq!(
            readable_update_error(&UpdaterError::Network("Download request failed with status: 404 Not Found".into())),
            "网络错误：`Download request failed with status: 404 Not Found`（检查网络或出网代理后重试）"
        );
    }

    /// settings.toml 的 `[updater]` 区：缺文件/缺区都落缺省（未启用），写了就照读。
    #[test]
    fn updater_section_roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        // 老用户的 settings.toml 没有 [updater] 区：读进来 = 未启用，不报错
        std::fs::write(dir.path().join("settings.toml"), "locale = \"zh-CN\"\n").unwrap();
        let loaded = store::load_settings(dir.path()).unwrap();
        assert!(!loaded.updater.enabled);
        assert_eq!(loaded.updater.endpoint, None);

        // 写入后往返一致
        store::save_settings(
            dir.path(),
            &settings_with(true, Some("https://releases.example.com/latest.json")),
        )
        .unwrap();
        let raw = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(raw.contains("[updater]"), "嵌套结构落成 TOML 区：{raw}");
        let reread = store::load_settings(dir.path()).unwrap();
        assert_eq!(reread.updater, settings_with(true, Some("https://releases.example.com/latest.json")).updater);
    }
}
