//! Tauri 命令层：前端可调用的入口。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

use crate::card;
use crate::event::{self, LogBody, LogRecord};
use crate::llm::{self, Provider, StreamEvent};
use crate::prompt;
use crate::store::{self, Message, NewSessionRequest, Settings};

#[tauri::command]
pub fn app_info() -> serde_json::Value {
    serde_json::json!({
        "name": "化境 Huajing",
        "slogan": "扮谁，便入谁之境。",
        "version": env!("CARGO_PKG_VERSION"),
        // 编译期注入（build.rs）：界面显示它，就能一眼看出跑的是哪一版
        "buildTs": env!("HUAJING_BUILD_TS").parse::<u64>().unwrap_or(0),
        "dataRoot": store::data_root(),
    })
}

/// 前端记一条诊断（拖放被忽略等只有前端知道的事）
#[tauri::command]
pub fn record_diagnostic(kind: String, detail: String) {
    crate::diag::record(&kind, detail);
}

/// 最近的运行时诊断（钩子/导入/错误的关键决策），新的在前
#[tauri::command]
pub fn recent_diagnostics(limit: Option<usize>) -> Vec<crate::diag::DiagRecord> {
    crate::diag::recent(limit.unwrap_or(60).min(200))
}

/// 运行环境速览（设置页「运行环境」）：这是当前进程真正在用的路径与计数，
/// 排查「我改了卡怎么没用」时第一眼看这里——很可能是应用读的不是你改的那份。
#[tauri::command]
pub fn runtime_info() -> Result<serde_json::Value, String> {
    let root = root();
    let sessions = store::list_sessions(&root).map_err(|e| e.to_string())?.len();
    let cards = card::list_cards(&root).len();
    Ok(serde_json::json!({
        "dataRoot": root,
        "cardCount": cards,
        "sessionCount": sessions,
        "now": store::unix_now(),
        "buildTs": env!("HUAJING_BUILD_TS").parse::<u64>().unwrap_or(0),
    }))
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

/// 一次连通性自检的结果（设置页「测试」按钮）
#[derive(Debug, Clone, Serialize)]
pub struct ProviderTest {
    pub ok: bool,
    /// 人类可读的结论或错误（错误含 reqwest 的完整原因链）
    pub message: String,
    /// 实际请求的 URL（base_url 少写 /v1 之类一眼可见）
    pub url: String,
    pub model: String,
    /// 服务端原样返回的响应体摘要（成功时用于确认模型确实回了话）
    pub detail: String,
    pub elapsed_ms: u64,
    /// 这台机器上配了的代理（出网失败时的第一条线索）
    pub proxy: Vec<String>,
    /// 本次实际采用的代理（含来源）；None = 直连
    pub proxy_used: Option<String>,
}

/// 测试一个接入点是否真的能用：发一条最小请求（非流式，60s 上限）。
/// 覆盖三类常见故障：key 无效 / 地址写错（404 或连不上）/ 出网被拦（TLS 与代理）。
#[tauri::command]
pub async fn test_provider(provider: Provider) -> Result<ProviderTest, String> {
    // 与真实发送走同一份地址与代理规则，自检结果才对得上真实请求
    let url = llm::endpoint(&provider);
    let proxy = store::load_settings(&root()).ok().and_then(|s| s.proxy);
    let proxy_used = llm::build_client(proxy.as_deref())
        .await
        .map(|(_, used)| used)
        .unwrap_or(None);
    let messages = vec![llm::ChatMessage {
        role: "user".into(),
        content: "说「好」一个字即可。".into(),
    }];
    let started = std::time::Instant::now();
    let outcome = llm::chat_once(&provider, &messages, proxy.as_deref()).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(ProviderTest {
        ok: outcome.is_ok(),
        detail: outcome.clone().unwrap_or_default(),
        message: match &outcome {
            Ok(_) => format!("连接成功（{} ms）", elapsed_ms),
            Err(e) => e.clone(),
        },
        url,
        model: provider.model.clone(),
        elapsed_ms,
        proxy: llm::proxy_env(),
        proxy_used,
    })
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
    log: State<'_, store::EventLog>,
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

    // genesis：初始黑板进事件流（M2.0）。此后黑板/状态/宫殿一律由事件流投影写出，
    // 派生文件不再充当基线——老会话的判定见 event::has_genesis。
    let board = store::load_blackboard(&root, &meta.id).map_err(|e| e.to_string())?;
    log.append(
        &root,
        &meta.id,
        LogBody::Blackboard(event::BlackboardEvent {
            turn: 0,
            reason: "init".into(),
            board,
            ts: store::unix_now(),
        }),
    )
    .map_err(|e| e.to_string())?;

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
                let _ = log.append(&root, &meta.id, LogBody::Message(opening));
            }
            // on_load：角色入席（设计 §3「生命周期钩子」）——state 就位、
            // 卡内可能顺手初始化黑板与长期记忆
            run_load_hook(&app, &root, &meta, &loaded, &log, "hook.on_load")?;
        }
    }
    Ok(meta)
}

// ---------- 事件流：投影 / 派生文件 / 重放（M2.0 · 设计 §7.3「可回放」）----------

/// 会话的首个角色（M2 仍是 1v1；M3 群聊按角色分别投影）
fn first_character(meta: &store::SessionMeta) -> Result<String, String> {
    meta.characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())
}

/// 事件流的起始基线：
/// - M2 建的会话（事件流带 genesis）一切都在事件流里，基线为空；
/// - M1 老会话的状态/黑板/宫殿只存在于派生文件里，以它们为基线继续折叠。
fn base_for(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    records: &[LogRecord],
) -> event::Base {
    if event::has_genesis(records) {
        return event::Base::default();
    }
    let mut base = event::Base {
        blackboard: store::load_blackboard(root, &meta.id).ok(),
        memory: store::read_memory_records(root, &meta.id).unwrap_or_default(),
        ..Default::default()
    };
    if let Ok(character) = first_character(meta) {
        if let Ok(state) = store::load_state(root, &meta.id) {
            base.states.insert(character, state);
        }
    }
    base
}

