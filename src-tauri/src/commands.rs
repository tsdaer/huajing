//! Tauri 命令层：前端可调用的入口。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::ipc::Channel;
use tauri::State;

use crate::card;
use crate::llm::{self, ChatMessage, Provider, StreamEvent};
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
) -> Result<store::SessionMeta, String> {
    let req = NewSessionRequest {
        character,
        persona,
        day,
        clock,
        place,
        premise,
    };
    store::new_session(&root(), &req).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_sessions() -> Result<Vec<store::SessionMeta>, String> {
    store::list_sessions(&root()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_messages(session_id: String) -> Result<Vec<Message>, String> {
    store::read_messages(&root(), &session_id).map_err(|e| e.to_string())
}

// ---------- 对话生成（设计 §4 流程 + §11 流式）----------

/// 每个会话的生成中断标记
#[derive(Default)]
pub struct CancelFlags(Mutex<HashMap<String, Arc<AtomicBool>>>);

/// 本地消息角色 → OpenAI 角色（char → assistant）
fn to_openai(m: &Message) -> ChatMessage {
    ChatMessage {
        role: match m.role.as_str() {
            "user" => "user",
            "system" => "system",
            _ => "assistant",
        }
        .into(),
        content: m.content.clone(),
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
    let providers = store::load_providers(&root).map_err(|e| e.to_string())?;
    let provider = providers
        .into_iter()
        .find(|p| p.role == "chat")
        .ok_or_else(|| "未配置 chat 档接入点，请先到设置页添加".to_string())?;

    // 组装消息（M1.3 最小组装：卡静态字段做系统头 + 全量历史；M1.4 换正式 Prompt Builder）
    let history = store::read_messages(&root, &session_id).map_err(|e| e.to_string())?;
    let turn = history.last().map(|m| m.turn).unwrap_or(0) + 1;
    let mut chat: Vec<ChatMessage> = Vec::new();
    if !loaded.card.scenario.is_empty() || !loaded.card.personality.is_empty() {
        chat.push(ChatMessage {
            role: "system".into(),
            content: format!("{}\n{}", loaded.card.scenario, loaded.card.personality),
        });
    }
    chat.extend(history.iter().map(to_openai));

    // 用户消息落盘后进入流式请求
    let user_msg = Message {
        turn,
        role: "user".into(),
        content: content.clone(),
        ts: store::unix_now(),
        scene_id: None,
    };
    store::append_message(&root, &session_id, &user_msg).map_err(|e| e.to_string())?;
    chat.push(ChatMessage {
        role: "user".into(),
        content,
    });

    // 中断标记：同会话并发防重
    let flag = {
        let mut map = flags
            .0
            .lock()
            .map_err(|_| "内部状态锁 poisoned".to_string())?;
        if let Some(f) = map.get(&session_id) {
            if !f.load(Ordering::Relaxed) {
                return Ok(StreamEvent::Error {
                    message: "上一条消息还在生成中".into(),
                });
            }
            map.remove(&session_id);
        }
        let flag = Arc::new(AtomicBool::new(false));
        map.insert(session_id.clone(), Arc::clone(&flag));
        flag
    };

    // 流式补全（取消检查在每个响应块之间）
    let stream = llm::chat_stream(&provider, &chat, |delta| {
        let _ = on_event.send(StreamEvent::Delta {
            text: delta.to_string(),
        });
    }, &flag)
    .await;

    // 清标记
    if let Ok(mut map) = flags.0.lock() {
        map.remove(&session_id);
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
                if let Err(e) = store::append_message(&root, &session_id, &reply) {
                    return Ok(StreamEvent::Error {
                        message: format!("回复落盘失败：{e}"),
                    });
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
