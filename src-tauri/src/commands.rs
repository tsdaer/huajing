//! Tauri 命令层：前端可调用的入口。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

use crate::card;
use crate::codex;
use crate::event::{self, LogBody, LogRecord};
use crate::palace;
use crate::psyche;
use crate::statetree;
use crate::threads;
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
    // M2.6 的两份派生文件（摘要与设定收件箱）同样由投影写出
    store::write_summary(root, &meta.id, &proj.summary).map_err(|e| e.to_string())?;
    let proposals: Vec<serde_json::Value> = proj.proposals.values().cloned().collect();
    store::write_proposals(root, &meta.id, &proposals).map_err(|e| e.to_string())?;
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
            memory: memory_env(&proj.memory),
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
                memory: memory_env(&proj.memory),
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
        // ⑤ 轮末心理运行时推进（情绪衰减也随重放重算——心理状态同样可回放）
        if msg.role == "char" {
            let (body, _emotion) = tick_psyche(&proj, &character, loaded, msg.turn);
            if let Some(body) = body {
                let rec = LogRecord::new(0, body);
                out.push(rec.clone());
                event::fold(&mut proj, &rec);
            }
            // ⑥ 轮末状态树转移（与 commit_reply 同一份代码 → 重放同一条路径，设计 §7.3-5）
            if let Some(tree) = load_tree(loaded, None) {
                // 重放不重推界面事件（编辑历史不该再弹一次表情）
                for body in advance_state_tree(
                    &proj,
                    &character,
                    loaded,
                    &tree,
                    msg.turn,
                    "on_turn_end",
                    None,
                    meta.seed,
                )
                .0
                {
                    let rec = LogRecord::new(0, body);
                    out.push(rec.clone());
                    event::fold(&mut proj, &rec);
                }
            }
        }
    }
    Ok(out)
}

// ---------- 设定集与记忆宫殿的接入（M2.1 / M2.2）----------

/// B3 实体卡与 B4 回忆的预算（设计 §4.2 的完整预算表在 M2.7 落地，这里先给固定值）
const B3_TOKENS: usize = 1200;
const B4_TOKENS: usize = 800;
/// B3/B4 上限条数
const B3_MAX_CARDS: usize = 12;
const B4_TOP_K: usize = 6;
/// 设定集别名扫描窗口（设计 §6.3：默认最近 16 条消息）
const SCAN_WINDOW_MESSAGES: usize = 16;
/// 滞回轮数（设计 §6.3：实体激活后保持 N 轮再退场）
const CODEX_HOLD_ROUNDS: u32 = 3;

/// 会话启用的世界（未指定时用 default；DataHub/codex/default 是脚手架自带的示例世界）
fn session_world(meta: &store::SessionMeta) -> String {
    meta.world.clone().unwrap_or_else(|| "default".into())
}

fn codex_entities_dir(root: &std::path::Path, world: &str) -> std::path::PathBuf {
    root.join("codex").join(world).join("entities")
}

/// 实体目录指纹（文件名 + 大小 + 修改时间）：设定集是只读输入，
/// 每轮重解析上百个 Lua 文件并不划算，指纹没变就直接用缓存（改文件即生效，与热加载同款判据）。
fn world_fingerprint(dir: &std::path::Path) -> u64 {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let mut fp: u64 = 1469598103934665603; // FNV 偏移
    for path in entries {
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for byte in format!("{}:{}:{}", path.display(), md.len(), mtime).bytes() {
            fp = (fp ^ byte as u64).wrapping_mul(1099511628211);
        }
    }
    fp
}

/// 设定集缓存（Tauri State）：世界名 → (指纹, 解析结果)
#[derive(Default)]
pub struct CodexCache(Mutex<HashMap<String, (u64, Arc<codex::Codex>)>>);

/// 逐文件解析实体：.lua 走沙箱（设计 §6.2 双格式），坏文件跳过并留诊断，不让一个手滑的文件瘫痪整局
fn parse_entities(dir: &std::path::Path) -> Vec<codex::CodexEntity> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("lua") | Some("json")
            )
        })
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                crate::diag::record("codex", format!("实体读取失败 {}：{e}", path.display()));
                continue;
            }
        };
        let value = if path.extension().and_then(|e| e.to_str()) == Some("lua") {
            card::eval_lua_value(&raw)
        } else {
            serde_json::from_str(&raw).map_err(|e| e.to_string())
        };
        match value.and_then(|v| codex::CodexEntity::from_value(&v)) {
            Ok(entity) => out.push(entity),
            Err(e) => crate::diag::record("codex", format!("实体解析失败 {}：{e}", path.display())),
        }
    }
    out
}

/// 取某个世界的设定集（带指纹缓存）
fn load_codex(
    root: &std::path::Path,
    cache: Option<&CodexCache>,
    world: &str,
) -> Arc<codex::Codex> {
    let dir = codex_entities_dir(root, world);
    let fp = world_fingerprint(&dir);
    if let Some(cache) = cache {
        if let Ok(map) = cache.0.lock() {
            if let Some((cached, codex)) = map.get(world) {
                if *cached == fp {
                    return codex.clone();
                }
            }
        }
    }
    let codex = Arc::new(codex::Codex::build(parse_entities(&dir)));
    if let Some(cache) = cache {
        if let Ok(mut map) = cache.0.lock() {
            map.insert(world.to_string(), (fp, codex.clone()));
        }
    }
    codex
}

/// 会话级跨轮运行时（设定集滞回等需要「上一轮」的记忆；M2.3 起还会放活跃路径）
#[derive(Default)]
pub struct SessionRuntime(Mutex<HashMap<String, RuntimeEntry>>);

#[derive(Default, Clone)]
struct RuntimeEntry {
    /// 上一轮激活的实体 id（设定集滞回用）
    previously_active: std::collections::BTreeSet<String>,
}

impl SessionRuntime {
    fn previously_active(&self, session_id: &str) -> std::collections::BTreeSet<String> {
        self.0
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).map(|e| e.previously_active.clone()))
            .unwrap_or_default()
    }

    fn set_previously_active(
        &self,
        session_id: &str,
        ids: std::collections::BTreeSet<String>,
    ) {
        if let Ok(mut map) = self.0.lock() {
            map.entry(session_id.to_string()).or_default().previously_active = ids;
        }
    }
}

