//! DataHub 明文数据层（设计 §12）：一切用户数据为本地明文文件。
//!
//! M1.1 范围：providers / settings / personas 读写；会话目录骨架与
//! messages.jsonl 追加式落盘（可回放）。卡片加载与热加载见 card.rs（M1.2）。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::llm::Provider;

// ---------- 错误 ----------

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TomlDe(toml::de::Error),
    TomlSer(toml::ser::Error),
    NotFound(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "IO 错误：{}", e),
            StoreError::Json(e) => write!(f, "JSON 解析错误：{}", e),
            StoreError::TomlDe(e) => write!(f, "TOML 解析错误：{}", e),
            StoreError::TomlSer(e) => write!(f, "TOML 写入错误：{}", e),
            StoreError::NotFound(what) => write!(f, "不存在：{}", what),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

impl From<toml::de::Error> for StoreError {
    fn from(e: toml::de::Error) -> Self {
        StoreError::TomlDe(e)
    }
}

impl From<toml::ser::Error> for StoreError {
    fn from(e: toml::ser::Error) -> Self {
        StoreError::TomlSer(e)
    }
}

/// 原子落盘（加固 A2）：写同目录临时文件 → fsync → rename 原子替换。
/// 中途崩溃/断电最坏留一个 `.tmp` 残骸，目标文件要么是完整的旧内容要么是
/// 完整的新内容，绝不出现半截文件（messages.jsonl 是唯一事实来源，半截不可重建）。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

// ---------- 目录 ----------

/// DataHub 目录解析（设计 §12「程序与数据分离」），按优先级：
///
/// 1. `HUAJING_DATA` 环境变量（便携/多套数据用）；
/// 2. 可执行文件旁的 `DataHub`（便携版、绿色解压即用）；
/// 3. 从 cwd 逐级向上找已有的 `DataHub`（开发期：`tauri dev` 的 cwd 是 src-tauri）；
/// 4. 用户数据目录下的 `DataHub`（安装版首启：绝不在 Program Files 里写数据，
///    也不能依赖启动时的 cwd——从开始菜单启动时 cwd 可能是 system32）。
pub fn data_root() -> PathBuf {
    if let Ok(p) = std::env::var("HUAJING_DATA") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("DataHub")))
        .filter(|p| p.is_dir())
    {
        return p;
    }
    if let Some(p) = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.ancestors().map(|d| d.join("DataHub")).find(|p| p.is_dir()))
    {
        return p;
    }
    user_data_root()
}

/// 兜底数据目录：`%APPDATA%\huajing\DataHub`（其他平台退回 `~/.huajing/DataHub`）
fn user_data_root() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("huajing").join("DataHub")
}

/// 确保数据目录骨架存在（设计 §12 目录树；scripts/ 是 M4.1 的剧本模板目录）
pub fn ensure_layout(root: &Path) -> std::io::Result<()> {
    for d in ["personas", "characters", "codex", "sessions", "scripts"] {
        std::fs::create_dir_all(root.join(d))?;
    }
    Ok(())
}

// ---------- providers.toml（设计 §11：LLM 接入点；手改配置用 TOML，带注释友好）----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<Provider>,
}

pub fn providers_path(root: &Path) -> PathBuf {
    root.join("providers.toml")
}

pub fn load_providers(root: &Path) -> StoreResult<Vec<Provider>> {
    let path = providers_path(root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str::<ProvidersFile>(&raw)?.providers)
}

pub fn save_providers(root: &Path, providers: &[Provider]) -> StoreResult<()> {
    std::fs::create_dir_all(root)?;
    let toml = toml::to_string_pretty(&ProvidersFile {
        providers: providers.to_vec(),
    })?;
    atomic_write(&providers_path(root), (toml + "\n").as_bytes())?;
    Ok(())
}

/// 按名称 upsert 一条接入点，返回更新后的全量列表
pub fn upsert_provider(root: &Path, provider: Provider) -> StoreResult<Vec<Provider>> {
    let mut providers = load_providers(root)?;
    match providers.iter_mut().find(|p| p.name == provider.name) {
        Some(slot) => *slot = provider,
        None => providers.push(provider),
    }
    save_providers(root, &providers)?;
    Ok(providers)
}

pub fn delete_provider(root: &Path, name: &str) -> StoreResult<Vec<Provider>> {
    let mut providers = load_providers(root)?;
    let before = providers.len();
    providers.retain(|p| p.name != name);
    if providers.len() == before {
        return Err(StoreError::NotFound(format!("provider「{}」", name)));
    }
    save_providers(root, &providers)?;
    Ok(providers)
}

// ---------- settings.toml（界面与全局配置）----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_narrative_mode")]
    pub narrative_mode: String,
    /// 首启向导是否已走完（M1.9：填过 key 或显式跳过）
    #[serde(default)]
    pub wizard_done: bool,
    /// 出网代理（可空）。空则自动：环境变量 > Windows 系统代理（需在监听）> 直连。
    /// 国内直连不上 API、系统代理又读不到时，在这里手填 `http://127.0.0.1:7890` 即可。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// 模型上下文窗口（token；缺省按 32768 计）。设计 §4.2：输入预算 = 上下文 × 75%
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// 运行期自动接受既有实体的小事实（M3.8 · 设计 §6.8-2 分级捕获的中档）。
    /// false = 进收件箱人工确认（缺省）；瞬时状态写黑板与全新实体必人工不受它影响。
    #[serde(default)]
    pub auto_accept_minor_facts: bool,
    /// 每轮工具调用上限（增强 A5，缺省 6）：超出的调用全部弃置并记一条汇总诊断
    #[serde(default)]
    pub tool_calls_per_turn: Option<usize>,
    /// 阶段转移旁白（增强 E2）：状态树转移时可选生成一段氛围旁白（便宜档非流式，
    /// 模板兜底）。false = 不生成（缺省）
    #[serde(default)]
    pub stage_narration: bool,
    /// 签名自动更新（M4.2 · 设计 §13 · 决断 1）：`[updater]` 区，端点可配缺省关
    #[serde(default)]
    pub updater: UpdaterConfig,
    /// 宫殿睡眠整理（M4.4 · 设计 §5.4 · 决断 4/5）：`[consolidate]` 区，自动缺省关
    #[serde(default)]
    pub consolidate: ConsolidateConfig,
}

/// `[updater]` 区（M4.2 · 决断 1）：静态清单（latest.json）的完整 URL + 开关。
/// enabled=false 或 endpoint 空 = 自动更新未启用——UI 只提示不报错，检查入口置灰。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterConfig {
    /// 更新清单（latest.json）的完整 URL。发布形态见 docs/release.md：
    /// GitHub Releases 或任意静态托管皆可，客户端不做任何动态协商
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 自动更新开关，缺省 false。v1 自用没有分发基础设施：客户端能力先行，
    /// 端点由用户显式启用（哪怕是官方清单也尊重这个开关）
    #[serde(default)]
    pub enabled: bool,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        UpdaterConfig {
            endpoint: None,
            enabled: false,
        }
    }
}

/// `[consolidate]` 区（M4.4 · 决断 4/5）：睡眠整理的阈值与空闲自动开关。
/// 整理本体永远是**手动按钮优先**（确认框说明「归档不复活」），自动只是省心档。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidateConfig {
    /// 候选阈值：salience 按故事时钟衰减后仍低于它的 episode/hearsay 才进候选。缺省 0.25。
    #[serde(default = "default_consolidate_threshold")]
    pub threshold: f64,
    /// 空闲自动整理，缺省关（整理产物是模型产物，质量不佳用户不开即可——风险 4 的边界）。
    #[serde(default)]
    pub auto: bool,
    /// 自动触发的空闲判据：会话轮末距上一条消息的真实间隔超过该分钟数才动手。缺省 30。
    #[serde(default = "default_consolidate_idle_minutes")]
    pub idle_minutes: u64,
}

fn default_consolidate_threshold() -> f64 {
    0.25
}
fn default_consolidate_idle_minutes() -> u64 {
    30
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        ConsolidateConfig {
            threshold: default_consolidate_threshold(),
            auto: false,
            idle_minutes: default_consolidate_idle_minutes(),
        }
    }
}

impl Settings {
    /// 输入预算（设计 §4.2）：模型上下文窗口 × 75%；输出预留另计，不在这里扣。
    /// `context_window` 缺省按 32768 计。
    /// 加固 A7：手填的超大窗口先钳到 1..=10_000_000——usize 乘 75 在 debug 构建会溢出 panic。
    pub fn input_budget(&self) -> usize {
        self.context_window
            .unwrap_or(32768)
            .clamp(1, 10_000_000)
            .saturating_mul(75)
            / 100
    }
}

fn default_locale() -> String {
    "zh-CN".into()
}
fn default_theme() -> String {
    "im".into()
}
fn default_narrative_mode() -> String {
    "台词体".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            locale: default_locale(),
            theme: default_theme(),
            narrative_mode: default_narrative_mode(),
            wizard_done: false,
            proxy: None,
            context_window: None,
            auto_accept_minor_facts: false,
            tool_calls_per_turn: None,
            stage_narration: false,
            updater: UpdaterConfig::default(),
            consolidate: ConsolidateConfig::default(),
        }
    }
}

pub fn load_settings(root: &Path) -> StoreResult<Settings> {
    let path = root.join("settings.toml");
    if !path.exists() {
        return Ok(Settings::default());
    }
    Ok(toml::from_str(&std::fs::read_to_string(&path)?)?)
}

pub fn save_settings(root: &Path, settings: &Settings) -> StoreResult<()> {
    std::fs::create_dir_all(root)?;
    let toml = toml::to_string_pretty(settings)?;
    std::fs::write(root.join("settings.toml"), toml + "\n")?;
    Ok(())
}

// ---------- personas/（用户人格，*.toml）----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

