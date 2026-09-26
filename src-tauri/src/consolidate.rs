//! 宫殿睡眠整理（M4.4 · 设计 §5.4 · 决断 4/5）。
//!
//! 低显著度且彼此相似的记忆由 util 档合并成一段合并稿：合并稿是新增 episode
//! 记忆（`source = consolidation`，salience = 组内最大），原记忆打 archived 标记
//! （同 id 整对象重发，事件流折叠按 id 后写覆盖），合并稿追加进对应场景卷摘要的
//! 「章节归档」段。全部走事件流——模型产物重放不重调模型；记忆数据零物理删除
//! （决断 5：**归档不复活**，「被再次提及则回升」只作用于未归档记忆）。
//!
//! 宿主确定性部分（候选筛选 + 贪心分组）在本模块，LLM 只负责每组一段合并稿；
//! 失败/空稿/超长的组放弃不阻塞其他组（没有合并稿就不归档，原文入口不丢）。

use crate::event::{self, LogBody};
use crate::palace::{self, MemObject};

/// 合并稿/归档标记事件的 origin（与 pipeline / hook 并列）。
pub const ORIGIN: &str = "consolidation";
/// 合并稿记忆的 source。
pub const SOURCE: &str = "consolidation";
/// 组的最小/最大条数：不足 2 条不成组（单条没有「合并」），满 8 条不再收。
pub const MIN_GROUP: usize = 2;
pub const MAX_GROUP: usize = 8;
/// 合并稿长度上限（字符）：超长视为失控输出，该组放弃。
pub const DRAFT_MAX_CHARS: usize = 400;

// ---------- 候选与分组（宿主确定性：同输入同输出，无哈希迭代序参与） ----------

/// 候选：kind ∈ {episode, hearsay}、未归档、salience 按故事时钟衰减后仍 < 阈值。
/// 按 id 升序输出（分组贪心的确定性输入序）。
pub fn candidates(objs: &[MemObject], now_day: i64, threshold: f64) -> Vec<MemObject> {
    let mut out: Vec<MemObject> = objs
        .iter()
        .filter(|m| {
            if m.archived {
                return false;
            }
            if m.kind != palace::KIND_EPISODE && m.kind != palace::KIND_HEARSAY {
                return false;
            }
            let elapsed = (now_day - m.story_day) as f64;
            let decayed = palace::sanitize_salience(m.salience) as f64 * palace::decay_factor(elapsed);
            decayed < threshold
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 亲和分级：0 = 同 thread，1 = 同 place，2 = 与组的 links 并集重叠 ≥2。
/// None = 三级都不命中（候选自开新组）。
fn affinity(m: &MemObject, group: &[MemObject]) -> Option<u8> {
    let trim_empty = |s: &str| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    if let Some(t) = m.thread.as_deref().and_then(trim_empty) {
        if group
            .iter()
            .any(|o| o.thread.as_deref().and_then(trim_empty).as_deref() == Some(&t))
        {
            return Some(0);
        }
    }
    if let Some(p) = m.place.as_deref().and_then(trim_empty) {
        if group
            .iter()
            .any(|o| o.place.as_deref().and_then(trim_empty).as_deref() == Some(&p))
        {
            return Some(1);
        }
    }
    let norm = |s: &str| s.trim().to_lowercase().replace('：', ":");
    let group_links: std::collections::BTreeSet<String> = group
        .iter()
        .flat_map(|o| o.links.iter().map(|l| norm(l)))
        .filter(|l| !l.is_empty())
        .collect();
    let overlap = m
        .links
        .iter()
        .map(|l| norm(l))
        .filter(|l| !l.is_empty() && group_links.contains(l))
        .count();
    if overlap >= 2 {
        return Some(2);
    }
    None
}

/// 贪心分组：候选按 id 升序依次入组；每条找亲和最高（同 thread > 同 place >
/// links 重叠 ≥2，同级取最早成组）且未满 8 条的组加入，全不命中则自开新组；
/// 最后不足 MIN_GROUP 的组丢弃。同样的输入必然得到同样的分组。
pub fn plan_groups(cands: &[MemObject]) -> Vec<Vec<MemObject>> {
    let mut groups: Vec<Vec<MemObject>> = Vec::new();
    for c in cands {
        let mut best: Option<(u8, usize)> = None;
        for (idx, g) in groups.iter().enumerate() {
            if g.len() >= MAX_GROUP {
                continue;
            }
            if let Some(level) = affinity(c, g) {
                if best.map_or(true, |(bl, _)| level < bl) {
                    best = Some((level, idx));
                }
            }
        }
        match best {
            Some((_, idx)) => groups[idx].push(c.clone()),
            None => groups.push(vec![c.clone()]),
        }
    }
    groups.into_iter().filter(|g| g.len() >= MIN_GROUP).collect()
}

/// 现有记忆里的最大 `mem_XXXX` 序号 + 1（没有可解析的 id → 1）。
pub fn next_index_of(objs: &[MemObject]) -> usize {
    objs.iter()
        .filter_map(|m| m.id.strip_prefix("mem_").and_then(|s| s.parse::<usize>().ok()))
        .max()
        .unwrap_or(0)
        + 1
}

// ---------- 合并稿（util 档每组的输入输出） ----------

/// 每组一次 util 档调用的提示词：合并成 100–200 字第三人称回忆梗概，不发明新信息。
pub fn build_prompt(group: &[MemObject]) -> String {
    let mut lines = String::new();
    for m in group {
        lines.push_str(&format!(
            "- 第{}天 {}：{}（在场：{}）\n",
            m.story_day,
            m.story_clock.trim(),
            m.content.trim(),
            m.actors.join("、"),
        ));
    }
    format!(
        "下面是同一个故事里的 {} 条旧记忆，它们彼此相似、显著度已经很低。\
请把它们合并写成一段 100–200 字的第三人称回忆梗概：保留人物、地点与关键事实，\
不要发明新信息，不要逐条罗列，只输出合并稿正文。\n\n{lines}",
        group.len()
    )
}

/// 合并稿解析：宽容剥掉代码围栏后取正文；空稿/超长（> DRAFT_MAX_CHARS 字符）= None。
pub fn parse_draft(raw: &str) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            continue;
        }
        lines.push(line);
    }
    let text = lines.join("\n").trim().to_string();
    if text.is_empty() || text.chars().count() > DRAFT_MAX_CHARS {
        return None;
    }
    Some(text)
}