/// 别名扫描窗口的正文：最近 N 条消息（带角色名，让「小雨说……」也算提及）
fn scan_window_text(history: &[Message], card_name: &str) -> String {
    let start = history.len().saturating_sub(SCAN_WINDOW_MESSAGES);
    history[start..]
        .iter()
        .map(|m| format!("{}：{}", display_role(&m.role, card_name), m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 记忆对象：把事件流里的记忆记录（`api.memory.set` 的键值形态）读成宫殿对象（设计 §5.2）。
///
/// 两处宿主侧补全，都写明了设计依据：
/// - **见证者**：卡内 `api.memory` 是**角色私有**记忆（设计 §2.1/§10.4），故 witnesses 取本角色——
///   否则具名视角的召回过滤会把它挡在门外（旧记录没有 actors 字段）；
/// - **故事时刻**：M1 的 fact 记录只有轮次、没有故事天，按「当下」计（Δ=0 不衰减）；
///   L3 事实本就是跨会话持久的键值，不该像情景记忆那样随时间淡去（设计 §5.1 分层责任）。
fn memory_objects(
    proj: &event::Projection,
    character: &str,
    now_day: i64,
) -> Vec<palace::MemObject> {
    // ① 结构化记忆对象（总结管线写入的情景记忆；自带见证者与显著度）
    let mut out: Vec<palace::MemObject> = proj
        .episodes
        .iter()
        .filter_map(|v| serde_json::from_value::<palace::MemObject>(v.clone()).ok())
        .collect();
    let base = out.len();
    // ② 卡内键值事实（api.memory.set）：补见证者与故事时刻，见下方注释
    out.extend(proj.memory.iter().enumerate().map(|(i, rec)| {
        let mut obj = palace::from_legacy_fact(&rec.key, &rec.value, &rec.source, rec.turn, rec.ts);
        obj.id = palace::next_id(base + i + 1);
        obj.actors = vec![character.to_string()];
        obj.witnesses = vec![character.to_string()];
        obj.story_day = now_day;
        obj
    }));
    out
}

/// `api.memory.get` 的读侧：宫殿里的键值（同 key 后写覆盖；M1 里这一侧恒空）
fn memory_env(records: &[store::MemRecord]) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    for rec in records {
        map.insert(rec.key.clone(), rec.value.clone());
    }
    map
}

/// 角色名（消息显示用；卡名而不是目录名）
fn display_role(role: &str, card_name: &str) -> String {
    match role {
        "user" => "玩家".to_string(),
        "char" | "assistant" => card_name.to_string(),
        other => other.to_string(),
    }
}

/// 状态树结构缓存（Tauri State）：卡目录名 → (源码指纹, 解析出的树)。
/// 卡源每轮从磁盘重读（热加载的前提），所以指纹就是源码本身。
#[derive(Default)]
pub struct TreeCache(Mutex<HashMap<String, (u64, Arc<statetree::StateTree>)>>);

fn source_fingerprint(source: &str) -> u64 {
    let mut fp: u64 = 1469598103934665603;
    for byte in source.as_bytes() {
        fp = (fp ^ *byte as u64).wrapping_mul(1099511628211);
    }
    fp
}

/// 取卡上的状态树（没有 state_tree 的卡返回 None；解析失败留诊断并按无树处理）
fn load_tree(
    loaded: &card::LoadedCard,
    cache: Option<&TreeCache>,
) -> Option<Arc<statetree::StateTree>> {
    let fp = source_fingerprint(&loaded.source);
    if let Some(cache) = cache {
        if let Ok(map) = cache.0.lock() {
            if let Some((cached, tree)) = map.get(&loaded.dir_name) {
                if *cached == fp {
                    return Some(tree.clone());
                }
            }
        }
    }
    let shape = match card::state_tree_shape(&loaded.source) {
        Ok(s) => s,
        Err(e) => {
            crate::diag::record("statetree", format!("状态树读取失败：{e}"));
            return None;
        }
    };
    if shape
        .get("root")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .is_empty()
    {
        return None; // 卡上没有状态树（不是错误）
    }
    let tree = match statetree::StateTree::from_value(&shape) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            crate::diag::record("statetree", format!("状态树解析失败：{e}"));
            return None;
        }
    };
    for warning in tree.validate() {
        crate::diag::record("statetree", format!("状态树校验：{warning}"));
    }
    if let Some(cache) = cache {
        if let Ok(mut map) = cache.0.lock() {
            map.insert(loaded.dir_name.clone(), (fp, tree.clone()));
        }
    }
    Some(tree)
}

/// 当前活跃路径：转移事件是权威（投影折叠出「最后一次转移的去向」），
/// 没有转移时用树根播种（设计 §7.3：路径同样是事件流的函数）。
fn active_path_of(proj: &event::Projection, tree: &statetree::StateTree) -> Vec<String> {
    proj.transitions
        .last()
        .map(|t| t.to.clone())
        .unwrap_or_else(|| tree.active_path(&tree.root))
}

/// 状态树求值用的判据环境（设计 §7.2：when 可查黑板、state、设定集与剧情线）
fn tree_env(
    proj: &event::Projection,
    character: &str,
    loaded: &card::LoadedCard,
    event_name: &str,
    active_entities: Option<&std::collections::BTreeSet<String>>,
) -> card::TreeEnv {
    let mut threads_active = std::collections::BTreeSet::new();
    let mut threads_resolved = std::collections::BTreeSet::new();
    for (id, snapshot) in &proj.threads {
        match snapshot.get("state").and_then(|s| s.as_str()) {
            Some("active") => {
                threads_active.insert(id.clone());
            }
            Some("resolved") => {
                threads_resolved.insert(id.clone());
            }
            _ => {}
        }
    }
    card::TreeEnv {
        event: event_name.to_string(),
        blackboard: blackboard_env(&blackboard_of(proj)),
        state: current_state(proj, character, loaded),
        known: proj.known.clone(),
        codex_active: active_entities.cloned().unwrap_or_default(),
        threads_active,
        threads_resolved,
    }
}