pub fn list_personas(root: &Path) -> StoreResult<Vec<Persona>> {
    let dir = root.join("personas");
    let mut out = Vec::new();
    // 坏文件跳过而非整体报错：明文数据层对单个文件损坏保持容错
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(p) = toml::from_str::<Persona>(&raw) {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// ---------- themes/（自定义主题，M4.3 · 决断 7：主题块是运行时数据，归 JSON 侧）----------
//
// `themes/<名>.json`：拷走 DataHub 即主题跟走。固定保留名 `custom` 是「当前生效的
// 自定义主题」——启动时由前端加载并应用；其余名字是主题库（主题页保存/应用）。

/// 当前生效的自定义主题的固定文件名（stem）。活动主题永远写这里，避免「哪个文件
/// 在生效」需要第二个指针。
pub const THEME_ACTIVE_STEM: &str = "custom";

/// 主题 JSON（与前端 `CustomTheme` 同形，camelCase 由 serde 映射）。
/// vars 是令牌 → 值（如 `--color-primary` → `oklch(...)`）；BTreeMap 让序列化
/// 顺序确定（同主题多次落盘字节一致，diff 友好）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTheme {
    #[serde(default)]
    pub preset: String,
    pub color_scheme: String,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

pub fn save_theme(root: &Path, name: &str, theme: &CustomTheme) -> StoreResult<String> {
    let stem = crate::stimport::sanitize_dir_name(name);
    let dir = root.join("themes");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{stem}.json"));
    let json = serde_json::to_string_pretty(theme)?;
    std::fs::write(&path, json + "\n")?;
    Ok(stem)
}

/// 读单个主题；文件不存在 = None（不是错误——启动加载依赖这个语义）。
pub fn load_theme(root: &Path, name: &str) -> StoreResult<Option<CustomTheme>> {
    let stem = crate::stimport::sanitize_dir_name(name);
    let path = root.join("themes").join(format!("{stem}.json"));
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(&path)?)?))
}

pub fn list_themes(root: &Path) -> StoreResult<Vec<String>> {
    let dir = root.join("themes");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            out.push(stem.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// 删除主题文件；返回是否真的删了东西（清除活动主题时用于幂等）。
pub fn delete_theme(root: &Path, name: &str) -> StoreResult<bool> {
    let stem = crate::stimport::sanitize_dir_name(name);
    let path = root.join("themes").join(format!("{stem}.json"));
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// 主题块导入的解析结果（name/color-scheme 取自块内声明，vars 为键值对）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedThemeImport {
    pub name: String,
    pub color_scheme: String,
    pub vars: BTreeMap<String, String>,
}

/// 导入解析（纯函数，便于单测）：接受 toPluginCss 同格式的 `@plugin "daisyui/theme"`
/// CSS 块，或前端 `CustomTheme` 同形的 JSON。键不在 allowed_keys 里的一律拒绝并
/// **列出全部非法键**（DoD：坏令牌键导入拒绝并列出非法键）——导入面只认编辑器
/// 认识的令牌，未知键宁可让用户删掉也不静默带上。
pub fn parse_theme_import(payload: &str, allowed_keys: &[String]) -> Result<ParsedThemeImport, String> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Err("导入内容为空".into());
    }
    let (name, color_scheme, raw_vars) = if trimmed.starts_with('{') {
        parse_theme_json(trimmed)?
    } else {
        parse_theme_css(trimmed)?
    };
    let mut vars = BTreeMap::new();
    let mut illegal: Vec<String> = Vec::new();
    for (key, value) in raw_vars {
        let value = value.trim().trim_end_matches(';').trim().to_string();
        if !allowed_keys.iter().any(|k| k == &key) {
            illegal.push(key);
            continue;
        }
        if value.is_empty() {
            continue;
        }
        vars.insert(key, value);
    }
    if !illegal.is_empty() {
        illegal.sort();
        illegal.dedup();
        return Err(format!("存在不认识的令牌键：{}", illegal.join("、")));
    }
    if vars.is_empty() {
        return Err("没有可用的令牌键值对".into());
    }
    let name = if name.trim().is_empty() { "custom".to_string() } else { name.trim().to_string() };
    let color_scheme = if color_scheme == "dark" { "dark".to_string() } else { "light".to_string() };
    Ok(ParsedThemeImport { name, color_scheme, vars })
}

/// JSON 分支：`{preset?, colorScheme?, vars?}`（前端 CustomTheme 同形）。
fn parse_theme_json(trimmed: &str) -> Result<(String, String, Vec<(String, String)>), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        #[serde(default)]
        preset: String,
        #[serde(default)]
        color_scheme: String,
        #[serde(default)]
        vars: BTreeMap<String, String>,
    }
    let raw: Raw = serde_json::from_str(trimmed).map_err(|e| format!("主题 JSON 解析失败：{e}"))?;
    Ok((
        raw.preset,
        raw.color_scheme,
        raw.vars.into_iter().collect(),
    ))
}

/// CSS 分支：取 `@plugin "daisyui/theme" { … }` 块内的 `name` / `color-scheme`
/// 声明与 `--key: value;` 令牌行。块外的内容一律忽略（用户可能连着别的 CSS 一起粘）。
fn parse_theme_css(trimmed: &str) -> Result<(String, String, Vec<(String, String)>), String> {
    const MARK: &str = "@plugin";
    let mark_pos = trimmed.find(MARK).ok_or("未找到 @plugin 主题块（应粘「主题」页导出的 CSS）")?;
    let open = trimmed[mark_pos..]
        .find('{')
        .ok_or("主题块缺少 {（应粘「主题」页导出的 CSS）")?;
    let after_open = &trimmed[mark_pos + open + 1..];
    // 主题块不嵌套：碰到第一个 } 即收尾
    let close = after_open.find('}').ok_or("主题块缺少 }（应粘「主题」页导出的 CSS）")?;
    let body = &after_open[..close];

    let mut name = String::new();
    let mut color_scheme = String::new();
    let mut vars = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let line = line.strip_suffix(';').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').to_string();
        if key == "name" {
            name = value;
        } else if key == "color-scheme" {
            color_scheme = value;
        } else if key.starts_with("--") {
            vars.push((key.to_string(), value));
        }
        // 其余声明（default / prefersdark 等）不是令牌，忽略
    }
    Ok((name, color_scheme, vars))
}

// ---------- sessions/（设计 §12：session.json + messages.jsonl + state.json + blackboard.json）----------

/// 剧场模式配置（M3.6 · 设计 §10.5）：自动轮次的轮数预算与起点。
///
/// `budget` = 本场戏的轮数预算（目标函数「限定轮数内完成完整的开线→收线弧」的轮数）；
/// `start_turn` = 开场时的最后轮次（进度 = 当前轮 − start_turn）。None = 剧场关闭。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TheaterConfig {
    pub budget: u32,
    pub start_turn: u64,
}

/// 会话元数据（session.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    /// ISO-8601 UTC
    pub created_at: String,
    /// 角色阵容（M1 为 1v1：单元素；元素是 characters/ 下的目录名）
    pub characters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// 启用的世界（设定集名）；M1.1 不读卡，先留空由 M1.2 补
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    /// 会话种子：api.random 的可回放随机源（设计 §3.1）
    pub seed: u64,
    /// 起因（premise），新建向导 v0 可填
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premise: Option<String>,
    /// 每轮发言数上限（M3.4 群聊 · 设计 §10.5；None = 缺省 2。导演调度天然限流，
    /// 对冲隔离模式的请求成本）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_speakers: Option<u32>,
    /// 剧场模式（M3.6 · 设计 §10.5：自动轮次 + 导演树起承转合 + 交叉剪辑）；
    /// None = 关闭。导演树本体住在 sessions/<id>/director.lua（缺省用内置起承转合树）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theater: Option<TheaterConfig>,
    /// 即兴模式（M3.8 · 设计 §6.8-4，默认关）：激活实体过薄时，便宜模型现场补一条
    /// 「设定·暂定」注入当轮生效；提案照落收件箱，确认后才算正史。
    #[serde(default)]
    pub improv: bool,
    /// 小说模式（增强 F1 · 决断 9：会话级模式开关，非新引擎）：v1 仅 1v1 单场景
    /// 可开。开启后叙事锁小说体、消息按散文排版、段末生成走向选项
    #[serde(default)]
    pub novel_mode: bool,
    /// 语义源嵌入模型（M3.10 · 设计 §6.13）：首次实际启用语义召回时把
    /// 「provider 名 + 模型名」记进会话（可回放语义随版本声明）；None = 从未启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
}

/// 事件流里的一条消息（M2.0 起 messages.jsonl 是类型化事件流，
/// 消息只是其中一种；事件类型见 event.rs）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// 轮次，从 1 起
    pub turn: u64,
    /// user | char | system
    pub role: String,
    pub content: String,
    /// unix 秒
    pub ts: u64,
    /// 场景分段标识（M1 恒为单场景，可缺省）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    /// 这条消息是谁说的（M3.1 群聊：char 消息的角色署名；缺省 = 会话首个角色）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 与正文同报文的原始工具调用（增强 A·决断 3）：只存档不落效果，
    /// 效果由「tool_calls + 确定性校验器」在重放时投影推导——编辑/重roll 后自动正确
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<crate::llm::ToolCall>>,
}

/// 黑板 v0（设计 §4.1 B1 的数据源；UI 可手动编辑，每轮时钟步进）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blackboard {
    pub day: i64,
    pub clock: String,
    pub place: String,
    pub actors: Vec<String>,
    /// 实体作用域键（设计 §6.4：状态树/导演改 `bb["char.小雨"].status`，设定集的
    /// `live` 字段据此拼出 `▸当前`）。键形如 `char.小雨.status`；旧会话没有这个字段，
    /// 反序列化默认空，不做破坏性迁移。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Blackboard {
    /// 缺省黑板（没有黑板事件、也没有旧文件的会话用；与 new_session 的默认值一致）
    pub fn default_board() -> Blackboard {
        Blackboard {
            day: 1,
            clock: String::new(),
            place: String::new(),
            actors: Vec::new(),
            extra: std::collections::BTreeMap::new(),
        }
    }
}

