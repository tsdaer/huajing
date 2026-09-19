//! DataHub 明文数据层（设计 §12）：一切用户数据为本地明文文件。

use std::path::PathBuf;

/// DataHub 目录解析：HUAJING_DATA 环境变量 > 可执行文件旁 DataHub > 仓库 DataHub
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
        None => PathBuf::from("DataHub"),
    }
}

/// 确保数据目录骨架存在（设计 §12 目录树）
pub fn ensure_layout(root: &PathBuf) -> std::io::Result<()> {
    for d in ["personas", "characters", "codex", "sessions"] {
        std::fs::create_dir_all(root.join(d))?;
    }
    Ok(())
}

// TODO(M1): store.rs — 会话目录（messages.jsonl 追加式，可回放）；
//   providers.json / 卡片目录读取；卡片热加载监听。
