//! Tauri 命令层：前端可调用的入口。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::ipc::Channel;
use tauri::State;

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
pub fn new_session(
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
        }
    }
    Ok(meta)
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
fn assemble_prompt(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    history: &[Message],
    user_content: Option<&str>,
) -> Result<prompt::PromptAssembly, String> {
    let settings = store::load_settings(root).map_err(|e| e.to_string())?;
    let persona = match &meta.persona {
        Some(name) => store::list_personas(root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| &p.name == name),
        None => None,
    };
    // state.json 为空时降级用卡上 default_state（M1.6 接入持久化回写）
    let mut card_state = store::load_state(root, &meta.id).map_err(|e| e.to_string())?;
    if card_state.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        card_state = loaded.default_state.clone();
    }
    let blackboard = store::load_blackboard(root, &meta.id).map_err(|e| e.to_string())?;

    // B5：on_context hook 注入（降级卡不执行；窗口给最近消息）
    let start = history.len().saturating_sub(prompt::WINDOW_MESSAGES);
    let hook_injections = if loaded.degraded {
        Vec::new()
    } else {
        card::run_hook(
            &loaded.source,
            card::HookCall::OnContext { window: &history[start..] },
            card_state.clone(),
            meta.seed,
        )
        .injections
    };

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
    Ok(prompt::build(&inputs))
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
async fn stream_reply(
    root: &std::path::Path,
    session_id: &str,
    turn: u64,
    provider: &Provider,
    assembly: prompt::PromptAssembly,
    on_event: &Channel<StreamEvent>,
    flags: &CancelFlags,
    msg_log: &store::MessageLog,
    assemblies: &LastAssemblies,
) -> Result<StreamEvent, String> {
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
            }
            Ok(StreamEvent::Done {
                full: outcome.text,
                cancelled: outcome.cancelled,
            })
        }
        Err(message) => Ok(StreamEvent::Error { message }),
    }
}

/// 发送一条用户消息并流式生成回复。
/// 流事件经 `on_event` 通道推给前端（delta / done / error），
/// 返回值即终态事件。用户消息先落盘；回复（含中断时的部分文本）生成后落盘。
#[tauri::command]
pub async fn send_message(
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
    let assembly = assemble_prompt(&root, &meta, &loaded, &history, Some(&content))?;

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
        &root,
        &session_id,
        turn,
        &provider,
        assembly,
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

    let assembly = assemble_prompt(&root, &meta, &loaded, &prior, Some(&content))?;
    stream_reply(
        &root,
        &session_id,
        turn,
        &provider,
        assembly,
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
    assemble_prompt(&root, &meta, &loaded, &history, None)
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