pub fn load_blackboard(root: &Path, session_id: &str) -> StoreResult<Blackboard> {
    let path = session_dir(root, session_id).join("blackboard.json");
    if !path.exists() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
}

pub fn save_blackboard(root: &Path, session_id: &str, bb: &Blackboard) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    std::fs::write(
        dir.join("blackboard.json"),
        serde_json::to_string_pretty(bb)? + "\n",
    )?;
    Ok(())
}

/// 各角色 Lua state 快照（缺文件/坏文件回退空对象；空对象由调用方
/// 降级为卡上 default_state）
pub fn load_state(root: &Path, session_id: &str) -> StoreResult<serde_json::Value> {
    let path = session_dir(root, session_id).join("state.json");
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(&path)?).unwrap_or(serde_json::json!({})))
}

pub fn save_state(root: &Path, session_id: &str, state: &serde_json::Value) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_string_pretty(state)? + "\n",
    )?;
    Ok(())
}

// ---------- palace.jsonl：卡内长期记忆写入流（设计 §12；M2 记忆宫殿的落点）----------

/// 记忆对象（设计 §12：`palace.jsonl` 追加流中的一条）。
///
/// M1.6 只落卡内 `api.memory.set` 的键值写入，并按 `turn` 溯源；
/// 记忆宫殿的读侧（召回/房间/时间线）在 M2 长出来，届时本结构按设计扩充
/// （`kind` 之外的字段、witnesses 等），旧记录保持可读。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemRecord {
    /// 记录类型：M1 恒为 `fact`（卡内写入的长期事实）
    pub kind: String,
    pub key: String,
    pub value: serde_json::Value,
    /// 来源：`hook.on_message` / `hook.on_load`（设计 §3.1 的卡内写入）
    pub source: String,
    /// 产生该记录的轮次（on_load 为 0）
    pub turn: u64,
    pub ts: u64,
}

/// 追加一条记忆记录（创建文件；调用方保证目录存在）
pub fn append_memory_record(root: &Path, session_id: &str, rec: &MemRecord) -> StoreResult<()> {
    use std::io::Write;
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let line = serde_json::to_string(rec)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("palace.jsonl"))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// 全量重写 palace.jsonl（M2.0：宫殿由事件流投影写出，不再由写入点各自追加）。
/// 旧记录格式不变，读侧照旧可读。
pub fn write_memory_records(root: &Path, session_id: &str, records: &[MemRecord]) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let mut buf = String::new();
    for rec in records {
        buf.push_str(&serde_json::to_string(rec)?);
        buf.push('\n');
    }
    std::fs::write(dir.join("palace.jsonl"), buf)?;
    Ok(())
}

/// 读取全部记忆记录（坏行跳过；文件不存在返回空）
pub fn read_memory_records(root: &Path, session_id: &str) -> StoreResult<Vec<MemRecord>> {
    let path = session_dir(root, session_id).join("palace.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_to_string(&path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<MemRecord>(l).ok())
        .collect())
}


pub struct NewSessionRequest {
    pub character: String,
    /// 角色阵容（M3.1 群聊；空 = 单角色，取 character；首个是主角色/默认发言人）
    pub characters: Vec<String>,
    pub persona: Option<String>,
    pub day: Option<i64>,
    pub clock: Option<String>,
    pub place: Option<String>,
    pub premise: Option<String>,
    /// 剧本模板（M4.1 · 决断 3）：给出时初始 premise 与黑板从 `scripts/<名>/`
    /// 取——向导显式填写的字段覆盖剧本值，阵容永远用本会话的
    pub script: Option<String>,
    /// 启用的设定集（世界）名（M5.3）：空/缺省 = default；命令层已校验目录存在
    pub world: Option<String>,
}

pub fn session_dir(root: &Path, id: &str) -> PathBuf {
    root.join("sessions").join(id)
}

/// 创建会话目录骨架并写入初始文件，返回元数据
pub fn new_session(root: &Path, req: &NewSessionRequest) -> StoreResult<SessionMeta> {
    ensure_layout(root)?;
    let now_secs = unix_now();
    // 角色阵容：显式给的全量用（去重、保序、主角色在前），没给就退回单角色。
    // 加固 B2：dedup() 只去相邻重复——`["b","a","b"]` 会让同一角色每轮跑两次钩子、
    // 双份记忆写入、黑板 actors 重复；改全量保序去重。
    let cast: Vec<String> = if req.characters.is_empty() {
        vec![req.character.clone()]
    } else {
        let mut seen: Vec<String> = Vec::with_capacity(req.characters.len());
        for c in &req.characters {
            if !seen.contains(c) {
                seen.push(c.clone());
            }
        }
        seen
    };
    // 剧本模板（M4.1）：premise/初始黑板从模板取底，向导显式字段覆盖
    let script = match req.script.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => Some(
            crate::pack::load_script_template(root, name)
                .map_err(|e| StoreError::NotFound(e))?,
        ),
        None => None,
    };
    let premise = req
        .premise
        .clone()
        .filter(|p| !p.trim().is_empty())
        .or_else(|| {
            script
                .as_ref()
                .map(|s| s.premise.clone())
                .filter(|p| !p.trim().is_empty())
        });
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    let meta = SessionMeta {
        id: unique_session_id_at(root, now_secs, millis),
        created_at: iso8601(now_secs),
        novel_mode: false,
        characters: cast.clone(),
        persona: req.persona.clone(),
        // 启用的设定集（M5.3）：命令层已校验目录存在，这里只管落盘；None → 读侧回落 default
        world: req.world.clone(),
        seed: seed_now(),
        premise,
        max_speakers: None,
        theater: None,
        improv: false,
        embed_model: None,
    };
    let dir = session_dir(root, &meta.id);
    std::fs::create_dir_all(&dir)?;

    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&meta)? + "\n",
    )?;
    std::fs::write(dir.join("messages.jsonl"), "")?;
    // 各角色 Lua state 快照（M1.2 卡片加载后由 hooks 写入，初始为空表）
    std::fs::write(dir.join("state.json"), "{}\n")?;
    let script_board = script.as_ref().and_then(|s| s.blackboard.clone());
    let blackboard = Blackboard {
        day: req.day.or(script_board.as_ref().map(|b| b.day)).filter(|d| *d >= 1).unwrap_or(1),
        clock: match req.clock.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => c.to_string(),
            None => script_board
                .as_ref()
                .map(|b| b.clock.clone())
                .unwrap_or_default(),
        },
        place: match req.place.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => p.to_string(),
            None => script_board
                .as_ref()
                .map(|b| b.place.clone())
                .unwrap_or_default(),
        },
        actors: cast,
        // 剧本初始黑板的实体作用域键（设计 §6.4）原样带来；没有剧本则空表
        extra: script_board.map(|b| b.extra).unwrap_or_default(),
    };
    std::fs::write(
        dir.join("blackboard.json"),
        serde_json::to_string_pretty(&blackboard)? + "\n",
    )?;
    // 剧本模板的自定义导演树落进会话（既有机制：sessions/<id>/director.lua 覆盖
    // 内置起承转合树）；模板没带树就落回内置，无需动作
    if let Some(tpl) = &script {
        if tpl.has_director {
            let src = crate::pack::scripts_dir(root).join(&tpl.name).join("director.lua");
            std::fs::copy(&src, dir.join("director.lua"))?;
        }
    }
    Ok(meta)
}

/// 解析缓冲区里的完整事件行（返回事件与消费的字节数）。
/// 只到最后一个换行符为止——结尾半行（崩溃残留）留待补全；坏行跳过。
/// 字节级切行对 UTF-8 安全（多字节字符的续字节不含换行）。
fn parse_complete_lines(
    buf: &[u8],
    next_seq: crate::event::Seq,
) -> (Vec<crate::event::LogRecord>, usize) {
    crate::event::parse_lines(buf, next_seq)
}

/// 读取全量消息（直读文件；热路径走 [`EventLog`] 增量缓存）
pub fn read_messages(root: &Path, session_id: &str) -> StoreResult<Vec<Message>> {
    let path = session_dir(root, session_id).join("messages.jsonl");
    if !path.exists() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    Ok(crate::event::messages(
        &parse_complete_lines(&std::fs::read(&path)?, 1).0,
    ))
}

/// 全量重写 messages.jsonl，**只写消息行**（M1 时代的形态；事件流的整体重写请用
/// [`EventLog::rewrite`]。保留它是因为「纯消息」文件仍是合法的旧格式，测试与
/// 手工修复都用得上）。
/// 调用方必须随后 `EventLog::invalidate`：缓存按字节偏移增量读，
/// 重写后偏移失效（变短的文件会自动重置，但等长/变长改写检测不到）。
pub fn write_messages(root: &Path, session_id: &str, messages: &[Message]) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let mut buf = String::new();
    for m in messages {
        buf.push_str(&serde_json::to_string(m)?);
        buf.push('\n');
    }
    std::fs::write(dir.join("messages.jsonl"), buf)?;
    Ok(())
}

/// 扫描 sessions/*/session.json，按创建时间倒序
pub fn list_sessions(root: &Path) -> StoreResult<Vec<SessionMeta>> {
    let dir = root.join("sessions");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let meta_path = entry.path().join("session.json");
        if let Ok(raw) = std::fs::read_to_string(&meta_path) {
            if let Ok(m) = serde_json::from_str::<SessionMeta>(&raw) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

// ---------- summary.md / proposals.jsonl（M2.6 的派生文件）----------

/// 全量重写 summary.md（由投影写出：摘要事件按序拼接）
pub fn write_summary(root: &Path, session_id: &str, text: &str) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    std::fs::write(dir.join("summary.md"), text)?;
    Ok(())
}

/// 全量重写 scenes.json（M3.2：场景投影的派生文件，明文可查）
pub fn write_scenes(
    root: &Path,
    session_id: &str,
    scenes: &[crate::scene::Scene],
    active: Option<&str>,
) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let payload = serde_json::json!({ "active": active, "scenes": scenes });
    std::fs::write(
        dir.join("scenes.json"),
        serde_json::to_string_pretty(&payload)? + "\n",
    )?;
    Ok(())
}

