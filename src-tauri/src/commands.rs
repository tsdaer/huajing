//! Tauri 命令层：前端可调用的入口。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};

use crate::card;
use crate::codex;
use crate::complete;
use crate::director;
use crate::event::{self, LogBody, LogRecord};
use crate::ingest;
use crate::palace;
use crate::psyche;
use crate::scene;
use crate::semantic;
use crate::statetree;
use crate::summarize;
use crate::threads;
use crate::toolcall;
use crate::llm::{self, Provider, StreamEvent};
use crate::prompt;
use crate::store::{self, Message, NewSessionRequest, Settings};
use crate::worldline;

// 命令域子模块（E1 物理拆分）：接入点/场景/剧场与世界主线/总结/素材管线。
// 各域经 `pub use` 顶回 commands 根——lib.rs 的注册表与既有调用点不受影响。
pub mod ingestion;
pub mod providers;
pub mod scenes;
pub mod summary;
pub mod theater;

pub use ingestion::*;
pub use providers::*;
pub use scenes::*;
pub use summary::*;
pub use theater::*;

#[tauri::command]
pub fn app_info() -> serde_json::Value {
    serde_json::json!({
        "name": "化境 Huajing",
        "slogan": "化万千相，随心入境。",
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

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)] // 前端一次性传入向导的全部字段，保持扁平更好用
pub fn new_session(
    app: AppHandle,
    character: String,
    characters: Option<Vec<String>>,
    persona: Option<String>,
    day: Option<i64>,
    clock: Option<String>,
    place: Option<String>,
    premise: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<store::SessionMeta, String> {
    let root = root();
    // 世界时钟基准（M3.7 · 设计 §6.6）：没有显式指定天就从世界时钟出发——
    // 新会话开局即续上世界的大势（「各会话读取为开局基准」）；显式天 = 玩家指定的
    // 时点（flashback 由此成立——回写是 max，更早的会话拉不低世界）
    let day = baseline_day_from_world(&root, day);
    // 角色阵容（M3.1）：显式给的全量用（首个是主角色），没给就单角色——1v1 行为不变
    let mut cast = characters.unwrap_or_default();
    if !cast.contains(&character) {
        cast.insert(0, character.clone());
    }
    let req = NewSessionRequest {
        character,
        characters: cast,
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
            scene_id: None,
            board: board.clone(),
            ts: store::unix_now(),
        }),
    )
    .map_err(|e| e.to_string())?;

    // 缺省场景落地（M3.2 · 设计 §10.3）：从初始黑板长出 scene.main。
    // 新会话从第一轮起就有场景归属（消息分段、黑板分区、摘要分卷都有了挂靠点）；
    // 老会话不迁移，读侧把 scene_id: None 归一到同一个 id。
    let scene = scene::Scene::from_board(
        scene::DEFAULT_SCENE_ID,
        "开场",
        &board,
        0,
        "default",
        store::unix_now(),
    );
    log.append(
        &root,
        &meta.id,
        LogBody::Scene(event::SceneEvent {
            turn: 0,
            op: "create".into(),
            scene_id: scene.id.clone(),
            scene: Some(scene),
            others: Vec::new(),
            origin: "default".into(),
            note: None,
            ts: store::unix_now(),
        }),
    )
    .map_err(|e| e.to_string())?;

    // 全阵容入席（M3.1）：每张卡都跑 on_load（state 就位、初始化记忆/黑板写入）；
    // 开场白只取主角色（群聊的开场调度是 M3.4 导演的事）
    for dir in &meta.characters {
        let Ok(loaded) = card::load_card(&root, dir) else {
            continue; // 卡片读取失败不阻塞建会话（M1 起的行为）
        };
        if dir == meta.characters.first().expect("阵容非空") {
            let first = loaded.card.first_mes.trim();
            if !first.is_empty() {
                let opening = Message {
                    turn: 0,
                    role: "char".into(),
                    content: first.to_string(),
                    ts: store::unix_now(),
                    scene_id: Some(scene::DEFAULT_SCENE_ID.into()),
                    name: Some(display_name_of(&loaded)),
                    tool_calls: None,
                };
                let _ = log.append(&root, &meta.id, LogBody::Message(opening));
            }
        }
        run_load_hook(&app, &root, &meta, &loaded, &log, "hook.on_load")?;
    }
    Ok(meta)
}

/// 卡的显示名（目录名兜底；消息署名与界面用）
fn display_name_of(loaded: &card::LoadedCard) -> String {
    if loaded.card.name.trim().is_empty() {
        loaded.dir_name.clone()
    } else {
        loaded.card.name.clone()
    }
}

// ---------- 事件流：投影 / 派生文件 / 重放（M2.0 · 设计 §7.3「可回放」）----------

/// 会话的首个角色（M2 仍是 1v1；M3 群聊按角色分别投影）
fn first_character(meta: &store::SessionMeta) -> Result<String, String> {
    meta.characters
        .first()
        .cloned()
        .ok_or_else(|| "会话未配置角色".to_string())
}

/// 检查器/预览的视角解析（M3.11 多角色检查器）：显式角色必须在阵容里，
/// 缺省 = 主角色。1v1 走同一条路径，行为不变。
fn resolve_viewpoint(
    meta: &store::SessionMeta,
    character: Option<String>,
) -> Result<String, String> {
    match character {
        Some(dir) if meta.characters.iter().any(|c| c == &dir) => Ok(dir),
        Some(dir) => Err(format!("角色「{dir}」不在这个会话的阵容里")),
        None => first_character(meta),
    }
}

/// 角色阵容的一个成员：目录名 + 已装载卡
pub struct CastMember {
    pub dir: String,
    pub loaded: card::LoadedCard,
}

/// 角色阵容（M3.1 隔离模式 · 设计 §10.2）：每轮发言 = 发言人独立的上下文组装与请求。
/// 单角色会话同样是它（一个成员）——1v1 走同一条代码路径，行为与 M2 一致（退化不浪费）。
pub struct Cast {
    pub members: Vec<CastMember>,
}

impl Cast {
    /// 装载会话的全阵容（发送路径不该静默吞掉阵容缺员，读卡失败即错）
    pub fn load(root: &std::path::Path, meta: &store::SessionMeta) -> Result<Cast, String> {
        let mut members = Vec::new();
        for dir in &meta.characters {
            let loaded = card::load_card(root, dir).map_err(|e| e.to_string())?;
            members.push(CastMember {
                dir: dir.clone(),
                loaded,
            });
        }
        if members.is_empty() {
            return Err("会话未配置角色".to_string());
        }
        Ok(Cast { members })
    }

    pub fn first(&self) -> &CastMember {
        &self.members[0]
    }

    pub fn get(&self, dir: &str) -> Option<&CastMember> {
        self.members.iter().find(|m| m.dir == dir)
    }

    /// 解析本轮发言人：None = 主角色；Some 必须在阵容里
    pub fn resolve(&self, speaker: Option<&str>) -> Result<&CastMember, String> {
        match speaker {
            None => Ok(self.first()),
            Some(s) => self
                .get(s)
                .ok_or_else(|| format!("角色「{s}」不在这个会话的阵容里")),
        }
    }

    /// 署名（消息 name 字段与界面显示用；卡名优先，目录名兜底）
    pub fn display_name(&self, dir: &str) -> String {
        match self.get(dir) {
            Some(m) => display_name_of(&m.loaded),
            None => dir.to_string(),
        }
    }

    /// 全阵容署名（A1 契约的「你只扮演 X」提示用）
    pub fn display_names(&self) -> Vec<String> {
        self.members.iter().map(|m| display_name_of(&m.loaded)).collect()
    }

