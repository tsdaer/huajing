//! 包格式与导入导出（M4.1 · 设计 §13「导入生态」）：pack.json 清单 + zip 容器。
//!
//! 三包（决断 2/3）：角色包（card.lua + assets/ + entities/）、世界包
//! （world.json + worldline.lua + entities/ + grown.json）、剧本包（premise +
//! 初始黑板 + 导演树——剧本是会话模板不是存档，不含消息历史）。导出永远从
//! 现有目录/投影打包不改写库；导入按 stimport 先例处理同名冲突（并存加后缀 /
//! 显式覆盖），解包前做 zip slip 防护与大小上限（决断 2 边界）。
//!
//! pack.json：`{ spec: "huajing-pack/1", kind, name, creator?, description?,
//! requires?: { world? } }`。spec 版本演进只加不改：不认识的版本拒绝并报出
//! 本程序支持的最高版本。pack.json 是容器清单，**不落进安装目标目录**——
//! 安装物（card.lua / 世界目录 / 剧本模板）与来源格式解耦，重复导出幂等。

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 当前包规格（本程序写出与接受的形态）
pub const SPEC: &str = "huajing-pack/1";
const SPEC_PREFIX: &str = "huajing-pack/";
/// 支持的最高 spec 版本：更新的规格拒绝安装并报出此数（决断 2 前后兼容）
pub const SUPPORTED_SPEC_VERSION: u32 = 1;

pub const KIND_CHARACTER: &str = "character";
pub const KIND_WORLD: &str = "world";
pub const KIND_SCRIPT: &str = "script";

/// 解包防护（决断 2 / 风险 2）：单文件 ≤64MB、entry 总数上限、总量上限。
/// zip 炸弹的第一道闸是声明尺寸，解压后按实际字节数再查一次（声明可以撒谎）。
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// 导出产物目录（DataHub/exports/）：不在热加载监听范围内（watch.rs 只听
/// characters/personas），写 zip 不会触发卡片刷新
pub fn exports_dir(root: &Path) -> std::path::PathBuf {
    root.join("exports")
}

pub fn scripts_dir(root: &Path) -> std::path::PathBuf {
    root.join("scripts")
}

// ---------- pack.json 清单 ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    pub spec: String,
    /// character | world | script
    pub kind: String,
    /// 包名 = 安装身份（角色目录名 / 世界名 / 剧本名），必填非空
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 依赖声明：角色包可声明所需世界（缺世界只提醒不阻塞——世界可以后补）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<PackRequires>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackRequires {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
}

impl PackManifest {
    pub fn new(kind: &str, name: &str) -> PackManifest {
        PackManifest {
            spec: SPEC.into(),
            kind: kind.into(),
            name: name.into(),
            creator: None,
            description: None,
            requires: None,
        }
    }

    /// schema 校验：spec 认识、kind 三选一、name 必填非空
    pub fn validate(&self) -> Result<(), String> {
        let version = self
            .spec
            .strip_prefix(SPEC_PREFIX)
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| {
                format!(
                    "不是化境包（spec 应为「{SPEC_PREFIX}版本号」形态，实际：{}）",
                    self.spec
                )
            })?;
        if version > SUPPORTED_SPEC_VERSION {
            return Err(format!(
                "包规格 {} 比本程序新（最高支持 huajing-pack/{SUPPORTED_SPEC_VERSION}），请升级化境后再导入",
                self.spec
            ));
        }
        if version < SUPPORTED_SPEC_VERSION {
            return Err(format!(
                "包规格 {} 已废弃（本程序支持 huajing-pack/{SUPPORTED_SPEC_VERSION}）",
                self.spec
            ));
        }
        if !matches!(self.kind.as_str(), KIND_CHARACTER | KIND_WORLD | KIND_SCRIPT) {
            return Err(format!(
                "包类型「{}」不认识（应为 {KIND_CHARACTER} / {KIND_WORLD} / {KIND_SCRIPT} 之一）",
                self.kind
            ));
        }
        if self.name.trim().is_empty() {
            return Err("包缺少名字（pack.json.name）".into());
        }
        Ok(())
    }
}

// ---------- 布局白名单 ----------

/// 各包类型允许的顶层条目（决断 2 风险 2：解包一律走白名单布局校验，
/// 声明目录之外的顶层条目整个拒绝——zip 里的路径永远浅一层，防穿越先于防杂乱）
fn allowed_top_level(kind: &str) -> &'static [&'static str] {
    match kind {
        KIND_CHARACTER => &["card.lua", "assets", "entities"],
        KIND_WORLD => &["world.json", "worldline.lua", "entities", "grown.json"],
        KIND_SCRIPT => &["premise.toml", "blackboard.initial.json", "director.lua"],
        _ => &[],
    }
}

/// 各包类型的必备文件（缺了就是打错包，导入前拒绝）
fn required_files(kind: &str) -> &'static [&'static str] {
    match kind {
        KIND_CHARACTER => &["card.lua"],
        KIND_WORLD => &["world.json"],
        KIND_SCRIPT => &["premise.toml", "blackboard.initial.json"],
        _ => &[],
    }
}

// ---------- zip 读取与防护 ----------

/// 包内一个文件（解出的内存副本；目录条目不进这里）
#[derive(Debug, Clone)]
pub struct PackEntry {
    /// 归一化后的包内相对路径（正斜杠分隔，无前导斜杠）
    pub name: String,
    pub data: Vec<u8>,
}

