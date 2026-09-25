//! Tauri 命令 · 自动总结管线（M2.6 · 设计 §5.3：滑出 L0 窗口的批次 → 六类产物）。
//! 后台总结经 per-session 写入闸门与前台互斥（加固 A4）；批次被占用时整批暂存重放。

use super::*;
/// 暂存的总结批次：闸门被前台占用时整批挂起，空出后原样重放
pub(crate) struct ParkedSummary {
    outcome: summarize::SummaryOutcome,
    from_turn: u64,
    to_turn: u64,
    story_day: i64,
    story_clock: String,
    scene_id: String,
}

/// 暂存队列上限（正常只会有 1 条：有暂存时 spawn_summary 直接跳过；
/// 手动 summarize_now 可能再塞，超限丢最旧——下轮总结会重跑同批消息）
pub(crate) const MAX_PARKED_SUMMARIES: usize = 4;

/// apply_summary_outcome 的两种归宿（调用方据此写诊断/回显）
#[derive(Debug)]
pub(crate) enum SummaryApply {
    Applied(usize),
    Parked,
}

// ---------- M2.6 自动总结管线（设计 §5.3：滑出 L0 窗口的批次 → 六类产物）----------

/// 管线在跑的会话（防同一会话并发总结：两次重叠的调用会总结出重复的记忆）。
/// Arc 包一层：后台任务结束时要在 spawn 里释放标记（否则一次失败后管线永久停摆）。
#[derive(Clone, Default)]
pub struct SummaryFlags(std::sync::Arc<Mutex<std::collections::HashSet<String>>>);

impl SummaryFlags {
    fn begin(&self, session_id: &str) -> bool {
        self.0
            .lock()
            .map(|mut set| set.insert(session_id.to_string()))
            .unwrap_or(false)
    }
    fn end(&self, session_id: &str) {
        if let Ok(mut set) = self.0.lock() {
            set.remove(session_id);
        }
    }
}

/// 取待总结的批次（滑出 L0 窗口、且未被此前摘要覆盖的消息）。
/// 设计 §5.3：消息滑出窗口即异步触发一次总结（不阻塞对话）。
/// 场景维度的批次判定（M3.2 · 设计 §10.3/§10.4）：每个场景各记水位、各卷各的摘要。
/// 「滑出窗口」按**当前舞台**算——非活跃场景（被切走/冻结）的消息本来就不在任何
/// 上下文窗口里，过水位即可总结；活跃场景保留最近窗口不总结。一次消化最老的一个场景。
pub(crate) fn summary_batch_scenes(
    proj: &event::Projection,
    include_window: bool,
) -> Option<(String, Vec<summarize::BatchMessage>, u64)> {
    let active = proj.active_scene_id().map(str::to_string);
    let mut by_scene: BTreeMap<String, Vec<&Message>> = BTreeMap::new();
    for m in &proj.messages {
        by_scene
            .entry(proj.scene_of_message(m))
            .or_default()
            .push(m);
    }
    let mut candidates: Vec<(String, Vec<summarize::BatchMessage>, u64)> = Vec::new();
    for (scene_id, msgs) in &by_scene {
        let upto = proj.summary_upto_for(scene_id);
        let mut pending: Vec<&Message> = msgs
            .iter()
            .copied()
            .filter(|m| m.turn > upto)
            .filter(|m| m.role == "user" || m.role == "char") // OOC/system 不进剧情记忆（§4.1）
            .collect();
        // 活跃场景的最近窗口仍在上下文里，不总结；include_window（force）时照常吞掉
        if Some(scene_id) == active.as_ref() && !include_window {
            if pending.len() <= prompt::WINDOW_MESSAGES {
                continue;
            }
            let cut = pending.len() - prompt::WINDOW_MESSAGES;
            pending.truncate(cut);
        }
        if pending.is_empty() {
            continue;
        }
        let batch: Vec<summarize::BatchMessage> = pending
            .into_iter()
            .map(summarize::BatchMessage::from_message)
            .collect();
        let to_turn = batch.iter().map(|m| m.turn).max().unwrap_or(0);
        candidates.push((scene_id.clone(), batch, to_turn));
    }
    // 最老的场景先沉淀（按批次末轮升序；并列时按场景名，保持确定性）
    candidates.sort_by(|a, b| {
        (a.2, &a.0)
            .cmp(&(b.2, &b.0))
    });
    candidates.into_iter().next()
}

