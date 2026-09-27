//! Tauri 命令 · 剧场模式/导演树（M3.6 · 设计 §8.5/§10.5）与世界主线/世界时钟
//! （M3.7 · 设计 §6.6）：导演弧的两层——会话级剧场与世界级主线。

use super::*;
/// 导演调度（M3.4 · 设计 §10.5，与 Tauri 无关，便于单测）：从投影里取一切信号
/// （在场者、最近消息、黑板在场名单、活跃线、意图投票、最近的 char 发言人），
/// 交给 [`director::plan_speakers`] 打分，返回本轮发言人计划。
pub(crate) fn director_plan(
    meta: &store::SessionMeta,
    cast: &Cast,
    proj: &event::Projection,
    scene: Option<&str>,
    content: &str,
) -> Result<Vec<director::Pick>, String> {
    let present_members = present_members(cast, proj, scene);
    let candidates: Vec<director::Candidate> = present_members
        .iter()
        .map(|m| director::Candidate {
            dir: m.dir.clone(),
            name: cast.display_name(&m.dir),
        })
        .collect();
    if candidates.is_empty() {
        return Err("当前场景没有可发言的角色".into());
    }
    let present: Vec<String> = candidates.iter().map(|c| c.dir.clone()).collect();

    // 最近消息窗口与 char 发言人序列（本场景；窗口大小归 director 的权重表管，
    // 这里只切片不搬整段历史）
    let history = scene_messages(&proj.messages, scene);
    let recent: Vec<String> = history
        .iter()
        .rev()
        .take(director::weights::RECENT_WINDOW)
        .rev()
        .map(|m| m.content.clone())
        .collect();
    let name_to_dir = |name: &str| -> Option<String> {
        cast.members
            .iter()
            .find(|m| cast.display_name(&m.dir) == name)
            .map(|m| m.dir.clone())
            .or_else(|| cast.get(name).map(|m| m.dir.clone()))
    };
    let recent_speakers: Vec<String> = history
        .iter()
        .rev()
        .filter(|m| m.role == "char")
        .take(director::weights::COOLDOWN_SPAN)
        .filter_map(|m| m.name.as_deref().and_then(name_to_dir))
        .collect();

    let board = proj.effective_board(scene);
    // 活跃线切片（mention 窗口词 = resurface 的话题窗口）
    let threads: Vec<director::ThreadRef> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .filter(|t| t.state == threads::STATE_ACTIVE)
        .map(|t| director::ThreadRef {
            title: t.title.clone(),
            actors: t.actors.clone(),
            importance: t.importance,
            mention_words: t
                .resurface
                .windows
                .iter()
                .filter_map(|w| match w {
                    threads::ResurfaceWindow::Mention(words) => Some(words.clone()),
                    threads::ResurfaceWindow::All(parts) => {
                        let mut all = Vec::new();
                        for p in parts {
                            if let threads::ResurfaceWindow::Mention(words) = p {
                                all.extend(words.clone());
                            }
                        }
                        if all.is_empty() { None } else { Some(all) }
                    }
                    _ => None,
                })
                .flatten()
                .collect(),
        })
        .collect();

    // want_to_speak 投票：意图强度为基础；心里话队列非空记满票（M3.5 · 设计 §9.2
    // 「下一轮主动发消息」的调度侧——憋着话要说比一般意向更急，导演优先给她发言权）
    let votes: Vec<(String, f32)> = present_members
        .iter()
        .map(|m| {
            let psyche = psyche::Psyche::from_state(&current_state(proj, &m.dir, &m.loaded));
            let mut strength = psyche
                .intents
                .iter()
                .map(|i| i.strength)
                .fold(0.0f32, f32::max);
            if psyche.has_scheduled() {
                strength = strength.max(1.0);
            }
            (m.dir.clone(), strength)
        })
        .collect();

    let query = director::SpeechQuery {
        candidates: &candidates,
        present: &present,
        content,
        recent: &recent,
        board_actors: &board.actors,
        threads: &threads,
        votes: &votes,
        recent_speakers: &recent_speakers,
        max_speakers: meta_max_speakers(meta, cast),
    };
    Ok(director::plan_speakers(&query))
}

/// 每轮发言数的会话配置（夹到 1..=阵容数；导演调度天然限流，上限就是全阵容）。
/// 会话未配置（None）时取 [DEFAULT_MAX_SPEAKERS]。
fn meta_max_speakers(meta: &store::SessionMeta, cast: &Cast) -> usize {
    let configured = meta.max_speakers.unwrap_or(DEFAULT_MAX_SPEAKERS as u32) as usize;
    configured.clamp(1, cast.members.len().max(1))
}

// ---------- 剧场模式与导演树（M3.6 · 设计 §8.5/§10.5）----------