/// 事件流 → 会话现状
fn project(
    records: &[LogRecord],
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> event::Projection {
    event::project_over(records, &base_for(root, meta, records))
}

/// 读事件流并投影
fn project_session(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<event::Projection, String> {
    let records = log.read(root, &meta.id).map_err(|e| e.to_string())?;
    Ok(project(&records, root, meta))
}

/// 投影 → 派生文件（state.json / blackboard.json / palace.jsonl）。
/// **派生文件只能由这里写**——写入点各自落盘正是 M1 回滚不了状态的根因。
fn sync_derived(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    proj: &event::Projection,
) -> Result<(), String> {
    let state = first_character(meta)
        .ok()
        .and_then(|c| proj.state_of(&c).cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    store::save_state(root, &meta.id, &state).map_err(|e| e.to_string())?;
    if let Some(bb) = &proj.blackboard {
        store::save_blackboard(root, &meta.id, bb).map_err(|e| e.to_string())?;
    }
    store::write_memory_records(root, &meta.id, &proj.memory).map_err(|e| e.to_string())?;
    Ok(())
}

/// 只重投影 + 落派生文件（没有新事件时用）
fn sync_now(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<event::Projection, String> {
    let proj = project_session(log, root, meta)?;
    sync_derived(root, meta, &proj)?;
    Ok(proj)
}

/// 追加事件 → 重投影 → 落派生文件。**这是会话状态唯一的写入路径。**
fn commit(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    body: LogBody,
) -> Result<event::Projection, String> {
    log.append(root, &meta.id, body).map_err(|e| e.to_string())?;
    sync_now(log, root, meta)
}

/// 角色 state 现状：投影优先，空对象降级用卡上 default_state（设计 §4.3）
fn current_state(
    proj: &event::Projection,
    character: &str,
    loaded: &card::LoadedCard,
) -> serde_json::Value {
    match proj.state_of(character) {
        Some(v) if v.as_object().map(|o| !o.is_empty()).unwrap_or(false) => v.clone(),
        _ => loaded.default_state.clone(),
    }
}

fn blackboard_of(proj: &event::Projection) -> store::Blackboard {
    proj.blackboard
        .clone()
        .unwrap_or_else(store::Blackboard::default_board)
}

/// 一次钩子运行 → 事件。没改动就不记（事件流只留真发生的事）；
/// 「入席建基线」（force_baseline）例外：即使与卡上默认值相同也要记，
/// 因为它定义的正是这个会话的起点。
fn hook_effect(
    run: &card::HookRun,
    before: &serde_json::Value,
    character: &str,
    turn: u64,
    trigger: &str,
    force_baseline: bool,
) -> Option<LogBody> {
    let after = run.state.clone().unwrap_or_else(|| before.clone());
    let state_set = if force_baseline {
        event::state_patch(&serde_json::json!({}), &after)
    } else {
        event::state_patch(before, &after)
    };
    if state_set.is_empty() && run.blackboard.is_empty() && run.memory.is_empty() {
        return None;
    }
    Some(LogBody::Effect(event::EffectEvent {
        turn,
        trigger: trigger.into(),
        character: character.into(),
        state_set,
        blackboard: run.blackboard.clone(),
        memory: run.memory.clone(),
        ts: store::unix_now(),
    }))
}

/// 跑 `on_load`（建会话、角色入席时一次）。环境构造失败在此上报
/// （建会话不能带着半截状态继续）。
fn run_load_hook(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    log: &store::EventLog,
    source: &str,
) -> Result<llm::HookReport, String> {
    run_load_hook_core(root, meta, loaded, log, source, &ui_sink(app))
}

/// on_load 的内核（与 Tauri 无关：生产传 ui_sink(app)，单测传空回调）
fn run_load_hook_core(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    log: &store::EventLog,
    source: &str,
    sink: &card::UiSink,
) -> Result<llm::HookReport, String> {
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_load") {
        return Ok(llm::HookReport {
            turn: 0,
            ..Default::default()
        });
    }
    let proj = project_session(log, root, meta)?;
    let character = first_character(meta)?;
    let state = current_state(&proj, &character, loaded);
    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnLoad,
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&blackboard_of(&proj)),
            memory: BTreeMap::new(),
        },
        meta.seed,
        sink,
    );
    apply_load_hook(root, meta, log, source, &run, &state)
}

/// `on_load` 的落盘：入席是「建立基线」——生效后的 state 作为基线补丁记进事件流
/// （此后一律以会话为准，改卡的默认值不回头覆盖已有会话）。
/// 与 [`apply_message_hook`] 分开：on_message 只记「真变了的」，on_load 必记。
fn apply_load_hook(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
    source: &str,
    run: &card::HookRun,
    before: &serde_json::Value,
) -> Result<llm::HookReport, String> {
    let character = first_character(meta)?;
    let proj = match hook_effect(run, before, &character, 0, source, true) {
        Some(body) => commit(log, root, meta, body)?,
        None => sync_now(log, root, meta)?,
    };
    Ok(llm::HookReport {
        turn: 0,
        ran: run.ran(),
        logs: run.result.logs.clone(),
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
        memory: run.memory.clone(),
        card_state: proj
            .state_of(&character)
            .cloned()
            .unwrap_or_else(|| before.clone()),
    })
}

#[tauri::command]
pub fn list_sessions() -> Result<Vec<store::SessionMeta>, String> {
    store::list_sessions(&root()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_messages(
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    log.messages(&root(), &session_id).map_err(|e| e.to_string())
}

/// 第 index 条**消息**在事件流里的位置与轮次（下标是消息视图里的位置）
fn locate_message(records: &[LogRecord], index: usize) -> Option<(usize, u64)> {
    let mut seen = 0usize;
    for (pos, rec) in records.iter().enumerate() {
        if let Some(m) = rec.as_message() {
            if seen == index {
                return Some((pos, m.turn));
            }
            seen += 1;
        }
    }
    None
}

/// 编辑指定下标的消息内容：改写该消息事件 → 丢弃它所在轮次起的派生事件 → 重放重算。
///
/// 这正是 M1 遗留问题的解药：改掉那句「谢谢」，好感度会跟着退回去
/// （设计 §7.3-5「转移是事件流的纯函数」）。返回更新后的全量消息。
#[tauri::command]
pub fn edit_message(
    session_id: String,
    index: usize,
    content: String,
    log: State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let loaded = card::load_card(&root, &first_character(&meta)?).map_err(|e| e.to_string())?;
    let records = log.read(&root, &session_id).map_err(|e| e.to_string())?;
    let Some((pos, turn)) = locate_message(&records, index) else {
        return Err(format!("消息下标越界：{index}"));
    };
    let mut edited = records.as_ref().clone();
    if let LogBody::Message(m) = &mut edited[pos].body {
        m.content = content;
    }
    let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &edited, turn)?;
    log.rewrite(&root, &session_id, &rebuilt)
        .map_err(|e| e.to_string())?;
    sync_now(&log, &root, &meta)?;
    Ok(event::messages(&rebuilt))
}

/// 删除指定下标的消息：移除该消息事件 → 从它所在轮次起重放重算，返回更新后的全量消息
#[tauri::command]
pub fn delete_message(
    session_id: String,
    index: usize,
    log: State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let loaded = card::load_card(&root, &first_character(&meta)?).map_err(|e| e.to_string())?;
    let records = log.read(&root, &session_id).map_err(|e| e.to_string())?;
    let Some((pos, turn)) = locate_message(&records, index) else {
        return Err(format!("消息下标越界：{index}"));
    };
    let mut edited = records.as_ref().clone();
    edited.remove(pos);
    let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &edited, turn)?;
    log.rewrite(&root, &session_id, &rebuilt)
        .map_err(|e| e.to_string())?;
    sync_now(&log, &root, &meta)?;
    Ok(event::messages(&rebuilt))
}