/// 角色的 needs/values（设定集 char 实体的倾向性；心理评价的对照清单，设计 §9.2）
fn codex_needs(cx: &codex::Codex, name: &str) -> Vec<String> {
    let Some(entity) = cx.entities().iter().find(|e| e.name == name || e.id == name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["needs", "values", "motivation", "interests"] {
        match entity.facts.get(key) {
            Some(serde_json::Value::String(s)) => out.push(s.clone()),
            Some(serde_json::Value::Array(list)) => {
                out.extend(list.iter().filter_map(|v| v.as_str().map(str::to_string)))
            }
            _ => {}
        }
    }
    out
}

/// 轮末异步总结（设计 §5.3：不阻塞对话；同一会话并发时跳过）。
/// `active` = 当轮激活的实体 id（M3.10 关联审计的对照表；空 = 没有记录）。
pub(crate) fn spawn_summary(
    root: &std::path::Path,
    session_id: &str,
    flags: &SummaryFlags,
    active: Vec<String>,
) {
    // 加固 A4：已有暂存批次等着重放——同一批消息不再跑一遍总结（防重复落模型产物）
    if gate().has_pending(session_id) {
        return;
    }
    if !flags.begin(session_id) {
        return; // 上一次总结还在跑
    }
    let root = root.to_path_buf();
    let session_id = session_id.to_string();
    let flags = flags.clone();
    // 后台任务用自己的 EventLog 实例（读盘 + 追加；主缓存靠字节偏移自动跟上）。
    // 无论成败都要释放标记：不释放的话，第一次失败后这个会话的总结就永远不再触发。
    tauri::async_runtime::spawn(async move {
        let outcome = run_summary(root, session_id.clone(), false, active).await;
        flags.end(&session_id);
        if let Err(e) = outcome {
            crate::diag::record("summary", format!("总结失败：{e}"));
        }
    });
}

/// 重放暂存的总结批次（加固 A4）：前台命令收尾时调用——闸门空出的确定性时机。
/// 同步执行（无 LLM，纯落盘）；拿不到闸门（紧接的下一个命令抢了先）就原样保留，
/// 等下次收尾再试。
pub(crate) fn flush_parked_summaries(root: &std::path::Path, session_id: &str) -> usize {
    let mut flushed = 0;
    while gate().has_pending(session_id) {
        if !gate().try_acquire(session_id) {
            return flushed; // 别人先占了：等它的收尾再试
        }
        let batch = gate().take_parked(session_id);
        match batch {
            Some(batch) => match apply_parked_summary(root, session_id, &batch) {
                Ok(()) => {
                    flushed += 1;
                    crate::diag::record(
                        "summary",
                        format!(
                            "暂存批次重放完成（第 {}–{} 轮，场景 {}）",
                            batch.from_turn, batch.to_turn, batch.scene_id
                        ),
                    );
                }
                Err(e) => {
                    crate::diag::record("summary", format!("暂存批次重放失败（丢弃）：{e}"));
                }
            },
            None => break,
        }
        gate().release(session_id);
    }
    flushed
}

/// 把一个暂存批次落成事件（上下文按当前事件流重derive；闸门由调用方持有）
fn apply_parked_summary(
    root: &std::path::Path,
    session_id: &str,
    batch: &ParkedSummary,
) -> Result<(), String> {
    let log = store::EventLog::new();
    let meta = store::load_session(root, session_id).map_err(|e| e.to_string())?;
    let world = session_world(&meta);
    let cx = load_codex(root, None, &world);
    let proj = project_session(&log, root, &meta)?;
    apply_summary_outcome_locked(
        &log,
        root,
        &meta,
        &cx,
        &proj,
        batch.outcome.clone(),
        batch.from_turn,
        batch.to_turn,
        batch.story_day,
        &batch.story_clock,
        &batch.scene_id,
    )?;
    Ok(())
}

/// 故事时间前缀表（加固 D8 · M3.0 ⑤）：从事件流一次遍历折出「黑板事件按轮」的
/// 故事时刻，之后按轮二分查询。批次总结的情景记忆盖**事发时刻**的章——原实现
/// 每条记忆都全量扫一遍事件流（O(n×k)），一批 10–30 条的长会话是实打实的平方级。
///
/// 语义与原线性扫描逐字节一致：记录序里最后一个 `turn <= 查询轮` 的黑板事件；
/// 事件流的黑板轮次非降（正常写入恒真）走二分，万一乱序退回线性扫保持语义。
pub(crate) struct StoryTimeline {
    /// 记录序的 (turn, day, clock)
    rows: Vec<(u64, i64, String)>,
    /// turn 非降（二分可用）
    pub(crate) sorted: bool,
}

impl StoryTimeline {
    pub(crate) fn build(records: &[LogRecord]) -> Self {
        let mut rows: Vec<(u64, i64, String)> = Vec::new();
        let mut sorted = true;
        for r in records {
            if let LogBody::Blackboard(b) = &r.body {
                if let Some(last) = rows.last() {
                    if b.turn < last.0 {
                        sorted = false;
                    }
                }
                rows.push((b.turn, b.board.day, b.board.clock.clone()));
            }
        }
        StoryTimeline { rows, sorted }
    }

    /// 第 turn 轮结束时（含该轮）最后一次黑板事件的故事时间
    pub(crate) fn at(&self, turn: u64) -> Option<(i64, String)> {
        if self.sorted {
            let idx = self.rows.partition_point(|(t, _, _)| *t <= turn);
            idx.checked_sub(1)
                .map(|i| (self.rows[i].1, self.rows[i].2.clone()))
        } else {
            self.rows
                .iter()
                .rev()
                .find(|(t, _, _)| *t <= turn)
                .map(|(_, d, c)| (*d, c.clone()))
        }
    }
}

/// 把总结产物落成事件（与 Tauri 无关，便于单测）：摘要增量、情景记忆、L3 事实、设定提案。
///
/// 一切 LLM 产物都**先落草稿/提案**，注入只认 canon（设计 §6.9）——唯一的例外是 L3 事实与
/// 情景记忆：它们是「角色的亲身经历」，本就不进设定注入，而是走宫殿召回（§5.2）。
///
/// 加固 A4：这是**闸门管理版**——落盘前 try-acquire 写入闸门，拿不到（前台正在
/// 生成/编辑）就整批暂存待重放，绝不与前台 rewrite 交错。闸门持有期内的实际落盘
/// 走 [`apply_summary_outcome_locked`]。
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_summary_outcome(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cx: &codex::Codex,
    // 投影视图（M3.8 瞬时状态写黑板要取场景分区/世界层的现状）
    proj: &event::Projection,
    outcome: summarize::SummaryOutcome,
    from_turn: u64,
    to_turn: u64,
    story_day: i64,
    story_clock: &str,
    // 本批归属的场景（M3.2 摘要分卷）：场景卷进 scene_summaries，大事记进世界层
    scene_id: &str,
) -> Result<SummaryApply, String> {
    let session_id = meta.id.as_str();
    if !gate().try_acquire(session_id) {
        gate().park_summary(
            session_id,
            ParkedSummary {
                outcome,
                from_turn,
                to_turn,
                story_day,
                story_clock: story_clock.to_string(),
                scene_id: scene_id.to_string(),
            },
        );
        crate::diag::record(
            "summary",
            format!(
                "会话写入中，第 {from_turn}–{to_turn} 轮总结批次已暂存待重放"
            ),
        );
        return Ok(SummaryApply::Parked);
    }
    let applied = apply_summary_outcome_locked(
        log, root, meta, cx, proj, outcome, from_turn, to_turn, story_day, story_clock, scene_id,
    );
    gate().release(session_id);
    applied.map(SummaryApply::Applied)
}

/// 落盘本体（闸门由调用方持有；与 Tauri 无关，便于单测）。
#[allow(clippy::too_many_arguments)]
fn apply_summary_outcome_locked(
    log: &store::EventLog,
    root: &std::path::Path,
    meta: &store::SessionMeta,
    cx: &codex::Codex,
    // 投影视图（M3.8 瞬时状态写黑板要取场景分区/世界层的现状）
    proj: &event::Projection,
    outcome: summarize::SummaryOutcome,
    from_turn: u64,
    to_turn: u64,
    story_day: i64,
    story_clock: &str,
    // 本批归属的场景（M3.2 摘要分卷）：场景卷进 scene_summaries，大事记进世界层
    scene_id: &str,
) -> Result<usize, String> {
    let character = first_character(meta)?;
    let mut applied = 0usize;
    let ts = store::unix_now();
    // D1-1：整批产物收拢进一个 batch，末尾一次 commit_batch（一次投影 + 一次派生同步）
    let mut bodies: Vec<LogBody> = Vec::new();
    // D8：故事时间前缀表——一次遍历，之后每条记忆 O(log n) 查询
    let timeline = StoryTimeline::build(&log.read(root, &meta.id).map_err(|e| e.to_string())?);

    if !outcome.summary_delta.trim().is_empty() {
        bodies.push(
            LogBody::Summary(event::SummaryEvent {
                turn: to_turn,
                delta: outcome.summary_delta.clone(),
                from_turn,
                to_turn,
                scene_id: Some(scene_id.to_string()),
                ts,
            }),
        );
        applied += 1;
    }
    // 世界层大事记（仅公开事件）：单独成卷，任何场景组装时都能读到
    if !outcome.chronicle.trim().is_empty() {
        bodies.push(
            LogBody::Summary(event::SummaryEvent {
                turn: to_turn,
                delta: outcome.chronicle.clone(),
                from_turn,
                to_turn,
                scene_id: None,
                ts,
            }),
        );
        applied += 1;
    }

    // 情景记忆 + L3 事实：都作为记忆对象进宫殿（episode 走召回，fact 也可被 recall 命中）
    let base = project_session(log, root, meta)
        .map(|p| p.episodes.len() + p.memory.len())
        .unwrap_or(0);
    for (i, ep) in outcome.episodes.iter().enumerate() {
        // 情景记忆盖**事发时刻**的章（M3.0 ⑤）：批次内消息的故事时间从事件流折出，
        // 而不是管线运行时的当前黑板；事件流里查不到才回退当前（例如 force 总结远古批次）
        let ep_turn = ep.turns.first().copied().unwrap_or(to_turn);
        let (ep_day, ep_clock) = timeline
            .at(ep_turn)
            .unwrap_or((story_day, story_clock.to_string()));
        let mut obj = palace::MemObject {
            id: palace::next_id(base + i + 1),
            kind: palace::KIND_EPISODE.to_string(),
            content: ep.content.clone(),
            turn: ep_turn,
            story_day: ep_day,
            story_clock: ep_clock,
            place: ep.place.clone(),
            actors: if ep.actors.is_empty() {
                vec![character.clone()]
            } else {
                ep.actors.clone()
            },
            witnesses: ep.witnesses.clone(),
            salience: ep.salience,
            emotion: ep.emotion.clone(),
            links: ep.links.clone(),
            thread: ep.thread.clone(),
            source: "pipeline.summary".into(),
            ts,
            rehearsals: 0,
        };
        if obj.witnesses.is_empty() {
            obj.witnesses = obj.actors.clone();
        }
        let object = serde_json::to_value(&obj).map_err(|e| e.to_string())?;
        bodies.push(
            LogBody::Memory(event::MemoryEvent {
                turn: obj.turn,
                origin: "pipeline".into(),
                object,
                ts,
            }),
        );
        applied += 1;
    }

    // 转述记忆（M3.3 · 设计 §10.4）：A 告知 B → 为每个听众各写一条 hearsay
    // （content=转述内容、source=告知者、salience 折半、links 继承）——信息跨视角流动的
    // 唯一通道，召回行尾自带「转述自X」（palace::render_memory_line），翻旧账有据可查。
    // 每位听众 witnesses = 她自己：没在名单里的角色召不回这件事（视角过滤是硬约束）。
    // 盖的章是**告知发生的时刻**（听见的时刻，不是原事件时刻）——故事时间从事件流折出，
    // 与情景记忆同一口径（M3.0 ⑤）。
    let mut hearsay_slots = base + outcome.episodes.len();
    for hs in &outcome.hearsays {
        let hs_turn = hs.turns.first().copied().unwrap_or(to_turn);
        let (hs_day, hs_clock) = timeline
            .at(hs_turn)
            .unwrap_or((story_day, story_clock.to_string()));
        for listener in &hs.listeners {
            hearsay_slots += 1;
            let obj = palace::MemObject {
                id: palace::next_id(hearsay_slots),
                kind: palace::KIND_HEARSAY.to_string(),
                content: hs.content.clone(),
                turn: hs_turn,
                story_day: hs_day,
                story_clock: hs_clock.clone(),
                place: hs.place.clone(),
                actors: vec![hs.source.clone(), listener.clone()],
                witnesses: vec![listener.clone()],
                salience: hs.salience * palace::HEARSAY_SALIENCE_FACTOR,
                emotion: hs.emotion.clone(),
                links: hs.links.clone(),
                thread: hs.thread.clone(),
                source: hs.source.clone(),
                ts,
                rehearsals: 0,
            };
            let object = serde_json::to_value(&obj).map_err(|e| e.to_string())?;
            bodies.push(
                LogBody::Memory(event::MemoryEvent {
                    turn: obj.turn,
                    origin: "pipeline".into(),
                    object,
                    ts,
                }),
            );
            applied += 1;
        }
    }
    // 转述顺带揭示的秘密（M3.3 与 M3.1 判定闭环）：听众从此「知情」——视角揭示集
    // 增项后，M3.1 的深卡判定（known_for + 激活源 3b）对她展开秘密卡，对别人照旧关门。
    // origin = pipeline：模型产物随事件流重放（is_derived 只认 tree 来源），编辑历史不丢。
    for hs in &outcome.hearsays {
        for target in &hs.reveals {
            bodies.push(
                LogBody::Codex(event::CodexEvent {
                    turn: hs.turns.first().copied().unwrap_or(to_turn),
                    op: "reveal".into(),
                    target: target.clone(),
                    origin: "pipeline".into(),
                    value: None,
                    note: Some(format!("听{}说起", hs.source)),
                    witnesses: hs.listeners.clone(),
                    ts,
                }),
            );
            applied += 1;
        }
    }
    // L3 事实：批次末的故事时间（事实是批次里学到的，同样不该盖总结时刻的章）
    let (fact_day, fact_clock) = timeline
        .at(to_turn)
        .unwrap_or((story_day, story_clock.to_string()));
    // id 顺延在转述之后（转述记忆一条提案 × 每位听众各占一号）
    let fact_base =
        base + outcome.episodes.len() + outcome.hearsays.iter().map(|h| h.listeners.len()).sum::<usize>();
    for (i, fact) in outcome.facts.iter().enumerate() {
        let mut obj = palace::MemObject {
            id: palace::next_id(fact_base + i + 1),
            kind: palace::KIND_FACT.to_string(),
            content: format!("{}：{}", fact.key, fact.value),
            turn: to_turn,
            story_day: fact_day,
            story_clock: fact_clock.clone(),
            place: None,
            actors: vec![character.clone()],
            witnesses: vec![character.clone()],
            salience: 0.6, // L3 事实：跨会话持久的键值，权重高于普通情景（设计 §5.1 分层责任）
            emotion: None,
            links: vec![format!("topic:{}", fact.key)],
            thread: None,
            source: "pipeline.summary".into(),
            ts,
            rehearsals: 0,
        };
        let object = serde_json::to_value(&obj).map_err(|e| e.to_string())?;
        bodies.push(
            LogBody::Memory(event::MemoryEvent {
                turn: to_turn,
                origin: "pipeline".into(),
                object,
                ts,
            }),
        );
        applied += 1;
    }

    // 关联审计（M3.10 · 设计 §6.13）：三类发现两路走——
    //   missed / facet → 收件箱提示条目（kind="audit"，确认与否决只改状态，不物化）；
    //   fact → 并入 codex 提案走同一条链路（anchors 驳回 + 分级 + 物化，reason 带标记）。
    // 审计是 LLM 产物：提案事件化（重放不重调模型），与 §5.3 同构。
    for (i, a) in outcome.audit.iter().enumerate() {
        if a.finding == summarize::AUDIT_FACT {
            continue; // 与 codex 合并处理，见下
        }
        bodies.push(proposal_event(
            to_turn,
            format!("audit.{}.{}.{}", a.target, to_turn, i),
            "propose",
            "audit",
            "pipeline",
            Some(serde_json::json!({
                "finding": a.finding,
                "target": a.target,
                "facet": a.facet,
                "evidence": a.evidence,
            })),
            Some(a.evidence.clone()),
            ts,
        ));
        applied += 1;
    }
    let mut codex_drafts: Vec<summarize::CodexDraft> = outcome.codex.clone();
    for a in &outcome.audit {
        if a.finding != summarize::AUDIT_FACT {
            continue;
        }
        codex_drafts.push(summarize::CodexDraft {
            kind: summarize::CODEX_NEW_FACT.into(),
            target: a.target.clone(),
            value: serde_json::json!({ "facet": a.facet, "value": a.value }),
            reason: format!("关联审计：{}", a.evidence),
        });
    }

    // 设定提案：运行期捕获分级（M3.8 · 设计 §6.8-2）+ anchors 驳回（§6.8 最高保护级）
    //   瞬时状态 → 直接写黑板（不进收件箱）；既有实体小事实 → 按配置自动接受；
    //   全新实体 / 关系 / 改写 → 收件箱人工。
    let auto_minor = store::load_settings(root)
        .map(|s| s.auto_accept_minor_facts)
        .unwrap_or(false);
    let world = session_world(meta);
    for (i, draft) in codex_drafts.iter().enumerate() {
        let payload = serde_json::json!({
            "target": draft.target,
            "value": draft.value,
            "reason": draft.reason,
        });
        let conflict = cx
            .get(&draft.target)
            .and_then(|e| codex::anchors_conflict(e, &draft.value));
        let id = format!("codex.{}.{}.{}", draft.target, to_turn, i);
        if let Some(reason) = conflict {
            crate::diag::record(
                "summary",
                format!("设定提案与辨识点冲突，已驳回：{}（{}）", draft.target, reason),
            );
            bodies.push(proposal_event(
                to_turn,
                id,
                "reject",
                &draft.kind,
                "pipeline",
                Some(payload),
                Some(format!("与辨识点冲突，自动驳回：{reason}")),
                ts,
            ));
            applied += 1;
            continue;
        }
        match complete::capture_grade(&draft.kind, &draft.target, cx) {
            complete::CaptureGrade::Transient => {
                // 直接写黑板：键值进 extra（场景 flags 或世界层）；黑板事件就是记录，
                // 不进收件箱——瞬时状态不值得人工审（§6.8-2）
                let Some((key, val)) = transient_key_value(&draft.value) else {
                    continue;
                };
                let mut board = match proj.scenes.get(scene_id) {
                    Some(sc) => store::Blackboard {
                        day: sc.day,
                        clock: sc.clock.clone(),
                        place: sc.place.clone(),
                        actors: sc.actors.clone(),
                        extra: sc.flags.clone(),
                    },
                    None => proj
                        .blackboard
                        .clone()
                        .unwrap_or_else(store::Blackboard::default_board),
                };
                board.extra.insert(key, val);
                bodies.push(
                    LogBody::Blackboard(event::BlackboardEvent {
                        turn: to_turn,
                        reason: "pipeline".into(),
                        board,
                        scene_id: Some(scene_id.to_string()),
                        ts,
                    }),
                );
                applied += 1;
            }
            grade if grade == complete::CaptureGrade::MinorFact && auto_minor => {
                // 既有实体的小事实 + 用户开了自动接受：连落 propose 与 accept 两条事件
                // （动作可溯源），物化与手动确认同一条路
                bodies.push(proposal_event(
                    to_turn,
                    id.clone(),
                    "propose",
                    &draft.kind,
                    "pipeline",
                    Some(payload.clone()),
                    None,
                    ts,
                ));
                bodies.push(proposal_event(
                    to_turn,
                    id,
                    "accept",
                    &draft.kind,
                    "pipeline",
                    None,
                    Some("小事实自动接受（设置：运行期自动接受）".into()),
                    ts,
                ));
                materialize_accepted(root, &world, &draft.kind, payload);
                applied += 2;
            }
            _ => {
                bodies.push(proposal_event(
                    to_turn,
                    id,
                    "propose",
                    &draft.kind,
                    "pipeline",
                    Some(payload),
                    None,
                    ts,
                ));
                applied += 1;
            }
        }
    }

    // 剧情线提案（含提及时机起草，设计 §8.3）
    for (i, draft) in outcome.threads.iter().enumerate() {
        let payload = serde_json::json!({
            "title": draft.title,
            "cause": draft.cause,
            "actors": draft.actors,
            "importance": draft.importance,
            "resurface": draft.resurface_value(),
        });
        bodies.push(proposal_event(
            to_turn,
            format!("thread.{}.{}.{}", to_turn, i, draft.title),
            "propose",
            "thread",
            "pipeline",
            Some(payload),
            Some(draft.framing.clone()),
            ts,
        ));
        applied += 1;
    }

    // 心理评价提案（需要满足/受挫 → 情绪与意图，设计 §9.2）
    for (i, draft) in outcome.psyche.iter().enumerate() {
        let payload = serde_json::json!({
            "kind": draft.kind,
            "name": draft.name,
            "intensity": draft.intensity,
            "source": draft.source,
        });
        bodies.push(proposal_event(
            to_turn,
            format!("psyche.{}.{}.{}", to_turn, i, draft.name),
            "propose",
            "psyche",
            "pipeline",
            Some(payload),
            None,
            ts,
        ));
        applied += 1;
    }

    // D1-1：整批一次落盘
    commit_batch(log, root, meta, bodies)?;
    Ok(applied)
}

/// 跑一次总结（批次 → 便宜档 provider → 事件落盘）。
///
/// 设计 §5.3：轮末**异步**触发，不阻塞对话；失败只留诊断，下轮或手动可重试。
async fn run_summary(
    root: std::path::PathBuf,
    session_id: String,
    force: bool,
    active_entities: Vec<String>,
) -> Result<String, String> {
    let log = store::EventLog::new();
    let meta = store::load_session(&root, &session_id).map_err(|e| e.to_string())?;
    let character = first_character(&meta)?;
    let loaded = card::load_card(&root, &character).map_err(|e| e.to_string())?;
    let proj = project_session(&log, &root, &meta)?;

    // 场景维度的批次（M3.2）：force 时连活跃场景的最近窗口一并消化
    let (scene_id, mut batch, mut to_turn) = match summary_batch_scenes(&proj, force) {
        Some(b) => b,
        None if !force => return Ok("没有待总结的批次".into()),
        None => return Ok("没有待总结的消息".into()),
    };
    // 批次分块：一次失败的总结会让积压越滚越大（从未总结的消息全堆进下一次调用），
    // 推理型模型的思考 token 随批单调涨——直到永远挤不出正文（真机压测 2026-09-20
    // 复现：首批成功后一次失败，此后 4 连败全为空正文）。每次只消化最老的
    // SUMMARY_CHUNK 条，剩下的留给下一轮末继续补，失败也永远有进度。
    if batch.len() > SUMMARY_CHUNK {
        batch.truncate(SUMMARY_CHUNK);
        to_turn = batch.iter().map(|m| m.turn).max().unwrap_or(0);
    }
    let from_turn = batch.iter().map(|m| m.turn).min().unwrap_or(0);

    let provider = pick_util_provider(&root)?;
    let world = session_world(&meta);
    let cx = load_codex(&root, None, &world);
    // 故事时钟取该场景的局部时钟（被冻结的场景按它冻结时的时刻总结）
    let board = proj.effective_board(Some(scene_id.as_str()));
    let scene_label = proj
        .scenes
        .get(&scene_id)
        .map(|sc| if sc.title.is_empty() { sc.place.clone() } else { sc.title.clone() });
    let active_threads: Vec<String> = proj
        .threads
        .values()
        .filter_map(|v| threads::Thread::from_value(v).ok())
        .filter(|t| t.is_active())
        .map(|t| format!("{}（{}）", t.id, t.title))
        .collect();
    let needs = codex_needs(&cx, &loaded.card.name);
    // 关联审计的对照表（M3.10 · §6.13）：设定集实体清单（canon，id + 名 + 一句话）
    let entity_catalog: Vec<String> = cx
        .entities()
        .iter()
        .filter(|e| e.is_canon())
        .map(|e| format!("{}（{}）{}", e.id, e.name, e.one_liner))
        .collect();
    let ctx = summarize::SummaryContext {
        card_name: &loaded.card.name,
        persona_name: meta.persona.as_deref(),
        premise: meta.premise.as_deref(),
        scene_label: scene_label.as_deref(),
        story_clock: &board.clock,
        rolling_summary: &proj.summary_for(Some(scene_id.as_str())).unwrap_or_default(),
        active_threads: &active_threads,
        needs: &needs,
        entity_catalog: &entity_catalog,
        active_entities: &active_entities,
    };
    let prompt_text = summarize::build_prompt(&ctx, &batch);

    let proxy = proxy_of(&root);
    let raw = llm::chat_complete(
        &provider,
        &[llm::ChatMessage {
            role: "user".into(),
            content: prompt_text,
        }],
        // 推理型模型的思考 token 计入 max_tokens：给太小会「正文为空、全部耗在思考上」
        //（真机压测 2026-09-20：deepseek-flash 在 1600 下稳定返回空正文）。六类产物
        // 的 JSON 本体 + 思考各需 2–4k，8192 才留得住正文；批次本身另有 16 条的分块封顶。
        8192,
        0.3,
        proxy.as_deref(),
    )
    .await?;
    let outcome = summarize::sanitize(
        summarize::parse_outcome(&raw).map_err(|e| format!("总结回复解析失败：{e}"))?,
    );
    let applied = apply_summary_outcome(
        &log,
        &root,
        &meta,
        &cx,
        &proj,
        outcome,
        from_turn,
        to_turn,
        board.day,
        &board.clock,
        &scene_id,
    )?;
    let applied_msg = match applied {
        SummaryApply::Applied(n) => format!("落 {n} 条事件"),
        SummaryApply::Parked => "会话写入中，批次已暂存待重放".to_string(),
    };
    crate::diag::record(
        "summary",
        format!(
            "总结第 {from_turn}–{to_turn} 轮（场景 {}）：{applied_msg}（provider={}）",
            scene_id,
            provider.name
        ),
    );
    Ok(format!(
        "已总结第 {from_turn}–{to_turn} 轮（场景 {scene_id}）：{applied_msg}"
    ))
}

/// 手动触发一次总结（设置页/排查用；正常路径是轮末自动触发）
#[tauri::command]
pub async fn summarize_now(session_id: String) -> Result<String, String> {
    // 手动触发没有「当轮激活记录」（审计对照表里的激活名单给空，提示词里写明）
    run_summary(root(), session_id, true, Vec::new()).await
}
