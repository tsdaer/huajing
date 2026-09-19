//! Tauri 命令层：前端可调用的入口。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

use crate::card;
use crate::llm::{self, Provider, StreamEvent};
use crate::prompt;
use crate::store::{self, Message, NewSessionRequest, Settings};

#[tauri::command]
pub fn app_info() -> serde_json::Value {
    serde_json::json!({
        "name": "化境 Huajing",
        "slogan": "扮谁，便入谁之境。",
        "version": env!("CARGO_PKG_VERSION"),
        "dataRoot": store::data_root(),
    })
}

fn root() -> std::path::PathBuf {
    store::data_root()
}

// ---------- providers（设计 §11）----------

#[tauri::command]
pub fn list_providers() -> Result<Vec<Provider>, String> {
    store::load_providers(&root()).map_err(|e| e.to_string())
}

/// 按名称 upsert，返回更新后的全量列表
#[tauri::command]
pub fn save_provider(provider: Provider) -> Result<Vec<Provider>, String> {
    store::upsert_provider(&root(), provider).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_provider(name: String) -> Result<Vec<Provider>, String> {
    store::delete_provider(&root(), &name).map_err(|e| e.to_string())
}

// ---------- settings ----------

#[tauri::command]
pub fn get_settings() -> Result<Settings, String> {
    store::load_settings(&root()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(settings: Settings) -> Result<Settings, String> {
    store::save_settings(&root(), &settings).map_err(|e| e.to_string())?;
    Ok(settings)
}

// ---------- personas ----------

#[tauri::command]
pub fn list_personas() -> Result<Vec<store::Persona>, String> {
    store::list_personas(&root()).map_err(|e| e.to_string())
}

// ---------- cards（设计 §3）----------

#[tauri::command]
pub fn list_cards() -> Result<Vec<card::CardSummary>, String> {
    Ok(card::list_cards(&root()))
}

#[tauri::command]
pub fn get_card(dir_name: String) -> Result<card::CardDetail, String> {
    card::load_card(&root(), &dir_name)
        .map(card::CardDetail::from)
        .map_err(|e| e.to_string())
}

// ---------- sessions（设计 §12）----------

#[tauri::command]
#[allow(clippy::too_many_arguments)] // 前端一次性传入向导的全部字段，保持扁平更好用
pub fn new_session(
    app: AppHandle,
    character: String,
    persona: Option<String>,
    day: Option<i64>,
    clock: Option<String>,
    place: Option<String>,
    premise: Option<String>,
    msg_log: State<'_, store::MessageLog>,
) -> Result<store::SessionMeta, String> {
    let root = root();
    let req = NewSessionRequest {
        character,
        persona,
        day,
        clock,
        place,
        premise,
    };
    let meta = store::new_session(&root, &req).map_err(|e| e.to_string())?;
    // first_mes 开场白：turn 0 的角色消息（设计 §3；卡片读取失败不阻塞建会话）
    if let Some(dir) = meta.characters.first() {
        if let Ok(loaded) = card::load_card(&root, dir) {
            let first = loaded.card.first_mes.trim();
            if !first.is_empty() {
                let opening = Message {
                    turn: 0,
                    role: "char".into(),
                    content: first.to_string(),
                    ts: store::unix_now(),
                    scene_id: None,
                };
                let _ = msg_log.append(&root, &meta.id, &opening);
            }
            // on_load：角色入席（设计 §3「生命周期钩子」）——state 就位、
            // 卡内可能顺手初始化黑板与长期记忆
            run_load_hook(&app, &root, &meta, &loaded, "hook.on_load")?;
        }
    }
    Ok(meta)
}

/// 跑 `on_load`（建会话、角色入席时一次）。与 on_message 共用同一套
/// 环境构造与落盘规则；环境构造失败在此上报（建会话不能带着半截状态继续）。
#[allow(clippy::too_many_arguments)] // 与 new_session 的参数一一对应，拆结构体反而绕
fn run_load_hook(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    source: &str,
) -> Result<llm::HookReport, String> {
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_load") {
        return Ok(llm::HookReport {
            turn: 0,
            ..Default::default()
        });
    }
    let mut state = load_card_state(root, meta, loaded)?;
    let mut blackboard = store::load_blackboard(root, &meta.id).map_err(|e| e.to_string())?;
    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnLoad,
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&blackboard),
            memory: BTreeMap::new(),
        },
        meta.seed,
        &ui_sink(app),
    );
    apply_load_hook(root, &meta.id, source, &run, &mut state, &mut blackboard)
}