/// 重放：丢弃 from_turn 起的派生事件，按卡重新跑这些轮的钩子（设计 §7.3-5）。
///
/// - **M2 会话**（事件流带 genesis）：先折叠 from_turn 之前的事件得到起点，再从该轮重放；
/// - **M1 老会话**（没有 genesis）：历史状态只存在于派生文件里、无法回退，故**从头全量重放**，
///   并顺带把 init 事件补进流——就地升级为事件溯源会话（此后消息级操作都能精确回滚）。
///   老会话的黑板已是终态、没有逐步的时钟记录，故重放时不再叠加时钟步进。
/// - 玩家手动产生的事件（手改黑板、手动开收线）永不被丢弃——它们不是派生结果。
/// - 重放期间不推界面事件：编辑历史不该再弹一次表情。
fn rebuild_from(
    _log: &store::EventLog, // 重建结果由调用方 rewrite；这里只依据传入的记录重算
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    records: &[LogRecord],
    from_turn: u64,
) -> Result<Vec<LogRecord>, String> {
    let character = first_character(meta)?;
    let genesis = event::has_genesis(records);
    let replay_from = if genesis { from_turn } else { 0 };

    // 保留：全部消息（内容可能已被编辑）+ 重放点之前的记录 + 玩家手动事件
    let kept: Vec<LogRecord> = records
        .iter()
        .filter(|r| {
            r.as_message().is_some()
                || r.turn() < replay_from
                || !r.is_derived()
                || matches!(&r.body, LogBody::Effect(e) if e.trigger == "hook.on_load")
        })
        .cloned()
        .collect();

    let mut out: Vec<LogRecord> = Vec::new();
    let mut proj = event::Projection::default();
    if !genesis {
        // 老会话补 genesis：初始黑板取当前文件里的那份（M1 没留下更早的黑板）
        let board = blackboard_of(&project(records, root, meta));
        let init = LogRecord::new(
            0,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "init".into(),
                board: board.clone(),
                ts: store::unix_now(),
            }),
        );
        out.push(init.clone());
        event::fold(&mut proj, &init);
        // 再补跑一次 on_load：老会话的事件流里没有入席基线，重放得从「角色刚入席」重新开始
        let state = loaded.default_state.clone();
        let run = card::run_hook_full(
            &loaded.source,
            card::HookCall::OnLoad,
            &card::HookEnv {
                state: state.clone(),
                blackboard: blackboard_env(&board),
                memory: BTreeMap::new(),
            },
            meta.seed,
            &NOOP_SINK,
        );
        if let Some(body) = hook_effect(&run, &state, &character, 0, "hook.on_load", true) {
            let rec = LogRecord::new(0, body);
            out.push(rec.clone());
            event::fold(&mut proj, &rec);
        }
    }

    for rec in &kept {
        let Some(msg) = rec.as_message().cloned() else {
            // 非消息记录：重放点之前的直接折进起点；重放区内的只可能是手动事件
            if rec.turn() < replay_from || !rec.is_derived() {
                out.push(rec.clone());
                event::fold(&mut proj, rec);
            }
            continue;
        };
        if msg.turn < replay_from || msg.turn == 0 {
            // 重放点之前，或 turn 0 的开场白（它没有 on_message，设计 §3）
            out.push(rec.clone());
            event::fold(&mut proj, rec);
            continue;
        }
        // ① 用户消息：on_context 在它之前跑（设计 §4.1 B5 的注入时机）
        if msg.role == "user" {
            let (run, before) = run_context_hook(
                loaded,
                &proj,
                &character,
                meta.seed,
                &proj.messages.clone(),
                &NOOP_SINK,
            );
            if let Some(body) = hook_effect(&run, &before, &character, msg.turn, "hook.on_context", false)
            {
                let rec = LogRecord::new(0, body);
                out.push(rec.clone());
                event::fold(&mut proj, &rec);
            }
        }
        // ② 消息本身
        out.push(rec.clone());
        event::fold(&mut proj, rec);
        // ③ 角色回复：时钟步进（设计 M1：每轮 +10 分钟，跨日进位）
        if msg.role == "char" && genesis {
            let mut board = blackboard_of(&proj);
            let (day, clock) = prompt::advance_clock(board.day, &board.clock);
            board.day = day;
            board.clock = clock;
            let rec = LogRecord::new(
                0,
                LogBody::Blackboard(event::BlackboardEvent {
                    turn: msg.turn,
                    reason: "clock".into(),
                    board,
                    ts: store::unix_now(),
                }),
            );
            out.push(rec.clone());
            event::fold(&mut proj, &rec);
        }
        // ④ on_message（每条新消息落地后，设计 §3）
        let (run, before) =
            run_message_hook_at(loaded, &proj, &character, &msg, meta.seed, &NOOP_SINK);
        if let Some(body) = hook_effect(&run, &before, &character, msg.turn, "hook.on_message", false) {
            let rec = LogRecord::new(0, body);
            out.push(rec.clone());
            event::fold(&mut proj, &rec);
        }
    }
    Ok(out)
}

// ---------- 对话生成（设计 §4 流程 + §11 流式）----------

/// 每个会话的生成中断标记
#[derive(Default)]
pub struct CancelFlags(Mutex<HashMap<String, Arc<AtomicBool>>>);

/// 每会话最近一次实际发送的组装结果（记忆检查器"本次注入"数据源）
#[derive(Default)]
pub struct LastAssemblies(Mutex<HashMap<String, prompt::PromptAssembly>>);

/// 组装一轮上下文（send_message / regenerate / preview_prompt 共用）。
/// `user_content` = Some 时为本轮真实发送（末尾带用户消息）；None 为检查器预览。
/// 本轮组装的产物：注入层 + 被 hook 顺带改动的宿主状态。
///
/// 落盘规则（M2.0）：真实组装（log = Some）会把 on_context 的副作用记成事件；
/// **预览是干跑**（log = None），一律不落盘、不记事件——M1 让预览也落盘是为了避免
/// 「预览一次变一次、发送又变一次」的漂移，而事件化之后正式发送自己会跑一次，
/// 预览再落盘反而是多算一次。
struct PromptRun {
    assembly: prompt::PromptAssembly,
    /// 生效后的角色 state（on_context 可能原地改过）
    card_state: serde_json::Value,
    /// 生效后的黑板（on_context 可能经 api.blackboard.set 改过）
    blackboard: store::Blackboard,
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

/// 跑 `on_context`（B5 注入时机，设计 §4.1）：返回 (运行结果, 运行前的 state)。
/// 降级卡与未定义该 hook 的卡返回默认（ran=false），不新建 Lua 实例。
#[allow(clippy::too_many_arguments)]
fn run_context_hook(
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    character: &str,
    seed: u64,
    history: &[Message],
    sink: &card::UiSink,
) -> (card::HookRun, serde_json::Value) {
    let state = current_state(proj, character, loaded);
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_context") {
        return (card::HookRun::default(), state);
    }
    let start = history.len().saturating_sub(prompt::WINDOW_MESSAGES);
    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnContext {
            window: &history[start..],
        },
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&blackboard_of(proj)),
            memory: BTreeMap::new(), // 长期记忆读侧（记忆宫殿）在 M2.1
        },
        seed,
        sink,
    );
    (run, state)
}

/// 跑 `on_message`（每条新消息落地后，设计 §3）：返回 (运行结果, 运行前的 state)。
fn run_message_hook_at(
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    character: &str,
    msg: &Message,
    seed: u64,
    sink: &card::UiSink,
) -> (card::HookRun, serde_json::Value) {
    let state = current_state(proj, character, loaded);
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_message") {
        return (card::HookRun::default(), state);
    }
    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnMessage { msg },
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&blackboard_of(proj)),
            memory: BTreeMap::new(),
        },
        seed,
        sink,
    );
    (run, state)
}