/// 读取 scenes.json（不存在/坏文件返回 None；权威数据在事件流投影里）
pub fn read_scenes(root: &Path, session_id: &str) -> Option<(Vec<crate::scene::Scene>, Option<String>)> {
    let path = session_dir(root, session_id).join("scenes.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let scenes = v
        .get("scenes")?
        .as_array()?
        .iter()
        .filter_map(|s| serde_json::from_value::<crate::scene::Scene>(s.clone()).ok())
        .collect();
    let active = v
        .get("active")
        .and_then(|a| a.as_str())
        .map(str::to_string);
    Some((scenes, active))
}

/// 读取 summary.md（不存在返回空串）
pub fn read_summary(root: &Path, session_id: &str) -> StoreResult<String> {
    let path = session_dir(root, session_id).join("summary.md");
    if !path.exists() {
        return Ok(String::new());
    }
    Ok(std::fs::read_to_string(&path)?)
}

/// 全量重写 proposals.jsonl（设定收件箱：一条提案一行，由投影写出）
pub fn write_proposals(
    root: &Path,
    session_id: &str,
    proposals: &[serde_json::Value],
) -> StoreResult<()> {
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let mut buf = String::new();
    for p in proposals {
        buf.push_str(&serde_json::to_string(p)?);
        buf.push('\n');
    }
    std::fs::write(dir.join("proposals.jsonl"), buf)?;
    Ok(())
}

/// 读取全部提案（坏行跳过；文件不存在返回空）
pub fn read_proposals(root: &Path, session_id: &str) -> StoreResult<Vec<serde_json::Value>> {
    let path = session_dir(root, session_id).join("proposals.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_to_string(&path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect())
}

// ---------- EventLog：会话事件流的增量缓存（高轮次性能）----------
//
// messages.jsonl 是追加式**事件流**（设计 §12 + §7.3），本应用是唯一写入者。缓存记住每会话
// 已读到的字节偏移，读取时只 seek 续读新增部分——高轮次下每轮开销与新增行数成正比，
// 而非全量重解析。半行（崩溃时未写完）留待补全后再消费；文件被外部截断/重写时缓存自动重置。

/// 会话事件流缓存（Tauri State；跨命令复用）
#[derive(Default)]
pub struct EventLog {
    /// 按会话分锁（加固 D4）：外层表只做条目查找，条目内的文件 IO 与缓存操作
    /// 持各自的锁——会话 A 的大文件增量读不再阻塞会话 B 的一切命令与后台总结。
    inner: std::sync::Mutex<HashMap<String, std::sync::Arc<std::sync::Mutex<LogEntry>>>>,
}

#[derive(Default)]
struct LogEntry {
    records: std::sync::Arc<Vec<crate::event::LogRecord>>,
    /// 已消费到的字节偏移（最后一个完整行的行尾）
    pos: u64,
    /// 增量投影缓存（加固 D1-2）：折叠是纯函数（基线为空的 genesis 流）时，
    /// 新记录只 fold 进缓存——每条事件的投影成本从 O(流长度) 降到 O(新增)。
    /// rewrite / 外部截断重置时清空。
    proj: Option<ProjCache>,
}

#[derive(Default)]
struct ProjCache {
    /// 缓存投影覆盖到的记录数（records 链是 append-only 前缀扩展时才可续）
    based_on_len: usize,
    proj: std::sync::Arc<crate::event::Projection>,
}

fn poisoned() -> StoreError {
    StoreError::Io(std::io::Error::other("事件流缓存锁 poisoned"))
}

impl EventLog {
    pub fn new() -> Self {
        EventLog::default()
    }

    /// 取（或建）一个会话的缓存条目句柄；外层表锁只在这里短暂持有
    fn entry(
        &self,
        session_id: &str,
    ) -> StoreResult<std::sync::Arc<std::sync::Mutex<LogEntry>>> {
        let mut map = self.inner.lock().map_err(|_| poisoned())?;
        Ok(map.entry(session_id.to_string()).or_default().clone())
    }

    /// 读取会话全部事件（增量续读；无新数据时直接返回缓存 Arc，零拷贝零解析）
    pub fn read(
        &self,
        root: &Path,
        session_id: &str,
    ) -> StoreResult<std::sync::Arc<Vec<crate::event::LogRecord>>> {
        let entry = self.entry(session_id)?;
        let mut e = entry.lock().map_err(|_| poisoned())?;
        sync_entry(&mut e, root, session_id)?;
        Ok(e.records.clone())
    }

    /// 消息视图（对话历史；顺序即对话顺序）
    pub fn messages(&self, root: &Path, session_id: &str) -> StoreResult<Vec<Message>> {
        Ok(crate::event::messages(&self.read(root, session_id)?))
    }

    /// 追加一条事件：写文件 + 同步缓存，返回落定后的记录（含按位置分配的 seq）。
    /// 参数接受任何能转成 [`crate::event::LogBody`] 的东西（`Message` 与 `&Message` 都行），
    /// 于是「追加一条消息」在调用点读起来仍是原来那句。
    pub fn append<B: Into<crate::event::LogBody>>(
        &self,
        root: &Path,
        session_id: &str,
        body: B,
    ) -> StoreResult<crate::event::LogRecord> {
        let body = body.into();
        use std::io::Write;
        let entry = self.entry(session_id)?;
        let mut e = entry.lock().map_err(|_| poisoned())?;
        sync_entry(&mut e, root, session_id)?;

        let path = session_dir(root, session_id).join("messages.jsonl");
        // 加固 A3：崩溃残留的尾部半行——sync_entry 只消费完整行，pos 落后于文件长度。
        // 直接 append 会把新行粘在半行后面（非法 JSON，这条消息重启后读不回）。
        // 先补一个换行封口：半行（或无换行的整行）成为独立一行被读侧处理，pos 对齐后再写新行。
        let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(e.pos);
        if file_len > e.pos {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
            f.write_all(b"\n")?;
            e.pos = file_len + 1;
        }

        let seq = e.records.last().map(|r| r.seq).unwrap_or(0) + 1;
        let record = crate::event::LogRecord::new(seq, body);
        let line = record
            .to_line()
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;

        std::sync::Arc::make_mut(&mut e.records).push(record.clone());
        e.pos += line.len() as u64;
        Ok(record)
    }

    /// 全量重写事件流（消息编辑/删除/重roll 的重建结果）。
    /// 序号按位置重排；写盘后缓存直接换新——重写是本进程发起的，无需重读文件。
    pub fn rewrite(
        &self,
        root: &Path,
        session_id: &str,
        records: &[crate::event::LogRecord],
    ) -> StoreResult<()> {
        let dir = session_dir(root, session_id);
        if !dir.is_dir() {
            return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
        }
        let text = crate::event::render_lines(records)
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
        atomic_write(&dir.join("messages.jsonl"), text.as_bytes())?;

        let entry = self.entry(session_id)?;
        let mut e = entry.lock().map_err(|_| poisoned())?;
        // 重新编号后缓存与文件一致（parse_lines 也是按位置编号）
        e.records = std::sync::Arc::new(crate::event::parse_lines(text.as_bytes(), 1).0);
        e.pos = text.len() as u64;
        // D1-2：链被整体换掉，投影缓存作废（下一次投影全量重算）
        e.proj = None;
        Ok(())
    }

    /// 丢弃某会话（或全部）缓存；下次读取全量重建。
    /// 外部改动文件后调用。
    pub fn invalidate(&self, session_id: Option<&str>) {
        if let Ok(mut map) = self.inner.lock() {
            match session_id {
                Some(id) => {
                    map.remove(id);
                }
                None => map.clear(),
            }
        }
    }

    /// 增量投影（加固 D1-2）：缓存命中（链自上次投影起只是 append 扩展）时只
    /// fold 新增记录，否则用 `full` 全量重算并放入缓存。`full` 只应做「空基线
    /// 全量折叠」——基线来自派生文件的老会话投影不走这条路径（基线不在缓存
    /// 语义内）。rewrite / 外部截断重置自动失效。
    pub fn project_cached_records(
        &self,
        session_id: &str,
        records: &std::sync::Arc<Vec<crate::event::LogRecord>>,
        full: impl Fn(&[crate::event::LogRecord]) -> crate::event::Projection,
    ) -> StoreResult<std::sync::Arc<crate::event::Projection>> {
        let entry = self.entry(session_id)?;
        let mut e = entry.lock().map_err(|_| poisoned())?;
        let cache_ok = e
            .proj
            .as_ref()
            .map(|c| c.based_on_len <= records.len())
            .unwrap_or(false);
        let proj = if cache_ok {
            let cache = e.proj.as_mut().expect("上面刚判过 Some");
            let p = std::sync::Arc::make_mut(&mut cache.proj);
            for rec in &records[cache.based_on_len..] {
                crate::event::fold(p, rec);
            }
            cache.based_on_len = records.len();
            std::sync::Arc::clone(&cache.proj)
        } else {
            let fresh = std::sync::Arc::new(full(records));
            e.proj = Some(ProjCache {
                based_on_len: records.len(),
                proj: std::sync::Arc::clone(&fresh),
            });
            fresh
        };
        Ok(proj)
    }
}

/// 将缓存条目推进到文件当前末尾（只解析新增的完整行）
fn sync_entry(entry: &mut LogEntry, root: &Path, session_id: &str) -> StoreResult<()> {
    use std::io::{Read, Seek, SeekFrom};
    let path = session_dir(root, session_id).join("messages.jsonl");
    if !path.exists() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let len = std::fs::metadata(&path)?.len();
    if len < entry.pos {
        // 文件被外部截断/重写：丢弃缓存全量重读
        entry.pos = 0;
        std::sync::Arc::make_mut(&mut entry.records).clear();
        entry.proj = None; // D1-2：链被换，投影缓存一并作废
    }
    if len == entry.pos {
        return Ok(());
    }
    let mut f = std::fs::File::open(&path)?;
    f.seek(SeekFrom::Start(entry.pos))?;
    let mut buf = Vec::with_capacity((len - entry.pos) as usize);
    f.read_to_end(&mut buf)?;

    let next_seq = entry.records.last().map(|r| r.seq).unwrap_or(0) + 1;
    let (records, consumed) = parse_complete_lines(&buf, next_seq);
    if consumed == 0 {
        return Ok(()); // 只有半行：留待补全
    }
    std::sync::Arc::make_mut(&mut entry.records).extend(records);
    entry.pos += consumed as u64;
    Ok(())
}

// ---------- 时间工具（不引入时间库；Howard Hinnant civil 算法）----------

/// 当前 unix 秒（命令层落盘时间戳用）
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 读取单个会话元数据
pub fn load_session(root: &Path, id: &str) -> StoreResult<SessionMeta> {
    let path = session_dir(root, id).join("session.json");
    if !path.exists() {
        return Err(StoreError::NotFound(format!("会话「{}」", id)));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
}

/// 回写单个会话元数据（session.json 全量覆盖；调用方先 load 后改）
pub fn save_session(root: &Path, meta: &SessionMeta) -> StoreResult<()> {
    let dir = session_dir(root, &meta.id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(meta)? + "\n",
    )?;
    Ok(())
}

// ---------- 世界（M3.7 · 设计 §6.6/§12）：世界元信息、世界时钟与世界级线的持久化 ----------

/// 世界状态文件：`codex/<世界>/world.json`。世界级单例——跨会话持久的时钟基准、
/// 主线进度与世界级线都在这里；会话事件流只记「本会话见证的走位」。
pub fn world_path(root: &Path, world: &str) -> PathBuf {
    root.join("codex").join(world).join("world.json")
}

/// 读世界状态。缺文件 / 坏文件 = 缺省（第 1 天、无主线）——世界文件是可选层，
/// 不该让任何会话停摆（与 director.lua 坏配置回落默认树同纪律）。
pub fn load_world(root: &Path, world: &str) -> crate::worldline::WorldState {
    std::fs::read_to_string(world_path(root, world))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写世界状态（目录不存在则建；全量覆盖）
pub fn save_world(
    root: &Path,
    world: &str,
    state: &crate::worldline::WorldState,
) -> StoreResult<()> {
    let path = world_path(root, world);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(&path, (serde_json::to_string_pretty(state)? + "\n").as_bytes())?;
    Ok(())
}

// ---------- 收件箱正史物化（M3.8 · 设计 §6.9）：确认的提案落世界级 grown.json ----------

/// 正史增量文件：`codex/<世界>/grown.json`。玩家手写的实体文件**永不被机器改写**
/// （anchors 等手写内容不被覆盖）；收件箱确认的提案物化成这里的补丁，加载设定集时
/// 应用（`codex::apply_grown`）：新实体追加、既有实体深合并。明文 JSON，可手动审阅删除。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GrownFile {
    /// 实体 id → 补丁（新实体 = 全量骨架；既有实体 = 部分覆盖：facts 深合并、
    /// aliases/relations 追加去重、secrets 按 key 合并）
    #[serde(default)]
    pub entities: BTreeMap<String, serde_json::Value>,
}

pub fn grown_path(root: &Path, world: &str) -> PathBuf {
    root.join("codex").join(world).join("grown.json")
}

/// 读正史增量。缺文件 = 空（从没有过收件箱写入）。
pub fn load_grown(root: &Path, world: &str) -> GrownFile {
    std::fs::read_to_string(grown_path(root, world))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写正史增量（目录不存在则建；全量覆盖）
pub fn save_grown(root: &Path, world: &str, grown: &GrownFile) -> StoreResult<()> {
    let path = grown_path(root, world);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(&path, (serde_json::to_string_pretty(grown)? + "\n").as_bytes())?;
    Ok(())
}

fn seed_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 ^ (d.as_secs() << 32))
        .unwrap_or(0)
}

/// 会话 id：`20260919-180102-483`（本地无关的 UTC，毫秒尾数防同秒碰撞）
fn session_id_at(secs: u64, millis: u32) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        millis
    )
}

