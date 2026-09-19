//! Tauri 命令层：前端可调用的入口。

use crate::store;

#[tauri::command]
pub fn app_info() -> serde_json::Value {
    serde_json::json!({
        "name": "化境 Huajing",
        "slogan": "扮谁，便入谁之境。",
        "version": env!("CARGO_PKG_VERSION"),
        "dataRoot": store::data_root(),
    })
}

// TODO(M1): commands.rs — list_providers / list_cards / load_card /
//   new_session / send_message（SSE 流式事件）/ snapshot 等命令。
