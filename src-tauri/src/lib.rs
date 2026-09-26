// 化境 Huajing — 本地优先的智能体角色扮演客户端
// 设计文档：docs/design.md（各模块对应章节见文件头注释）

mod card;     // 角色卡与 Lua 沙箱（设计 §3）
mod codex;    // 设定集：实体图谱、激活与分级注入（M2.2 · 设计 §6）
mod commands; // Tauri 命令层
mod complete; // 设定补全：模板补全、一致性校验、即兴模式（M3.8 · 设计 §6.8）
mod consolidate; // 宫殿睡眠整理：候选分组、合并稿、归档标记（M4.4 · 设计 §5.4）
mod diag;     // 运行时诊断环形缓冲
mod director; // 导演调度：发言权打分与发言计划（M3.4 · 设计 §10.5）
mod event;    // 事件日志：类型化事件流与投影（M2.0 · 设计 §7.3）
mod ingest;   // 素材规格化管线：清洗分段、机械映射、草稿包、切入点向导（M3.9 · 设计 §6.7）
mod llm;      // OpenAI 兼容 SSE 客户端（设计 §11）
mod pack;     // 包格式与导入导出：pack.json + zip，三包往返（M4.1 · 设计 §13）
mod palace;   // 记忆宫殿：记忆对象、召回与视图（M2.1 · 设计 §5）
mod prompt;   // Prompt Builder 双槽位组装（设计 §4）
mod psyche;   // 心理运行时：情绪槽、衰减、意图（M2.5 · 设计 §9）
mod scene;    // 场景与多线：「与此同时」的隔离顶层单元（M3.2 · 设计 §10.3）
mod semantic; // 语义关联：嵌入索引、余弦与门禁评测（M3.10 · 设计 §6.13）
#[cfg(test)]
mod smoke_datahub; // DataHub 资产冒烟测试：仓库示例卡与设定集的守卫（设计 §12）
mod threads;  // 剧情线：生命周期与提及时机（M2.4 · 设计 §8）
mod statetree; // 状态树：剧情状态机的纯数据与算法（M2.3 · 设计 §7）
mod stimport; // SillyTavern 角色卡导入（M1.8 · 设计 §13）
mod store;    // DataHub 明文数据层（设计 §12）
mod summarize; // 自动总结管线：批次 → 六类产物（M2.6 · 设计 §5.3）
mod toolcall; // 主演模型的工具调用快通道：schema、校验、直写/提案应用器（增强 · 包 A）
mod watch;    // DataHub 热加载监听（M1.7）
mod worldline; // 世界主线与世界时钟：世界作用域的阶段弧（M3.7 · 设计 §6.6）

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // 签名自动更新（M4.2 · 决断 1）：公钥进 tauri.conf.json（plugins.updater.pubkey），
        // endpoint 运行时从 settings.toml 读（commands/updater.rs）；签名不符在下载后即拒装
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(commands::CancelFlags::default())
        .manage(commands::LastAssemblies::default())
        .manage(store::EventLog::new())
        .manage(commands::CodexCache::default())
        .manage(commands::EmbedCache::default())
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
            commands::session_timeline,
            commands::get_card_state,
            commands::list_card_memory,
            commands::list_scenes,
            commands::create_scene,
            commands::switch_scene,
            commands::split_scene,
            commands::merge_scenes,
            commands::update_scene,
            commands::set_max_speakers,
            commands::set_improv,
            commands::set_novel_mode,
            commands::latest_options,
            commands::export_novel,
            commands::decide_all_proposals,
            commands::codex_complete,
            commands::codex_complete_apply,
            commands::codex_semantic_check,
            commands::set_theater,
            commands::theater_view,
            commands::worldline_view,
            commands::world_set_clock,
            commands::codex_resolve_preview,
            commands::ingest_prompts,
            commands::ingest_prepare,
            commands::ingest_classify,
            commands::ingest_extract,
            commands::ingest_commit,
            commands::import_worldbook,
            commands::preview_pack,
            commands::import_pack,
            commands::export_card_pack,
            commands::export_world_pack,
            commands::export_script_pack,
            commands::export_worldbook_st,
            commands::list_scripts,
            commands::get_script,
            commands::updater_info,
            commands::check_update,
            commands::download_and_install,
            commands::theme_save,
            commands::theme_load,
            commands::theme_list,
            commands::theme_delete,
            commands::theme_parse_import,
            commands::theme_export_file,
            commands::consolidate_now,
            watch::watch_cards,
            watch::unwatch_cards,
            stimport::import_st_card,
            stimport::preview_st_card,
        ])
        .run(tauri::generate_context!())
        .expect("error while running huajing");
}