/// 轮末状态树求值（设计 §7.3-2/3）：首个命中的转移 → 转移事件；进入新路径的 reveal → 设定事件。
///
/// 返回待落盘的事件（无转移则空）。commit_reply 与 rebuild_from 共用同一份代码——
/// 转移同样是 (事件流, 黑板, state, 揭示, 剧情线) 的纯函数（§7.3-5）。
#[allow(clippy::too_many_arguments)]
fn advance_state_tree(
    proj: &event::Projection,
    character: &str,
    loaded: &card::LoadedCard,
    tree: &statetree::StateTree,
    turn: u64,
    event_name: &str,
    active_entities: Option<&std::collections::BTreeSet<String>>,
    seed: u64,
) -> (Vec<LogBody>, Vec<llm::UiEmit>) {
    let mut emits: Vec<llm::UiEmit> = Vec::new();
    let path = active_path_of(proj, tree);
    let Some(leaf) = path.last().cloned() else {
        return (Vec::new(), emits);
    };
    let env = tree_env(proj, character, loaded, event_name, active_entities);
    let decision = match card::eval_state_tree(&loaded.source, &path, &env) {
        Ok(Some(d)) => d,
        Ok(None) => return (Vec::new(), emits),
        Err(e) => {
            crate::diag::record("statetree", format!("状态树求值失败：{e}"));
            return (Vec::new(), emits);
        }
    };
    let to_path = tree.active_path(&decision.to);
    if to_path.is_empty() {
        crate::diag::record(
            "statetree",
            format!("转移目标未声明，保持原地：{}", decision.to),
        );
        return (Vec::new(), emits);
    }
    // 设计 §7.3-3 的执行顺序：exit 钩子 → 切换活跃路径 → enter 钩子/任务；
    // 每一步的副作用都作为事件追加（回放时同样重跑，得到同一份状态）。
    let mut out: Vec<LogBody> = Vec::new();
    let mut local = proj.clone();

    // ① on_exit（旧叶）
    let (exit_body, exit_emits) = run_state_hook(
        loaded, &local, character, &leaf, "on_exit", event_name, turn, seed,
    );
    emits.extend(exit_emits);
    if let Some(body) = exit_body {
        let rec = LogRecord::new(0, body.clone());
        out.push(body);
        event::fold(&mut local, &rec);
    }

    // ② 切换活跃路径
    let transition = LogBody::Transition(event::TransitionEvent {
        turn,
        from: path,
        to: to_path.clone(),
        reason: decision.reason,
        ts: store::unix_now(),
    });
    let rec = LogRecord::new(0, transition.clone());
    out.push(transition);
    event::fold(&mut local, &rec);

    // ③ on_enter（新叶）
    let to_leaf = to_path.last().cloned().unwrap_or_default();
    let (enter_body, enter_emits) = run_state_hook(
        loaded, &local, character, &to_leaf, "on_enter", event_name, turn, seed,
    );
    emits.extend(enter_emits);
    if let Some(body) = enter_body {
        let rec = LogRecord::new(0, body.clone());
        out.push(body);
        event::fold(&mut local, &rec);
    }

    // ④ 进入新路径即揭示（设计 §6.4：状态树的 reveal 解锁设定）
    for target in tree.reveal_of(&to_path) {
        if !proj.known.contains(&target) {
            out.push(LogBody::Codex(event::CodexEvent {
                turn,
                op: "reveal".into(),
                target,
                origin: "tree".into(),
                value: None,
                note: Some(leaf.clone()),
                ts: store::unix_now(),
            }));
        }
    }
    (out, emits)
}