/// 组装一轮上下文的**内核**（与 Tauri 无关：生产传 ui_sink(app)，单测传空回调，
/// 两边走同一份代码——M1 曾因单测另写一份等价逻辑而漏掉生产的半步）。
///
/// `user_content` = Some 时为本轮真实发送（末尾带用户消息）；None 为检查器预览。
/// on_context hook 在此运行（B5 注入时机，设计 §4.1）。
///
/// 落盘：`log` = Some（真实组装）时把 on_context 的副作用记成事件并落派生文件；
/// `log` = None（预览干跑）时只在内存里生效，不落盘——理由见 [PromptRun]。
#[allow(clippy::too_many_arguments)]
fn assemble_prompt_core(
    sink: &card::UiSink,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    history: &[Message],
    proj: &event::Projection,
    user_content: Option<&str>,
    turn: u64,
    log: Option<&store::EventLog>,
) -> Result<PromptRun, String> {
    let settings = store::load_settings(root).map_err(|e| e.to_string())?;
    let persona = match &meta.persona {
        Some(name) => store::list_personas(root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| &p.name == name),
        None => None,
    };
    let character = first_character(meta)?;

    // B5：on_context hook（降级卡与未定义该 hook 的卡都跳过；窗口给最近消息）
    let (run, before) = run_context_hook(loaded, proj, &character, meta.seed, history, sink);
    let card_state = run.state.clone().unwrap_or_else(|| before.clone());
    let mut blackboard = blackboard_of(proj);
    if run.ran() {
        event::apply_blackboard_sets(&mut blackboard, &run.blackboard);
    }
    if let Some(log) = log {
        if let Some(body) = hook_effect(&run, &before, &character, turn, "hook.on_context", false) {
            commit(log, root, meta, body)?;
        }
    }

    let inputs = prompt::BuildInputs {
        settings: &settings,
        persona: persona.as_ref(),
        card: &loaded.card,
        card_state: &card_state,
        blackboard: &blackboard,
        hook_injections: &run.result.injections,
        history,
        user_content,
    };
    Ok(PromptRun {
        assembly: prompt::build(&inputs),
        card_state,
        blackboard,
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
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

/// 卡片事件 → 流事件类型（字段一致，避免同一概念两处定义）
fn ui_emit(event: &card::UiEvent) -> llm::UiEmit {
    llm::UiEmit {
        kind: event.kind.clone(),
        value: event.value.clone(),
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
/// 中断标记 → 流式补全 → 回复落盘 → 时钟步进 → on_message → 记录组装（记忆检查器）。
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
    log: &store::EventLog,
    assemblies: &LastAssemblies,
    // 用户消息那一步的钩子报告（回复落盘后另有一次，会一起回给前端）
    user_report: llm::HookReport,
) -> Result<StreamEvent, String> {
    let session_id = meta.id.as_str();
    let flag = match acquire_flag(flags, session_id) {
        Ok(f) => f,
        Err(e) => return Ok(e),
    };

    // 流式补全（取消检查在每个响应块之间）。代理跟随设置页：空则自动探测。
    let proxy = store::load_settings(root)
        .ok()
        .and_then(|s| s.proxy)
        .filter(|p| !p.trim().is_empty());
    let chat = assembly.messages.clone();
    let stream = llm::chat_stream(provider, &chat, |delta| {
        let _ = on_event.send(StreamEvent::Delta {
            text: delta.to_string(),
        });
    }, &flag, proxy.as_deref())
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
            // 有用户消息那一步的报告打底：即使本轮没生成回复（失败/中断为空），
            // 前端也能看到卡对用户输入的反应
            let mut report = Some(user_report);
            if !outcome.text.is_empty() {
                // 回复落定后的收尾与单测共用同一份代码：回复事件 → 时钟步进 → on_message
                match commit_reply(
                    root,
                    meta,
                    loaded,
                    turn,
                    &outcome.text,
                    Some(&ui_sink(app)),
                    log,
                ) {
                    Ok(next) => {
                        forward_ui_events(on_event, &next);
                        report = Some(next);
                    }
                    Err(e) => {
                        return Ok(StreamEvent::Error { message: e });
                    }
                }
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

/// 重roll / 重试的历史裁剪（纯函数，便于单测）：
///
/// - 末尾是角色回复 → 去掉它（重roll），以最后一条用户消息重新生成；
/// - 末尾是用户消息 → 上一轮生成失败（例如「请求失败」），直接**重试**这一轮；
/// - 其它（空、只有开场白）→ 报错。
///
/// 返回 `(写回磁盘的消息, 组装用的历史, 轮次, 用户输入)`。
#[allow(clippy::type_complexity)]
fn plan_regenerate(
    messages: &[Message],
) -> Result<(Vec<Message>, Vec<Message>, u64, String), String> {
    let mut kept = messages.to_vec();
    if kept.last().map(|m| m.role.as_str()) == Some("char") {
        kept.pop();
    }
    let Some(user_msg) = kept.last().filter(|m| m.role == "user") else {
        return Err("末尾没有可重新生成的用户消息".into());
    };
    let turn = user_msg.turn;
    let content = user_msg.content.clone();
    let prior = kept[..kept.len() - 1].to_vec();
    Ok((kept, prior, turn, content))
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
    on_event: Option<&Channel<StreamEvent>>,
    log: &store::EventLog,
) -> llm::HookReport {
    let sink = ui_sink(app);
    let report = run_message_hook_core(root, meta, loaded, turn, Some(&sink), log);
    if let Some(channel) = on_event {
        for event in &report.ui_events {
            let _ = channel.send(StreamEvent::HookEvent {
                kind: event.kind.clone(),
                value: event.value.clone(),
            });
        }
    }
    report
}

/// `on_message` 的完整流程（与 Tauri 无关，便于单测走同一份代码）：
/// 探测 → 取最新消息 → 投影出环境 → 沙箱执行 → 事件化落盘 → 报告。
/// `sink` 为 None 时卡片推来的界面事件只进报告、不实时外推。
fn run_message_hook_core(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    turn: u64,
    sink: Option<&card::UiSink>,
    log: &store::EventLog,
) -> llm::HookReport {
    if loaded.degraded || !loaded.hook_names.iter().any(|h| h == "on_message") {
        // 诊断：这条最容易被误判成「钩子没生效」——其实是这张卡没写 on_message
        crate::diag::record(
            "hook",
            format!(
                "on_message 跳过：卡「{}」degraded={} 已探测钩子={:?}",
                loaded.dir_name, loaded.degraded, loaded.hook_names
            ),
        );
        return llm::HookReport {
            turn,
            ..Default::default()
        };
    }
    // 钩子看到的最新一条消息：用户消息或刚落盘的角色回复。
    // 每个提前返回都要留痕——静默返回正是「钩子看起来没生效」最难查的形态。
    let messages = match log.messages(root, &meta.id) {
        Ok(m) => m,
        Err(e) => {
            let log = format!("历史读取失败：{e}");
            crate::diag::record("hook", format!("on_message 中止：{log}"));
            return report_with_log(turn, log);
        }
    };
    let Some(current) = messages.last().cloned() else {
        crate::diag::record("hook", "on_message 中止：没有可处理的消息");
        return report_with_log(turn, "没有可处理的消息".into());
    };
    let proj = match project_session(log, root, meta) {
        Ok(p) => p,
        Err(e) => {
            crate::diag::record("hook", format!("on_message 中止：投影失败：{e}"));
            return report_with_log(turn, e);
        }
    };
    let character = match first_character(meta) {
        Ok(c) => c,
        Err(e) => return report_with_log(turn, e),
    };

    // 钩子入参现场：把卡实际收到的 msg 与 state 原样记下来（JSON）。
    // 「条件不成立」这类静默失败，只有看到入参本身才能定死原因。
    let (run, before) = run_message_hook_at(
        loaded,
        &proj,
        &character,
        &current,
        meta.seed,
        sink.unwrap_or(&NOOP_SINK),
    );
    crate::diag::record(
        "hook",
        format!(
            "on_message 入参：msg={} state={}",
            serde_json::to_string(&current).unwrap_or_default(),
            before
        ),
    );

    let report = apply_message_hook(log, root, meta, turn, &run, &before);
    crate::diag::record(
        "hook",
        format!(
            "on_message turn={turn} ran={} state={} 记忆写入={} 日志={:?}",
            report.ran,
            report.card_state,
            report.memory.len(),
            report.logs
        ),
    );
    report
}

/// 卡片界面事件的空回调（单测与无界面场景）
static NOOP_SINK: std::sync::LazyLock<card::UiSink> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(|_: &card::UiEvent| {}));

fn report_with_log(turn: u64, log: String) -> llm::HookReport {
    llm::HookReport {
        turn,
        logs: vec![log],
        ..Default::default()
    }
}

/// 把一轮 `on_message` 的结果事件化落盘：副作用 → effect 事件（state 顶层键补丁 +
/// 黑板写入 + 记忆写入），派生文件（state.json / blackboard.json / palace.jsonl）由投影写出。
///
/// 与 `run_message_hook` 分开，是为了让「钩子副作用真的落盘」这件事能被单测直接钉住
/// （不必启动 Tauri 运行时）；也正因如此，编辑/删除历史时同一份代码能被重放调用。
fn apply_message_hook(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    turn: u64,
    run: &card::HookRun,
    before: &serde_json::Value,
) -> llm::HookReport {
    let mut report = llm::HookReport {
        turn,
        ran: run.ran(),
        logs: run.result.logs.clone(),
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
        memory: run.memory.clone(),
        card_state: run.state.clone().unwrap_or_else(|| before.clone()),
    };

    let character = first_character(meta).unwrap_or_default();
    if let Some(body) = hook_effect(run, before, &character, turn, "hook.on_message", false) {
        if let Err(e) = commit(log, root, meta, body) {
            report.logs.push(e);
        }
    }
    report
}

/// 回复落定后的收尾（stream_reply 与单测共用同一份代码，避免两处等价逻辑）：
/// 回复事件 → 时钟步进事件 → on_message 事件。
///
/// 事件化后时钟步进也是事件：重放同一轮必然得到同一时刻（设计 §7.3-5）。
fn commit_reply(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    turn: u64,
    text: &str,
    sink: Option<&card::UiSink>,
    log: &store::EventLog,
) -> Result<llm::HookReport, String> {
    let reply = Message {
        turn,
        role: "char".into(),
        content: text.to_string(),
        ts: store::unix_now(),
        scene_id: None,
    };
    log.append(root, &meta.id, LogBody::Message(reply))
        .map_err(|e| format!("回复落盘失败：{e}"))?;

    // 一轮完成：黑板时钟步进（设计 M1：每轮 +10 分钟，跨日进位）
    let proj = project_session(log, root, meta)?;
    let mut bb = blackboard_of(&proj);
    let (day, clock) = prompt::advance_clock(bb.day, &bb.clock);
    bb.day = day;
    bb.clock = clock;
    log.append(
        root,
        &meta.id,
        LogBody::Blackboard(event::BlackboardEvent {
            turn,
            reason: "clock".into(),
            board: bb,
            ts: store::unix_now(),
        }),
    )
    .map_err(|e| e.to_string())?;

    // 回复落盘后跑 on_message（设计 §3：每条新消息落地后调用）
    Ok(run_message_hook_core(root, meta, loaded, turn, sink, log))
}

/// 把钩子推来的界面事件转推前端（用户消息一步与回复一步共用）
fn forward_ui_events(channel: &Channel<StreamEvent>, report: &llm::HookReport) {
    for event in &report.ui_events {
        let _ = channel.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }
}

/// 发送一条用户消息并流式生成回复。
/// 流事件经 `on_event` 通道推给前端（delta / done / error），
/// 返回值即终态事件。用户消息先落盘；回复（含中断时的部分文本）生成后落盘。
///
/// 事件顺序（与重放顺序一致，见 rebuild_from）：
/// on_context 事件 → 用户消息事件 → on_message 事件 → 回复事件 → 时钟步进事件 → on_message 事件。
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    session_id: String,
    content: String,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    log: State<'_, store::EventLog>,
    assemblies: State<'_, LastAssemblies>,
) -> Result<StreamEvent, String> {
    let root = root();

    // 会话与角色卡
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = first_character(&meta)?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;

    // 接入点（chat 档；先校验再落盘用户消息，配置错误不产生半截会话）
    let provider = pick_chat_provider(&root)?;

    crate::diag::record(
        "chat",
        format!(
            "send_message 会话={} 卡={}（{}，钩子={:?}）",
            session_id,
            character,
            root.display(),
            loaded.hook_names
        ),
    );

    // 双槽位组装（设计 §4.1）：历史来自事件流投影，高轮次只解析新增行
    let proj = project_session(&log, &root, &meta)?;
    let history = proj.messages.clone();
    let turn = history.last().map(|m| m.turn).unwrap_or(0) + 1;
    // on_context 在此运行并事件化落盘：卡片可能顺手改了 state/黑板/界面事件
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &loaded,
        &history,
        &proj,
        Some(&content),
        turn,
        Some(&log),
    )?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }

    // 用户消息落盘后进入流式请求
    let user_msg = Message {
        turn,
        role: "user".into(),
        content: content.clone(),
        ts: store::unix_now(),
        scene_id: None,
    };
    log.append(&root, &session_id, LogBody::Message(user_msg))
        .map_err(|e| e.to_string())?;

    // 设计 §3：`on_message` 在**每条**新消息落地后调用——用户消息同样要跑，
    // 否则卡看不到本轮输入，且它的反应（比如好感度 +1）来不及影响这一轮的生成。
    let report = run_message_hook(&app, &root, &meta, &loaded, turn, Some(&on_event), &log);

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
        &log,
        &assemblies,
        report,
    )
    .await
}

/// 重roll 的截断（regenerate 与单测共用）：丢掉 turn 起的**派生事件**，
/// 必要时连同末尾那条角色回复一起移除。剩下的记录就是「这一轮还没开始」的状态。
///
/// 手动事件（手改黑板、手动开收线）不是派生结果，照旧保留。
fn truncate_turn(records: &[LogRecord], turn: u64, drop_reply: bool) -> Vec<LogRecord> {
    let mut kept: Vec<LogRecord> = records
        .iter()
        .filter(|r| r.turn() < turn || !r.is_derived())
        .cloned()
        .collect();
    if drop_reply {
        if let Some(pos) = kept.iter().rposition(|r| {
            r.as_message()
                .map(|m| m.turn == turn && m.role == "char")
                .unwrap_or(false)
        }) {
            kept.remove(pos);
        }
    }
    kept
}

/// 重roll（设计 §4 消息级操作）：移除末尾角色回复，以最后一条用户消息
/// 重新流式生成。先删后生成——失败也不会出现两条并列回复。
#[tauri::command]
pub async fn regenerate(
    app: AppHandle,
    session_id: String,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    log: State<'_, store::EventLog>,
    assemblies: State<'_, LastAssemblies>,
) -> Result<StreamEvent, String> {
    let root = root();

    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = first_character(&meta)?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;
    let provider = pick_chat_provider(&root)?;

    let records = log.read(&root, &session_id).map_err(|e| e.to_string())?;
    let all = event::messages(&records);
    let (rewritten, _prior, turn, content) = match plan_regenerate(&all) {
        Ok(plan) => plan,
        Err(message) => return Ok(StreamEvent::Error { message }),
    };

    // 截断本轮：丢掉 turn 起的**派生事件**（上一次的 on_context / on_message / 时钟步进），
    // 并按 plan 移除末尾回复。它们随后由正常流程重新产生——**「重roll 不重复计分」
    // 由此从启发式判据变成结构性保证**：旧效果已经不在流里了。
    // 手动事件（手改黑板、手动开收线）不是派生结果，照旧保留。
    let kept = truncate_turn(&records, turn, rewritten.len() < all.len());
    log.rewrite(&root, &session_id, &kept)
        .map_err(|e| e.to_string())?;
    sync_now(&log, &root, &meta)?;

    // 重roll 前先让 on_context 按当前（已删掉末尾回复的）历史跑一轮
    let proj = project_session(&log, &root, &meta)?;
    let history = &proj.messages[..proj.messages.len().saturating_sub(1)]; // 组装历史不含本轮用户消息
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &loaded,
        history,
        &proj,
        Some(&content),
        turn,
        Some(&log),
    )?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
        });
    }
    // 上一轮的 on_message 效果已随截断消失，这里补跑：**恰好一次**，不是重复计分
    let report = run_message_hook(&app, &root, &meta, &loaded, turn, Some(&on_event), &log);
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
        &log,
        &assemblies,
        report,
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