    /// 阵容里是否有多于一个角色（隔离提示与 UI 的判据）
    pub fn is_multi(&self) -> bool {
        self.members.len() > 1
    }
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

/// 读事件流并投影（加固 D1-2：genesis 流走增量投影缓存，成本 O(新增记录) 而非
/// O(流长度)；老会话（基线来自派生文件）保持全量投影）。
fn project_session(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<std::sync::Arc<event::Projection>, String> {
    let records = log.read(root, &meta.id).map_err(|e| e.to_string())?;
    Ok(project_cached(log, root, meta, &records))
}

/// D1-2：带缓存的投影——genesis 流的折叠是纯函数（基线为空），缓存可续；
/// 老会话的基线来自派生文件，不在缓存语义内，保持全量。
fn project_cached(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    records: &std::sync::Arc<Vec<LogRecord>>,
) -> std::sync::Arc<event::Projection> {
    if event::has_genesis(records) {
        return log
            .project_cached_records(&meta.id, records, |rs| {
                event::project_over(rs, &event::Base::default())
            })
            .unwrap_or_else(|_| {
                std::sync::Arc::new(event::project_over(records, &event::Base::default()))
            });
    }
    std::sync::Arc::new(event::project_over(records, &base_for(root, meta, records)))
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
    // summary.md：世界层大事记 + 各场景分卷（可读排版；注入走 summary_for，不经过这里）
    let mut summary_text = proj.summary.clone();
    for (id, volume) in &proj.scene_summaries {
        if volume.trim().is_empty() {
            continue;
        }
        let title = proj
            .scenes
            .get(id)
            .map(|sc| sc.title.clone())
            .unwrap_or_else(|| id.clone());
        summary_text.push_str(&format!("\n\n【{title}】\n{volume}"));
    }
    store::write_summary(root, &meta.id, summary_text.trim()).map_err(|e| e.to_string())?;
    let proposals: Vec<serde_json::Value> = proj.proposals.values().cloned().collect();
    store::write_proposals(root, &meta.id, &proposals).map_err(|e| e.to_string())?;
    // 场景投影（M3.2）：场景表 + 聚焦场景，明文可查
    let scenes: Vec<scene::Scene> = proj.scenes.values().cloned().collect();
    store::write_scenes(root, &meta.id, &scenes, proj.active_scene.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 只重投影 + 落派生文件（没有新事件时用）
fn sync_now(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<std::sync::Arc<event::Projection>, String> {
    let proj = project_session(log, root, meta)?;
    sync_derived(root, meta, &proj)?;
    Ok(proj)
}

/// 命令前奏三连（E1：root + 会话元数据 + 事件流投影）的合并体，
/// 只读命令入口用；需要中途改 meta 或先读卡再投影的路径保持手写展开。
struct SessionCtx {
    root: std::path::PathBuf,
    meta: store::SessionMeta,
    proj: std::sync::Arc<event::Projection>,
}

fn session_ctx(log: &store::EventLog, session_id: &str) -> Result<SessionCtx, String> {
    let root = root();
    let meta = store::load_session(&root, session_id).map_err(|e| e.to_string())?;
    let proj = project_session(log, &root, &meta)?;
    Ok(SessionCtx { root, meta, proj })
}

/// 一批事件一次落盘（加固 D1-1）：append N 条 + **一次**投影 + **一次**派生文件同步。
/// 一轮的轮末推进/总结批次原来逐条 commit——每条都投影 + 重写 6 个派生文件
///（palace.jsonl 是全量记忆），单轮累计 O(n²)。单事件路径 [`commit`] 是 batch=1 的包装。
fn commit_batch(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    bodies: Vec<LogBody>,
) -> Result<std::sync::Arc<event::Projection>, String> {
    for body in bodies {
        log.append(root, &meta.id, body).map_err(|e| e.to_string())?;
    }
    sync_now(log, root, meta)
}

/// 追加事件 → 重投影 → 落派生文件。**这是会话状态唯一的写入路径。**
fn commit(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    body: LogBody,
) -> Result<std::sync::Arc<event::Projection>, String> {
    commit_batch(log, root, meta, vec![body])
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

/// 代理设置读取（E1 提取）：设置页手填的代理（空串视同未填）
fn proxy_of(root: &std::path::Path) -> Option<String> {
    store::load_settings(root)
        .ok()
        .and_then(|s| s.proxy)
        .filter(|p| !p.trim().is_empty())
}

/// 提案事件构造器（E1 提取）：`commit(.., LogBody::Proposal(ProposalEvent { .. }))`
/// 的骨架在总结管线/收件箱/剧场里重复十余处，各处只差字段值。
#[allow(clippy::too_many_arguments)]
fn proposal_event(
    turn: u64,
    id: String,
    op: &str,
    kind: &str,
    origin: &str,
    payload: Option<serde_json::Value>,
    note: Option<String>,
    ts: u64,
) -> LogBody {
    LogBody::Proposal(event::ProposalEvent {
        turn,
        id,
        op: op.into(),
        kind: kind.into(),
        origin: origin.into(),
        payload,
        note,
        ts,
    })
}

/// 会话当前的聚焦场景（多场景会话才有；单场景/老会话 = None，一切读侧退化为世界层）
fn scene_ctx(proj: &event::Projection) -> Option<String> {
    proj.active_scene_id().map(str::to_string)
}

/// 消息流的场景视图（设计 §10.3 消息流分段）：多场景会话只取该场景的消息——
/// 别的场景发生的事是「与此同时」的叙事盲区，不进本场景的组装与钩子窗口。
fn scene_messages(messages: &[Message], scene: Option<&str>) -> Vec<Message> {
    match scene {
        None => messages.to_vec(),
        Some(id) => messages
            .iter()
            .filter(|m| scene::normalize(m.scene_id.as_deref()) == id)
            .cloned()
            .collect(),
    }
}

/// 本轮参与轮转的成员：所在场景的在场者（actors 空 = 不设限，兼容旧黑板）。
/// 被切走的场景冻结——不在场的角色不跑钩子、不推心理、不求值状态树。
fn present_members<'a>(
    cast: &'a Cast,
    proj: &event::Projection,
    scene: Option<&str>,
) -> Vec<&'a CastMember> {
    match scene.and_then(|id| proj.scenes.get(id)) {
        None => cast.members.iter().collect(),
        Some(sc) => cast
            .members
            .iter()
            .filter(|m| sc.has_actor(&m.dir))
            .collect(),
    }
}

/// 场景有效的归一 id：会话里真的存在这个场景分区时返回 Some（否则 None——
/// 老事件/老会话的写入路由回世界层，行为与 M2 完全一致）
fn scoped_scene(proj: &event::Projection, scene_id: Option<&str>) -> Option<String> {
    scene_id
        .filter(|id| proj.scenes.contains_key(*id))
        .map(str::to_string)
}

/// 一次钩子运行 → 事件。没改动就不记（事件流只留真发生的事）；
/// 「入席建基线」（force_baseline）例外：即使与卡上默认值相同也要记，
/// 因为它定义的正是这个会话的起点。
///
/// `scene`（M3.2）：钩子运行所在的场景——黑板写入按它路由到场景分区
/// （地点/在场者/时钟），实体键照旧归世界层。None = 无场景会话（世界层）。
#[allow(clippy::too_many_arguments)]
fn hook_effect(
    run: &card::HookRun,
    before: &serde_json::Value,
    character: &str,
    turn: u64,
    trigger: &str,
    force_baseline: bool,
    scene: Option<&str>,
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
        scene_id: scene.map(str::to_string),
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
    // 入席发生在聚焦场景里（M3.2）：on_load 的黑板写入按场景路由
    let scene = scene_ctx(&proj);
    let run = card::run_hook_full(
        &loaded.source,
        card::HookCall::OnLoad,
        &card::HookEnv {
            state: state.clone(),
            blackboard: blackboard_env(&proj.effective_board(scene.as_deref())),
            memory: memory_env(&proj.memory),
            turn: 0,
        },
        meta.seed,
        sink,
    );
    apply_load_hook(root, meta, log, source, &run, &state, scene.as_deref())
}

/// `on_load` 的落盘：入席是「建立基线」——生效后的 state 作为基线补丁记进事件流
/// （此后一律以会话为准，改卡的默认值不回头覆盖已有会话）。
/// 与 [`apply_message_hook`] 分开：on_message 只记「真变了的」，on_load 必记。
#[allow(clippy::too_many_arguments)]
fn apply_load_hook(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
    source: &str,
    run: &card::HookRun,
    before: &serde_json::Value,
    scene: Option<&str>,
) -> Result<llm::HookReport, String> {
    let character = first_character(meta)?;
    let proj = match hook_effect(run, before, &character, 0, source, true, scene) {
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

/// 编辑指定下标的消息内容（加固 A4/B1 包装）：入口获取会话写入闸门并全程持有，
/// 生成/编辑进行中被拒；主体见 [`edit_message_body`]。
#[tauri::command(async)]
pub fn edit_message(
    session_id: String,
    index: usize,
    content: String,
    log: State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let guard = gate()
        .acquire(&session_id)
        .ok_or_else(|| "会话写入进行中，请等当前操作结束".to_string())?;
    let result = edit_message_body(&session_id, index, content, &log);
    drop(guard);
    flush_parked_summaries(&root(), &session_id);
    result
}

/// 编辑主体：改写该消息事件 → 丢弃它所在轮次起的派生事件 → 重放重算。
///
/// 这正是 M1 遗留问题的解药：改掉那句「谢谢」，好感度会跟着退回去
/// （设计 §7.3-5「转移是事件流的纯函数」）。返回更新后的全量消息。
fn edit_message_body(
    session_id: &str,
    index: usize,
    content: String,
    log: &State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let meta = store::load_session(&root, session_id).map_err(|e| e.to_string())?;
    let cast = Cast::load(&root, &meta)?;
    let records = log.read(&root, session_id).map_err(|e| e.to_string())?;
    let Some((pos, turn)) = locate_message(&records, index) else {
        return Err(format!("消息下标越界：{index}"));
    };
    let mut edited = records.as_ref().clone();
    if let LogBody::Message(m) = &mut edited[pos].body {
        m.content = content;
    }
    let rebuilt = rebuild_from(log, &root, &meta, &cast, &edited, turn)?;
    log.rewrite(&root, session_id, &rebuilt)
        .map_err(|e| e.to_string())?;
    sync_now(log, &root, &meta)?;
    Ok(event::messages(&rebuilt))
}

/// 删除指定下标的消息（加固 A4/B1 包装）：入口获取会话写入闸门并全程持有；
/// 主体见 [`delete_message_body`]。
#[tauri::command(async)]
pub fn delete_message(
    session_id: String,
    index: usize,
    log: State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let guard = gate()
        .acquire(&session_id)
        .ok_or_else(|| "会话写入进行中，请等当前操作结束".to_string())?;
    let result = delete_message_body(&session_id, index, &log);
    drop(guard);
    flush_parked_summaries(&root(), &session_id);
    result
}

/// 删除主体：移除该消息事件 → 从它所在轮次起重放重算，返回更新后的全量消息
fn delete_message_body(
    session_id: &str,
    index: usize,
    log: &State<'_, store::EventLog>,
) -> Result<Vec<Message>, String> {
    let root = root();
    let meta = store::load_session(&root, session_id).map_err(|e| e.to_string())?;
    let cast = Cast::load(&root, &meta)?;
    let records = log.read(&root, session_id).map_err(|e| e.to_string())?;
    let Some((pos, turn)) = locate_message(&records, index) else {
        return Err(format!("消息下标越界：{index}"));
    };
    let mut edited = records.as_ref().clone();
    edited.remove(pos);
    let rebuilt = rebuild_from(log, &root, &meta, &cast, &edited, turn)?;
    log.rewrite(&root, session_id, &rebuilt)
        .map_err(|e| e.to_string())?;
    sync_now(log, &root, &meta)?;
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
    cast: &Cast,
    records: &[LogRecord],
    from_turn: u64,
) -> Result<Vec<LogRecord>, String> {
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
    // 各角色的状态树只解析一次（轮末求值与手动收线的补求值共用）
    let trees: Vec<Option<std::sync::Arc<statetree::StateTree>>> =
        cast.members.iter().map(|m| load_tree(&m.loaded, None)).collect();
    // 工具提案的幂等集（增强 A·决断 3）：提案事件不是派生事件、重放保留，
    // 重跑应用器时撞上已有 id 即跳过——「人工确认不因改历史蒸发」由此兑现
    let tool_proposal_ids: std::collections::BTreeSet<String> = records
        .iter()
        .filter_map(|r| match &r.body {
            LogBody::Proposal(p) => Some(p.id.clone()),
            _ => None,
        })
        .collect();
    if !genesis {
        // 老会话补 genesis：初始黑板取当前文件里的那份（M1 没留下更早的黑板）
        let board = blackboard_of(&project(records, root, meta));
        let init = LogRecord::new(
            0,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "init".into(),
                board: board.clone(),
                scene_id: None,
                ts: store::unix_now(),
            }),
        );
        out.push(init.clone());
        event::fold(&mut proj, &init);
        // 再补跑一次 on_load：老会话的事件流里没有入席基线，重放得从「角色刚入席」重新开始
        // （M3.1：全阵容各入席一次；此刻场景尚未落地，写入路由回世界层）
        for m in &cast.members {
            let state = m.loaded.default_state.clone();
            let run = card::run_hook_full(
                &m.loaded.source,
                card::HookCall::OnLoad,
                &card::HookEnv {
                    state: state.clone(),
                    blackboard: blackboard_env(&board),
                    memory: memory_env(&proj.memory),
                    turn: 0,
                },
                meta.seed,
                &NOOP_SINK,
            );
            if let Some(body) = hook_effect(&run, &state, &m.dir, 0, "hook.on_load", true, None) {
                let rec = LogRecord::new(0, body);
                out.push(rec.clone());
                event::fold(&mut proj, &rec);
            }
        }
    }

    for rec in &kept {
        let Some(msg) = rec.as_message().cloned() else {
            // 非消息记录：重放点之前的直接折进起点；重放区内的只可能是手动事件
            if rec.turn() < replay_from || !rec.is_derived() {
                let in_replay_zone = rec.turn() >= replay_from;
                out.push(rec.clone());
                event::fold(&mut proj, rec);
                // 手动收线驱动的转移要随重放重演（M3.0 ⑦）：线事件本身是手动事件
                // （保留），但它驱动的转移是派生事件（重建丢弃）——钩子重跑不带
                // thread:<id>:resolved 触发器，编辑历史一次就会把它弄丢。
                // 这里对着刚折进来的收线事件补求值一次，与 resolve_thread_at ③ 同构；
                // 重放点之前的收线不补（它驱动的转移记录本来就在 kept 里）。
                // M3.1：全阵容各求值自己的树。
                if in_replay_zone {
                    if let LogBody::Thread(t) = &rec.body {
                        if t.op == threads::OP_RESOLVE {
                            for (i, m) in cast.members.iter().enumerate() {
                                let Some(tree) = trees[i].as_ref() else {
                                    continue;
                                };
                                for body in advance_state_tree(
                                    &proj,
                                    &m.dir,
                                    &m.loaded,
                                    tree,
                                    t.turn,
                                    &format!("{}:resolved", t.thread_id),
                                    None,
                                    meta.seed,
                                    None,
                                    None,
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
                }
            }
            continue;
        };
        if msg.turn < replay_from || msg.turn == 0 {
            // 重放点之前，或 turn 0 的开场白（它没有 on_message，设计 §3）
            out.push(rec.clone());
            event::fold(&mut proj, rec);
            continue;
        }
        // 这条消息所属的场景（M3.2）：分区已落地才有效，否则退回世界层路径。
        // 场景事件是手动事件、早于消息折进投影，所以此刻的场景表就是当时的场景表。
        let msg_scene = scoped_scene(
            &proj,
            Some(scene::normalize(msg.scene_id.as_deref())),
        );
        // ① 用户消息：on_context 在它之前跑（设计 §4.1 B5 的注入时机）——
        //    本场景在场的成员各跑一次（被切走的场景冻结，不参与轮转）
        if msg.role == "user" {
            for m in present_members(cast, &proj, msg_scene.as_deref()) {
                let (run, before) = run_context_hook(
                    &m.loaded,
                    &proj,
                    &m.dir,
                    meta.seed,
                    &proj.messages.clone(),
                    &NOOP_SINK,
                    msg_scene.as_deref(),
                    msg.turn,
                );
                if let Some(body) = hook_effect(
                    &run,
                    &before,
                    &m.dir,
                    msg.turn,
                    "hook.on_context",
                    false,
                    msg_scene.as_deref(),
                ) {
                    let rec = LogRecord::new(0, body);
                    out.push(rec.clone());
                    event::fold(&mut proj, &rec);
                }
            }
        }
        // ② 消息本身
        out.push(rec.clone());
        event::fold(&mut proj, rec);
        // ②c 模型工具调用（增强 A·决断 3）：效果是消息的衍生事件——重放按
        //    tool_calls + 同一份应用器重推导（与生成路径同一落盘序：工具 → 时钟步进
        //    → 消费 → on_message）。提案撞上已有 id 不重落（保留物化正史）；
        //    grown.json 的物化不重跑（accept 事件保留、文件幂等）
        if msg.role == "char" {
            if let Some(calls) = msg.tool_calls.as_ref().filter(|c| !c.is_empty()) {
                if let Ok(member) = cast.resolve(msg.name.as_deref()) {
                    let state = current_state(&proj, &member.dir, &member.loaded);
                    let app = apply_model_tools_core(
                        root,
                        meta,
                        &member.loaded,
                        &member.dir,
                        &cast.display_name(&member.dir),
                        msg.turn,
                        calls,
                        msg_scene.as_deref(),
                        state,
                        cast.is_multi(),
                        &tool_proposal_ids,
                    );
                    for body in app.bodies {
                        let rec = LogRecord::new(0, body);
                        event::fold(&mut proj, &rec);
                        out.push(rec);
                    }
                }
            }
        }
        // ③ 角色回复：时钟步进（设计 M1：每轮 +10 分钟，跨日进位）——推进的是
        //    这条消息所在场景的局部时钟（世界层镜像由投影折叠同步）
        if msg.role == "char" && genesis {
            let mut board = proj.effective_board(msg_scene.as_deref());
            let (day, clock) = prompt::advance_clock(board.day, &board.clock);
            board.day = day;
            board.clock = clock;
            let board = scene_board_value(&proj, msg_scene.as_deref(), board);
            let rec = LogRecord::new(
                0,
                LogBody::Blackboard(event::BlackboardEvent {
                    turn: msg.turn,
                    reason: "clock".into(),
                    board,
                    scene_id: msg_scene.clone(),
                    ts: store::unix_now(),
                }),
            );
            out.push(rec.clone());
            event::fold(&mut proj, &rec);
        }
        // ③b 主动心声消费（M3.5）：与 commit_reply_core 同序——时钟步进之后、
        //    on_message 之前；章取步进后的故事时刻（生成路径同口径）。队列到期才消费，
        //    否则是 no-op（不产生事件），重放由此保持一致
        if msg.role == "char" {
            if let Some(speaker) = cast
                .resolve(msg.name.as_deref())
                .ok()
                .map(|m| m.dir.clone())
            {
                let stepped = proj.effective_board(msg_scene.as_deref());
                for body in consume_proactive_say(
                    &proj,
                    cast,
                    &speaker,
                    msg.turn,
                    msg_scene.as_deref(),
                    stepped.day,
                    &stepped.clock,
                ) {
                    let rec = LogRecord::new(0, body);
                    out.push(rec.clone());
                    event::fold(&mut proj, &rec);
                }
            }
        }
        // ④ on_message（每条新消息落地后，设计 §3）——本场景在场的成员各跑一次。
        //    过渡插页（system）不触发钩子：它是场景的叙事接缝，不是任何人说的话
        //    （live 侧切场只落盘不跑钩子，重放这里保持同一条路径）
        if msg.role != "system" {
            for m in present_members(cast, &proj, msg_scene.as_deref()) {
                let (run, before) = run_message_hook_at(
                    &m.loaded,
                    &proj,
                    &m.dir,
                    &msg,
                    meta.seed,
                    &NOOP_SINK,
                    msg_scene.as_deref(),
                );
                if let Some(body) = hook_effect(
                    &run,
                    &before,
                    &m.dir,
                    msg.turn,
                    "hook.on_message",
                    false,
                    msg_scene.as_deref(),
                ) {
                    let rec = LogRecord::new(0, body);
                    out.push(rec.clone());
                    event::fold(&mut proj, &rec);
                }
            }
        }
        // ⑤⑥ 轮末：心理推进 + 状态树转移（与 commit_reply 同一份代码 → 重放同一条路径，
        //     设计 §7.3-5）。本场景在场的成员各自推进；重放不重推界面事件。
        if msg.role == "char" {
            let members: Vec<usize> = present_members(cast, &proj, msg_scene.as_deref())
                .iter()
                .filter_map(|m| cast.members.iter().position(|x| x.dir == m.dir))
                .collect();
            for i in members {
                let m = &cast.members[i];
                let (body, _emotion) = tick_psyche(&proj, &m.dir, &m.loaded, msg.turn);
                if let Some(body) = body {
                    let rec = LogRecord::new(0, body);
                    out.push(rec.clone());
                    event::fold(&mut proj, &rec);
                }
                let Some(tree) = trees[i].as_ref() else {
                    continue;
                };
                for body in advance_state_tree(
                    &proj,
                    &m.dir,
                    &m.loaded,
                    tree,
                    msg.turn,
                    "on_turn_end",
                    None,
                    meta.seed,
                    msg_scene.as_deref(),
                    None,
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

/// 场景感知的黑板快照（M3.2）：场景分区事件的 extra 必须只装场景 flags——
/// 折叠时它整体替换分区 flags，混入世界层实体键会把实体状态误记成场景事实。
fn scene_board_value(
    proj: &event::Projection,
    scene: Option<&str>,
    mut board: store::Blackboard,
) -> store::Blackboard {
    if let Some(flags) = scene.and_then(|id| proj.scenes.get(id)).map(|sc| sc.flags.clone()) {
        board.extra = flags;
    }
    board
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

/// 一次总结调用最多消化的消息数（批次分块；积压再大也按最老的先补，见 run_summary）。
/// 16 条：真机压测（2026-09-20，deepseek-flash）显示 40 条时推理 token 就能把
/// 8k 输出预算耗尽——批次越小，六类产物的正文越有把握写完。
const SUMMARY_CHUNK: usize = 16;
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
pub(crate) fn parse_entities(dir: &std::path::Path) -> Vec<codex::CodexEntity> {
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
    let mut fp = world_fingerprint(&dir);
    // 指纹混入 grown.json（M3.8）：确认提案后文件变了，缓存自然失效（改文件即生效，同款判据）
    let grown_file = store::grown_path(root, world);
    if let Ok(md) = std::fs::metadata(&grown_file) {
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        for byte in format!("grown:{}:{}", md.len(), mtime).bytes() {
            fp = (fp ^ byte as u64).wrapping_mul(1099511628211);
        }
    }
    if let Some(cache) = cache {
        if let Ok(map) = cache.0.lock() {
            if let Some((cached, codex)) = map.get(world) {
                if *cached == fp {
                    return codex.clone();
                }
            }
        }
    }
    // 正史增量在解析后应用（M3.8 · 设计 §6.9）：收件箱确认的提案经它进注入
    let entities = codex::apply_grown(parse_entities(&dir), &store::load_grown(root, world));
    let codex = Arc::new(codex::Codex::build(entities));
    if let Some(cache) = cache {
        if let Ok(mut map) = cache.0.lock() {
            map.insert(world.to_string(), (fp, codex.clone()));
        }
    }
    codex
}

// ---------- 语义关联层（M3.10 · 设计 §6.13：可选第六激活源）----------
//
// 纪律（m3.md 决断 7）：未配 embed 档 = 整层关闭，行为与纯确定性版一字不差；
// 检索失败也只在诊断里留一笔、当轮退回纯确定性（降级即设计，不挡说话）。
// 本模块只做两件事：把实体检索文档批量嵌入成索引（挂在设定集指纹缓存旁，
// 文件没变不重嵌），以及每轮把扫描窗口嵌入一次去索引里取候选 id。

/// 实体嵌入索引缓存（Tauri State）：世界 → (设定集指纹 + 模型名, 索引)。
/// 指纹混入模型名——换嵌入模型必须重嵌（两套向量空间不可混用）。
#[derive(Default)]
pub struct EmbedCache(Mutex<HashMap<String, (u64, Arc<semantic::SemanticIndex>)>>);

/// 嵌入批次大小：一次请求打太多文本，本地 Ollama 会顶到超时，云 API 会撞单请求上限
const EMBED_BATCH: usize = 16;

/// embed 档接入点（设计 §11 新增用途档；未配置 = 语义层整层关闭，返回 None）
fn pick_embed_provider(root: &std::path::Path) -> Result<Option<Provider>, String> {
    let providers = store::load_providers(root).map_err(|e| e.to_string())?;
    Ok(providers
        .iter()
        .find(|p| p.role == "embed")
        .filter(|p| !p.base_url.trim().is_empty() && !p.model.trim().is_empty())
        .cloned())
}

/// 取（或建）某个世界的实体嵌入索引。实体文档用 [`semantic::entity_document`]
/// 的确定性形态，canon 实体才入索引（draft/retired 反正不参与注入）。
async fn load_semantic_index(
    root: &std::path::Path,
    codex: &codex::Codex,
    cache: Option<&EmbedCache>,
    provider: &Provider,
    extra_proxy: Option<&str>,
    world: &str,
) -> Result<Arc<semantic::SemanticIndex>, String> {
    let dir = codex_entities_dir(root, world);
    let mut fp = world_fingerprint(&dir);
    for byte in provider.model.trim().to_ascii_lowercase().bytes() {
        fp = (fp ^ byte as u64).wrapping_mul(1099511628211);
    }
    // grown.json 物化的新实体也要进索引：指纹同款混入（必须在缓存查找前算全）
    let grown_file = store::grown_path(root, world);
    if let Ok(md) = std::fs::metadata(&grown_file) {
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        for byte in format!("grown:{}:{}", md.len(), mtime).bytes() {
            fp = (fp ^ byte as u64).wrapping_mul(1099511628211);
        }
    }
    if let Some(cache) = cache {
        if let Ok(map) = cache.0.lock() {
            if let Some((cached, idx)) = map.get(world) {
                if *cached == fp {
                    return Ok(idx.clone());
                }
            }
        }
    }
    let docs: Vec<String> = codex
        .entities()
        .iter()
        .filter(|e| e.is_canon())
        .map(semantic::entity_document)
        .collect();
    let ids: Vec<String> = codex
        .entities()
        .iter()
        .filter(|e| e.is_canon())
        .map(|e| e.id.clone())
        .collect();
    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(docs.len());
    for chunk in docs.chunks(EMBED_BATCH) {
        vectors.extend(llm::embeddings(provider, chunk, extra_proxy).await?);
    }
    let index = Arc::new(semantic::SemanticIndex::from_raw(ids, docs, vectors)?);
    if let Some(cache) = cache {
        if let Ok(mut map) = cache.0.lock() {
            map.insert(world.to_string(), (fp, index.clone()));
        }
    }
    Ok(index)
}

/// 语义源查询（宿主旁路）：扫描窗口嵌入一次 → 索引余弦 top-K。
/// 任何失败都降级为空候选（诊断里留原因）——语义层永远不挡说话。
/// `record`：是否把嵌入模型记进会话元数据（§6.13 版本声明）——正式发送记，
/// 预览干跑不写盘。
async fn semantic_hits_for(
    root: &std::path::Path,
    codex: &codex::Codex,
    embed_cache: Option<&EmbedCache>,
    meta: &store::SessionMeta,
    world: &str,
    query_text: &str,
    record: bool,
) -> Vec<semantic::SemanticHit> {
    let provider = match pick_embed_provider(root) {
        Ok(Some(p)) => p,
        _ => return Vec::new(),
    };
    let proxy = proxy_of(root);
    let query_text = query_text.trim();
    if query_text.is_empty() {
        return Vec::new();
    }
    let query = format!("{}{}", semantic::QUERY_INSTRUCTION, query_text);
    let index = match load_semantic_index(root, codex, embed_cache, &provider, proxy.as_deref(), world)
        .await
    {
        Ok(i) => i,
        Err(e) => {
            crate::diag::record("semantic", format!("嵌入索引构建失败（本轮降级为纯确定性）：{e}"));
            return Vec::new();
        }
    };
    let vectors = match llm::embeddings(&provider, &[query], proxy.as_deref()).await {
        Ok(v) => v,
        Err(e) => {
            crate::diag::record("semantic", format!("查询嵌入失败（本轮降级为纯确定性）：{e}"));
            return Vec::new();
        }
    };
    let Some(qv) = vectors.into_iter().next() else {
        return Vec::new();
    };
    // 版本声明（§6.13 可回放语义）：首次真正启用语义召回时把模型记进会话元数据。
    // 写失败不影响本轮（元数据只是审计锚点，注入不依赖它）。
    let stamp = format!("{}/{}", provider.name, provider.model);
    if record && meta.embed_model.as_deref() != Some(stamp.as_str()) {
        let mut updated = meta.clone();
        updated.embed_model = Some(stamp);
        if let Err(e) = store::save_session(root, &updated) {
            crate::diag::record("semantic", format!("嵌入模型版本记录失败：{e}"));
        }
    }
    index
        .query(&qv, semantic::DEFAULT_TOP_K, semantic::DEFAULT_THRESHOLD)
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

/// 语义源的查询文本（M3.10c）：扫描窗口的**消息原文**拼接（与 scan_window_text
/// 同窗口；嵌入吃原文，不带角色名前缀）。`extra` 是尚未落盘的本轮用户输入——
/// 首位发言人组装时她也还没进历史，查询里不能少了她。只拼 user/char 消息，
/// OOC 与 system 不该影响「这在说谁」。
fn semantic_query_text(history: &[Message], extra: Option<&str>) -> String {
    let start = history.len().saturating_sub(SCAN_WINDOW_MESSAGES);
    let mut parts: Vec<String> = history[start..]
        .iter()
        .filter(|m| m.role == "user" || m.role == "char")
        .map(|m| m.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(e) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(e.to_string());
    }
    parts.join("\n")
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
    // ② 卡内键值事实（api.memory.set）：**同 key 的重复写入合并成一条**（最新值），
    //    次数进 rehearsals（召回打分的「再提及增强」本来就用它，M3.0 ⑥）。
    //    不合并的话，last_thanked 这类每轮都写的键会以 0.50 显著度逐轮堆条目，
    //    把 B4 的有效容量稀释掉；读侧（memory_env）也一直是「同 key 后写覆盖」，
    //    宫殿侧对齐同一语义。value 变了照样只留最新——旧值要靠事件流回放才能看到。
    let mut fact_writes: std::collections::BTreeMap<&str, (u32, &store::MemRecord)> =
        std::collections::BTreeMap::new();
    for rec in &proj.memory {
        let entry = fact_writes.entry(rec.key.as_str()).or_insert((0, rec));
        entry.0 += 1;
        entry.1 = rec;
    }
    out.extend(
        fact_writes
            .into_values()
            .enumerate()
            .map(|(i, (writes, rec))| {
                let mut obj =
                    palace::from_legacy_fact(&rec.key, &rec.value, &rec.source, rec.turn, rec.ts);
                obj.id = palace::next_id(base + i + 1);
                obj.actors = vec![character.to_string()];
                obj.witnesses = vec![character.to_string()];
                obj.story_day = now_day;
                obj.rehearsals = writes.saturating_sub(1); // 每多写一次 = 多提一次
                obj
            }),
    );
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

/// 状态树结构缓存（Tauri State）：卡目录名 → (源码指纹, 解析出的树, shape JSON)。
/// 卡源每轮从磁盘重读（热加载的前提），所以指纹就是源码本身。
/// shape 里有 has_enter/has_exit 布尔位——状态钩子的「有没有」问题从此走缓存
///（加固 E2），不再每问一次重跑整卡。
#[derive(Default)]
pub struct TreeCache(Mutex<HashMap<String, (u64, Arc<statetree::StateTree>, serde_json::Value)>>);

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
            if let Some((cached, tree, _shape)) = map.get(&loaded.dir_name) {
                if *cached == fp {
                    return Some(tree.clone());
                }
            }
        }
    }
    let cached_fp = fp;
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
            map.insert(loaded.dir_name.clone(), (cached_fp, tree.clone(), shape));
        }
    }
    Some(tree)
}

/// 该状态有没有可执行的 on_enter/on_exit 钩子（加固 E2）：has_enter/has_exit
/// 布尔位随树一起缓存在 TreeCache；缓存未命中（重放等无缓存路径）回退全卡
/// 重跑，行为不变。
fn has_state_hook(
    loaded: &card::LoadedCard,
    cache: Option<&TreeCache>,
    state_id: &str,
    kind: &str,
) -> bool {
    if let Some(cache) = cache {
        let fp = source_fingerprint(&loaded.source);
        if let Ok(map) = cache.0.lock() {
            if let Some((cached, _, shape)) = map.get(&loaded.dir_name) {
                if *cached == fp {
                    let key = match kind {
                        "on_enter" => "has_enter",
                        "on_exit" => "has_exit",
                        _ => return card::card_has_state_hook(&loaded.source, state_id, kind),
                    };
                    return shape
                        .get("states")
                        .and_then(|s| s.get(state_id))
                        .and_then(|st| st.get(key))
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                }
            }
        }
    }
    card::card_has_state_hook(&loaded.source, state_id, kind)
}

/// 当前活跃路径：转移事件是权威（投影折叠出「最后一次转移的去向」），
/// 没有转移时用树根播种（设计 §7.3：路径同样是事件流的函数）。
fn active_path_of(
    proj: &event::Projection,
    tree: &statetree::StateTree,
    character: &str,
) -> Vec<String> {
    // M3.1：转移按角色分道（TransitionEvent.character）；缺省 = 旧会话的单角色，任何视角都认
    proj.transitions
        .iter()
        .rev()
        .find(|t| t.character.as_deref().map(|c| c == character).unwrap_or(true))
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
    scene: Option<&str>,
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
        blackboard: blackboard_env(&proj.effective_board(scene)),
        state: current_state(proj, character, loaded),
        known: proj.known_for(character),
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
    // 转移发生的场景（M3.2）：判据环境读该场景分区，reveal 见证者 = 该场景在场者
    scene: Option<&str>,
    tree_cache: Option<&TreeCache>,
) -> (Vec<LogBody>, Vec<llm::UiEmit>) {
    let mut emits: Vec<llm::UiEmit> = Vec::new();
    let path = active_path_of(proj, tree, character);
    let Some(leaf) = path.last().cloned() else {
        return (Vec::new(), emits);
    };
    let env = tree_env(proj, character, loaded, event_name, active_entities, scene);
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
        loaded, &local, character, &leaf, "on_exit", event_name, turn, seed, scene, tree_cache,
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
        character: Some(character.to_string()),
        ts: store::unix_now(),
    });
    let rec = LogRecord::new(0, transition.clone());
    out.push(transition);
    event::fold(&mut local, &rec);

    // ③ on_enter（新叶）
    let to_leaf = to_path.last().cloned().unwrap_or_default();
    let (enter_body, enter_emits) = run_state_hook(
        loaded, &local, character, &to_leaf, "on_enter", event_name, turn, seed, scene, tree_cache,
    );
    emits.extend(enter_emits);
    if let Some(body) = enter_body {
        let rec = LogRecord::new(0, body.clone());
        out.push(body);
        event::fold(&mut local, &rec);
    }

    // ④ 进入新路径即揭示（设计 §6.4：状态树的 reveal 解锁设定）。
    //    M3.1 视角化：见证者 = 黑板在场者 ∪ 转移者本人——她经历过这次揭示，
    //    不在场的角色不知道（设计 §10.4「秘密真正成为某些人知道的事」）。
    //    M3.2 场景化：在场者取转移发生场景的分区（被切走的场景不目击）。
    let mut witnesses: Vec<String> = proj.effective_board(scene).actors.clone();
    if !witnesses.iter().any(|w| w == character) {
        witnesses.push(character.to_string());
    }
    for target in tree.reveal_of(&to_path) {
        if !proj.known_for(character).contains(&target) {
            out.push(LogBody::Codex(event::CodexEvent {
                turn,
                op: "reveal".into(),
                target,
                origin: "tree".into(),
                value: None,
                note: Some(leaf.clone()),
                witnesses: witnesses.clone(),
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
    scene: Option<&str>,
    tree_cache: Option<&TreeCache>,
) -> (Option<LogBody>, Vec<llm::UiEmit>) {
    if !has_state_hook(loaded, tree_cache, state_id, kind) {
        return (None, Vec::new()); // 卡上没写这个钩子：不新建 Lua 实例
    }
    let env = tree_env(proj, character, loaded, event_name, None, scene);
    let before = current_state(proj, character, loaded);
    let run = card::run_state_hook_full(&loaded.source, state_id, kind, &env, seed, &NOOP_SINK);
    if !run.result.logs.is_empty() {
        crate::diag::record(
            "statetree",
            format!("{state_id}.{kind} 日志：{:?}", run.result.logs),
        );
    }
    let emits: Vec<llm::UiEmit> = run.result.ui_events.iter().map(ui_emit).collect();
    let body = hook_effect(
        &run,
        &before,
        character,
        turn,
        &format!("state.{kind}"),
        false,
        scene,
    );
    (body, emits)
}

// ---------- 对话生成（设计 §4 流程 + §11 流式）----------

/// 每个会话的生成中断标记
#[derive(Default)]
pub struct CancelFlags(Mutex<HashMap<String, Arc<AtomicBool>>>);

/// 每会话最近一次实际发送的组装结果（记忆检查器"本次注入"数据源）
#[derive(Default)]
pub struct LastAssemblies(Mutex<HashMap<String, prompt::PromptAssembly>>);

// ---------- 会话写入闸门（加固 A4，与 B1 同一把锁）----------
//
// messages.jsonl 是唯一事实来源，而「动事件流」有两类写者：
// - 前台命令（send_message / regenerate / edit_message / delete_message）：先读快照、
//   计算上百毫秒后整体 rewrite（消息级操作）或全程持续 append（生成）；
// - 后台总结（run_summary → apply_summary_outcome）：独立 EventLog 实例，批次
//   append 10–30 条模型产物。情景记忆/转述重放不可再生，被旧快照覆盖即永久丢失。
//
// 闸门是 per-session 的 busy 位：前台命令**入口即获取、全程持有**（含全部 await），
// 结束释放；总结落盘前 try-acquire，拿不到就整批暂存（内存队列），闸门空出后重放。
// 实现 deliberately 不用 std Mutex 守卫跨 await：busy 位是原子标记，没有可阻塞的
// 临界区，「全程持有」只是标记存活期，不违反本项目「锁不跨 await」的纪律。
// stop_generation 故意不取闸门：它只置中断位，收尾的部分落盘发生在持有者自己的闸门内。

#[derive(Default)]
pub struct WriteGate(Mutex<HashMap<String, Arc<GateSlot>>>);

struct GateSlot {
    busy: AtomicBool,
    pending: Mutex<VecDeque<ParkedSummary>>,
}

impl Default for GateSlot {
    fn default() -> Self {
        GateSlot {
            busy: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
        }
    }
}

/// 前台持有闸门的 RAII 标记：Drop 时清位。armed = false 表示没拿到（不释放）。
struct GateGuard {
    session: String,
    armed: bool,
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if self.armed {
            gate().release(&self.session);
        }
    }
}

fn gate() -> &'static WriteGate {
    static GATE: std::sync::OnceLock<WriteGate> = std::sync::OnceLock::new();
    GATE.get_or_init(WriteGate::default)
}

impl WriteGate {
    fn slot(&self, session_id: &str) -> std::sync::Arc<GateSlot> {
        let Ok(mut map) = self.0.lock() else {
            // 锁 poisoned：进程已在不一致状态，直接 panic 传播比静默串写好
            panic!("写入闸门锁 poisoned")
        };
        map.entry(session_id.to_string())
            .or_default()
            .clone()
    }

    /// 前台获取（入口独占）：已在写入中返回 None
    fn acquire(&self, session_id: &str) -> Option<GateGuard> {
        let slot = self.slot(session_id);
        if slot
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        Some(GateGuard {
            session: session_id.to_string(),
            armed: true,
        })
    }

    /// 后台尝试：同 acquire，但调用方自行决定拿不到时的退路（暂存/跳过）
    fn try_acquire(&self, session_id: &str) -> bool {
        self.acquire(session_id).is_some()
    }

    fn release(&self, session_id: &str) {
        let slot = self.slot(session_id);
        slot.busy.store(false, Ordering::Release);
    }

    fn park_summary(&self, session_id: &str, batch: ParkedSummary) {
        let slot = self.slot(session_id);
        let Ok(mut q) = slot.pending.lock() else { return };
        if q.len() >= MAX_PARKED_SUMMARIES {
            q.pop_front(); // 丢最旧
        }
        q.push_back(batch);
    }

    /// 仅 flush 循环用：持有闸门后取出暂存批次
    fn take_parked(&self, session_id: &str) -> Option<ParkedSummary> {
        let slot = self.slot(session_id);
        let Ok(mut q) = slot.pending.lock() else { return None };
        q.pop_front()
    }

    fn has_pending(&self, session_id: &str) -> bool {
        let slot = self.slot(session_id);
        slot.pending.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }
}

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
/// `scene`（M3.2）：钩子看到的是该场景的有效黑板（世界层 ∪ 场景分区）。
/// `turn`：本轮轮次（M3.5 心里话盖章用）。
#[allow(clippy::too_many_arguments)]
fn run_context_hook(
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    character: &str,
    seed: u64,
    history: &[Message],
    sink: &card::UiSink,
    scene: Option<&str>,
    turn: u64,
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
            blackboard: blackboard_env(&proj.effective_board(scene)),
            memory: memory_env(&proj.memory), // 长期记忆读侧：记忆宫殿的键值（同 key 后写覆盖）
            turn,
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
    scene: Option<&str>,
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
            blackboard: blackboard_env(&proj.effective_board(scene)),
            memory: memory_env(&proj.memory),
            turn: msg.turn,
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
    cast: &Cast,
    speaker: &str,
    history: &[Message],
    proj: &event::Projection,
    user_content: Option<&str>,
    turn: u64,
    log: Option<&store::EventLog>,
    codex_cache: Option<&CodexCache>,
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
    // 发言所在的场景（M3.2 · 设计 §10.3）：组装只看这个舞台——
    // 该场景的消息流分段、黑板分区、摘要分卷；其他场景是「与此同时」的盲区
    scene: Option<&str>,
    // 语义源候选（M3.10 · 设计 §6.13）：宿主旁路算好的嵌入召回（未配 embed 档为空切片，
    // 行为与纯确定性版一字不差）。算好再传进来——本函数保持同步，网络不进组装路径
    semantic: &[semantic::SemanticHit],
) -> Result<PromptRun, String> {
    let settings = store::load_settings(root).map_err(|e| e.to_string())?;
    let persona = match &meta.persona {
        Some(name) => store::list_personas(root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| &p.name == name),
        None => None,
    };
    // 隔离模式（设计 §10.2）：本轮发言 = 发言人独立的上下文组装。
    // 本场景在场的成员各跑 on_context（各自 state 演进、黑板写入是公开的），
    // 但注入（B5/卡片视角）只取发言人那份——其他角色的内心不进她的请求。
    let member = cast
        .get(speaker)
        .ok_or_else(|| format!("角色「{speaker}」不在这个会话的阵容里"))?;
    let loaded = &member.loaded;
    let character = member.dir.clone();
    let scene_history = scene_messages(history, scene);
    let history: &[Message] = &scene_history;

    // B5：on_context hook（降级卡与未定义该 hook 的卡都跳过；窗口给本场景最近消息）
    let mut blackboard = proj.effective_board(scene);
    let mut speaker_run: Option<(card::HookRun, serde_json::Value)> = None;
    for m in present_members(cast, proj, scene) {
        let (run, before) = run_context_hook(
            &m.loaded, proj, &m.dir, meta.seed, history, sink, scene, turn,
        );
        if run.ran() {
            // 黑板写入是公开事件（设计 §10.1：角色之间共享说出口的与做出来的）——
            // 合并视图按同一路由语义生效（分区侧由折叠的场景路由事件落盘）
            event::apply_blackboard_sets(&mut blackboard, &run.blackboard);
        }
        if let Some(log) = log {
            if let Some(body) = hook_effect(
                &run,
                &before,
                &m.dir,
                turn,
                "hook.on_context",
                false,
                scene,
            ) {
                commit(log, root, meta, body)?;
            }
        }
        if m.dir == speaker {
            speaker_run = Some((run, before));
        }
    }
    // 发言人不在本场景的在场名单（检查器预览场外成员 / 重roll 后场景变动）：
    // 她不在台上——on_context 不跑（与冻结场景同一纪律：不参与轮转的成员钩子全跳过），
    // B 层照常按她的视角组装（状态取既有 state）。此前这里是 panic——
    // 异步命令里 panic 前端 promise 永不返回（真机 M3.11 验收当场暴露）
    let (run, before) = match speaker_run {
        Some(pair) => pair,
        None => (card::HookRun::default(), current_state(proj, &character, loaded)),
    };
    let card_state = run.state.clone().unwrap_or_else(|| before.clone());

    // ---- B2 指令层：状态树活跃路径的 directive（设计 §7.4「输出约束」）----
    let tree = load_tree(loaded, tree_cache);
    let active_path = tree
        .as_ref()
        .map(|t| active_path_of(proj, t, &character))
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

    // ---- 世界主线（M3.7 · 设计 §6.6）：B1 时代行 + B2 世界段（大势压着小情绪）----
    // 世界是会话的母层：阶段 directive 拼在角色 directive 之前，era 行进 B1 现状卡。
    // 无 worldline 的世界两项全 None，注入与 M3.6 一字不差（可选层）
    let world_name = session_world(meta);
    let wl = load_worldline(root, &world_name);
    let world_state = store::load_world(root, &world_name);
    let wl_path = worldline_path_of(&wl, proj, &world_state);
    let (world_directive, era): (Option<String>, Option<String>) = match &wl {
        Some(w) if !wl_path.is_empty() => (
            {
                let d = w.tree.directive_of(&wl_path);
                (!d.trim().is_empty()).then_some(d)
            },
            worldline::era_line(&w.tree, &wl_path),
        ),
        _ => (None, None),
    };

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
    // 有效知情集按组装视角取（M3.1 · 设计 §10.4）：全局揭示 ∪ 只对她的揭示——
    // 仅 A 见过的事实不进 B 的 B3 深卡，串台在数据结构上不可能。
    // reveals 是**本轮**揭示（命中即强制深卡、权重最高），累积已知集只喂 known——
    // 否则揭示过的实体会每轮都插深卡并挤占 B3 预算。reveal 由状态树在 M2.3 写入事件流。
    let reveals: Vec<String> = Vec::new();
    let known: std::collections::BTreeSet<String> = proj.known_for(&character);
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
        viewer: &character,
        semantic_hits: semantic,
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
    let mut thread_list: Vec<threads::Thread> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .collect();
    // 世界级线并入（M3.7 · 设计 §6.6）：world.json 的存档线任何会话可见、可推进——
    // 同 id 时以本会话自己的为准（那是活的推进记录）
    for t in &world_state.threads {
        if let Ok(t) = threads::Thread::from_value(t) {
            if !proj.threads.contains_key(&t.id) {
                thread_list.push(t);
            }
        }
    }
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
    // M3.5：憋着没说出口的心里话同槽注入——「下一轮主动发消息」的说话侧
    //（发言权优先在导演投票，这里负责让她把那句话自然说出来）
    let psy = psyche::Psyche::from_state(&card_state);
    let mut psyche_line = psy.summary_line_for(&loaded.card.name);
    if let Some(text) = psy.first_scheduled() {
        let urge = format!("【心里话】你憋着一句话想说：「{text}」——这轮找机会把它自然说出口，不要生硬念稿。");
        if psyche_line.is_empty() {
            psyche_line = urge;
        } else {
            psyche_line.push('\n');
            psyche_line.push_str(&urge);
        }
    }
    let psyche_line = if psyche_line.trim().is_empty() {
        None
    } else {
        Some(psyche_line)
    };

    // A1 的隔离提示（M3.1 · 设计 §10.2）：多角色时明确「只扮演谁」——
    // 其他角色的言行只是她听到、看到的公开事件
    let cast_note = if cast.is_multi() {
        let names = cast.display_names().join("、");
        let me = display_name_of(loaded);
        Some(format!(
            "本场景有多位角色在场：{names}。你只扮演「{me}」；其余角色的言行只是你听到、看到的公开事件——不要替他们说话、思考或决定他们的反应。"
        ))
    } else {
        None
    };

    let summary_text = proj.summary_for(scene);
    // 工具旁注（增强 A1）：chat 档接入点开工具、且卡策略放行了至少一件 → 注入契约段
    //（T 层，A1 契约区尾部）。组装是读路径，这里顺手读 providers.toml（小文件）。
    let tools_contract = tools_contract_for(root, &loaded.card);
    // B2「设定·暂定」（M3.8 · 设计 §6.8-4）：本轮落流的 improv 提案回读进注入。
    // 注入内容从投影取（不重调模型）——重放/重建时同一提案事件还在，同一行还在，
    // 这正是「重放语义不随模型漂移」的兑现形式。
    let improv_lines: Vec<String> = proj
        .proposals
        .values()
        .filter_map(|v| {
            if v.get("origin").and_then(|o| o.as_str()) != Some("improv")
                || v.get("turn").and_then(|t| t.as_u64()) != Some(turn)
            {
                return None;
            }
            v.get("payload")
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        })
        .collect();
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
        world_directive: world_directive.as_deref(),
        era: era.as_deref(),
        improv_lines: &improv_lines,
        // 摘要分卷（M3.2 · 设计 §10.4）：世界层大事记 + 本场景分卷；别的场景不进这次请求
        summary: summary_text.as_deref(),
        cast_note: cast_note.as_deref(),
        tools_contract: tools_contract.as_deref(),
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

/// 工具旁注的注入判据（增强 A1）：chat 档接入点开工具 && 卡策略放行至少一件。
/// 关工具的接入点组装结果与现状逐字节一致（T 层整层省略）。
fn tools_contract_for(root: &std::path::Path, card: &card::Card) -> Option<String> {
    let provider = store::load_providers(root)
        .ok()?
        .into_iter()
        .find(|p| p.role == "chat")?;
    if !provider.tools_enabled() {
        return None;
    }
    let permitted = |name: &str| card.tools.as_ref().map(|p| p.permits(name)).unwrap_or(true);
    toolcall::TOOL_DEFS
        .iter()
        .any(|d| permitted(d.name))
        .then(|| toolcall::contract_text())
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

/// B1：生成互斥标记的 RAII 守卫——**命令入口获取，全程持有**（含回复落盘与轮末推进），
/// Drop 时从 map 摘除。生成中 stop_generation 会先摘走并置位，Drop 的摘除是 no-op。
/// 修复：标记原先在流式结束时就释放，而 commit_reply（回复落盘/时钟/钩子/状态树）
/// 在释放之后才跑——期间剧场/下一条命令能拿到标记，事件顺序错乱。
struct FlagGuard<'a> {
    flags: &'a CancelFlags,
    session: String,
    flag: Arc<AtomicBool>,
}

impl std::ops::Deref for FlagGuard<'_> {
    type Target = AtomicBool;
    fn deref(&self) -> &AtomicBool {
        &self.flag
    }
}

impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.flags.0.lock() {
            // 只摘自己的位置：stop 先摘走后这里 no-op
            if map
                .get(&self.session)
                .map(|f| Arc::ptr_eq(f, &self.flag))
                .unwrap_or(false)
            {
                map.remove(&self.session);
            }
        }
    }
}

/// 命令入口获取生成互斥标记（B1）：拿到再动事件流，拿不到直接拒绝
fn acquire_flag_guard<'a>(flags: &'a CancelFlags, session_id: &str) -> Result<FlagGuard<'a>, String> {
    let mut map = flags
        .0
        .lock()
        .map_err(|_| "内部状态锁 poisoned".to_string())?;
    if let Some(f) = map.get(session_id) {
        if !f.load(Ordering::Relaxed) {
            return Err("上一条消息还在生成中".to_string());
        }
        map.remove(session_id);
    }
    let flag = Arc::new(AtomicBool::new(false));
    map.insert(session_id.to_string(), Arc::clone(&flag));
    Ok(FlagGuard {
        flags,
        session: session_id.to_string(),
        flag,
    })
}

/// 流式生成的后半程（send_message 与 regenerate 共用）：
/// 流式补全 → 回复落盘 → 时钟步进 → on_message → 记录组装（记忆检查器）。
/// `finalize`（M3.4 群聊）：是否在这一步做轮末推进——一轮的最后一位发言人
/// 才做（心理/状态树/总结每轮一次）；中间发言人只落回复。
/// B1：中断标记由调用方在命令入口获取后传入，本函数不再自行 acquire/release。
#[allow(clippy::too_many_arguments)]
async fn stream_reply(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    speaker: &str,
    turn: u64,
    provider: &Provider,
    assembly: prompt::PromptAssembly,
    on_event: &Channel<StreamEvent>,
    // 生成互斥/中断标记（命令入口获取，生命周期覆盖到轮末推进结束）
    flag: &AtomicBool,
    log: &store::EventLog,
    assemblies: &LastAssemblies,
    // 轮末状态树求值要用：上一轮激活的实体（codex.active 判据）与状态树结构缓存
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
    // 轮末异步总结（设计 §5.3）
    summary_flags: Option<&SummaryFlags>,
    // 用户消息那一步的钩子报告（回复落盘后另有一次，会一起回给前端）
    user_report: llm::HookReport,
    // 本轮对话所在的场景（M3.2 · 设计 §10.3）：回复与轮末推进都归属这个舞台
    scene: Option<&str>,
    // 轮末推进（见上）
    finalize: bool,
) -> Result<StreamEvent, String> {
    let session_id = meta.id.as_str();
    // 流式气泡带署名（M3.4）：前端据此区分一轮里的多位发言人
    let speaker_name = cast.display_name(speaker);

    // 流式补全（取消检查在每个响应块之间）。代理跟随设置页：空则自动探测。
    let proxy = proxy_of(root);
    let chat = assembly.messages.clone();
    // 增强 A1：接入点开了工具 → 请求带 schema（卡策略只做减法），provider 不支持时
    // 自动降级去工具重试（棘轮在 llm 层）；关工具的接入点与现状完全同路。
    // C3 空正文棘轮 / C2 失败保部分文本：chat_stream_auto 内部同路生效
    let tools: Option<Vec<serde_json::Value>> = if provider.tools_enabled() {
        let policy = cast.get(speaker).and_then(|m| m.loaded.card.tools.as_ref());
        let schemas: Vec<serde_json::Value> = toolcall::TOOL_DEFS
            .iter()
            .filter(|d| policy.map(|p| p.permits(d.name)).unwrap_or(true))
            .map(toolcall::schema_of)
            .collect();
        (!schemas.is_empty()).then_some(schemas)
    } else {
        None
    };
    let stream = llm::chat_stream_auto(provider, &chat, tools.as_deref(), |delta| {
        let _ = on_event.send(StreamEvent::Delta {
            text: delta.to_string(),
            name: Some(speaker_name.clone()),
        });
    }, flag, proxy.as_deref())
    .await;

    // 记录本次组装（记忆检查器）。中断标记不在这里清——B1：它的生命周期
    // 覆盖到 commit_reply/轮末推进结束（命令收尾由 FlagGuard 摘除）
    if let Ok(mut map) = assemblies.0.lock() {
        map.insert(session_id.to_string(), assembly);
    }

    match stream {
        Ok(outcome) => {
            // 有用户消息那一步的报告打底：即使本轮没生成回复（失败/中断为空），
            // 前端也能看到卡对用户输入的反应
            let mut report = Some(user_report);
            if outcome.tools_fallback {
                // A1 降级已发生（llm 层记过 diag）：界面提示走报告日志
                if let Some(r) = report.as_mut() {
                    r.logs.push("该接入点不支持工具调用，本轮已自动降级为纯文本（可在设置页关闭此接入点的工具开关）".to_string());
                }
            }
            if !outcome.text.is_empty() {
                // 回复落定后的收尾与单测共用同一份代码：回复事件 → 工具应用 → 时钟步进 → on_message
                let committed = if finalize {
                    commit_reply(
                        root,
                        meta,
                        cast,
                        speaker,
                        turn,
                        &outcome.text,
                        &outcome.tool_calls,
                        Some(&ui_sink(app)),
                        log,
                        runtime,
                        tree_cache,
                        summary_flags,
                        scene,
                    )
                } else {
                    commit_reply_core(
                        root,
                        meta,
                        cast,
                        speaker,
                        turn,
                        &outcome.text,
                        &outcome.tool_calls,
                        Some(&ui_sink(app)),
                        log,
                        scene,
                    )
                };
                match committed {
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
        Err(failure) => {
            // C2：流中途失败不再丢弃已生成的文本——partial 非空时按取消语义落盘
            // （先报错提示，再走与「用户中断保留部分文本」相同的收尾），文字不凭空消失
            if failure.partial.is_empty() {
                return Ok(StreamEvent::Error {
                    message: failure.message,
                });
            }
            let _ = on_event.send(StreamEvent::Error {
                message: failure.message,
            });
            let user_report = Some(user_report);
            // 流中途失败：tool_calls 增量不完整（参数多半解析不出），不随部分文本应用
            let committed = if finalize {
                commit_reply(
                    root,
                    meta,
                    cast,
                    speaker,
                    turn,
                    &failure.partial,
                    &[],
                    Some(&ui_sink(app)),
                    log,
                    runtime,
                    tree_cache,
                    summary_flags,
                    scene,
                )
            } else {
                commit_reply_core(
                    root,
                    meta,
                    cast,
                    speaker,
                    turn,
                    &failure.partial,
                    &[],
                    Some(&ui_sink(app)),
                    log,
                    scene,
                )
            };
            let report = match committed {
                Ok(next) => {
                    forward_ui_events(on_event, &next);
                    Some(next)
                }
                Err(e) => {
                    return Ok(StreamEvent::Error { message: e });
                }
            };
            Ok(StreamEvent::Done {
                full: failure.partial,
                cancelled: true,
                report: report.or(user_report),
            })
        }
    }
}

/// 重roll / 重试的历史裁剪（纯函数，便于单测）：
///
/// - 末尾是角色回复 → 去掉它（重roll），以最后一条用户消息重新生成；
/// - 末尾是一条用户消息 → 上一轮生成失败（例如「请求失败」），直接**重试**这一轮；
/// - 末尾是**同一轮的多条角色回复**（M3.4 群聊一轮多人）→ 重roll 末尾那条，
///   同轮更早的回复保留为上下文（「重roll 不换人」：重生成的是被点掉的那位的回复）；
/// - 其它（空、只有开场白）→ 报错。
///
/// 返回 `(写回磁盘的消息, 组装用的历史截尾条数, 轮次, 用户输入)`：
/// `content` = Some（末尾是用户消息）时历史截掉它、输入随请求走；
/// None（重roll 群聊中后位发言人）时历史全量保留——本轮用户消息与先发言者的
/// 回复都在历史里，与该发言人当初生成时的上下文同构。
#[allow(clippy::type_complexity)]
fn plan_regenerate(messages: &[Message]) -> Result<(Vec<Message>, usize, u64, Option<String>), String> {
    let mut kept = messages.to_vec();
    if kept.last().map(|m| m.role.as_str()) == Some("char") {
        kept.pop();
    }
    // 末尾（去掉被重roll 的回复后）必须是用户消息，或全是**同一轮**的角色回复
    match kept.last().map(|m| (m.role.as_str(), m.turn)) {
        Some(("user", turn)) => {
            let content = kept.last().unwrap().content.clone();
            Ok((kept, 1, turn, Some(content)))
        }
        Some(("char", _)) => {
            let turn = kept.last().unwrap().turn;
            // 同轮 char 之后不允许再冒出别的轮次（流必须以本轮收尾）
            if kept.iter().rev().take_while(|m| m.role == "char").any(|m| m.turn != turn) {
                return Err("末尾不是本轮的回复，无法重新生成".into());
            }
            Ok((kept, 0, turn, None))
        }
        _ => Err("末尾没有可重新生成的用户消息".into()),
    }
}

/// 跑一轮 `on_message`（用户消息与回复都已落盘后调用，设计 §3）——
/// **本场景在场的成员各跑一次**（M3.1 全阵容 → M3.2 场景化：被切走的场景冻结，
/// 不参与轮转）。返回发言人的报告（卡内状态展示用），其他成员的日志并入报告、界面事件照发。
#[allow(clippy::too_many_arguments)]
fn run_message_hooks(
    app: &AppHandle,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    speaker: &str,
    turn: u64,
    on_event: Option<&Channel<StreamEvent>>,
    log: &store::EventLog,
    scene: Option<&str>,
) -> llm::HookReport {
    let sink = ui_sink(app);
    let mut speaker_report: Option<llm::HookReport> = None;
    let mut other_logs: Vec<String> = Vec::new();
    // 投影失败时退回空投影（无场景 → 全员在场），与 M3.1 行为一致
    let proj = project_session(log, root, meta).unwrap_or_default();
    for m in present_members(cast, &proj, scene) {
        let report = run_message_hook_core(root, meta, &m.loaded, &m.dir, turn, Some(&sink), log);
        if let Some(channel) = on_event {
            for event in &report.ui_events {
                let _ = channel.send(StreamEvent::HookEvent {
                    kind: event.kind.clone(),
                    value: event.value.clone(),
                    turn,
                });
            }
        }
        if m.dir == speaker {
            speaker_report = Some(report);
        } else {
            other_logs.extend(report.logs);
        }
    }
    // 发言人不在场（场景变动后重roll 等边缘）：空报告 = 钩子不跑，不 panic
    let mut report = speaker_report.unwrap_or_default();
    report.logs.extend(other_logs);
    report
}

/// `on_message` 的完整流程（与 Tauri 无关，便于单测走同一份代码）：
/// 探测 → 取最新消息 → 投影出环境 → 沙箱执行 → 事件化落盘 → 报告。
/// `sink` 为 None 时卡片推来的界面事件只进报告、不实时外推。
fn run_message_hook_core(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    character: &str,
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
    // 消息落在哪个场景，钩子就在哪个场景里跑（M3.2）：分区已落地才有效
    let msg_scene = scoped_scene(&proj, Some(scene::normalize(current.scene_id.as_deref())));

    // 钩子入参现场：把卡实际收到的 msg 与 state 原样记下来（JSON）。
    // 「条件不成立」这类静默失败，只有看到入参本身才能定死原因。
    let (run, before) = run_message_hook_at(
        loaded,
        &proj,
        character,
        &current,
        meta.seed,
        sink.unwrap_or(&NOOP_SINK),
        msg_scene.as_deref(),
    );
    crate::diag::record(
        "hook",
        format!(
            "on_message 入参：msg={} state={}",
            serde_json::to_string(&current).unwrap_or_default(),
            before
        ),
    );

    let report = apply_message_hook(
        log,
        root,
        meta,
        character,
        turn,
        &run,
        &before,
        msg_scene.as_deref(),
    );
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
#[allow(clippy::too_many_arguments)]
fn apply_message_hook(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    character: &str,
    turn: u64,
    run: &card::HookRun,
    before: &serde_json::Value,
    scene: Option<&str>,
) -> llm::HookReport {
    let mut report = llm::HookReport {
        turn,
        ran: run.ran(),
        logs: run.result.logs.clone(),
        ui_events: run.result.ui_events.iter().map(ui_emit).collect(),
        memory: run.memory.clone(),
        card_state: run.state.clone().unwrap_or_else(|| before.clone()),
    };

    // 效果归给**真正跑钩子的角色**（M3.1 隔离的题中之义）——此前误归主角色，
    // 群聊里她人的钩子写入会记错人（M3.5 群聊心里话用例当场暴露）
    if let Some(body) = hook_effect(run, before, character, turn, "hook.on_message", false, scene)
    {
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
///
/// M3.5 主动行为触发（设计 §9.2「strength ≥ 阈值且状态窗口开启 → 触发主动行为」）：
/// 「状态窗口」= 她在本场景在场（finalize_turn / 重放都只对 present_members 调用，
/// 进到本函数即窗口开启）。队列空且存在没触发过的高强度意图 → 宿主替她把心里话
/// 入队（下一轮发言权优先 + B5 注入），并在意图上记下触发记录（可溯源到面板）。
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
    if !p.has_scheduled() {
        if let Some(intent) = p.proactive_candidate() {
            let action = format!("主动想说：「{intent}」");
            p.mark_triggered(&intent, turn, &action);
            p.schedule(&intent, turn);
            // 触发记录与心里话随本轮 psyche tick 的 state 补丁一起落盘
        }
    }
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
            scene_id: None,
            ts: store::unix_now(),
        }))
    };
    (body, p.auto_emotion())
}

/// 主动心声消费（M3.5 · 设计 §9.2「意图说出口 → 意志外化为剧情线」）：
/// 她这轮真的开口了（回复落盘）→ 心里话队列清空；若存在没外化的意图，
/// 取最强的一条（强度降序、同分名字升序）开线并绑定 `linked_thread`
/// （线已存在且活跃则直接绑上去，不重复开）。事件顺序：线事件 → psyche 补丁。
///
/// 由 commit_reply_core 与 rebuild_from 共用（消费是派生效果，重放要重演同一条路径）；
/// 返回需要落盘的事件体，空 = 没有到期的心里话。`story_day`/`story_clock` = 本轮
/// 时钟步进**之后**的故事时刻（开线的章盖在上面；两处调用方各自算好传入，保证一致）。
fn consume_proactive_say(
    proj: &event::Projection,
    cast: &Cast,
    speaker: &str,
    turn: u64,
    scene: Option<&str>,
    story_day: i64,
    story_clock: &str,
) -> Vec<LogBody> {
    let Some(m) = cast.get(speaker) else {
        return Vec::new();
    };
    let state = current_state(proj, speaker, &m.loaded);
    let mut p = psyche::Psyche::from_state(&state);
    // 只消费到期的心里话（turn < 本轮）：她自己这轮刚憋下的话，下一轮才主动说
    let said = p.consume_ready(turn);
    if said.is_empty() {
        return Vec::new();
    }
    let mut bodies: Vec<LogBody> = Vec::new();

    // 外化：最强未绑线的意图 → 开线绑定（origin=psyche：说出口是历史事实，
    // 消息级重建不丢；线已存在且活跃则直接绑上去，不重复开）
    let candidate = p
        .intents
        .iter()
        .filter(|i| i.linked_thread.is_none())
        .max_by(|a, b| {
            a.strength
                .partial_cmp(&b.strength)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.name.cmp(&a.name))
        })
        .cloned();
    if let Some(intent) = candidate {
        let id = threads::id_from_title(&intent.name);
        let existing_active = proj
            .threads
            .get(&id)
            .and_then(|v| threads::Thread::from_value(v).ok())
            .map(|t| t.is_active())
            .unwrap_or(false);
        if existing_active {
            p.bind_thread(&intent.name, &id);
        } else {
            let thread = threads::Thread::open(
                &id,
                &intent.name,
                &said.join("；"),
                &[cast.display_name(speaker)],
                intent.strength,
                threads::ThreadStamp {
                    turn,
                    story_day,
                    story_clock: story_clock.to_string(),
                },
            );
            let snapshot = thread.to_value();
            if p.bind_thread(&intent.name, &id) {
                bodies.push(LogBody::Thread(event::ThreadEvent {
                    turn,
                    op: threads::OP_OPEN.into(),
                    thread_id: id,
                    thread: Some(snapshot),
                    origin: threads::ORIGIN_PSYCHE.into(),
                    note: Some("心里话说出口，意志外化为剧情线".into()),
                    ts: store::unix_now(),
                }));
            }
        }
    }

    // psyche 状态补丁（队列清空 + 可能的 linked_thread 绑定）
    let mut next = state.clone();
    p.write_into(&mut next);
    let patch = event::state_patch(&state, &next);
    if !patch.is_empty() {
        bodies.push(LogBody::Effect(event::EffectEvent {
            turn,
            trigger: "psyche.consume".into(),
            character: speaker.to_string(),
            state_set: patch,
            blackboard: Vec::new(),
            memory: Vec::new(),
            scene_id: scene.map(str::to_string),
            ts: store::unix_now(),
        }));
    }
    bodies
}

/// 回复落定后的收尾（stream_reply 与单测共用同一份代码，避免两处等价逻辑）：
/// 回复事件（带发言人署名、场景归属与**原始 tool_calls 存档**）→ 工具直写应用（增强
/// A2：origin 语义由 trigger="tool" 表达，事件顺序固定在消息之后、时钟步进之前）→
/// 时钟步进事件（发言人所在场景的局部时钟）→ 主动心声消费（M3.5，[consume_proactive_say]）
/// → on_message 事件（本场景在场成员）。
/// 只做「这一条回复」的事；轮末推进（心理/状态树/总结）在 [finalize_turn]，
/// 群聊一轮多人发言时它只跑一次。`scene` = 本轮所在场景（M3.2）。
#[allow(clippy::too_many_arguments)]
fn commit_reply_core(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    speaker: &str,
    turn: u64,
    text: &str,
    tool_calls: &[llm::ToolCall],
    sink: Option<&card::UiSink>,
    log: &store::EventLog,
    scene: Option<&str>,
) -> Result<llm::HookReport, String> {
    let reply = Message {
        turn,
        role: "char".into(),
        content: text.to_string(),
        ts: store::unix_now(),
        scene_id: scene.map(str::to_string),
        name: Some(cast.display_name(speaker)),
        // 原始调用存档（决断 3）：效果由重放按 tool_calls 重推导
        tool_calls: (!tool_calls.is_empty()).then(|| tool_calls.to_vec()),
    };
    log.append(root, &meta.id, LogBody::Message(reply))
        .map_err(|e| format!("回复落盘失败：{e}"))?;

    // 工具段的产出先落地（最后并入总报告——发言人不在场时也要回给前端）
    let mut tool_logs: Vec<String> = Vec::new();
    let mut tool_ui: Vec<llm::UiEmit> = Vec::new();
    // 消息已落盘的投影（工具应用与后续时钟步进同用一份现场；
    // 工具效果折进这份投影——工具改的黑板/心理对同一轮的后续求值立即可见）
    let mut proj = project_session(log, root, meta)?;

    // 工具直写/提案应用（A2 落盘序：消息事件 → 工具效果 → 时钟步进 → 消费 → on_message）
    if !tool_calls.is_empty() {
        if let Some(member) = cast.get(speaker) {
            let state = current_state(&proj, speaker, &member.loaded);
            let existing = std::collections::BTreeSet::new();
            let app = apply_model_tools_core(
                root,
                meta,
                &member.loaded,
                speaker,
                &cast.display_name(speaker),
                turn,
                tool_calls,
                scene,
                state,
                cast.is_multi(),
                &existing,
            );
            for body in &app.bodies {
                // 工具效果折进现场投影（make_mut：无共享时零拷贝；本分支只在带工具的
                // 回复进入——无工具轮沿用缓存 Arc，不多拷一份）
                let rec = LogRecord::new(0, body.clone());
                event::fold(std::sync::Arc::make_mut(&mut proj), &rec);
            }
            if !app.bodies.is_empty() {
                if let Err(e) = commit_batch(log, root, meta, app.bodies.clone()) {
                    tool_logs.push(e);
                }
            }
            // 小事实自动接受（生成路径）：物化与手动确认同一条路
            if !app.auto_accepts.is_empty() {
                let world = session_world(meta);
                for (kind, payload) in &app.auto_accepts {
                    materialize_accepted(root, &world, kind, payload.clone());
                }
            }
            // 界面事件（ui_emit）转推前端；verdict 进日志（检查器/面板可见）
            tool_ui.extend(app.ui_events);
            for v in &app.verdicts {
                tool_logs.push(format!("工具 {}〔{}〕{}", v.name, v.verdict, v.reason));
            }
        }
    }

    // 一轮完成：黑板时钟步进（设计 M1：每轮 +10 分钟，跨日进位）。
    // 多场景会话推进的是**本场景的局部时钟**（场景分区快照 + 世界层镜像由折叠同步）；
    // 单场景/老会话照旧是世界层事件（scene_id = None）。
    let scene_id = scoped_scene(&proj, scene);
    let mut board = proj.effective_board(scene);
    let (day, clock) = prompt::advance_clock(board.day, &board.clock);
    board.day = day;
    board.clock = clock;
    let board = scene_board_value(&proj, scene_id.as_deref(), board);
    let (stepped_day, stepped_clock) = (board.day, board.clock.clone());
    // 时钟步进 + 主动心声消费一批落盘（加固 D1-1）：事件顺序不变，落盘收敛为一次投影
    let mut bodies = vec![LogBody::Blackboard(event::BlackboardEvent {
        turn,
        reason: "clock".into(),
        board,
        scene_id: scene_id.clone(),
        ts: store::unix_now(),
    })];
    // 主动心声消费（M3.5）：她这轮真的开口了 → 心里话清空 + 意图外化开线。
    // 章盖在步进后的时钟上（与 rebuild 的重放路径同口径）
    bodies.extend(consume_proactive_say(
        &proj,
        cast,
        speaker,
        turn,
        scene,
        stepped_day,
        &stepped_clock,
    ));
    commit_batch(log, root, meta, bodies)?;

    // 回复落盘后跑 on_message（设计 §3：每条新消息落地后，在场成员各跑一次）
    let mut report: Option<llm::HookReport> = None;
    let mut other_logs: Vec<String> = Vec::new();
    for m in present_members(cast, &proj, scene_id.as_deref()) {
        let r = run_message_hook_core(root, meta, &m.loaded, &m.dir, turn, sink, log);
        if m.dir == speaker {
            report = Some(r);
        } else {
            other_logs.extend(r.logs);
        }
    }
    // 发言人不在场（场景变动后的边缘）：空报告 = 钩子不跑，不 panic；
    // 工具段的日志/界面事件无论钩子跑没跑都要回给前端
    let mut report = report.unwrap_or_default();
    report.logs.extend(other_logs);
    report.logs.extend(std::mem::take(&mut tool_logs));
    report.ui_events.extend(std::mem::take(&mut tool_ui));
    Ok(report)
}

/// 轮末推进（每轮**一次**，与回复条数无关）：心理 tick → 状态树轮末求值 → 异步总结。
/// 群聊一轮多人发言（M3.4）：只在最后一位发言人之后调用——
/// 「默认转移推迟到轮末」的轮是用户的一轮，不是某条回复。
#[allow(clippy::too_many_arguments)]
fn finalize_turn(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    turn: u64,
    log: &store::EventLog,
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
    summary_flags: Option<&SummaryFlags>,
    scene: Option<&str>,
    report: &mut llm::HookReport,
) -> Result<(), String> {
    // 轮末：心理运行时推进（情绪衰减/意图慢衰减）——在场成员各自推进，结果同样是事件。
    // 加固 D1-1：全成员的 psyche 补丁一批落盘（一次投影 + 一次派生同步）
    if let Ok(proj) = project_session(log, root, meta) {
        let mut bodies: Vec<LogBody> = Vec::new();
        for m in present_members(cast, &proj, scene) {
            let (body, emotion) = tick_psyche(&proj, &m.dir, &m.loaded, turn);
            if let Some(body) = body {
                bodies.push(body);
            }
            if let Some(emotion) = emotion {
                report.ui_events.push(llm::UiEmit {
                    kind: "emotion".into(),
                    value: emotion,
                });
            }
        }
        if !bodies.is_empty() {
            if let Err(e) = commit_batch(log, root, meta, bodies) {
                report.logs.push(e);
            }
        }
    }

    // 轮末：状态树转移求值（设计 §7.3-2「默认转移推迟到轮末」，保证一轮对话内状态稳定）。
    // M3.1：每个角色各求值自己的树（转移事件带 character，路径按角色分道）；
    // M3.2：判据环境与揭示见证者取本场景分区。
    let active_entities = runtime.map(|r| r.previously_active(&meta.id));
    if let Ok(proj) = project_session(log, root, meta) {
        let mut tree_bodies: Vec<LogBody> = Vec::new();
        for m in present_members(cast, &proj, scene) {
            let Some(tree) = load_tree(&m.loaded, tree_cache) else {
                continue;
            };
            let (events, emits) = advance_state_tree(
                &proj,
                &m.dir,
                &m.loaded,
                &tree,
                turn,
                "on_turn_end",
                active_entities.as_ref(),
                meta.seed,
                scene,
                tree_cache,
            );
            // 转移的界面事件（on_enter 的 ui.emit）与本轮钩子事件一起回给前端
            report.ui_events.extend(emits);
            // D1-1：各成员的转移事件先收拢，全成员一批落盘（判据环境取的是同一份
            // proj，与原逐条提交的判据输入一致；事件顺序 = 成员序 × 转移序，不变）
            tree_bodies.extend(events);
        }
        if !tree_bodies.is_empty() {
            if let Err(e) = commit_batch(log, root, meta, tree_bodies) {
                report.logs.push(e);
            }
        }
    }

    // 轮末：世界主线推进（M3.7 · 设计 §6.6）——阶段转移落 worldline 事件（重建保留），
    // 世界层动作（reveal / 开世界级线）逐条执行。排在角色状态树之后、剧场之前：
    // 导演树的判据可以查 `st.worldline_stage`（母层查询子层，得先走完这一步）
    if let Err(e) = advance_worldline(root, meta, log, report) {
        report.logs.push(format!("世界主线推进失败：{e}"));
    }

    // 轮末：剧场模式（M3.6）——导演树起承转合推进 + 交叉剪辑切场。
    // 只在剧场开着时进；全部动作落事件流（与重放同路径，可回放）
    if meta.theater.is_some() {
        if let Err(e) = advance_theater(root, meta, cast, log, tree_cache, runtime, report) {
            report.logs.push(format!("剧场推进失败：{e}"));
        }
    }

    // 轮末：世界回写（M3.7）——world.json 取 max(世界, 本会话)，多线并行不回退、
    // flashback 不拉低。在轮末连续做而非等「会话结束」：max 单调，语义等价且崩溃不丢
    if let Err(e) = sync_world_now(root, meta, log) {
        report.logs.push(format!("世界回写失败：{e}"));
    }

    // 轮末异步总结（消息已滑出 L0 窗口时才真的干活；不阻塞本轮返回）。
    // 当轮激活记录一起带走：关联审计要拿它对照「剧情涉及了谁、激活了谁」（M3.10）
    if let Some(flags) = summary_flags {
        let active = runtime
            .map(|r| {
                r.previously_active(&meta.id)
                    .into_iter()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        spawn_summary(root, &meta.id, flags, active);
    }
    Ok(())
}

/// 一条角色回复的完整收尾 = 回复级落盘 + 轮末推进（单发言人路径；语义与拆分前一致）。
///
/// 事件化后时钟步进也是事件：重放同一轮必然得到同一时刻（设计 §7.3-5）。
#[allow(clippy::too_many_arguments)]
fn commit_reply(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    speaker: &str,
    turn: u64,
    text: &str,
    tool_calls: &[llm::ToolCall],
    sink: Option<&card::UiSink>,
    log: &store::EventLog,
    runtime: Option<&SessionRuntime>,
    tree_cache: Option<&TreeCache>,
    summary_flags: Option<&SummaryFlags>,
    scene: Option<&str>,
) -> Result<llm::HookReport, String> {
    let mut report = commit_reply_core(
        root, meta, cast, speaker, turn, text, tool_calls, sink, log, scene,
    )?;
    finalize_turn(
        root,
        meta,
        cast,
        turn,
        log,
        runtime,
        tree_cache,
        summary_flags,
        scene,
        &mut report,
    )?;
    Ok(report)
}

/// 工具调用的应用核心（生成与重放共用，增强 A2/A5）：构建现场 → 应用器产出事件。
/// `existing` = 已在流里的提案 id 集（重放传流内集合实现幂等；生成传空集）。
#[allow(clippy::too_many_arguments)]
fn apply_model_tools_core(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    loaded: &card::LoadedCard,
    speaker: &str,
    display_name: &str,
    turn: u64,
    tool_calls: &[llm::ToolCall],
    scene: Option<&str>,
    state: serde_json::Value,
    multi_cast: bool,
    existing: &std::collections::BTreeSet<String>,
) -> toolcall::ToolApplication {
    let settings = store::load_settings(root).unwrap_or_default();
    let policy = loaded.card.tools.clone().unwrap_or_default();
    let cx = load_codex(root, None, &session_world(meta));
    let limit = settings
        .tool_calls_per_turn
        .filter(|n| *n > 0)
        .unwrap_or(toolcall::DEFAULT_PER_TURN);
    crate::diag::record(
        "tools",
        format!(
            "第 {turn} 轮 {speaker}：{} 次工具调用（上限 {limit}，卡策略 {}）",
            tool_calls.len(),
            if loaded.card.tools.is_some() { "已声明" } else { "缺省全放行" },
        ),
    );
    let ctx = toolcall::ToolCtx {
        character: speaker,
        display_name,
        turn,
        scene,
        state,
        multi_cast,
        policy: &policy,
        limit,
        cx: &cx,
        auto_minor: settings.auto_accept_minor_facts,
        existing_proposals: existing,
        ts: store::unix_now(),
    };
    toolcall::apply_tool_calls(tool_calls, &ctx)
}

/// 把钩子推来的界面事件转推前端（用户消息一步与回复一步共用）
fn forward_ui_events(channel: &Channel<StreamEvent>, report: &llm::HookReport) {
    for event in &report.ui_events {
        let _ = channel.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
            turn: report.turn,
        });
    }
}

/// 每轮发言数的缺省值（M3.4：每轮发言数可配，默认 1–2；导演调度本就天然限流，
/// 对冲隔离模式的请求成本）。会话未配置（None）时取它。
pub const DEFAULT_MAX_SPEAKERS: usize = 2;

// ---------- 设定史变的解析预览（M3.7 · 设计 §6.5）----------

/// 解析预览：按故事时钟回答「第 N 天，这个世界是什么样」。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvePreview {
    pub day: i64,
    pub entities: Vec<ResolvePreviewEntity>,
}

/// 单个实体在指定故事天的解析切片。
/// versions 追加不覆盖、组装按天解析（facet_at）——这里是同一份解析的**人类视图**：
/// 生效版本逐条列出（哪条在第 N 天说了算）、生命周期按生效时刻判定、
/// retired 留档照常可查（死亡是正史变更，不是删除）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvePreviewEntity {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// canon | draft | retired
    pub status: String,
    /// 生命周期在第 N 天的判定：status（active/departed/dead）、at_day 生效时刻、
    /// in_effect（已生效）、present（此刻是否算在场——flashback 回到生效前仍在场）
    pub lifecycle: serde_json::Value,
    /// 史变版本流：from_day 起生效、在第 N 天是否生效
    pub versions: Vec<serde_json::Value>,
    /// 按 day 解析出的一句话简介（versions/variants 覆盖后的「那一天的事实」）
    pub one_liner: String,
}

/// 解析预览（day 缺省 = 会话当前故事天）
#[tauri::command]
pub fn codex_resolve_preview(
    session_id: String,
    day: Option<i64>,
    log: State<'_, store::EventLog>,
    codex_cache: State<'_, CodexCache>,
) -> Result<ResolvePreview, String> {
    codex_resolve_preview_of(&log, &codex_cache, &root(), &session_id, day)
}

fn codex_resolve_preview_of(
    log: &store::EventLog,
    codex_cache: &CodexCache,
    root: &std::path::Path,
    session_id: &str,
    day: Option<i64>,
) -> Result<ResolvePreview, String> {
    let meta = store::load_session(root, session_id).map_err(|e| e.to_string())?;
    let world = session_world(&meta);
    let cx = load_codex(root, Some(codex_cache), &world);
    let proj = project_session(log, root, &meta)?;
    let board = proj.effective_board(None);
    let day = day.unwrap_or(board.day);
    let bb = blackboard_env(&board);
    let mut entities: Vec<ResolvePreviewEntity> = cx
        .entities()
        .iter()
        .map(|e| {
            let one_liner = e
                .facet_at("one_liner", day, &board.clock, &bb)
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| e.one_liner.clone());
            let lifecycle = match &e.lifecycle {
                Some(lc) => serde_json::json!({
                    "status": lc.status,
                    "at_day": lc.at_day,
                    "in_effect": lc.in_effect_at(day),
                    "present": lc.present_at(day),
                    "note": lc.note,
                }),
                None => serde_json::json!({ "status": codex::Lifecycle::ACTIVE, "present": true }),
            };
            let versions = e
                .versions
                .iter()
                .map(|v| {
                    serde_json::json!({
                        "from_day": v.from_day,
                        "facet": v.facet,
                        "value": v.value,
                        "note": v.note,
                        "active": v.from_day <= day,
                    })
                })
                .collect();
            ResolvePreviewEntity {
                id: e.id.clone(),
                name: e.name.clone(),
                ty: e.ty.clone(),
                status: e.status.clone(),
                lifecycle,
                versions,
                one_liner,
            }
        })
        .collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(ResolvePreview { day, entities })
}

/// 发送一条用户消息并流式生成回复（加固 A4/B1 包装）：
/// 入口获取会话写入闸门并**全程持有**——并发的第二条消息在动事件流之前就被拒掉；
/// 闸门清位后顺手重放暂存的总结批次。主体见 [`send_message_inner`]。
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    session_id: String,
    content: String,
    speaker: Option<String>,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    log: State<'_, store::EventLog>,
    assemblies: State<'_, LastAssemblies>,
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
    summary_flags: State<'_, SummaryFlags>,
    embed_cache: State<'_, EmbedCache>,
) -> Result<StreamEvent, String> {
    let guard = gate()
        .acquire(&session_id)
        .ok_or_else(|| "上一条消息还在生成中".to_string())?;
    let result = send_message_inner(
        app,
        session_id.clone(),
        content,
        speaker,
        on_event,
        flags,
        log,
        assemblies,
        codex_cache,
        runtime,
        tree_cache,
        summary_flags,
        embed_cache,
    )
    .await;
    drop(guard);
    flush_parked_summaries(&root(), &session_id);
    result
}

/// 流事件经 `on_event` 通道推给前端（delta / done / error），
/// 返回值即终态事件。用户消息先落盘；回复（含中断时的部分文本）生成后落盘。
///
/// 发言权（M3.4 · 设计 §10.5）：
/// - 显式 `speaker` = **点名直通**（只他一人接话，语义与 M3.1 一致）；
/// - `None` 且阵容多人 = **导演调度**——发言权打分选出 1–N 位发言人按序接话
///   （后发言者听到先发言者刚说的话），调度落 `director` 事件（回放可重现）；
/// - `None` 且单角色 = 主角色（1v1 退化，不走导演，行为与 M2 一致）。
///
/// 事件顺序（与重放顺序一致，见 rebuild_from）：
/// director 事件 → on_context 事件 → 用户消息事件 → on_message 事件 → 回复事件 → 时钟步进事件。
#[allow(clippy::too_many_arguments)]
async fn send_message_inner(
    app: AppHandle,
    session_id: String,
    content: String,
    speaker: Option<String>,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    log: State<'_, store::EventLog>,
    assemblies: State<'_, LastAssemblies>,
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
    summary_flags: State<'_, SummaryFlags>,
    embed_cache: State<'_, EmbedCache>,
) -> Result<StreamEvent, String> {
    // B1：入口拿生成互斥标记，全程持有（含回复落盘与轮末推进）——并发生成在
    // 动事件流之前就被拒，不会再留下「无回复的用户消息 + 好感度多算一次」
    let flag = acquire_flag_guard(&flags, &session_id)?;
    let root = root();

    // 会话与角色阵容（M3.1 隔离模式：每轮发言 = 发言人独立的上下文组装与请求）
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let cast = Cast::load(&root, &meta)?;

    // 接入点（chat 档；先校验再落盘用户消息，配置错误不产生半截会话）
    let provider = pick_chat_provider(&root)?;

    let mut proj = project_session(&log, &root, &meta)?;
    let scene = scene_ctx(&proj);

    // 发言人解析（见函数头注释）
    let explicit = match speaker.as_deref() {
        Some(s) if !s.trim().is_empty() => Some(cast.resolve(Some(s))?.dir.clone()),
        _ => None,
    };
    let scheduled = explicit.is_none() && cast.is_multi();
    let picks: Vec<director::Pick> = if scheduled {
        director_plan(&meta, &cast, &proj, scene.as_deref(), &content)?
    } else {
        let dir = explicit.clone().unwrap_or_else(|| cast.first().dir.clone());
        vec![director::Pick {
            dir: dir.clone(),
            name: cast.display_name(&dir),
            score: 0.0,
            reasons: Vec::new(),
        }]
    };
    let first_hooks = picks
        .first()
        .and_then(|p| cast.get(&p.dir))
        .map(|m| m.loaded.hook_names.clone())
        .unwrap_or_default();

    crate::diag::record(
        "chat",
        format!(
            "send_message 会话={} 发言人={:?} 阵容={:?} 调度={} 钩子={:?}（{}）",
            session_id,
            picks.iter().map(|p| p.dir.as_str()).collect::<Vec<_>>(),
            meta.characters,
            if scheduled { "导演" } else { "点名/主角色" },
            first_hooks,
            root.display(),
        ),
    );

    // 发言人必须在本场景（导演计划只在在场者里挑；点名要显式检查——
    // 被切走的场景冻结，不在场的人点不动）
    if let Some(id) = &scene {
        for p in &picks {
            let present = proj.scenes.get(id).map(|sc| sc.has_actor(&p.dir));
            if present == Some(false) {
                return Err(format!(
                    "「{}」不在当前场景——切到他所在的场景再说话",
                    cast.display_name(&p.dir)
                ));
            }
        }
    }

    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0) + 1;

    // 调度落流（只有导演真正拍板的轮才记——点名直通本身就是消息署名可见的动作），
    // 同时给前端一条调度指示：第一个字出现前，「谁在说话、为何轮到她」就有答案
    if scheduled {
        log.append(
            &root,
            &session_id,
            LogBody::Director(event::DirectorEvent {
                turn,
                op: "schedule".into(),
                picks: picks
                    .iter()
                    .map(|p| event::DirectorPick {
                        dir: p.dir.clone(),
                        name: p.name.clone(),
                        score: p.score,
                        reasons: p.reasons.clone(),
                    })
                    .collect(),
                direct: None,
                note: None,
                ts: store::unix_now(),
            }),
        )
        .map_err(|e| e.to_string())?;
        let _ = on_event.send(StreamEvent::Director {
            names: picks.iter().map(|p| p.name.clone()).collect(),
            brief: picks
                .iter()
                .map(|p| {
                    if p.reasons.is_empty() {
                        p.name.clone()
                    } else {
                        format!("{}（{}）", p.name, p.reasons.join(" · "))
                    }
                })
                .collect::<Vec<_>>()
                .join("、"),
        });
    }

    let first = &picks[0];
    // 语义源候选（M3.10 · 设计 §6.13，可选）：本轮算一次、全轮共用——查询取扫描
    // 窗口原文 + 尚未落盘的本轮输入；未配 embed 档或失败时空候选（纯确定性路径）。
    // 群聊后位发言人的重组装沿用同一批候选：语义只是弱信号，不为它多打一次向量
    let world_name = session_world(&meta);
    let world_codex = load_codex(&root, Some(&codex_cache), &world_name);
    let scene_history = scene_messages(&proj.messages, scene.as_deref());
    let semantic_query = semantic_query_text(&scene_history, Some(&content));
    let semantic_hits = semantic_hits_for(
        &root,
        &world_codex,
        Some(&embed_cache),
        &meta,
        &world_name,
        &semantic_query,
        true,
    )
    .await;
    if !semantic_hits.is_empty() {
        crate::diag::record(
            "semantic",
            format!(
                "语义候选：{}",
                semantic_hits
                    .iter()
                    .map(|h| format!("{}({:.2})", h.id, h.score))
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        );
    }
    // 即兴模式（M3.8 · 设计 §6.8-4，默认关）：本轮被提及的实体过薄时，便宜模型
    // 现场补一条「设定·暂定」——提案先落流（origin=improv），组装时经投影回读进 B2。
    // 失败静默跳过：即兴是锦上添花，永远不能挡住说话。
    if meta.improv {
        match maybe_improv(
            &root,
            &meta,
            &codex_cache,
            &content,
            &proj,
            scene.as_deref(),
            turn,
            &log,
        )
        .await
        {
            Ok(Some(id)) => {
                proj = project_session(&log, &root, &meta)?;
                crate::diag::record("improv", format!("即兴补设定：{id}"));
            }
            Ok(None) => {}
            Err(e) => crate::diag::record("improv", format!("即兴补设定失败（跳过）：{e}")),
        }
    }
    // on_context 在此运行并事件化落盘：卡片可能顺手改了 state/黑板/界面事件。
    // （第一位发言人的组装——此时用户消息尚未落盘，随 content 单独进上下文）
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &cast,
        &first.dir,
        &proj.messages,
        &proj,
        Some(&content),
        turn,
        Some(&log),
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
        scene.as_deref(),
        &semantic_hits,
    )?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
            turn,
        });
    }