/// 跑一个状态钩子（on_enter / on_exit）并把副作用折成 effect 事件（没改动则不记）。
/// 与 hook_effect 同一套语义：state 顶层键补丁 + 黑板写入 + 记忆写入。
#[allow(clippy::too_many_arguments)]
fn run_state_hook(
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    character: &str,
    state_id: &str,
    kind: &str,
    event_name: &str,
    turn: u64,
    seed: u64,
) -> (Option<LogBody>, Vec<llm::UiEmit>) {
    if !card::card_has_state_hook(&loaded.source, state_id, kind) {
        return (None, Vec::new()); // 卡上没写这个钩子：不新建 Lua 实例
    }
    let env = tree_env(proj, character, loaded, event_name, None);
    let before = current_state(proj, character, loaded);
    let run = card::run_state_hook_full(&loaded.source, state_id, kind, &env, seed, &NOOP_SINK);
    if !run.result.logs.is_empty() {
        crate::diag::record(
            "statetree",
            format!("{state_id}.{kind} 日志：{:?}", run.result.logs),
        );
    }
    let emits: Vec<llm::UiEmit> = run.result.ui_events.iter().map(ui_emit).collect();
    let body = hook_effect(&run, &before, character, turn, &format!("state.{kind}"), false);
    (body, emits)
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

/// 黑板 → hook / 设定集读侧快照（`api.blackboard.get` 与 `live` 取值的数据源）。
///
/// 世界层四字段平铺；实体作用域键**两种形态都给**（设计 §6.4 的 `bb["char.小雨"].status`）：
/// - 平铺 `char.小雨.status`（设定集 live 的第二种取值路径）；
/// - 嵌套 `char.小雨` → `{ status: … }`（第一种取值路径，也是卡作者最顺手的写法）。
fn blackboard_env(bb: &store::Blackboard) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert("day".into(), serde_json::json!(bb.day));
    map.insert("clock".into(), serde_json::json!(bb.clock));
    map.insert("place".into(), serde_json::json!(bb.place));
    map.insert("actors".into(), serde_json::json!(bb.actors));
    for (key, value) in &bb.extra {
        map.insert(key.clone(), value.clone());
        if let Some((scope, field)) = key.rsplit_once('.') {
            let entry = map
                .entry(scope.to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert(field.to_string(), value.clone());
            }
        }
    }
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
            memory: memory_env(&proj.memory), // 长期记忆读侧：记忆宫殿的键值（同 key 后写覆盖）
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
            memory: memory_env(&proj.memory),
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
    codex_cache: Option<&CodexCache>,
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
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

    // ---- B2 指令层：状态树活跃路径的 directive（设计 §7.4「输出约束」）----
    let tree = load_tree(loaded, tree_cache);
    let active_path = tree
        .as_ref()
        .map(|t| active_path_of(proj, t))
        .unwrap_or_default();
    let directive = tree
        .as_ref()
        .map(|t| t.directive_of(&active_path))
        .unwrap_or_default();
    let directive = if directive.trim().is_empty() {
        None
    } else {
        Some(directive)
    };
    // 状态树的 recall 提示 → 宫殿召回权重（设计 §5.4：「回到事发地点才想起那件事」）
    let recall_hints = tree
        .as_ref()
        .map(|t| t.recall_of(&active_path))
        .unwrap_or_default();

    // ---- B3 设定集：别名扫描 → 五激活源 → 分级注入（设计 §6.3）----
    let world = session_world(meta);
    let cx = load_codex(root, codex_cache, &world);
    let window_text = scan_window_text(history, &loaded.card.name);
    let place = {
        let p = blackboard.place.trim();
        if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        }
    };
    let bb_map = blackboard_env(&blackboard);
    let previously = runtime
        .map(|r| r.previously_active(&meta.id))
        .unwrap_or_default();
    let known: std::collections::BTreeSet<String> = proj.known.clone();
    // reveals 是**本轮**揭示（命中即强制深卡、权重最高），累积已知集只喂 known——
    // 否则揭示过的实体会每轮都插深卡并挤占 B3 预算。reveal 由状态树在 M2.3 写入事件流。
    let reveals: Vec<String> = Vec::new();
    let activation = codex::ActivationContext {
        window_text: &window_text,
        place: place.as_deref(),
        actors: &blackboard.actors,
        reveals: &reveals,
        known: &known,
        previously_active: &previously,
        hold_rounds: CODEX_HOLD_ROUNDS,
        day: blackboard.day,
        clock: &blackboard.clock,
        blackboard: &bb_map,
    };
    let activated = cx.activate(
        &activation,
        &codex::CodexBudget {
            tokens: B3_TOKENS,
            max_cards: B3_MAX_CARDS,
        },
    );
    if let Some(runtime) = runtime {
        runtime.set_previously_active(
            &meta.id,
            activated.iter().map(|a| a.id.clone()).collect(),
        );
    }
    let entity_cards: Vec<prompt::SourceCard> = activated
        .iter()
        .map(|a| prompt::SourceCard {
            id: format!("{}·{}", a.name, codex::type_cn(&a.ty)),
            text: a.text.clone(),
            reasons: a.reasons.clone(),
            tokens: a.tokens,
        })
        .collect();

    // ---- B4 记忆宫殿：视角过滤 → 召回打分 → top-K（设计 §5.4）----
    let memories = memory_objects(proj, &character, blackboard.day);
    // 话题窗口词表（召回与剧情线窗口共用，两处都按「包含」匹配）：
    // ① 设定集别名扫描命中的实体 id；② 最近窗口的**消息原文片段**——
    //    剧情线的 mention 窗口要的是「话题擦边」，真正的词在原文里，光有实体 id 不够。
    let mut mentions: Vec<String> = cx
        .scan_mentions(&window_text)
        .into_iter()
        .map(|m| m.id)
        .collect();
    let scan_start = history.len().saturating_sub(SCAN_WINDOW_MESSAGES);
    mentions.extend(history[scan_start..].iter().map(|m| m.content.clone()));

    // ---- B1 心里有事 / 了结未远 · C1 未决事项：剧情线（设计 §8.4/§8.5）----
    // 线本身由事件流持有（thread 事件带全量快照），这里只做「此刻能不能提」的确定性求值。
    let thread_list: Vec<threads::Thread> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .collect();
    let thread_query = threads::ThreadQuery {
        turn,
        story_day: blackboard.day,
        story_clock: &blackboard.clock,
        blackboard: &bb_map,
        mentions: &mentions,
        present: &blackboard.actors,
        state_path: &[], // 状态树活跃路径在 M2.3 接入
    };
    let picks = threads::select_resurface(&thread_list, &thread_query, 3);
    let concerns = threads::render_concerns(&picks);
    let resolutions = threads::recent_resolutions(&thread_list, 20, turn);
    let pending = threads::pending_lines(&thread_list);
    let query = palace::RecallQuery {
        viewer: character.clone(),
        now_day: blackboard.day,
        place: place.clone(),
        present: blackboard.actors.clone(),
        mentions,
        hints: recall_hints,             // 状态树 recall 提示（M2.3）
        active_threads: thread_list
            .iter()
            .filter(|t| t.is_active())
            .map(|t| t.id.clone())
            .collect(),
        top_k: B4_TOP_K,
        budget_tokens: B4_TOKENS,
    };
    let hits = palace::recall(&memories, &query);
    let memory_cards: Vec<prompt::SourceCard> = hits
        .iter()
        .map(|hit| {
            let text = palace::render_memory_block(std::slice::from_ref(hit));
            let tokens = hit.tokens();
            prompt::SourceCard {
                id: hit.mem.id.clone(),
                text,
                reasons: hit.reasons.clone(),
                tokens,
            }
        })
        .collect();

    // ---- B5 内心：心理运行时摘要（设计 §9.2：主观世界一行，空则省略）----
    let psyche_line = psyche::Psyche::from_state(&card_state).summary_line_for(&loaded.card.name);
    let psyche_line = if psyche_line.trim().is_empty() {
        None
    } else {
        Some(psyche_line)
    };

    let inputs = prompt::BuildInputs {
        settings: &settings,
        persona: persona.as_ref(),
        card: &loaded.card,
        card_state: &card_state,
        blackboard: &blackboard,
        hook_injections: &run.result.injections,
        entity_cards: &entity_cards,
        memory_cards: &memory_cards,
        concerns: &concerns,
        resolutions: &resolutions,
        pending_threads: &pending,
        psyche_line: psyche_line.as_deref(),
        directive: directive.as_deref(),
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
    // 轮末状态树求值要用：上一轮激活的实体（codex.active 判据）与状态树结构缓存
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
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
                    runtime,
                    tree_cache,
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

/// 轮末推进心理运行时（设计 §9.2）：情绪按气质参数衰减、意图慢衰减，写回 state.psyche。
///
/// 返回自动表情（宿主据此 ui.emit，"情绪跨轮连续"由此保证——衰减可查，不凭模型记忆）。
/// 结果作为 effect 事件落盘，因此**消息级重放会重新长出同一份心理状态**。
fn tick_psyche(
    proj: &event::Projection,
    character: &str,
    loaded: &card::LoadedCard,
    turn: u64,
) -> (Option<LogBody>, Option<String>) {
    let state = current_state(proj, character, loaded);
    // 卡没声明/没用心理运行时就不给它塞 psyche 块——静卡的 state 照旧保持干净
    // （「静态卡跑完什么都不写」是 M1 起就钉住的契约）
    if state.get(psyche::STATE_KEY).is_none() {
        return (None, None);
    }
    let mut p = psyche::Psyche::from_state(&state);
    p.tick(turn, 1.0);
    let mut next = state.clone();
    p.write_into(&mut next);
    let patch = event::state_patch(&state, &next);
    let body = if patch.is_empty() {
        None
    } else {
        Some(LogBody::Effect(event::EffectEvent {
            turn,
            trigger: "psyche.tick".into(),
            character: character.into(),
            state_set: patch,
            blackboard: Vec::new(),
            memory: Vec::new(),
            ts: store::unix_now(),
        }))
    };
    (body, p.auto_emotion())
}

/// 回复落定后的收尾（stream_reply 与单测共用同一份代码，避免两处等价逻辑）：
/// 回复事件 → 时钟步进事件 → on_message 事件 → 心理运行时推进。
///
/// 事件化后时钟步进也是事件：重放同一轮必然得到同一时刻（设计 §7.3-5）。
#[allow(clippy::too_many_arguments)]
fn commit_reply(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    turn: u64,
    text: &str,
    sink: Option<&card::UiSink>,
    log: &store::EventLog,
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
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
    let mut report = run_message_hook_core(root, meta, loaded, turn, sink, log);

    // 轮末：心理运行时推进（情绪衰减/意图慢衰减）——结果同样是事件
    let character = first_character(meta).unwrap_or_default();
    if let Ok(proj) = project_session(log, root, meta) {
        let (body, emotion) = tick_psyche(&proj, &character, loaded, turn);
        if let Some(body) = body {
            if let Err(e) = commit(log, root, meta, body) {
                report.logs.push(e);
            }
        }
        if let Some(emotion) = emotion {
            report.ui_events.push(llm::UiEmit {
                kind: "emotion".into(),
                value: emotion,
            });
        }
    }

    // 轮末：状态树转移求值（设计 §7.3-2「默认转移推迟到轮末」，保证一轮对话内状态稳定）
    if let Some(tree) = load_tree(loaded, tree_cache) {
        let active_entities = runtime.map(|r| r.previously_active(&meta.id));
        if let Ok(proj) = project_session(log, root, meta) {
            let (events, emits) = advance_state_tree(
                &proj,
                &character,
                loaded,
                &tree,
                turn,
                "on_turn_end",
                active_entities.as_ref(),
                meta.seed,
            );
            // 转移的界面事件（on_enter 的 ui.emit）与本轮钩子事件一起回给前端
            report.ui_events.extend(emits);
            for body in events {
                if let Err(e) = commit(log, root, meta, body) {
                    report.logs.push(e);
                }
            }
        }
    }
    Ok(report)
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
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
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
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
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
        Some(&runtime),
        Some(&tree_cache),
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
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
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
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
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
        Some(&runtime),
        Some(&tree_cache),
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
        extra: Default::default(),
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
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
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
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
    )?;
    Ok(run.assembly)
}

