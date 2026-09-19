// 化境 Huajing — 本地优先的智能体角色扮演客户端
// 设计文档：docs/design.md（各模块对应章节见文件头注释）

mod card;     // 角色卡与 Lua 沙箱（设计 §3）
mod commands; // Tauri 命令层
mod llm;      // OpenAI 兼容 SSE 客户端（设计 §11）
mod prompt;   // Prompt Builder 双槽位组装（设计 §4）
mod store;    // DataHub 明文数据层（设计 §12）

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(commands::CancelFlags::default())
        .manage(store::MessageLog::new())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::list_providers,
            commands::save_provider,
            commands::delete_provider,
            commands::get_settings,
            commands::save_settings,
            commands::list_personas,
            commands::list_cards,
            commands::get_card,
            commands::new_session,
            commands::list_sessions,
            commands::read_messages,
            commands::send_message,
            commands::stop_generation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running huajing");
}