    // 用户消息落盘后进入流式请求（归属当前场景；无场景会话照旧 None）
    let user_msg = Message {
        turn,
        role: "user".into(),
        content: content.clone(),
        ts: store::unix_now(),
        scene_id: scene.clone(),
        name: None,
        tool_calls: None,
    };
    log.append(&root, &session_id, LogBody::Message(user_msg))
        .map_err(|e| e.to_string())?;

    // 设计 §3：`on_message` 在**每条**新消息落地后调用——用户消息同样要跑，
    // 否则卡看不到本轮输入，且它的反应（比如好感度 +1）来不及影响这一轮的生成。
    // （每轮一次，报告归属第一位发言人；其他成员的日志并入报告）
    let report = run_message_hooks(
        &app,
        &root,
        &meta,
        &cast,
        &first.dir,
        turn,
        Some(&on_event),
        &log,
        scene.as_deref(),
    );

    // 逐位发言（顺序，不并行——后发言者的组装要含先发言者刚落盘的回复，
    // 「后发言者自然听到先发言者刚说的话」，设计 §10.2）
    let mut assembly = Some(run.assembly);
    let mut user_report = Some(report);
    let mut last: Result<StreamEvent, String> = Err("没有可发言的角色".into());
    for (i, pick) in picks.iter().enumerate() {
        let is_last = i + 1 == picks.len();
        let asm = match assembly.take() {
            Some(a) => a,
            None => {
                // 重投影：上一位的回复（与更早的消息）已进历史，这位发言人按自己的视角组装
                let proj = project_session(&log, &root, &meta)?;
                assemble_prompt_core(
                    &ui_sink(&app),
                    &root,
                    &meta,
                    &cast,
                    &pick.dir,
                    &proj.messages,
                    &proj,
                    None,
                    turn,
                    Some(&log),
                    Some(&codex_cache),
                    Some(&runtime),
                    Some(&tree_cache),
                    scene.as_deref(),
                    &semantic_hits,
                )?
                .assembly
            }
        };
        let report = user_report.take().unwrap_or_else(|| llm::HookReport {
            turn,
            ..Default::default()
        });
        let res = stream_reply(
            &app,
            &root,
            &meta,
            &cast,
            &pick.dir,
            turn,
            &provider,
            asm,
            &on_event,
            &flag,
            &log,
            &assemblies,
            Some(&runtime),
            Some(&tree_cache),
            Some(&summary_flags),
            report,
            scene.as_deref(),
            is_last,
        )
        .await;
        let stop = matches!(
            &res,
            Ok(StreamEvent::Error { .. }) | Ok(StreamEvent::Done { cancelled: true, .. })
        );
        last = res;
        if stop {
            break; // 出错或被中断：保留已说出的部分，不再往下排
        }
    }
    last
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

/// 重roll（设计 §4 消息级操作，加固 A4/B1 包装）：入口获取会话写入闸门并全程持有，
/// 生成中被拒；主体见 [`regenerate_inner`]。
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
    summary_flags: State<'_, SummaryFlags>,
    embed_cache: State<'_, EmbedCache>,
) -> Result<StreamEvent, String> {
    let guard = gate()
        .acquire(&session_id)
        .ok_or_else(|| "上一条消息还在生成中".to_string())?;
    let result = regenerate_inner(
        app,
        session_id.clone(),
        on_event,
        flags,
        log,
        assemblies,
        codex_cache,
        runtime,
        tree_cache,
        summary_flags,
        embed_cache,
    )
    .await;
    drop(guard);
    flush_parked_summaries(&root(), &session_id);
    result
}