// ---------- 记忆检查器数据（M2.8 面板的数据源）----------

/// 组装检查器面板需要的全部投影数据（与 Tauri 无关的内核，便于单测）。
///
/// 一次给全：状态树路径与转移历史、剧情线、心理、宫殿三视图、设定集清单与揭示集。
/// 设计 §14：这些面板合称「记忆检查器」——看「我们处于哪个阶段、欠着什么线、
/// 她心里在想什么、她记得什么、这次注入了什么」。
fn inspector_payload(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    codex_cache: Option<&CodexCache>,
    tree_cache: Option<&TreeCache>,
    runtime: Option<&SessionRuntime>,
) -> Result<serde_json::Value, String> {
    let character = first_character(meta)?;
    let board = blackboard_of(proj);
    let bb_map = blackboard_env(&board);

    // 状态树：活跃路径 + 最近转移历史（新的在前）
    let tree = load_tree(loaded, tree_cache);
    let path = tree
        .as_ref()
        .map(|t| active_path_of(proj, t))
        .unwrap_or_default();
    let state_tree = tree.as_ref().map(|t| {
        serde_json::json!({
            "root": t.root,
            "path": path,
            "directive": t.directive_of(&path),
            "recall": t.recall_of(&path),
            "reveal": t.reveal_of(&path),
            "states": t.states.keys().cloned().collect::<Vec<_>>(),
            "warnings": t.validate(),
        })
    });
    let transitions: Vec<&event::TransitionEvent> =
        proj.transitions.iter().rev().take(20).collect();

    // 剧情线：活跃/已了结/已放弃 + C1 的只读投影
    let thread_list: Vec<threads::Thread> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .collect();
    let pick = |state: &str| -> Vec<serde_json::Value> {
        thread_list
            .iter()
            .filter(|t| t.state == state)
            .map(|t| t.to_value())
            .collect()
    };
    let window_text = scan_window_text(&proj.messages, &loaded.card.name);
    let mut mentions: Vec<String> = Vec::new();
    if let Some(cx) = codex_cache.map(|c| load_codex(root, Some(c), &session_world(meta))) {
        mentions.extend(cx.scan_mentions(&window_text).into_iter().map(|m| m.id));
    }
    mentions.extend(
        proj.messages
            .iter()
            .rev()
            .take(SCAN_WINDOW_MESSAGES)
            .map(|m| m.content.clone()),
    );
    let query = threads::ThreadQuery {
        turn: proj.last_message().map(|m| m.turn).unwrap_or(0),
        story_day: board.day,
        story_clock: &board.clock,
        blackboard: &bb_map,
        mentions: &mentions,
        present: &board.actors,
        state_path: &path,
    };
    let in_window: Vec<serde_json::Value> = threads::select_resurface(&thread_list, &query, 5)
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id, "title": p.title, "grade": p.grade,
                "framing": p.framing, "reason": p.reason,
            })
        })
        .collect();

    // 心理：内心摘要 + 情绪槽 + 意图 + 衰减轨迹
    let state = current_state(proj, &character, loaded);
    let p = psyche::Psyche::from_state(&state);
    let psyche_view = serde_json::json!({
        "summary": p.summary_line_for(&loaded.card.name),
        "affects": p.affects,
        "intents": p.intents,
        "trail": p.decay_trail(),
        "auto_emotion": p.auto_emotion(),
    });

    // 宫殿：三视图 + 最近记忆（每条都能溯源到轮次）
    let memories = memory_objects(proj, &character, board.day);
    let palace_view = serde_json::json!({
        "count": memories.len(),
        "rooms": palace::rooms(&memories),
        "timeline": palace::timeline(&memories),
        "graph": palace::link_graph(&memories),
        "recent": memories.iter().rev().take(20).map(palace::brief).collect::<Vec<_>>(),
    });

    // 设定集：世界清单（草稿与正史都列，注入只认 canon）
    let world = session_world(meta);
    let cx = load_codex(root, codex_cache, &world);
    let entities: Vec<serde_json::Value> = cx
        .entities()
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id, "name": e.name, "type": e.ty, "status": e.status,
                "oneLiner": e.one_liner, "anchors": e.anchors(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "session": meta.id,
        "character": loaded.card.name,
        "stateTree": state_tree,
        "transitions": transitions,
        "threads": {
            "active": pick("active"),
            "resolved": pick("resolved"),
            "abandoned": pick("abandoned"),
            "pending": threads::pending_lines(&thread_list),
            "inWindow": in_window,
            "eventCount": proj.thread_log.len(),
        },
        "psyche": psyche_view,
        "palace": palace_view,
        "codex": { "world": world, "count": entities.len(), "entities": entities },
        "known": proj.known.iter().cloned().collect::<Vec<_>>(),
        "blackboard": board,
        "activeEntities": runtime.map(|r| r.previously_active(&meta.id)).unwrap_or_default(),
    }))
}