/// `on_load` 的落盘：入席是「建立基线」——无论 hook 是否改动，都把生效后的 state
/// 写进会话快照（此后一律以会话为准，改卡的默认值不回头覆盖已有会话）。
/// 与 [`apply_message_hook`] 分开：on_message 只写「真变了的」，on_load 必写。
fn apply_load_hook(
    root: &std::path::Path,
    session_id: &str,
    source: &str,
    run: &card::HookRun,
    state: &mut serde_json::Value,
    blackboard: &mut store::Blackboard,
) -> Result<llm::HookReport, String> {
    let mut report = llm::HookReport {
        turn: 0,
        ran: run.ran(),
        logs: run.result.logs.clone(),
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
        memory: run.memory.clone(),
        ..Default::default()
    };
    let _ = apply_hook_state(state, run);
    let _ = merge_blackboard(blackboard, &run.blackboard);
    persist_hook_state(root, session_id, Some(&*state), Some(&*blackboard))?;
    persist_memory(root, session_id, source, 0, &run.memory);
    report.card_state = state.clone();
    Ok(report)
}

#[tauri::command]
pub fn list_sessions() -> Result<Vec<store::SessionMeta>, String> {
    store::list_sessions(&root()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_messages(
    session_id: String,
    msg_log: State<'_, store::MessageLog>,
) -> Result<std::sync::Arc<Vec<Message>>, String> {
    msg_log
        .read(&root(), &session_id)
        .map_err(|e| e.to_string())
}

/// 编辑指定下标的消息内容（全量重写 + 缓存失效），返回更新后的全量消息
#[tauri::command]
pub fn edit_message(
    session_id: String,
    index: usize,
    content: String,
    msg_log: State<'_, store::MessageLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let mut messages = msg_log
        .read(&root, &session_id)
        .map_err(|e| e.to_string())?
        .as_slice()
        .to_vec();
    let Some(m) = messages.get_mut(index) else {
        return Err(format!("消息下标越界：{index}"));
    };
    m.content = content;
    store::write_messages(&root, &session_id, &messages).map_err(|e| e.to_string())?;
    msg_log.invalidate(Some(&session_id));
    Ok(messages)
}

/// 删除指定下标的消息（全量重写 + 缓存失效），返回更新后的全量消息
#[tauri::command]
pub fn delete_message(
    session_id: String,
    index: usize,
    msg_log: State<'_, store::MessageLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let mut messages = msg_log
        .read(&root, &session_id)
        .map_err(|e| e.to_string())?
        .as_slice()
        .to_vec();
    if index >= messages.len() {
        return Err(format!("消息下标越界：{index}"));
    }
    messages.remove(index);
    store::write_messages(&root, &session_id, &messages).map_err(|e| e.to_string())?;
    msg_log.invalidate(Some(&session_id));
    Ok(messages)
}

// ---------- 对话生成（设计 §4 流程 + §11 流式）----------

/// 每个会话的生成中断标记
#[derive(Default)]
pub struct CancelFlags(Mutex<HashMap<String, Arc<AtomicBool>>>);

/// 每会话最近一次实际发送的组装结果（记忆检查器"本次注入"数据源）
#[derive(Default)]
pub struct LastAssemblies(Mutex<HashMap<String, prompt::PromptAssembly>>);

/// 组装一轮上下文（send_message 与 preview_prompt 共用）。
/// `user_content` = Some 时为本轮真实发送（末尾带用户消息）；None 为检查器预览。
/// 本轮组装的产物：注入层 + 被 hook 顺带改动的宿主状态（调用方负责落盘）。
struct PromptRun {
    assembly: prompt::PromptAssembly,
    /// 生效后的角色 state（on_context 可能原地改过）
    card_state: serde_json::Value,
    /// on_context 原地改过 state（需要回写 state.json）
    state_dirty: bool,
    /// 生效后的黑板（on_context 可能经 api.blackboard.set 改过）
    blackboard: store::Blackboard,
    /// 黑板被 hook 改过（需要回写 blackboard.json）
    blackboard_dirty: bool,
    /// on_context 期间 `api.ui.emit` 的界面事件
    ui_events: Vec<llm::UiEmit>,
}

/// 角色 state：优先会话快照，空快照（会话初始/被清）降级用卡上 default_state
fn load_card_state(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
) -> Result<serde_json::Value, String> {
    let mut state = store::load_state(root, &meta.id).map_err(|e| e.to_string())?;
    if state.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        state = loaded.default_state.clone();
    }
    Ok(state)
}

/// 黑板 → hook 读侧快照（`api.blackboard.get` 的数据源）
fn blackboard_env(bb: &store::Blackboard) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert("day".into(), serde_json::json!(bb.day));
    map.insert("clock".into(), serde_json::json!(bb.clock));
    map.insert("place".into(), serde_json::json!(bb.place));
    map.insert("actors".into(), serde_json::json!(bb.actors));
    map
}

/// 把 `api.blackboard.set` 的写入合并进黑板（只认黑白板 v0 字段；类型不符即忽略）。
/// 返回是否有实际改动。
fn merge_blackboard(bb: &mut store::Blackboard, sets: &[card::KvSet]) -> bool {
    let before = format!("{bb:?}");
    for kv in sets {
        match (kv.key.as_str(), &kv.value) {
            ("day", v) => {
                if let Some(n) = v.as_i64() {
                    bb.day = n;
                }
            }
            ("clock", v) => {
                if let Some(s) = v.as_str() {
                    bb.clock = s.to_string();
                }
            }
            ("place", v) => {
                if let Some(s) = v.as_str() {
                    bb.place = s.to_string();
                }
            }
            ("actors", v) => {
                if let Some(arr) = v.as_array() {
                    bb.actors = arr
                        .iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect();
                }
            }
            _ => {} // 白名单已在沙箱侧拦住；这里兜底忽略
        }
    }
    format!("{bb:?}") != before
}