/// 重roll 主体：移除末尾角色回复，以最后一条用户消息
/// 重新流式生成。先删后生成——失败也不会出现两条并列回复。
/// 发言人取被重roll 回复的署名（多角色时不换人重roll；1v1 无署名 = 主角色）。
#[allow(clippy::too_many_arguments)]
async fn regenerate_inner(
    app: AppHandle,
    session_id: String,
    on_event: Channel<StreamEvent>,
    flags: State<'_, CancelFlags>,
    log: State<'_, store::EventLog>,
    assemblies: State<'_, LastAssemblies>,
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
    summary_flags: State<'_, SummaryFlags>,
    embed_cache: State<'_, EmbedCache>,
) -> Result<StreamEvent, String> {
    // B1：入口拿生成互斥标记，全程持有——生成中点重roll 在 truncate/rewrite 之前就被拒
    let flag = acquire_flag_guard(&flags, &session_id)?;
    let root = root();

    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let cast = Cast::load(&root, &meta)?;
    let provider = pick_chat_provider(&root)?;

    let records = log.read(&root, &session_id).map_err(|e| e.to_string())?;
    let all = event::messages(&records);
    // 被重roll 的回复是谁说的：末尾 char 消息的署名（缺省主角色）
    let rerolled_speaker = all
        .iter()
        .rev()
        .find(|m| m.role == "char")
        .and_then(|m| m.name.clone());
    let member = cast.resolve(rerolled_speaker.as_deref())?;
    let speaker = member.dir.clone();
    let (rewritten, trim, turn, content) = match plan_regenerate(&all) {
        Ok(plan) => plan,
        Err(message) => return Ok(StreamEvent::Error { message }),
    };

    // 截断本轮：丢掉 turn 起的**派生事件**（上一次的 on_context / on_message / 时钟步进），
    // 并按 plan 移除末尾回复。它们随后由正常流程重新产生——**「重roll 不重复计分」
    // 由此从启发式判据变成结构性保证**：旧效果已经不在流里了。
    // 手动事件（手改黑板、手动开收线）与导演调度事件不是派生结果，照旧保留。
    let kept = truncate_turn(&records, turn, rewritten.len() < all.len());
    log.rewrite(&root, &session_id, &kept)
        .map_err(|e| e.to_string())?;
    sync_now(&log, &root, &meta)?;

    // 重roll 前先让 on_context 按当前（已删掉末尾回复的）历史跑一轮。
    // 场景归属跟随被重roll 的那条用户消息（回复与它同场，M3.2）
    let proj = project_session(&log, &root, &meta)?;
    let scene = all
        .iter()
        .rev()
        .find(|m| m.role == "user" && m.turn == turn)
        .and_then(|m| scoped_scene(&proj, m.scene_id.as_deref()));
    // content = Some：历史截掉本轮用户消息（重roll 首位发言人 / 重试失败轮）；
    // content = None：本轮用户消息与先发言者的回复都留在历史里（重roll 群聊的后位发言人）
    let history = &proj.messages[..proj.messages.len().saturating_sub(trim)]; // 组装历史
    // 语义源候选（M3.10 · 设计 §6.13）：与 send_message 同一入口同一口径——
    // 查询取本场景窗口原文 +（首位发言人时）本轮用户输入
    let world_name = session_world(&meta);
    let world_codex = load_codex(&root, Some(&codex_cache), &world_name);
    let scene_history = scene_messages(history, scene.as_deref());
    let semantic_query = semantic_query_text(&scene_history, content.as_deref());
    let semantic_hits = semantic_hits_for(
        &root,
        &world_codex,
        Some(&embed_cache),
        &meta,
        &world_name,
        &semantic_query,
        true,
    )
    .await;
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &cast,
        &speaker,
        history,
        &proj,
        content.as_deref(),
        turn,
        Some(&log),
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
        scene.as_deref(),
        &semantic_hits,
    )?;
    for event in &run.ui_events {
        let _ = on_event.send(StreamEvent::HookEvent {
            kind: event.kind.clone(),
            value: event.value.clone(),
            turn,
        });
    }
    // 上一轮的 on_message 效果已随截断消失，这里补跑：**恰好一次**，不是重复计分
    let report = run_message_hooks(
        &app,
        &root,
        &meta,
        &cast,
        &speaker,
        turn,
        Some(&on_event),
        &log,
        scene.as_deref(),
    );
    stream_reply(
        &app,
        &root,
        &meta,
        &cast,
        &speaker,
        turn,
        &provider,
        run.assembly,
        &on_event,
        &flag,
        &log,
        &assemblies,
        Some(&runtime),
        Some(&tree_cache),
        Some(&summary_flags),
        report,
        scene.as_deref(),
        true,
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
pub fn get_blackboard(
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<store::Blackboard, String> {
    let ctx = session_ctx(&log, &session_id)?;
    // 返回聚焦场景的有效黑板（世界层 ∪ 场景分区）——界面编辑的就是这个视图
    Ok(ctx
        .proj
        .effective_board(scene_ctx(&ctx.proj).as_deref()))
}

/// 手动编辑黑板（全量替换；保存后下一轮组装生效）。
/// 手改进事件流（reason=manual）——它不是派生结果，重放历史时不会被抹掉。
///
/// 场景路由（M3.2）：时间是世界层的；地点/在场者是**聚焦场景分区**的
/// （多场景会话改的是当前舞台），折叠时世界层镜像自动同步。
#[tauri::command]
pub fn update_blackboard(
    session_id: String,
    day: i64,
    clock: String,
    place: String,
    actors: Vec<String>,
    log: State<'_, store::EventLog>,
) -> Result<store::Blackboard, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let proj = &ctx.proj;
    let scene_id = scene_ctx(proj);
    let partition = scene_id
        .as_deref()
        .and_then(|id| proj.scenes.get(id))
        .cloned();
    let board = store::Blackboard {
        // 加固 A7：与世界时钟校准同一口径——极端 day 会让时钟步进的 day 加法溢出 panic
        day: prompt::clamp_story_day(day),
        clock: clock.trim().to_string(),
        place: place.trim().to_string(),
        actors: actors
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        // 场景分区事件的 extra = 场景 flags（整体替换语义，保留既有 flags）
        extra: partition
            .as_ref()
            .map(|sc| sc.flags.clone())
            .unwrap_or_default(),
    };
    let turn = proj.last_message().map(|m| m.turn).unwrap_or(0);
    let board_scene = if partition.is_some() { scene_id.clone() } else { None };
    let proj = commit(
        &log,
        &ctx.root,
        &ctx.meta,
        LogBody::Blackboard(event::BlackboardEvent {
            turn,
            reason: "manual".into(),
            scene_id: board_scene,
            board: board.clone(),
            ts: store::unix_now(),
        }),
    )?;
    Ok(proj.effective_board(scene_id.as_deref()))
}

// ---------- 即兴模式（M3.8 · 设计 §6.8-4）：薄实体现场补「设定·暂定」----------

/// 发送路径的即兴步骤：本轮文本提及的实体过薄（`complete::THIN_THRESHOLD`）时，
/// 调便宜档补一条暂定事实并落 improv 提案。
///
/// 返回 `Ok(Some(id))` = 落了一条提案（调用方随后重投影，让组装经投影回读注入行）；
/// `Ok(None)` = 没触发（未开 / 无薄实体 / 模型没给出有效内容）；`Err` = 管线侧失败，
/// 调用方记诊断跳过——即兴永远不挡说话。
#[allow(clippy::too_many_arguments)]
async fn maybe_improv(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    codex_cache: &CodexCache,
    user_text: &str,
    proj: &event::Projection,
    scene: Option<&str>,
    turn: u64,
    log: &store::EventLog,
) -> Result<Option<String>, String> {
    let cx = load_codex(root, Some(codex_cache), &session_world(meta));
    // 会话种子：本轮输入 + 当前场景最近几条（即兴要顺着刚聊起来的话头长）
    let mut seed = String::from(user_text);
    for m in proj.messages.iter().rev() {
        if seed.lines().count() >= 8 {
            break;
        }
        if let Some(sc) = scene {
            if proj.scene_of_message(m) != sc {
                continue;
            }
        }
        seed = format!("{}：{}\n{seed}", m.name.as_deref().unwrap_or(m.role.as_str()), m.content);
    }
    let Some(entity) = complete::improv_candidates(&cx, &seed).into_iter().next() else {
        return Ok(None);
    };
    let world_lines: Vec<String> = cx
        .entities()
        .iter()
        .filter(|e| e.status != "retired")
        .take(40)
        .map(|e| format!("{} {} {}——{}", e.id, e.ty, e.name, e.one_liner))
        .collect();
    let prompt_text = complete::build_improv_prompt(entity, &world_lines, &seed);
    let provider = pick_util_provider(root)?;
    let proxy = proxy_of(root);
    let raw = llm::chat_complete(
        &provider,
        &[llm::ChatMessage {
            role: "user".into(),
            content: prompt_text,
            tool_calls: None,
        }],
        1024,
        0.6,
        proxy.as_deref(),
    )
    .await?;
    let Some(draft) = complete::parse_improv(&raw)? else {
        return Ok(None);
    };
    // anchors 最高保护级：即兴也不许碰（§6.8-3）
    let proposal_value = serde_json::json!({ "facet": draft.facet, "value": draft.value });
    if let Some(reason) = cx
        .get(&entity.id)
        .and_then(|e| codex::anchors_conflict(e, &proposal_value))
    {
        crate::diag::record("improv", format!("即兴提案与辨识点冲突，丢弃：{reason}"));
        return Ok(None);
    }
    let id = format!("improv.{}.{}", entity.id, turn);
    commit(
        log,
        root,
        meta,
        LogBody::Proposal(event::ProposalEvent {
            turn,
            id: id.clone(),
            op: "propose".into(),
            kind: "new_fact".into(),
            origin: "improv".into(),
            payload: Some(serde_json::json!({
                "target": entity.id,
                "value": { "facet": draft.facet, "value": draft.value },
                "text": draft.text,
                "provisional": true,
                "reason": format!("第 {turn} 轮即兴补一条暂定设定（{}）", provider.name),
            })),
            note: None,
            ts: store::unix_now(),
        }),
    )?;
    Ok(Some(id))
}

/// 手动补全（M3.8 · 设计 §6.8-1）：为选中实体的缺失 facet 生成草稿。
///
/// 读实体 + 一跳关系 + 世界概览 → util 档生成 → 返回未落流的草稿（前端 diff 卡片
/// 呈现：接受 = codex_complete_apply 走提案通道物化；重写 = 再调一次；丢弃 = 无事发生）。
#[tauri::command]
pub async fn codex_complete(
    session_id: String,
    target: String,
    codex_cache: State<'_, CodexCache>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let cx = load_codex(&root, Some(&codex_cache), &session_world(&meta));
    let entity = cx
        .get(&target)
        .ok_or_else(|| format!("实体 {target} 不存在"))?
        .clone();
    let missing = complete::missing_facets(&entity);
    if missing.is_empty() {
        return Err("这个实体的模板 facets 都齐了，没什么可补的".into());
    }
    // 一跳关系邻体（relations.to 指向的实体，一行一个）
    let neighbors: Vec<String> = entity
        .relations
        .iter()
        .filter_map(|r| {
            cx.get(&r.to).map(|n| {
                format!("{} {} {}——{}", n.id, n.ty, n.name, n.one_liner)
            })
        })
        .collect();
    let world_lines: Vec<String> = cx
        .entities()
        .iter()
        .filter(|e| e.status != "retired")
        .take(40)
        .map(|e| format!("{} {} {}——{}", e.id, e.ty, e.name, e.one_liner))
        .collect();
    // 时代基调（M3.7 世界主线的叶阶段 directive）：补全要与「现在」的大势一致。
    // 路径回落用世界时钟进度（补全不依赖会话事件流，任何入口都能拿到同一大势）
    let world_name = session_world(&meta);
    let era = load_worldline(&root, &world_name).and_then(|wl| {
        let world_state = store::load_world(&root, &world_name);
        let path = match world_state.worldline {
            Some(prog) if prog.id == wl.id && !prog.path.is_empty() => prog.path,
            _ => wl.tree.active_path(&wl.tree.root),
        };
        worldline::era_line(&wl.tree, &path)
    });
    let ctx = complete::CompletionContext {
        entity: &entity,
        neighbors: &neighbors,
        world_lines: &world_lines,
        premise: meta.premise.as_deref(),
        era: era.as_deref(),
    };
    let prompt_text = complete::build_completion_prompt(&ctx);
    let provider = pick_util_provider(&root)?;
    let proxy = proxy_of(&root);
    let raw = llm::chat_complete(
        &provider,
        &[llm::ChatMessage {
            role: "user".into(),
            content: prompt_text,
            tool_calls: None,
        }],
        4096,
        0.4,
        proxy.as_deref(),
    )
    .await?;
    let (facets, note) = complete::parse_completion(&raw)
        .map_err(|e| format!("补全回复解析失败：{e}"))?;
    // 逐 facet 校验（确定性先行）：anchors 冲突的条目直接剔除并说明
    let mut items: Vec<serde_json::Value> = Vec::new();
    for (facet, value) in &facets {
        let payload = serde_json::json!({ "facet": facet, "value": value });
        let issues = complete::validate_proposal(&cx, &target, "new_fact", &payload);
        let rejected = issues
            .iter()
            .find(|i| i.level == complete::IssueLevel::Reject)
            .map(|i| i.detail.clone());
        let warn = issues
            .iter()
            .find(|i| i.level == complete::IssueLevel::Warn)
            .map(|i| serde_json::json!({ "detail": i.detail, "current": i.current }));
        items.push(serde_json::json!({
            "facet": facet,
            "value": value,
            "rejected": rejected,
            "warn": warn,
        }));
    }
    Ok(serde_json::json!({
        "target": target,
        "items": items,
        "note": note,
        "provider": provider.name,
    }))
}

/// 语义矛盾检测（M3.8 · 设计 §6.8-3「可选 LLM 语义矛盾检测」）：按需对收件箱里的
/// 一条 codex 提案跑一次便宜档对照判断。结论只返回给界面做双源呈现，确认/否决时
/// 随 note 留档进事件流——重放不重调模型，与摘要产物同纪律。
#[tauri::command]
pub async fn codex_semantic_check(
    session_id: String,
    id: String,
    log: tauri::State<'_, store::EventLog>,
    codex_cache: tauri::State<'_, CodexCache>,
) -> Result<serde_json::Value, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let (root, meta) = (&ctx.root, &ctx.meta);
    let proj = &ctx.proj;
    let entry = proj
        .proposals
        .get(&id)
        .ok_or_else(|| format!("提案 {id} 不存在"))?;
    let payload = entry.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    let target = payload
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    if target.is_empty() {
        return Err("这条提案没有目标实体，语义检查无对象".into());
    }
    let cx = load_codex(&root, Some(&codex_cache), &session_world(&meta));
    let entity = cx
        .get(&target)
        .ok_or_else(|| format!("实体 {target} 不在设定集里"))?;
    let fact_brief = serde_json::to_string_pretty(&serde_json::json!({
        "id": entity.id,
        "name": entity.name,
        "one_liner": entity.one_liner,
        "facts": entity.facts,
    }))
    .map_err(|e| e.to_string())?;
    let proposal_brief = serde_json::to_string_pretty(&payload)
        .map_err(|e| e.to_string())?;
    let prompt_text = complete::build_semantic_check_prompt(&fact_brief, &proposal_brief);
    let provider = pick_util_provider(&root)?;
    let proxy = proxy_of(&root);
    let raw = llm::chat_complete(
        &provider,
        &[llm::ChatMessage {
            role: "user".into(),
            content: prompt_text,
            tool_calls: None,
        }],
        1024,
        0.2,
        proxy.as_deref(),
    )
    .await?;
    let contradictions = complete::parse_semantic_check(&raw);
    Ok(serde_json::json!({
        "id": id,
        "contradictions": contradictions,
        "provider": provider.name,
    }))
}

/// 手动补全的接受侧：一条提案落 propose + accept（origin=complete，动作可溯源），
/// 物化与收件箱确认同一条路——确认即写正史进注入。
#[tauri::command]
pub fn codex_complete_apply(
    session_id: String,
    target: String,
    facets: std::collections::BTreeMap<String, serde_json::Value>,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<usize, String> {
    codex_complete_apply_core(&log, &root(), &session_id, target, facets, note)
}

fn codex_complete_apply_core(
    log: &store::EventLog,
    root: &std::path::Path,
    session_id: &str,
    target: String,
    facets: std::collections::BTreeMap<String, serde_json::Value>,
    note: Option<String>,
) -> Result<usize, String> {
    let meta = store::load_session(root, session_id).map_err(|e| e.to_string())?;
    let world = session_world(&meta);
    let turn = project_session(log, root, &meta)?
        .last_message()
        .map(|m| m.turn)
        .unwrap_or(0);
    let mut applied = 0usize;
    for (facet, value) in &facets {
        let id = format!("complete.{}.{}.{}", target, turn, facet);
        let payload = serde_json::json!({
            "target": target,
            "value": { "facet": facet, "value": value },
            "reason": "手动补全（模板驱动）",
        });
        commit(
            log,
            root,
            &meta,
            LogBody::Proposal(event::ProposalEvent {
                turn,
                id: id.clone(),
                op: "propose".into(),
                kind: "new_fact".into(),
                origin: "complete".into(),
                payload: Some(payload.clone()),
                note: note.clone(),
                ts: store::unix_now(),
            }),
        )?;
        commit(
            log,
            root,
            &meta,
            proposal_event(
                turn,
                id,
                "accept",
                "new_fact",
                "manual",
                None,
                Some("补全卡片接受".into()),
                store::unix_now(),
            ),
        )?;
        materialize_accepted(root, &world, "new_fact", payload);
        applied += 1;
    }
    Ok(applied)
}

// ---------- 记忆检查器 v0（设计 §4.2：组装结果逐层可见）----------

/// 预览组装：按当前状态干跑一轮（不含用户消息），不发送。
///
/// **干跑不落盘**（M2.0 起）：预览不再记事件、也不再改 state/黑板。
/// M1 让预览也落盘，是为了避免「预览一次状态变了、正式发送又变一次」的漂移；
/// 事件化之后正式发送自己会跑一次并留下事件，预览再落盘反而是多算一次。
/// 语义源候选照算（M3.10）：检查器里「语义」激活原因要能点验，不必真发一轮。
#[tauri::command]
pub async fn preview_prompt(
    app: AppHandle,
    session_id: String,
    speaker: Option<String>,
    log: State<'_, store::EventLog>,
    codex_cache: State<'_, CodexCache>,
    runtime: State<'_, SessionRuntime>,
    tree_cache: State<'_, TreeCache>,
    embed_cache: State<'_, EmbedCache>,
) -> Result<prompt::PromptAssembly, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let cast = Cast::load(&root, &meta)?;
    // M3.11 多角色检查器：`speaker` = 以谁的视角预览注入层；缺省 = 主角色
    let speaker = resolve_viewpoint(&meta, speaker)?;
    let proj = project_session(&log, &root, &meta)?;
    let scene = scene_ctx(&proj);
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0) + 1;
    let world_name = session_world(&meta);
    let world_codex = load_codex(&root, Some(&codex_cache), &world_name);
    let scene_history = scene_messages(&proj.messages, scene.as_deref());
    let semantic_query = semantic_query_text(&scene_history, None);
    let semantic_hits = semantic_hits_for(
        &root,
        &world_codex,
        Some(&embed_cache),
        &meta,
        &world_name,
        &semantic_query,
        false, // 干跑：连元数据也不写
    )
    .await;
    let run = assemble_prompt_core(
        &ui_sink(&app),
        &root,
        &meta,
        &cast,
        &speaker,
        &proj.messages,
        &proj,
        None,
        turn,
        None, // 干跑：不记事件、不落盘
        Some(&codex_cache),
        Some(&runtime),
        Some(&tree_cache),
        scene.as_deref(),
        &semantic_hits,
    )?;
    Ok(run.assembly)
}

// ---------- 剧情线的手动操作（设计 §8.3：玩家手动开线/收线）----------

/// 手动开线（面板或 OOC；归属 origin=manual，重放历史时不会被丢弃）
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn open_thread(
    session_id: String,
    title: String,
    cause: String,
    actors: Vec<String>,
    importance: Option<f32>,
    log: State<'_, store::EventLog>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    open_thread_at(&log, &root, &meta, &title, &cause, &actors, importance, threads::ORIGIN_MANUAL)
}

/// 开线的内核（与 Tauri 无关，便于单测）。`origin` 标记动作来源
/// （manual 玩家 / director 导演树——两者都是元层动作，重建不丢）。
#[allow(clippy::too_many_arguments)]
fn open_thread_at(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    title: &str,
    cause: &str,
    actors: &[String],
    importance: Option<f32>,
    origin: &str,
) -> Result<serde_json::Value, String> {
    open_thread_scoped_at(
        log, root, meta, title, cause, actors, importance, origin,
        threads::SCOPE_SESSION, None,
    )
}

/// 开线的内核（带作用域与显式 id）。`scope` = session（会话线，缺省）| world
/// （世界级线 M3.7 · 设计 §6.6——压着整个世界、任何会话可推进，轮末回写 world.json）；
/// `id` = 沙箱 api 显式声明的线 id（尊重声明，缺省由标题生成）。
#[allow(clippy::too_many_arguments)]
fn open_thread_scoped_at(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    title: &str,
    cause: &str,
    actors: &[String],
    importance: Option<f32>,
    origin: &str,
    scope: &str,
    id: Option<&str>,
) -> Result<serde_json::Value, String> {
    let proj = project_session(log, root, meta)?;
    let board = blackboard_of(&proj);
    let turn = proj.last_message().map(|m| m.turn).unwrap_or(0);
    let id = id
        .map(str::to_string)
        .unwrap_or_else(|| threads::id_from_title(title));
    let mut thread = threads::Thread::open(
        &id,
        title,
        cause,
        actors,
        importance.unwrap_or(0.6),
        threads::ThreadStamp {
            turn,
            story_day: board.day,
            story_clock: board.clock.clone(),
        },
    );
    thread.scope = scope.to_string();
    let snapshot = thread.to_value();
    commit(
        log,
        root,
        meta,
        LogBody::Thread(event::ThreadEvent {
            turn,
            op: threads::OP_OPEN.into(),
            thread_id: id,
            thread: Some(snapshot.clone()),
            origin: origin.into(),
            note: None,
            ts: store::unix_now(),
        }),
    )?;
    Ok(snapshot)
}

/// 手动收线（设计 §8.3「收线自动做三件事」）：
/// ① 高显著结果记忆入宫殿（带 thread 链接，供「这件事的来龙去脉」聚合召回）；
/// ② 线事件落流（origin=manual）：现状卡的「心里有事」随之消失、C1 只读投影不再列出；
/// ③ 以 thread:<id>:resolved 为事件名求值一次状态树转移（设计 §8.5：线了结可驱动状态转移）。
#[tauri::command]
pub fn resolve_thread(
    session_id: String,
    id: String,
    outcome: String,
    log: State<'_, store::EventLog>,
    tree_cache: State<'_, TreeCache>,
    runtime: State<'_, SessionRuntime>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    resolve_thread_at(&log, &root, &meta, &id, &outcome, &tree_cache, &runtime, threads::ORIGIN_MANUAL)
}

/// 收线的内核（与 Tauri 无关，便于单测）。`origin` 标记动作来源
/// （manual 玩家 / director 导演树——收线三件事对两者一视同仁）。
#[allow(clippy::too_many_arguments)]
fn resolve_thread_at(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    id: &str,
    outcome: &str,
    tree_cache: &TreeCache,
    runtime: &SessionRuntime,
    origin: &str,
) -> Result<serde_json::Value, String> {
    let character = first_character(meta)?;
    // E2：loaded（整卡 + Lua 解析）加载后从未用——手动收线不再白读一次卡
    let proj = project_session(log, root, meta)?;
    let board = blackboard_of(&proj);
    let turn = proj.last_message().map(|m| m.turn).unwrap_or(0);

    let Some(mut thread) = proj
        .threads
        .get(id)
        .and_then(|v| threads::Thread::from_value(v).ok())
    else {
        return Err(format!("没有这条线：{id}"));
    };
    thread.resolve(turn, board.day, &board.clock, outcome);

    // ① 结果记忆（高显著：它是「结果」，比过程更该被记住）
    let seq = proj.episodes.len() + proj.memory.len() + 1;
    let memory = palace::MemObject {
        id: palace::next_id(seq),
        kind: palace::KIND_EPISODE.to_string(),
        content: outcome.to_string(),
        turn,
        story_day: board.day,
        story_clock: board.clock.clone(),
        place: if board.place.is_empty() {
            None
        } else {
            Some(board.place.clone())
        },
        actors: if board.actors.is_empty() {
            vec![character.clone()]
        } else {
            board.actors.clone()
        },
        witnesses: board.actors.clone(),
        salience: 0.9,
        emotion: None,
        links: vec![format!("thread:{id}")],
        thread: Some(id.to_string()),
        source: "thread.resolve".into(),
        ts: store::unix_now(),
        rehearsals: 0,
    };
    thread.attach_resolution_memory(&memory.id);
    let snapshot = thread.to_value();

    // ② 线事件（快照带 resolution 与结果记忆 id）
    commit(
        log,
        root,
        meta,
        LogBody::Thread(event::ThreadEvent {
            turn,
            op: threads::OP_RESOLVE.into(),
            thread_id: id.to_string(),
            thread: Some(snapshot.clone()),
            origin: origin.into(),
            note: Some(outcome.to_string()),
            ts: store::unix_now(),
        }),
    )?;
    commit(
        log,
        root,
        meta,
        LogBody::Memory(event::MemoryEvent {
            turn,
            origin: origin.into(),
            object: serde_json::to_value(&memory).map_err(|e| e.to_string())?,
            ts: store::unix_now(),
        }),
    )?;

    // ③ 线了结驱动状态树（设计 §8.5：thread:<id>:resolved 可作转移事件）。
    //    M3.1：全阵容各求值一次——线了结对每个角色的树都是一次事件
    let cast = Cast::load(root, meta)?;
    let active_entities = runtime.previously_active(&meta.id);
    let proj2 = project_session(log, root, meta)?;
    for m in &cast.members {
        let Some(tree) = load_tree(&m.loaded, Some(tree_cache)) else {
            continue;
        };
        let (events, _emits) = advance_state_tree(
            &proj2,
            &m.dir,
            &m.loaded,
            &tree,
            turn,
            &format!("{id}:resolved"), // 设计 §8.5 的 thread:<id>:resolved（id 本身已带 thread. 前缀）
            Some(&active_entities),
            meta.seed,
            // 手动收线是全局事件：所有场景的角色树都要求值，不限定场景
            None,
            Some(tree_cache),
        );
        for body in events {
            commit(log, root, meta, body)?;
        }
    }

    // ④ 意图回流评价（M3.5 · 设计 §9.2「线被拒绝/了结 → 回流评价：受挫情绪 +
    //    相关意图削弱」）：绑定了这条线的意图削弱，心理面板可查
    reflow_thread_feedback(&proj2, &cast, root, meta, log, id, &thread.title, turn)?;
    Ok(snapshot)
}

/// 回流评价的常数（M3.5 · 设计 §9.2）：线了结/被拒给意图主人的心理反馈。
/// 情绪名与强度固定（确定性、可回放）；削弱量按负增量应用。
const REFLOW_EMOTION: &str = "受挫";
const REFLOW_AFFECT_INTENSITY: f32 = 0.5;
const REFLOW_INTENT_DELTA: f32 = 0.35;

/// 意图回流评价（M3.5 · 设计 §9.2）：线被收结时，绑定了这条线的意图削弱
/// （`REFLOW_INTENT_DELTA` 的负增量，削到 0 即移除），并给意图主人记一条受挫情绪
/// （来源标注哪条线了结）。效果事件落盘（trigger=psyche.reflow），重放一致；
/// 没有任何意图绑定这条线时不产生事件。
#[allow(clippy::too_many_arguments)]
fn reflow_thread_feedback(
    proj: &event::Projection,
    cast: &Cast,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
    thread_id: &str,
    title: &str,
    turn: u64,
) -> Result<(), String> {
    let source = format!("剧情线「{title}」了结");
    for m in &cast.members {
        let state = current_state(proj, &m.dir, &m.loaded);
        let mut p = psyche::Psyche::from_state(&state);
        let linked: Vec<String> = p
            .intents
            .iter()
            .filter(|i| i.linked_thread.as_deref() == Some(thread_id))
            .map(|i| i.name.clone())
            .collect();
        if linked.is_empty() {
            continue;
        }
        for name in &linked {
            p.feel(REFLOW_EMOTION, REFLOW_AFFECT_INTENSITY, &source, turn);
            p.intend(name, -REFLOW_INTENT_DELTA, turn);
        }
        let mut next = state.clone();
        p.write_into(&mut next);
        let patch = event::state_patch(&state, &next);
        if !patch.is_empty() {
            commit(
                log,
                root,
                meta,
                LogBody::Effect(event::EffectEvent {
                    turn,
                    trigger: "psyche.reflow".into(),
                    character: m.dir.clone(),
                    state_set: patch,
                    blackboard: Vec::new(),
                    memory: Vec::new(),
                    scene_id: None,
                    ts: store::unix_now(),
                }),
            )?;
        }
    }
    Ok(())
}

// ---------- 事件流视图（M3.0 ④：检查器「事件流」页签的数据源）----------

/// 事件的一行摘要（seq/kind/turn 之外的人读内容）
fn timeline_brief(record: &event::LogRecord) -> String {
    let clip = |s: &str, n: usize| -> String {
        let t: String = s
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        if t.chars().count() > n {
            let head: String = t.chars().take(n).collect();
            format!("{head}…")
        } else {
            t
        }
    };
    match &record.body {
        LogBody::Message(m) => {
            let who = match m.role.as_str() {
                "user" => "我",
                "char" => "角色",
                _ => m.role.as_str(),
            };
            // 增强 A7：带工具调用的消息在事件流里可见（数量标注；逐条裁决看 effect/提案行）
            let tools = match m.tool_calls.as_deref().map(|c| c.len()) {
                Some(n) if n > 0 => format!("〔工具×{n}〕"),
                _ => String::new(),
            };
            format!("{who}{tools}：{}", clip(&m.content, 60))
        }
        LogBody::Effect(e) => {
            let mut parts: Vec<String> = Vec::new();
            if !e.state_set.is_empty() {
                parts.push(format!("state×{}", e.state_set.len()));
            }
            if !e.blackboard.is_empty() {
                parts.push(format!("黑板×{}", e.blackboard.len()));
            }
            if !e.memory.is_empty() {
                parts.push(format!("记忆×{}", e.memory.len()));
            }
            if parts.is_empty() {
                format!("{}（{}）无副作用", e.trigger, e.character)
            } else {
                format!("{}（{}）{}", e.trigger, e.character, parts.join(" "))
            }
        }
        LogBody::Blackboard(b) => format!(
            "{} → 第{}天 {} {}",
            b.reason, b.board.day, b.board.clock, b.board.place
        ),
        LogBody::Transition(t) => format!(
            "{} → {}（{}）",
            t.from.join("/"),
            t.to.join("/"),
            clip(&t.reason, 40)
        ),
        LogBody::Thread(t) => {
            let title = t
                .thread
                .as_ref()
                .and_then(|v| v.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let label = if title.is_empty() { t.thread_id.clone() } else { title.to_string() };
            match t.note.as_deref() {
                Some(note) if !note.is_empty() => format!("{} {}（{}）", t.op, clip(&label, 24), clip(note, 30)),
                _ => format!("{} {}（{}）", t.op, clip(&label, 24), t.origin),
            }
        }
        LogBody::Codex(c) => match c.note.as_deref() {
            Some(note) if !note.is_empty() => format!("{} {}（{}）", c.op, c.target, clip(note, 30)),
            _ => format!("{} {}（{}）", c.op, c.target, c.origin),
        },
        LogBody::Summary(s) => format!("批次 {}–{}：{}", s.from_turn, s.to_turn, clip(&s.delta, 50)),
        LogBody::Proposal(p) => format!("{} {}（{}）", p.op, p.kind, p.id),
        LogBody::Memory(m) => {
            let content = m
                .object
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{}：{}", m.origin, clip(content, 50))
        }
        LogBody::Scene(s) => {
            let title = s
                .scene
                .as_ref()
                .map(|sc| sc.title.clone())
                .unwrap_or_default();
            match s.op.as_str() {
                "switch" => format!("切场 → {}", s.scene_id),
                "split" => format!("分场 → {}（{}）", title, clip(&s.scene_id, 24)),
                "merge" => format!(
                    "合场 ← {} 并入 {}",
                    s.others.join("、"),
                    clip(&title, 24)
                ),
                _ => format!("{} {}（{}）", s.op, clip(&title, 24), s.origin),
            }
        }
        LogBody::Director(d) => match d.op.as_str() {
            "schedule" => {
                let who: Vec<String> = d
                    .picks
                    .iter()
                    .map(|p| {
                        if p.reasons.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{}（{}）", p.name, p.reasons.join(" · "))
                        }
                    })
                    .collect();
                match &d.direct {
                    Some(name) => format!("点名 → {name}"),
                    None => format!("调度 → {}", who.join("、")),
                }
            }
            "cut" => format!("转场 → {}（{}）", d.direct.clone().unwrap_or_default(), clip(&d.note.clone().unwrap_or_default(), 30)),
            "merge" => format!("合场裁决（{}）", clip(&d.note.clone().unwrap_or_default(), 40)),
            other => format!("{other}（{} 人）", d.picks.len()),
        },
        LogBody::DirectorTree(t) => {
            let from = if t.from.is_empty() {
                "开场".to_string()
            } else {
                t.from.last().cloned().unwrap_or_default()
            };
            format!("剧情：{} → {}（{}）", from, t.to.last().cloned().unwrap_or_default(), clip(&t.reason, 40))
        }
        LogBody::Worldline(t) => {
            let from = if t.from.is_empty() {
                "承袭世界".to_string()
            } else {
                t.from.last().cloned().unwrap_or_default()
            };
            format!("世界：{} → {}（{}）", from, t.to.last().cloned().unwrap_or_default(), clip(&t.reason, 40))
        }
    }
}

/// 类型化事件流视图（诊断/检查器）：最新在前，默认最近 200 条。
/// 每条 = { seq, kind, turn, brief }；brief 是人读摘要，重放语义以事件原文为准。
#[tauri::command]
pub fn session_timeline(
    session_id: String,
    limit: Option<usize>,
    log: State<'_, store::EventLog>,
) -> Result<Vec<serde_json::Value>, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    timeline_entries(&log, &root, &meta, limit)
}

/// 事件流视图的内核（与 Tauri 无关，便于单测）
fn timeline_entries(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let records = log.read(root, &meta.id).map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(200);
    Ok(records
        .iter()
        .rev()
        .take(limit)
        .map(|r| {
            serde_json::json!({
                "seq": r.seq,
                "kind": r.body.kind(),
                "turn": r.turn(),
                "brief": timeline_brief(r),
            })
        })
        .collect())
}

// ---------- 记忆检查器数据（M2.8 面板的数据源）----------

/// 组装检查器面板需要的全部投影数据（与 Tauri 无关的内核，便于单测）。
///
/// 一次给全：状态树路径与转移历史、剧情线、心理、宫殿三视图、设定集清单与揭示集。
/// 设计 §14：这些面板合称「记忆检查器」——看「我们处于哪个阶段、欠着什么线、
/// 她心里在想什么、她记得什么、这次注入了什么」。
#[allow(clippy::too_many_arguments)]
fn inspector_payload(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    character: &str,
    loaded: &card::LoadedCard,
    proj: &event::Projection,
    codex_cache: Option<&CodexCache>,
    tree_cache: Option<&TreeCache>,
    runtime: Option<&SessionRuntime>,
) -> Result<serde_json::Value, String> {
    let board = blackboard_of(proj);
    let bb_map = blackboard_env(&board);

    // 状态树：活跃路径 + 最近转移历史（新的在前）；M3.1 起按角色分道
    let tree = load_tree(loaded, tree_cache);
    let path = tree
        .as_ref()
        .map(|t| active_path_of(proj, t, character))
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
    // M3.11 视角切换：转移按角色分道，其他角色的转移不进这个视角的面板
    // （旧会话的转移无 character 字段 = 单角色历史，任何视角都认）
    let transitions: Vec<&event::TransitionEvent> = proj
        .transitions
        .iter()
        .rev()
        .filter(|t| t.character.as_deref().map(|c| c == character).unwrap_or(true))
        .take(20)
        .collect();

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
    // E2：同一世界只 load 一次 codex（原来 mentions 与实体清单各 load 一次）
    let world = session_world(meta);
    let cx = load_codex(root, codex_cache, &world);
    let mut mentions: Vec<String> = Vec::new();
    mentions.extend(cx.scan_mentions(&window_text).into_iter().map(|m| m.id));
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

    // 心理：内心摘要 + 情绪槽 + 意图（含触发记录）+ 心里话队列 + 衰减轨迹
    let state = current_state(proj, character, loaded);
    let p = psyche::Psyche::from_state(&state);
    let psyche_view = serde_json::json!({
        "summary": p.summary_line_for(&loaded.card.name),
        "affects": p.affects,
        "intents": p.intents,
        "scheduled": p.scheduled,
        "trail": p.decay_trail(),
        "auto_emotion": p.auto_emotion(),
    });

    // 宫殿：三视图 + 最近记忆（每条都能溯源到轮次）
    let memories = memory_objects(proj, character, board.day);
    let palace_view = serde_json::json!({
        "count": memories.len(),
        "rooms": palace::rooms(&memories),
        "timeline": palace::timeline(&memories),
        "graph": palace::link_graph(&memories),
        "recent": memories.iter().rev().take(20).map(palace::brief).collect::<Vec<_>>(),
    });

    // 设定集：世界清单（草稿与正史都列，注入只认 canon）
    // M3.8：每个实体带缺失 facet 清单（实体编辑器高亮 +「补全」按钮的数据源）
    let entities: Vec<serde_json::Value> = cx
        .entities()
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id, "name": e.name, "type": e.ty, "status": e.status,
                "oneLiner": e.one_liner, "anchors": e.anchors(),
                "missing": complete::missing_paths(e),
            })
        })
        .collect();

    // 收件箱富化（M3.8 · DoD 8「冲突双源呈现」）：codex 类提案带正史现值——
    // 收件箱里「现状 vs 提案」两边都有出处
    let proposals: Vec<serde_json::Value> = proj
        .proposals
        .values()
        .map(|p| {
            let mut enriched = p.clone();
            if let Some(obj) = enriched.as_object_mut() {
                let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
                if matches!(kind, "new_fact" | "fact_change") {
                    let target = obj
                        .get("payload")
                        .and_then(|v| v.get("target"))
                        .and_then(|t| t.as_str())
                        .unwrap_or_default();
                    let facet = obj
                        .get("payload")
                        .and_then(|v| {
                            let value = v.get("value").cloned().unwrap_or(serde_json::Value::Null);
                            facet_and_value(v, &value)
                        })
                        .map(|(f, _)| f);
                    if let (Some(entity), Some(facet)) = (cx.get(target), facet) {
                        if let Some(current) = codex::static_fact(entity, &facet) {
                            obj.insert(
                                "currentValue".into(),
                                serde_json::json!({
                                    "facet": facet,
                                    "value": current,
                                    "source": format!("{target}（正史）"),
                                }),
                            );
                        }
                    }
                }
            }
            enriched
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
        "summary": proj.summary,
        "proposals": proposals,
        "known": proj.known_for(character).into_iter().collect::<Vec<_>>(),
        "blackboard": board,
        "activeEntities": runtime.map(|r| r.previously_active(&meta.id)).unwrap_or_default(),
    }))
}