/// 记忆检查器数据（M2.8 面板）：一次性给前端全部投影视图
#[tauri::command]
pub fn inspector_data(
    session_id: String,
    log: State<'_, store::EventLog>,
    codex_cache: State<'_, CodexCache>,
    tree_cache: State<'_, TreeCache>,
    runtime: State<'_, SessionRuntime>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let loaded = card::load_card(&root, &first_character(&meta)?).map_err(|e| e.to_string())?;
    let proj = project_session(&log, &root, &meta)?;
    inspector_payload(
        &root,
        &meta,
        &loaded,
        &proj,
        Some(&codex_cache),
        Some(&tree_cache),
        Some(&runtime),
    )
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
            None,
            None,
            None,
        )
        .unwrap();
        log.append(root, &meta.id, LogBody::Message(user_msg(turn, content)))
            .unwrap();
        let after_user = run_message_hook_core(root, meta, loaded, turn, None, log);
        let after_reply = commit_reply(root, meta, loaded, turn, "（回复）", None, log, None, None).unwrap();

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
        // 注：这里**不再断言诊断环形缓冲**——它是全进程共享的，并发跑测试时会被别的用例冲掉
        // （这个坑踩过两次）。「钩子必须看到用户消息本身」改由行为断言钉住，见本测试末尾。
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

        // 行为断言（不依赖任何全局缓冲）：一条含「谢谢」的**用户消息**单独落盘后跑钩子，
        // 好感度必须立刻 +1——这正是「卡看到了用户消息本身」的证据（若只看到回复，不会有变化）。
        log.append(&root, &meta.id, LogBody::Message(user_msg(9, "谢谢你。")))
            .unwrap();
        let report = run_message_hook_core(&root, &meta, &loaded, 9, None, &log);
        assert!(report.ran, "钩子应被触发");
        assert_eq!(
            report.card_state["favorability"], 53,
            "用户消息落盘即跑钩子：52 → 53"
        );
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
            None,
            None,
            None,
        )
        .unwrap();
        run_message_hook_core(&root, &meta, &loaded, turn, None, &log);
        commit_reply(&root, &meta, &loaded, turn, "（重roll 的回复）", None, &log, None, None).unwrap();

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
            None,
            None,
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

    /// M2.1/M2.2 接入验收：设定集实体进 B3（逐卡激活原因 + anchors 恒注入），
    /// 宫殿记忆进 B4（视角过滤之后，卡内私有记忆仍召回得到）。
    #[test]
    fn codex_entities_and_palace_memories_reach_the_prompt() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  hooks = {
    on_load = function(state, api) end,
    on_context = function(ctx, state)
      ctx.inject('system', string.format('【角色内部状态】好感度 %d/100', state.favorability))
    end,
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('谢谢') then
        state.favorability = math.min(100, state.favorability + 1)
        api.memory.set('last_thanked', msg.turn)
        api.blackboard.set('char.小雨.mood', '心情不错')
      end
    end,
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        // 造一个最小世界：角色实体（别名提及激活 + anchors）与地点实体（在场激活）
        let dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("char.小雨.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'char.小雨', type = 'char', name = '小雨',
  aliases = { '夜班管理员' },
  one_liner = '大学图书馆夜班管理员。',
  facts = { look = { impression = '旧毛衣', anchors = { '左眼角一颗泪痣' } } },
  live = { 'mood' },
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("place.自习区.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'place.自习区', type = 'place', name = '自习区',
  one_liner = '靠窗的一排长桌。',
}
"#,
        )
        .unwrap();

        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        // 第一轮：用户提到别名「夜班管理员」→ 提及激活；黑板地点「自习区」→ 在场激活
        let (assembly, _) = simulate_turn(&root, &meta, &loaded, &log, 1, "夜班管理员今天在吗？");
        let b3 = assembly
            .layers
            .iter()
            .find(|l| l.id == "B3")
            .expect("B3 实体卡层应出现");
        assert!(b3.content.starts_with("<world>") && b3.content.ends_with("</world>"));
        assert!(b3.content.contains("小雨"), "提及即激活：{}", b3.content);
        assert!(b3.content.contains("自习区"), "在场即激活：{}", b3.content);
        assert!(
            b3.content.contains("左眼角一颗泪痣"),
            "anchors 必须恒注入（设计 §6.3）：{}",
            b3.content
        );
        assert!(
            b3.sources
                .iter()
                .any(|s| s.contains("小雨") && s.contains("提及")),
            "逐卡激活原因（设计 §6.11）：{:?}",
            b3.sources
        );

        // 第二轮说到「谢谢」→ 卡内写记忆；第三轮的 B4 应召回它（视角过滤后仍命中）
        simulate_turn(&root, &meta, &loaded, &log, 2, "谢谢你。");
        let (assembly, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "今天也是。");
        let b4 = assembly
            .layers
            .iter()
            .find(|l| l.id == "B4")
            .expect("B4 回忆层应出现");
        assert!(b4.content.starts_with("<memory>") && b4.content.ends_with("</memory>"));
        assert!(
            b4.content.contains("last_thanked"),
            "卡内记忆应可召回：{}",
            b4.content
        );
        assert!(
            assembly.total_tokens >= b3.tokens + b4.tokens,
            "逐层 token 记账应含 B3/B4"
        );

        // 实体作用域黑板键（设计 §6.4）：卡写 char.小雨.mood → 实体的 live 字段拼出 ▸当前
        let bb = store::load_blackboard(&root, &meta.id).unwrap();
        assert_eq!(
            bb.extra.get("char.小雨.mood"),
            Some(&serde_json::json!("心情不错"))
        );
        let b3_now = assembly
            .layers
            .iter()
            .find(|l| l.id == "B3")
            .expect("B3 实体卡层");
        assert!(
            b3_now.content.contains("▸当前") && b3_now.content.contains("心情不错"),
            "live 应把黑板现状拼进实体卡：{}",
            b3_now.content
        );
        // 作用域键也进 hook / 设定集的读侧（平铺 + 嵌套两种形态都给）
        let env = blackboard_env(&bb);
        assert_eq!(
            env.get("char.小雨.mood"),
            Some(&serde_json::json!("心情不错"))
        );
        assert_eq!(
            env.get("char.小雨").and_then(|v| v.get("mood")),
            Some(&serde_json::json!("心情不错"))
        );
    }

    /// 仓库自带的世界能真的加载成 Codex（真机数据是最有说服力的样本）：
    /// 钉住「实体 schema 改了没人发现」，也钉住目录指纹缓存真的生效。
    #[test]
    fn workspace_worlds_load_into_codex() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../DataHub");
        if !root.is_dir() {
            return; // 打包/CI 环境没有 DataHub 时跳过
        }
        let cache = CodexCache::default();
        let mut checked = 0;
        for world in ["default", "崩坏3"] {
            let dir = codex_entities_dir(&root, world);
            if !dir.is_dir() {
                continue;
            }
            let cx = load_codex(&root, Some(&cache), world);
            assert!(!cx.entities().is_empty(), "世界 {world} 应至少解析出一个实体");
            for e in cx.entities() {
                assert!(!e.id.trim().is_empty() && !e.name.trim().is_empty());
            }
            // 指纹没变 → 复用同一份解析结果（每轮重解析上百个 Lua 文件是浪费）
            let again = load_codex(&root, Some(&cache), world);
            assert!(Arc::ptr_eq(&cx, &again), "世界 {world} 的缓存应命中");
            checked += 1;
        }
        assert!(checked > 0, "仓库里应至少有一个世界");
    }

    /// M2.4 验收（设计 §8.4「克制是核心」）：线**只在可提及窗口内**进现状卡，
    /// 窗口外只由 C1 的只读投影兜底——「每轮强行提起」在结构上被挡住。
    #[test]
    fn threads_only_surface_inside_their_window() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();

        // 一条 natural 线（mention 窗口 + day>=5 窗口），一条 dormant 线（即使被提及也不该进 B1）
        let mut due = threads::Thread::open(
            "thread.周五还书",
            "周五还书的约定",
            "玩家忘带借书卡，约定周五来还。",
            &["小雨".into(), "玩家".into()],
            0.8,
            threads::ThreadStamp {
                turn: 1,
                story_day: 1,
                story_clock: "20:00".into(),
            },
        );
        due.resurface.windows = vec![threads::ResurfaceWindow::Mention(vec!["还书".into()])];
        due.resurface.framing = "她在意但不好意思催。".into();
        let mut buried = threads::Thread::open(
            "thread.旧伤",
            "不提的旧伤",
            "她从没说过的事。",
            &["小雨".into()],
            0.4,
            threads::ThreadStamp {
                turn: 1,
                story_day: 1,
                story_clock: "20:00".into(),
            },
        );
        buried.resurface.grade = "dormant".into();
        buried.resurface.windows = vec![threads::ResurfaceWindow::Mention(vec!["旧伤".into()])];
        for t in [&due, &buried] {
            log.append(
                &root,
                &meta.id,
                LogBody::Thread(event::ThreadEvent {
                    turn: 1,
                    op: threads::OP_OPEN.into(),
                    thread_id: t.id.clone(),
                    thread: Some(t.to_value()),
                    origin: threads::ORIGIN_MANUAL.into(),
                    note: None,
                    ts: store::unix_now(),
                }),
            )
            .unwrap();
        }

        let layer = |a: &prompt::PromptAssembly, id: &str| {
            a.layers
                .iter()
                .find(|l| l.id == id)
                .map(|l| l.content.clone())
                .unwrap_or_default()
        };

        // 第一轮：窗口未命中（day=1、无提及）→ B1 不得出现「心里有事」，C1 仍列得出这条欠账
        let (a1, _) = simulate_turn(&root, &meta, &loaded, &log, 1, "今天天气不错。");
        assert!(
            !layer(&a1, "B1").contains("心里有事"),
            "窗口未命中时线绝不进现状卡：{}",
            layer(&a1, "B1")
        );
        assert!(
            layer(&a1, "C1").contains("周五还书的约定"),
            "全量欠账由 C1 兜底（六要素完备性不牺牲）：{}",
            layer(&a1, "C1")
        );

        // 第二轮说了「还书」——它在本轮组装之后才落盘，故本轮仍不该进 B1
        let (a2, _) = simulate_turn(&root, &meta, &loaded, &log, 2, "那本《夜航》我明天还书。");
        assert!(!layer(&a2, "B1").contains("心里有事"), "提及落在本轮之后");

        // 第三轮：窗口里已有「还书」→ 进 B1，且带 framing（不是任务清单，是情境脉冲）
        let (a3, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "嗯，怎么了？");
        let b1 = layer(&a3, "B1");
        assert!(b1.contains("心里有事"), "窗口命中后应进现状卡：{b1}");
        assert!(b1.contains("周五还书的约定") && b1.contains("不好意思催"), "带 framing：{b1}");
        assert!(
            !b1.contains("不提的旧伤"),
            "dormant 线即使被提及也不进 B1：{b1}"
        );

        // 收线 → 「了结未远」出现在 B1，且不再是未决事项
        let mut resolved = due.clone();
        resolved.resolve(3, 1, "20:40", "玩家如约还书，小雨送了张便签。");
        log.append(
            &root,
            &meta.id,
            LogBody::Thread(event::ThreadEvent {
                turn: 3,
                op: threads::OP_RESOLVE.into(),
                thread_id: resolved.id.clone(),
                thread: Some(resolved.to_value()),
                origin: threads::ORIGIN_MANUAL.into(),
                note: Some("如约还书".into()),
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        let (a4, _) = simulate_turn(&root, &meta, &loaded, &log, 4, "那就好。");
        assert!(
            layer(&a4, "B1").contains("了结未远"),
            "近期收线应进「了结未远」：{}",
            layer(&a4, "B1")
        );
        assert!(
            !layer(&a4, "C1").contains("周五还书的约定"),
            "收线后不该再列为未决事项：{}",
            layer(&a4, "C1")
        );
    }

    /// M2.5 验收（设计 §9.2）：情绪跨轮连续——三轮前的强情绪仍有余波，且衰减轨迹可查；
    /// B5 的「内心」一行把主观世界带给模型（不凭它自己的记忆）。
    #[test]
    fn affect_carries_across_turns_and_decays() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50, psyche = { affects = {}, intents = {} } },
  hooks = {
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('谢谢') then
        state.psyche.affects = { { name = '喜悦', intensity = 0.9, source = '被道谢' } }
      end
    end,
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();

        let intensity = |root: &std::path::Path, meta: &store::SessionMeta| -> f64 {
            stored_state(root, meta)["psyche"]["affects"][0]["intensity"]
                .as_f64()
                .unwrap_or(0.0)
        };

        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        let after1 = intensity(&root, &meta);
        assert!(after1 > 0.5, "轮末已衰减一次但仍强：{after1}");

        simulate_turn(&root, &meta, &loaded, &log, 2, "今天也好。");
        let (a3, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "嗯。");
        let after3 = intensity(&root, &meta);
        assert!(after3 < after1, "应逐轮衰减：{after1} → {after3}");
        assert!(after3 > 0.2, "三轮后仍应有可测余波：{after3}");

        let b5 = a3
            .layers
            .iter()
            .find(|l| l.id == "B5" && l.name == "内心")
            .expect("B5「内心」层");
        assert!(b5.content.contains("喜悦"), "B5 应带上内心摘要：{}", b5.content);
        assert!(b5.content.contains("小雨"), "摘要应带角色名：{}", b5.content);

        // 面板要的衰减轨迹：每轮都留下采样
        let hist = stored_state(&root, &meta)["psyche"]["affects"][0]["history"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(hist >= 3, "衰减轨迹应随轮次增长：{hist}");
    }

    /// M2.3 验收（设计 §7.3/§7.4）：状态树活跃路径的 directive 进 B2；
    /// 轮末求值首个命中即转移，转移与 reveal 都进事件流（可回放）。
    #[test]
    fn state_tree_drives_directive_and_transitions() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  hooks = {
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('夜深') then
        state.favorability = 80
        api.blackboard.set('clock', '23:10')
      end
    end,
  },
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '保持轻松日常的氛围，话题围绕图书馆与学业。',
        transitions = {
          { to = '日常.夜谈', priority = 10,
            when = function(ev, bb, st) return bb.clock >= '23:00' and st.favorability >= 60 end },
        },
      },
      ['日常.夜谈'] = {
        parent = '日常',
        directive = '夜深人静，两人独处。语速放慢，允许长时间沉默。',
        reveal = { 'char.小雨.secrets.工作牌' },
        on_enter = function(api, state)
          state.in_night = true
          api.blackboard.set('place.图书馆.status', '闭馆中')
          api.ui.emit('emotion', 'calm')
        end,
      },
    },
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();

        // 第一轮：还没到 23 点、好感度 50 → 不转移，B2 是根状态的指令
        let (a1, _) = simulate_turn(&root, &meta, &loaded, &log, 1, "今天也在看书吗？");
        let b2 = a1
            .layers
            .iter()
            .find(|l| l.id == "B2")
            .expect("B2 指令层");
        assert!(b2.content.starts_with("<directive>"), "{}", b2.content);
        assert!(b2.content.contains("轻松日常"), "{}", b2.content);
        let proj = project_session(&log, &root, &meta).unwrap();
        assert!(proj.transitions.is_empty(), "条件不满足不该转移");

        // 第二轮：钩子把好感度推到 80、时钟推到 23:10 → 轮末（on_turn_end）转移
        let (a2, r2) = simulate_turn(&root, &meta, &loaded, &log, 2, "夜深了，你还不回去吗？");
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(proj.transitions.len(), 1, "应恰好转移一次（一轮内状态稳定）");
        assert_eq!(proj.transitions[0].to, vec!["日常", "日常.夜谈"]);
        assert!(
            proj.transitions[0].reason.contains("日常.夜谈"),
            "转移原因应可读：{}",
            proj.transitions[0].reason
        );
        // reveal 进设定事件 → 投影的揭示集
        assert!(
            proj.known.contains("char.小雨.secrets.工作牌"),
            "进入状态应揭示秘密：{:?}",
            proj.known
        );
        // 转移发生在轮末：本轮组装仍是根状态的指令，下一轮才换
        assert!(a2.layers.iter().find(|l| l.id == "B2").unwrap().content.contains("轻松日常"));
        // on_enter 的副作用随转移落盘（设计 §7.3-3）：state、黑板作用域键、界面事件
        assert_eq!(
            stored_state(&root, &meta)["in_night"],
            serde_json::json!(true),
            "on_enter 应能改 state"
        );
        let bb = store::load_blackboard(&root, &meta.id).unwrap();
        assert_eq!(
            bb.extra.get("place.图书馆.status"),
            Some(&serde_json::json!("闭馆中")),
            "on_enter 应能写黑板作用域键"
        );
        assert!(
            r2.ui_events
                .iter()
                .any(|e| e.kind == "emotion" && e.value == "calm"),
            "on_enter 的 ui.emit 应进本轮报告：{:?}",
            r2.ui_events
        );

        // 第三轮：B2 变成根→叶拼接（子覆盖父）
        let (a3, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "……嗯。");
        let b2 = a3.layers.iter().find(|l| l.id == "B2").unwrap();
        assert!(
            b2.content.contains("轻松日常") && b2.content.contains("夜深人静"),
            "根→叶都要在：{}",
            b2.content
        );
        assert!(
            b2.content.find("轻松日常").unwrap() < b2.content.find("夜深人静").unwrap(),
            "父在前、子在后（子覆盖父）：{}",
            b2.content
        );

        // 重放同一事件流得到同一条路径（设计 §7.3-5）
        let records = log.read(&root, &meta.id).unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &records, 1).unwrap();
        let c = event::project_over(&rebuilt, &event::Base::default());
        assert_eq!(c.transitions, proj.transitions, "重放得到同一批转移");
        assert_eq!(c.known, proj.known, "重放得到同一份揭示集");
    }

    /// M2.8 面板数据源：一次给全状态树/线/心理/宫殿/设定集的投影视图
    #[test]
    fn inspector_payload_reports_every_panel() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50, psyche = { affects = {}, intents = {} } },
  hooks = {
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('谢谢') then
        state.favorability = state.favorability + 1
        api.memory.set('last_thanked', msg.turn)
        state.psyche.affects = { { name = '喜悦', intensity = 0.8, source = '被道谢' } }
      end
    end,
  },
  state_tree = {
    root = '日常',
    states = { ['日常'] = { directive = '轻松日常。' } },
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");

        let proj = project_session(&log, &root, &meta).unwrap();
        let payload = inspector_payload(&root, &meta, &loaded, &proj, None, None, None).unwrap();

        assert_eq!(payload["character"], "小雨");
        let tree = &payload["stateTree"];
        assert_eq!(tree["root"], "日常");
        assert_eq!(tree["path"][0], "日常", "无转移时用树根播种");
        assert!(tree["directive"].as_str().unwrap().contains("轻松日常"));
        assert_eq!(payload["transitions"].as_array().unwrap().len(), 0);
        assert!(payload["blackboard"]["day"].as_i64().unwrap() >= 1);
        assert_eq!(payload["palace"]["count"], 1, "卡写的记忆应进宫殿视图");
        assert_eq!(payload["palace"]["recent"][0]["content"], "last_thanked：1");
        // 卡内键值记忆没有地点（设计 §5.2 的房间 = 故事地点），故不进房间图；时间线必须有桶
        assert!(
            !payload["palace"]["timeline"].as_array().unwrap().is_empty(),
            "时间线走廊应能列出这条记忆"
        );
        assert!(
            payload["psyche"]["summary"].as_str().unwrap().contains("喜悦"),
            "心理面板应给内心摘要：{}",
            payload["psyche"]["summary"]
        );
        assert!(payload["psyche"]["trail"].as_array().unwrap().len() >= 1, "衰减轨迹可查");
        assert_eq!(payload["threads"]["active"].as_array().unwrap().len(), 0);
        assert_eq!(payload["codex"]["world"], "default");
        assert!(payload["known"].as_array().unwrap().is_empty());
    }

    /// M2.6 数据模型：摘要增量与设定提案进事件流，派生文件（summary.md / proposals.jsonl）由投影写出；
    /// 它们是**模型产物**、不是确定性派生，所以编辑历史的重建不会把它们丢掉。
    #[test]
    fn summary_and_proposals_are_projected_to_files() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let log = store::EventLog::new();

        for (turn, delta) in [
            (2u64, "第一段：她记住了那个约定。"),
            (4u64, "第二段：约定如约了结。"),
        ] {
            log.append(
                &root,
                &meta.id,
                LogBody::Summary(event::SummaryEvent {
                    turn,
                    delta: delta.into(),
                    from_turn: turn - 1,
                    to_turn: turn,
                    ts: 0,
                }),
            )
            .unwrap();
        }
        log.append(
            &root,
            &meta.id,
            LogBody::Proposal(event::ProposalEvent {
                turn: 2,
                id: "p1".into(),
                op: "propose".into(),
                kind: "new_fact".into(),
                origin: "pipeline".into(),
                payload: Some(serde_json::json!({"key": "猫名", "value": "墨墨"})),
                note: None,
                ts: 0,
            }),
        )
        .unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Proposal(event::ProposalEvent {
                turn: 3,
                id: "p1".into(),
                op: "accept".into(),
                kind: String::new(),
                origin: "manual".into(),
                payload: None,
                note: Some("玩家确认".into()),
                ts: 0,
            }),
        )
        .unwrap();
        sync_now(&log, &root, &meta).unwrap();

        let summary = store::read_summary(&root, &meta.id).unwrap();
        assert!(summary.contains("第一段") && summary.contains("第二段"));
        assert!(
            summary.find("第一段").unwrap() < summary.find("第二段").unwrap(),
            "摘要增量按序拼接：{summary}"
        );
        let proposals = store::read_proposals(&root, &meta.id).unwrap();
        assert_eq!(proposals.len(), 1, "同一提案的确认不新增行");
        assert_eq!(proposals[0]["status"], "accept");
        assert_eq!(
            proposals[0]["payload"]["value"], "墨墨",
            "确认不丢提案正文：{}",
            proposals[0]
        );

        // 模型产物保留：编辑历史的重建不丢弃摘要与提案
        let records = log.read(&root, &meta.id).unwrap();
        let loaded = card::load_card(&root, "小雨").unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &loaded, &records, 1).unwrap();
        assert!(
            rebuilt
                .iter()
                .any(|r| matches!(&r.body, LogBody::Summary(_))),
            "摘要应保留"
        );
        assert!(
            rebuilt
                .iter()
                .any(|r| matches!(&r.body, LogBody::Proposal(_))),
            "提案应保留"
        );
    }
}