/// 组装一轮上下文（send_message / regenerate / preview_prompt 共用）。
///
/// `user_content` = Some 时为本轮真实发送（末尾带用户消息）；None 为检查器预览。
/// on_context hook 在此运行（B5 注入时机，设计 §4.1）：其 state / 黑板改动
/// 就地生效并标记 dirty，由调用方决定何时落盘（预览也会落盘——卡片的
/// on_context 本就允许改状态，与是否真的发送无关）。
fn assemble_prompt(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    history: &[Message],
    user_content: Option<&str>,
) -> Result<PromptRun, String> {
    let settings = store::load_settings(root).map_err(|e| e.to_string())?;
    let persona = match &meta.persona {
        Some(name) => store::list_personas(root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| &p.name == name),
        None => None,
    };
    let mut card_state = load_card_state(root, meta, loaded)?;
    let mut blackboard = store::load_blackboard(root, &meta.id).map_err(|e| e.to_string())?;

    // B5：on_context hook（降级卡与未定义该 hook 的卡都跳过；窗口给最近消息）
    let start = history.len().saturating_sub(prompt::WINDOW_MESSAGES);
    let mut hook_injections = Vec::new();
    let mut state_dirty = false;
    let mut blackboard_dirty = false;
    let mut ui_events = Vec::new();
    if !loaded.degraded && loaded.hook_names.iter().any(|h| h == "on_context") {
        let mut hook_state = card_state.clone();
        let mut hook_board = blackboard.clone();
        let sink = ui_sink(app);
        let run = card::run_hook_full(
            &loaded.source,
            card::HookCall::OnContext { window: &history[start..] },
            &card::HookEnv {
                state: hook_state.clone(),
                blackboard: blackboard_env(&blackboard),
                memory: BTreeMap::new(), // 长期记忆读侧（记忆宫殿）在 M2
            },
            meta.seed,
            &sink,
        );
        hook_injections = run.result.injections.clone();
        ui_events = run.result.ui_events.iter().map(ui_emit).collect();
        state_dirty = apply_hook_state(&mut hook_state, &run);
        blackboard_dirty = merge_blackboard(&mut hook_board, &run.blackboard);
        card_state = hook_state;
        blackboard = hook_board;
    }

    let inputs = prompt::BuildInputs {
        settings: &settings,
        persona: persona.as_ref(),
        card: &loaded.card,
        card_state: &card_state,
        blackboard: &blackboard,
        hook_injections: &hook_injections,
        history,
        user_content,
    };
    Ok(PromptRun {
        assembly: prompt::build(&inputs),
        card_state,
        state_dirty,
        blackboard,
        blackboard_dirty,
        ui_events,
    })
}

/// 把 `ui.emit` 实时转推前端（表情/立绘位等）。emit 失败（窗口已关等）静默忽略。
fn ui_sink(app: &AppHandle) -> card::UiSink {
    let app = app.clone();
    Arc::new(move |event: &card::UiEvent| {
        let _ = app.emit(
            "hook_event",
            serde_json::json!({ "kind": event.kind, "value": event.value }),
        );
    })
}

/// hook 运行后的 state 回传到 `card_state`；返回是否真的变了
fn apply_hook_state(card_state: &mut serde_json::Value, run: &card::HookRun) -> bool {
    match &run.state {
        Some(next) if next != card_state => {
            *card_state = next.clone();
            true
        }
        _ => false,
    }
}

/// 卡片事件 → 流事件类型（字段一致，避免同一概念两处定义）
fn ui_emit(event: &card::UiEvent) -> llm::UiEmit {
    llm::UiEmit {
        kind: event.kind.clone(),
        value: event.value.clone(),
    }
}