/// 设定收件箱：确认或否决一条提案（设计 §6.9：确认/否决动作进事件流，回放不受影响）。
/// 确认的 codex 提案（M3.8）同时物化进世界的 grown.json——「确认写正史进注入」的兑现点。
#[tauri::command]
pub fn decide_proposal(
    session_id: String,
    id: String,
    accept: bool,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<serde_json::Value, String> {
    decide_proposal_core(&log, &root(), &session_id, id, accept, note)
}

fn decide_proposal_core(
    log: &store::EventLog,
    root: &std::path::Path,
    session_id: &str,
    id: String,
    accept: bool,
    note: Option<String>,
) -> Result<serde_json::Value, String> {
    let meta = store::load_session(root, session_id).map_err(|e| e.to_string())?;
    let turn = project_session(log, root, &meta)?
        .last_message()
        .map(|m| m.turn)
        .unwrap_or(0);
    let proj = commit(
        log,
        root,
        &meta,
        proposal_event(
            turn,
            id.clone(),
            if accept { "accept" } else { "reject" },
            "",
            "manual",
            None,
            note,
            store::unix_now(),
        ),
    )?;
    if accept {
        if let Some(entry) = proj.proposals.get(&id) {
            materialize_accepted(
                root,
                &session_world(&meta),
                entry.get("kind").and_then(|k| k.as_str()).unwrap_or_default(),
                entry.get("payload").cloned().unwrap_or(serde_json::Value::Null),
            );
        }
    }
    Ok(proj
        .proposals
        .get(&id)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "id": id })))
}

/// 收件箱批量处理（M3.8 · DoD 8）：全部确认 / 全部否决。
/// 只动 status = propose 的条目；codex 提案照走单条同款物化。
#[tauri::command(async)]
pub fn decide_all_proposals(
    session_id: String,
    accept: bool,
    log: State<'_, store::EventLog>,
) -> Result<usize, String> {
    decide_all_proposals_core(&log, &root(), &session_id, accept)
}

fn decide_all_proposals_core(
    log: &store::EventLog,
    root: &std::path::Path,
    session_id: &str,
    accept: bool,
) -> Result<usize, String> {
    let meta = store::load_session(root, session_id).map_err(|e| e.to_string())?;
    // 加固 D9：轮次取一次——提案确认不产生新消息，批内 turn 根本不变
    //（原来每确认一条都全量投影一次只为取它）
    let start = project_session(log, root, &meta)?;
    let pending: Vec<String> = start
        .proposals
        .iter()
        .filter(|(_, v)| v.get("status").and_then(|s| s.as_str()) == Some("propose"))
        .map(|(id, _)| id.clone())
        .collect();
    let turn = start.last_message().map(|m| m.turn).unwrap_or(0);
    let world = session_world(&meta);
    for id in &pending {
        let proj = commit(
            log,
            root,
            &meta,
            LogBody::Proposal(event::ProposalEvent {
                turn,
                id: id.clone(),
                op: if accept { "accept" } else { "reject" }.into(),
                kind: String::new(),
                origin: "manual".into(),
                payload: None,
                note: Some(if accept { "批量确认" } else { "批量否决" }.into()),
                ts: store::unix_now(),
            }),
        )?;
        if accept {
            if let Some(entry) = proj.proposals.get(id) {
                materialize_accepted(
                    root,
                    &world,
                    entry.get("kind").and_then(|k| k.as_str()).unwrap_or_default(),
                    entry.get("payload").cloned().unwrap_or(serde_json::Value::Null),
                );
            }
        }
    }
    Ok(pending.len())
}

/// 确认的 codex 提案物化进世界的 grown.json（M3.8 · 设计 §6.9）。
///
/// 只认四种 codex 类型；其他类型（thread/psyche 等）只改状态不写设定集。
/// payload 形态宽容：value 里带 `facet`（或 `path`/`key`）字段的写指定路径，
/// 否则整个 value 当作该 facet 的内容无法落点——留诊断跳过（提案仍是收件箱里的记录）。
fn materialize_accepted(root: &std::path::Path, world: &str, kind: &str, payload: serde_json::Value) {
    const MATERIALIZABLE: [&str; 4] = ["new_entity", "new_fact", "fact_change", "relation"];
    if !MATERIALIZABLE.contains(&kind) {
        return;
    }
    let Some(target) = payload
        .get("target")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let value = payload.get("value").cloned().unwrap_or(serde_json::Value::Null);
    if value.is_null() {
        return;
    }
    let mut grown = store::load_grown(root, world);
    let entry = grown.entities.entry(target.clone()).or_insert_with(|| serde_json::json!({}));
    let obj = match entry.as_object_mut() {
        Some(o) => o,
        None => {
            crate::diag::record("codex", format!("正史增量 {target} 不是对象，跳过物化"));
            return;
        }
    };
    match kind {
        "new_entity" => {
            // 全量骨架；id 以提案 target 为准
            *entry = value;
            if let Some(o) = entry.as_object_mut() {
                o.insert("id".into(), serde_json::json!(target));
            }
        }
        "new_fact" | "fact_change" => {
            let Some((facet, val)) = facet_and_value(&payload, &value) else {
                crate::diag::record(
                    "codex",
                    format!("提案 {target} 的 value 没有可辨识的 facet 路径，未物化"),
                );
                return;
            };
            let facts = obj
                .entry("facts")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(f) = facts.as_object_mut() {
                f.insert(facet, val);
            }
        }
        "relation" => {
            let rel = if value.is_object() {
                value
            } else {
                serde_json::Value::Null
            };
            if rel.is_null() {
                return;
            }
            let rels = obj.entry("relations").or_insert_with(|| serde_json::json!([]));
            if let Some(list) = rels.as_array_mut() {
                let to = rel.get("to").and_then(|t| t.as_str()).unwrap_or_default();
                let rkind = rel.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
                let dup = list.iter().any(|r| {
                    r.get("to").and_then(|t| t.as_str()) == Some(to)
                        && r.get("kind").and_then(|k| k.as_str()) == Some(rkind)
                });
                if !dup {
                    list.push(rel);
                }
            }
        }
        _ => {}
    }
    if let Err(e) = store::save_grown(root, world, &grown) {
        crate::diag::record("codex", format!("正史增量写入失败：{e}"));
    }
}

/// 提案的 facet 路径宽容解析：value/payload 里带 `facet`（或 `path`/`key`）+ `value`
/// 的写指定路径；value 本身是 {facet, value} 对象也认。
fn facet_and_value(
    payload: &serde_json::Value,
    value: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    for layer in [payload, value] {
        if let Some(obj) = layer.as_object() {
            let facet = obj
                .get("facet")
                .or_else(|| obj.get("path"))
                .or_else(|| obj.get("key"))
                .and_then(|f| f.as_str())
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_string);
            if let Some(facet) = facet {
                if let Some(v) = obj.get("value") {
                    return Some((facet, v.clone()));
                }
            }
        }
    }
    None
}

/// 瞬时状态提案的 {key, value} 解析（value 形态见 PRODUCT_SPEC 第 8 类的 transient 行）
fn transient_key_value(value: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    let obj = value.as_object()?;
    let key = obj
        .get("key")
        .and_then(|k| k.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)?;
    let val = obj.get("value")?;
    if val.is_null() {
        return None;
    }
    Some((key, val.clone()))
}

/// 记忆检查器数据（M2.8 面板）：一次性给前端全部投影视图。
/// M3.11 多角色检查器：`character` = 以谁的视角看（状态树/心理/宫殿/揭示集按角色分道），
/// 必须在阵容里；缺省 = 主角色，1v1 行为不变。
#[tauri::command]
pub fn inspector_data(
    session_id: String,
    character: Option<String>,
    log: State<'_, store::EventLog>,
    codex_cache: State<'_, CodexCache>,
    tree_cache: State<'_, TreeCache>,
    runtime: State<'_, SessionRuntime>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let viewpoint = resolve_viewpoint(&meta, character)?;
    let loaded = card::load_card(&root, &viewpoint).map_err(|e| e.to_string())?;
    let proj = project_session(&log, &root, &meta)?;
    inspector_payload(
        &root,
        &meta,
        &viewpoint,
        &loaded,
        &proj,
        Some(&codex_cache),
        Some(&tree_cache),
        Some(&runtime),
    )
}

/// 便宜档接入点（设计 §11：自动总结用 util 档；没配就回退 chat 档）
fn pick_util_provider(root: &std::path::Path) -> Result<Provider, String> {
    let providers = store::load_providers(root).map_err(|e| e.to_string())?;
    providers
        .iter()
        .find(|p| p.role == "util")
        .or_else(|| providers.iter().find(|p| p.role == "chat"))
        .cloned()
        .ok_or_else(|| "未配置可用接入点".to_string())
}

// ---------- 卡内状态与长期记忆（M1.6：hooks 的可观测面）----------

/// 角色私有 state 现状（会话快照为空时回退卡上 `state` 初始值）
#[tauri::command]
/// 卡内私有 state 现状（卡内状态面板）。M3.11：`character` = 看谁的 state，缺省主角色。
pub fn get_card_state(
    session_id: String,
    character: Option<String>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let viewpoint = resolve_viewpoint(&meta, character)?;
    let loaded = card::load_card(&root, &viewpoint).map_err(|e| e.to_string())?;
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
            characters: Vec::new(),
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
                scene_id: None,
                board: board.clone(),
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        // 缺省场景落地（与 new_session 同构）
        log.append(
            &root,
            &meta.id,
            LogBody::Scene(event::SceneEvent {
                turn: 0,
                op: "create".into(),
                scene_id: scene::DEFAULT_SCENE_ID.into(),
                scene: Some(scene::Scene::from_board(
                    scene::DEFAULT_SCENE_ID,
                    "开场",
                    &board,
                    0,
                    "default",
                    store::unix_now(),
                )),
                others: Vec::new(),
                origin: "default".into(),
                note: None,
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
                name: None,
                    turn: 0,
                    role: "char".into(),
                    content: first,
                    ts: store::unix_now(),
                    scene_id: None,
                    tool_calls: None,
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
            characters: Vec::new(),
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
        name: None,
            turn,
            role: "user".into(),
            content: content.into(),
            ts: store::unix_now(),
            scene_id: None,
        tool_calls: None,
        }
            
    }

    fn stored_state(root: &std::path::Path, meta: &store::SessionMeta) -> serde_json::Value {
        store::load_state(root, &meta.id).unwrap()
    }

    /// 一轮完整生成：**与生产共用同一份内核**（assemble_prompt_core / run_message_hook_core /
    /// commit_reply），只把界面事件回调换成空实现。此前单测另写了一份等价逻辑，
    /// 于是生产漏掉「用户消息那一步」时测试仍然绿——这个坑不再犯。
    /// 单角色夹具（多角色测试另见 cast_of / simulate_turn_as）。
    fn simulate_turn(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        loaded: &card::LoadedCard,
        log: &store::EventLog,
        turn: u64,
        content: &str,
    ) -> (prompt::PromptAssembly, llm::HookReport) {
        let dir = first_character(meta).unwrap();
        let cast = Cast {
            members: vec![CastMember {
                dir,
                loaded: loaded.clone(),
            }],
        };
        let speaker = cast.first().dir.clone();
        let proj = project_session(log, root, meta).unwrap();
        let history = proj.messages.clone();
        let run = assemble_prompt_core(
            &noop_sink(),
            root,
            meta,
            &cast,
            &speaker,
            &history,
            &proj,
            Some(content),
            turn,
            Some(log),
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        log.append(root, &meta.id, LogBody::Message(user_msg(turn, content)))
            .unwrap();
        let after_user = run_message_hook_core(root, meta, loaded, &speaker, turn, None, log);
        let after_reply = commit_reply(
            root, meta, &cast, &speaker, turn, "（回复）", &[], None, log, None, None, None, None,
        )
        .unwrap();

        // 报告取「本轮最后一次」（回复后的状态就是前端看到的最终状态）
        let mut report = after_reply;
        report.ran |= after_user.ran;
        report.memory = [after_user.memory, report.memory].concat();
        (run.assembly, report)
    }

    /// 装载会话阵容（多角色测试用；与生产同一入口）
    fn cast_of(root: &std::path::Path, meta: &store::SessionMeta) -> Cast {
        Cast::load(root, meta).unwrap()
    }

    /// 双卡阵容夹具（M3.1 隔离模式用例）：主角色小雨 + 次角色阿澈，时钟 20:55
    /// （第一轮末步进到 21:05，越过主角色状态树的 21:00 转移门槛）
    fn setup_cast2(card_xiaoyu: &str, card_ache: &str) -> (tempfile::TempDir, store::SessionMeta, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        store::ensure_layout(&root).unwrap();
        for (name, src) in [("小雨", card_xiaoyu), ("阿澈", card_ache)] {
            let d = root.join(format!("characters/{name}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("card.lua"), src).unwrap();
        }
        let meta = store::new_session(
            &root,
            &store::NewSessionRequest {
                character: "小雨".into(),
                characters: vec!["小雨".into(), "阿澈".into()],
                persona: None,
                day: Some(1),
                clock: Some("20:55".into()),
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
                scene_id: None,
                board: board.clone(),
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Scene(event::SceneEvent {
                turn: 0,
                op: "create".into(),
                scene_id: scene::DEFAULT_SCENE_ID.into(),
                scene: Some(scene::Scene::from_board(
                    scene::DEFAULT_SCENE_ID,
                    "开场",
                    &board,
                    0,
                    "default",
                    store::unix_now(),
                )),
                others: Vec::new(),
                origin: "default".into(),
                note: None,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        (dir, meta, root)
    }

    /// 三人阵容夹具（M3.3 视角记忆用例）：小雨 + 阿澈 + 小玲，时钟 20:55
    /// （第一轮末步进到 21:05，越过主角色状态树的 21:00 转移门槛），genesis + 缺省场景
    fn setup_cast3(
        card_xiaoyu: &str,
        card_ache: &str,
        card_ling: &str,
    ) -> (tempfile::TempDir, store::SessionMeta, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        store::ensure_layout(&root).unwrap();
        for (name, src) in [("小雨", card_xiaoyu), ("阿澈", card_ache), ("小玲", card_ling)] {
            let d = root.join(format!("characters/{name}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("card.lua"), src).unwrap();
        }
        let meta = store::new_session(
            &root,
            &store::NewSessionRequest {
                character: "小雨".into(),
                characters: vec!["小雨".into(), "阿澈".into(), "小玲".into()],
                persona: None,
                day: Some(1),
                clock: Some("20:55".into()),
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
                scene_id: None,
                board: board.clone(),
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Scene(event::SceneEvent {
                turn: 0,
                op: "create".into(),
                scene_id: scene::DEFAULT_SCENE_ID.into(),
                scene: Some(scene::Scene::from_board(
                    scene::DEFAULT_SCENE_ID,
                    "开场",
                    &board,
                    0,
                    "default",
                    store::unix_now(),
                )),
                others: Vec::new(),
                origin: "default".into(),
                note: None,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        (dir, meta, root)
    }

    /// 用已装载的卡造单角色阵容（直接调内核的单角色测试用，省一次读盘）
    fn single_cast(meta: &store::SessionMeta, loaded: &card::LoadedCard) -> Cast {
        Cast {
            members: vec![CastMember {
                dir: first_character(meta).unwrap(),
                loaded: loaded.clone(),
            }],
        }
    }

    /// 以指定发言人身份跑一轮（M3.1 隔离模式的行为断言用）
    fn simulate_turn_as(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        cast: &Cast,
        speaker: &str,
        log: &store::EventLog,
        turn: u64,
        content: &str,
    ) -> prompt::PromptAssembly {
        let proj = project_session(log, root, meta).unwrap();
        let history = proj.messages.clone();
        let run = assemble_prompt_core(
            &noop_sink(),
            root,
            meta,
            cast,
            speaker,
            &history,
            &proj,
            Some(content),
            turn,
            Some(log),
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        log.append(root, &meta.id, LogBody::Message(user_msg(turn, content)))
            .unwrap();
        let member = cast.get(speaker).unwrap();
        run_message_hook_core(root, meta, &member.loaded, speaker, turn, None, log);
        commit_reply(
            root, meta, cast, speaker, turn, "（回复）", &[], None, log, None, None, None, None,
        )
        .unwrap();
        run.assembly
    }

    /// 增强 A DoD：带 tool_calls 的回复——直写当轮生效、提案当轮进收件箱；
    /// 编辑该消息（内容改写）后重放，投影与首次一致（决断 3：效果是消息的衍生事件），
    /// 已入箱的提案保留正史、不重复落事件。
    #[test]
    fn model_tool_calls_replay_identically_after_edit() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let log = store::EventLog::new();
        let cast = Cast::load(&root, &meta).unwrap();
        let speaker = cast.first().dir.clone();
        let calls = vec![
            llm::ToolCall {
                id: "c1".into(),
                name: "psyche_feel".into(),
                arguments: serde_json::json!({"name": "欣慰", "intensity": 0.8, "source": "被道谢"}),
            },
            llm::ToolCall {
                id: "c2".into(),
                name: "propose_fact".into(),
                arguments: serde_json::json!({"target": "char.小雨", "facet": "habit", "value": "道谢时会低头", "reason": "第 1 轮演出细节"}),
            },
        ];
        // commit_reply = 回复级落盘 + 轮末推进（心理 tick / 状态树）：重放路径的 ⑤
        // 同样会重推 tick，两端必须同口径，否则重放一致性无从谈起
        let report = commit_reply(
            &root, &meta, &cast, &speaker, 1, "（回复）", &calls, None, &log, None, None, None, None,
        )
        .unwrap();
        // 直写档证据：心理槽位与聚合效果
        assert!(
            report.logs.iter().any(|l| l.contains("工具 psyche_feel〔applied〕")),
            "verdict 要进报告：{:?}",
            report.logs
        );
        let records = log.read(&root, &meta.id).unwrap();
        let before = event::project_over(&records, &event::Base::default());
        let state1 = before.state_of(&speaker).unwrap();
        let affects1 = state1["psyche"]["affects"].as_array().unwrap().clone();
        assert!(
            affects1.iter().any(|a| a["name"] == "欣慰"),
            "psyche_feel 当轮生效：{state1}"
        );
        // 提案档证据：origin=model 一条，payload 完整
        let proposals1: Vec<_> = records
            .iter()
            .filter_map(|r| match &r.body {
                LogBody::Proposal(p) if p.origin == "model" => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(proposals1.len(), 1, "propose_fact 落一条提案事件");
        assert_eq!(proposals1[0].id, "codex.char.小雨.1.m1");
        // 消息体存档原始 tool_calls
        let reply = records
            .iter()
            .filter_map(|r| r.as_message())
            .find(|m| m.role == "char" && m.turn == 1)
            .unwrap();
        assert_eq!(
            reply.tool_calls.as_ref().map(|c| c.len()),
            Some(2),
            "原始调用随消息存档（决断 3）"
        );

        // 「编辑」：同一条流重建（rebuild_from 自会丢弃派生事件、重跑工具与钩子）
        let rebuilt = rebuild_from(&log, &root, &meta, &cast, &records, 1).unwrap();
        let after = event::project_over(&rebuilt, &event::Base::default());
        assert_eq!(
            before.state_of(&speaker),
            after.state_of(&speaker),
            "重放后的 state 与首次一致"
        );
        let affects2 = after.state_of(&speaker).unwrap()["psyche"]["affects"]
            .as_array()
            .unwrap();
        assert_eq!(&affects1, affects2);
        // 提案不重复：仍只有一条 origin=model（撞 id 跳过，保留正史）
        let proposals2: Vec<_> = rebuilt
            .iter()
            .filter_map(|r| match &r.body {
                LogBody::Proposal(p) if p.origin == "model" => Some(p.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(proposals2, vec!["codex.char.小雨.1.m1".to_string()]);
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
        let report = run_message_hook_core(&root, &meta, &loaded, "小雨", 9, None, &log);
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
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &edited, turn).unwrap();
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
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &edited, turn).unwrap();
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
        let (rewritten, _trim, turn, content) = plan_regenerate(&all).unwrap();
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
        let cast = single_cast(&meta, &loaded);
        let speaker = cast.first().dir.clone();
        assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            &speaker,
            history,
            &proj,
            content.as_deref(),
            turn,
            Some(&log),
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        run_message_hook_core(&root, &meta, &loaded, &speaker, turn, None, &log);
        commit_reply(
            &root, &meta, &cast, &speaker, turn, "（重roll 的回复）", &[], None, &log, None, None, None, None,
        )
        .unwrap();

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
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &records, 1).unwrap();
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
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &records, 1).unwrap();
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
        let again = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &edited, turn).unwrap();
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
        let cast = single_cast(&meta, &loaded);
        let speaker = cast.first().dir.clone();
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            &speaker,
            &proj.messages.clone(),
            &proj,
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(!run.assembly.layers.is_empty());
        assert_eq!(log.read(&root, &meta.id).unwrap().len(), before, "预览不写事件");
        assert_eq!(stored_state(&root, &meta), state_before, "预览不改状态");
    }

    /// M3.11 真机验收暴露的缺陷：发言人不在这个场景的在场名单时，
    /// assemble_prompt_core 里的 expect 直接 panic——异步命令里 panic 让前端
    /// promise 永不返回（检查器预览场外成员 / 重roll 后场景变动的边缘都会踩）。
    /// 纪律：不参与轮转的成员钩子全跳过（与冻结场景一致），B 层照常按她的视角组装。
    #[test]
    fn assembling_an_offstage_member_skips_hooks_instead_of_panicking() {
        let ache = r#"
return {
  spec = 'charcard/1.0', name = '阿澈', scenario = '图书馆', personality = '爽朗', first_mes = '（阿澈入席）',
  state = { favorability = 10 },
  hooks = {
    on_context = function(ctx, state)
      state.favorability = (state.favorability or 0) + 1
      ctx.inject('system', '【阿澈在场】')
    end,
  },
}
"#;
        let xiaoyu = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
}
"#;
        let (_dir, meta, root) = setup_cast2(xiaoyu, ache);
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);
        // 小玲式的离场：把阿澈从**聚焦场景分区**的在场名单划掉
        // （真机的 update_blackboard 命令写的就是场景分区；世界层事件不动分区）
        let mut proj = project_session(&log, &root, &meta).unwrap();
        let mut board = blackboard_of(&proj);
        board.actors = vec!["小雨".into()];
        log.append(
            &root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "manual".into(),
                scene_id: Some(scene::DEFAULT_SCENE_ID.into()),
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        proj = project_session(&log, &root, &meta).unwrap();

        // 场外成员组装：不 panic，A1/B 层照常，但她的 on_context 没跑（好感不动、无注入行）
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            "阿澈",
            &proj.messages.clone(),
            &proj,
            None,
            1,
            None,
            None,
            None,
            None,
            Some(scene::DEFAULT_SCENE_ID), // 聚焦场景：阿澈已不在场
            &[],
        )
        .expect("场外成员应能组装而非 panic");
        assert!(!run.assembly.layers.is_empty());
        let joined = run
            .assembly
            .layers
            .iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("【阿澈在场】"), "不在台上的成员 on_context 不跑：{joined}");
        assert!(
            !joined.contains("【阿澈在场】"),
            "不在台上的成员 on_context 不跑（无她的注入行）：{joined}"
        );
        assert_eq!(
            run.card_state["favorability"], serde_json::json!(10),
            "on_context 没跑：好感不动（她注入行也没有）"
        );
        assert!(run.ui_events.is_empty(), "场外成员无界面事件：{:?}", run.ui_events);
        // 在场成员照常跑钩子（对照面：主角色小雨没有钩子，但机制上 present 循环未被跳过）
        let run2 = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            "小雨",
            &proj.messages.clone(),
            &proj,
            None,
            1,
            None,
            None,
            None,
            None,
            Some(scene::DEFAULT_SCENE_ID),
            &[],
        )
        .expect("在场成员照常组装");
        assert!(!run2.assembly.layers.is_empty());
    }

    #[test]
    fn regenerate_plan_rolls_back_reply_or_retries_failed_turn() {
        let user = |turn, content: &str| Message {
        name: None,
            turn,
            role: "user".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        tool_calls: None,
        }
            ;
        let ch = |turn, content: &str| Message {
        name: None,
            turn,
            role: "char".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        tool_calls: None,
        }
            ;

        // 完整一轮：重roll 去掉末尾回复，组装历史不含本轮用户消息（trim=1，content 随请求走）
        let full = vec![ch(0, "开场"), user(1, "你好"), ch(1, "……嗯")];
        let (rewritten, trim, turn, content) = plan_regenerate(&full).unwrap();
        assert_eq!(rewritten.len(), 2, "末尾回复被移除");
        assert_eq!((trim, turn), (1, 1), "组装历史截掉本轮用户消息");
        assert_eq!(content.as_deref(), Some("你好"));

        // 请求失败：末尾只剩用户消息 → 直接重试这一轮
        let failed = vec![ch(0, "开场"), user(1, "你好")];
        let (rewritten, trim, turn, content) = plan_regenerate(&failed).unwrap();
        assert_eq!(rewritten.len(), 2, "没有回复可删，原样保留");
        assert_eq!(trim, 1);
        assert_eq!(content.as_deref(), Some("你好"));

        // M3.4 群聊一轮多回复：重roll 末位发言人——同轮更早的回复留在历史里
        // （trim=0、content=None：组装历史含本轮用户消息与先发言者的回复）
        let group = vec![user(1, "你们好"), ch(1, "嗨"), ch(1, "……你好呀")];
        let (rewritten, trim, turn, content) = plan_regenerate(&group).unwrap();
        assert_eq!(rewritten.len(), 2, "只移除末位发言人的回复");
        assert_eq!((trim, turn), (0, 1), "历史全量保留");
        assert_eq!(content, None);
        assert_eq!(rewritten.last().unwrap().role, "char");

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

    /// M3.10c 第六激活源（设计 §6.13）的宿主接线：旁路算好的语义候选经
    /// assemble_prompt_core 进 B3——窗口里没有任何别名命中也能激活实体，
    /// 检查器的逐卡激活原因可见「语义」。空切片 = 层关闭（其余测试的既有形态）。
    #[test]
    fn semantic_hits_reach_b3_as_activation_reasons() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
}
"#;
        let (_dir, meta, root) = setup(card);
        let dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&dir).unwrap();
        // 阿雪不在阵容、不在黑板——只有语义源能把她带出来（小雨本人作为发言人
        // 本就会由在场源激活，做不了「语义有无」的对照面）
        std::fs::write(
            dir.join("char.阿雪.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'char.阿雪', type = 'char', name = '阿雪',
  one_liner = '图书馆的前任管理员。',
}
"#,
        )
        .unwrap();
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        let proj = project_session(&log, &root, &meta).unwrap();
        let cast = single_cast(&meta, &loaded);
        let speaker = cast.first().dir.clone();
        let history = proj.messages.clone();

        // 窗口只有代词「她」——不命中任何别名；语义候选把阿雪带成 1 行
        let hits = vec![semantic::SemanticHit {
            id: "char.阿雪".into(),
            score: 0.62,
        }];
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            &speaker,
            &history,
            &proj,
            Some("她今晚也在吗？"),
            1,
            None,
            None,
            None,
            None,
            None,
            &hits,
        )
        .unwrap();
        let b3 = run
            .assembly
            .layers
            .iter()
            .find(|l| l.id == "B3")
            .expect("B3 实体卡层应出现");
        assert!(b3.content.contains("阿雪"), "语义候选激活实体：{}", b3.content);
        assert!(
            b3.sources.iter().any(|s| s.contains("语义")),
            "激活原因可见「语义」（检查器可点验）：{:?}",
            b3.sources
        );

        // 同一输入不给语义候选：纯确定性版不激活（降级即设计的对照面）
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            &speaker,
            &history,
            &proj,
            Some("她今晚也在吗？"),
            1,
            None,
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(
            run.assembly
                .layers
                .iter()
                .all(|l| !l.content.contains("阿雪")),
            "不给语义候选就不激活（降级即设计）：{:?}",
            run.assembly.layers.iter().map(|l| &l.content).collect::<Vec<_>>()
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
            proj.known_for("小雨").contains("char.小雨.secrets.工作牌"),
            "进入状态应向见证者揭示秘密：{:?}",
            proj.known_for("小雨")
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

        // 重放同一事件流得到同一条路径（设计 §7.3-5）。
        // 转移是重放时重导的派生事件，ts 取重放时刻——比较时忽略它（跨秒边界会偶发）
        let records = log.read(&root, &meta.id).unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &records, 1).unwrap();
        let c = event::project_over(&rebuilt, &event::Base::default());
        let shape = |ts: &[event::TransitionEvent]| -> Vec<(u64, Vec<String>, Vec<String>, String)> {
            ts.iter()
                .map(|t| (t.turn, t.from.clone(), t.to.clone(), t.reason.clone()))
                .collect()
        };
        assert_eq!(shape(&c.transitions), shape(&proj.transitions), "重放得到同一批转移");
        assert_eq!(c.known_for("小雨"), proj.known_for("小雨"), "重放得到同一份揭示集");
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
        let payload =
            inspector_payload(&root, &meta, "小雨", &loaded, &proj, None, None, None).unwrap();

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

    /// M3.11 多角色检查器：面板逐视角切换——状态树转移/宫殿/揭示集按角色分道，
    /// 仅 A 见过的事实不出现在 B 的检查器面板（DoD 4 的检查器侧镜像）。
    #[test]
    fn inspector_serves_each_viewpoint_in_a_multi_character_session() {
        let xiaoyu = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '轻松日常。',
        transitions = {
          { to = '夜谈', priority = 5,
            when = function(ev, bb, st)
              return (bb.clock or '') >= '21:00'
            end },
        },
      },
      ['夜谈'] = { parent = '日常', directive = '夜深人静。', reveal = { 'char.小雨.secrets.工作牌' } },
    },
  },
}
"#;
        let ache = r#"
return {
  spec = 'charcard/1.0', name = '阿澈', scenario = '图书馆', personality = '爽朗', first_mes = '（阿澈入席）',
}
"#;
        let (_dir, meta, root) = setup_cast2(xiaoyu, ache);
        let dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("char.小雨.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'char.小雨', type = 'char', name = '小雨',
  aliases = { '夜班管理员' },
  one_liner = '大学图书馆夜班管理员。',
  facts = { look = { impression = '旧毛衣' } },
  secrets = {
    ['工作牌'] = { content = '她挂着的旧胸牌，其实是已故母亲的遗物。', known_by = { '小雨' } },
  },
}
"#,
        )
        .unwrap();

        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);
        // 阿澈离场：reveal 的见证者只剩小雨
        let mut proj = project_session(&log, &root, &meta).unwrap();
        let mut board = blackboard_of(&proj);
        board.actors = vec!["小雨".into()];
        log.append(
            &root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "manual".into(),
                scene_id: None,
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        // 第一轮（小雨发言）：越过门槛转移进「夜谈」并 reveal 秘密
        simulate_turn_as(&root, &meta, &cast, "小雨", &log, 1, "小雨，你的胸牌挺特别的。");
        proj = project_session(&log, &root, &meta).unwrap();

        let secret_path = "char.小雨.secrets.工作牌";
        let yu = card::load_card(&root, "小雨").unwrap();
        let che = card::load_card(&root, "阿澈").unwrap();

        // 小雨视角：转移史是她的，揭示集含秘密
        let pyu = inspector_payload(&root, &meta, "小雨", &yu, &proj, None, None, None).unwrap();
        assert!(pyu["transitions"].as_array().unwrap().iter().any(|t| {
            t["to"].as_array().unwrap().last().map(|s| s == "夜谈").unwrap_or(false)
        }), "小雨视角应有夜谈转移：{:?}", pyu["transitions"]);
        assert!(
            pyu["known"].as_array().unwrap().iter().any(|k| k == secret_path),
            "小雨视角的揭示集应含秘密：{:?}",
            pyu["known"]
        );
        assert_eq!(pyu["stateTree"]["path"].as_array().unwrap().last(), Some(&serde_json::json!("夜谈")));

        // 阿澈视角：没有自己的转移，揭示集无秘密——检查器面板与注入层同一纪律
        let pche = inspector_payload(&root, &meta, "阿澈", &che, &proj, None, None, None).unwrap();
        assert!(
            pche["transitions"].as_array().unwrap().is_empty(),
            "阿澈视角不应看到小雨的转移：{:?}",
            pche["transitions"]
        );
        assert!(
            !pche["known"].as_array().unwrap().iter().any(|k| k == secret_path),
            "串台：仅小雨见过的秘密不得进阿澈的揭示集面板：{:?}",
            pche["known"]
        );

        // 不在阵容里的视角直接报错（命令入口的 resolve_viewpoint 必须守住）
        assert!(
            resolve_viewpoint(&meta, Some("路人".into())).is_err(),
            "阵容外的视角应被拒绝"
        );
        assert!(resolve_viewpoint(&meta, Some("小雨".into())).is_ok());
        assert_eq!(resolve_viewpoint(&meta, None).unwrap(), "小雨", "缺省 = 主角色");
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
                    scene_id: None,
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

        // 摘要进 C1（设计 §4.1：C1 = 滚动摘要 + 未决事项清单）
        let loaded = card::load_card(&root, "小雨").unwrap();
        let (assembly, _) = simulate_turn(&root, &meta, &loaded, &log, 1, "你好。");
        let c1 = assembly
            .layers
            .iter()
            .find(|l| l.id == "C1")
            .expect("C1 摘要层");
        assert!(c1.content.contains("<summary>"), "{}", c1.content);
        assert!(c1.content.contains("第一段"), "摘要正文应进 C1：{}", c1.content);

        // 检查器面板能看到摘要与提案
        let proj_view = project_session(&log, &root, &meta).unwrap();
        let payload =
            inspector_payload(&root, &meta, "小雨", &loaded, &proj_view, None, None, None).unwrap();
        assert!(payload["summary"].as_str().unwrap().contains("第二段"));
        assert_eq!(payload["proposals"][0]["status"], "accept");

        // 模型产物保留：编辑历史的重建不丢弃摘要与提案
        let records = log.read(&root, &meta.id).unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &records, 1).unwrap();
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

    /// M2.6 验收（不调模型）：批次判定 + 产物落事件 + 派生文件；情景记忆能被召回
    #[test]
    fn summary_pipeline_lands_events() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let log = store::EventLog::new();
        // 直接灌 46 条消息（模拟长会话；不跑钩子——本测试只关心管线本身）
        for turn in 1..=23u64 {
            log.append(&root, &meta.id, LogBody::Message(user_msg(turn, &format!("第{turn}轮"))))
                .unwrap();
            log.append(
                &root,
                &meta.id,
                LogBody::Message(Message {
                name: None,
                    turn,
                    role: "char".into(),
                    content: format!("回复{turn}"),
                    ts: 0,
                    scene_id: None,
                tool_calls: None,
                }
                    ),
            )
            .unwrap();
        }
        let proj = project_session(&log, &root, &meta).unwrap();
        let (_scene, batch, to_turn) =
            summary_batch_scenes(&proj, false).expect("应有滑出窗口的批次");
        assert_eq!(
            batch.len(),
            46 - prompt::WINDOW_MESSAGES,
            "批次 = 滑出 L0 窗口的部分"
        );
        assert_eq!(batch[0].role, "user");
        assert!(to_turn >= 1);

        let loaded = card::load_card(&root, "小雨").unwrap();
        let cx = load_codex(&root, None, "default");
        // 造一份总结产物（等价于模型回了合法 JSON 并被 sanitize）
        let outcome = summarize::sanitize(summarize::SummaryOutcome {
            chronicle: String::new(),
            summary_delta: "她记住了那条约定。".into(),
            hearsays: Vec::new(),
            episodes: vec![summarize::EpisodeDraft {
                content: "深夜闭馆时她把便签递过来。".into(),
                salience: 0.9,
                emotion: Some("温暖".into()),
                place: Some("图书馆".into()),
                actors: vec!["小雨".into()],
                witnesses: Vec::new(),
                links: vec!["topic:便签".into()],
                thread: None,
                turns: vec![3, 4],
            }],
            facts: vec![summarize::FactDraft {
                key: "玩家称呼".into(),
                value: serde_json::json!("阿澈"),
            }],
            threads: vec![summarize::ThreadDraft {
                title: "周五还书".into(),
                cause: "约定周五来还。".into(),
                actors: vec!["小雨".into()],
                importance: 0.7,
                grade: "natural".into(),
                windows: vec![serde_json::json!({ "mention": ["还书"] })],
                deadline_day: Some(5),
                cooldown: 5,
                framing: "她在意但不好意思催。".into(),
            }],
            psyche: vec![summarize::PsycheDraft {
                kind: "feel".into(),
                name: "忐忑".into(),
                intensity: 0.6,
                source: "怕他忘了".into(),
            }],
            codex: vec![summarize::CodexDraft {
                kind: "new_fact".into(),
                target: "char.小雨".into(),
                value: serde_json::json!({ "facts": { "schedule": "周三休息" } }),
                reason: "剧情里提到".into(),
            }],
            audit: Vec::new(),
        });
        let applied = applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj, outcome, 1, to_turn, 1, "20:00", "scene.main",
        ));
        assert!(applied >= 5, "摘要 + 2 条记忆 + 3 条提案：{applied}");

        let proj = project_session(&log, &root, &meta).unwrap();
        // 摘要分卷（M3.2）：场景卷挂在 scene.main 名下，世界层大事记为空
        let volume = proj.summary_for(Some("scene.main")).unwrap_or_default();
        assert!(volume.contains("约定"), "场景分卷应进投影：{volume}");
        assert!(proj.summary.is_empty(), "没有公开事件就不该写世界层大事记");
        assert_eq!(
            proj.summary_upto_for("scene.main"),
            to_turn,
            "场景水位记到哪一轮（批次从它之后取）"
        );
        assert_eq!(proj.episodes.len(), 2, "情景记忆 + L3 事实都进记忆对象层");

        // 情景记忆能被召回（见证者默认取 actors，视角过滤后仍命中）
        let objs = memory_objects(&proj, "小雨", 1);
        let hits = palace::recall(
            &objs,
            &palace::RecallQuery {
                viewer: "小雨".into(),
                now_day: 1,
                place: Some("图书馆".into()),
                present: vec!["小雨".into()],
                mentions: vec!["便签".into()],
                hints: Vec::new(),
                active_threads: Vec::new(),
                top_k: 5,
                budget_tokens: 0,
            },
        );
        assert!(
            hits.iter().any(|h| h.mem.content.contains("便签")),
            "情景记忆应可召回：{:?}",
            hits.iter().map(|h| &h.mem.content).collect::<Vec<_>>()
        );

        // 三类提案齐活，且派生文件落盘
        assert_eq!(
            proj.proposals.len(),
            3,
            "codex / thread / psyche 各一条：{:?}",
            proj.proposals.keys().collect::<Vec<_>>()
        );
        assert!(store::read_summary(&root, &meta.id).unwrap().contains("约定"));
        assert_eq!(store::read_proposals(&root, &meta.id).unwrap().len(), 3);

        // 第二批：已总结过的部分不再重复总结
        assert!(
            summary_batch_scenes(&proj, false).is_none(),
            "批次已被覆盖，不该重复总结"
        );
    }

    /// M3.10d 关联审计（设计 §6.13）：missed/facet 是收件箱提示条目（kind="audit"，
    /// 确认与否决只改状态、不物化）；fact 并入标准 codex 提案链路——
    /// **anchors 冲突自动驳回**（§6.8 最高保护级）与管线产物同一条路。
    #[test]
    fn audit_findings_reach_inbox_and_anchor_conflicts_are_rejected() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        let proj = project_session(&log, &root, &meta).unwrap();
        // 测试世界里的 char.小雨 带一条 anchors：动 look.anchors 的提案必须被驳回
        let entities_dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&entities_dir).unwrap();
        std::fs::write(
            entities_dir.join("char.小雨.json"),
            r#"{"id":"char.小雨","type":"char","name":"小雨","one_liner":"大学图书馆夜班管理员。","facts":{"look":{"anchors":["左眼角一颗泪痣"]}}}"#,
        )
        .unwrap();
        let cx = load_codex(&root, None, "default");

        let outcome = summarize::sanitize(summarize::SummaryOutcome {
            audit: vec![
                summarize::AuditDraft {
                    finding: summarize::AUDIT_MISSED.into(),
                    target: "char.小雨".into(),
                    facet: String::new(),
                    value: String::new(),
                    evidence: "第 1 轮「她今晚不在」用代词指小雨，激活记录里没有她".into(),
                },
                summarize::AuditDraft {
                    finding: summarize::AUDIT_FACET.into(),
                    target: "char.小雨".into(),
                    facet: "schedule".into(),
                    value: String::new(),
                    evidence: "多轮提到夜班但设定里没有作息".into(),
                },
                // 与 anchors 冲突的「新事实」：动 look.anchors → 必须被驳回
                summarize::AuditDraft {
                    finding: summarize::AUDIT_FACT.into(),
                    target: "char.小雨".into(),
                    facet: "look.anchors".into(),
                    value: "右眼角的泪痣".into(),
                    evidence: "第 1 轮提到泪痣换了边".into(),
                },
                // 不冲突的普通新事实：正常进收件箱待审
                summarize::AuditDraft {
                    finding: summarize::AUDIT_FACT.into(),
                    target: "char.小雨".into(),
                    facet: "schedule".into(),
                    value: "周三休息".into(),
                    evidence: "第 1 轮提到周三不来".into(),
                },
                // 缺引源的被丢弃（sanitize 纪律：没有引源的疑心不报）
                summarize::AuditDraft {
                    finding: summarize::AUDIT_MISSED.into(),
                    target: "place.图书馆".into(),
                    facet: String::new(),
                    value: String::new(),
                    evidence: String::new(),
                },
            ],
            ..Default::default()
        });
        let applied = applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj, outcome, 1, 1, 1, "20:00", "scene.main",
        ));

        let proj = project_session(&log, &root, &meta).unwrap();
        // 2 条提示条目 + 1 驳回 + 1 待审提案 = 4 条事件（缺引源的那条没进流）
        assert_eq!(applied, 4, "审计事件数：missed/facet/驳回/待审");
        let audit_proposals: Vec<_> = proj
            .proposals
            .values()
            .filter(|p| p.get("kind").and_then(|k| k.as_str()) == Some("audit"))
            .collect();
        assert_eq!(
            audit_proposals.len(),
            2,
            "missed + facet 两条提示：{audit_proposals:?}"
        );

        // anchors 冲突的审计提案已自动驳回，备注写明原因
        let rejected = proj
            .proposals
            .values()
            .find(|p| p.get("status").and_then(|s| s.as_str()) == Some("reject"))
            .expect("与 anchors 冲突的审计提案应被驳回");
        let note = rejected
            .get("note")
            .and_then(|n| n.as_str())
            .unwrap_or("");
        assert!(note.contains("辨识点"), "驳回备注应说明原因：{note}");
        assert!(
            store::load_grown(&root, "default").entities.is_empty(),
            "驳回的提案不物化"
        );

        // 普通新事实待审（接受后才会物化——与管线提案同一条路）
        let pending = proj
            .proposals
            .values()
            .find(|p| {
                p.get("status").and_then(|s| s.as_str()) == Some("propose")
                    && p.get("kind").and_then(|k| k.as_str()) == Some("new_fact")
            })
            .expect("不冲突的审计新事实应待审");
        assert_eq!(
            pending
                .get("payload")
                .and_then(|p| p.get("reason"))
                .and_then(|r| r.as_str())
                .unwrap_or(""),
            "关联审计：第 1 轮提到周三不来",
            "reason 带关联审计标记（收件箱可溯源）"
        );
    }

    /// M2 验收第 2 条：约定类情节能被正确了结——现状卡同步更新、结果自动入宫殿，
    /// 且线了结可驱动状态树转移（设计 §8.3 收线三件事 / §8.5）。
    #[test]
    fn resolving_a_thread_updates_scene_palace_and_state_tree() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '轻松日常。',
        transitions = {
          { to = '释然', priority = 5, when = 'event:thread.周五还书:resolved' },
        },
      },
      ['释然'] = { parent = '日常', directive = '事情说开了，她松了一口气。' },
    },
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "今天好冷。");

        // 玩家手动开线（设计 §8.3 ②）
        let snapshot = open_thread_at(
            &log,
            &root,
            &meta,
            "周五还书",
            "玩家忘带借书卡，小雨破例让他先把书拿走，约定周五来还。",
            &["小雨".into(), "玩家".into()],
            Some(0.8),
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        let id = snapshot["id"].as_str().unwrap().to_string();
        assert_eq!(id, "thread.周五还书", "线 id 由标题生成且带前缀");

        // 未收线：C1 的只读投影列得出这条欠账
        let (a2, _) = simulate_turn(&root, &meta, &loaded, &log, 2, "嗯。");
        assert!(
            a2.layers
                .iter()
                .find(|l| l.id == "C1")
                .map(|l| l.content.contains("周五还书"))
                .unwrap_or(false),
            "未了结的线应出现在 C1：{:?}",
            a2.layers.iter().find(|l| l.id == "C1").map(|l| l.content.clone())
        );

        // 收线：三件事一次做完
        let cache = TreeCache::default();
        let runtime = SessionRuntime::default();
        let resolved = resolve_thread_at(
            &log,
            &root,
            &meta,
            &id,
            "玩家如约还书，小雨送了张画着太阳的便签。",
            &cache,
            &runtime,
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        assert_eq!(resolved["state"], "resolved");
        assert!(
            resolved["resolution"]["memory"].is_string(),
            "结果记忆 id 应回填到线上：{resolved}"
        );

        // ① 结果自动入宫殿（高显著，带 thread 链接）
        let proj = project_session(&log, &root, &meta).unwrap();
        let result = proj
            .episodes
            .iter()
            .find(|e| e["content"].as_str().unwrap_or("").contains("如约还书"))
            .expect("结果记忆应进宫殿");
        assert_eq!(result["thread"], "thread.周五还书", "结果记忆应挂在线 id 上");
        // f32 → f64 的精度损失：0.9f32 序列化回来是 0.8999999…
        assert!(
            result["salience"].as_f64().unwrap() >= 0.89,
            "结果记忆应是高显著：{}",
            result["salience"]
        );

        // ② ③ 下一轮：C1 不再列它、B1 出现「了结未远」、状态树已被线驱动转移
        let (a3, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "那就好。");
        let c1 = a3.layers.iter().find(|l| l.id == "C1").map(|l| l.content.clone()).unwrap_or_default();
        assert!(!c1.contains("周五还书"), "收线后不该再列为未决事项：{c1}");
        let b1 = a3.layers.iter().find(|l| l.id == "B1").unwrap().content.clone();
        assert!(b1.contains("了结未远"), "现状卡应出现「了结未远」：{b1}");
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(
            proj.transitions.last().unwrap().to,
            vec!["日常", "释然"],
            "线了结应驱动状态转移：{:?}",
            proj.transitions
        );
    }

    /// M3.0 ④：session_timeline 把类型化事件流摊成人读视图（最新在前、带摘要）
    #[test]
    fn session_timeline_lists_events_newest_first_with_briefs() {
        let card = r#"
name = '小雨'
scenario = '图书馆'
personality = '安静'
first_mes = '晚上好。'
state_tree = {
  root = '日常',
  states = {
    ['日常'] = {
      directive = '轻松日常。',
      transitions = {
        { to = '释然', priority = 5, when = 'event:thread.周五还书:resolved' },
      },
    },
    ['释然'] = { parent = '日常', directive = '事情说开了。' },
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        open_thread_at(
            &log,
            &root,
            &meta,
            "周五还书",
            "玩家忘带借书卡。",
            &["小雨".into()],
            Some(0.8),
            threads::ORIGIN_MANUAL,
        )
        .unwrap();

        let entries = timeline_entries(&log, &root, &meta, None).unwrap();
        assert!(!entries.is_empty(), "事件流视图不该为空");
        // 最新在前：线程开线事件是最后落的，应该在首位
        assert_eq!(entries[0]["kind"], "thread");
        assert!(
            entries[0]["brief"]
                .as_str()
                .unwrap_or("")
                .contains("周五还书"),
            "线的摘要应带标题：{}",
            entries[0]["brief"]
        );
        // seq 单调：视图顺序的 seq 严格递减
        let seqs: Vec<u64> = entries.iter().filter_map(|e| e["seq"].as_u64()).collect();
        assert!(
            seqs.windows(2).all(|w| w[0] > w[1]),
            "最新在前意味着 seq 递减：{seqs:?}"
        );
        // 消息事件带人读摘要；limit 生效
        assert!(
            entries
                .iter()
                .any(|e| e["kind"] == "message" && e["brief"].as_str().unwrap_or("").contains("我：")),
            "消息事件的摘要应署名：{:?}",
            entries.iter().find(|e| e["kind"] == "message").map(|e| e["brief"].clone())
        );
        let limited = timeline_entries(&log, &root, &meta, Some(1)).unwrap();
        assert_eq!(limited.len(), 1, "limit 应截取最近 N 条");
    }

    /// M3.0 ⑤：批次总结的情景记忆盖**事发时刻**的故事章，不是总结时刻
    /// （管线在轮末异步跑，当前黑板可能已经推进了很多天——那不该算到记忆头上）
    #[test]
    fn pipeline_episodes_stamp_the_story_time_of_the_event() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");
        // 第 1 轮结束：init 20:00 + 步进 10 分钟 = 第 1 天 20:10

        let cx = load_codex(&root, None, "default");
        let outcome = summarize::sanitize(summarize::SummaryOutcome {
            chronicle: String::new(),
            summary_delta: String::new(),
            hearsays: Vec::new(),
            episodes: vec![summarize::EpisodeDraft {
                content: "他道了谢，她记住了。".into(),
                salience: 0.8,
                emotion: None,
                place: None,
                actors: vec!["小雨".into()],
                witnesses: Vec::new(),
                links: Vec::new(),
                thread: None,
                turns: vec![1],
            }],
            facts: vec![summarize::FactDraft {
                key: "称呼".into(),
                value: serde_json::json!("朋友"),
            }],
            threads: Vec::new(),
            psyche: Vec::new(),
            codex: Vec::new(),
            audit: Vec::new(),
        });
        // 假装总结发生在很久以后：调用方传来的「当前」黑板已是第 9 天深夜
        let proj_view = project_session(&log, &root, &meta).unwrap();
        applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj_view, outcome, 1, 1, 9, "23:50", "scene.main",
        ));

        let proj = project_session(&log, &root, &meta).unwrap();
        let objects: Vec<palace::MemObject> = proj
            .episodes
            .iter()
            .filter_map(|v| serde_json::from_value::<palace::MemObject>(v.clone()).ok())
            .collect();
        let episode = objects
            .iter()
            .find(|m| m.kind == palace::KIND_EPISODE)
            .expect("情景记忆应落宫殿");
        assert_eq!(episode.turn, 1, "溯源轮次取批次首个轮次");
        assert_eq!(episode.story_day, 1, "事发在第 1 天，不是总结时的第 9 天");
        assert_eq!(episode.story_clock, "20:10", "故事时刻取第 1 轮结束时的黑板");
        let fact = objects
            .iter()
            .find(|m| m.kind == palace::KIND_FACT)
            .expect("L3 事实应落宫殿");
        assert_eq!(fact.story_day, 1, "L3 事实盖批次末的章，也不是总结时刻");
    }

    /// M3.0 ⑥：last_thanked 类每轮都写的键值记忆合并成一条（最新值 + rehearsals），
    /// 不再以 0.50 显著度逐轮堆条目稀释 B4 的有效容量
    #[test]
    fn repeated_hook_memory_keys_merge_into_one_object() {
        let mut proj = event::Projection::default();
        for (turn, value) in [(1u64, 1u64), (2, 2), (3, 3)] {
            proj.memory.push(store::MemRecord {
                kind: "fact".into(),
                key: "last_thanked".into(),
                value: serde_json::json!(value),
                source: "hook.on_message".into(),
                turn,
                ts: turn,
            });
        }
        proj.memory.push(store::MemRecord {
            kind: "fact".into(),
            key: "borrowed_book".into(),
            value: serde_json::json!("《城南旧志》"),
            source: "hook.on_message".into(),
            turn: 1,
            ts: 1,
        });

        let objs = memory_objects(&proj, "小雨", 3);
        assert_eq!(objs.len(), 2, "两个键、不堆条目：{objs:?}");
        let thanked = objs
            .iter()
            .find(|o| o.links.iter().any(|l| l.contains("last_thanked")))
            .expect("last_thanked 应在对象里");
        assert_eq!(thanked.rehearsals, 2, "写了 3 次 = 再提及 2 次（召回打分据此增强）");
        assert_eq!(thanked.turn, 3, "保留最新一次写入的轮次与值");
        let other = objs
            .iter()
            .find(|o| o.links.iter().any(|l| l.contains("borrowed_book")))
            .expect("borrowed_book 应在对象里");
        assert_eq!(other.rehearsals, 0, "只写一次的键没有再提及加成");
    }

    /// M3.0 ⑦：编辑历史后，「收线驱动的转移」仍在——重放对着保留的手动线事件
    /// 补求值一次（与 resolve_thread_at ③ 同构），不因钩子重跑不带该触发器而丢失
    #[test]
    fn editing_history_keeps_thread_resolution_driven_transition() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '轻松日常。',
        transitions = {
          { to = '释然', priority = 5, when = 'event:thread.周五还书:resolved' },
        },
      },
      ['释然'] = { parent = '日常', directive = '事情说开了。' },
    },
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "谢谢你。");

        // 手动开线 → 收线：转移由 thread:resolved 事件驱动，落进事件流
        open_thread_at(
            &log,
            &root,
            &meta,
            "周五还书",
            "玩家忘带借书卡。",
            &["小雨".into()],
            Some(0.8),
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        let cache = TreeCache::default();
        let runtime = SessionRuntime::default();
        resolve_thread_at(
            &log,
            &root,
            &meta,
            "thread.周五还书",
            "玩家如约还书。",
            &cache,
            &runtime,
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        let proj = project_session(&log, &root, &meta).unwrap();
        let driven: Vec<&event::TransitionEvent> = proj
            .transitions
            .iter()
            .filter(|t| t.to == vec!["日常".to_string(), "释然".to_string()])
            .collect();
        assert_eq!(driven.len(), 1, "收线应恰好驱动一次转移：{:?}", proj.transitions);

        // 编辑第 1 轮的用户消息 → 重建：线事件是手动事件（保留），它驱动的转移是
        // 派生事件（丢弃）——重放必须对着线事件补求值，把转移长回来
        let records = log.read(&root, &meta.id).unwrap();
        let (pos, turn) = locate_message(&records, 1).expect("第 1 条消息");
        let mut edited = records.as_ref().clone();
        if let LogBody::Message(m) = &mut edited[pos].body {
            m.content = "（改写过的）今天好冷。".into();
        }
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &edited, turn).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        sync_now(&log, &root, &meta).unwrap();

        let proj2 = project_session(&log, &root, &meta).unwrap();
        let driven2: Vec<&event::TransitionEvent> = proj2
            .transitions
            .iter()
            .filter(|t| t.to == vec!["日常".to_string(), "释然".to_string()])
            .collect();
        assert_eq!(
            driven2.len(),
            1,
            "编辑后收线驱动的转移应原样保留（不丢失、不翻倍）：{:?}",
            proj2.transitions
        );
    }
    /// M3.1 验收：串台构造用例——隔离模式下，仅 A 见过的事实不出现在 B 的任何注入层
    /// （设计 §10.1「隔离总原则」：不在提示词里恳求，用数据结构让串台不可能）
    #[test]
    fn isolated_assembly_never_leaks_witness_only_facts_to_the_other() {
        let xiaoyu = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '轻松日常。',
        transitions = {
          { to = '夜谈', priority = 5,
            when = function(ev, bb, st)
              return (bb.clock or '') >= '21:00'
            end },
        },
      },
      ['夜谈'] = { parent = '日常', directive = '夜深人静。', reveal = { 'char.小雨.secrets.工作牌' } },
    },
  },
}
"#;
        let ache = r#"