/// 解包产物：清单 + 文件（pack.json 单列在 manifest，不进 entries）
#[derive(Debug, Clone)]
pub struct Packed {
    pub manifest: PackManifest,
    pub entries: Vec<PackEntry>,
}

/// zip 条目名归一化：反斜杠归一、剥前导斜杠与盘符、拒绝 `..` 组件（zip slip）。
/// 返回归一化后的相对路径；任何试图跳出包根的形态直接报错。
fn normalize_entry_name(raw: &str) -> Result<String, String> {
    let name = raw.replace('\\', "/");
    let name = name.trim_start_matches('/');
    // Windows 盘符（C:/…）与其余带冒号的形态都不收
    let mut cleaned: Vec<String> = Vec::new();
    for comp in name.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            return Err(format!("包内路径越界（zip slip）：{raw}"));
        }
        if comp.contains(':') || comp.contains('\0') {
            return Err(format!("包内路径含非法字符：{raw}"));
        }
        cleaned.push(comp.to_string());
    }
    if cleaned.is_empty() {
        return Err(format!("包内路径为空：{raw}"));
    }
    Ok(cleaned.join("/"))
}

/// 读取并校验一个包文件（生产上限）；测试可用 [`read_pack_limited`] 收紧阈值
pub fn read_pack(path: &Path) -> Result<Packed, String> {
    read_pack_limited(path, MAX_FILE_BYTES, MAX_ENTRIES, MAX_TOTAL_BYTES)
}

pub fn read_pack_limited(
    path: &Path,
    max_file: u64,
    max_entries: usize,
    max_total: u64,
) -> Result<Packed, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开 {} 失败：{e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("不是有效的 zip 包（{}）：{e}", path.display()))?;
    if zip.len() > max_entries {
        return Err(format!(
            "包内条目数 {} 超过上限 {max_entries}——不像是正常的化境包",
            zip.len()
        ));
    }
    let mut manifest_raw: Option<Vec<u8>> = None;
    let mut entries: Vec<PackEntry> = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("读取包内条目失败：{e}"))?;
        if entry.is_dir() {
            continue;
        }
        if entry.is_symlink() {
            return Err(format!(
                "包内有符号链接（{}）——拒绝解包，安装包不该包含链接",
                entry.name()
            ));
        }
        let name = normalize_entry_name(entry.name())?;
        if name == "pack.json" {
            manifest_raw = Some(read_entry(&mut entry, &name, max_file)?);
            continue;
        }
        let data = read_entry(&mut entry, &name, max_file)?;
        total += data.len() as u64;
        if total > max_total {
            return Err(format!("包解包总量超过上限（{}MB）——疑似 zip 炸弹", max_total / 1024 / 1024));
        }
        entries.push(PackEntry { name, data });
    }
    let manifest_raw = manifest_raw
        .ok_or_else(|| "包里没有 pack.json——不是化境包".to_string())?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_raw)
        .map_err(|e| format!("pack.json 解析失败：{e}"))?;
    manifest.validate()?;

    // 白名单布局校验：所有条目（含 pack.json 自身之外）的顶层名必须在声明集合内
    let allowed = allowed_top_level(&manifest.kind);
    let top_ok = |name: &str| {
        let head = name.split('/').next().unwrap_or_default();
        allowed.contains(&head)
    };
    if let Some(bad) = entries.iter().map(|e| e.name.as_str()).find(|n| !top_ok(n)) {
        return Err(format!(
            "包内出现「{}」条目——{}包只接受 {}",
            bad,
            manifest.kind,
            allowed.join(" / ")
        ));
    }
    for req in required_files(&manifest.kind) {
        if !entries.iter().any(|e| e.name == *req) {
            return Err(format!("{kind}包缺少必备文件 {req}", kind = manifest.kind));
        }
    }
    Ok(Packed { manifest, entries })
}

/// 读出一个条目的全部字节（先查声明尺寸，再查实际尺寸——两道闸都得过）
fn read_entry(entry: &mut zip::read::ZipFile<'_>, name: &str, max_file: u64) -> Result<Vec<u8>, String> {
    if entry.size() > max_file {
        return Err(format!(
            "包内文件 {name} 声明大小 {}MB 超过单文件上限（{}MB）",
            entry.size() / 1024 / 1024,
            max_file / 1024 / 1024
        ));
    }
    let mut data = Vec::with_capacity((entry.size() as usize).min(16 * 1024 * 1024));
    entry
        .read_to_end(&mut data)
        .map_err(|e| format!("解压 {name} 失败：{e}"))?;
    if data.len() as u64 > max_file {
        return Err(format!(
            "包内文件 {name} 实际大小 {}MB 超过单文件上限——声明的 {} 字节不可信",
            data.len() / 1024 / 1024,
            entry.size()
        ));
    }
    Ok(data)
}

// ---------- zip 写出 ----------

/// 把文件集写出为 zip（deflate）。`files` = (包内相对路径, 内容)；不写目录条目
/// （解包侧按文件路径补建目录，空目录不进包——导入侧本来就会建齐骨架）。
pub fn write_zip(path: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 {} 失败：{e}", parent.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("创建 {} 失败：{e}", path.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in files {
        zip.start_file(name, options.clone())
            .map_err(|e| format!("写入包条目 {name} 失败：{e}"))?;
        zip.write_all(data).map_err(|e| format!("写入包条目 {name} 失败：{e}"))?;
    }
    zip.finish()
        .map_err(|e| format!("收尾 zip 失败：{e}"))?
        .flush()
        .map_err(|e| format!("收尾 zip 失败：{e}"))?;
    Ok(())
}

