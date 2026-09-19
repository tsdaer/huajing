// 化境 Huajing — 本地优先的智能体角色扮演客户端
// 设计文档：docs/design.md（各模块对应章节见文件头注释）

mod card;     // 角色卡与 Lua 沙箱（设计 §3）
mod commands; // Tauri 命令层
mod llm;      // OpenAI 兼容 SSE 客户端（设计 §11）
mod prompt;   // Prompt Builder 双槽位组装（设计 §4）
mod stimport; // SillyTavern 角色卡导入（M1.8 · 设计 §13）
mod store;    // DataHub 明文数据层（设计 §12）
mod watch;    // DataHub 热加载监听（M1.7）

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(commands::CancelFlags::default())
        .manage(commands::LastAssemblies::default())
        .manage(store::MessageLog::new())
        .manage(watch::CardWatch::default())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::runtime_info,
            commands::test_provider,
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
            commands::edit_message,
            commands::delete_message,
            commands::send_message,
            commands::regenerate,
            commands::stop_generation,
            commands::get_blackboard,
            commands::update_blackboard,
            commands::preview_prompt,
            commands::last_prompt,
            commands::get_card_state,
            commands::list_card_memory,
            watch::watch_cards,
            watch::unwatch_cards,
            stimport::import_st_card,
            stimport::preview_st_card,
        ])
        .run(tauri::generate_context!())
        .expect("error while running huajing");
}