/// 落盘 state / 黑板（调用方决定失败是中断还是仅记日志）
fn persist_hook_state(
    root: &std::path::Path,
    session_id: &str,
    state: Option<&serde_json::Value>,
    blackboard: Option<&store::Blackboard>,
) -> Result<(), String> {
    if let Some(state) = state {
        store::save_state(root, session_id, state).map_err(|e| e.to_string())?;
    }
    if let Some(bb) = blackboard {
        store::save_blackboard(root, session_id, bb).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `api.memory.set` 的写入落 palace.jsonl（设计 §12；读侧由记忆宫殿在 M2 提供）
fn persist_memory(
    root: &std::path::Path,
    session_id: &str,
    source: &str,
    turn: u64,
    sets: &[card::KvSet],
) {
    for kv in sets {
        let rec = store::MemRecord {
            kind: "fact".into(),
            key: kv.key.clone(),
            value: kv.value.clone(),
            source: source.into(),
            turn,
            ts: store::unix_now(),
        };
        if let Err(e) = store::append_memory_record(root, session_id, &rec) {
            eprintln!("[huajing] palace.jsonl 写入失败：{e}");
        }
    }
}

/// 取 chat 档接入点（设计 §11 分档）
fn pick_chat_provider(root: &std::path::Path) -> Result<Provider, String> {
    store::load_providers(root)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.role == "chat")
        .ok_or_else(|| "未配置 chat 档接入点，请先到设置页添加".to_string())
}

/// 取会话的中断标记；同会话已有进行中的生成时返回并发错误事件
fn acquire_flag(flags: &CancelFlags, session_id: &str) -> Result<Arc<AtomicBool>, StreamEvent> {
    let mut map = flags.0.lock().map_err(|_| StreamEvent::Error {
        message: "内部状态锁 poisoned".into(),
    })?;
    if let Some(f) = map.get(session_id) {
        if !f.load(Ordering::Relaxed) {
            return Err(StreamEvent::Error {
                message: "上一条消息还在生成中".into(),
            });
        }
        map.remove(session_id);
    }
    let flag = Arc::new(AtomicBool::new(false));
    map.insert(session_id.to_string(), Arc::clone(&flag));
    Ok(flag)
}

/// 流式生成的后半程（send_message 与 regenerate 共用）：
/// 中断标记 → 流式补全 → 回复落盘 → 时钟步进 → 记录组装（记忆检查器）。
#[allow(clippy::too_many_arguments)]
async fn stream_reply(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    turn: u64,
    provider: &Provider,
    assembly: prompt::PromptAssembly,
    on_event: &Channel<StreamEvent>,
    flags: &CancelFlags,
    msg_log: &store::MessageLog,
    assemblies: &LastAssemblies,
) -> Result<StreamEvent, String> {
    let session_id = meta.id.as_str();
    let flag = match acquire_flag(flags, session_id) {
        Ok(f) => f,
        Err(e) => return Ok(e),
    };

    // 流式补全（取消检查在每个响应块之间）
    let chat = assembly.messages.clone();
    let stream = llm::chat_stream(provider, &chat, |delta| {
        let _ = on_event.send(StreamEvent::Delta {
            text: delta.to_string(),
        });
    }, &flag)
    .await;

    // 清标记；记录本次组装（记忆检查器）
    if let Ok(mut map) = flags.0.lock() {
        map.remove(session_id);
    }
    if let Ok(mut map) = assemblies.0.lock() {
        map.insert(session_id.to_string(), assembly);
    }

    match stream {
        Ok(outcome) => {
            let mut report = None;
            if !outcome.text.is_empty() {
                let reply = Message {
                    turn,
                    role: "char".into(),
                    content: outcome.text.clone(),
                    ts: store::unix_now(),
                    scene_id: None,
                };
                if let Err(e) = msg_log.append(root, session_id, &reply) {
                    return Ok(StreamEvent::Error {
                        message: format!("回复落盘失败：{e}"),
                    });
                }
                // 一轮完成：黑板时钟步进（设计 M1：每轮 +10 分钟，跨日进位）
                if let Ok(mut bb) = store::load_blackboard(root, session_id) {
                    let (day, clock) = prompt::advance_clock(bb.day, &bb.clock);
                    bb.day = day;
                    bb.clock = clock;
                    let _ = store::save_blackboard(root, session_id, &bb);
                }
                // M1.6：回复落盘后跑 on_message（设计 §3：每条新消息落地后调用）
                report = Some(run_message_hook(app, root, meta, loaded, turn, on_event));
            }
            Ok(StreamEvent::Done {
                full: outcome.text,
                cancelled: outcome.cancelled,
                report,
            })
        }
        Err(message) => Ok(StreamEvent::Error { message }),
    }
}

/// 跑一轮 `on_message`（用户消息与回复都已落盘后调用，设计 §3）。
///
/// 卡片没定义该 hook 时直接返回 ran=false（不新建 Lua 实例）；其余情况：
/// state 原地改动、`api.memory` / `api.blackboard` / `api.ui.emit` 的写入
/// 分别落 state.json、palace.jsonl、blackboard.json，并实时推前端。
/// 任何失败都只进报告（错误边界），不影响本轮对话。
fn run_message_hook(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    turn: u64,
    on_event: &Channel<StreamEvent>,
) -> llm::HookReport {
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_message") {
        return llm::HookReport {
            turn,
            ..Default::default()
        };
    }
    // 钩子看到的最新一条消息：用户消息或刚落盘的角色回复
    let messages = match store::read_messages(root, &meta.id) {
        Ok(m) => m,
        Err(e) => {
            return report_with_log(turn, format!("历史读取失败：{e}"));
        }
    };
    let Some(current) = messages.last() else {
        return report_with_log(turn, "没有可处理的消息".into());
    };
    let mut state = match load_card_state(root, meta, loaded) {
        Ok(s) => s,
        Err(e) => return report_with_log(turn, e),
    };
    let mut blackboard = match store::load_blackboard(root, &meta.id) {
        Ok(bb) => bb,
        Err(e) => return report_with_log(turn, format!("黑板读取失败：{e}")),
    };

    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnMessage { msg: current },
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&blackboard),
            memory: BTreeMap::new(), // 长期记忆读侧（记忆宫殿）在 M2
        },
        meta.seed,
        &ui_sink(app),
    );
    let report = apply_message_hook(root, &meta.id, turn, &run, &mut state, &mut blackboard);
    for event in &report.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }
    report
}