/// 剧场轮数预算缺省值（验收目标：自动跑 20 轮完成至少一次完整的开线→收线弧）
pub const DEFAULT_THEATER_BUDGET: u32 = 20;

/// 剧场模式视图（进度指示与前端自动轮次的数据源）
#[derive(Debug, Clone, serde::Serialize)]
pub struct TheaterView {
    pub on: bool,
    pub budget: u32,
    /// 已走掉的剧场轮数（当前轮 − 开场轮）
    pub used: u64,
    pub start_turn: u64,
    pub last_turn: u64,
    /// 导演树当前活跃路径（根→叶；还没开场 = 树根）
    pub path: Vec<String>,
    /// 当前阶段的 directive（「这一幕该是什么调子」）
    pub stage_directive: String,
    /// 用的是会话自带的 director.lua（false = 内置默认起承转合树）
    pub custom_tree: bool,
}

/// 导演树的加载（M3.6 ·「会话模板声明，Lua 走卡沙箱」）：
/// `sessions/<id>/director.lua` 优先（剧本包 v0 形态），缺省用内置起承转合树。
/// 自定义树解析失败回落默认树（诊断留痕）——剧场不因坏配置而停摆。
fn load_director_tree(
    root: &std::path::Path,
    session_id: &str,
) -> (String, std::sync::Arc<statetree::StateTree>, bool) {
    let custom = std::fs::read_to_string(
        store::session_dir(root, session_id).join("director.lua"),
    )
    .ok()
    .filter(|s| !s.trim().is_empty());
    if let Some(source) = custom {
        match card::state_tree_shape(&source)
            .map_err(|e| e)
            .and_then(|shape| statetree::StateTree::from_value(&shape).map_err(|e| e))
        {
            Ok(tree) => {
                for warning in tree.validate() {
                    crate::diag::record("director", format!("导演树校验：{warning}"));
                }
                return (source, std::sync::Arc::new(tree), true);
            }
            Err(e) => {
                crate::diag::record("director", format!("director.lua 解析失败，回落默认树：{e}"));
            }
        }
    }
    let tree = statetree::StateTree::from_value(&card::state_tree_shape(director::DEFAULT_DIRECTOR_LUA).expect("内置导演树应能解析"))
        .expect("内置导演树应能解析");
    (
        director::DEFAULT_DIRECTOR_LUA.to_string(),
        std::sync::Arc::new(tree),
        false,
    )
}