return {
  spec = 'charcard/1.0', name = '阿澈', scenario = '图书馆', personality = '爽朗', first_mes = '（阿澈入席）',
}
"#;
        let (_dir, meta, root) = setup_cast2(xiaoyu, ache);
        // 世界：小雨实体带秘密（只有她自己一直知道，known_by 名单）
        let dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("char.小雨.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'char.小雨', type = 'char', name = '小雨',
  aliases = { '夜班管理员' },
  one_liner = '大学图书馆夜班管理员。',
  facts = { look = { impression = '旧毛衣' } },
  secrets = {
    ['工作牌'] = { content = '她挂着的旧胸牌，其实是已故母亲的遗物。', known_by = { '小雨' } },
  },
}
"#,
        )
        .unwrap();

        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);
        assert!(cast.is_multi(), "双卡阵容");

        // 阿澈离场（黑板手动事件，重放保留）：reveal 的见证者只剩小雨
        let mut proj = project_session(&log, &root, &meta).unwrap();
        let mut board = blackboard_of(&proj);
        board.actors = vec!["小雨".into()];
        log.append(
            &root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "manual".into(),
                scene_id: None,
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();

        // 第一轮（小雨发言）：20:55 → 21:05 越过门槛，轮末转移进「夜谈」并 reveal 秘密
        simulate_turn_as(&root, &meta, &cast, "小雨", &log, 1, "小雨，你的胸牌挺特别的。");
        proj = project_session(&log, &root, &meta).unwrap();
        assert!(
            proj.transitions.iter().any(|t| t.character.as_deref() == Some("小雨")
                && t.to.last().map(|s| s.as_str()) == Some("夜谈")),
            "小雨的树应转移进夜谈：{:?}",
            proj.transitions
        );
        let secret_path = "char.小雨.secrets.工作牌";
        assert!(proj.known.is_empty(), "见证者揭示不进全局集：{:?}", proj.known);
        assert!(
            proj.known_for("小雨").contains(secret_path),
            "小雨的视角应含该秘密：{:?}",
            proj.known_for("小雨")
        );
        assert!(
            !proj.known_for("阿澈").contains(secret_path),
            "阿澈的视角不含：{:?}",
            proj.known_for("阿澈")
        );

        // 串台断言①：以阿澈视角组装——B3 深卡不得出现只有小雨知道的秘密
        let b_assembly = simulate_turn_as(&root, &meta, &cast, "阿澈", &log, 2, "小雨今天怎么了？说说呗。");
        let b3 = b_assembly.layers.iter().find(|l| l.id == "B3").map(|l| l.content.clone());
        if let Some(b3) = &b3 {
            assert!(b3.contains("小雨"), "实体本身照常激活（提及）：{b3}");
            assert!(
                !b3.contains("已故母亲"),
                "串台：仅小雨见过的秘密不得进阿澈的 B3 深卡：{b3}"
            );
        }
        // A1 隔离提示：多角色时明确「你只扮演谁」
        let a1 = b_assembly.layers.iter().find(|l| l.id == "A1").unwrap();
        assert!(a1.content.contains("你只扮演「阿澈」"), "A1 应带隔离提示：{}", a1.content);
        // 回复署名：这条 char 消息是阿澈说的
        proj = project_session(&log, &root, &meta).unwrap();
        let last_char = proj.messages.iter().rev().find(|m| m.role == "char").unwrap();
        assert_eq!(last_char.name.as_deref(), Some("阿澈"), "回复应带发言人署名");

        // 串台断言②：以小雨视角组装——她自己的秘密照常在深卡里
        let a_assembly = simulate_turn_as(&root, &meta, &cast, "小雨", &log, 3, "……那张胸牌。");
        let b3a = a_assembly.layers.iter().find(|l| l.id == "B3").map(|l| l.content.clone());
        if let Some(b3a) = &b3a {
            assert!(
                b3a.contains("已故母亲"),
                "小雨的视角应见到自己的秘密（known_by 名单 + 见证 reveal）：{b3a}"
            );
        }

        // 串台断言③：B4 回忆——只有小雨见证的记忆，阿澈召不回
        proj = project_session(&log, &root, &meta).unwrap();
        let seq = proj.episodes.len() + proj.memory.len() + 1;
        let memory = palace::MemObject {
            id: palace::next_id(seq),
            kind: palace::KIND_EPISODE.to_string(),
            content: "深夜闭馆时她把画着猫头鹰的书签夹进了他的书".into(),
            turn: 1,
            story_day: 1,
            story_clock: "21:05".into(),
            place: Some("自习区".into()),
            actors: vec!["小雨".into()],
            witnesses: vec!["小雨".into()],
            salience: 0.9,
            emotion: None,
            links: vec!["topic:书签".into()],
            thread: None,
            source: "manual".into(),
            ts: store::unix_now(),
            rehearsals: 0,
        };
        log.append(
            &root,
            &meta.id,
            LogBody::Memory(event::MemoryEvent {
                turn: 1,
                origin: "manual".into(),
                object: serde_json::to_value(&memory).unwrap(),
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        let b4_assembly = simulate_turn_as(&root, &meta, &cast, "阿澈", &log, 4, "书签是哪来的？");
        let b4 = b4_assembly
            .layers
            .iter()
            .find(|l| l.id == "B4")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(
            !b4.contains("猫头鹰"),
            "串台：只有小雨见证的记忆不得进阿澈的 B4：{b4}"
        );
        let a4_assembly = simulate_turn_as(&root, &meta, &cast, "小雨", &log, 5, "……书签。");
        let b4a = a4_assembly
            .layers
            .iter()
            .find(|l| l.id == "B4")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(
            b4a.contains("猫头鹰"),
            "小雨应召回自己见证的记忆（话题命中）：{b4a}"
        );
    }

    /// M3.3 验收：视角记忆与转述（串台用例扩展到三人）——A 目击事件后 B 通过对话得知：
    /// B 的 B4 出现带来源标注的转述记忆（salience 约为亲历一半）、转述揭示的秘密对 B 展开深卡；
    /// C 始终不知情（B4 召不回、B3 深卡不展开）。设计 §10.4「转述是信息跨视角流动的唯一通道」。
    #[test]
    fn hearsay_reaches_only_the_listener_with_halved_salience() {
        let xiaoyu = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { favorability = 50 },
  state_tree = {
    root = '日常',
    states = {
      ['日常'] = {
        directive = '轻松日常。',
        transitions = {
          { to = '夜谈', priority = 5,
            when = function(ev, bb, st)
              return (bb.clock or '') >= '21:00'
            end },
        },
      },
      ['夜谈'] = { parent = '日常', directive = '夜深人静。', reveal = { 'char.小雨.secrets.工作牌' } },
    },
  },
}
"#;
        let ache = r#"
return {
  spec = 'charcard/1.0', name = '阿澈', scenario = '图书馆', personality = '爽朗', first_mes = '（阿澈入席）',
}
"#;
        let ling = r#"
return {
  spec = 'charcard/1.0', name = '小玲', scenario = '图书馆', personality = '活泼', first_mes = '（小玲入席）',
}
"#;
        let (_dir, meta, root) = setup_cast3(xiaoyu, ache, ling);
        // 世界：小雨实体带秘密（只有她自己一直知道，known_by 名单）
        let dir = root.join("codex/default/entities");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("char.小雨.lua"),
            r#"
return {
  spec = 'codex/1.0', id = 'char.小雨', type = 'char', name = '小雨',
  aliases = { '夜班管理员' },
  one_liner = '大学图书馆夜班管理员。',
  facts = { look = { impression = '旧毛衣' } },
  secrets = {
    ['工作牌'] = { content = '她挂着的旧胸牌，其实是已故母亲的遗物。', known_by = { '小雨' } },
  },
}
"#,
        )
        .unwrap();

        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);
        assert_eq!(cast.members.len(), 3, "三人阵容");

        // 阿澈与小玲离场（黑板手动事件，重放保留）：状态树 reveal 的见证者只剩小雨
        let mut proj = project_session(&log, &root, &meta).unwrap();
        let mut board = blackboard_of(&proj);
        board.actors = vec!["小雨".into()];
        log.append(
            &root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: 0,
                reason: "manual".into(),
                scene_id: None,
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();

        // 第 1 轮（小雨发言）：跨过 21:00 门槛 → 转移进「夜谈」并 reveal 秘密（见证者=小雨）
        simulate_turn_as(&root, &meta, &cast, "小雨", &log, 1, "（小雨独自整理书架。）");
        proj = project_session(&log, &root, &meta).unwrap();
        let secret_path = "char.小雨.secrets.工作牌";
        assert!(
            proj.known_for("小雨").contains(secret_path),
            "小雨的视角应含自己的秘密：{:?}",
            proj.known_for("小雨")
        );
        assert!(
            !proj.known_for("阿澈").contains(secret_path),
            "阿澈此刻还不知道：{:?}",
            proj.known_for("阿澈")
        );

        // A 目击事件：亲历记忆（只有小雨见证，salience 0.8）
        let seq = proj.episodes.len() + proj.memory.len() + 1;
        let lived = palace::MemObject {
            id: palace::next_id(seq),
            kind: palace::KIND_EPISODE.to_string(),
            content: "小雨把亲手做的猫头鹰书签夹进了阿澈借走的书".into(),
            turn: 1,
            story_day: 1,
            story_clock: "21:05".into(),
            place: Some("自习区".into()),
            actors: vec!["小雨".into()],
            witnesses: vec!["小雨".into()],
            salience: 0.8,
            emotion: None,
            links: vec!["topic:书签".into()],
            thread: None,
            source: "manual".into(),
            ts: store::unix_now(),
            rehearsals: 0,
        };
        log.append(
            &root,
            &meta.id,
            LogBody::Memory(event::MemoryEvent {
                turn: 1,
                origin: "manual".into(),
                object: serde_json::to_value(&lived).unwrap(),
                ts: store::unix_now(),
            }),
        )
        .unwrap();

        // B 通过对话得知（第 2 轮阿澈问起、小雨转告）→ 总结管线产出转述提案：
        // 宿主为阿澈写一条 hearsay（salience 折半），顺带揭示的秘密让他「知情」
        let cx = load_codex(&root, None, "default");
        let outcome = summarize::sanitize(summarize::SummaryOutcome {
            hearsays: vec![summarize::HearsayDraft {
                content: "小雨告诉阿澈，那只猫头鹰书签是她亲手做的".into(),
                source: "小雨".into(),
                listeners: vec!["阿澈".into()],
                salience: 0.8, // 原事件（亲历）的显著度；宿主写入时折半
                emotion: Some("得意".into()),
                place: None,
                links: vec!["topic:书签".into(), "person:小雨".into()],
                thread: None,
                turns: vec![2],
                reveals: vec![secret_path.into()],
            }],
            ..summarize::SummaryOutcome::default()
        });
        let proj_view = project_session(&log, &root, &meta).unwrap();
        let applied = applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj_view, outcome, 1, 2, 1, "21:15", "scene.main",
        ));
        assert_eq!(applied, 2, "一条转述记忆 + 一条揭示事件：{applied}");

        proj = project_session(&log, &root, &meta).unwrap();
        // 转述记忆：只有阿澈见证、来源标注、显著度约为亲历一半
        let hearsay: Vec<palace::MemObject> = proj
            .episodes
            .iter()
            .filter_map(|v| serde_json::from_value::<palace::MemObject>(v.clone()).ok())
            .filter(|m| m.kind == palace::KIND_HEARSAY)
            .collect();
        assert_eq!(hearsay.len(), 1, "应为阿澈写一条转述：{:?}", hearsay);
        let hs = &hearsay[0];
        assert_eq!(hs.witnesses, vec!["阿澈".to_string()]);
        assert_eq!(hs.source, "小雨");
        assert!((hs.salience - 0.4).abs() < 1e-6, "salience 折半：{}", hs.salience);
        assert!(hs.links_match("topic:书签"), "links 从原事件继承");
        // 揭示闭环（M3.1 判定）：听过秘密的人进知情集，第三个人依旧不知道
        assert!(
            proj.known_for("阿澈").contains(secret_path),
            "阿澈听过秘密 → 视角知情集应含该路径：{:?}",
            proj.known_for("阿澈")
        );
        assert!(
            !proj.known_for("小玲").contains(secret_path),
            "小玲没听到 → 不知情：{:?}",
            proj.known_for("小玲")
        );
        assert!(proj.known.is_empty(), "见证者揭示不进全局集：{:?}", proj.known);

        // 串台断言①（B 视角）：B4 出现带来源标注的转述，B3 深卡对她展开
        let b_assembly = simulate_turn_as(&root, &meta, &cast, "阿澈", &log, 3, "小雨，书签的事我听说了。");
        let b4 = b_assembly
            .layers
            .iter()
            .find(|l| l.id == "B4")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(b4.contains("猫头鹰"), "阿澈的 B4 应召回转述：{b4}");
        assert!(b4.contains("转述自小雨"), "转述要带来源标注：{b4}");
        assert!(b4.contains("0.40"), "渲染的显著度是折半后的 0.40：{b4}");
        let b3 = b_assembly
            .layers
            .iter()
            .find(|l| l.id == "B3")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(
            b3.contains("已故母亲"),
            "阿澈听过秘密 → 深卡应对他展开：{b3}"
        );

        // 串台断言②（C 视角）：转述与秘密都不出现在小玲的任何注入层
        let c_assembly = simulate_turn_as(&root, &meta, &cast, "小玲", &log, 4, "小玲：你们在聊什么呀？");
        let b4c = c_assembly
            .layers
            .iter()
            .find(|l| l.id == "B4")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(
            !b4c.contains("猫头鹰"),
            "串台：小玲没听到转述，B4 不得出现：{b4c}"
        );
        let b3c = c_assembly
            .layers
            .iter()
            .find(|l| l.id == "B3")
            .map(|l| l.content.clone())
            .unwrap_or_default();
        assert!(
            !b3c.contains("已故母亲"),
            "串台：秘密只对知情者展开：{b3c}"
        );
    }

    // M3.4 验收（commands 层）：发言权调度——点名提及优先、冷却轮换、
    // 调度事件落流可查且消息级重建不丢（设计 §10.5）。
    #[test]
    fn director_schedules_speakers_logs_and_survives_rebuild() {
        let plain = |name: &str, mes: &str| {
            format!(
                r#"return {{ spec='charcard/1.0', name='{name}', scenario='图书馆', personality='温柔', first_mes='{mes}' }}"#
            )
        };
        let (_dir, meta, root) = setup_cast3(
            &plain("小雨", "（开场）"),
            &plain("阿澈", "（阿澈入席）"),
            &plain("小玲", "（小玲入席）"),
        );
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        // ① 点名提及：指名小雨 → 小雨排第一（导演打分的强信号）；缺省每轮至多 2 人
        let proj = project_session(&log, &root, &meta).unwrap();
        let picks = director_plan(&meta, &cast, &proj, None, "小雨，你觉得呢？").unwrap();
        assert_eq!(picks[0].dir, "小雨");
        assert!(picks[0].reasons.iter().any(|r| r.contains("点名")));
        assert_eq!(picks.len(), 2, "每轮缺省至多 2 人接话：{picks:?}");
        assert!(picks[0].dir != picks[1].dir, "不打架：不重复点名");

        // ② 冷却轮换：小雨刚说完话、这轮没人被点名 → 她不开头（不冷场也不独占）
        simulate_turn_as(&root, &meta, &cast, "小雨", &log, 1, "（大家随意聊）");
        let proj = project_session(&log, &root, &meta).unwrap();
        let picks = director_plan(&meta, &cast, &proj, None, "（继续）").unwrap();
        assert_ne!(
            picks[0].dir, "小雨",
            "刚说过话的排后面（防独占）：{:?}",
            picks.iter().map(|p| p.dir.clone()).collect::<Vec<_>>()
        );

        // ③ 调度事件落流：时间线有人读摘要；消息级重建（丢派生事件）不丢调度史
        let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0) + 1;
        log.append(
            &root,
            &meta.id,
            LogBody::Director(event::DirectorEvent {
                turn,
                op: "schedule".into(),
                picks: picks
                    .iter()
                    .map(|p| event::DirectorPick {
                        dir: p.dir.clone(),
                        name: p.name.clone(),
                        score: p.score,
                        reasons: p.reasons.clone(),
                    })
                    .collect(),
                direct: None,
                note: None,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
        let entries = timeline_entries(&log, &root, &meta, Some(50)).unwrap();
        let brief = entries
            .iter()
            .find(|e| e["kind"] == "director")
            .map(|e| e["brief"].as_str().unwrap_or_default().to_string());
        let brief = brief.unwrap_or_default();
        assert!(brief.contains("调度"), "时间线应有调度摘要：{brief}");
        assert!(brief.contains("阿澈") || brief.contains("小玲"), "摘要应带发言人：{brief}");

        let records = log.read(&root, &meta.id).unwrap();
        let kept = truncate_turn(&records, turn, false);
        assert!(
            kept.iter()
                .any(|r| matches!(&r.body, LogBody::Director(_))),
            "调度史不随重建丢弃"
        );
        // fold 对 director 是 no-op：重建前后投影一致（可回放承诺，设计 §7.3-5）
        let before = event::project_over(&records, &event::Base::default());
        let after = event::project_over(&kept, &event::Base::default());
        assert_eq!(before.states, after.states);
        assert_eq!(before.blackboard, after.blackboard);
    }

    // M3.5 验收（设计 §9.2 / §15 DoD 3）：主动消息与意图动态——
    // 触发可溯源、外化开线、回流评价，全链路事件化可回放。

    /// 意图动态闭环（1v1）：阈值下不触发 → 剧情把意图推过阈值 → 轮末触发心里话
    /// （带触发记录）→ 下一轮 B5 注入「心里话」→ 她开口后队列消费、意图外化开线绑定。
    #[test]
    fn intent_loop_triggers_speaks_and_externalizes() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { psyche = { affects = {}, intents = { { name = '惦记着坦白', strength = 0.3 } } } },
  hooks = {
    on_message = function(msg, state, api)
      if msg.role == 'user' and msg.content:find('工作牌') then
        state.psyche.intents[1].strength = 0.8
      end
    end,
  },
}
"#;
        let (_dir, meta, root) = setup(card);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();

        let psyche_of = |root: &std::path::Path, meta: &store::SessionMeta| -> serde_json::Value {
            stored_state(root, meta)["psyche"].clone()
        };

        // ① 意图低于阈值（0.3 < 0.52 生效阈值）：轮末不触发，队列空
        simulate_turn(&root, &meta, &loaded, &log, 1, "（随便聊聊）");
        let psy = psyche_of(&root, &meta);
        assert!(psy["scheduled"].as_array().unwrap().is_empty(), "阈值下不触发：{psy}");
        assert!(psy["intents"][0]["triggered"].is_null());

        // ② 剧情把意图推过阈值 → 轮末触发：心里话入队 + 触发记录（哪轮/何阈值/多强/触发什么）
        simulate_turn(&root, &meta, &loaded, &log, 2, "工作牌的事，你还好吗？");
        let psy = psyche_of(&root, &meta);
        assert_eq!(
            psy["scheduled"][0]["text"], "惦记着坦白",
            "宿主把高强度意图转成心里话：{psy}"
        );
        assert_eq!(psy["scheduled"][0]["turn"], 2, "盖上憋下的轮次");
        let trig = &psy["intents"][0]["triggered"];
        assert_eq!(trig["turn"], 2);
        approx_f64(trig["threshold"].as_f64().unwrap(), 0.52, "默认气质生效阈值");
        approx_f64(trig["strength"].as_f64().unwrap(), 0.76, "0.8 过一轮衰减");
        assert!(trig["action"].as_str().unwrap().contains("惦记着坦白"));

        // ③ 下一轮：B5 注入「心里话」（主动消息的说话侧）
        let (a3, _) = simulate_turn(&root, &meta, &loaded, &log, 3, "嗯？");
        let b5 = a3
            .layers
            .iter()
            .find(|l| l.id == "B5" && l.name == "内心")
            .expect("B5「内心」层");
        assert!(b5.content.contains("心里话"), "B5 应带心里话：{}", b5.content);
        assert!(b5.content.contains("惦记着坦白"));

        // ④ 她开口（回复落盘）→ 队列消费 + 意图外化开线（origin=psyche，重建不丢）
        let psy = psyche_of(&root, &meta);
        assert!(psy["scheduled"].as_array().unwrap().is_empty(), "说出口即清空：{psy}");
        assert_eq!(
            psy["intents"][0]["linked_thread"], "thread.惦记着坦白",
            "意志外化为剧情线：{psy}"
        );
        let proj = project_session(&log, &root, &meta).unwrap();
        let opened = proj
            .thread_log
            .iter()
            .find(|t| t.op == threads::OP_OPEN && t.thread_id == "thread.惦记着坦白")
            .expect("外化开线事件在流");
        assert_eq!(opened.origin, threads::ORIGIN_PSYCHE);
        let t = threads::Thread::from_value(&opened.thread.clone().unwrap()).unwrap();
        assert!(t.is_active());
        assert_eq!(t.actors, vec!["小雨".to_string()], "线 actor 是开口的她");

        // ⑤ 触发过的意图不再重复触发（一条意图只催一次，不轰炸）
        assert_eq!(psy["intents"][0]["triggered"]["turn"], 2, "触发记录保持在第一次");
        assert!(psy["scheduled"].as_array().unwrap().is_empty());

        // ⑥ 消息级重建（丢派生事件）后重放：触发/消费/开线沿同一路径重演，投影一致
        let records = log.read(&root, &meta.id).unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &Cast::load(&root, &meta).unwrap(), &records, 3)
            .unwrap();
        let before = event::project_over(&records, &event::Base::default());
        let after = event::project_over(&rebuilt, &event::Base::default());
        assert_eq!(before.states, after.states, "重放长出同一份心理状态");
        assert_eq!(before.threads, after.threads, "外化开线在重放后不丢不重");
    }

    /// 群聊（M3.4 导演）：心里话队列记满票——憋着话要说的人在下一轮优先拿到发言权，
    /// 且她的组装带「心里话」；说出口后队列清空。
    #[test]
    fn scheduled_say_earns_the_floor_in_group_chat() {
        let plain = |name: &str, mes: &str| {
            format!(
                r#"return {{ spec='charcard/1.0', name='{name}', scenario='图书馆', personality='温柔', first_mes='{mes}' }}"#
            )
        };
        let ache = r#"
return {
  spec = 'charcard/1.0', name = '阿澈', scenario = '图书馆', personality = '安静', first_mes = '（阿澈入席）',
  hooks = {
    on_message = function(msg, state, api)
      if msg.content:find('秘密') then
        api.schedule_say('我想问问你工作牌的事。')
      end
    end,
  },
}
"#;
        let (_dir, meta, root) = setup_cast2(&plain("小雨", "（开场）"), ache);
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        // 第 1 轮阿澈接话，「秘密」触发他憋下一句心里话（on_message 在消费点之后跑，
        // 话留到下一轮——「让该角色在下一轮主动发消息」）
        simulate_turn_as(&root, &meta, &cast, "阿澈", &log, 1, "说个秘密吧");
        let proj = project_session(&log, &root, &meta).unwrap();
        let ache_psy = proj.states.get("阿澈").cloned().unwrap_or_default()["psyche"].clone();
        assert_eq!(
            ache_psy["scheduled"][0]["text"], "我想问问你工作牌的事。",
            "心里话入队：{ache_psy}"
        );

        // ② 下一轮导演调度：阿澈记满票（2.0），压过没有任何信号的小雨
        let proj = project_session(&log, &root, &meta).unwrap();
        let picks = director_plan(&meta, &cast, &proj, None, "（大家继续）").unwrap();
        assert_eq!(picks[0].dir, "阿澈", "憋着话的人优先拿发言权：{picks:?}");
        assert!(picks[0].reasons.iter().any(|r| r.contains("想说话")));

        // ③ 他的组装带「心里话」注入；回复落盘后队列消费（无意图 → 不开线）
        let a2 = simulate_turn_as(&root, &meta, &cast, "阿澈", &log, 2, "（继续）");
        let b5 = a2
            .layers
            .iter()
            .find(|l| l.id == "B5" && l.name == "内心")
            .expect("B5「内心」层");
        assert!(b5.content.contains("心里话"), "B5 应带心里话：{}", b5.content);
        assert!(b5.content.contains("工作牌"));
        let proj = project_session(&log, &root, &meta).unwrap();
        let ache_psy = proj.states.get("阿澈").cloned().unwrap_or_default()["psyche"].clone();
        assert!(ache_psy["scheduled"].as_array().unwrap().is_empty(), "说出口即清空：{ache_psy}");
        assert!(proj.thread_log.is_empty(), "没有可外化的意图就不开线");
    }

    /// 回流评价（M3.5 · 设计 §9.2「线被拒绝/了结 → 受挫情绪 + 相关意图削弱」）：
    /// 收线时绑定了这条线的意图削弱、意图主人记一条受挫情绪；没绑线的角色不受影响。
    #[test]
    fn resolving_linked_thread_refluxes_frustration() {
        let card = r#"
return {
  spec = 'charcard/1.0', name = '小雨', scenario = '图书馆', personality = '温柔', first_mes = '（开场）',
  state = { psyche = { affects = {}, intents = {
    { name = '想解释', strength = 0.8, linked_thread = 'thread.周五还书' },
    { name = '不相干的念头', strength = 0.6 },
  } } },
}
"#;
        let (_dir, meta, root) = setup(card);
        let log = store::EventLog::new();

        // 手动开一条线（id 与意图绑定的对上）
        open_thread_at(
            &log,
            &root,
            &meta,
            "周五还书",
            "借书卡的约定",
            &["小雨".to_string()],
            Some(0.6),
            threads::ORIGIN_MANUAL,
        )
        .unwrap();

        // 收线 → 回流
        let tree_cache = TreeCache::default();
        let runtime = SessionRuntime::default();
        let snapshot = resolve_thread_at(
            &log,
            &root,
            &meta,
            "thread.周五还书",
            "玩家如约还了书",
            &tree_cache,
            &runtime,
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        assert_eq!(snapshot["state"], threads::STATE_RESOLVED);

        let psy = stored_state(&root, &meta)["psyche"].clone();
        let affects = psy["affects"].as_array().unwrap();
        assert!(
            affects.iter().any(|a| a["name"] == "受挫"),
            "记一条受挫情绪：{psy}"
        );
        let hurt = affects.iter().find(|a| a["name"] == "受挫").unwrap();
        assert!(hurt["source"].as_str().unwrap().contains("周五还书"), "情绪可溯源到线：{hurt}");
        let intents = psy["intents"].as_array().unwrap();
        let explain = intents.iter().find(|i| i["name"] == "想解释").unwrap();
        approx_f64(explain["strength"].as_f64().unwrap(), 0.45, "0.8 − 0.35 = 0.45");
        let untouched = intents.iter().find(|i| i["name"] == "不相干的念头").unwrap();
        approx_f64(untouched["strength"].as_f64().unwrap(), 0.6, "没绑线的意图不动");

        // 收线驱动效果事件落流（psyche.reflow），可回放
        let proj = project_session(&log, &root, &meta).unwrap();
        // 回流事件的 trigger 记在 effect 里——投影不另设通道，检查器时间线可查
        let reflowed = log
            .read(&root, &meta.id)
            .unwrap()
            .iter()
            .filter_map(|r| match &r.body {
                LogBody::Effect(e) if e.trigger == "psyche.reflow" => Some(e),
                _ => None,
            })
            .count();
        assert!(reflowed >= 1, "应有 psyche.reflow 事件");
        let _ = proj;
    }

    /// 浮点断言（测试夹具；json 路径取出的 f64 与期望值比对）
    fn approx_f64(actual: f64, expected: f64, note: &str) {
        assert!(
            (actual - expected).abs() < 1e-3,
            "{note}：期望 {expected}，实际 {actual}"
        );
    }

    // M3.2 验收（commands 层）：消息级重建时，钩子的黑板写入按消息所在场景重演；
    // 场景事件（手动事件）在重建后原样保留。设计 §10.3 + §7.3-5。

    use super::*;
    use crate::event::SceneEvent;
    use crate::scene::{self, Scene};

    fn split_scene_event(turn: u64, from: &Scene, id: &str, title: &str, place: &str, moving: &[&str], ts: u64) -> LogBody {
        let (_rest, sc) = from
            .split_from(id, title, place, &moving.iter().map(|s| s.to_string()).collect::<Vec<_>>(), ts)
            .unwrap();
        LogBody::Scene(SceneEvent {
            turn,
            op: "split".into(),
            scene_id: id.into(),
            scene: Some(sc),
            others: vec![from.id.clone()],
            origin: "manual".into(),
            note: None,
            ts,
        })
    }

    #[test]
    fn rebuild_reroutes_hook_writes_to_their_scene() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let cast = single_cast(&meta, &loaded);
        let log = store::EventLog::new();

        // 分场：小雨离开 scene.main 另立 scene.b（main 留空不行——单角色会话，
        // 所以这里直接把缺省场景让给「书店」：把 main 的在场者改成玩家以外的人不行，
        // 退而求其次：新建场景并切过去（create 语义）。
        let proj = project_session(&log, &root, &meta).unwrap();
        let main = proj.scenes.get(scene::DEFAULT_SCENE_ID).cloned().unwrap();
        let ts = store::unix_now();
        let (_next, _sc) = {
            // 单角色没法 split（必须留一人），改走 create：书架另一头的「子场景」
            let sc = Scene {
                id: "scene.b".into(),
                title: "书店".into(),
                place: "旧书店".into(),
                actors: vec!["小雨".into()],
                day: main.day,
                clock: main.clock.clone(),
                flags: Default::default(),
                created_turn: 0,
                origin: "manual".into(),
                parent: Some(main.id.clone()),
                status: scene::STATUS_ACTIVE.into(),
                ts,
            };
            log.append(
                &root,
                &meta.id,
                LogBody::Scene(SceneEvent {
                    turn: 0,
                    op: "create".into(),
                    scene_id: sc.id.clone(),
                    scene: Some(sc),
                    others: Vec::new(),
                    origin: "manual".into(),
                    note: None,
                    ts,
                }),
            )
            .unwrap();
            log.append(
                &root,
                &meta.id,
                LogBody::Scene(SceneEvent {
                    turn: 0,
                    op: "switch".into(),
                    scene_id: "scene.b".into(),
                    scene: None,
                    others: Vec::new(),
                    origin: "manual".into(),
                    note: None,
                    ts,
                }),
            )
            .unwrap();
            ((), ())
        };

        // 在 scene.b 里对话：HOOK_CARD 的 on_message 收到「谢谢」写 place=天台
        log.append(
            &root,
            &meta.id,
            LogBody::Message(Message {
                turn: 1,
                role: "user".into(),
                content: "谢谢".into(),
                ts: 1,
                scene_id: Some("scene.b".into()),
                name: None,
                tool_calls: None,
            }),
        )
        .unwrap();
        let proj = project_session(&log, &root, &meta).unwrap();
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            "小雨",
            &proj.messages,
            &proj,
            Some("谢谢"),
            1,
            Some(&log),
            None,
            None,
            None,
            Some("scene.b"),
            &[],
        )
        .unwrap();
        let _ = run;
        run_message_hook_core(&root, &meta, &loaded, "小雨", 1, None, &log);

        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(
            proj.scenes["scene.b"].place, "天台",
            "钩子写入路由到消息所在场景"
        );
        assert_eq!(
            proj.scenes[scene::DEFAULT_SCENE_ID].place,
            "自习区",
            "缺省场景分区不被波及"
        );

        // 编辑那条消息 → 重建：场景事件保留，钩子按场景重演
        let records = log.read(&root, &meta.id).unwrap();
        let mut edited = records.as_ref().clone();
        if let LogBody::Message(m) = &mut edited
            .iter_mut()
            .find(|r| r.as_message().map(|m| m.role == "user").unwrap_or(false))
            .expect("应有用户消息")
            .body
        {
            m.content = "谢谢你啦".into();
        }
        let rebuilt = rebuild_from(&log, &root, &meta, &cast, &edited, 1).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(proj.scenes.len(), 2, "场景事件在重建后保留");
        assert_eq!(proj.active_scene.as_deref(), Some("scene.b"));
        assert_eq!(
            proj.scenes["scene.b"].place, "天台",
            "重建重演的钩子写入仍路由到 scene.b"
        );
        assert_eq!(
            proj.scenes[scene::DEFAULT_SCENE_ID].place,
            "自习区",
            "重建后缺省场景分区仍不波及"
        );
    }

    // ---------- M3.6 剧场模式与导演树（设计 §8.5/§10.5）----------

    const XY_PLAIN: &str =
        "return { spec='charcard/1.0', name='小雨', scenario='图书馆', personality='温柔', first_mes='（开场）' }";
    const AC_PLAIN: &str =
        "return { spec='charcard/1.0', name='阿澈', scenario='图书馆', personality='爽朗', first_mes='（入席）' }";

    /// 跑一轮剧场轮（不发 LLM）：用户消息 + 发言人回复 + 生产同一份轮末推进
    /// （finalize_turn：心理 → 状态树 → **剧场**；总结关闭）。
    /// 发言人取当前场景的第一位在场者（生产里由导演打分选出，语义同构：只在本场景选人）。
    fn theater_round(
        root: &std::path::Path,
        meta: &store::SessionMeta,
        cast: &Cast,
        log: &store::EventLog,
        turn: u64,
    ) {
        let proj = project_session(log, root, meta).unwrap();
        let scene = scene_ctx(&proj);
        let speaker = present_members(cast, &proj, scene.as_deref())
            .first()
            .unwrap_or_else(|| panic!("第 {turn} 轮没有任何在场角色"))
            .dir
            .clone();
        let mut msg = user_msg(turn, "（剧场继续）");
        msg.scene_id = scene.clone();
        log.append(root, &meta.id, LogBody::Message(msg)).unwrap();
        let mut report =
            commit_reply_core(root, meta, cast, &speaker, turn, "（回复）", &[], None, log, scene.as_deref())
                .unwrap();
        finalize_turn(root, meta, cast, turn, log, None, None, None, scene.as_deref(), &mut report)
            .unwrap();
    }

    fn theater_meta(budget: u32) -> Option<store::TheaterConfig> {
        Some(store::TheaterConfig { budget, start_turn: 0 })
    }

    /// DoD 第 2 项（单测侧）：剧场自动跑 20 轮，完成至少一次**完整的开线→收线弧**——
    /// 导演树起承转合走位齐全，导演开的线全部在「合」收束，转移历史可回放。
    #[test]
    fn theater_twenty_rounds_completes_a_full_arc() {
        let (_dir, mut meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        meta.theater = theater_meta(20);
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        for turn in 1..=20 {
            theater_round(&root, &meta, &cast, &log, turn);
        }

        let proj = project_session(&log, &root, &meta).unwrap();

        // ① 起承转合走位齐全；第一条是开场播种（from 为空）
        let stages: Vec<String> = proj
            .director_tree
            .iter()
            .map(|e| e.to.last().cloned().unwrap_or_default())
            .collect();
        assert_eq!(
            stages,
            vec!["起".to_string(), "承".to_string(), "转".to_string(), "合".to_string()],
            "导演树应在 20 轮内走完四段：{:?}",
            proj.director_tree
        );
        assert!(proj.director_tree[0].from.is_empty(), "第一条应是开场播种");
        assert_eq!(proj.director_tree[0].reason, "剧场开场");

        // ② 完整弧：起开主线、转开反转线（≥2 条导演开的线），且全部在合收束
        let opened: Vec<&event::ThreadEvent> = proj
            .thread_log
            .iter()
            .filter(|e| e.op == threads::OP_OPEN && e.origin == "director")
            .collect();
        assert!(
            opened.len() >= 2,
            "起与转各应开一条线：{:?}",
            opened.iter().map(|e| e.thread_id.clone()).collect::<Vec<_>>()
        );
        for e in &opened {
            let t = threads::Thread::from_value(&proj.threads[&e.thread_id]).unwrap();
            assert_eq!(
                t.state,
                threads::STATE_RESOLVED,
                "导演开的线「{}」应在合段收束",
                e.thread_id
            );
            assert!(t.resolution.is_some(), "收线结果应落在线上");
        }

        // ③ 转移历史可回放：导演树事件不随消息级重建丢弃（元层动作）
        let records = log.read(&root, &meta.id).unwrap();
        let kept: Vec<LogRecord> = records.iter().filter(|r| !r.is_derived()).cloned().collect();
        let tree_kept = kept.iter().filter(|r| matches!(r.body, LogBody::DirectorTree(_))).count();
        assert_eq!(tree_kept, proj.director_tree.len(), "走位史全部保留（is_derived=false）");
        let dir_threads_kept = kept
            .iter()
            .filter(|r| {
                matches!(&r.body, LogBody::Thread(e) if e.origin == "director")
            })
            .count();
        assert!(
            dir_threads_kept >= opened.len() * 2,
            "导演开/收线事件（origin=director）重建不丢：{dir_threads_kept}"
        );
    }

    /// 交叉剪辑（设计 §10.5）：两路场景按节奏轮换推进，「合」段合场后归于一路；
    /// 调度史（cut 事件）与切场事件（origin=director）落流可回放。
    #[test]
    fn intercut_rotates_between_two_scenes_and_merges_on_the_final_act() {
        let (_dir, mut meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        meta.theater = theater_meta(20);
        let log = store::EventLog::new();

        // 分场：阿澈离场另立旧书店（手动事件，重放保留）
        let proj = project_session(&log, &root, &meta).unwrap();
        let parent = proj.scenes[scene::DEFAULT_SCENE_ID].clone();
        let ts = store::unix_now();
        let (_rest, sc) = parent
            .split_from("scene.b", "旧书店", "坡下的旧书店", &["阿澈".to_string()], ts)
            .unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Scene(event::SceneEvent {
                turn: 0,
                op: "split".into(),
                scene_id: "scene.b".into(),
                scene: Some(sc),
                others: vec![parent.id.clone()],
                origin: "manual".into(),
                note: None,
                ts,
            }),
        )
        .unwrap();

        let cast = cast_of(&root, &meta);
        for turn in 1..=20 {
            theater_round(&root, &meta, &cast, &log, turn);
        }

        let proj = project_session(&log, &root, &meta).unwrap();

        // ① 交叉剪辑真的发生过：至少两次转场（节奏轮换），调度史带理由
        let records = log.read(&root, &meta.id).unwrap();
        let cuts: Vec<&event::DirectorEvent> = records
            .iter()
            .filter_map(|r| match &r.body {
                LogBody::Director(d) if d.op == "cut" => Some(d),
                _ => None,
            })
            .collect();
        assert!(cuts.len() >= 2, "两路场景应发生节奏轮换：{} 次", cuts.len());
        assert!(
            cuts.iter().all(|d| d.note.as_deref().map(|n| n.contains("交叉剪辑")).unwrap_or(false)),
            "转场调度应记录缘由：{:?}",
            cuts.iter().map(|d| d.note.clone()).collect::<Vec<_>>()
        );
        // 切场事件 origin=director（重建不丢），每次转场都有对应事件
        let switch_events = records
            .iter()
            .filter(|r| {
                matches!(&r.body, LogBody::Scene(s) if s.op == "switch" && s.origin == "director")
            })
            .count();
        assert_eq!(switch_events, cuts.len(), "每次转场都有对应切场事件");

        // ② 合段把两路并成一路：只剩一个 active 场景，另一路归档
        let active: Vec<&String> = proj
            .scenes
            .iter()
            .filter(|(_, sc)| sc.is_active())
            .map(|(id, _)| id)
            .collect();
        assert_eq!(active.len(), 1, "合场后只剩一路：{:?}", proj.scenes);
        let merged = proj.scenes.values().filter(|sc| sc.status == scene::STATUS_MERGED).count();
        assert_eq!(merged, 1, "被并入的场景归档留档");

        // ③ 走位史仍然完整（交叉剪辑不打断起承转合）
        let stages: Vec<String> = proj
            .director_tree
            .iter()
            .map(|e| e.to.last().cloned().unwrap_or_default())
            .collect();
        assert_eq!(
            stages,
            vec!["起".to_string(), "承".to_string(), "转".to_string(), "合".to_string()],
            "多场景下的弧完整性：{:?}",
            proj.director_tree
        );
    }

    /// 会话模板声明导演树（sessions/<id>/director.lua）+ 窗口调度权：
    /// 自定义树覆盖默认树，resurface 动作落 retune 事件（grade 调整），重建不丢。
    #[test]
    fn custom_director_tree_resurface_tunes_grade_and_survives_rebuild() {
        let (_dir, mut meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        // 自定义树：起→承→合；承的 on_exit 把手动开的线往后压（延后）
        std::fs::write(
            store::session_dir(&root, &meta.id).join("director.lua"),
            concat!(
                "return { state_tree = {\n",
                "  root = '起',\n",
                "  states = {\n",
                "    ['起'] = {\n",
                "      transitions = { { to = '承', priority = 10,\n",
                "        when = function(ev, bb, st) return st.stage_turns >= 1 end } },\n",
                "    },\n",
                "    ['承'] = {\n",
                "      has_exit = true,\n",
                "      on_exit = function(api) api.resurface('thread.周五还书', 'later') end,\n",
                "      transitions = { { to = '合', priority = 10,\n",
                "        when = function(ev, bb, st) return st.stage_turns >= 1 end } },\n",
                "    },\n",
                "    ['合'] = {\n",
                "      has_enter = true,\n",
                "      on_enter = function(api) api.resolve_threads(nil, '收。') end,\n",
                "    },\n",
                "  },\n",
                "} }\n",
            ),
        )
        .unwrap();
        // 视图应报告自定义树
        let log = store::EventLog::new();
        let view = theater_view_of(&log, &root, &meta).unwrap();
        assert!(view.custom_tree, "应识别会话自带的 director.lua");
        assert_eq!(view.path, vec!["起".to_string()], "没开场时路径 = 树根");

        // 手动开一条线（natural），剧场开着跑三轮：起 → 承 →（承 on_exit 调窗）合
        meta.theater = theater_meta(20);
        let cast = cast_of(&root, &meta);
        open_thread_at(
            &log,
            &root,
            &meta,
            "周五还书",
            "借书卡的约定",
            &["小雨".to_string()],
            Some(0.6),
            threads::ORIGIN_MANUAL,
        )
        .unwrap();
        for turn in 1..=3 {
            theater_round(&root, &meta, &cast, &log, turn);
        }

        let proj = project_session(&log, &root, &meta).unwrap();
        let stages: Vec<String> = proj
            .director_tree
            .iter()
            .map(|e| e.to.last().cloned().unwrap_or_default())
            .collect();
        assert_eq!(stages, vec!["起".to_string(), "承".to_string(), "合".to_string()]);
        // 调窗生效：grade 降到 dormant，但线**不被**收束（manual 开的线不动）
        let t = threads::Thread::from_value(&proj.threads["thread.周五还书"]).unwrap();
        assert_eq!(t.resurface.grade, threads::GRADE_DORMANT, "延后 = dormant");
        assert_eq!(t.state, threads::STATE_ACTIVE, "导演收束只动自己开的线");
        // retune 事件落流（origin=director）
        assert!(
            log.read(&root, &meta.id)
                .unwrap()
                .iter()
                .any(|r| matches!(&r.body, LogBody::Thread(e) if e.op == threads::OP_RETUNE && e.origin == "director")),
            "调窗应落 retune 事件"
        );

        // 重建（丢派生、保留元层动作）后：走位史与调窗结果原样
        let records = log.read(&root, &meta.id).unwrap();
        let kept: Vec<LogRecord> = records.iter().filter(|r| !r.is_derived()).cloned().collect();
        let rebuilt = rebuild_from(&log, &root, &meta, &cast, &kept, 1).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        let proj2 = project_session(&log, &root, &meta).unwrap();
        assert_eq!(
            proj.director_tree, proj2.director_tree,
            "走位史重建不丢、不翻倍"
        );
        let t2 = threads::Thread::from_value(&proj2.threads["thread.周五还书"]).unwrap();
        assert_eq!(t2.resurface.grade, threads::GRADE_DORMANT, "调窗结果重建保留");
    }

    // ---------- M3.7 世界主线与世界时钟（设计 §6.6）----------

    /// 设计 §6.6 的示例主线：传闻期 →(day≥3) 公告期，进公告期揭示世界设定 + 开世界级线
    const WORLDLINE_LUA: &str = r#"
return {
  id = "worldline.图书馆拆迁",
  premise = "老图书馆月底拆除，所有人都在倒数。",
  stages = {
    { id = "传闻期", directive = "日常氛围，偶尔可闻拆迁传闻，多数人不在意。" },
    { id = "公告期", when = { day = 3 },
      directive = "公告已贴出，空气里有告别的味道；各角色心怀不同的盘算。",
      on_enter = {
        reveal = { "rule.拆迁公告" },
        open_thread = { id = "thread.最后一个月", title = "最后一个月",
                        cause = "世界大势：闭馆倒计时开始。", importance = 0.8 },
      } },
  },
  world_threads = { "thread.最后一个月" },
}
"#;

    /// 把会话故事时钟拨到指定天（manual 黑板事件，重放保留）
    fn bump_day(log: &store::EventLog, root: &std::path::Path, meta: &store::SessionMeta, day: i64) {
        let proj = project_session(log, root, meta).unwrap();
        let mut board = proj.effective_board(None);
        board.day = day;
        log.append(
            root,
            &meta.id,
            LogBody::Blackboard(event::BlackboardEvent {
                turn: proj.last_message().map(|m| m.turn).unwrap_or(0),
                reason: "manual".into(),
                scene_id: None,
                board,
                ts: store::unix_now(),
            }),
        )
        .unwrap();
    }

    fn worldline_stages(proj: &event::Projection) -> Vec<String> {
        proj.worldline
            .iter()
            .map(|e| e.to.last().cloned().unwrap_or_default())
            .collect()
    }

    /// 组装产物里取某层正文
    fn layer_of<'a>(run: &'a PromptRun, id: &str) -> &'a str {
        &run.assembly
            .layers
            .iter()
            .find(|l| l.id == id)
            .expect("应存在该注入层")
            .content
    }

    /// DoD 第 6 项（单测侧）：世界主线阶段转移落事件、揭示与开世界级线可溯源、
    /// 时代行与世界 directive 进注入、进度回写 world.json。
    #[test]
    fn worldline_stage_transition_reveals_opens_and_injects() {
        let (_dir, meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        std::fs::create_dir_all(root.join("codex/default")).unwrap();
        std::fs::write(root.join("codex/default/worldline.lua"), WORLDLINE_LUA).unwrap();
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        // 第 1 轮：播种「传闻期」（承袭世界，不跑钩子）；day=1 不过门槛
        theater_round(&root, &meta, &cast, &log, 1);
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(worldline_stages(&proj), vec!["传闻期".to_string()]);
        assert!(proj.known.is_empty(), "承袭不重跑 on_enter");
        let world = store::load_world(&root, "default");
        assert_eq!(world.day, 1, "轮末回写已建立世界时钟基线");
        assert_eq!(
            world.worldline.as_ref().map(|p| p.path.clone()),
            Some(vec!["传闻期".to_string()]),
            "主线进度回写 world.json"
        );

        // 拨到第 3 天再来一轮：公告期门槛越过 → 转移 + reveal + 开世界级线
        bump_day(&log, &root, &meta, 3);
        theater_round(&root, &meta, &cast, &log, 2);
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(
            worldline_stages(&proj),
            vec!["传闻期".to_string(), "公告期".to_string()]
        );
        assert!(
            proj.known.contains("rule.拆迁公告"),
            "阶段揭示无见证者 = 全局知情"
        );
        let t = threads::Thread::from_value(&proj.threads["thread.最后一个月"]).unwrap();
        assert_eq!(t.scope, threads::SCOPE_WORLD, "主线开的线是世界级线");

        // world.json：时钟与进度双回写、世界级线入档
        let world = store::load_world(&root, "default");
        assert_eq!(world.day, 3);
        assert_eq!(
            world.worldline.as_ref().map(|p| p.path.clone()),
            Some(vec!["传闻期".to_string(), "公告期".to_string()])
        );
        assert!(
            world.has_thread("thread.最后一个月"),
            "世界级线回写 world.json（别的会话可推进）"
        );

        // 注入：B1 时代行 + B2 世界 directive（无角色状态树时 B2 只有大势）
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            "小雨",
            &proj.messages,
            &proj,
            Some("（继续）"),
            2,
            None,
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        let b1 = layer_of(&run, "B1");
        assert!(b1.contains("时代:公告期"), "B1 应有时代行：{b1}");
        let b2 = layer_of(&run, "B2");
        assert!(
            b2.contains("公告已贴出"),
            "B2 应含世界 directive（大势压着小情绪）：{b2}"
        );

        // 重建（消息级）：worldline 事件是元层动作，走位史不丢
        let records = log.read(&root, &meta.id).unwrap();
        let kept: Vec<LogRecord> = records.iter().filter(|r| !r.is_derived()).cloned().collect();
        let rebuilt = rebuild_from(&log, &root, &meta, &cast, &kept, 1).unwrap();
        log.rewrite(&root, &meta.id, &rebuilt).unwrap();
        let proj2 = project_session(&log, &root, &meta).unwrap();
        assert_eq!(proj.worldline, proj2.worldline, "走位史重建不丢");
        assert!(proj2.known.contains("rule.拆迁公告"), "揭示不因重建丢失");
    }

    /// DoD 第 6 项：跨会话持久——新会话承袭世界进度（阶段与线），flashback 不拉低时钟。
    #[test]
    fn world_state_persists_across_sessions_and_flashback_never_regresses() {
        let (_dir, meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        std::fs::create_dir_all(root.join("codex/default")).unwrap();
        std::fs::write(root.join("codex/default/worldline.lua"), WORLDLINE_LUA).unwrap();
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        // 会话 A：走到公告期（第 3 天）
        theater_round(&root, &meta, &cast, &log, 1);
        bump_day(&log, &root, &meta, 3);
        theater_round(&root, &meta, &cast, &log, 2);

        // 会话 B（同一世界，无显式天）：开局即续上大势
        //（基准 = baseline_day_from_world，new_session 命令内部走的就是它）
        let meta_b = store::new_session(
            &root,
            &store::NewSessionRequest {
                character: "小雨".into(),
                characters: vec!["小雨".into()],
                persona: None,
                day: baseline_day_from_world(&root, None),
                clock: Some("09:00".into()),
                place: Some("公告栏".into()),
                premise: None,
            },
        )
        .unwrap();
        assert_eq!(
            store::load_blackboard(&root, &meta_b.id).unwrap().day,
            3,
            "新会话缺省从世界时钟出发"
        );
        let board_b = store::load_blackboard(&root, &meta_b.id).unwrap();
        let log_b = store::EventLog::new();
        log_b
            .append(
                &root,
                &meta_b.id,
                LogBody::Blackboard(event::BlackboardEvent {
                    turn: 0,
                    reason: "init".into(),
                    scene_id: None,
                    board: board_b,
                    ts: store::unix_now(),
                }),
            )
            .unwrap();
        let cast_b = cast_of(&root, &meta_b);
        theater_round(&root, &meta_b, &cast_b, &log_b, 1);

        // 承袭：不重跑 on_enter（揭示/开线已在 A 发生过），但视图与注入都在公告期
        let proj_b = project_session(&log_b, &root, &meta_b).unwrap();
        assert_eq!(worldline_stages(&proj_b), vec!["公告期".to_string()]);
        assert!(
            !proj_b.threads.contains_key("thread.最后一个月"),
            "承袭不重演开线——线住 world.json，不随会话重放"
        );
        let view = worldline_view_of(&log_b, &root, &meta_b).unwrap();
        assert_eq!(view.stage, "公告期");
        assert_eq!(view.world_day, 3);
        assert!(
            view.world_threads
                .iter()
                .any(|t| t["id"] == "thread.最后一个月"),
            "世界级线从 world.json 并入任何会话的视图"
        );

        // flashback 会话（显式回到第 1 天）结束后，世界时钟不被拉低
        let meta_c = store::new_session(
            &root,
            &store::NewSessionRequest {
                character: "小雨".into(),
                characters: vec!["小雨".into()],
                persona: None,
                day: baseline_day_from_world(&root, Some(1)),
                clock: Some("09:00".into()),
                place: Some("回忆里的自习区".into()),
                premise: None,
            },
        )
        .unwrap();
        let board_c = store::load_blackboard(&root, &meta_c.id).unwrap();
        let log_c = store::EventLog::new();
        log_c
            .append(
                &root,
                &meta_c.id,
                LogBody::Blackboard(event::BlackboardEvent {
                    turn: 0,
                    reason: "init".into(),
                    scene_id: None,
                    board: board_c,
                    ts: store::unix_now(),
                }),
            )
            .unwrap();
        let cast_c = cast_of(&root, &meta_c);
        theater_round(&root, &meta_c, &cast_c, &log_c, 1);
        let world = store::load_world(&root, "default");
        assert_eq!(world.day, 3, "flashback 不拉低世界时钟");
        assert_eq!(
            world.worldline.as_ref().map(|p| p.path.clone()),
            Some(vec!["传闻期".to_string(), "公告期".to_string()]),
            "更浅的进度不覆盖世界"
        );

        // 基准函数的直接点验：显式天优先、无世界文件回 None
        assert_eq!(baseline_day_from_world(&root, Some(7)), Some(7));
        assert_eq!(baseline_day_from_world(&root, None), Some(3));
    }

    /// 可选层纪律：没有 worldline.lua 的世界一切照旧——无走位、无时代行、无世界段，
    /// 但世界时钟照常回写（时钟独立于主线存在）。
    #[test]
    fn without_worldline_the_world_still_runs_and_clocks_persist() {
        let (_dir, meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        theater_round(&root, &meta, &cast, &log, 1);
        let proj = project_session(&log, &root, &meta).unwrap();
        assert!(proj.worldline.is_empty(), "无主线就没有走位史");

        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            "小雨",
            &proj.messages,
            &proj,
            Some("（继续）"),
            1,
            None,
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(!layer_of(&run, "B1").contains("时代:"), "无主线就没有时代行");
        assert!(
            run.assembly.layers.iter().all(|l| l.id != "B2"),
            "无主线且无状态树时 B2 整层省略"
        );

        bump_day(&log, &root, &meta, 9);
        theater_round(&root, &meta, &cast, &log, 2);
        let world = store::load_world(&root, "default");
        assert_eq!(world.day, 9, "世界时钟独立于主线照常回写");
        assert!(world.worldline.is_none());
    }

    /// M3.6 遗留联动：导演树判据环境补 `worldline_stage`——「公告期不排纯搞笑日常」
    /// 写成一条 when 即可（母层查询子层；主线先推进，剧场后求值）。
    #[test]
    fn director_tree_judges_on_the_worldline_stage() {
        let (_dir, mut meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        std::fs::create_dir_all(root.join("codex/default")).unwrap();
        std::fs::write(root.join("codex/default/worldline.lua"), WORLDLINE_LUA).unwrap();
        std::fs::write(
            store::session_dir(&root, &meta.id).join("director.lua"),
            r#"
return { state_tree = {
  root = "起",
  states = {
    ["起"] = {
      directive = "起：铺陈。",
      transitions = {
        { to = "转", priority = 10,
          when = function(ev, bb, st) return st.worldline_stage == "公告期" end },
      },
    },
    ["转"] = { directive = "转：大势压顶。" },
  },
} }
"#,
        )
        .unwrap();
        meta.theater = theater_meta(20);
        let log = store::EventLog::new();
        let cast = cast_of(&root, &meta);

        // 第 1 轮：导演播种「起」，主线承袭「传闻期」（不满足转段判据）
        theater_round(&root, &meta, &cast, &log, 1);
        bump_day(&log, &root, &meta, 3);
        // 第 2 轮：主线先进公告期 → 剧场求值时 worldline_stage 已是公告期 → 转
        theater_round(&root, &meta, &cast, &log, 2);

        let proj = project_session(&log, &root, &meta).unwrap();
        let stages: Vec<String> = proj
            .director_tree
            .iter()
            .map(|e| e.to.last().cloned().unwrap_or_default())
            .collect();
        assert_eq!(
            stages,
            vec!["起".to_string(), "转".to_string()],
            "导演树应读到主线阶段并转段（自定义树无预算压力，靠的就是联动）：{stages:?}"
        );
    }

    /// 解析预览（设计 §6.5 收尾）：按故事时钟回答「第 N 天的事实」——
    /// versions 生效切换、生命周期生效前仍在场（flashback 里逝者可对话）、retired 留档可查。
    #[test]
    fn resolve_preview_serves_versions_and_lifecycle_by_story_clock() {
        let (_dir, meta, root) = setup_cast2(XY_PLAIN, AC_PLAIN);
        std::fs::create_dir_all(root.join("codex/default/entities")).unwrap();
        std::fs::write(
            root.join("codex/default/entities/preview.json"),
            serde_json::json!({
                "id": "char.小雨",
                "type": "char",
                "name": "小雨",
                "one_liner": "长发的小雨。",
                "versions": [
                    { "day": 15, "facet": "one_liner", "value": "剪了短发的小雨。", "note": "第15天剪发" }
                ],
                "lifecycle": { "status": "dead", "at_day": 20, "note": "第20天病故" }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("codex/default/entities/retired.json"),
            serde_json::json!({
                "id": "place.旧书店",
                "type": "place",
                "name": "旧书店",
                "one_liner": "已经关门的旧书店。",
                "status": "retired"
            })
            .to_string(),
        )
        .unwrap();
        let log = store::EventLog::new();
        let cache = CodexCache::default();

        let early = codex_resolve_preview_of(&log, &cache, &root, &meta.id, Some(10)).unwrap();
        let e = early.entities.iter().find(|e| e.id == "char.小雨").unwrap();
        assert_eq!(e.one_liner, "长发的小雨。", "第 10 天取旧版事实");
        assert_eq!(e.lifecycle["present"], serde_json::json!(true), "死亡生效前仍在场");
        assert_eq!(e.versions[0]["active"], serde_json::json!(false));

        let late = codex_resolve_preview_of(&log, &cache, &root, &meta.id, Some(25)).unwrap();
        let e = late.entities.iter().find(|e| e.id == "char.小雨").unwrap();
        assert_eq!(e.one_liner, "剪了短发的小雨。", "第 25 天取新版事实");
        assert_eq!(e.lifecycle["in_effect"], serde_json::json!(true));
        assert_eq!(e.lifecycle["present"], serde_json::json!(false), "生效后不在场");
        assert_eq!(e.versions[0]["active"], serde_json::json!(true));
        assert_eq!(e.versions[0]["note"], serde_json::json!("第15天剪发"));

        // retired 留档可查：预览照常列出（状态本身带 retired）
        assert!(
            late.entities.iter().any(|e| e.id == "place.旧书店" && e.status == "retired"),
            "retired 是留档不是删除"
        );

        // day 缺省 = 会话当前故事天
        let default_day = codex_resolve_preview_of(&log, &cache, &root, &meta.id, None).unwrap();
        assert_eq!(default_day.day, 1);
    }

    // ---------- M3.8 设定补全管线（设计 §6.8）----------

    /// 往世界 default 的实体目录写一个最小 char 实体（分级/物化测试的靶子）
    fn seed_entity(root: &std::path::Path, id: &str, extra: serde_json::Value) {
        let dir = codex_entities_dir(root, "default");
        std::fs::create_dir_all(&dir).unwrap();
        let mut obj = serde_json::json!({
            "id": id,
            "type": id.split('.').next().unwrap_or("char"),
            "name": id.split('.').nth(1).unwrap_or(id),
            "one_liner": "测试实体。",
        });
        if let (Some(dst), Some(src)) = (obj.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        std::fs::write(dir.join(format!("{id}.json")), serde_json::to_string(&obj).unwrap()).unwrap();
    }

    /// DoD 8「确认写正史进注入」：确认的 codex 提案物化进 grown.json，
    /// 下一次 load_codex（同一世界任何会话）都能读到
    #[test]
    fn accepted_codex_proposal_materializes_into_grown_history() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        seed_entity(&root, "char.小雨", serde_json::json!({ "facts": { "schedule": "夜班" } }));
        let log = store::EventLog::new();

        // 管线提案（新事实 + 全新实体）进事件流
        log.append(
            &root,
            &meta.id,
            LogBody::Proposal(event::ProposalEvent {
                turn: 2,
                id: "codex.char.小雨.2.0".into(),
                op: "propose".into(),
                kind: "new_fact".into(),
                origin: "pipeline".into(),
                payload: Some(serde_json::json!({
                    "target": "char.小雨",
                    "value": { "facet": "schedule", "value": "周三也休息" },
                    "reason": "第 2 轮提到"
                })),
                note: None,
                ts: 0,
            }),
        )
        .unwrap();
        log.append(
            &root,
            &meta.id,
            LogBody::Proposal(event::ProposalEvent {
                turn: 2,
                id: "codex.char.墨墨.2.1".into(),
                op: "propose".into(),
                kind: "new_entity".into(),
                origin: "pipeline".into(),
                payload: Some(serde_json::json!({
                    "target": "char.墨墨",
                    "value": { "type": "char", "name": "墨墨", "facts": { "look": { "impression": "一只黑猫" } } },
                    "reason": "第 2 轮即兴发明"
                })),
                note: None,
                ts: 0,
            }),
        )
        .unwrap();

        // 全部确认（批量路径与单条共用物化内核）
        let n = decide_all_proposals_core(&log, &root, &meta.id, true).unwrap();
        assert_eq!(n, 2);

        // 正史增量落了文件，且 load_codex 能读到（「进注入」）
        let grown = store::load_grown(&root, "default");
        assert!(grown.entities.contains_key("char.小雨"));
        assert!(grown.entities.contains_key("char.墨墨"));
        let cx = load_codex(&root, None, "default");
        assert_eq!(
            codex::static_fact(cx.get("char.小雨").unwrap(), "schedule"),
            Some(&serde_json::json!("周三也休息")),
            "确认的新事实覆盖了原值"
        );
        assert!(cx.get("char.墨墨").is_some(), "确认的新实体进了设定集");
        assert_eq!(cx.get("char.墨墨").unwrap().status, "canon");

        // 手写实体文件不被机器改写：磁盘上的原文件还是夜班
        let raw = std::fs::read_to_string(codex_entities_dir(&root, "default").join("char.小雨.json"))
            .unwrap();
        assert!(raw.contains("夜班"), "增量住在 grown.json，不动手写文件");

        // 重建（编辑历史）不丢确认动作：提案事件不是派生产物
        let records = log.read(&root, &meta.id).unwrap();
        let loaded = card::load_card(&root, "小雨").unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &single_cast(&meta, &loaded), &records, 1)
            .unwrap();
        assert!(rebuilt.iter().any(|r| matches!(&r.body, LogBody::Proposal(p) if p.op == "accept")));

        // 幂等重放：重复确认一条已确认的提案不再改 grown（状态机后写覆盖，物化只发生一次）
        let grown_before = store::load_grown(&root, "default");
        decide_proposal_core(&log, &root, &meta.id, "codex.char.小雨.2.0".into(), true, None).unwrap();
        assert_eq!(store::load_grown(&root, "default"), grown_before);
    }

    /// §6.8-2 运行期捕获分级：瞬时状态直接写黑板、小事实按配置自动接受、
    /// 全新实体必须人工
    #[test]
    fn runtime_capture_grades_split_transient_minor_and_new_entity() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        seed_entity(&root, "char.小雨", serde_json::json!({ "facts": { "schedule": "夜班" } }));
        let log = store::EventLog::new();
        let cx = load_codex(&root, None, "default");

        let outcome = summarize::SummaryOutcome {
            codex: vec![
                summarize::CodexDraft {
                    kind: "transient".into(),
                    target: String::new(),
                    value: serde_json::json!({ "key": "天气", "value": "雨渐大" }),
                    reason: "第 3 轮".into(),
                },
                summarize::CodexDraft {
                    kind: "new_fact".into(),
                    target: "char.小雨".into(),
                    value: serde_json::json!({ "facet": "schedule", "value": "周三休息" }),
                    reason: "第 3 轮提到".into(),
                },
                summarize::CodexDraft {
                    kind: "new_entity".into(),
                    target: "char.墨墨".into(),
                    value: serde_json::json!({ "type": "char", "name": "墨墨" }),
                    reason: "第 3 轮即兴发明".into(),
                },
            ],
            ..summarize::SummaryOutcome::default()
        };

        // 缺省（不自动接受）：transient → 黑板；小事实与全新实体都进收件箱待审
        let proj = project_session(&log, &root, &meta).unwrap();
        let applied = applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj, outcome, 2, 3, 1, "20:00", "scene.main",
        ));
        assert_eq!(applied, 3);
        let proj = project_session(&log, &root, &meta).unwrap();
        let sc = proj.scenes.get("scene.main").expect("genesis 场景");
        assert_eq!(sc.flags.get("天气"), Some(&serde_json::json!("雨渐大")), "瞬时状态写进场景 flags");
        assert!(
            proj.blackboard.as_ref().map(|b| b.extra.contains_key("天气")).unwrap_or(false)
                || sc.flags.contains_key("天气"),
            "世界层镜像或场景分区至少一处可见"
        );
        assert_eq!(
            proj.proposals.get("codex.char.小雨.3.1").unwrap()["status"],
            "propose",
            "小事实缺省仍要人工"
        );
        assert_eq!(
            proj.proposals.get("codex.char.墨墨.3.2").unwrap()["status"],
            "propose",
            "全新实体必须人工"
        );

        // 开「自动接受小事实」：小事实连落 propose+accept 并物化；全新实体照旧人工
        let mut settings = store::load_settings(&root).unwrap();
        settings.auto_accept_minor_facts = true;
        store::save_settings(&root, &settings).unwrap();
        let outcome2 = summarize::SummaryOutcome {
            codex: vec![
                summarize::CodexDraft {
                    kind: "new_fact".into(),
                    target: "char.小雨".into(),
                    value: serde_json::json!({ "facet": "schedule", "value": "周五休息" }),
                    reason: "第 5 轮改口".into(),
                },
                summarize::CodexDraft {
                    kind: "new_entity".into(),
                    target: "char.豆豆".into(),
                    value: serde_json::json!({ "type": "char", "name": "豆豆" }),
                    reason: "第 5 轮即兴发明".into(),
                },
            ],
            ..summarize::SummaryOutcome::default()
        };
        let proj = project_session(&log, &root, &meta).unwrap();
        applied_n(apply_summary_outcome(
            &log, &root, &meta, &cx, &proj, outcome2, 4, 5, 1, "21:00", "scene.main",
        ));
        let proj = project_session(&log, &root, &meta).unwrap();
        assert_eq!(
            proj.proposals.get("codex.char.小雨.5.0").unwrap()["status"],
            "accept",
            "小事实自动接受"
        );
        assert_eq!(
            proj.proposals.get("codex.char.豆豆.5.1").unwrap()["status"],
            "propose",
            "全新实体永远人工"
        );
        let cx = load_codex(&root, None, "default");
        assert_eq!(
            codex::static_fact(cx.get("char.小雨").unwrap(), "schedule"),
            Some(&serde_json::json!("周五休息")),
            "自动接受的小事实已进注入层"
        );
        assert!(cx.get("char.豆豆").is_none(), "没确认的新实体不进注入");
    }

    /// §6.8-4 即兴模式：improv 提案当轮回读进 B2（带「设定·暂定」标记），
    /// 重建不丢、重放同一行——注入不随模型漂移
    #[test]
    fn improv_proposal_reenters_b2_and_survives_rebuild() {
        let (_dir, meta, root) = setup(HOOK_CARD);
        let loaded = card::load_card(&root, "小雨").unwrap();
        let log = store::EventLog::new();
        simulate_turn(&root, &meta, &loaded, &log, 1, "你好呀。");

        // 模拟 maybe_improv 的落流产物（LLM 调用本身不在单测覆盖面）：
        // 本轮即兴补了一条暂定设定
        log.append(
            &root,
            &meta.id,
            LogBody::Proposal(event::ProposalEvent {
                turn: 2,
                id: "improv.char.小雨.2".into(),
                op: "propose".into(),
                kind: "new_fact".into(),
                origin: "improv".into(),
                payload: Some(serde_json::json!({
                    "target": "char.小雨",
                    "value": { "facet": "facts.日常", "value": "养了一只叫墨墨的猫" },
                    "text": "她养了一只叫墨墨的猫。",
                    "provisional": true,
                    "reason": "第 2 轮即兴补一条暂定设定"
                })),
                note: None,
                ts: 0,
            }),
        )
        .unwrap();

        // 第 2 轮组装：B2 带「设定·暂定」行
        let meta2 = meta.clone();
        let dir = first_character(&meta2).unwrap();
        let cast = Cast { members: vec![CastMember { dir, loaded: loaded.clone() }] };
        let proj = project_session(&log, &root, &meta).unwrap();
        let history = proj.messages.clone();
        let run = assemble_prompt_core(
            &noop_sink(),
            &root,
            &meta,
            &cast,
            &cast.first().dir,
            &history,
            &proj,
            Some("猫最近怎么样？"),
            2,
            Some(&log),
            None,
            None,
            None,
            None,
            &[],
        )
        .unwrap();
        let b2 = run
            .assembly
            .layers
            .iter()
            .find(|l| l.id == "B2")
            .expect("B2 层");
        assert!(b2.content.contains("设定·暂定"), "{}", b2.content);
        assert!(b2.content.contains("墨墨"), "{}", b2.content);

        // 编辑重建：improv 提案不是派生产物，保留——重放同一轮，同一行还在
        let records = log.read(&root, &meta.id).unwrap();
        let rebuilt = rebuild_from(&log, &root, &meta, &cast, &records, 1).unwrap();
        assert!(
            rebuilt
                .iter()
                .any(|r| matches!(&r.body, LogBody::Proposal(p) if p.origin == "improv" && p.op == "propose")),
            "improv 提案重建不丢"
        );
        let _ = history;
    }

    // M3.9 验收（commands 层）：草稿包落盘——卡、正史增量、世界线三路产物；
    // 重复导入被查重拦住；切入点切面烘焙秘密知情集。设计 §6.7。

    /// 一份可直接 commit 的最小草稿包（爱莉希雅 wiki 样例的浓缩形态）
    fn sample_pack(world: &str) -> ingest::IngestPack {
        let secrets = vec![ingest::SecretDraft {
            key: "律者".into(),
            content: "实为人之律者".into(),
            revealed_by: Some("finale".into()),
            known_by_advice: vec!["爱莉希雅".into()],
            source: None,
            include: true,
            origin: "llm".into(),
        }];
        let mut pack = ingest::IngestPack {
            world: world.into(),
            char_id: "char.爱莉希雅".into(),
            card: ingest::CardSide {
                first_mes: "你好呀，我是爱莉希雅♪".into(),
                scenario: "前文明纪，终焉倒计时".into(),
                personality: "动机：让所有人被温柔以待".into(),
                tags: vec!["素材规格化".into()],
                example_dialogue: vec![card::ExampleTurn {
                    tag: Some("初见".into()),
                    messages: vec![card::ExampleLine {
                        role: "char".into(),
                        content: "要像花一样绽放哦♪".into(),
                    }],
                }],
                sources: BTreeMap::new(),
            },
            entity: ingest::EntityDraft {
                id: "char.爱莉希雅".into(),
                ty: "char".into(),
                name: "爱莉希雅".into(),
                aliases: vec!["人之律者".into()],
                one_liner: "笑起来像花的逐火战士。".into(),
                facts: BTreeMap::from([
                    (
                        "look".into(),
                        serde_json::json!({ "anchors": ["发间的花"], "impression": "粉色长发" }),
                    ),
                    ("motivation".into(), serde_json::json!("让所有人被温柔以待")),
                ]),
                relations: vec![codex::Relation {
                    to: "org.逐火十三英桀".into(),
                    kind: "所属".into(),
                    always_with: true,
                }],
                sources: BTreeMap::new(),
                include: true,
                stub: false,
            },
            others: vec![ingest::EntityDraft {
                id: "org.逐火十三英桀".into(),
                ty: "org".into(),
                name: "逐火十三英桀".into(),
                aliases: vec![],
                one_liner: String::new(),
                facts: BTreeMap::new(),
                relations: vec![],
                sources: BTreeMap::new(),
                include: true,
                stub: true,
            }],
            events: vec![],
            secrets,
            lifecycle: Some(ingest::LifecycleDraft {
                status: "dead".into(),
                at_day: 30,
                note: Some("终焉之战".into()),
                source: None,
            }),
            versions: vec![],
            worldline: Some(ingest::WorldlineDraft {
                id: "main".into(),
                premise: "前文明纪，终焉倒计时".into(),
                stages: vec![
                    ingest::StageDraft { id: "opening".into(), name: String::new(), day: 1, directive: "日常的延续".into(), include: true },
                    ingest::StageDraft { id: "finale".into(), name: String::new(), day: 30, directive: "最后的战斗".into(), include: true },
                ],
            }),
            canon_points: vec![
                ingest::CanonPoint { name: "opening".into(), day: 1, stage: Some("opening".into()), note: String::new(), after_death: false, premise: None },
                ingest::CanonPoint { name: "finale".into(), day: 30, stage: Some("finale".into()), note: String::new(), after_death: true, premise: Some("记忆体框架：她已不在人世——按此前提扮演".into()) },
            ],
            pending: vec![],
            qc: vec![],
        };
        pack.canon_points = ingest::build_canon_points(&pack);
        pack
    }

    #[test]
    fn ingest_commit_writes_card_grown_and_worldline_with_canon_point() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("codex/testworld/entities")).unwrap();

        // 切入点 1（死亡前）：秘密只有本人知道；世界线落盘
        let report = ingest_commit_core(root, "testworld", sample_pack("testworld"), 1, false, false).unwrap();
        assert_eq!(report["entitiesWritten"].as_array().unwrap().len(), 2, "{report}");
        assert_eq!(report["worldlineWritten"].as_bool(), Some(true));

        let card_dir = root.join("characters").join(report["cardDir"].as_str().unwrap());
        let card_name = report["cardDir"].as_str().unwrap().to_string();
        assert!(card_dir.join("card.lua").is_file());
        let loaded = card::load_card(root, "爱莉希雅").unwrap();
        assert_eq!(loaded.card.name, "爱莉希雅");
        assert_eq!(loaded.card.first_mes, "你好呀，我是爱莉希雅♪");
        let _ = card_name;

        // 正史增量：秘密知情集 = 本人（第 1 天「finale@30」未揭示）；lifecycle dead@30
        let grown = store::load_grown(root, "testworld");
        let char_e = grown.entities.get("char.爱莉希雅").expect("角色实体应写入 grown");
        let known_by = char_e
            .pointer("/secrets/律者/known_by/0")
            .expect("秘密进正史");
        assert_eq!(known_by, "爱莉希雅");
        assert_eq!(
            char_e.pointer("/lifecycle/at_day").and_then(|d| d.as_i64()),
            Some(30)
        );
        // 世界线声明可被归一化读回（M3.7 引擎吃这份）
        let wl_source = std::fs::read_to_string(root.join("codex/testworld/worldline.lua")).unwrap();
        let shape = card::worldline_shape(&wl_source).unwrap();
        assert_eq!(shape["id"], "main");
        assert_eq!(shape["state_tree"]["root"], "opening");

        // load_codex 指纹包含 grown.json：立刻能读到新实体（确认即进注入）
        let cx = load_codex(root, None, "testworld");
        assert!(cx.get("char.爱莉希雅").is_some());
        assert!(cx.get("org.逐火十三英桀").is_some());
        assert!(
            cx.get("char.爱莉希雅").unwrap().secrets.contains_key("律者"),
            "秘密进正史"
        );

        // 重复 commit：角色与组织都被查重拦下（skipped），世界线默认不覆盖
        let again = ingest_commit_core(root, "testworld", sample_pack("testworld"), 1, false, false).unwrap();
        let skipped = again["skipped"].as_array().unwrap();
        assert!(skipped.len() >= 2, "重复实体应被跳过：{skipped:?}");
        assert_eq!(again["worldlineWritten"].as_bool(), Some(false));
        assert!(
            (again["warnings"].as_array().unwrap())
                .iter()
                .any(|w| w.as_str().unwrap().contains("世界主线")),
            "已有主线要提示未覆盖：{:?}",
            again["warnings"]
        );
    }

    #[test]
    fn ingest_commit_at_late_canon_point_publishes_secrets_and_premise() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("codex/testworld/entities")).unwrap();

        // 切入点 30（死亡后）：秘密公开（known_by=*），记忆体前提写进卡 scenario
        let report =
            ingest_commit_core(root, "testworld", sample_pack("testworld"), 30, true, true).unwrap();
        let grown = store::load_grown(root, "testworld");
        let char_e = grown.entities.get("char.爱莉希雅").unwrap();
        let known_by = char_e.pointer("/secrets/律者/known_by/0").unwrap();
        assert_eq!(known_by, "*");
        // 卡的 scenario 带记忆体前提（card.lua 是字节转义文本，读解析后的卡来断言）
        let loaded = card::load_card(root, "爱莉希雅").unwrap();
        assert!(loaded.card.scenario.contains("开场前提"), "{}", loaded.card.scenario);
        assert!(loaded.card.scenario.contains("记忆体"));
        // set_world_day：世界时钟拨到切入点
        assert_eq!(store::load_world(root, "testworld").day, 30);
    }

    // ---------- A4 写入闸门：前台 rewrite 与后台总结批次互斥 ----------

    /// 测试便捷：闸门空闲路径下批次即时落盘，取应用条数
    fn applied_n(r: Result<SummaryApply, String>) -> usize {
        match r.expect("落盘要成功") {
            SummaryApply::Applied(n) => n,
            SummaryApply::Parked => panic!("闸门空闲时不应暂存"),
        }
    }

    /// 一批带单条情景记忆的总结产物（A4 用例的弹药）
    fn episode_outcome() -> summarize::SummaryOutcome {
        summarize::SummaryOutcome {
            episodes: vec![summarize::EpisodeDraft {
                content: "小雨在图书馆把书还了".into(),
                salience: 0.9,
                emotion: Some("平静".into()),
                place: Some("图书馆".into()),
                actors: vec!["小雨".into()],
                witnesses: vec!["小雨".into()],
                links: vec!["place:图书馆".into()],
                thread: None,
                turns: vec![1],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn gate_is_exclusive_per_session() {
        let g = gate();
        let h1 = g.acquire("闸门单测-互斥").expect("首个持有者要拿到");
        assert!(
            g.acquire("闸门单测-互斥").is_none(),
            "持有期间并发进入要被拒"
        );
        assert!(!g.try_acquire("闸门单测-互斥"));
        drop(h1);
        let h2 = g.acquire("闸门单测-互斥").expect("释放后要能再拿");
        drop(h2);
        // 不同会话互不影响
        let _a = g.acquire("闸门单测-A").expect("A 要拿到");
        let _b = g.acquire("闸门单测-B").expect("B 会话独立拿锁");
    }

    #[test]
    fn summary_batch_parks_during_write_and_replays_without_loss() {
        // A4 核心场景：前台 read 快照 → rewrite 期间，后台总结的批次不得
        // 插进重写窗口、也不得丢——先暂存，闸门空出后原样重放。
        let (_dir, meta, root) = setup(HOOK_CARD);
        let session_id = meta.id.clone();
        let log = store::EventLog::new();
        let base_records = log.read(&root, &session_id).unwrap().len();

        // 前台持有闸门（模拟生成/编辑进行中）
        let guard = gate().acquire(&session_id).expect("拿闸门");

        // 后台总结要落盘：闸门被占 → 整批暂存，事件流一条不动
        let proj = project_session(&log, &root, &meta).unwrap();
        let cx = load_codex(&root, None, "default");
        let applied = apply_summary_outcome(
            &log,
            &root,
            &meta,
            &cx,
            &proj,
            episode_outcome(),
            1,
            2,
            1,
            "20:00",
            scene::DEFAULT_SCENE_ID,
        )
        .unwrap();
        assert!(matches!(applied, SummaryApply::Parked), "{applied:?}");
        assert!(gate().has_pending(&session_id), "批次要进暂存队列");
        assert_eq!(
            log.read(&root, &session_id).unwrap().len(),
            base_records,
            "暂存期间不落任何事件"
        );

        // 前台编辑路径照常 rewrite（用旧快照重建），随后收尾释放闸门
        let records = log.read(&root, &session_id).unwrap();
        let rebuilt = records.as_ref().clone();
        log.rewrite(&root, &session_id, &rebuilt).unwrap();
        drop(guard);

        // 收尾重放：批次完整落进 rewrite 之后的流，无一条丢失
        assert_eq!(flush_parked_summaries(&root, &session_id), 1);
        assert!(!gate().has_pending(&session_id));
        let records = log.read(&root, &session_id).unwrap();
        assert_eq!(
            records.len(),
            base_records + 1,
            "重放恰好多一条记忆事件：{}",
            records.len()
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.body, LogBody::Memory(_))),
            "暂存批次的情景记忆要落盘"
        );
        // 派生文件同步过（投影里读回宫殿情景记忆验证）
        let proj2 = project_session(&log, &root, &meta).unwrap();
        assert!(
            proj2.episodes.iter().any(|e| e.get("content").and_then(|c| c.as_str())
                == Some("小雨在图书馆把书还了")),
            "palace 情景记忆要能读回：{:?}",
            proj2.episodes
        );
    }

    #[test]
    fn flush_defers_while_gate_still_busy() {
        // 暂存后 flush 时闸门仍被占（下一个命令抢了先）→ 批次保留，下次收尾再试
        let (_dir, meta, root) = setup(HOOK_CARD);
        let session_id = meta.id.clone();
        let log = store::EventLog::new();

        let guard = gate().acquire(&session_id).expect("拿闸门");
        let proj = project_session(&log, &root, &meta).unwrap();
        let cx = load_codex(&root, None, "default");
        let applied = apply_summary_outcome(
            &log,
            &root,
            &meta,
            &cx,
            &proj,
            episode_outcome(),
            1,
            2,
            1,
            "20:00",
            scene::DEFAULT_SCENE_ID,
        )
        .unwrap();
        assert!(matches!(applied, SummaryApply::Parked));

        assert_eq!(
            flush_parked_summaries(&root, &session_id),
            0,
            "闸门未释放时 flush 是空操作"
        );
        assert!(gate().has_pending(&session_id), "批次保留待下次收尾");
        drop(guard);
        assert_eq!(flush_parked_summaries(&root, &session_id), 1, "释放后重放");
        assert!(!gate().has_pending(&session_id));
    }

    #[test]
    fn cancel_flag_is_exclusive_and_survives_stop_removal() {
        // B1：生成互斥标记入口独占；stop 摘走后旧守卫 Drop 不得误删新标记
        let flags = CancelFlags::default();

        let guard = acquire_flag_guard(&flags, "flag-test").expect("首取要成功");
        let err = match acquire_flag_guard(&flags, "flag-test") {
            Err(e) => e,
            Ok(_) => panic!("持有期间要被拒"),
        };
        assert!(err.contains("生成中"), "{err}");
        // 中断位可被置（stop_generation 的动作路径）
        guard.store(true, Ordering::Relaxed);

        // stop 摘走标记 → 旧守卫 Drop 不再摘；新来的命令拿到全新标记
        if let Ok(mut map) = flags.0.lock() {
            map.remove("flag-test");
        }
        drop(guard);
        let fresh = acquire_flag_guard(&flags, "flag-test").expect("stop 后要能重新拿");
        assert!(!fresh.load(Ordering::Relaxed), "新标记必须从未置位");
        drop(fresh);

        // 常规路径：守卫 Drop 即释放，可再次获取
        let again = acquire_flag_guard(&flags, "flag-test").expect("Drop 后要能再拿");
        drop(again);
        acquire_flag_guard(&flags, "flag-test").expect("串行多轮不受影响");
    }

    #[test]
    fn eventlog_projection_cache_is_incremental() {
        // D1-2：同链 append 后投影只 fold 新增（零全量折叠），rewrite 后恰一次全量重建
        let (_dir, meta, root) = setup(HOOK_CARD);
        let log = store::EventLog::new();
        let fulls = std::cell::Cell::new(0u64);
        let counted = |rs: &[LogRecord]| {
            fulls.set(fulls.get() + 1);
            event::project_over(rs, &event::Base::default())
        };

        let records = log.read(&root, &meta.id).unwrap();
        let p1 = log
            .project_cached_records(&meta.id, &records, counted)
            .unwrap();
        assert_eq!(fulls.get(), 1, "冷启动恰一次全量折叠");

        // 同链重复投影：缓存命中，零全量
        let again = log
            .project_cached_records(&meta.id, &records, counted)
            .unwrap();
        assert_eq!(fulls.get(), 1, "命中缓存不重算");
        assert_eq!(again.messages.len(), p1.messages.len());

        // append 后投影：只 fold 新增
        log.append(
            &root,
            &meta.id,
            LogBody::Message(Message {
                name: None,
                turn: 9,
                role: "user".into(),
                content: "增量的一条".into(),
                ts: 0,
                scene_id: None,
            tool_calls: None,
            }
                ),
        )
        .unwrap();
        let records2 = log.read(&root, &meta.id).unwrap();
        let p2 = log
            .project_cached_records(&meta.id, &records2, counted)
            .unwrap();
        assert_eq!(fulls.get(), 1, "append 后投影走增量，零全量折叠");
        assert_eq!(p2.messages.len(), p1.messages.len() + 1);
        assert_eq!(p2.messages.last().unwrap().content, "增量的一条");
        assert_eq!(p2.last_seq, records2.last().unwrap().seq, "增量折叠推进 seq");

        // rewrite：链被换，缓存作废 → 恰一次全量
        log.rewrite(&root, &meta.id, &records2).unwrap();
        let records3 = log.read(&root, &meta.id).unwrap();
        let p3 = log
            .project_cached_records(&meta.id, &records3, counted)
            .unwrap();
        assert_eq!(fulls.get(), 2, "rewrite 后恰一次全量重建");
        assert_eq!(p3.messages.len(), p2.messages.len(), "rewrite 内容不变时投影一致");
    }

    #[test]
    fn story_timeline_matches_linear_scan() {
        // D8：前缀表与原线性扫描逐字节一致（含同轮重复取后者、乱序回退线性）
        let mk = |turn: u64, day: i64, clock: &str| {
            let mut board = store::Blackboard::default_board();
            board.day = day;
            board.clock = clock.into();
            LogBody::Blackboard(event::BlackboardEvent {
                turn,
                reason: "clock".into(),
                board,
                scene_id: None,
                ts: 0,
            })
        };
        let naive = |records: &[LogRecord], turn: u64| {
            records
                .iter()
                .filter_map(|r| match &r.body {
                    LogBody::Blackboard(b) if b.turn <= turn => {
                        Some((b.board.day, b.board.clock.clone()))
                    }
                    _ => None,
                })
                .last()
        };

        let records: Vec<LogRecord> = vec![mk(1, 1, "08:00"), mk(2, 1, "10:00"), mk(2, 1, "11:00"), mk(5, 3, "09:00")]
            .into_iter()
            .enumerate()
            .map(|(i, b)| LogRecord::new(i as u64 + 1, b))
            .collect();
        let tl = StoryTimeline::build(&records);
        assert!(tl.sorted);
        for turn in 0..=6u64 {
            assert_eq!(tl.at(turn), naive(&records, turn), "turn={turn}");
        }

        // 乱序事件流：退回线性扫，语义与原实现完全一致
        let bad: Vec<LogRecord> = vec![mk(5, 3, "09:00"), mk(2, 1, "10:00")]
            .into_iter()
            .enumerate()
            .map(|(i, b)| LogRecord::new(i as u64 + 1, b))
            .collect();
        let tl = StoryTimeline::build(&bad);
        assert!(!tl.sorted);
        for turn in 0..=6u64 {
            assert_eq!(tl.at(turn), naive(&bad, turn), "乱序 turn={turn}");
        }
    }
}