/// 加固 A7：同毫秒两次 `new_session` 会拿到同一个 id、目录互覆——
/// 目录已存在就递增毫秒尾数重滚，直到空位。
fn unique_session_id_at(root: &Path, secs: u64, millis: u32) -> String {
    let mut millis = millis % 1000;
    let mut id = session_id_at(secs, millis);
    while session_dir(root, &id).exists() {
        millis = (millis + 1) % 1000;
        id = session_id_at(secs, millis);
    }
    id
}

/// unix 秒 → ISO-8601 UTC（如 `2026-09-19T10:02:03Z`）
fn iso8601(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// 天数 → 公历年月日（Howard Hinnant，civil_from_days）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_provider(name: &str, role: &str) -> Provider {
        Provider {
            name: name.into(),
            base_url: "https://api.deepseek.com/v1".into(),
            api_key: "sk-test".into(),
            model: "deepseek-chat".into(),
            temperature: 0.8,
            role: role.into(),
            tools: "off".into(),
        }
    }

    #[test]
    fn providers_roundtrip_and_upsert_delete() {
        let root = tempfile::tempdir().unwrap();
        assert!(load_providers(root.path()).unwrap().is_empty());

        upsert_provider(root.path(), sample_provider("deepseek", "chat")).unwrap();
        upsert_provider(root.path(), sample_provider("deepseek-cheap", "util")).unwrap();
        let list = load_providers(root.path()).unwrap();
        assert_eq!(list.len(), 2);

        // 同名 upsert 覆盖而非追加
        let mut edited = sample_provider("deepseek", "chat");
        edited.model = "deepseek-reasoner".into();
        upsert_provider(root.path(), edited).unwrap();
        let list = load_providers(root.path()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.iter().find(|p| p.name == "deepseek").unwrap().model,
            "deepseek-reasoner"
        );

        delete_provider(root.path(), "deepseek-cheap").unwrap();
        assert_eq!(load_providers(root.path()).unwrap().len(), 1);
        assert!(delete_provider(root.path(), "不存在的").is_err());
    }

    #[test]
    fn settings_defaults_and_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let s = load_settings(root.path()).unwrap();
        assert_eq!(s.narrative_mode, "台词体");

        let mut s = Settings::default();
        s.narrative_mode = "小说体".into();
        save_settings(root.path(), &s).unwrap();
        assert_eq!(load_settings(root.path()).unwrap().narrative_mode, "小说体");
    }

    #[test]
    fn personas_listing_skips_bad_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("personas")).unwrap();
        std::fs::write(
            root.path().join("personas/b.toml"),
            "name = \"beta\"\ndescription = \"夜读者\"\n",
        )
        .unwrap();
        std::fs::write(root.path().join("personas/a.toml"), "name = \"alpha\"\n").unwrap();
        std::fs::write(root.path().join("personas/bad.toml"), "{not toml").unwrap();
        // 旧 json 不再识别
        std::fs::write(
            root.path().join("personas/legacy.json"),
            "{\"name\":\"legacy\"}",
        )
        .unwrap();

        let list = list_personas(root.path()).unwrap();
        assert_eq!(
            list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert_eq!(list[0].description, "");
        assert_eq!(list[1].description, "夜读者");
    }

    #[test]
    fn providers_toml_roundtrip_with_special_chars() {
        let root = tempfile::tempdir().unwrap();
        let mut p = sample_provider("deepseek", "chat");
        p.model = "模型#\"引号\"\n带换行".into(); // TOML 需正确转义
        upsert_provider(root.path(), p).unwrap();
        let list = load_providers(root.path()).unwrap();
        assert_eq!(list[0].model, "模型#\"引号\"\n带换行");
    }

    /// 真机会话的 JSONL 解析回归：把 APPDATA 里最新一场会话读回，逐条断言 role/content。
    #[test]
    fn installed_app_session_parses_from_jsonl() {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let sessions = std::path::Path::new(&appdata).join("huajing/DataHub/sessions");
        if !sessions.is_dir() {
            println!("SKIP: 无 APPDATA sessions");
            return;
        }
        let mut newest: Option<std::path::PathBuf> = None;
        for entry in std::fs::read_dir(&sessions).unwrap().flatten() {
            let p = entry.path();
            if !p.join("messages.jsonl").is_file() {
                continue;
            }
            let mtime = entry.metadata().and_then(|m| m.modified()).ok();
            let better = match (&newest, mtime) {
                (None, _) => true,
                (Some(prev), Some(t)) => std::fs::metadata(prev)
                    .and_then(|m| m.modified())
                    .map(|pt| t > pt)
                    .unwrap_or(false),
                _ => false,
            };
            if better {
                newest = Some(p);
            }
        }
        let Some(dir) = newest else {
            println!("SKIP: 无会话");
            return;
        };
        let raw = std::fs::read_to_string(dir.join("messages.jsonl")).unwrap();
        let msgs: Vec<Message> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Message>(l).expect("每条 JSONL 都应解析为 Message"))
            .collect();
        println!("CHECK 会话 {} 共 {} 条", dir.file_name().unwrap().to_string_lossy(), msgs.len());
        for m in &msgs {
            println!(
                "CHECK turn={} role={:?} content={:?} 字符数={}",
                m.turn,
                m.role,
                m.content.chars().take(12).collect::<String>(),
                m.content.chars().count()
            );
        }
        assert!(msgs.iter().any(|m| m.role == "user" && m.content.contains("谢谢")));
    }

    /// 真机数据回归：若本机 `%APPDATA%\huajing\DataHub` 存在（安装版跑过），
    /// 用它那份卡跑一遍钩子流水线——钩子探测、state 演进、记忆写入、诊断留痕。
    /// 无该目录时跳过（开发机/CI 上不会因此变红）。
    #[test]
    fn installed_app_card_flows_through_hooks() {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let hub = std::path::Path::new(&appdata).join("huajing/DataHub");
        if !hub.is_dir() {
            println!("PROBE: 跳过（无 APPDATA DataHub）");
            return;
        }
        let loaded = crate::card::load_card(&hub, "小雨").unwrap();
        println!("PROBE hook_names={:?} degraded={}", loaded.hook_names, loaded.degraded);
        println!("PROBE default_state={}", loaded.default_state);

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        ensure_layout(&root).unwrap();
        std::fs::create_dir_all(root.join("characters/小雨")).unwrap();
        std::fs::copy(
            hub.join("characters/小雨/card.lua"),
            root.join("characters/小雨/card.lua"),
        )
        .unwrap();
        let meta = new_session(
            &root,
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: Some(1),
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        let sink: crate::card::UiSink = std::sync::Arc::new(|_| {});

        // on_load
        let mut state = loaded.default_state.clone();
        let run = crate::card::run_hook_full(
            &loaded.source,
            crate::card::HookCall::OnLoad,
            &crate::card::HookEnv { state: state.clone(), ..Default::default() },
            meta.seed,
            &sink,
        );
        println!("PROBE on_load ran={} logs={:?}", run.ran(), run.result.logs);
        save_state(&root, &meta.id, &state).unwrap();

        for (turn, text) in [(1u64, "你好"), (2u64, "谢谢")] {
            log.append(
                &root,
                &meta.id,
                &Message { turn, role: "user".into(), content: text.into(), ts: 0, scene_id: None, name: None, tool_calls: None },
            )
            .unwrap();
            let msgs = read_messages(&root, &meta.id).unwrap();
            let mut st = load_state(&root, &meta.id).unwrap();
            let bb = load_blackboard(&root, &meta.id).unwrap();
            let mut env = std::collections::BTreeMap::new();
            env.insert("place".to_string(), serde_json::json!(bb.place));
            let r = crate::card::run_hook_full(
                &loaded.source,
                crate::card::HookCall::OnMessage { msg: msgs.last().unwrap() },
                &crate::card::HookEnv { state: st.clone(), blackboard: env, memory: Default::default(), turn: msgs.last().unwrap().turn, mirror: Default::default() },
                meta.seed,
                &sink,
            );
            println!(
                "PROBE turn{turn} text={text:?} ran={} logs={:?} state_before={} state_after={} memory={:?}",
                r.ran(),
                r.result.logs,
                st,
                r.state.clone().unwrap_or(serde_json::Value::Null),
                r.memory
            );
            // 诊断通道：钩子执行必须留痕（真机排查靠它）
            crate::diag::record(
                "hook",
                format!("turn={turn} ran={} state={:?}", r.ran(), r.state),
            );
            if let Some(next) = r.state.clone() {
                if next != st {
                    save_state(&root, &meta.id, &next).unwrap();
                }
            }
            st = load_state(&root, &meta.id).unwrap();
            println!("PROBE turn{turn} state.json = {st}");
        }
        let diag = crate::diag::recent(5);
        println!("PROBE 诊断条数={} 最新={:?}", diag.len(), diag.first().map(|d| &d.detail));
        assert!(diag.iter().any(|d| d.detail.contains("turn=2")));
    }

    #[test]
    fn new_session_layout_and_append() {
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();

        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: Some("夜读者".into()),
                day: Some(3),
                clock: Some("21:30".into()),
                place: Some("图书馆自习区".into()),
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let dir = session_dir(root.path(), &meta.id);
        assert!(dir.join("session.json").is_file());
        assert!(dir.join("messages.jsonl").is_file());
        assert!(dir.join("state.json").is_file());
        assert!(dir.join("blackboard.json").is_file());
        assert_eq!(meta.characters, vec!["小雨"]);

        let bb: Blackboard =
            serde_json::from_str(&std::fs::read_to_string(dir.join("blackboard.json")).unwrap())
                .unwrap();
        assert_eq!((bb.day, bb.clock.as_str(), bb.place.as_str()), (3, "21:30", "图书馆自习区"));
        assert_eq!(bb.actors, vec!["小雨"]);

        let log = EventLog::new();
        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 1,
                role: "user".into(),
                content: "今天好冷。".into(),
                ts: 1_758_000_000,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();
        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 1,
                role: "char".into(),
                content: "……嗯。".into(),
                ts: 1_758_000_020,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();
        let msgs = read_messages(root.path(), &meta.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "今天好冷。");

        // 会话列表可枚举；id 唯一
        let sessions = list_sessions(root.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        let another = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        assert_ne!(meta.id, another.id);
        assert!(another.id.starts_with("20"));

        // 不存在的会话追加报错而非 panic
        assert!(log.append(root.path(), "no-such", &msgs[0]).is_err());
    }

    #[test]
    fn blackboard_and_state_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        // 初始：默认黑板 + 空 state
        let bb = load_blackboard(root.path(), &meta.id).unwrap();
        assert_eq!((bb.day, bb.clock.as_str()), (1, ""));
        assert_eq!(load_state(root.path(), &meta.id).unwrap(), serde_json::json!({}));

        let bb = Blackboard {
            day: 7,
            clock: "23:05".into(),
            place: "天台".into(),
            actors: vec!["小雨".into(), "玩家".into()],
            extra: Default::default(),
        };
        save_blackboard(root.path(), &meta.id, &bb).unwrap();
        assert_eq!(load_blackboard(root.path(), &meta.id).unwrap().place, "天台");

        let st = serde_json::json!({ "favorability": 61 });
        save_state(root.path(), &meta.id, &st).unwrap();
        assert_eq!(load_state(root.path(), &meta.id).unwrap()["favorability"], 61);

        // 不存在的会话报错
        assert!(load_blackboard(root.path(), "no-such").is_err());
        assert!(save_state(root.path(), "no-such", &st).is_err());
    }

    #[test]
    fn env_override_wins_and_fallback_is_user_writable() {
        // 环境变量优先（便携/多套数据）
        std::env::set_var("HUAJING_DATA", "J:/tmp/huajing-test-data");
        assert_eq!(data_root(), PathBuf::from("J:/tmp/huajing-test-data"));
        std::env::remove_var("HUAJING_DATA");

        // 兜底目录必须在用户数据目录下（安装版不能在 Program Files 里写盘）
        let fallback = user_data_root();
        assert!(fallback.ends_with("DataHub"), "{fallback:?}");
        let host = std::env::var_os("APPDATA").map(PathBuf::from);
        if let Some(appdata) = host {
            assert!(fallback.starts_with(&appdata), "兜底应落在 %APPDATA% 下：{fallback:?}");
        }
    }

    #[test]
    fn time_helpers() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(946_684_800), "2000-01-01T00:00:00Z");
        // 闰日：2024-03-01 前一天是 2024-02-29（1709164800 = 2024-02-29T00:00:00Z）
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    /// 读事件流里的消息视图（测试里比 records 本身更常用）
    fn read_log_messages(log: &EventLog, root: &Path, id: &str) -> Vec<Message> {
        crate::event::messages(&log.read(root, id).unwrap())
    }

    #[test]
    fn message_log_incremental_reads() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();

        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 1,
                role: "user".into(),
                content: "你好".into(),
                ts: 1,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();
        let a = log.read(root.path(), &meta.id).unwrap();
        assert_eq!(a.len(), 1);
        // 无新数据：复用缓存（同一份 Arc，零重解析）
        let b = log.read(root.path(), &meta.id).unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &b));

        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 1,
                role: "char".into(),
                content: "……嗯。".into(),
                ts: 2,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();
        let c = log.read(root.path(), &meta.id).unwrap();
        assert_eq!(c.len(), 2);
        assert!(!std::sync::Arc::ptr_eq(&a, &c));

        // 与直接读文件的自由函数结果一致
        let raw = read_messages(root.path(), &meta.id).unwrap();
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[1].content, "……嗯。");
        assert_eq!(read_log_messages(&log, root.path(), &meta.id).len(), 2);
    }

    #[test]
    fn message_log_partial_line_waits_for_newline() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 1,
                role: "user".into(),
                content: "第一条".into(),
                ts: 1,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();

        // 模拟崩溃：直接写入半行 JSON（无换行）
        let path = session_dir(root.path(), &meta.id).join("messages.jsonl");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(r#"{"turn":2,"role":"user","content":"第"#.as_bytes())
            .unwrap();
        assert_eq!(log.read(root.path(), &meta.id).unwrap().len(), 1, "半行不消费");

        // 补全换行后整行出现
        f.write_all(r#"二条","ts":2}"#.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        drop(f);
        assert_eq!(log.read(root.path(), &meta.id).unwrap().len(), 2);
    }

    #[test]
    fn message_log_external_truncation_resets_cache() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        for i in 0..3 {
            log.append(
                root.path(),
                &meta.id,
                &Message {
                name: None,
                    turn: i + 1,
                    role: "user".into(),
                    content: format!("m{i}"),
                    ts: i as u64,
                    scene_id: None,
                tool_calls: None,
                }
                    ,
            )
            .unwrap();
        }
        assert_eq!(log.read(root.path(), &meta.id).unwrap().len(), 3);

        // 外部重写文件（消息编辑场景的简化版）
        let path = session_dir(root.path(), &meta.id).join("messages.jsonl");
        std::fs::write(
            &path,
            "{\"turn\":1,\"role\":\"user\",\"content\":\"edited\",\"ts\":9}\n",
        )
        .unwrap();
        let msgs = read_log_messages(&log, root.path(), &meta.id);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "edited");
    }

    #[test]
    fn write_messages_rewrites_and_invalidates() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        for i in 0..3 {
            log.append(
                root.path(),
                &meta.id,
                &Message {
                name: None,
                    turn: 1,
                    role: "user".into(),
                    content: format!("原文{i}"),
                    ts: i as u64,
                    scene_id: None,
                tool_calls: None,
                }
                    ,
            )
            .unwrap();
        }

        // 编辑：等长改写（缓存偏移检测不到，必须 invalidate）
        let mut msgs = read_log_messages(&log, root.path(), &meta.id);
        msgs[1].content = "改写X".into(); // 与"原文1"字节数相同（均 7 字节）的等长改写
        assert_eq!(msgs[1].content.len(), "原文1".len());
        write_messages(root.path(), &meta.id, &msgs).unwrap();
        log.invalidate(Some(&meta.id));
        let after = read_log_messages(&log, root.path(), &meta.id);
        assert_eq!(after[1].content, "改写X");
        assert_eq!(after.len(), 3);

        // 删除：移除末尾后读回 2 条；文件变短走自动重置也行，但统一 invalidate
        msgs = after.clone();
        msgs.pop();
        write_messages(root.path(), &meta.id, &msgs).unwrap();
        log.invalidate(Some(&meta.id));
        assert_eq!(read_log_messages(&log, root.path(), &meta.id).len(), 2);

        // 事件流整体重写（消息编辑/删除的实际路径）：序号重排、缓存换新
        let mut records = log.read(root.path(), &meta.id).unwrap().as_ref().clone();
        records.push(crate::event::LogRecord::new(
            0,
            crate::event::LogBody::Effect(crate::event::EffectEvent {
                turn: 1,
                trigger: "hook.on_message".into(),
                character: "小雨".into(),
                state_set: vec![],
                blackboard: vec![],
                memory: vec![],
                scene_id: None,
                ts: 0,
            }),
        ));
        log.rewrite(root.path(), &meta.id, &records).unwrap();
        let rewritten = log.read(root.path(), &meta.id).unwrap();
        assert_eq!(rewritten.len(), 3);
        assert_eq!(rewritten.last().unwrap().seq, 3, "重写后序号按位置重排");

        // 重写后继续 append 不串行
        log.append(
            root.path(),
            &meta.id,
            &Message {
            name: None,
                turn: 2,
                role: "char".into(),
                content: "新回复".into(),
                ts: 9,
                scene_id: None,
            tool_calls: None,
            }
                ,
        )
        .unwrap();
        let final_msgs = read_messages(root.path(), &meta.id).unwrap();
        assert_eq!(final_msgs.len(), 3);
        assert_eq!(final_msgs[2].content, "新回复");

        // 不存在的会话报错
        assert!(write_messages(root.path(), "no-such", &msgs).is_err());
    }

    #[test]
    fn message_log_survives_high_volume() {
        let root = tempfile::tempdir().unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
            characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        let n = 10_000;
        for i in 0..n {
            log.append(
                root.path(),
                &meta.id,
                &Message {
                name: None,
                    turn: i as u64 / 2 + 1,
                    role: if i % 2 == 0 { "user" } else { "char" }.into(),
                    content: format!("消息正文 {:064}", i), // ~100B/行
                    ts: i as u64,
                    scene_id: None,
                tool_calls: None,
                }
                    ,
            )
            .unwrap();
        }
        let started = std::time::Instant::now();
        let msgs = log.read(root.path(), &meta.id).unwrap();
        assert_eq!(msgs.len(), n);
        // 增量读取：无新增时 read 只做一次 metadata 检查
        assert!(started.elapsed().as_millis() < 500, "读取过慢");
        // 新建缓存实例的全量重建也能工作（invalidate 后等价路径）
        log.invalidate(Some(&meta.id));
        assert_eq!(log.read(root.path(), &meta.id).unwrap().len(), n);
    }

    fn msg(turn: u64, content: &str) -> Message {
        Message {
            name: None,
            turn,
            role: "user".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        tool_calls: None,
        }
            
    }

    #[test]
    fn rewrite_is_atomic_and_leaves_no_tmp_residue() {
        // A2：messages.jsonl 是唯一事实来源，rewrite 走临时文件 + rename，
        // 崩溃最坏留 .tmp 残骸，目标文件绝不半截
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
                characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let log = EventLog::new();
        let r1 = log.append(root.path(), &meta.id, msg(1, "第一条")).unwrap();
        let kept = vec![r1];
        log.rewrite(root.path(), &meta.id, &kept).unwrap();

        let path = session_dir(root.path(), &meta.id).join("messages.jsonl");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 1, "rewrite 后只剩 kept 的那一行");
        assert_eq!(
            crate::event::messages(&log.read(root.path(), &meta.id).unwrap()).len(),
            1
        );
        let residue: Vec<_> = std::fs::read_dir(session_dir(root.path(), &meta.id))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(residue.is_empty(), "不允许 .tmp 残留：{residue:?}");

        // 同款修法的三个小落盘点：写后无 .tmp、内容完整
        save_world(
            root.path(),
            "w1",
            &crate::worldline::WorldState {
                day: 7,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            load_world(root.path(), "w1").day, 7,
            "save_world 原子写后可读回"
        );
        save_grown(root.path(), "w1", &GrownFile::default()).unwrap();
        assert!(load_grown(root.path(), "w1").entities.is_empty());
        save_providers(root.path(), &[sample_provider("p", "chat")]).unwrap();
        assert_eq!(load_providers(root.path()).unwrap().len(), 1);
        let strays: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn append_seals_torn_tail_line_instead_of_gluing() {
        // A3：上次崩溃残留「末行无换行的半截 JSON」时，append 先封口再写——
        // 新消息要能完整读回，坏行被读侧跳过，缓存偏移不错位
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
                characters: Vec::new(),
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        let dir = session_dir(root.path(), &meta.id);
        let log = EventLog::new();
        log.append(root.path(), &meta.id, msg(1, "完整的一条")).unwrap();

        // 模拟崩溃残留：抹掉事件流，手写一行完整记录 + 一行无换行的半截 JSON
        log.invalidate(Some(&meta.id));
        let good_line = crate::event::LogRecord::message(1, msg(1, "完整的一条"))
            .to_line()
            .unwrap();
        let full_line = crate::event::LogRecord::message(2, msg(1, "torn-half-line"))
            .to_line()
            .unwrap();
        let torn = &full_line[..full_line.len() / 2]; // 无换行的半截（ASCII 内容，字节切安全）
        std::fs::write(dir.join("messages.jsonl"), format!("{good_line}{torn}")).unwrap();

        let record = log
            .append(root.path(), &meta.id, msg(2, "崩溃后新消息"))
            .unwrap();
        assert_eq!(record.seq, 2, "坏行不计入序号");

        // 缓存与直读文件两条路径都要一致：新消息完整、半行被跳过
        let cached = crate::event::messages(&log.read(root.path(), &meta.id).unwrap());
        log.invalidate(Some(&meta.id));
        let reread = read_messages(root.path(), &meta.id).unwrap();
        assert_eq!(cached.len(), 2, "{cached:?}");
        assert_eq!(reread.len(), 2, "{reread:?}");
        assert_eq!(cached[1].content, "崩溃后新消息");
        assert_eq!(reread[1].content, "崩溃后新消息");
        assert!(
            !cached[1].content.contains("断电") && !reread[0].content.contains("断电"),
            "半行不得粘连进任何一条消息"
        );
    }

    #[test]
    fn session_id_skips_existing_directory_on_collision() {
        // A7：同毫秒两次建会话同 ID 会互覆目录——已占用就递增毫秒尾数重滚
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();
        let secs: u64 = 1_790_000_000; // 固定秒，测毫秒碰撞路径
        let first = unique_session_id_at(root.path(), secs, 483);
        std::fs::create_dir_all(session_dir(root.path(), &first)).unwrap();
        let second = unique_session_id_at(root.path(), secs, 483);
        assert_ne!(first, second, "同毫秒第二次要避开已存在目录");
        assert!(second.ends_with("-484"), "顺延毫秒尾数：{second}");
        // 空目录无碰撞：原样返回
        assert_eq!(unique_session_id_at(root.path(), secs, 700), session_id_at(secs, 700));
    }

    #[test]
    fn new_session_cast_dedups_non_adjacent_duplicates() {
        // B2：`["b","a","b"]` 曾让同一角色每轮跑两次钩子、双份记忆写入
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();
        let meta = new_session(
            root.path(),
            &NewSessionRequest {
                characters: vec!["b".into(), "a".into(), "b".into(), "a".into(), "b".into()],
                character: "b".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        assert_eq!(meta.characters, vec!["b", "a"], "保序去重：{meta:?}");
        let bb = load_blackboard(root.path(), &meta.id).unwrap();
        assert_eq!(bb.actors, vec!["b", "a"], "黑板 actors 同样无重复");
    }

    #[test]
    fn settings_roundtrip_defaults_and_updater_block() {
        // 缺省 + 缺区兼容：settings.toml 里没有 [updater] / [consolidate] 也不炸
        let mut s = Settings::default();
        s.wizard_done = true;
        let raw = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&raw).unwrap();
        assert_eq!(back, s);
        let mut legacy = Settings::default();
        legacy.wizard_done = true;
        let back: Settings = toml::from_str("wizard_done = true").unwrap();
        assert_eq!(back, legacy, "单字段文件：其余字段全走缺省");

        // [updater] 区（M4.2）：端点 + 开关往返
        let mut s = Settings::default();
        s.updater.endpoint = Some("http://127.0.0.1:8452/latest.json".into());
        s.updater.enabled = true;
        let raw = toml::to_string_pretty(&s).unwrap();
        assert!(raw.contains("[updater]") && raw.contains("enabled = true"));
        let back: Settings = toml::from_str(&raw).unwrap();
        assert_eq!(back.updater.endpoint.as_deref(), Some("http://127.0.0.1:8452/latest.json"));
        assert!(back.updater.enabled);

        // [consolidate] 区（M4.4）：阈值 / 自动 / 空闲分钟数往返，缺省自动关
        let mut s = Settings::default();
        assert!(!s.consolidate.auto, "自动整理缺省关");
        assert!((s.consolidate.threshold - 0.25).abs() < 1e-9, "阈值缺省 0.25");
        assert_eq!(s.consolidate.idle_minutes, 30);
        s.consolidate.auto = true;
        s.consolidate.threshold = 0.15;
        s.consolidate.idle_minutes = 45;
        let raw = toml::to_string_pretty(&s).unwrap();
        assert!(raw.contains("[consolidate]") && raw.contains("auto = true"));
        let back: Settings = toml::from_str(&raw).unwrap();
        assert!(back.consolidate.auto);
        assert!((back.consolidate.threshold - 0.15).abs() < 1e-9);
        assert_eq!(back.consolidate.idle_minutes, 45);
    }

    #[test]
    fn input_budget_clamps_absurd_context_window() {
        // A7：手填超大窗口曾让 usize 乘 75 在 debug 构建直接溢出 panic
        let mut s = Settings::default();
        assert_eq!(s.input_budget(), 32768 * 75 / 100, "缺省行为不变");
        s.context_window = Some(usize::MAX);
        assert_eq!(s.input_budget(), 10_000_000 * 75 / 100, "超大值钳到上限");
        s.context_window = Some(0);
        assert_eq!(s.input_budget(), 0, "0 钳到 1 后预算归零但不 panic");
        s.context_window = Some(8192);
        assert_eq!(s.input_budget(), 8192 * 75 / 100, "正常值不受影响");
    }

    // ---------- themes/（M4.3 · 决断 7） ----------

    fn sample_theme(name: &str) -> CustomTheme {
        CustomTheme {
            preset: name.into(),
            color_scheme: "dark".into(),
            vars: BTreeMap::from([
                ("--color-primary".to_string(), "oklch(55% 0.2 20)".to_string()),
                ("--radius-box".to_string(), "1rem".to_string()),
            ]),
        }
    }

    #[test]
    fn themes_roundtrip_list_and_delete() {
        let root = tempfile::tempdir().unwrap();
        assert!(list_themes(root.path()).unwrap().is_empty());
        assert!(load_theme(root.path(), THEME_ACTIVE_STEM).unwrap().is_none(), "空库读活动主题 = None（不是错误）");

        save_theme(root.path(), THEME_ACTIVE_STEM, &sample_theme("当前")).unwrap();
        save_theme(root.path(), "cupcake 改", &sample_theme("cupcake 改")).unwrap();
        // 覆盖同名而非堆文件
        let mut edited = sample_theme("cupcake 改");
        edited.vars.insert("--color-base-100".into(), "#111".into());
        save_theme(root.path(), "cupcake 改", &edited).unwrap();

        assert_eq!(list_themes(root.path()).unwrap(), vec!["cupcake 改", "custom"]);
        let loaded = load_theme(root.path(), THEME_ACTIVE_STEM).unwrap().unwrap();
        assert_eq!(loaded, sample_theme("当前"));
        assert_eq!(
            load_theme(root.path(), "cupcake 改").unwrap().unwrap().vars.get("--color-base-100").map(String::as_str),
            Some("#111")
        );

        assert!(delete_theme(root.path(), THEME_ACTIVE_STEM).unwrap());
        assert!(!delete_theme(root.path(), THEME_ACTIVE_STEM).unwrap(), "重复删除幂等报 false");
        assert_eq!(list_themes(root.path()).unwrap(), vec!["cupcake 改"]);
    }

    #[test]
    fn theme_name_sanitized_into_safe_stem() {
        let root = tempfile::tempdir().unwrap();
        // 路径分隔与上跳片段不能构成目录穿越；落盘文件落在 themes/ 平面一层
        let stem = save_theme(root.path(), "a/b\\c..d:e", &sample_theme("x")).unwrap();
        assert!(!stem.contains('/') && !stem.contains('\\') && !stem.contains(".."));
        assert!(root.path().join("themes").join(format!("{stem}.json")).is_file());
        assert_eq!(list_themes(root.path()).unwrap(), vec![stem]);
    }

    #[test]
    fn themes_survive_datahub_copy() {
        // DoD「DataHub 拷贝主题跟走」读侧：themes/ 目录原样拷到新数据根后读回一致
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        save_theme(src.path(), THEME_ACTIVE_STEM, &sample_theme("当前")).unwrap();
        save_theme(src.path(), "library", &sample_theme("library")).unwrap();

        // themes/ 目录是平面一层 *.json，逐文件拷贝（copy_dir_all 尚在 stable 之外）
        let dst_themes = dst.path().join("themes");
        std::fs::create_dir_all(&dst_themes).unwrap();
        for entry in std::fs::read_dir(src.path().join("themes")).unwrap().flatten() {
            std::fs::copy(entry.path(), dst_themes.join(entry.file_name())).unwrap();
        }
        assert_eq!(list_themes(dst.path()).unwrap(), vec!["custom", "library"]);
        assert_eq!(load_theme(dst.path(), THEME_ACTIVE_STEM).unwrap(), Some(sample_theme("当前")));
        // 字节级一致：同主题多次落盘内容稳定（BTreeMap 顺序确定）
        let first = std::fs::read(src.path().join("themes").join("custom.json")).unwrap();
        let second = std::fs::read(dst.path().join("themes").join("custom.json")).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn theme_import_parses_css_block() {
        let allowed = vec!["--color-primary".to_string(), "--radius-box".to_string()];
        let css = r#"@plugin "daisyui/theme" {
  name: "my theme";
  default: false;
  prefersdark: false;
  color-scheme: dark;
  --color-primary: oklch(55% 0.2 20);
  --radius-box: 1rem;
}"#;
        let parsed = parse_theme_import(css, &allowed).unwrap();
        assert_eq!(parsed.name, "my theme");
        assert_eq!(parsed.color_scheme, "dark");
        assert_eq!(parsed.vars.get("--color-primary").map(String::as_str), Some("oklch(55% 0.2 20)"));
        assert_eq!(parsed.vars.get("--radius-box").map(String::as_str), Some("1rem"));
        // 结构键（default/prefersdark）不是令牌，忽略不算非法
    }

    #[test]
    fn theme_import_parses_json_payload() {
        let allowed = vec!["--color-primary".to_string()];
        let parsed = parse_theme_import(
            r##"{"preset":"库里的","colorScheme":"dark","vars":{"--color-primary":"#123456"}}"##,
            &allowed,
        )
        .unwrap();
        assert_eq!(parsed.name, "库里的");
        assert_eq!(parsed.color_scheme, "dark");
        assert_eq!(parsed.vars.get("--color-primary").map(String::as_str), Some("#123456"));
    }

    #[test]
    fn theme_import_rejects_illegal_keys_and_lists_them() {
        let allowed = vec!["--color-primary".to_string(), "--color-accent".to_string()];
        let css = r#"@plugin "daisyui/theme" {
  name: "bad";
  color-scheme: light;
  --color-primary: #fff;
  --color-nope: #000;
  --radius-unknown: 2px;
}"#;
        let err = parse_theme_import(css, &allowed).unwrap_err();
        assert!(err.contains("--color-nope") && err.contains("--radius-unknown"), "列出全部非法键：{err}");
        assert!(!err.contains("--color-primary"), "合法键不在报错里：{err}");

        let err = parse_theme_import(r#"{"vars":{"--bogus":"x"}}"#, &allowed).unwrap_err();
        assert!(err.contains("--bogus"), "JSON 分支同样拒绝：{err}");

        assert!(parse_theme_import("", &allowed).is_err(), "空内容拒绝");
        assert!(parse_theme_import("body { color: red; }", &allowed).is_err(), "无 @plugin 块拒绝");
        assert!(
            parse_theme_import("@plugin \"daisyui/theme\" { name: \"x\"; }", &allowed).is_err(),
            "没有可用键值对拒绝"
        );
        // name/color-scheme 缺省兜底
        let parsed = parse_theme_import(
            "@plugin \"daisyui/theme\" { --color-primary: #fff; }",
            &allowed,
        )
        .unwrap();
        assert_eq!(parsed.name, "custom");
        assert_eq!(parsed.color_scheme, "light");
    }

    /// M5.3：new_session 落盘启用的世界——给了且读得到就用（round-trip 回读一致），
    /// 没给就 None（读侧回落 default，老会话语义不变）
    #[test]
    fn new_session_persists_world_selection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_layout(root).unwrap();
        std::fs::create_dir_all(root.join("codex/魔女之城/entities")).unwrap();

        let meta = new_session(
            root,
            &NewSessionRequest {
                character: "小雨".into(),
                characters: Vec::new(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: Some("魔女之城".into()),
            },
        )
        .unwrap();
        assert_eq!(meta.world.as_deref(), Some("魔女之城"));
        // round-trip：重新读回 session.json 仍是 Some
        let mut metas = list_sessions(root).unwrap();
        metas.retain(|m| m.id == meta.id);
        assert_eq!(metas[0].world.as_deref(), Some("魔女之城"));

        let none = new_session(
            root,
            &NewSessionRequest {
                character: "小雨".into(),
                characters: Vec::new(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: None,
                world: None,
            },
        )
        .unwrap();
        assert_eq!(none.world, None, "不指定 = None，读侧回落 default");
    }
}
