//! Tauri 命令 · 场景与多线（M3.2 · 设计 §10.3：「与此同时」）：
//! 切场/分场/合场/冻结的场景操作与即兴开关。

use super::*;
// ---------- 场景与多线（M3.2 · 设计 §10.3：「与此同时」）----------

/// 场景视图（前端场景条的数据源）
#[derive(Serialize)]
pub struct SceneView {
    pub scenes: Vec<scene::Scene>,
    /// 当前聚焦场景（None = 无场景会话，单场景语义）
    pub active: Option<String>,
}

fn scene_view(proj: &event::Projection) -> SceneView {
    SceneView {
        scenes: proj.scenes.values().cloned().collect(),
        active: proj.active_scene.clone(),
    }
}

/// 小说式过渡插页（切场/分场/合场的叙事接缝）：system 消息，只归属目标场景。
/// 重放不重跑它的钩子（见 rebuild_from 的 system 跳过），live 侧同样只落盘不跑钩子。
fn append_transition(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    turn: u64,
    scene_id: &str,
    text: &str,
) -> Result<(), String> {
    let msg = Message {
        turn,
        role: "system".into(),
        content: text.to_string(),
        ts: store::unix_now(),
        tool_calls: None,
        scene_id: Some(scene_id.to_string()),
        name: None,
    };
    log.append(root, &meta.id, LogBody::Message(msg))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 场景事件 → 重投影 → 场景视图（场景命令的统一收尾）
fn commit_scene(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    event: event::SceneEvent,
) -> Result<SceneView, String> {
    let proj = commit(log, root, meta, LogBody::Scene(event))?;
    Ok(scene_view(&proj))
}

#[tauri::command]
pub fn list_scenes(
    session_id: String,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let proj = &ctx.proj;
    // 老会话没有场景事件：把世界层黑板补落地为缺省场景（只读视图，不落事件）
    if !proj.has_scenes() {
        let board = blackboard_of(proj);
        return Ok(SceneView {
            scenes: vec![scene::Scene::from_board(
                scene::DEFAULT_SCENE_ID,
                "开场",
                &board,
                0,
                "default",
                store::unix_now(),
            )],
            active: Some(scene::DEFAULT_SCENE_ID.into()),
        });
    }
    Ok(scene_view(proj))
}

/// 新建场景（另起一个舞台；视角随即切过去，插入过渡插页）
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn create_scene(
    session_id: String,
    title: String,
    place: String,
    actors: Vec<String>,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let (root, meta) = (&ctx.root, &ctx.meta);
    let proj = &ctx.proj;
    let board = proj.effective_board(scene_ctx(proj).as_deref());
    let ts = store::unix_now();
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let id = scene::new_scene_id(ts);
    let sc = scene::Scene {
        id: id.clone(),
        title: title.trim().to_string(),
        place: place.trim().to_string(),
        actors: actors
            .into_iter()
            .filter(|a| !a.trim().is_empty())
            .collect(),
        day: board.day,
        clock: board.clock.clone(),
        flags: BTreeMap::new(),
        created_turn: turn,
        origin: "manual".into(),
        parent: None,
        status: scene::STATUS_ACTIVE.into(),
        ts,
    };
    let title = sc.title.clone();
    let place_text = sc.place.clone();
    commit_scene(
        &log,
        root,
        meta,
        event::SceneEvent {
            turn,
            op: "create".into(),
            scene_id: id.clone(),
            scene: Some(sc),
            others: Vec::new(),
            origin: "manual".into(),
            note: note.clone(),
            ts,
        },
    )?;
    append_transition(
        &log,
        root,
        meta,
        turn,
        &id,
        &note.unwrap_or_else(|| format!("——{title}·{place_text}——")),
    )
    .map_err(|e| e.to_string())?;
    Ok(scene_view(sync_now(&log, root, meta)?.as_ref()))
}

/// 切场（设计 §10.3：视角切到另一场景，被切走的场景冻结；插入小说式过渡）
#[tauri::command]
pub fn switch_scene(
    session_id: String,
    scene_id: String,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    switch_scene_at(&log, &ctx.root, &ctx.meta, &ctx.proj, &scene_id, "manual", note)
}

/// 切场的内核（与 Tauri 无关）：manual（玩家）与 director（交叉剪辑，M3.6）共用。
/// 场景事件 origin=director 与 manual 同为元层动作——重建不丢。
pub(crate) fn switch_scene_at(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    proj: &event::Projection,
    scene_id: &str,
    origin: &str,
    note: Option<String>,
) -> Result<SceneView, String> {
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let Some(sc) = proj.scenes.get(scene_id) else {
        return Err(format!("场景「{scene_id}」不存在"));
    };
    let (place, frozen) = (sc.place.clone(), sc.status == scene::STATUS_FROZEN);
    commit_scene(
        log,
        root,
        meta,
        event::SceneEvent {
            turn,
            op: "switch".into(),
            scene_id: scene_id.to_string(),
            scene: None,
            others: Vec::new(),
            origin: origin.into(),
            note: note.clone(),
            ts: store::unix_now(),
        },
    )?;
    let transition = note.unwrap_or_else(|| {
        if frozen {
            format!("（回到）{place}——")
        } else {
            format!("与此同时，{place}——")
        }
    });
    append_transition(log, root, meta, turn, scene_id, &transition).map_err(|e| e.to_string())?;
    Ok(scene_view(sync_now(log, root, meta)?.as_ref()))
}

/// 分场（设计 §10.3：一部分角色离场另立场景，视角跟到新场景）
#[tauri::command]
pub fn split_scene(
    session_id: String,
    title: String,
    place: String,
    moving: Vec<String>,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let (root, meta) = (&ctx.root, &ctx.meta);
    let proj = &ctx.proj;
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let active = scene_ctx(proj).ok_or("这个会话还没有场景可以分场")?;
    let parent = proj
        .scenes
        .get(&active)
        .cloned()
        .ok_or("当前聚焦场景不存在")?;
    let ts = store::unix_now();
    let id = scene::new_scene_id(ts);
    // 校验与快照在这里算一遍（错误当场报）；在场者扣减由折叠按事件重演
    let (_next_parent, sc) = parent
        .split_from(&id, title.trim(), place.trim(), &moving, ts)
        .map_err(|e| e)?;
    let (title, place_text) = (sc.title.clone(), sc.place.clone());
    commit_scene(
        &log,
        root,
        meta,
        event::SceneEvent {
            turn,
            op: "split".into(),
            scene_id: id.clone(),
            scene: Some(sc),
            others: vec![parent.id.clone()],
            origin: "manual".into(),
            note: note.clone(),
            ts,
        },
    )?;
    append_transition(
        &log,
        root,
        meta,
        turn,
        &id,
        &note.unwrap_or_else(|| format!("与此同时，{place_text}——{title}")),
    )
    .map_err(|e| e.to_string())?;
    Ok(scene_view(sync_now(&log, root, meta)?.as_ref()))
}

/// 合场（设计 §10.3：两路场景并进聚焦场景——在场者并集、时间取较晚一路、
/// flags 冲突聚焦场景赢；被并入的场景归档。**各角色记忆不合并**——
/// 他们各自记得自己那条线里的事，这正是多线的戏剧价值。）
#[tauri::command]
pub fn merge_scenes(
    session_id: String,
    from: Vec<String>,
    note: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    merge_scenes_at(&log, &ctx.root, &ctx.meta, &ctx.proj, &from, "manual", note)
}

/// 合场的内核（与 Tauri 无关）：manual（玩家，经确认对话框）与
/// director（导演树的合场裁决，M3.6）共用。
pub(crate) fn merge_scenes_at(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    proj: &event::Projection,
    from: &[String],
    origin: &str,
    note: Option<String>,
) -> Result<SceneView, String> {
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let active = scene_ctx(proj).ok_or("这个会话还没有场景可以合场")?;
    if from.is_empty() {
        return Err("没有指定要并入哪些场景".into());
    }
    if from.contains(&active) {
        return Err("不能把场景并进它自己".into());
    }
    let target = proj
        .scenes
        .get(&active)
        .cloned()
        .ok_or("当前聚焦场景不存在")?;
    let mut sources = Vec::new();
    for id in from {
        let sc = proj
            .scenes
            .get(id)
            .filter(|sc| sc.status != scene::STATUS_MERGED)
            .ok_or_else(|| format!("场景「{id}」不存在或已归档"))?;
        sources.push(sc.clone());
    }
    let ts = store::unix_now();
    let target = target.merge_into(&sources, ts);
    let place = target.place.clone();
    commit_scene(
        log,
        root,
        meta,
        event::SceneEvent {
            turn,
            op: "merge".into(),
            scene_id: active.clone(),
            scene: Some(target),
            others: from.to_vec(),
            origin: origin.into(),
            note: note.clone(),
            ts,
        },
    )?;
    append_transition(
        log,
        root,
        meta,
        turn,
        &active,
        &note.unwrap_or_else(|| format!("两条线在此交汇——{place}——")),
    )
    .map_err(|e| e.to_string())?;
    Ok(scene_view(sync_now(log, root, meta)?.as_ref()))
}

/// 编辑场景分区（地点/在场者/局部时钟/标题；flags 经黑板面板的场景视图维护）
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_scene(
    session_id: String,
    scene_id: String,
    title: Option<String>,
    place: Option<String>,
    actors: Option<Vec<String>>,
    day: Option<i64>,
    clock: Option<String>,
    log: State<'_, store::EventLog>,
) -> Result<SceneView, String> {
    let ctx = session_ctx(&log, &session_id)?;
    let proj = &ctx.proj;
    let turn = proj.messages.last().map(|m| m.turn).unwrap_or(0);
    let mut sc = proj
        .scenes
        .get(&scene_id)
        .cloned()
        .ok_or_else(|| format!("场景「{scene_id}」不存在"))?;
    if let Some(t) = title {
        sc.title = t.trim().to_string();
    }
    if let Some(p) = place {
        sc.place = p.trim().to_string();
    }
    if let Some(a) = actors {
        sc.actors = a.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
    }
    if let Some(d) = day {
        sc.day = d;
    }
    if let Some(c) = clock {
        sc.clock = c.trim().to_string();
    }
    commit_scene(
        &log,
        &ctx.root,
        &ctx.meta,
        event::SceneEvent {
            turn,
            op: "update".into(),
            scene_id: scene_id.clone(),
            scene: Some(sc),
            others: Vec::new(),
            origin: "manual".into(),
            note: None,
            ts: store::unix_now(),
        },
    )
}

/// 配置每轮发言数上限（M3.4 群聊 · 设计 §10.5；导演调度的天然限流旋钮）。
/// 0 = 恢复缺省（2）。夹取与生效都在发送路径做（[meta_max_speakers]）。
#[tauri::command]
pub fn set_max_speakers(session_id: String, max_speakers: u32) -> Result<u32, String> {
    let root = root();
    let mut meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    meta.max_speakers = if max_speakers == 0 { None } else { Some(max_speakers) };
    store::save_session(&root, &meta).map_err(|e| e.to_string())?;
    Ok(meta.max_speakers.unwrap_or(DEFAULT_MAX_SPEAKERS as u32))
}

/// 即兴模式开关（M3.8 · 设计 §6.8-4，默认关）
#[tauri::command]
pub fn set_improv(session_id: String, improv: bool) -> Result<bool, String> {
    let root = root();
    let mut meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    meta.improv = improv;
    store::save_session(&root, &meta).map_err(|e| e.to_string())?;
    Ok(meta.improv)
}

