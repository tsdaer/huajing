// 化境 Huajing — 本地优先的智能体角色扮演客户端
// 设计文档：docs/design.md（各模块对应章节见文件头注释）

mod card;     // 角色卡与 Lua 沙箱（设计 §3）
mod codex;    // 设定集：实体图谱、激活与分级注入（M2.2 · 设计 §6）
mod commands; // Tauri 命令层
mod diag;     // 运行时诊断环形缓冲
mod event;    // 事件日志：类型化事件流与投影（M2.0 · 设计 §7.3）
mod llm;      // OpenAI 兼容 SSE 客户端（设计 §11）
mod palace;   // 记忆宫殿：记忆对象、召回与视图（M2.1 · 设计 §5）
mod prompt;   // Prompt Builder 双槽位组装（设计 §4）
mod psyche;   // 心理运行时：情绪槽、衰减、意图（M2.5 · 设计 §9）
mod threads;  // 剧情线：生命周期与提及时机（M2.4 · 设计 §8）
mod statetree; // 状态树：剧情状态机的纯数据与算法（M2.3 · 设计 §7）
mod stimport; // SillyTavern 角色卡导入（M1.8 · 设计 §13）
mod store;    // DataHub 明文数据层（设计 §12）
mod summarize; // 自动总结管线：批次 → 六类产物（M2.6 · 设计 §5.3）
mod watch;    // DataHub 热加载监听（M1.7）

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(commands::CancelFlags::default())
        .manage(commands::LastAssemblies::default())
        .manage(store::EventLog::new())
        .manage(commands::CodexCache::default())
        .manage(commands::SessionRuntime::default())
        .manage(commands::TreeCache::default())
        .manage(commands::SummaryFlags::default())
        .manage(watch::CardWatch::default())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::runtime_info,
            commands::recent_diagnostics,
            commands::record_diagnostic,
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
            commands::inspector_data,
            commands::decide_proposal,
            commands::summarize_now,
            commands::open_thread,
            commands::resolve_thread,
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