fn report_with_log(turn: u64, log: String) -> llm::HookReport {
    llm::HookReport {
        turn,
        logs: vec![log],
        ..Default::default()
    }
}

/// 把一轮 `on_message` 的结果落盘：state → state.json、黑板 → blackboard.json、
/// `api.memory` 写入 → palace.jsonl，并汇总成给前端的钩子报告。
///
/// 与 `run_message_hook` 分开，是为了让「钩子副作用真的落盘」这件事能被单测直接钉住
/// （不必启动 Tauri 运行时）。
fn apply_message_hook(
    root: &std::path::Path,
    session_id: &str,
    turn: u64,
    run: &card::HookRun,
    state: &mut serde_json::Value,
    blackboard: &mut store::Blackboard,
) -> llm::HookReport {
    let mut report = llm::HookReport {
        turn,
        ran: run.ran(),
        logs: run.result.logs.clone(),
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
        memory: run.memory.clone(),
        ..Default::default()
    };

    let state_changed = apply_hook_state(state, run);
    let board_changed = merge_blackboard(blackboard, &run.blackboard);
    if let Err(e) = persist_hook_state(
        root,
        session_id,
        state_changed.then_some(&*state),
        board_changed.then_some(&*blackboard),
    ) {
        report.logs.push(e);
    }
    persist_memory(root, session_id, "hook.on_message", turn, &run.memory);
    report.card_state = state.clone();
    report
}

/// 发送一条用户消息并流式生成回复。
/// 流事件经 `on_event` 通道推给前端（delta / done / error），
/// 返回值即终态事件。用户消息先落盘；回复（含中断时的部分文本）生成后落盘。
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    session_id: String,
    content: String,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    msg_log: State<'_, store::MessageLog>,
    assemblies: State<'_, LastAssemblies>,
) -> Result<StreamEvent, String> {
    let root = root();

    // 会话与角色卡
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = meta
        .characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;

    // 接入点（chat 档；先校验再落盘用户消息，配置错误不产生半截会话）
    let provider = pick_chat_provider(&root)?;

    // 双槽位组装（设计 §4.1）：历史读取走增量缓存，高轮次只解析新增行
    let history = msg_log
        .read(&root, &session_id)
        .map_err(|e| e.to_string())?;
    let turn = history.last().map(|m| m.turn).unwrap_or(0) + 1;
    // on_context 在此运行：卡片可能顺手改了 state/黑板/界面事件，先落盘再生成本轮
    let run = assemble_prompt(&app, &root, &meta, &loaded, &history, Some(&content))?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }
    if run.state_dirty || run.blackboard_dirty {
        persist_hook_state(
            &root,
            &session_id,
            run.state_dirty.then_some(&run.card_state),
            run.blackboard_dirty.then_some(&run.blackboard),
        )?;
    }

    // 用户消息落盘后进入流式请求
    let user_msg = Message {
        turn,
        role: "user".into(),
        content: content.clone(),
        ts: store::unix_now(),
        scene_id: None,
    };
    msg_log
        .append(&root, &session_id, &user_msg)
        .map_err(|e| e.to_string())?;

    stream_reply(
        &app,
        &root,
        &meta,
        &loaded,
        turn,
        &provider,
        run.assembly,
        &on_event,
        &flags,
        &msg_log,
        &assemblies,
    )
    .await
}

/// 重roll（设计 §4 消息级操作）：移除末尾角色回复，以最后一条用户消息
/// 重新流式生成。先删后生成——失败也不会出现两条并列回复。
#[tauri::command]
pub async fn regenerate(
    app: AppHandle,
    session_id: String,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    msg_log: State<'_, store::MessageLog>,
    assemblies: State<'_, LastAssemblies>,
) -> Result<StreamEvent, String> {
    let root = root();

    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = meta
        .characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;
    let provider = pick_chat_provider(&root)?;

    let mut messages = msg_log
        .read(&root, &session_id)
        .map_err(|e| e.to_string())?
        .as_slice()
        .to_vec();

    // 末尾必须是角色回复，其前必须有用户消息
    match messages.last() {
        Some(m) if m.role == "char" => {
            messages.pop();
        }
        _ => {
            return Ok(StreamEvent::Error {
                message: "末尾没有可重roll的角色回复".into(),
            })
        }
    }
    let Some(user_msg) = messages.last().filter(|m| m.role == "user") else {
        return Ok(StreamEvent::Error {
            message: "角色回复前找不到用户消息，无法重roll".into(),
        });
    };
    let (turn, content) = (user_msg.turn, user_msg.content.clone());
    let prior: Vec<Message> = messages[..messages.len() - 1].to_vec();

    // 先移除末尾回复（文件与缓存同步），再组装生成
    store::write_messages(&root, &session_id, &messages).map_err(|e| e.to_string())?;
    msg_log.invalidate(Some(&session_id));

    // 重roll 前先让 on_context 按当前（已删掉末尾回复的）历史跑一轮
    let run = assemble_prompt(&app, &root, &meta, &loaded, &prior, Some(&content))?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }
    if run.state_dirty || run.blackboard_dirty {
        persist_hook_state(
            &root,
            &session_id,
            run.state_dirty.then_some(&run.card_state),
            run.blackboard_dirty.then_some(&run.blackboard),
        )?;
    }
    stream_reply(
        &app,
        &root,
        &meta,
        &loaded,
        turn,
        &provider,
        run.assembly,
        &on_event,
        &flags,
        &msg_log,
        &assemblies,
    )
    .await
}