/// 手动编辑黑板（全量替换；保存后下一轮组装生效）。
/// 手改进事件流（reason=manual）——它不是派生结果，重放历史时不会被抹掉。
#[tauri::command]
pub fn update_blackboard(
    session_id: String,
    day: i64,
    clock: String,
    place: String,
    actors: Vec<String>,
    log: State<'_, store::EventLog>,
) -> Result<store::Blackboard, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let bb = store::Blackboard {
        day,
        clock: clock.trim().to_string(),
        place: place.trim().to_string(),
        actors: actors.into_iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(),
    };
    let turn = project_session(&log, &root, &meta)?
        .last_message()
        .map(|m| m.turn)
        .unwrap_or(0);
    let proj = commit(
        &log,
        &root,
        &meta,
        LogBody::Blackboard(event::BlackboardEvent {
            turn,
            reason: "manual".into(),
            board: bb.clone(),
            ts: store::unix_now(),
        }),
    )?;
    Ok(proj.blackboard.unwrap_or(bb))
}

// ---------- 记忆检查器 v0（设计 §4.2：组装结果逐层可见）----------

/// 预览组装：按当前状态干跑一轮（不含用户消息），不发送。
///
/// **干跑不落盘**（M2.0 起）：预览不再记事件、也不再改 state/黑板。
/// M1 让预览也落盘，是为了避免「预览一次状态变了、正式发送又变一次」的漂移；
/// 事件化之后正式发送自己会跑一次并留下事件，预览再落盘反而是多算一次。
#[tauri::command]
pub fn preview_prompt(
    app: AppHandle,
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<prompt::PromptAssembly, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let loaded = card::load_card(&root, &first_character(&meta)?).map_err(|e| e.to_string())?;
    let proj = project_session(&log, &root, &meta)?;
    let history = proj.messages.clone();
    let turn = history.last().map(|m| m.turn).unwrap_or(0) + 1;
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &loaded,
        &history,
        &proj,
        None,
        turn,
        None, // 干跑：不记事件、不落盘
    )?;
    Ok(run.assembly)
}