// ---------- 落盘（全部构造为事件，由调用方 commit_batch 一次写入） ----------

/// 整理结论（命令层透出给面板）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ConsolidateReport {
    /// 分到的组数
    pub groups: usize,
    /// 成功落合并稿的组数
    pub merged: usize,
    /// 归档的原记忆条数
    pub archived: usize,
    /// 放弃的组与原因（组号从 1 起）
    pub skipped: Vec<String>,
}

/// 由分组与各组合并稿构造事件批次：
/// 合并稿 Memory（新 id）+ 组摘要「章节归档」Summary + 成员归档 Memory（同 id 重发）。
/// `drafts` 与 `plans` 等长；None = 该组放弃（原记忆不归档）。
/// `next_index` = 新记忆 id 起始序号；`scene_of_place` 把组内地点映射到场景 id
/// （None = 世界层卷）。事件顺序：合并稿在前、归档在后（重放先 push 新稿再覆盖成员）。
pub fn build_bodies(
    plans: &[Vec<MemObject>],
    drafts: &[Option<String>],
    next_index: usize,
    now_day: i64,
    scene_of_place: &dyn Fn(Option<&str>) -> Option<String>,
    ts: u64,
) -> (Vec<LogBody>, ConsolidateReport) {
    let mut bodies: Vec<LogBody> = Vec::new();
    let mut report = ConsolidateReport {
        groups: plans.len(),
        merged: 0,
        archived: 0,
        skipped: Vec::new(),
    };
    let mut next = next_index;
    let dedup_push = |out: &mut Vec<String>, v: &[String]| {
        for s in v {
            let t = s.trim();
            if !t.is_empty() && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
    };
    for (gi, (group, draft)) in plans.iter().zip(drafts.iter()).enumerate() {
        let Some(text) = draft.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            report.skipped.push(format!("第{}组：没有可用的合并稿", gi + 1));
            continue;
        };
        let mut actors: Vec<String> = Vec::new();
        let mut witnesses: Vec<String> = Vec::new();
        let mut links: Vec<String> = Vec::new();
        for m in group {
            dedup_push(&mut actors, &m.actors);
            dedup_push(&mut witnesses, &m.witnesses_or_actors());
            dedup_push(&mut links, &m.links);
        }
        let max_salience = group
            .iter()
            .map(|m| palace::sanitize_salience(m.salience))
            .fold(0.0f32, f32::max);
        let turn = group.iter().map(|m| m.turn).max().unwrap_or(0);
        let from_turn = group.iter().map(|m| m.turn).min().unwrap_or(0);
        let place = group.iter().find_map(|m| m.place.clone());
        let thread = group
            .iter()
            .find_map(|m| m.thread.clone().filter(|t| !t.trim().is_empty()));

        let merged = MemObject {
            id: palace::next_id(next),
            kind: palace::KIND_EPISODE.to_string(),
            content: text.to_string(),
            turn,
            story_day: now_day,
            story_clock: String::new(),
            place: place.clone(),
            actors: actors.clone(),
            witnesses,
            salience: max_salience,
            emotion: None,
            links,
            thread,
            source: SOURCE.to_string(),
            ts,
            rehearsals: 0,
            archived: false,
        };
        next += 1;
        bodies.push(LogBody::Memory(event::MemoryEvent {
            turn: merged.turn,
            origin: ORIGIN.to_string(),
            object: serde_json::to_value(&merged).expect("MemObject 可序列化"),
            ts,
        }));
        bodies.push(LogBody::Summary(event::SummaryEvent {
            // 归档段不声明批次覆盖（from/to_turn = 0）：水位取 max 不回退，
            // 0 意味着整理不虚报「已总结到哪」，总结管线的真实水位不受扰
            turn,
            delta: format!("【章节归档】{text}"),
            from_turn: 0,
            to_turn: 0,
            scene_id: scene_of_place(place.as_deref()),
            ts,
        }));
        for m in group {
            let mut archived_obj = m.clone();
            archived_obj.archived = true;
            bodies.push(LogBody::Memory(event::MemoryEvent {
                turn: m.turn,
                origin: ORIGIN.to_string(),
                object: serde_json::to_value(&archived_obj).expect("MemObject 可序列化"),
                ts,
            }));
        }
        report.merged += 1;
        report.archived += group.len();
    }
    (bodies, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event;

    fn mem(id: usize, content: &str, salience: f32, day: i64) -> MemObject {
        MemObject {
            id: palace::next_id(id),
            kind: palace::KIND_EPISODE.to_string(),
            content: content.to_string(),
            turn: id as u64,
            story_day: day,
            story_clock: "12:00".to_string(),
            place: Some("图书馆".to_string()),
            actors: vec!["小雨".to_string()],
            witnesses: vec!["小雨".to_string()],
            salience,
            emotion: None,
            links: vec!["topic:便签".to_string()],
            thread: None,
            source: "pipeline.summary".to_string(),
            ts: 1,
            rehearsals: 0,
            archived: false,
        }
    }

    #[test]
    fn candidates_filter_by_kind_archived_and_decayed_salience() {
        let mut objs = vec![
            mem(1, "高显著新近", 0.9, 10),
            mem(2, "低显著且久远", 0.3, 1),
            mem(3, "转述也候选", 0.2, 1),
            mem(4, "键值事实不进", 0.1, 1),
            mem(5, "中显著近事", 0.4, 9),
        ];
        objs[3].kind = palace::KIND_FACT.to_string();
        objs[2].kind = palace::KIND_HEARSAY.to_string();
        let mut archived = mem(6, "已归档不再进", 0.1, 1);
        archived.archived = true;
        objs.push(archived);

        // 第 10 天看：0.3×0.5^(9/7)≈0.12 < 0.25 进候选；0.4×0.5^(1/7)≈0.37 不进
        let cands = candidates(&objs, 10, 0.25);
        let ids: Vec<&str> = cands.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["mem_0002", "mem_0003"], "只收低显著 episode/hearsay，归档与 fact 排除");
    }

    #[test]
    fn plan_groups_is_deterministic_and_respects_bounds() {
        // 20 条同 place 低显著 → 贪心成 8+8+4，最后的 4 条组不足 2 丢弃?——不，4 ≥ 2 保留
        let cands: Vec<MemObject> = (1..=20).map(|i| mem(i, "相似旧事", 0.1, 1)).collect();
        let g1 = plan_groups(&cands);
        let g2 = plan_groups(&cands);
        assert_eq!(g1, g2, "同输入同分组");
        assert_eq!(g1.iter().map(|g| g.len()).collect::<Vec<_>>(), vec![8, 8, 4], "满 8 封顶");

        // 不足 2 条不成组：只有 1 条候选 → 无组
        assert!(plan_groups(&cands[..1]).is_empty());
    }

    #[test]
    fn plan_groups_prefers_thread_over_place_over_links() {
        // 组 A：同 place 成对；候选 b1 与组 A 同 place 但与组 B 同 thread → 必须进组 B
        let mut a1 = mem(1, "A1", 0.1, 1);
        a1.place = Some("图书馆".to_string());
        let mut a2 = mem(3, "A2", 0.1, 1);
        a2.place = Some("图书馆".to_string());
        let mut b1 = mem(2, "B1", 0.1, 1);
        b1.place = Some("天台".to_string());
        b1.thread = Some("thread.周五还书".to_string());
        let groups = plan_groups(&[a1, b1.clone(), b1, a2]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2, "组 A = 同 place 的 a1+a2");
        assert_eq!(groups[1].len(), 2, "thread 亲和优先于 place：b1 与重复的 b1 同组");

        // 无 thread/place 亲和，但 links 重叠 ≥2 → 同组
        let mut c1 = mem(1, "C1", 0.1, 1);
        c1.place = None;
        c1.links = vec!["topic:便签".into(), "person:小雨".into()];
        let mut c2 = mem(2, "C2", 0.1, 1);
        c2.place = None;
        c2.links = vec!["topic:便签".into(), "person:小雨".into(), "topic:别的".into()];
        let mut c3 = mem(3, "C3", 0.1, 1);
        c3.place = None;
        c3.links = vec!["topic:便签".into()];
        let groups = plan_groups(&[c1, c2, c3]);
        assert_eq!(groups.len(), 1, "c3 只重叠 1 条 link 不成组，独组不足 2 条丢弃");
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn parse_draft_rejects_empty_and_overlong() {
        assert_eq!(parse_draft("  "), None);
        assert_eq!(parse_draft("```\n\n```"), None);
        assert_eq!(parse_draft("```\n合并稿正文。\n```").as_deref(), Some("合并稿正文。"));
        let long = "长".repeat(DRAFT_MAX_CHARS + 1);
        assert_eq!(parse_draft(&long), None, "超长放弃");
    }

    /// DoD 构造用例：20 条低显著相似记忆整理后——合并稿进摘要、原记忆召回不再返回、
    /// 事件流全量在、重放一致。
    #[test]
    fn twenty_low_salience_memories_consolidate_end_to_end() {
        let mut objs: Vec<MemObject> = (1..=20).map(|i| mem(i, &format!("旧事{i}：在图书馆整理便签"), 0.1, 1)).collect();
        // 混入高显著记忆：不进候选，整理后必须原样可召回
        objs.push(mem(21, "高显著：周五要还书", 0.9, 10));

        let cands = candidates(&objs, 20, 0.25);
        assert_eq!(cands.len(), 20);
        let plans = plan_groups(&cands);
        assert_eq!(plans.len(), 3, "20 条 → 8+8+4 三组");

        let drafts: Vec<Option<String>> = plans
            .iter()
            .map(|g| Some(format!("合并稿（{} 条旧事的梗概）", g.len())))
            .collect();
        let next_index = next_index_of(&objs);
        assert_eq!(next_index, 22);

        let scene_of_place = |place: Option<&str>| place.map(|p| p.to_string());
        let (bodies, report) = build_bodies(&plans, &drafts, next_index, 20, &scene_of_place, 7);
        assert_eq!((report.merged, report.archived), (3, 20), "三组全成，20 条全部归档");

        // 先把 21 条原始记忆作为既有事件铺进投影（真实流程里它们已在事件流中），
        // 再折叠整理事件——归档标记走「同 id 后写覆盖」分支。
        let mut proj = event::Projection::default();
        let base_recs: Vec<event::LogRecord> = objs
            .iter()
            .map(|m| event::LogRecord {
                seq: m.turn,
                body: LogBody::Memory(event::MemoryEvent {
                    turn: m.turn,
                    origin: "pipeline".into(),
                    object: serde_json::to_value(m).expect("MemObject 可序列化"),
                    ts: 1,
                }),
            })
            .collect();
        for rec in &base_recs {
            event::fold(&mut proj, rec);
        }
        let recs: Vec<event::LogRecord> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| event::LogRecord { seq: 100 + i as u64, body: b.clone() })
            .collect();
        for rec in &recs {
            event::fold(&mut proj, rec);
        }
        let mems: Vec<MemObject> = proj
            .episodes
            .iter()
            .filter_map(|v| serde_json::from_value::<MemObject>(v.clone()).ok())
            .collect();
        assert_eq!(mems.len(), 21 + 3, "20 原记忆 + 1 高显著 + 3 合并稿（归档是覆盖不是追加）");
        assert_eq!(mems.iter().filter(|m| m.archived).count(), 20);
        assert_eq!(mems.iter().filter(|m| m.source == SOURCE).count(), 3);
        let merged = mems.iter().find(|m| m.source == SOURCE).unwrap();
        assert_eq!(merged.salience, 0.1, "合并稿 salience = 组内最大（全 0.1）");
        assert_eq!(merged.id, "mem_0022", "id 从现有最大序号接续");

        // 原记忆退出召回层：按地点与 mentions 查询不再返回 mem_0001，合并稿可召回
        let hits = palace::recall(
            &mems,
            &palace::RecallQuery {
                viewer: String::new(),
                now_day: 20,
                place: Some("图书馆".into()),
                present: vec![],
                mentions: vec!["在图书馆整理便签".into()],
                hints: vec![],
                active_threads: vec![],
                top_k: 10,
                budget_tokens: 4000,
            },
        );
        assert!(!hits.iter().any(|h| h.mem.id == "mem_0001"), "归档记忆不再召回");
        assert!(hits.iter().any(|h| h.mem.source == SOURCE), "合并稿进入召回层");
        assert!(hits.iter().any(|h| h.mem.id == "mem_0021"), "高显著记忆不受整理影响");

        // 合并稿进摘要（场景卷「章节归档」段）
        let summary_text = proj.scene_summaries.values().cloned().collect::<Vec<_>>().join("\n");
        assert_eq!(summary_text.matches("【章节归档】").count(), 3);
        // 摘要水位不推高（成员的 turn 都远小于真实水位时，水位取 max 不回退）
        assert_eq!(proj.summary_upto_of.get("图书馆"), Some(&0), "归档段 to_turn=0，不虚推总结水位（保持 0）");
        assert_eq!(proj.summary_upto, 0);

        // 事件流全量在：完整重放（原始记忆 + 整理事件）与直接折叠结果一致（重放不重调模型）
        let mut replayed = event::Projection::default();
        for rec in base_recs.iter().chain(recs.iter()) {
            event::fold(&mut replayed, rec);
        }
        assert_eq!(replayed.episodes, proj.episodes);
        assert_eq!(recs.len(), 3 * 2 + 20, "每组 合并稿1+摘要1，归档成员各 1：6+20=26");
    }

    #[test]
    fn failed_groups_keep_members_unarchived() {
        let cands: Vec<MemObject> = (1..=4).map(|i| mem(i, "旧事", 0.1, 1)).collect();
        let plans = plan_groups(&cands);
        assert_eq!(plans.len(), 1);
        let scene_of_place = |_: Option<&str>| None::<String>;
        let (bodies, report) = build_bodies(&plans, &[None], 5, 20, &scene_of_place, 7);
        assert!(bodies.is_empty(), "没有合并稿就没有任何事件");
        assert_eq!(report.merged, 0);
        assert_eq!(report.archived, 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].contains("第1组"));
    }
}
