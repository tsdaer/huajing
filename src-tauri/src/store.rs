//! DataHub 明文数据层（设计 §12）：一切用户数据为本地明文文件。
//!
//! M1.1 范围：providers / settings / personas 读写；会话目录骨架与
//! messages.jsonl 追加式落盘（可回放）。卡片加载与热加载见 card.rs（M1.2）。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::llm::Provider;

// ---------- 错误 ----------

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    NotFound(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "IO 错误：{}", e),
            StoreError::Json(e) => write!(f, "JSON 解析错误：{}", e),
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

pub type StoreResult<T> = Result<T, StoreError>;

// ---------- 目录 ----------

/// DataHub 目录解析：HUAJING_DATA 环境变量 > 可执行文件旁 DataHub >
/// 从 cwd 向上找（覆盖 `tauri dev` 时 cwd=src-tauri 的情况）> 当前目录 DataHub
pub fn data_root() -> PathBuf {
    if let Ok(p) = std::env::var("HUAJING_DATA") {
        return PathBuf::from(p);
    }
    let exe_adjacent = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("DataHub")))
        .filter(|p| p.is_dir());
    match exe_adjacent {
        Some(p) => p,
        None => std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.ancestors().map(|d| d.join("DataHub")).find(|p| p.is_dir()))
            .unwrap_or_else(|| PathBuf::from("DataHub")),
    }
}

/// 确保数据目录骨架存在（设计 §12 目录树）
pub fn ensure_layout(root: &Path) -> std::io::Result<()> {
    for d in ["personas", "characters", "codex", "sessions"] {
        std::fs::create_dir_all(root.join(d))?;
    }
    Ok(())
}

// ---------- providers.json（设计 §11：LLM 接入点）----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<Provider>,
}

pub fn providers_path(root: &Path) -> PathBuf {
    root.join("providers.json")
}

pub fn load_providers(root: &Path) -> StoreResult<Vec<Provider>> {
    let path = providers_path(root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str::<ProvidersFile>(&raw)?.providers)
}

pub fn save_providers(root: &Path, providers: &[Provider]) -> StoreResult<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_string_pretty(&ProvidersFile {
        providers: providers.to_vec(),
    })?;
    std::fs::write(providers_path(root), json + "\n")?;
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

// ---------- settings.json（界面与全局配置）----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_narrative_mode")]
    pub narrative_mode: String,
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
        }
    }
}

pub fn load_settings(root: &Path) -> StoreResult<Settings> {
    let path = root.join("settings.json");
    if !path.exists() {
        return Ok(Settings::default());
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
}

pub fn save_settings(root: &Path, settings: &Settings) -> StoreResult<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(root.join("settings.json"), json + "\n")?;
    Ok(())
}

// ---------- personas/（用户人格）----------

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
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(p) = serde_json::from_str::<Persona>(&raw) {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// ---------- sessions/（设计 §12：session.json + messages.jsonl + state.json + blackboard.json）----------

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
}

/// messages.jsonl 中的一行（追加式消息流；事件类条目后续里程碑再加）
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// 黑板 v0（M1.4 补 UI 编辑与时钟步进；M1.1 仅初始落盘）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blackboard {
    pub day: i64,
    pub clock: String,
    pub place: String,
    pub actors: Vec<String>,
}

pub struct NewSessionRequest {
    pub character: String,
    pub persona: Option<String>,
    pub day: Option<i64>,
    pub clock: Option<String>,
    pub place: Option<String>,
    pub premise: Option<String>,
}

pub fn session_dir(root: &Path, id: &str) -> PathBuf {
    root.join("sessions").join(id)
}

/// 创建会话目录骨架并写入初始文件，返回元数据
pub fn new_session(root: &Path, req: &NewSessionRequest) -> StoreResult<SessionMeta> {
    ensure_layout(root)?;
    let now_secs = unix_now();
    let meta = SessionMeta {
        id: session_id(now_secs),
        created_at: iso8601(now_secs),
        characters: vec![req.character.clone()],
        persona: req.persona.clone(),
        world: None,
        seed: seed_now(),
        premise: req.premise.clone(),
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
    let blackboard = Blackboard {
        day: req.day.unwrap_or(1),
        clock: req.clock.clone().unwrap_or_default(),
        place: req.place.clone().unwrap_or_default(),
        actors: vec![req.character.clone()],
    };
    std::fs::write(
        dir.join("blackboard.json"),
        serde_json::to_string_pretty(&blackboard)? + "\n",
    )?;
    Ok(meta)
}

/// 追加一行消息（messages.jsonl 为追加式日志：可回放、可恢复）
pub fn append_message(root: &Path, session_id: &str, msg: &Message) -> StoreResult<()> {
    use std::io::Write;
    let dir = session_dir(root, session_id);
    if !dir.is_dir() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("messages.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(msg)?)?;
    Ok(())
}

/// 读取全量消息（坏行跳过：追加式日志的容错读取）
pub fn read_messages(root: &Path, session_id: &str) -> StoreResult<Vec<Message>> {
    let path = session_dir(root, session_id).join("messages.jsonl");
    if !path.exists() {
        return Err(StoreError::NotFound(format!("会话「{}」", session_id)));
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
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

// ---------- 时间工具（不引入时间库；Howard Hinnant civil 算法）----------

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn seed_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 ^ (d.as_secs() << 32))
        .unwrap_or(0)
}

/// 会话 id：`20260919-180102-483`（本地无关的 UTC，毫秒尾数防同秒碰撞）
fn session_id(secs: u64) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
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
            root.path().join("personas/b.json"),
            r#"{"name":"beta","description":"夜读者"}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("personas/a.json"),
            r#"{"name":"alpha"}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("personas/bad.json"), "{not json").unwrap();

        let list = list_personas(root.path()).unwrap();
        assert_eq!(
            list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert_eq!(list[0].description, "");
        assert_eq!(list[1].description, "夜读者");
    }

    #[test]
    fn new_session_layout_and_append() {
        let root = tempfile::tempdir().unwrap();
        ensure_layout(root.path()).unwrap();

        let meta = new_session(
            root.path(),
            &NewSessionRequest {
                character: "小雨".into(),
                persona: Some("夜读者".into()),
                day: Some(3),
                clock: Some("21:30".into()),
                place: Some("图书馆自习区".into()),
                premise: None,
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

        append_message(
            root.path(),
            &meta.id,
            &Message {
                turn: 1,
                role: "user".into(),
                content: "今天好冷。".into(),
                ts: 1_758_000_000,
                scene_id: None,
            },
        )
        .unwrap();
        append_message(
            root.path(),
            &meta.id,
            &Message {
                turn: 1,
                role: "char".into(),
                content: "……嗯。".into(),
                ts: 1_758_000_020,
                scene_id: None,
            },
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
                character: "小雨".into(),
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
            },
        )
        .unwrap();
        assert_ne!(meta.id, another.id);
        assert!(another.id.starts_with("20"));

        // 不存在的会话追加报错而非 panic
        assert!(append_message(root.path(), "no-such", &msgs[0]).is_err());
    }

    #[test]
    fn time_helpers() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(946_684_800), "2000-01-01T00:00:00Z");
        // 闰日：2024-03-01 前一天是 2024-02-29（1709164800 = 2024-02-29T00:00:00Z）
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }
}