/// 中断某会话正在进行的生成（保留已生成的部分文本）
#[tauri::command]
pub fn stop_generation(session_id: String, flags: State<'_, CancelFlags>) -> Result<bool, String> {
    let mut map = flags
        .0
        .lock()
        .map_err(|_| "内部状态锁 poisoned".to_string())?;
    match map.remove(&session_id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            Ok(true)
        }
        None => Ok(false),
    }
}

// ---------- 黑板 v0（设计 §4.1 B1 数据源）----------

#[tauri::command]
pub fn get_blackboard(session_id: String) -> Result<store::Blackboard, String> {
    store::load_blackboard(&root(), &session_id).map_err(|e| e.to_string())
}

/// 手动编辑黑板（全量替换；保存后下一轮组装生效）
#[tauri::command]
pub fn update_blackboard(
    session_id: String,
    day: i64,
    clock: String,
    place: String,
    actors: Vec<String>,
) -> Result<store::Blackboard, String> {
    let bb = store::Blackboard {
        day,
        clock: clock.trim().to_string(),
        place: place.trim().to_string(),
        actors: actors.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(),
    };
    store::save_blackboard(&root(), &session_id, &bb).map_err(|e| e.to_string())?;
    Ok(bb)
}

// ---------- 记忆检查器 v0（设计 §4.2：组装结果逐层可见）----------

/// 预览组装：按当前状态干跑一轮（不含用户消息），不发送
#[tauri::command]
pub fn preview_prompt(
    app: AppHandle,
    session_id: String,
    msg_log: State<'_, store::MessageLog>,
) -> Result<prompt::PromptAssembly, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = meta
        .characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;
    let history = msg_log
        .read(&root, &session_id)
        .map_err(|e| e.to_string())?;
    let run = assemble_prompt(&app, &root, &meta, &loaded, &history, None)?;
    // 预览也落盘：on_context 允许改状态（与是否真的发送无关），不落盘会出现
    // 「预览一次状态变了、正式发送又变一次」的漂移
    if run.state_dirty || run.blackboard_dirty {
        persist_hook_state(
            &root,
            &session_id,
            run.state_dirty.then_some(&run.card_state),
            run.blackboard_dirty.then_some(&run.blackboard),
        )?;
    }
    Ok(run.assembly)
}

// ---------- 卡内状态与长期记忆（M1.6：hooks 的可观测面）----------

/// 角色私有 state 现状（会话快照为空时回退卡上 `state` 初始值）
#[tauri::command]
pub fn get_card_state(session_id: String) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = meta
        .characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;
    load_card_state(&root, &meta, &loaded)
}

/// 卡内长期记忆写入流（palace.jsonl；读侧召回在 M2 接记忆宫殿）
#[tauri::command]
pub fn list_card_memory(session_id: String) -> Result<Vec<store::MemRecord>, String> {
    store::read_memory_records(&root(), &session_id).map_err(|e| e.to_string())
}

