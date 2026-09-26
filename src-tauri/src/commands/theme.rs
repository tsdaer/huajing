//! Tauri 命令 · 主题持久化（M4.3 · 决断 7：主题块是机器生成的运行时数据，
//! 归 JSON 侧——`DataHub/themes/<名>.json`，拷走 DataHub 即主题跟走）。
//!
//! `custom` 是保留名：当前生效的自定义主题固定写这里（启动时加载应用）；
//! 其余名字是主题库（主题页「保存到主题库」→ 列表 → 点选应用）。

use super::*;

fn clean_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("主题名不能为空".into());
    }
    Ok(trimmed.to_string())
}

/// 保存主题（含覆盖同名）；返回落盘文件 stem（sanitize 之后）。
#[tauri::command]
pub fn theme_save(name: String, theme: crate::store::CustomTheme) -> Result<String, String> {
    let name = clean_name(&name)?;
    if theme.vars.is_empty() {
        return Err("主题没有任何令牌——清除请用删除，保存请先在编辑器里改点东西".into());
    }
    crate::store::save_theme(&root(), &name, &theme).map_err(|e| e.to_string())
}

/// 读主题；不存在 = None（启动加载依赖这个语义，不是错误）。
#[tauri::command]
pub fn theme_load(name: String) -> Result<Option<crate::store::CustomTheme>, String> {
    let name = clean_name(&name)?;
    crate::store::load_theme(&root(), &name).map_err(|e| e.to_string())
}

/// 主题库清单（themes/*.json 的 stem，字典序）。
#[tauri::command]
pub fn theme_list() -> Result<Vec<String>, String> {
    crate::store::list_themes(&root()).map_err(|e| e.to_string())
}

/// 删除主题文件；返回是否真的删了（清除活动主题时幂等）。
#[tauri::command]
pub fn theme_delete(name: String) -> Result<bool, String> {
    let name = clean_name(&name)?;
    crate::store::delete_theme(&root(), &name).map_err(|e| e.to_string())
}

/// 主题块导入解析（纯解析不落盘）：接受 toPluginCss 同格式的 CSS 块或
/// CustomTheme 同形的 JSON；键不在 allowed_keys（前端编辑器的令牌全集）里
/// 一律拒绝并列出非法键。
#[tauri::command]
pub fn theme_parse_import(
    payload: String,
    allowed_keys: Vec<String>,
) -> Result<crate::store::ParsedThemeImport, String> {
    crate::store::parse_theme_import(&payload, &allowed_keys)
}

/// 主题 CSS 落文件（导出的「保存文件」出口，复制到剪贴板之外的第二条路）：
/// `exports/<名>.theme.css`，返回完整路径。
#[tauri::command]
pub fn theme_export_file(name: String, css: String) -> Result<String, String> {
    let name = clean_name(&name)?;
    let root = root();
    let dir = crate::pack::exports_dir(&root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = crate::stimport::sanitize_dir_name(&name);
    let path = dir.join(format!("{stem}.theme.css"));
    std::fs::write(&path, css + "\n").map_err(|e| e.to_string())?;
    crate::diag::record("export", format!("主题导出：{} → {}", stem, path.display()));
    Ok(path.display().to_string())
}