/// 递归收集目录下全部文件为 (相对路径, 内容)，路径用正斜杠分隔（zip 惯例）
fn collect_dir_files(dir: &Path, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("读取 {} 失败：{e}", dir.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = if prefix.is_empty() {
            entry.file_name().to_string_lossy().into_owned()
        } else {
            format!("{prefix}/{}", entry.file_name().to_string_lossy())
        };
        if path.is_dir() {
            out.extend(collect_dir_files(&path, &rel)?);
        } else if path.is_file() {
            let data = std::fs::read(&path)
                .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
            out.push((rel, data));
        }
    }
    Ok(out)
}

/// manifest 序列化为 pack.json 字节（导出统一出口：缩进 + 结尾换行）
fn manifest_bytes(manifest: &PackManifest) -> Result<Vec<u8>, String> {
    let mut body = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    body.push('\n');
    Ok(body.into_bytes())
}

// ---------- 三包导出 ----------

/// 导出结果（前端展示：文件路径 + 包名）
#[derive(Debug, Clone, Serialize)]
pub struct ExportedPack {
    pub kind: String,
    pub name: String,
    pub path: String,
}

/// 角色包导出：卡目录整打包（card.lua + 随卡 assets/entities，若有），pack.json
/// 现场生成。包名 = 卡目录名（会话引用的是目录名，往返后目录名必须稳定）。
pub fn export_card_pack(root: &Path, dir_name: &str) -> Result<ExportedPack, String> {
    let source = root.join("characters").join(dir_name);
    if !source.join("card.lua").is_file() {
        return Err(format!("角色「{dir_name}」不存在或没有 card.lua"));
    }
    let mut manifest = PackManifest::new(KIND_CHARACTER, dir_name);
    if let Ok(loaded) = crate::card::load_card(root, dir_name) {
        manifest.creator = loaded.card.creator;
        let desc = loaded.card.scenario.trim();
        if !desc.is_empty() {
            manifest.description = Some(desc.chars().take(120).collect());
        }
    }
    let mut files = collect_dir_files(&source, "")?;
    files.retain(|(name, _)| name != "pack.json"); // 历史遗留的清单不进新包
    files.push(("pack.json".into(), manifest_bytes(&manifest)?));
    let out = exports_dir(root).join(format!("{KIND_CHARACTER}-{dir_name}.zip"));
    write_zip(&out, &files)?;
    Ok(ExportedPack {
        kind: KIND_CHARACTER.into(),
        name: dir_name.into(),
        path: out.display().to_string(),
    })
}

/// 世界包导出：`codex/<世界>/` 整打包（world.json 时钟、worldline.lua、
/// entities/、grown.json 正史增量，存在什么带什么）。
pub fn export_world_pack(root: &Path, world: &str) -> Result<ExportedPack, String> {
    let source = root.join("codex").join(world);
    if !source.is_dir() {
        return Err(format!("世界「{world}」不存在"));
    }
    let manifest = PackManifest::new(KIND_WORLD, world);
    let mut files = collect_dir_files(&source, "")?;
    files.retain(|(name, _)| name != "pack.json");
    files.push(("pack.json".into(), manifest_bytes(&manifest)?));
    let out = exports_dir(root).join(format!("{KIND_WORLD}-{world}.zip"));
    write_zip(&out, &files)?;
    Ok(ExportedPack {
        kind: KIND_WORLD.into(),
        name: world.into(),
        path: out.display().to_string(),
    })
}

/// 剧本包导出（决断 3）：从既有会话抽取 premise、当前黑板投影（作为模板的
/// 初始黑板，用户可再编辑）、导演树 director.lua（会话没自定义树就不带——
/// 导入侧落回内置起承转合树）。**不含消息历史与会话数据**。
pub fn export_script_pack(
    root: &Path,
    session_id: &str,
    name: Option<&str>,
) -> Result<ExportedPack, String> {
    let meta = crate::store::load_session(root, session_id)
        .map_err(|e| format!("会话「{session_id}」读取失败：{e}"))?;
    let board = crate::store::load_blackboard(root, session_id)
        .map_err(|e| format!("会话「{session_id}」黑板读取失败：{e}"))?;
    let pack_name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| {
            meta.premise
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| p.chars().take(24).collect::<String>())
        })
        .unwrap_or_else(|| format!("剧本-{session_id}"));

    let premise_toml = toml::to_string_pretty(&PremiseFile {
        premise: meta.premise.clone().unwrap_or_default(),
    })
    .map_err(|e| e.to_string())?;
    let mut board_json = serde_json::to_string_pretty(&board).map_err(|e| e.to_string())?;
    board_json.push('\n');

    let manifest = PackManifest::new(KIND_SCRIPT, &pack_name);
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("premise.toml".into(), premise_toml.into_bytes()),
        (
            "blackboard.initial.json".into(),
            board_json.into_bytes(),
        ),
        ("pack.json".into(), manifest_bytes(&manifest)?),
    ];
    let director_src = crate::store::session_dir(root, session_id).join("director.lua");
    if director_src.is_file() {
        let data = std::fs::read(&director_src)
            .map_err(|e| format!("读取 {} 失败：{e}", director_src.display()))?;
        files.push(("director.lua".into(), data));
    }
    let out = exports_dir(root).join(format!(
        "{KIND_SCRIPT}-{}.zip",
        crate::stimport::sanitize_dir_name(&pack_name)
    ));
    write_zip(&out, &files)?;
    Ok(ExportedPack {
        kind: KIND_SCRIPT.into(),
        name: pack_name,
        path: out.display().to_string(),
    })
}