// ---------- 卡内状态与长期记忆（M1.6：hooks 的可观测面）----------

/// 角色私有 state 现状（会话快照为空时回退卡上 `state` 初始值）
#[tauri::command]
pub fn get_card_state(session_id: String) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let loaded = card::load_card(&root, &first_character(&meta)?).map_err(|e| e.to_string())?;
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

    fn noop_sink() -> card::UiSink {
        std::sync::Arc::new(|_: &card::UiEvent| {})
    }

    /// 建一个临时 DataHub + 会话（表驱动：单测不碰真实用户数据）。
    /// 顺序与 new_session 命令一致：init 黑板事件（genesis）→ 开场白 → on_load。
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
        let log = store::EventLog::new();
        let board = store::load_blackboard(&root, &meta.id).unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "init".into(),
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        let loaded = card::load_card(&root, "小雨").unwrap();
        let first = loaded.card.first_mes.trim().to_string();
        if !first.is_empty() {
            log.append(
                &root,
                &meta.id,
                LogBody::Message(Message {
                    turn: 0,
                    role: "char".into(),
                    content: first,
                    ts: store::unix_now(),
                    scene_id: None,
                }),
            )
            .unwrap();
        }
        run_load_hook_core(&root, &meta, &loaded, &log, "hook.on_load", &noop_sink()).unwrap();
        (dir, meta, root)
    }

    /// 造一个 **M1 形态**的会话：只建目录与元数据，不写 init 事件、不跑 on_load——
    /// 状态只存在于派生文件里（老会话升级重放的测试用）。
    fn setup_legacy(card_src: &str) -> (tempfile::TempDir, store::SessionMeta, std::path::PathBuf) {
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

    fn stored_state(root: &std::path::Path, meta: &store::SessionMeta) -> serde_json::Value {
        store::load_state(root, &meta.id).unwrap()
    }

    /// 一轮完整生成：**与生产共用同一份内核**（assemble_prompt_core / run_message_hook_core /
    /// commit_reply），只把界面事件回调换成空实现。此前单测另写了一份等价逻辑，
    /// 于是生产漏掉「用户消息那一步」时测试仍然绿——这个坑不再重犯。
    fn simulate_turn(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        loaded: &card::LoadedCard,
        log: &store::EventLog,
        turn: u64,
        content: &str,
    ) -> (prompt::PromptAssembly, llm::HookReport) {
        let proj = project_session(log, root, meta).unwrap();
        let history = proj.messages.clone();
        let run = assemble_prompt_core(
            &noop_sink(),
            root,
            meta,
            loaded,
            &history,
            &proj,
            Some(content),
            turn,
            Some(log),
        )
        .unwrap();
        log.append(root, &meta.id, LogBody::Message(user_msg(turn, content)))
            .unwrap();
        let after_user = run_message_hook_core(root, meta, loaded, turn, None, log);
        let after_reply = commit_reply(root, meta, loaded, turn, "（回复）", None, log).unwrap();

        // 报告取「本轮最后一次」（回复后的状态就是前端看到的最终状态）
        let mut report = after_reply;
        report.ran |= after_user.ran;
        report.memory = [after_user.memory, report.memory].concat();
        (run.assembly, report)
    }

    #[test]
    fn hooks_persist_state_memory_and_blackboard() {
        // 对应 M1.6 验收：好感度随对话变化、写入落盘、重启后读得到
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        assert_eq!(loaded.hook_names.len(), 3, "示例卡应带三个 hook");
        let log = store::EventLog::new();

        // 回归：钩子必须在**用户消息**落盘后就跑（设计 §3「每条新消息落地后」）——
        // 此前只有回复落盘后跑一次，于是卡看不到用户输入、它的反应也来不及影响本轮生成。
        // 诊断留痕是当时唯一能看见这件事的地方，故在此也断言它。
        let (assembly, report) = simulate_turn(&root, &meta, &loaded, &log, 1, "今天好冷。");
        let traces: Vec<String> = crate::diag::recent(10).iter().map(|d| d.detail.clone()).collect();
        assert!(
            traces.iter().filter(|d| d.contains("入参")).count() >= 2,
            "用户消息与回复各应留下一条入参记录：{traces:?}"
        );
        assert!(
            traces
                .iter()
                .any(|d| d.contains("\"role\":\"user\"") && d.contains("今天好冷")),
            "钩子必须看到用户消息本身（而不是只看到角色回复）：{traces:?}"
        );
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

        // 第二轮：说「谢谢」→ 好感度 +1，state.json 与 palace.jsonl 都由投影写出
        let (_, report) = simulate_turn(&root, &meta, &loaded, &log, 2, "谢谢你。");
        assert_eq!(report.card_state["favorability"], 51);
        assert_eq!(report.memory.len(), 1);
        assert_eq!(report.memory[0].key, "last_thanked");
        assert_eq!(stored_state(&root, &meta)["favorability"], 51, "state.json 应记住好感度");
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
    fn editing_a_message_rolls_the_hook_state_back() {
        // M1 的同源遗留（docs/plan/m1.md）：编辑历史消息不会回滚钩子对 state 的改动。
        // 事件日志落地后这条必须成立——改掉那句「谢谢」，好感度跟着退回去。
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "今天好冷。");
        simulate_turn(&root, &meta, &loaded, &log, 2, "谢谢你。");
        assert_eq!(stored_state(&root, &meta)["favorability"], 51);
        assert_eq!(store::read_memory_records(&root, &meta.id).unwrap().len(), 1);

        // 消息视图：[0] 开场 · [1] user1 · [2] char1 · [3] user2 · [4] char2
        let records = log.read(&root, &meta.id).unwrap();
        let (pos, turn) = locate_message(&records, 3).expect("第 3 条消息");
        assert_eq!((turn, records[pos].as_message().unwrap().role.as_str()), (2, "user"));

        let mut edited = records.as_ref().clone();
        if let LogBody::Message(m) = &mut edited[pos].body {
            m.content = "今天也是。".into(); // 不再道谢
        }
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &edited, turn).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        sync_now(&log, &root, &meta).unwrap();

        assert_eq!(
            stored_state(&root, &meta)["favorability"],
            50,
            "改掉「谢谢」后好感度必须退回"
        );
        assert!(
            store::read_memory_records(&root, &meta.id).unwrap().is_empty(),
            "那条记忆也应随重放消失"
        );
        // 回复本身没被删（编辑的是用户消息），消息视图只少了内容变化
        assert_eq!(event::messages(&rebuilt).len(), 5);
        assert_eq!(event::messages(&rebuilt)[3].content, "今天也是。");
    }

    #[test]
    fn deleting_a_message_rolls_its_effects_back() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        assert_eq!(stored_state(&root, &meta)["favorability"], 51);

        // 删掉那条用户消息：这一轮的钩子效果必须一并消失
        let records = log.read(&root, &meta.id).unwrap();
        let (pos, turn) = locate_message(&records, 1).unwrap();
        assert_eq!(turn, 1);
        let mut edited = records.as_ref().clone();
        edited.remove(pos);
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &edited, turn).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        sync_now(&log, &root, &meta).unwrap();

        assert_eq!(event::messages(&rebuilt).len(), 2, "开场白 + 角色回复");
        assert_eq!(stored_state(&root, &meta)["favorability"], 50);
        assert!(store::read_memory_records(&root, &meta.id).unwrap().is_empty());
    }

    #[test]
    fn reroll_reapplies_the_turn_exactly_once() {
        // 重roll 曾经因为「用户消息的钩子被重放」而把好感度反复 +1（M1 真机 bug）。
        // 事件化后截断是结构性的：旧的派生事件先被丢掉，再恰好重跑一次。
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        assert_eq!(stored_state(&root, &meta)["favorability"], 51);

        // 截断（regenerate 命令与这里调的是同一个函数）
        let records = log.read(&root, &meta.id).unwrap();
        let all = event::messages(&records);
        let (rewritten, _prior, turn, content) = plan_regenerate(&all).unwrap();
        let kept = truncate_turn(&records, turn, rewritten.len() < all.len());
        log.rewrite(&root, &meta.id, &kept).unwrap();
        sync_now(&log, &root, &meta).unwrap();
        assert_eq!(
            stored_state(&root, &meta)["favorability"],
            50,
            "截断后回到本轮之前"
        );

        // 重放本轮：on_context → on_message → 回复
        let proj = project_session(&log, &root, &meta).unwrap();
        let history = &proj.messages[..proj.messages.len().saturating_sub(1)];
        assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &loaded,
            history,
            &proj,
            Some(&content),
            turn,
            Some(&log),
        )
        .unwrap();
        run_message_hook_core(&root, &meta, &loaded, turn, None, &log);
        commit_reply(&root, &meta, &loaded, turn, "（重roll 的回复）", None, &log).unwrap();

        assert_eq!(
            stored_state(&root, &meta)["favorability"],
            51,
            "重roll 之后仍是 51：恰好计一次分"
        );
        assert_eq!(store::read_memory_records(&root, &meta.id).unwrap().len(), 1);
        assert_eq!(event::messages(&log.read(&root, &meta.id).unwrap()).len(), 3);
    }

    #[test]
    fn replay_of_the_same_event_stream_is_identical() {
        // 设计 §7.3-5：同一事件流重放必然得到同一状态
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        simulate_turn(&root, &meta, &loaded, &log, 2, "今天好冷。");
        let records = log.read(&root, &meta.id).unwrap();

        let a = project(&records, &root, &meta);
        let b = project(&records, &root, &meta);
        assert_eq!(a.messages, b.messages);
        assert_eq!(a.states, b.states);
        assert_eq!(a.blackboard, b.blackboard);
        assert_eq!(a.memory, b.memory);
        assert_eq!(a.transitions, b.transitions);

        // 再重放一遍整段历史（等价于消息级操作后的重建）：状态逐字相同
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &records, 1).unwrap();
        let c = event::project_over(&rebuilt, &event::Base::default());
        assert_eq!(c.states, a.states, "重放得到同一份 state");
        assert_eq!(c.blackboard, a.blackboard, "重放得到同一块黑板");
        assert_eq!(c.memory, a.memory, "重放得到同一条记忆流");
        assert_eq!(
            event::messages(&rebuilt),
            a.messages,
            "消息一个字都不该变"
        );
    }

    #[test]
    fn legacy_session_without_genesis_is_upgraded_on_rebuild() {
        // M1 老会话：messages.jsonl 只有消息行、state.json 是快照、没有 init 事件
        let (_dir, meta, root) = setup_legacy(HOOK_CARD);
        let log = store::EventLog::new();
        // 手工造出 M1 形态：一条用户消息 + 一份「已经被 +1 过」的快照
        log.append(&root, &meta.id, LogBody::Message(user_msg(1, "谢谢你。")))
            .unwrap();
        store::save_state(&root, &meta.id, &serde_json::json!({"favorability": 51})).unwrap();
        let records = log.read(&root, &meta.id).unwrap();
        assert!(!event::has_genesis(&records));
        assert_eq!(
            project(&records, &root, &meta).state_of("小雨").unwrap()["favorability"],
            51,
            "老会话以派生文件为基线"
        );

        let loaded = card::load_card(&root, "小雨").unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &records, 1).unwrap();
        assert!(event::has_genesis(&rebuilt), "重建后应补上 init 事件");
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        sync_now(&log, &root, &meta).unwrap();

        // 从头重放：on_load 建立 50 → 「谢谢你」+1 = 51，与 M1 快照一致（重放可靠）
        assert_eq!(stored_state(&root, &meta)["favorability"], 51);
        // 此后再编辑历史就能精确回滚（见 editing_a_message_rolls_the_hook_state_back）
        let mut edited = rebuilt.clone();
        let (pos, turn) = locate_message(&edited, 0).expect("唯一那条用户消息");
        assert_eq!(turn, 1);
        if let LogBody::Message(m) = &mut edited[pos].body {
            m.content = "算了。".into();
        }
        let again = rebuild_from(&log, &root, &meta, &loaded, &edited, turn).unwrap();
        log.rewrite(&root, &meta.id, &again).unwrap();
        sync_now(&log, &root, &meta).unwrap();
        assert_eq!(stored_state(&root, &meta)["favorability"], 50);
    }

    #[test]
    fn preview_is_a_dry_run_and_leaves_the_log_untouched() {
        // 预览干跑：不记事件、不改 state/黑板（M2.0 起；M1 的预览会落盘）
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        let before = log.read(&root, &meta.id).unwrap().len();
        let state_before = stored_state(&root, &meta);

        let proj = project_session(&log, &root, &meta).unwrap();
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &loaded,
            &proj.messages.clone(),
            &proj,
            None,
            2,
            None,
        )
        .unwrap();
        assert!(!run.assembly.layers.is_empty());
        assert_eq!(log.read(&root, &meta.id).unwrap().len(), before, "预览不写事件");
        assert_eq!(stored_state(&root, &meta), state_before, "预览不改状态");
    }

    #[test]
    fn regenerate_plan_rolls_back_reply_or_retries_failed_turn() {
        let user = |turn, content: &str| Message {
            turn,
            role: "user".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        };
        let ch = |turn, content: &str| Message {
            turn,
            role: "char".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        };

        // 完整一轮：重roll 去掉末尾回复，组装历史不含本轮用户消息
        let full = vec![ch(0, "开场"), user(1, "你好"), ch(1, "……嗯")];
        let (rewritten, prior, turn, content) = plan_regenerate(&full).unwrap();
        assert_eq!(rewritten.len(), 2, "末尾回复被移除");
        assert_eq!(prior.len(), 1, "组装历史不含本轮用户消息");
        assert_eq!((turn, content.as_str()), (1, "你好"));

        // 请求失败：末尾只剩用户消息 → 直接重试这一轮
        let failed = vec![ch(0, "开场"), user(1, "你好")];
        let (rewritten, prior, turn, content) = plan_regenerate(&failed).unwrap();
        assert_eq!(rewritten.len(), 2, "没有回复可删，原样保留");
        assert_eq!(prior.len(), 1);
        assert_eq!((turn, content.as_str()), (1, "你好"));

        // 只有开场白 / 空会话：给明确错误，而不是生成出奇怪的一轮
        assert!(plan_regenerate(&[ch(0, "开场")]).is_err());
        assert!(plan_regenerate(&[]).is_err());
    }

    #[test]
    fn static_card_runs_no_hooks_and_keeps_state_empty() {
        let plain = "return { spec='charcard/1.0', name='静卡', scenario='s', personality='p', first_mes='f' }";
        let (_dir, meta, root) = setup(plain);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        let (assembly, report) = simulate_turn(&root, &meta, &loaded, &log, 1, "你好");
        assert!(!report.ran);
        // 没有 on_context：B5 层不该出现（空层省略）
        assert!(!assembly.layers.iter().any(|l| l.id == "B5"));
        // 也不该写 state/palace
        assert_eq!(stored_state(&root, &meta), serde_json::json!({}));
        assert!(store::read_memory_records(&root, &meta.id).unwrap().is_empty());
        // 静卡不产生 effect 事件（事件流只留真发生的事）
        let records = log.read(&root, &meta.id).unwrap();
        assert!(
            !records
                .iter()
                .any(|r| matches!(&r.body, LogBody::Effect(e) if e.trigger != "hook.on_load")),
            "静卡不该产生钩子副作用事件"
        );
    }

    #[test]
    fn on_load_initialises_state_from_card_defaults() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        // 会话初始 state.json 是空对象：卡上默认值尚未落到会话（这正是 on_load 的职责）
        let initial = load_card_state(&root, &meta, &loaded).unwrap();
        assert_eq!(initial, serde_json::json!({ "favorability": 50 }), "空快照降级用卡上默认值");

        // 生产路径（run_load_hook_core 是会话语料的同一份内核）：入席必写基线
        let report = run_load_hook_core(&root, &meta, &loaded, &log, "hook.on_load", &noop_sink()).unwrap();
        assert!(report.ran);
        assert_eq!(report.card_state["favorability"], 50);
        assert_eq!(report.ui_events[0].value, "calm");
        assert_eq!(stored_state(&root, &meta)["favorability"], 50);
        // 基线进了事件流：即使 state.json 被删，投影也能算出同一份状态
        let records = log.read(&root, &meta.id).unwrap();
        let proj = event::project_over(&records, &event::Base::default());
        assert_eq!(proj.state_of("小雨").unwrap()["favorability"], 50);
    }
}