/// 最近一次实际发送的组装（无记录返回 None，前端可回退到预览）
#[tauri::command]
pub fn last_prompt(
    session_id: String,
    assemblies: State<'_, LastAssemblies>,
) -> Result<Option<prompt::PromptAssembly>, String> {
    let map = assemblies
        .0
        .lock()
        .map_err(|_| "内部状态锁 poisoned".to_string())?;
    Ok(map.get(&session_id).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 DataHub/characters/小雨/card.lua 同构的行为卡（M1.6 验收用的最小版）
    const HOOK_CARD: &str = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  hooks = {
    on_load = function(state, api) api.ui.emit('emotion', 'calm') end,
    on_context = function(ctx, state)
      ctx.inject('system', string.format('【角色内部状态】好感度 %d/100', state.favorability))
    end,
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('谢谢') then
        state.favorability = math.min(100, state.favorability + 1)
        api.memory.set('last_thanked', msg.turn)
        api.blackboard.set('place', '天台')
      end
      api.ui.emit('emotion', state.favorability >= 80 and 'shy' or 'calm')
    end,
  },
}
"#;

    /// 建一个临时 DataHub + 会话（表驱动：单测不碰真实用户数据）
    fn setup(card_src: &str) -> (tempfile::TempDir, store::SessionMeta, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        store::ensure_layout(&root).unwrap();
        let card_dir = root.join("characters/小雨");
        std::fs::create_dir_all(&card_dir).unwrap();
        std::fs::write(card_dir.join("card.lua"), card_src).unwrap();
        let meta = store::new_session(
            &root,
            &store::NewSessionRequest {
                character: "小雨".into(),
                persona: None,
                day: Some(1),
                clock: Some("20:00".into()),
                place: Some("自习区".into()),
                premise: None,
            },
        )
        .unwrap();
        (dir, meta, root)
    }

    fn user_msg(turn: u64, content: &str) -> Message {
        Message {
            turn,
            role: "user".into(),
            content: content.into(),
            ts: store::unix_now(),
            scene_id: None,
        }
    }

    /// 一轮完整生成：on_context 组装（含 hook）→ 用户消息落盘 → 回复落盘 → on_message
    fn simulate_turn(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        loaded: &card::LoadedCard,
        log: &store::MessageLog,
        turn: u64,
        content: &str,
) -> (prompt::PromptAssembly, llm::HookReport) {
        let history = log.read(root, &meta.id).unwrap();
        let run = assemble_prompt_for(root, meta, loaded, &history, Some(content));
        if run.state_dirty || run.blackboard_dirty {
            persist_hook_state(
                root,
                &meta.id,
                run.state_dirty.then_some(&run.card_state),
                run.blackboard_dirty.then_some(&run.blackboard),
            )
            .unwrap();
        }
        // 与生产一致：用户消息落盘后跑一次，回复落盘后再跑一次
        log.append(root, &meta.id, &user_msg(turn, content)).unwrap();
        let after_user = run_message_hook_offline(root, meta, loaded, turn);
        log.append(
            root,
            &meta.id,
            &Message {
                turn,
                role: "char".into(),
                content: "（回复）".into(),
                ts: store::unix_now(),
                scene_id: None,
            },
        )
        .unwrap();
        let after_reply = run_message_hook_offline(root, meta, loaded, turn);

        // 报告取「本轮最后一次」（回复后的状态就是前端看到的最终状态）
        let mut report = after_reply;
        report.ran |= after_user.ran;
        report.memory = [after_user.memory, report.memory].concat();
        (run.assembly, report)
    }

    /// 与 assemble_prompt 同逻辑但不带 AppHandle（单测不启动 Tauri）
    fn assemble_prompt_for(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        loaded: &card::LoadedCard,
        history: &[Message],
        user_content: Option<&str>,
    ) -> PromptRun {
        let mut card_state = load_card_state(root, meta, loaded).unwrap();
        let mut blackboard = store::load_blackboard(root, &meta.id).unwrap();
        let start = history.len().saturating_sub(prompt::WINDOW_MESSAGES);
        let mut hook_injections = Vec::new();
        let mut state_dirty = false;
        let mut blackboard_dirty = false;
        let mut ui_events = Vec::new();
        if !loaded.degraded && loaded.hook_names.iter().any(|h| h == "on_context") {
            let run = card::run_hook_full(
                &loaded.source,
                card::HookCall::OnContext { window: &history[start..] },
                &card::HookEnv {
                    state: card_state.clone(),
                    blackboard: blackboard_env(&blackboard),
                    memory: BTreeMap::new(),
                },
                meta.seed,
                &noop_sink(),
            );
            hook_injections = run.result.injections.clone();
            ui_events = run.result.ui_events.iter().map(ui_emit).collect();
            state_dirty = apply_hook_state(&mut card_state, &run);
            blackboard_dirty = merge_blackboard(&mut blackboard, &run.blackboard);
        }
        let settings = store::load_settings(root).unwrap();
        let inputs = prompt::BuildInputs {
            settings: &settings,
            persona: None,
            card: &loaded.card,
            card_state: &card_state,
            blackboard: &blackboard,
            hook_injections: &hook_injections,
            history,
            user_content,
        };
        PromptRun {
            assembly: prompt::build(&inputs),
            card_state,
            state_dirty,
            blackboard,
            blackboard_dirty,
            ui_events,
        }
    }

    fn noop_sink() -> card::UiSink {
        std::sync::Arc::new(|_: &card::UiEvent| {})
    }

    /// run_message_hook 的离线等价物（去掉 AppHandle 与前端通道）
    fn run_message_hook_offline(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        loaded: &card::LoadedCard,
        turn: u64,
    ) -> llm::HookReport {
        if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_message") {
            return llm::HookReport {
                turn,
                ..Default::default()
            };
        }
        let mut state = load_card_state(root, meta, loaded).unwrap();
        let mut blackboard = store::load_blackboard(root, &meta.id).unwrap();
        let messages = store::read_messages(root, &meta.id).unwrap();
        let last = messages.last().unwrap();
        let hook_run = card::run_hook_full(
            &loaded.source,
            card::HookCall::OnMessage { msg: last },
            &card::HookEnv {
                state: state.clone(),
                blackboard: blackboard_env(&blackboard),
                memory: BTreeMap::new(),
            },
            meta.seed,
            &noop_sink(),
        );
        apply_message_hook(root, &meta.id, turn, &hook_run, &mut state, &mut blackboard)
    }

    #[test]
    fn hooks_persist_state_memory_and_blackboard() {
        // 对应 M1.6 验收：好感度随对话变化、写入落盘、重启后仍读得到
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        assert_eq!(loaded.hook_names.len(), 3, "示例卡应带三个 hook");
        let log = store::MessageLog::new();

        // 第一轮：on_context 注入的内部状态是卡上初始值 50
        let (assembly, report) = simulate_turn(&root, &meta, &loaded, &log, 1, "今天好冷。");
        assert!(
            assembly
                .layers
                .iter()
                .any(|l| l.id == "B5" && l.content.contains("好感度 50")),
            "B5 层应带上卡内状态：{:?}",
            assembly.layers.iter().map(|l| &l.id).collect::<Vec<_>>()
        );
        assert!(report.ran);
        assert_eq!(report.card_state["favorability"], 50, "没道谢就不涨");
        assert!(store::read_memory_records(&root, &meta.id).unwrap().is_empty());

        // 第二轮：说「谢谢」→ 好感度 +1，state.json 与 palace.jsonl 都落盘
        let (_, report) = simulate_turn(&root, &meta, &loaded, &log, 2, "谢谢你。");
        assert_eq!(report.card_state["favorability"], 51);
        assert_eq!(report.memory.len(), 1);
        assert_eq!(report.memory[0].key, "last_thanked");
        // 落盘：直接读文件（重启 App 等价于这一步）
        let state = store::load_state(&root, &meta.id).unwrap();
        assert_eq!(state["favorability"], 51, "state.json 应记住好感度");
        let records = store::read_memory_records(&root, &meta.id).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].value, serde_json::json!(2));
        assert_eq!(records[0].source, "hook.on_message");
        let bb = store::load_blackboard(&root, &meta.id).unwrap();
        assert_eq!(bb.place, "天台", "api.blackboard.set 应写进黑板");

        // 第三轮：重启后的等效状态继续演进（读磁盘 → 51 → 52）
        let (assembly, report) = simulate_turn(&root, &meta, &loaded, &log, 3, "谢谢你帮我。");
        assert_eq!(report.card_state["favorability"], 52);
        assert!(assembly
            .layers
            .iter()
            .any(|l| l.id == "B5" && l.content.contains("好感度 51")),
            "本轮注入用的应是上一轮存下的值");
        assert_eq!(store::read_memory_records(&root, &meta.id).unwrap().len(), 2);
    }

    #[test]
    fn static_card_runs_no_hooks_and_keeps_state_empty() {
        let plain = "return { spec='charcard/1.0', name='静卡', scenario='s', personality='p', first_mes='f' }";
        let (_dir, meta, root) = setup(plain);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::MessageLog::new();
        let (assembly, report) = simulate_turn(&root, &meta, &loaded, &log, 1, "你好");
        assert!(!report.ran);
        // 没有 on_context：B5 层不该出现（空层省略）
        assert!(!assembly.layers.iter().any(|l| l.id == "B5"));
        // 也不该写 state/palace
        assert_eq!(store::load_state(&root, &meta.id).unwrap(), serde_json::json!({}));
        assert!(store::read_memory_records(&root, &meta.id).unwrap().is_empty());
    }

    #[test]
    fn on_load_initialises_state_from_card_defaults() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        // 会话初始 state.json 是空对象：卡上默认值尚未落到会话（这正是 on_load 的职责）
        let initial = load_card_state(&root, &meta, &loaded).unwrap();
        assert_eq!(initial, serde_json::json!({ "favorability": 50 }), "空快照降级用卡上默认值");
        assert_eq!(store::load_state(&root, &meta.id).unwrap(), serde_json::json!({}));

        // 生产路径：new_session 用 load_card_state 的初值喂 on_load，再落盘
        let run = card::run_hook_full(
            &loaded.source,
            card::HookCall::OnLoad,
            &card::HookEnv {
                state: initial.clone(),
                ..Default::default()
            },
            meta.seed,
            &noop_sink(),
        );
        assert!(run.ran());
        assert_eq!(run.result.ui_events[0].value, "calm");
        let mut state = initial;
        let mut blackboard = store::load_blackboard(&root, &meta.id).unwrap();
        let report = apply_load_hook(
            &root,
            &meta.id,
            "hook.on_load",
            &run,
            &mut state,
            &mut blackboard,
        )
        .unwrap();
        assert_eq!(report.card_state["favorability"], 50);
        // 入席必写基线：即使 hook 没改状态，state.json 也要就位
        assert_eq!(
            store::load_state(&root, &meta.id).unwrap()["favorability"],
            50
        );
    }
}