/// 剧场模式的轮末推进（M3.6）：导演树求值 → 阶段转移落流 → 调度动作执行 →
/// 交叉剪辑切场。finalize_turn 与单测共用同一份代码；
/// 一切动作都是事件（导演树事件 / 线事件 / 场景事件 / 调度事件），回放可重现。
#[allow(clippy::too_many_arguments)]
pub(crate) fn advance_theater(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    log: &store::EventLog,
    tree_cache: Option<&TreeCache>,
    runtime: Option<&SessionRuntime>,
    report: &mut llm::HookReport,
) -> Result<(), String> {
    let Some(theater) = meta.theater.clone() else {
        return Ok(());
    };
    // resolve_thread_at 需要 &TreeCache/&SessionRuntime：调用方没给（单测等）就用临时的空件
    let fallback_cache;
    let tree_cache = match tree_cache {
        Some(tc) => tc,
        None => {
            fallback_cache = TreeCache::default();
            &fallback_cache
        }
    };
    let fallback_runtime;
    let runtime = match runtime {
        Some(rt) => rt,
        None => {
            fallback_runtime = SessionRuntime::default();
            &fallback_runtime
        }
    };
    let proj = project_session(log, root, meta)?;
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let scene = scene_ctx(&proj);
    let (source, tree, _custom) = load_director_tree(root, &meta.id);

    // 母层查询子层（M3.7 联动 · 设计 §6.6）：主线阶段进导演判据环境——
    // 「公告期不排纯搞笑日常」写成 `st.worldline_stage == "公告期"` 即可（树写法不变）
    let world_name = session_world(meta);
    let wl = load_worldline(root, &world_name);
    let world_state = store::load_world(root, &world_name);
    let worldline_stage = worldline_path_of(&wl, &proj, &world_state)
        .last()
        .cloned()
        .unwrap_or_default();

    // 判据环境：state 槽放**合成表**——轮次预算与阶段时长是导演树专属判据
    // （角色状态树不看这些）；threads 判据集与角色树同源
    let threads_active: Vec<String> = proj
        .threads
        .iter()
        .filter_map(|(id, v)| {
            threads::Thread::from_value(v)
                .ok()
                .filter(|t| t.is_active())
                .map(|_| id.clone())
        })
        .collect();
    let anchor = proj
        .director_tree
        .last()
        .map(|e| e.turn)
        .unwrap_or(theater.start_turn)
        .max(theater.start_turn);
    let stage_turns = turn.saturating_sub(anchor) as i64;
    let turns_left = theater.budget as i64 - turn.saturating_sub(theater.start_turn) as i64;
    let env = card::TreeEnv {
        event: "theater:turn_end".into(),
        blackboard: blackboard_env(&proj.effective_board(scene.as_deref())),
        state: serde_json::json!({
            "turn": turn,
            "turns_left": turns_left,
            "stage_turns": stage_turns,
            "threads_active": threads_active,
            "scenes_active": proj.scenes.values().filter(|s| s.is_active()).count(),
            "worldline_stage": worldline_stage,
        }),
        threads_active: threads_active.iter().cloned().collect(),
        ..Default::default()
    };

    // 开场播种：走位史为空 = 导演还没上场 → 进入树根（跑 on_enter），落第一条走位事件。
    // 当轮不再求值转移（刚进的状态要站得住一轮）。
    if proj.director_tree.is_empty() {
        let path = tree.active_path(&tree.root);
        let leaf = path.last().cloned().unwrap_or_default();
        let (actions, hook_logs) = card::run_director_hook(&source, &leaf, "on_enter");
        report.logs.extend(hook_logs);
        commit(
            log,
            root,
            meta,
            LogBody::DirectorTree(event::DirectorTreeEvent {
                turn,
                from: Vec::new(),
                to: path,
                reason: "剧场开场".into(),
                ts: store::unix_now(),
            }),
        )?;
        let proj = project_session(log, root, meta)?;
        execute_director_actions(
            &proj, root, meta, cast, log, turn, scene.as_deref(), actions,
            tree_cache, runtime, report,
        )?;
        return Ok(());
    }

    // 阶段转移求值（与角色状态树同引擎：priority 升序、叶先、首个命中即转）
    let path = proj
        .director_tree
        .last()
        .map(|e| e.to.clone())
        .unwrap_or_else(|| tree.active_path(&tree.root));
    let Some(leaf) = path.last().cloned() else {
        return Ok(());
    };
    let decision = match card::eval_state_tree(&source, &path, &env) {
        Ok(Some(d)) => Some(d),
        Ok(None) => None,
        Err(e) => {
            crate::diag::record("director", format!("导演树求值失败：{e}"));
            None
        }
    };
    if let Some(d) = decision {
        let to_path = tree.active_path(&d.to);
        if to_path.is_empty() {
            crate::diag::record("director", format!("导演树转移目标未声明，保持原地：{}", d.to));
        } else {
            let to_leaf = to_path.last().cloned().unwrap_or_default();
            // 执行顺序与状态树同构（§7.3-3）：exit 动作 → 转移事件 → enter 动作
            let (exit_actions, hook_logs) = card::run_director_hook(&source, &leaf, "on_exit");
            report.logs.extend(hook_logs);
            execute_director_actions(
                &proj, root, meta, cast, log, turn, scene.as_deref(), exit_actions,
                tree_cache, runtime, report,
            )?;
            commit(
                log,
                root,
                meta,
                LogBody::DirectorTree(event::DirectorTreeEvent {
                    turn,
                    from: path.clone(),
                    to: to_path.clone(),
                    reason: d.reason.clone(),
                    ts: store::unix_now(),
                }),
            )?;
            let (enter_actions, hook_logs) = card::run_director_hook(&source, &to_leaf, "on_enter");
            report.logs.extend(hook_logs);
            let proj = project_session(log, root, meta)?;
            execute_director_actions(
                &proj, root, meta, cast, log, turn, scene.as_deref(), enter_actions,
                tree_cache, runtime, report,
            )?;
            report.ui_events.push(llm::UiEmit {
                kind: "theater".into(),
                value: format!("剧情进入「{to_leaf}」——{}", d.reason),
            });
        }
    }

    // 交叉剪辑（设计 §10.5）：多路场景且当前场景满节奏轮数 → 切下一路。
    // 候选 = 全部未归档场景（冻结的分路可被切回——切回即解冻，设计 §10.3
    // 「被切走的场景冻结，剧场模式下可由导演继续自动推进」）。
    // 「合场时机」不在这里——它是导演树合段的显式动作（api.merge_scenes）。
    let proj = project_session(log, root, meta)?;
    if let Some(cur) = scene_ctx(&proj) {
        let stages: Vec<String> = proj
            .scenes
            .iter()
            .filter(|(_, sc)| sc.status != scene::STATUS_MERGED)
            .map(|(id, _)| id.clone())
            .collect();
        if stages.len() > 1 {
            let rounds = rounds_in_current(&proj, log, root, meta, &cur, theater.start_turn);
            if let director::CutDecision::Cut { to } = director::plan_cut(&director::IntercutQuery {
                scenes: &stages,
                current: &cur,
                rounds_in_current: rounds,
                cadence: 0,
            }) {
                let to_place = proj.scenes.get(&to).map(|sc| sc.place.clone()).unwrap_or_default();
                // 调度史先落一笔（导演面板可查「为何转场」），再真正切场
                commit(
                    log,
                    root,
                    meta,
                    LogBody::Director(event::DirectorEvent {
                        turn,
                        op: "cut".into(),
                        picks: Vec::new(),
                        direct: Some(to.clone()),
                        note: Some(format!("交叉剪辑：{cur} 连续推进 {rounds} 轮，转场")),
                        ts: store::unix_now(),
                    }),
                )?;
                switch_scene_at(
                    log,
                    root,
                    meta,
                    &proj,
                    &to,
                    "director",
                    Some(format!("与此同时，{to_place}——")),
                )?;
            }
        }
    }
    Ok(())
}