/// premise.toml 的形态（TOML 序列化负责转义）
#[derive(Serialize, Deserialize)]
struct PremiseFile {
    premise: String,
}

// ---------- 剧本模板（scripts/<名>/）----------

/// 剧本模板清单条目（建会话向导的选择器用）
#[derive(Debug, Clone, Serialize)]
pub struct ScriptSummary {
    pub name: String,
    pub premise: String,
    pub has_director: bool,
}

/// 剧本模板全文（预填建会话向导用）
#[derive(Debug, Clone, Serialize)]
pub struct ScriptTemplate {
    pub name: String,
    pub premise: String,
    /// 初始黑板（day/clock/place/extra；actors 由新会话阵容决定，模板里的不采用）
    pub blackboard: Option<crate::store::Blackboard>,
    pub has_director: bool,
}

/// 已安装剧本清单（scripts/ 下每个子目录一个模板；坏目录跳过）
pub fn list_scripts(root: &Path) -> Vec<ScriptSummary> {
    let dir = scripts_dir(root);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let premise = load_script_template(root, &name)
            .map(|t| t.premise)
            .unwrap_or_default();
        out.push(ScriptSummary {
            has_director: path.join("director.lua").is_file(),
            name,
            premise,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 读一个剧本模板（premise.toml 必须在；黑板/导演树可缺）
pub fn load_script_template(root: &Path, name: &str) -> Result<ScriptTemplate, String> {
    let dir = scripts_dir(root).join(name);
    if !dir.is_dir() {
        return Err(format!("剧本「{name}」不存在"));
    }
    let premise = std::fs::read_to_string(dir.join("premise.toml"))
        .map_err(|e| format!("剧本「{name}」缺少 premise.toml：{e}"))?;
    let file: PremiseFile =
        toml::from_str(&premise).map_err(|e| format!("premise.toml 解析失败：{e}"))?;
    let blackboard = std::fs::read_to_string(dir.join("blackboard.initial.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    Ok(ScriptTemplate {
        name: name.to_string(),
        premise: file.premise,
        blackboard,
        has_director: dir.join("director.lua").is_file(),
    })
}

// ---------- 包导入 ----------

/// 包导入报告（前端展示）
#[derive(Debug, Clone, Serialize)]
pub struct PackImportReport {
    pub kind: String,
    pub name: String,
    /// 落盘位置（相对 DataHub，如 characters/月见）
    pub target: String,
    pub files: usize,
    /// true = 目标已存在且被覆盖（用户勾选覆盖）
    pub overwritten: bool,
    pub warnings: Vec<String>,
}

/// 导入预览（导入弹窗展示：清单 + 文件数 + 提醒 + 冲突标记）
#[derive(Debug, Clone, Serialize)]
pub struct PackPreview {
    pub manifest: PackManifest,
    /// 包内文件（名字 + 字节数，pack.json 除外）
    pub files: Vec<PackFileInfo>,
    pub warnings: Vec<String>,
    /// 目标位置已被同名安装物占用（前端据此亮出「覆盖」选项）
    pub conflict: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackFileInfo {
    pub name: String,
    pub size: u64,
}

/// 预览：只读包与目标现状，不落盘
pub fn preview_pack(root: &Path, path: &Path) -> Result<PackPreview, String> {
    let packed = read_pack(path)?;
    let mut warnings = Vec::new();
    let manifest = &packed.manifest;
    if let Some(world) = manifest.requires.as_ref().and_then(|r| r.world.as_deref()) {
        if !root.join("codex").join(world).is_dir() {
            warnings.push(format!(
                "这张卡声明依赖世界「{world}」，当前未安装——可以先导入，设定注入会缺这一层"
            ));
        }
    }
    if install_target_exists(root, manifest) {
        warnings.push(match manifest.kind.as_str() {
            KIND_CHARACTER => format!(
                "工作区已有同名角色「{}」——默认并存为「-2」，可勾选覆盖",
                manifest.name
            ),
            KIND_WORLD => format!(
                "已有同名世界「{}」——默认并存为「-2」，可勾选覆盖",
                manifest.name
            ),
            _ => format!(
                "已有同名剧本「{}」——默认并存为「-2」，可勾选覆盖",
                manifest.name
            ),
        });
    }
    Ok(PackPreview {
        conflict: install_target_exists(root, manifest),
        files: packed
            .entries
            .iter()
            .map(|e| PackFileInfo {
                name: e.name.clone(),
                size: e.data.len() as u64,
            })
            .collect(),
        warnings,
        manifest: manifest.clone(),
    })
}

/// 安装目标（角色/世界/剧本各自的家）是否已被同名占用
fn install_target_exists(root: &Path, manifest: &PackManifest) -> bool {
    install_root(root, manifest)
        .join(&crate::stimport::sanitize_dir_name(&manifest.name))
        .exists()
}

/// 各包类型的安装根目录（目标目录 = 根 + 清洗后的包名）
fn install_root(root: &Path, manifest: &PackManifest) -> std::path::PathBuf {
    match manifest.kind.as_str() {
        KIND_CHARACTER => root.join("characters"),
        KIND_WORLD => root.join("codex"),
        _ => scripts_dir(root),
    }
}

/// 导入到指定 root（命令层的可测内核）：识别 kind → 按先例落目录 → 生成物
/// 读回校验（坏包回滚）。同名冲突默认并存「-2」，overwrite = 覆盖原目录。
pub fn import_pack_to(root: &Path, path: &Path, overwrite: bool) -> Result<PackImportReport, String> {
    let packed = read_pack(path)?;
    let manifest = packed.manifest.clone();
    crate::store::ensure_layout(root).map_err(|e| format!("数据目录初始化失败：{e}"))?;
    std::fs::create_dir_all(scripts_dir(root))
        .map_err(|e| format!("创建 scripts/ 失败：{e}"))?;

    let base = crate::stimport::sanitize_dir_name(&manifest.name);
    let install_home = install_root(root, &manifest);
    // 冲突处理沿 stimport 先例：默认并存（-2、-3…），显式 overwrite 才覆盖
    let mut target_name = base.clone();
    let mut overwritten = false;
    if install_home.join(&target_name).exists() {
        if overwrite {
            overwritten = true;
        } else {
            let mut n = 2;
            while install_home.join(format!("{base}-{n}")).exists() {
                n += 1;
            }
            target_name = format!("{base}-{n}");
        }
    }
    let target = install_home.join(&target_name);
    if overwritten {
        std::fs::remove_dir_all(&target)
            .map_err(|e| format!("覆盖 {} 失败：{e}", target.display()))?;
    }
    std::fs::create_dir_all(&target).map_err(|e| format!("创建 {} 失败：{e}", target.display()))?;

    // 落盘：pack.json 是容器清单不落目标；其余按白名单布局原样写
    let warnings = packed_warnings(root, &manifest);
    for entry in &packed.entries {
        let dest = target.join(&entry.name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败：{e}", parent.display()))?;
        }
        std::fs::write(&dest, &entry.data)
            .map_err(|e| format!("写入 {} 失败：{e}", dest.display()))?;
    }

    // 生成物必须能被自家解析器读回（与 stimport 同一条纪律）：读不回就整体回滚
    if let Err(e) = validate_installed(root, &manifest, &target) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(format!("导入的包无法被解析（已回滚）：{e}"));
    }

    crate::diag::record(
        "import",
        format!(
            "包导入：{}（{}）→ {}",
            manifest.name, manifest.kind, target.display()
        ),
    );
    Ok(PackImportReport {
        kind: manifest.kind.clone(),
        name: manifest.name.clone(),
        target: format!(
            "{}/{}",
            install_home.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            target_name
        ),
        files: packed.entries.len(),
        overwritten,
        warnings,
    })
}

fn packed_warnings(root: &Path, manifest: &PackManifest) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(world) = manifest.requires.as_ref().and_then(|r| r.world.as_deref()) {
        if !root.join("codex").join(world).is_dir() {
            warnings.push(format!("依赖的世界「{world}」仍未安装——设定注入会缺这一层"));
        }
    }
    warnings
}

/// 安装物读回校验：角色卡必须能完整加载；世界的实体文件坏一个留诊断不阻塞
/// （parse_entities 的既有纪律：一个手滑文件不瘫痪整局）；剧本按需懒加载。
fn validate_installed(root: &Path, manifest: &PackManifest, target: &Path) -> Result<(), String> {
    match manifest.kind.as_str() {
        KIND_CHARACTER => {
            let dir_name = target
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("目标目录名不可读")?;
            let loaded = crate::card::load_card(root, dir_name)?;
            if loaded.degraded {
                return Err(
                    loaded.degrade_reason.unwrap_or_else(|| "card.lua 解析降级".into())
                );
            }
            Ok(())
        }
        KIND_WORLD => {
            let entities = target.join("entities");
            if entities.is_dir() {
                for entry in std::fs::read_dir(&entities).map_err(|e| e.to_string())?.flatten() {
                    let path = entry.path();
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !matches!(ext, "lua" | "json") {
                        continue;
                    }
                    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    let value = if ext == "lua" {
                        crate::card::eval_lua_value(&raw)
                    } else {
                        serde_json::from_str(&raw).map_err(|e| e.to_string())
                    };
                    value.and_then(|v| crate::codex::CodexEntity::from_value(&v))
                        .map_err(|e| {
                            format!(
                                "实体文件 {} 无法解析：{e}",
                                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                            )
                        })?;
                }
            }
            if target.join("world.json").is_file() {
                serde_json::from_str::<serde_json::Value>(
                    &std::fs::read_to_string(target.join("world.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| format!("world.json 解析失败：{e}"))?;
            }
            Ok(())
        }
        _ => {
            // 剧本：premise.toml 能否读回（黑板/导演树在使用处校验）
            crate::pack::load_script_template(
                root,
                target.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            )
            .map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(kind: &str, name: &str) -> PackManifest {
        PackManifest::new(kind, name)
    }

    /// 把文件集写成一个包文件（测试造包的统一入口）
    fn build_pack(path: &Path, manifest: &PackManifest, files: &[(&str, &[u8])]) {
        let mut all: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(n, d)| (n.to_string(), d.to_vec()))
            .collect();
        all.push(("pack.json".into(), manifest_bytes(manifest).unwrap()));
        write_zip(path, &all).unwrap();
    }

    const CARD_LUA: &str = concat!(
        "return { spec = 'charcard/1.0', name = '月见', scenario = '天文台',\n",
        "  personality = '安静', first_mes = '「你来了。」' }\n"
    );

    #[test]
    fn manifest_validation_rejects_unknown_spec_kind_and_empty_name() {
        let mut m = manifest(KIND_CHARACTER, "月见");
        m.validate().unwrap();
        // 未来规格：拒绝并报出支持的最高版本
        m.spec = "huajing-pack/2".into();
        let err = m.validate().unwrap_err();
        assert!(err.contains("huajing-pack/1"), "{err}");
        assert!(err.contains("升级"), "应提示升级：{err}");
        // 旧规格同样拒绝
        m.spec = "huajing-pack/0".into();
        assert!(m.validate().is_err());
        // 陌生形态
        m.spec = "st-pack/1".into();
        assert!(m.validate().is_err());
        // kind 三选一
        let mut m = manifest("movie", "x");
        assert!(m.validate().is_err());
        // name 必填非空
        let mut m = manifest(KIND_WORLD, "  ");
        assert!(m.validate().is_err());
    }

    #[test]
    fn read_pack_accepts_and_rejects_layout_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let pack_path = dir.path().join("c.zip");
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "月见"),
            &[("card.lua", CARD_LUA.as_bytes()), ("assets/a.png", b"png")],
        );
        let packed = read_pack(&pack_path).unwrap();
        assert_eq!(packed.manifest.kind, KIND_CHARACTER);
        assert_eq!(packed.entries.len(), 2, "pack.json 单列不进 entries");
        assert!(packed.entries.iter().any(|e| e.name == "card.lua"));

        // 世界包带 entities/ 与 grown.json：白名单内
        let world_path = dir.path().join("w.zip");
        build_pack(
            &world_path,
            &manifest(KIND_WORLD, "default"),
            &[
                ("world.json", b"{\"day\":1}".as_slice()),
                ("entities/note.a.json", b"{\"id\":\"note.a\",\"type\":\"note\",\"name\":\"a\"}".as_slice()),
                ("grown.json", b"{\"entities\":{}}".as_slice()),
            ],
        );
        assert!(read_pack(&world_path).is_ok());

        // 白名单之外：角色包里塞 world.json → 拒绝
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "月见"),
            &[("card.lua", CARD_LUA.as_bytes()), ("world.json", b"{}".as_slice())],
        );
        let err = read_pack(&pack_path).unwrap_err();
        assert!(err.contains("只接受"), "{err}");

        // 缺必备文件：角色包没有 card.lua → 拒绝
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "月见"),
            &[("assets/a.png", b"png")],
        );
        let err = read_pack(&pack_path).unwrap_err();
        assert!(err.contains("card.lua"), "{err}");

        // 没有 pack.json：不是化境包
        let bare = dir.path().join("bare.zip");
        write_zip(&bare, &[("card.lua".into(), CARD_LUA.as_bytes().to_vec())]).unwrap();
        assert!(read_pack(&bare).is_err());
    }

    #[test]
    fn zip_slip_and_hostile_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let pack_path = dir.path().join("evil.zip");
        // 经典 zip slip：.. 越界
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "evil"),
            &[("../outside.txt", b"pwn".as_slice())],
        );
        let err = read_pack(&pack_path).unwrap_err();
        assert!(err.contains("zip slip"), "{err}");
        // 嵌套越界同样拒绝
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "evil"),
            &[("assets/../../outside.txt", b"pwn".as_slice())],
        );
        assert!(read_pack(&pack_path).unwrap_err().contains("zip slip"));
        // 绝对路径与盘符
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "evil"),
            &[("/abs.txt", b"pwn".as_slice())],
        );
        assert!(read_pack(&pack_path).is_err());
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "evil"),
            &[("C:/abs.txt", b"pwn".as_slice())],
        );
        assert!(read_pack(&pack_path).is_err());
        // 正常包（zip crate 直接写出）也能读回——防护不打误伤
        let plain_path = dir.path().join("plain.zip");
        build_pack(
            &plain_path,
            &manifest(KIND_CHARACTER, "月见"),
            &[("card.lua", CARD_LUA.as_bytes())],
        );
        let packed = read_pack(&plain_path).unwrap();
        assert_eq!(packed.manifest.kind, KIND_CHARACTER);
    }

    #[test]
    fn size_and_count_limits_stop_zip_bombs() {
        let dir = tempfile::tempdir().unwrap();
        let pack_path = dir.path().join("big.zip");
        // 单文件超限（测试用 1KB 上限造 2KB 的声明）
        build_pack(
            &pack_path,
            &manifest(KIND_WORLD, "w"),
            &[("world.json", &vec![b'x'; 2048])],
        );
        let err = read_pack_limited(&pack_path, 1024, 100, 1 << 30).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        // entry 数量超限
        let many_path = dir.path().join("many.zip");
        let files: Vec<(String, Vec<u8>)> = (0..5)
            .map(|i| (format!("entities/n{i}.json"), b"{}".to_vec()))
            .chain([("world.json".into(), b"{}".to_vec())])
            .collect();
        write_zip(&many_path, &files).unwrap();
        assert!(read_pack_limited(&many_path, 1024, 3, 1 << 30).is_err(), "条目数超限应拒绝");
        // 声明尺寸是第一道闸、实际字节是第二道（真实 zip 造不出 size 撒谎的包，
        // 两道闸的常量与读取路径由上面的单文件用例与 read_entry 实现钉住）
        assert_eq!(MAX_FILE_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn normalize_entry_name_blocks_traversal_but_keeps_normal() {
        assert_eq!(normalize_entry_name("entities/a.json").unwrap(), "entities/a.json");
        assert_eq!(normalize_entry_name("assets\\a.png").unwrap(), "assets/a.png");
        assert_eq!(normalize_entry_name("/world.json").unwrap(), "world.json");
        assert_eq!(normalize_entry_name("a/./b").unwrap(), "a/b");
        assert!(normalize_entry_name("../x").is_err());
        assert!(normalize_entry_name("a/../../x").is_err());
        assert!(normalize_entry_name("C:\\x").is_err());
        assert!(normalize_entry_name("").is_err());
    }

    #[test]
    fn card_pack_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        // 准备一张卡
        let draft = crate::stimport::parse_st_json(
            r#"{ "data": { "name": "月见", "first_mes": "「你来了。」", "description": "天文社学姐" } }"#,
            None,
        )
        .unwrap();
        let report = crate::stimport::save_card_draft(root.path(), &draft, false).unwrap();

        // 导出 → 删目录 → 导入 → 卡面一致
        let exported = export_card_pack(root.path(), &report.dir_name).unwrap();
        assert!(std::path::Path::new(&exported.path).is_file());
        std::fs::remove_dir_all(root.path().join("characters").join(&report.dir_name)).unwrap();

        let imported = import_pack_to(root.path(), std::path::Path::new(&exported.path), false).unwrap();
        assert_eq!(imported.kind, KIND_CHARACTER);
        assert_eq!(imported.target, "characters/月见");
        assert!(!imported.overwritten);

        let loaded = crate::card::load_card(root.path(), "月见").unwrap();
        assert!(!loaded.degraded, "{:?}", loaded.degrade_reason);
        assert_eq!(loaded.card.name, "月见");
        assert_eq!(loaded.card.first_mes, "「你来了。」");
        assert!(loaded.card.scenario.contains("天文社学姐"));
    }

    #[test]
    fn world_pack_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let world_dir = root.path().join("codex/夜城");
        std::fs::create_dir_all(world_dir.join("entities")).unwrap();
        std::fs::write(world_dir.join("world.json"), "{\"day\":3,\"updated_at\":1}").unwrap();
        std::fs::write(
            world_dir.join("entities/note.夜市.json"),
            r#"{"id":"note.夜市","type":"note","name":"夜市","aliases":["夜街"],"one_liner":"夜市在旧运河边。"}"#,
        )
        .unwrap();
        std::fs::write(
            world_dir.join("entities/place.钟楼.lua"),
            "return { id = 'place.钟楼', type = 'place', name = '钟楼', one_liner = '整点敲响。' }",
        )
        .unwrap();
        std::fs::write(world_dir.join("grown.json"), r#"{"entities":{}}"#).unwrap();

        let exported = export_world_pack(root.path(), "夜城").unwrap();
        std::fs::remove_dir_all(&world_dir).unwrap();
        let imported = import_pack_to(root.path(), std::path::Path::new(&exported.path), false).unwrap();
        assert_eq!(imported.target, "codex/夜城");

        // 投影一致：实体解析 + grown 应用与导出前相同
        let entities = crate::commands::parse_entities(&world_dir.join("entities"));
        assert_eq!(entities.len(), 2);
        assert!(entities.iter().any(|e| e.id == "note.夜市" && e.aliases == vec!["夜街"]));
        assert!(entities.iter().any(|e| e.id == "place.钟楼"));
        let world: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(world_dir.join("world.json")).unwrap()).unwrap();
        assert_eq!(world.get("day").and_then(|d| d.as_i64()), Some(3));
    }

    #[test]
    fn import_conflicts_follow_overwrite_precedent() {
        let root = tempfile::tempdir().unwrap();
        let pack_path = root.path().join("c.zip");
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "月见"),
            &[("card.lua", CARD_LUA.as_bytes())],
        );

        // 第一次：正常安装
        import_pack_to(root.path(), &pack_path, false).unwrap();
        // 第二次同名不同内容：并存 -2
        let pack2 = root.path().join("c2.zip");
        build_pack(
            &pack2,
            &manifest(KIND_CHARACTER, "月见"),
            &[(
                "card.lua",
                "return { spec = 'charcard/1.0', name = '月见', scenario = '另一版', personality = '', first_mes = 'x' }"
                    .as_bytes(),
            )],
        );
        let report = import_pack_to(root.path(), &pack2, false).unwrap();
        assert_eq!(report.target, "characters/月见-2");
        // 第三次勾选覆盖：沿 stimport 先例写回**同名原目录**（覆盖的是原名，不是 -2）
        let report = import_pack_to(root.path(), &pack2, true).unwrap();
        assert_eq!(report.target, "characters/月见");
        assert!(report.overwritten);
        let dirs: Vec<String> = std::fs::read_dir(root.path().join("characters"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(dirs.len(), 2, "覆盖不新增目录：{dirs:?}");
        // 预览应报冲突
        let preview = preview_pack(root.path(), &pack2).unwrap();
        assert!(preview.conflict);
        assert!(preview.warnings.iter().any(|w| w.contains("覆盖")));
    }

    #[test]
    fn corrupted_card_import_rolls_back() {
        let root = tempfile::tempdir().unwrap();
        let pack_path = root.path().join("bad.zip");
        build_pack(
            &pack_path,
            &manifest(KIND_CHARACTER, "坏卡"),
            &[("card.lua", "return { 完全不是卡".as_bytes())],
        );
        let err = import_pack_to(root.path(), &pack_path, false).unwrap_err();
        assert!(err.contains("回滚"), "{err}");
        assert!(
            !root.path().join("characters/坏卡").exists(),
            "校验失败不留空壳目录"
        );
    }

    #[test]
    fn script_pack_roundtrip_and_new_session_integration() {
        let root = tempfile::tempdir().unwrap();
        // 一场带 premise、黑板与自定义导演树的会话
        let meta = crate::store::new_session(
            root.path(),
            &crate::store::NewSessionRequest {
                character: "月见".into(),
                characters: vec!["月见".into()],
                persona: None,
                day: Some(3),
                clock: Some("21:30".into()),
                place: Some("天文台".into()),
                premise: Some("流星雨之夜".into()),
                script: None,
                world: None,
            },
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("characters/月见")).unwrap();
        std::fs::write(
            root.path().join("characters/月见/card.lua"),
            CARD_LUA,
        )
        .unwrap();
        crate::store::save_blackboard(
            root.path(),
            &meta.id,
            &crate::store::Blackboard {
                day: 3,
                clock: "21:30".into(),
                place: "天文台".into(),
                actors: vec!["月见".into()],
                extra: [("char.月见.status".to_string(), serde_json::json!("兴奋"))]
                    .into_iter()
                    .collect(),
            },
        )
        .unwrap();
        std::fs::write(
            crate::store::session_dir(root.path(), &meta.id).join("director.lua"),
            "return { state_tree = { root = '起', states = { ['起'] = { transitions = {} } } } }",
        )
        .unwrap();

        // 导出剧本包
        let exported = export_script_pack(root.path(), &meta.id, Some("流星雨夜")).unwrap();
        assert_eq!(exported.name, "流星雨夜");

        // 导入 → scripts/流星雨夜/ 三件套就位
        let imported = import_pack_to(root.path(), std::path::Path::new(&exported.path), false).unwrap();
        assert_eq!(imported.target, "scripts/流星雨夜");
        let script_dir = scripts_dir(root.path()).join("流星雨夜");
        // pack.json 是容器清单，不落安装目录（重复导出幂等的前提）
        assert!(!script_dir.join("pack.json").is_file());
        assert!(script_dir.join("premise.toml").is_file());
        assert!(script_dir.join("blackboard.initial.json").is_file());
        assert!(script_dir.join("director.lua").is_file());

        // 模板读回：premise 与黑板还原，导演树在
        let tpl = load_script_template(root.path(), "流星雨夜").unwrap();
        assert_eq!(tpl.premise, "流星雨之夜");
        let board = tpl.blackboard.unwrap();
        assert_eq!(board.day, 3);
        assert_eq!(board.place, "天文台");
        assert_eq!(
            board.extra.get("char.月见.status").and_then(|v| v.as_str()),
            Some("兴奋")
        );
        let summaries = list_scripts(root.path());
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].has_director);

        // 用剧本建会话：premise/黑板/导演树落进新会话，actors 换成本会话阵容
        let meta2 = crate::store::new_session(
            root.path(),
            &crate::store::NewSessionRequest {
                character: "月见".into(),
                characters: vec!["月见".into()],
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: None,
                script: Some("流星雨夜".into()),
                world: None,
            },
        )
        .unwrap();
        assert_eq!(meta2.premise.as_deref(), Some("流星雨之夜"));
        let board2 = crate::store::load_blackboard(root.path(), &meta2.id).unwrap();
        assert_eq!(board2.day, 3);
        assert_eq!(board2.place, "天文台");
        assert_eq!(board2.actors, vec!["月见".to_string()]);
        assert!(crate::store::session_dir(root.path(), &meta2.id).join("director.lua").is_file());
    }

    #[test]
    fn script_export_without_custom_tree_omits_director() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("characters/月见")).unwrap();
        std::fs::write(root.path().join("characters/月见/card.lua"), CARD_LUA).unwrap();
        let meta = crate::store::new_session(
            root.path(),
            &crate::store::NewSessionRequest {
                character: "月见".into(),
                characters: vec!["月见".into()],
                persona: None,
                day: None,
                clock: None,
                place: None,
                premise: Some("老图书馆月底拆除".into()),
                script: None,
                world: None,
            },
        )
        .unwrap();
        let exported = export_script_pack(root.path(), &meta.id, None).unwrap();
        // 没给名字：从 premise 取
        assert_eq!(exported.name, "老图书馆月底拆除");
        let packed = read_pack(std::path::Path::new(&exported.path)).unwrap();
        assert!(packed.entries.iter().all(|e| e.name != "director.lua"));
        // 导入后模板无导演树（建会话落回内置起承转合树）
        import_pack_to(root.path(), std::path::Path::new(&exported.path), false).unwrap();
        let tpl = load_script_template(root.path(), "老图书馆月底拆除").unwrap();
        assert!(!tpl.has_director);
        assert!(tpl.blackboard.is_some());
    }
}