/// 当前场景已连续推进的剧场轮数：本轮 − 最近一次「切进来」的轮次
/// （切场/交叉剪辑都算），没有切场史则从剧场开场起算。
fn rounds_in_current(
    proj: &event::Projection,
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    current: &str,
    start_turn: u64,
) -> u32 {
    let mut anchor = start_turn;
    if let Ok(records) = log.read(root, &meta.id) {
        for rec in records.iter() {
            match &rec.body {
                LogBody::Scene(s) if s.op == "switch" && s.scene_id == current => {
                    anchor = anchor.max(s.turn);
                }
                LogBody::Director(d) if d.op == "cut" && d.direct.as_deref() == Some(current) => {
                    anchor = anchor.max(d.turn);
                }
                _ => {}
            }
        }
    }
    proj.messages
        .last()
        .map(|m| m.turn)
        .unwrap_or(0)
        .saturating_sub(anchor) as u32
}

/// 执行导演动作（顺序执行，逐条落事件）。全部是元层调度：
/// 开/收线走线的生命周期（origin=director，重建保留），调窗落 retune 事件，合场走场景内核。
#[allow(clippy::too_many_arguments)]
fn execute_director_actions(
    proj: &event::Projection,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cast: &Cast,
    log: &store::EventLog,
    turn: u64,
    scene: Option<&str>,
    actions: Vec<director::DirectorAction>,
    tree_cache: &TreeCache,
    runtime: &SessionRuntime,
    report: &mut llm::HookReport,
) -> Result<(), String> {
    for action in actions {
        match action {
            director::DirectorAction::OpenThread { title, cause, actors, importance } => {
                let actors = actors.unwrap_or_else(|| {
                    present_members(cast, proj, scene)
                        .iter()
                        .map(|m| cast.display_name(&m.dir))
                        .collect()
                });
                let title = title.unwrap_or_else(|| {
                    intent_thread_title(proj, cast, scene).unwrap_or_else(|| "主线".into())
                });
                let cause =
                    cause.unwrap_or_else(|| format!("剧场主线：从角色的意图里生长出来——{title}"));
                open_thread_at(log, root, meta, &title, &cause, &actors, importance, "director")?;
                report.logs.push(format!("剧场开线：{title}"));
            }
            director::DirectorAction::ResolveThreads { thread_id, outcome } => {
                let ids: Vec<String> = match thread_id {
                    Some(id) => vec![id],
                    None => director_opened_active_threads(proj),
                };
                if ids.is_empty() {
                    report.logs.push("剧场收线：没有导演开的活跃线，跳过".into());
                }
                for id in ids {
                    let outcome = outcome.clone().unwrap_or_else(|| "剧场收束。".into());
                    resolve_thread_at(log, root, meta, &id, &outcome, tree_cache, runtime, "director")?;
                    report.logs.push(format!("剧场收线：{id}"));
                }
            }
            director::DirectorAction::Resurface { thread_id, direction } => {
                let Some(mut thread) = proj
                    .threads
                    .get(&thread_id)
                    .and_then(|v| threads::Thread::from_value(v).ok())
                else {
                    report.logs.push(format!("剧场调窗：没有这条线（{thread_id}），忽略"));
                    continue;
                };
                let (grade, note) = match direction.as_str() {
                    "earlier" => (threads::GRADE_EAGER, "导演把这条线往前赶（很想找机会说）"),
                    "later" => (threads::GRADE_DORMANT, "导演把这条线往后压（先放着别提）"),
                    _ => continue,
                };
                thread.resurface.grade = grade.into();
                commit(
                    log,
                    root,
                    meta,
                    LogBody::Thread(event::ThreadEvent {
                        turn,
                        op: threads::OP_RETUNE.into(),
                        thread_id: thread_id.clone(),
                        thread: Some(thread.to_value()),
                        origin: "director".into(),
                        note: Some(note.into()),
                        ts: store::unix_now(),
                    }),
                )?;
                report.logs.push(format!("剧场调窗：{thread_id} {direction}"));
            }
            director::DirectorAction::MergeScenes => {
                // 合段把全部分路收回（冻结的也算——「两路人马汇合」；已归档的不动）
                let others: Vec<String> = proj
                    .scenes
                    .iter()
                    .filter(|(id, sc)| {
                        sc.status != scene::STATUS_MERGED && Some(id.as_str()) != scene
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                if others.is_empty() {
                    report.logs.push("剧场合场：没有其他活跃场景，跳过".into());
                    continue;
                }
                commit(
                    log,
                    root,
                    meta,
                    LogBody::Director(event::DirectorEvent {
                        turn,
                        op: "merge".into(),
                        picks: Vec::new(),
                        direct: None,
                        note: Some(format!("合段裁决：{} 并入当前场景", others.join("、"))),
                        ts: store::unix_now(),
                    }),
                )?;
                merge_scenes_at(
                    log,
                    root,
                    meta,
                    proj,
                    &others,
                    "director",
                    Some("两条线在此交汇——".into()),
                )?;
            }
        }
    }
    Ok(())
}

/// 开线缺省标题：在场者最强的未外化意图（M3.5 的 proactive_candidate 口径）——
/// 让剧场主线从角色心里长出来，而不是凭空杜撰
fn intent_thread_title(
    proj: &event::Projection,
    cast: &Cast,
    scene: Option<&str>,
) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    for m in present_members(cast, proj, scene) {
        let p = psyche::Psyche::from_state(&current_state(proj, &m.dir, &m.loaded));
        for i in &p.intents {
            if i.linked_thread.is_some() || i.triggered.is_some() {
                continue;
            }
            if best.as_ref().map(|(b, _)| i.strength > *b).unwrap_or(true) {
                best = Some((i.strength, i.name.clone()));
            }
        }
    }
    best.map(|(_, name)| name)
}

/// 导演开过、且还活跃的线 id（收束动作的缺省对象；管线/心理外化的线不动）
fn director_opened_active_threads(proj: &event::Projection) -> Vec<String> {
    let mut ids: Vec<String> = proj
        .thread_log
        .iter()
        .filter(|e| e.op == threads::OP_OPEN && e.origin == "director")
        .map(|e| e.thread_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids.retain(|id| {
        proj.threads
            .get(id)
            .and_then(|v| threads::Thread::from_value(v).ok())
            .map(|t| t.is_active())
            .unwrap_or(false)
    });
    ids
}

/// 开/关剧场模式（M3.6）：开启记轮数预算与起点（进度 = 当前轮 − 起点）
#[tauri::command]
pub fn set_theater(
    session_id: String,
    on: bool,
    budget: Option<u32>,
    log: State<'_, store::EventLog>,
) -> Result<TheaterView, String> {
    let root = root();
    let mut meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    meta.theater = if on {
        let proj = project_session(&log, &root, &meta)?;
        let last_turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
        Some(store::TheaterConfig {
            budget: budget.unwrap_or(DEFAULT_THEATER_BUDGET).clamp(4, 200),
            start_turn: last_turn,
        })
    } else {
        None
    };
    store::save_session(&root, &meta).map_err(|e| e.to_string())?;
    theater_view_of(&log, &root, &meta)
}

/// 剧场模式视图（进度指示 / 当前阶段 / 剩余轮数）
#[tauri::command]
pub fn theater_view(
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<TheaterView, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    theater_view_of(&log, &root, &meta)
}

/// 剧场视图的内核（与 Tauri 无关，便于单测）
pub(crate) fn theater_view_of(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<TheaterView, String> {
    let proj = project_session(log, root, meta)?;
    let last_turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let (_source, tree, custom) = load_director_tree(root, &meta.id);
    let path = proj
        .director_tree
        .last()
        .map(|e| e.to.clone())
        .unwrap_or_else(|| tree.active_path(&tree.root));
    let (on, budget, used, start_turn) = match &meta.theater {
        Some(t) => (true, t.budget, last_turn.saturating_sub(t.start_turn), t.start_turn),
        None => (false, 0, 0, 0),
    };
    Ok(TheaterView {
        on,
        budget,
        used,
        start_turn,
        last_turn,
        stage_directive: tree.directive_of(&path),
        path,
        custom_tree: custom,
    })
}

// ---------- 世界主线与世界时钟（M3.7 · 设计 §6.6）----------

/// 世界时钟基准（new_session 的内核，可单测）：显式天优先；没有显式天就从世界时钟
/// 出发（世界还没走过第 1 天 = None，保持建会话缺省——没有 world.json 时行为不变）。
pub(crate) fn baseline_day_from_world(
    root: &std::path::Path,
    day: Option<i64>,
    world: &str,
) -> Option<i64> {
    match day {
        Some(d) => Some(d),
        None => {
            let w = store::load_world(root, world);
            (w.day > 1).then_some(w.day)
        }
    }
}

/// 世界主线视图（检查器「世界」面板的数据源）
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorldlineView {
    /// 这个世界配了 worldline.lua（false = 可选层缺席，其余字段为空档）
    pub configured: bool,
    pub id: String,
    pub premise: String,
    /// 当前活跃路径（根→叶）：会话走位史最后一条 → world.json 进度 → 树根
    pub path: Vec<String>,
    /// 当前阶段名（活跃路径的叶）
    pub stage: String,
    /// 当前阶段的 directive（根→叶拼接；B2 世界段的同一份数据）
    pub stage_directive: String,
    /// B1 时代行（「公告期——公告已贴出…」）
    pub era: String,
    /// 世界时钟（world.json 持久；会话轮末 max 回写）
    pub world_day: i64,
    /// 本会话的故事时钟（聚焦场景的局部天）
    pub session_day: i64,
    /// 世界级线（scope=world，任何会话可推进；本会话未见的从 world.json 并入）
    pub world_threads: Vec<serde_json::Value>,
    /// 世界时钟最近由谁推进（溯源）
    pub updated_by: Option<String>,
}

/// 世界主线的加载（M3.7 ·「Lua 走卡沙箱」，与 load_director_tree 同纪律）：
/// `codex/<世界>/worldline.lua` 可选——缺文件 = 无主线（纯日常世界照常运转）；
/// 坏文件回落 None 并留诊断，不让一个手滑的配置瘫痪整个世界。
/// 每次现读（与导演树同款：文件很小，重解析成本可忽略；改文件即生效）。
pub(crate) fn load_worldline(
    root: &std::path::Path,
    world: &str,
) -> Option<std::sync::Arc<worldline::Worldline>> {
    let source = std::fs::read_to_string(world_path_of(root, world))
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    match card::worldline_shape(&source)
        .and_then(|shape| worldline::Worldline::from_shape(&shape, card::worldline_canonical(&source)))
    {
        Ok(wl) => {
            for warning in wl.tree.validate() {
                crate::diag::record("worldline", format!("世界主线校验：{warning}"));
            }
            Some(std::sync::Arc::new(wl))
        }
        Err(e) => {
            crate::diag::record("worldline", format!("worldline.lua 解析失败，按无主线处理：{e}"));
            None
        }
    }
}

fn world_path_of(root: &std::path::Path, world: &str) -> std::path::PathBuf {
    root.join("codex").join(world).join("worldline.lua")
}

/// 当前活跃路径的三级回落：会话走位史（本会话见证的转移）→ world.json 的世界进度
/// （老会话没见过任何转移，但世界早已在走）→ 树根（worldline 刚配置还没人走到过）。
pub(crate) fn worldline_path_of(
    wl: &Option<std::sync::Arc<worldline::Worldline>>,
    proj: &event::Projection,
    world: &worldline::WorldState,
) -> Vec<String> {
    if let Some(e) = proj.worldline.last() {
        return e.to.clone();
    }
    if let Some(wl) = wl {
        if let Some(prog) = &world.worldline {
            if prog.id == wl.id && !prog.path.is_empty() {
                return prog.path.clone();
            }
        }
        return wl.tree.active_path(&wl.tree.root);
    }
    Vec::new()
}

/// 会话侧该回写的主线进度（proj 走位史的最后一条；没有 = 本会话没推进过，回写 None）
fn worldline_progress_of(
    wl: &Option<std::sync::Arc<worldline::Worldline>>,
    proj: &event::Projection,
) -> Option<worldline::WorldlineProgress> {
    let e = proj.worldline.last()?;
    Some(worldline::WorldlineProgress {
        id: wl.as_ref().map(|w| w.id.clone()).unwrap_or_default(),
        path: e.to.clone(),
        advanced_turn: e.turn,
        advanced_in: None,
    })
}

/// 轮末推进世界主线（M3.7）：阶段转移求值（与导演树同引擎：priority 升序、
/// 叶先、首个命中即转）→ 转移落 `worldline` 事件（重建保留）→ 阶段钩子的
/// 世界层动作（reveal / 开世界级线）逐条执行。判据 `st.day` = 聚焦场景的故事天。
pub(crate) fn advance_worldline(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
    report: &mut llm::HookReport,
) -> Result<(), String> {
    let world_name = session_world(meta);
    let Some(wl) = load_worldline(root, &world_name) else {
        return Ok(()); // 无主线：世界照常运转（可选层）
    };
    let proj = project_session(log, root, meta)?;
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let scene = scene_ctx(&proj);
    let board = proj.effective_board(scene.as_deref());

    // 开局承袭：走位史为空 = 本会话还没见证过世界 → 落第一条走位事件
    //（不跑 on_enter——那个阶段的钩子在把它推进到这里的会话里已经跑过，
    // 世界进度不该因换会话重放而重演）
    let world_state = store::load_world(root, &world_name);
    let path = worldline_path_of(&Some(wl.clone()), &proj, &world_state);
    if proj.worldline.is_empty() {
        commit(
            log,
            root,
            meta,
            LogBody::Worldline(event::WorldlineEvent {
                turn,
                from: Vec::new(),
                to: path.clone(),
                reason: format!("开局承袭世界主线（世界时钟第{}天）", world_state.day),
                ts: store::unix_now(),
            }),
        )?;
        return Ok(());
    }

    // 阶段转移求值：state 槽 = 主线专属合成表（世界时钟 + 本段已走轮数）
    let anchor = proj.worldline.last().map(|e| e.turn).unwrap_or(0);
    let stage_turns = turn.saturating_sub(anchor) as i64;
    let env = card::TreeEnv {
        event: "worldline:turn_end".into(),
        blackboard: blackboard_env(&board),
        state: serde_json::json!({
            "day": board.day,
            "clock": board.clock,
            "stage_turns": stage_turns,
            "world_day": world_state.day,
        }),
        ..Default::default()
    };
    let leaf = path.last().cloned().unwrap_or_default();
    let decision = match card::eval_state_tree(&wl.source, &path, &env) {
        Ok(Some(d)) => Some(d),
        Ok(None) => None,
        Err(e) => {
            crate::diag::record("worldline", format!("世界主线求值失败：{e}"));
            None
        }
    };
    if let Some(d) = decision {
        let to_path = wl.tree.active_path(&d.to);
        if to_path.is_empty() {
            crate::diag::record("worldline", format!("世界主线转移目标未声明，保持原地：{}", d.to));
        } else {
            let to_leaf = to_path.last().cloned().unwrap_or_default();
            // 执行顺序与状态树同构（§7.3-3）：exit 动作 → 转移事件 → enter 动作
            let (exit_actions, hook_logs) = card::run_worldline_hook(&wl.source, &leaf, "on_exit");
            report.logs.extend(hook_logs);
            execute_worldline_actions(&proj, root, meta, log, turn, exit_actions, report)?;
            commit(
                log,
                root,
                meta,
                LogBody::Worldline(event::WorldlineEvent {
                    turn,
                    from: proj
                        .worldline
                        .last()
                        .map(|e| e.to.clone())
                        .unwrap_or_default(),
                    to: to_path,
                    reason: d.reason.clone(),
                    ts: store::unix_now(),
                }),
            )?;
            let (enter_actions, hook_logs) =
                card::run_worldline_hook(&wl.source, &to_leaf, "on_enter");
            report.logs.extend(hook_logs);
            let proj = project_session(log, root, meta)?;
            execute_worldline_actions(&proj, root, meta, log, turn, enter_actions, report)?;
            report.ui_events.push(llm::UiEmit {
                kind: "worldline".into(),
                value: format!("世界进入「{to_leaf}」——{}", d.reason),
            });
        }
    }
    Ok(())
}

/// 执行世界主线动作（顺序执行，逐条落事件）。只有两个世界层动作：
/// reveal（无见证者 = 全局知情——大势对所有人可见）与开世界级线（scope=world）。
fn execute_worldline_actions(
    proj: &event::Projection,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
    turn: u64,
    actions: Vec<worldline::WorldlineAction>,
    report: &mut llm::HookReport,
) -> Result<(), String> {
    for action in actions {
        match action {
            worldline::WorldlineAction::Reveal { targets } => {
                for target in targets {
                    commit(
                        log,
                        root,
                        meta,
                        LogBody::Codex(event::CodexEvent {
                            turn,
                            op: "reveal".into(),
                            target: target.clone(),
                            origin: "worldline".into(),
                            value: None,
                            note: Some("世界大势所至".into()),
                            witnesses: Vec::new(),
                            ts: store::unix_now(),
                        }),
                    )?;
                    report.logs.push(format!("世界主线揭示：{target}"));
                }
            }
            worldline::WorldlineAction::OpenThread { id, title, cause, importance } => {
                // 世界级线没有 actor：它压着整个世界，谁碰上谁推进
                let title = title.unwrap_or_else(|| {
                    proj.worldline
                        .last()
                        .and_then(|e| e.to.last().cloned())
                        .unwrap_or_else(|| "世界主线".into())
                });
                let cause = cause.unwrap_or_else(|| "世界大势：主线阶段带来的变局。".into());
                let value = open_thread_scoped_at(
                    log, root, meta, &title, &cause, &[], importance, "worldline",
                    threads::SCOPE_WORLD, id.as_deref(),
                )?;
                report
                    .logs
                    .push(format!("世界主线开线：{}", value["id"].as_str().unwrap_or(&title)));
            }
        }
    }
    Ok(())
}

/// 轮末世界回写（M3.7）：`world.json` 取 `max(世界, 本会话)`。
/// 在轮末连续做（而非等「会话结束」）——语义与结束回写完全一致（max 单调），
/// 崩溃/强退也不丢进度。多场景会话取**最远场景**的故事天（并行的「与此同时」
/// 各自推进，世界的「现在」以走到最远处为准）。
pub(crate) fn sync_world_now(
    root: &std::path::Path,
    meta: &store::SessionMeta,
    log: &store::EventLog,
) -> Result<(), String> {
    let world_name = session_world(meta);
    let proj = project_session(log, root, meta)?;
    let day = proj
        .scenes
        .values()
        .filter(|sc| sc.status != scene::STATUS_MERGED)
        .map(|sc| sc.day)
        .max()
        .unwrap_or_else(|| proj.effective_board(None).day);
    let wl = load_worldline(root, &world_name);
    let progress = worldline_progress_of(&wl, &proj);
    // 世界级线：本会话推过的（scope=world）合回世界——别的会话接着推进
    let world_threads: Vec<serde_json::Value> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .filter(|t| t.scope == threads::SCOPE_WORLD)
        .map(|t| t.to_value())
        .collect();
    let mut world = store::load_world(root, &world_name);
    let mut progress = progress;
    if let Some(p) = &mut progress {
        p.advanced_in = Some(meta.id.clone());
    }
    worldline::sync(
        &mut world,
        &meta.id,
        day,
        progress,
        &world_threads,
        store::unix_now(),
    );
    store::save_world(root, &world_name, &world).map_err(|e| e.to_string())
}

/// 世界主线视图（检查器「世界」面板）
#[tauri::command]
pub fn worldline_view(
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<WorldlineView, String> {
    let root = root();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    worldline_view_of(&log, &root, &meta)
}

pub(crate) fn worldline_view_of(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
) -> Result<WorldlineView, String> {
    let world_name = session_world(meta);
    let proj = project_session(log, root, meta)?;
    let world = store::load_world(root, &world_name);
    let wl = load_worldline(root, &world_name);
    let path = worldline_path_of(&wl, &proj, &world);
    let (id, premise, directive, era) = match &wl {
        Some(w) => {
            let d = w.tree.directive_of(&path);
            (
                w.id.clone(),
                w.premise.clone(),
                d.clone(),
                worldline::era_line(&w.tree, &path).unwrap_or_default(),
            )
        }
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    // 世界级线 = world.json 的存档 ∪ 本会话自己的（会话侧优先——它是活的推进记录）
    // ∪ 声明里还没开的（面板预告「大势将至」；声明 id 与 api.open_thread 的 id 对得上）
    let session_world_threads: Vec<serde_json::Value> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .filter(|t| t.scope == threads::SCOPE_WORLD)
        .map(|t| t.to_value())
        .collect();
    let mut world_threads: Vec<serde_json::Value> = Vec::new();
    for t in &world.threads {
        let id = t.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if !session_world_threads
            .iter()
            .any(|x| x.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            world_threads.push(t.clone());
        }
    }
    if let Some(w) = &wl {
        for title in &w.world_threads {
            // has_thread 覆盖 world.json 存档；会话侧刚推过的按标题再对一遍
            if !world.has_thread(title)
                && !session_world_threads
                    .iter()
                    .any(|x| x.get("title").and_then(|v| v.as_str()) == Some(title.as_str()))
            {
                world_threads.push(serde_json::json!({
                    "id": threads::id_from_title(title),
                    "title": title,
                    "state": "declared",
                    "scope": threads::SCOPE_WORLD,
                }));
            }
        }
    }
    world_threads.extend(session_world_threads);
    let session_day = proj.effective_board(None).day;
    let stage = wl.as_ref().map(|w| w.stage_of(&path)).unwrap_or_default();
    Ok(WorldlineView {
        configured: wl.is_some(),
        id,
        premise,
        stage_directive: directive,
        era,
        stage,
        path,
        world_day: world.day,
        session_day,
        world_threads,
        updated_by: world.updated_by.clone(),
    })
}

/// 手动校准世界时钟（玩家纠正/ flashback 布景用）：只认合理的正数，写完即生效
#[tauri::command]
pub fn world_set_clock(world: String, day: i64) -> Result<i64, String> {
    let root = root();
    let name = if world.trim().is_empty() { "default".into() } else { world };
    let mut w = store::load_world(&root, &name);
    // 加固 A7：i64::MAX 天会让 prompt::advance_clock 的 day + total/1440 溢出 panic
    w.day = prompt::clamp_story_day(day);
    store::save_world(&root, &name, &w).map_err(|e| e.to_string())?;
    Ok(w.day)
}

