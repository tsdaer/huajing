//! Tauri 命令 · 素材规格化管线（M3.9 · 设计 §6.7）：wiki 页 → 卡/正史/世界线三路落盘。
//! （引擎在 crate::ingest；这里的 ingestion 避免与引擎模块同名。）

use super::*;
// ---------- 素材规格化管线（M3.9 · 设计 §6.7）----------
//
// 八步的命令切分：prepare=①②（确定性清洗分段）、classify=③（LLM P1）、
// extract=④⑤（机械映射 + LLM P3–P8）、commit=⑥⑦⑧（查重冲突 + 落盘 + 切入点切面）。
// 草稿包（ingest::IngestPack）在前端整包往返——审阅的 include/剔除都在前端改，
// commit 只认提交上来的那一份（创建期动作不进会话事件流，落盘的文件就是正史）。

/// P0–P11 提示词套件全文（手动·分步/一键模式的文本源；双用途见套件文档）
#[tauri::command]
pub fn ingest_prompts() -> String {
    ingest::SUITE.to_string()
}

/// ①② 导入与清洗分段（确定性）：去 wiki 标记、按标题切节、扫剧透候选。
/// 剧透标记必须在清洗**前**扫（模板壳会被清洗剥掉），所以这里一并返回。
#[tauri::command]
pub fn ingest_prepare(world: String, text: String) -> Result<serde_json::Value, String> {
    let name = if world.trim().is_empty() { "default".into() } else { world };
    let cleaned = ingest::clean_source(&text);
    if cleaned.trim().is_empty() {
        return Err("素材清洗后没有内容——检查粘贴的是不是空白页".into());
    }
    let sections = ingest::segment_sections(&cleaned);
    let spoilers = ingest::extract_spoilers(&text);
    Ok(serde_json::json!({
        "world": name,
        "sections": sections,
        "spoilers": spoilers,
    }))
}

/// ③ 分节分类（LLM P1，util 档）：拿不准的节由前端按 unknown 处理（宁漏勿错）。
#[tauri::command]
pub async fn ingest_classify(sections: Vec<serde_json::Value>) -> Result<serde_json::Value, String> {
    let root = root();
    let sections = parse_sections(&sections)?;
    let prompt_text = ingest::build_classify_prompt(&sections);
    let raw = run_ingest_stage(&root, prompt_text, 2048).await?;
    let tags = ingest::parse_classifications(&raw);
    Ok(serde_json::json!(tags
        .into_iter()
        .map(|(id, tag)| serde_json::json!({ "id": id, "tag": tag }))
        .collect::<Vec<_>>()))
}

/// ④⑤ 机械映射 + 语义归纳（LLM P3–P8，util 档）→ 完整草稿包。
/// mechanics 节在选材层就被排除（④ 的一律过滤）；某步选材为空则跳过该步调用。
#[tauri::command]
pub async fn ingest_extract(
    world: String,
    name_hint: Option<String>,
    sections: Vec<serde_json::Value>,
    tags: Vec<serde_json::Value>,
    spoilers: Vec<String>,
) -> Result<serde_json::Value, String> {
    let root = root();
    let name = if world.trim().is_empty() { "default".into() } else { world };
    let sections = parse_sections(&sections)?;
    let mut tag_map: BTreeMap<String, String> = BTreeMap::new();
    for t in &tags {
        let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let tag = t.get("tag").and_then(|v| v.as_str()).unwrap_or("unknown").trim().to_string();
        if !id.is_empty() {
            tag_map.insert(id, tag);
        }
    }
    // 没被分类到的节按 unknown 处理（分类失败/部分失败的兜底，宁漏勿错）
    for s in &sections {
        tag_map.entry(s.id.clone()).or_insert_with(|| "unknown".into());
    }

    let pick = |wanted: &[&str]| ingest::sections_of(&sections, &tag_map, wanted);
    let any_of = |wanted: &[&str]| {
        wanted.iter().any(|w| {
            tag_map.values().any(|t| t == w)
        })
    };
    // ---- ④ 机械映射（确定性）----
    let infobox_sections = pick(&["infobox"]);
    let infobox = ingest::parse_infobox(&infobox_sections);

    // ---- ⑤ 语义归纳（LLM，逐步调用）----
    // P3 秘密与生命周期
    let mut secrets = Vec::new();
    let mut lifecycle = None;
    let mut versions = Vec::new();
    let mut pending: Vec<ingest::PendingItem> = Vec::new();
    let events;
    let four;
    let psyche;
    let relations;
    let examples;

    // 显式名字提示贯穿全程（wiki 页标题常比信息框更可靠）；没有就用信息框名
    let explicit_hint: Option<String> = name_hint
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let char_name = explicit_hint
        .clone()
        .or_else(|| infobox.name.clone())
        .unwrap_or_else(|| "角色".into());

    // P3 秘密与生命周期
    if any_of(&["history", "relations", "dialogue_scene"]) {
        let p3_sections = pick(&["history", "relations", "dialogue_scene"]);
        let raw = run_ingest_stage(&root, ingest::build_secrets_prompt(&p3_sections, &char_name), 2048).await?;
        // 阶段天锚来自 P6——但 P6 在 P3 之后跑，先跑 P6 再回填 P3 的时点解析
        let p6_sections = if any_of(&["history"]) {
            let raw = run_ingest_stage(&root, ingest::build_events_prompt(&pick(&["history"]), &char_name), 3072).await?;
            Some(ingest::parse_events(&raw))
        } else {
            None
        };
        let (p3_secrets, p3_lifecycle, p3_versions, p3_pending) =
            ingest::parse_secrets(&raw, &stage_day_map_from(p6_sections.as_ref().unwrap_or(&ingest::EventsOut::default())));
        secrets = p3_secrets;
        lifecycle = p3_lifecycle;
        versions = p3_versions;
        for p in p3_pending {
            pending.push(ingest::PendingItem { title: "秘密与生命周期".into(), detail: p, source: None });
        }
        events = p6_sections.unwrap_or_default();
    } else {
        events = ingest::EventsOut::default();
        pending.push(ingest::PendingItem {
            title: "经历".into(),
            detail: "素材里没有分类为「经历」的小节——事件年表与世界线候选为空".into(),
            source: None,
        });
    }

    // P4 描写四法
    if any_of(&["dialogue_scene", "quote_table", "intro"]) {
        let raw = run_ingest_stage(
            &root,
            ingest::build_four_methods_prompt(&pick(&["dialogue_scene", "quote_table", "intro"]), &char_name),
            3072,
        )
        .await?;
        four = ingest::parse_four_methods(&raw);
    } else {
        four = ingest::FourMethods::default();
    }

    // P5 倾向性
    if any_of(&["intro", "history", "relations"]) {
        let raw = run_ingest_stage(
            &root,
            ingest::build_psyche_prompt(&pick(&["intro", "history", "relations"]), &char_name),
            1536,
        )
        .await?;
        psyche = ingest::parse_psyche(&raw);
    } else {
        psyche = ingest::FourMethods::default();
    }

    // P7 关系网
    if any_of(&["relations", "history"]) {
        let raw = run_ingest_stage(
            &root,
            ingest::build_relations_prompt(&pick(&["relations", "history"]), &char_name),
            2048,
        )
        .await?;
        relations = ingest::parse_relations(&raw);
    } else {
        relations = ingest::RelationsOut::default();
    }

    // P8 示例对话
    if any_of(&["dialogue_scene", "quote_table"]) {
        let raw = run_ingest_stage(
            &root,
            ingest::build_examples_prompt(&pick(&["dialogue_scene", "quote_table"]), &char_name),
            3072,
        )
        .await?;
        examples = ingest::parse_examples(&raw);
    } else {
        examples = ingest::ExamplesOut::default();
    }

    let mut pack = ingest::assemble_pack(&ingest::AssembleInputs {
        world: &name,
        infobox: &infobox,
        spoilers: &spoilers,
        secrets,
        lifecycle,
        versions,
        four: &four,
        psyche: &psyche,
        events: &events,
        relations: &relations,
        examples: &examples,
        stage_days: &stage_day_map_from(&events),
    });
    // 名字提示覆盖装配层取的名字（关系与占位实体指回旧 id 的改名）
    if let Some(hint) = explicit_hint {
        let old = pack.entity.name.clone();
        pack.entity.name = hint;
        pack.char_id = ingest::entity_id("char", &pack.entity.name);
        pack.entity.id = pack.char_id.clone();
        for r in &mut pack.entity.relations {
            if r.to == ingest::entity_id("char", &old) {
                r.to = pack.char_id.clone();
            }
        }
    }
    pack.pending.extend(pending);
    pack.qc.extend(ingest::qc_pack(&pack));
    Ok(serde_json::to_value(&pack).map_err(|e| e.to_string())?)
}

/// P6 产物里的阶段天锚（extract 内部的临时映射；装配层有自己的 stage_day_map）。
fn stage_day_map_from(events: &ingest::EventsOut) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for s in &events.stages {
        out.insert(s.id.clone(), s.day);
        if !s.name.is_empty() {
            out.entry(s.name.clone()).or_insert(s.day);
        }
    }
    out
}
/// 前端传来的小节数组 → Section（宽容：缺 title/text 的项跳过）。
fn parse_sections(items: &[serde_json::Value]) -> Result<Vec<ingest::Section>, String> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("s{}", i + 1));
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if text.trim().is_empty() {
            continue;
        }
        out.push(ingest::Section { id, title, text });
    }
    if out.is_empty() {
        return Err("没有可用的小节".into());
    }
    Ok(out)
}

/// util 档跑一个阶段（与 codex_complete 同一条通道：pick_util_provider + 代理设置）。
async fn run_ingest_stage(
    root: &std::path::Path,
    prompt_text: String,
    max_tokens: u32,
) -> Result<String, String> {
    let provider = pick_util_provider(root)?;
    let proxy = proxy_of(root);
    llm::chat_complete(
        &provider,
        &[llm::ChatMessage {
            role: "user".into(),
            content: prompt_text,
            tool_calls: None,
        }],
        max_tokens,
        0.3,
        proxy.as_deref(),
    )
    .await
}

/// ⑦⑧ 审阅后的落盘（确定性）：查重冲突 → 切入点切面 → 写卡 + 正史增量 + 世界线。
#[tauri::command(async)]
pub fn ingest_commit(
    world: String,
    pack: serde_json::Value,
    day: i64,
    overwrite_worldline: bool,
    set_world_day: bool,
) -> Result<serde_json::Value, String> {
    let root = root();
    let name = if world.trim().is_empty() { "default".into() } else { world };
    let pack: ingest::IngestPack =
        serde_json::from_value(pack).map_err(|e| format!("草稿包格式不对：{e}"))?;
    ingest_commit_core(&root, &name, pack, day, overwrite_worldline, set_world_day)
}

/// [`ingest_commit`] 的可测内核。
pub(crate) fn ingest_commit_core(
    root: &std::path::Path,
    world: &str,
    pack: ingest::IngestPack,
    day: i64,
    overwrite_worldline: bool,
    set_world_day: bool,
) -> Result<serde_json::Value, String> {
    let day = if day > 0 { day } else { 1 };
    let cx = load_codex(root, None, world);
    let mut warnings: Vec<String> = Vec::new();
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    // ---- ⑥ 查重与冲突（确定性先行；anchors 最高保护级）----
    let check_new = |cx: &codex::Codex, id: &str, name: &str| -> Option<String> {
        if cx.get(id).is_some() {
            return Some(format!("实体 {id} 已存在——同名提案被跳过（如需更新请走实体编辑/收件箱）"));
        }
        // id 不同但同名的疑似重复：警告不阻断（可能是不同世界的同名者）
        if cx
            .entities()
            .iter()
            .any(|e| e.name.trim().to_lowercase() == name.trim().to_lowercase())
        {
            return Some(format!("__WARN__已有同名实体「{name}」——请确认不是重复导入"));
        }
        None
    };
    let ensure_ok = |cx: &codex::Codex, id: &str, name: &str, skipped: &mut Vec<String>, warnings: &mut Vec<String>| -> bool {
        match check_new(cx, id, name) {
            Some(msg) if msg.starts_with("__WARN__") => {
                warnings.push(msg.trim_start_matches("__WARN__").to_string());
                true
            }
            Some(msg) => {
                skipped.push(msg);
                false
            }
            None => true,
        }
    };

    let mut to_write: Vec<(String, serde_json::Value)> = Vec::new();
    let mut planned_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // ---- ⑧ 切入点切面（secrets known_by / lifecycle / 史变告警）----
    let applied = ingest::apply_canon_point(&pack, day);
    for w in &applied.warnings {
        warnings.push(w.problem.clone());
    }
    if let Some(point) = pack.canon_points.iter().find(|p| p.day == day) {
        if let Some(premise) = &point.premise {
            warnings.push(format!("切入点「{}」：{}", point.name, premise));
        }
    }

    // 角色实体（含 secrets / lifecycle / versions）
    if ensure_ok(&cx, &pack.entity.id, &pack.entity.name, &mut skipped, &mut warnings) {
        let mut entity = applied.char_entity;
        if let Some(obj) = entity.as_object_mut() {
            if let Some(lc) = &applied.lifecycle {
                obj.insert("lifecycle".into(), lc.clone());
            }
            let versions: Vec<serde_json::Value> = pack
                .versions
                .iter()
                .filter(|v| v.include && v.day > 0)
                .map(|v| {
                    serde_json::json!({
                        "from_day": v.day, "facet": v.facet, "value": v.value,
                        "note": v.note.clone().unwrap_or_default(),
                    })
                })
                .collect();
            if !versions.is_empty() {
                obj.insert("versions".into(), serde_json::json!(versions));
            }
            // 引源留档（审阅补看；不参与注入）
            if !pack.entity.sources.is_empty() {
                obj.insert(
                    "sources".into(),
                    serde_json::json!(pack.entity.sources
                        .iter()
                        .map(|(k, s)| serde_json::json!({
                            "facet": k, "section": s.section, "quote": s.quote,
                        }))
                        .collect::<Vec<_>>()),
                );
            }
        }
        planned_ids.insert(pack.entity.id.clone());
        to_write.push((pack.entity.id.clone(), entity));
    }
    // 占位与其他实体
    for other in pack.others.iter().filter(|o| o.include) {
        if ensure_ok(&cx, &other.id, &other.name, &mut skipped, &mut warnings) {
            planned_ids.insert(other.id.clone());
            to_write.push((other.id.clone(), ingest::entity_value(other, BTreeMap::new())));
        }
    }
    // 事件实体
    for ev in pack.events.iter().filter(|e| e.include) {
        if ensure_ok(&cx, &ev.id, &ev.name, &mut skipped, &mut warnings) {
            planned_ids.insert(ev.id.clone());
            to_write.push((ev.id.clone(), ingest::entity_value(ev, BTreeMap::new())));
        }
    }
    // 悬空关系：to 指向既不在 codex 也不在本次写入名单的实体 → 警告（人工裁决线索）
    for r in &pack.entity.relations {
        if cx.get(&r.to).is_none() && !planned_ids.contains(&r.to) {
            warnings.push(format!("悬空关系：{} → {} 不在设定集也不在本次写入名单", pack.char_id, r.to));
        }
    }
    if to_write.is_empty() {
        // 全部提案与既有设定冲突：不报错——返回空写入的结构化报告（重复导入的正常形态），
        // 卡照走复用逻辑（同内容卡 reused，不堆积）
        warnings.push("没有可写入的实体——全部提案与既有设定冲突".into());
    }

    // ---- 落盘：grown.json（正史增量，加载时应用进注入）----
    let mut grown = store::load_grown(root, world);
    for (id, entity) in &to_write {
        grown.entities.insert(id.clone(), entity.clone());
    }
    store::save_grown(root, world, &grown).map_err(|e| e.to_string())?;
    for (id, _) in &to_write {
        written.push(id.clone());
    }

    // ---- 落盘：card.lua（与 ST 导入同一条路：清洗/查重/可解析性验证都在里面）----
    let chosen = pack.canon_points.iter().find(|p| p.day == day);
    let mut scenario = pack.card.scenario.clone();
    if let Some(premise) = chosen.and_then(|p| p.premise.clone()) {
        // 「与已死者对话」的第二种处理：记忆体前提写进剧本 premise
        if scenario.trim().is_empty() {
            scenario = format!("【开场前提】{premise}");
        } else {
            scenario = format!("{scenario}\n\n【开场前提】{premise}");
        }
    }
    let notes = if pack.pending.is_empty() {
        "由素材规格化管线生成。".to_string()
    } else {
        format!(
            "由素材规格化管线生成。待定 {} 项：{}",
            pack.pending.len(),
            pack.pending
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>()
                .join("、")
        )
    };
    let card_draft = crate::stimport::CardDraft {
        name: pack.entity.name.clone(),
        creator: None,
        tags: pack.card.tags.clone(),
        world: Some(world.to_string()),
        scenario,
        personality: pack.card.personality.clone(),
        first_mes: pack.card.first_mes.clone(),
        example_dialogue: pack.card.example_dialogue.clone(),
        notes,
        source_spec: "素材规格化（wiki/剧情记录）".into(),
        warnings: Vec::new(),
        content_hash: String::new(),
    };
    let card_report = crate::stimport::save_card_draft(root, &card_draft, true)?;

    // ---- 落盘：worldline.lua（可选层；已有主线默认不覆盖）----
    let mut worldline_written = false;
    if let Some(wl) = &pack.worldline {
        if wl.stages.iter().any(|s| s.include) {
            let wl_path = store::world_path(root, world)
                .parent()
                .map(|p| p.join("worldline.lua"))
                .unwrap_or_else(|| std::path::PathBuf::from("worldline.lua"));
            if wl_path.exists() && !overwrite_worldline {
                warnings.push(format!(
                    "{} 已有世界主线声明，未覆盖（勾选覆盖后重试才会写入新主线）",
                    wl_path.display()
                ));
            } else {
                let stages: Vec<ingest::StageDraft> = wl
                    .stages
                    .iter()
                    .filter(|s| s.include)
                    .cloned()
                    .collect();
                let source = ingest::render_worldline_lua(&ingest::WorldlineDraft {
                    id: wl.id.clone(),
                    premise: wl.premise.clone(),
                    stages,
                });
                // 生成物必须能被归一化适配器读回（与 card.lua 落盘同一条纪律）
                if let Err(e) = card::worldline_shape(&source) {
                    warnings.push(format!("世界线声明解析失败，未写入：{e}"));
                } else {
                    std::fs::write(&wl_path, source)
                        .map_err(|e| format!("写入 {} 失败：{e}", wl_path.display()))?;
                    worldline_written = true;
                }
            }
        }
    }

    // ---- 世界时钟拨到切入点（可选；多线并行时拨钟影响其他会话的开局基准）----
    if set_world_day {
        let mut w = store::load_world(root, world);
        if day > w.day {
            w.day = day;
            store::save_world(root, world, &w).map_err(|e| e.to_string())?;
        }
    }

    crate::diag::record(
        "ingest",
        format!(
            "素材落盘：{} → 卡「{}」+ 实体 {} 条 + 世界线（{}）",
            world,
            card_report.dir_name,
            written.len(),
            if worldline_written { "已写" } else { "未写" }
        ),
    );
    Ok(serde_json::json!({
        "world": world,
        "cardDir": card_report.dir_name,
        "cardPath": card_report.card_path,
        "entitiesWritten": written,
        "worldlineWritten": worldline_written,
        "canonDay": day,
        "skipped": skipped,
        "warnings": warnings,
    }))
}

/// ST 世界书导入（M3.9 补 M2.2 欠账 · 设计 §6.10）：JSON → note 实体。
/// `json_text` 与 `path` 二选一（粘贴导入 / 文件导入）。
#[tauri::command]
pub fn import_worldbook(
    world: String,
    json_text: Option<String>,
    path: Option<String>,
    book_name: Option<String>,
) -> Result<crate::stimport::WorldbookReport, String> {
    let root = root();
    let name = if world.trim().is_empty() { "default".into() } else { world };
    let text = match (json_text, path) {
        (Some(text), _) => text,
        (None, Some(path)) => {
            std::fs::read_to_string(&path).map_err(|e| format!("读取 {path} 失败：{e}"))?
        }
        (None, None) => return Err("没有可导入的内容——粘贴 JSON 或给一个文件路径".into()),
    };
    crate::stimport::import_worldbook_to(&root, &name, &text, book_name.as_deref())
}



// ---------- 包格式与导入导出（M4.1 · 设计 §13：三包 zip 往返）----------
//
// 内核在 crate::pack（纯函数可单测）；命令层只负责拿默认 DataHub 与诊断留痕。
// 重复导入的冲突处理沿 stimport 先例：预览阶段报冲突，导入默认并存「-2」，
// 显式 overwrite 才覆盖。

/// 包预览（导入弹窗：清单 + 文件清单 + 提醒 + 冲突标记；只读不落盘）
#[tauri::command]
pub fn preview_pack(path: String) -> Result<crate::pack::PackPreview, String> {
    let path = std::path::PathBuf::from(path.trim());
    let outcome = crate::pack::preview_pack(&root(), &path);
    crate::diag::record(
        if outcome.is_ok() { "import" } else { "error" },
        match &outcome {
            Ok(p) => format!(
                "包预览：{}（{}，{} 个文件）← {}",
                p.manifest.name, p.manifest.kind, p.files.len(), path.display()
            ),
            Err(e) => format!("包预览失败：{}：{e}", path.display()),
        },
    );
    outcome
}

/// 包导入：识别 kind → 角色进 characters/、世界进 codex/、剧本进 scripts/ →
/// 热加载生效（卡走 watch 通道、世界按指纹自动重建）
#[tauri::command]
pub fn import_pack(path: String, overwrite: Option<bool>) -> Result<crate::pack::PackImportReport, String> {
    let path = std::path::PathBuf::from(path.trim());
    let outcome = crate::pack::import_pack_to(&root(), &path, overwrite.unwrap_or(false));
    if outcome.is_err() {
        crate::diag::record(
            "error",
            format!("包导入失败：{}：{}", path.display(), outcome.as_ref().unwrap_err()),
        );
    }
    outcome
}

/// 角色包导出：卡目录整打包 → DataHub/exports/
#[tauri::command]
pub fn export_card_pack(dir_name: String) -> Result<crate::pack::ExportedPack, String> {
    let out = crate::pack::export_card_pack(&root(), dir_name.trim());
    diag_export(&out);
    out
}

/// 世界包导出：codex/<世界>/ 整打包 → DataHub/exports/
#[tauri::command]
pub fn export_world_pack(world: String) -> Result<crate::pack::ExportedPack, String> {
    let out = crate::pack::export_world_pack(&root(), world.trim());
    diag_export(&out);
    out
}

/// 剧本包导出：从既有会话抽取 premise/初始黑板/导演树（不含消息历史）→ exports/
#[tauri::command]
pub fn export_script_pack(
    session_id: String,
    name: Option<String>,
) -> Result<crate::pack::ExportedPack, String> {
    let out = crate::pack::export_script_pack(&root(), session_id.trim(), name.as_deref());
    diag_export(&out);
    out
}

fn diag_export(out: &Result<crate::pack::ExportedPack, String>) {
    crate::diag::record(
        if out.is_ok() { "export" } else { "error" },
        match out {
            Ok(p) => format!("包导出：{}（{}）→ {}", p.name, p.kind, p.path),
            Err(e) => format!("包导出失败：{e}"),
        },
    );
}

/// ST 世界书反向导出（§6.10）：canon 实体拍平 → exports/<世界>-worldbook.st.json
#[tauri::command]
pub fn export_worldbook_st(world: String) -> Result<crate::pack::ExportedPack, String> {
    let root = root();
    let name = if world.trim().is_empty() { "default".into() } else { world.trim().to_string() };
    let entities_dir = root.join("codex").join(&name).join("entities");
    if !entities_dir.is_dir() {
        return Err(format!("世界「{name}」不存在"));
    }
    // 导出的世界口径与注入一致：实体文件解析 + grown.json 正史增量应用
    let entities = crate::codex::apply_grown(
        parse_entities(&entities_dir),
        &crate::store::load_grown(&root, &name),
    );
    let day = crate::store::load_world(&root, &name).day;
    let book = crate::stimport::worldbook_from_entities(&name, &entities, day);
    let count = book
        .get("entries")
        .and_then(|e| e.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    let dir = crate::pack::exports_dir(&root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}-worldbook.st.json"));
    let body = serde_json::to_string_pretty(&book).map_err(|e| e.to_string())?;
    std::fs::write(&path, body + "\n").map_err(|e| e.to_string())?;
    crate::diag::record(
        "export",
        format!("ST 世界书导出：{name}（{count} 条）→ {}", path.display()),
    );
    Ok(crate::pack::ExportedPack {
        kind: "worldbook".into(),
        name,
        path: path.display().to_string(),
    })
}

/// 已安装剧本清单（建会话向导的选择器数据源）
#[tauri::command]
pub fn list_scripts() -> Result<Vec<crate::pack::ScriptSummary>, String> {
    Ok(crate::pack::list_scripts(&root()))
}

/// 剧本模板全文（选中后预填向导：premise/天/时间/地点）
#[tauri::command]
pub fn get_script(name: String) -> Result<crate::pack::ScriptTemplate, String> {
    crate::pack::load_script_template(&root(), name.trim())
}

// ---------- 导入暂存（M4.5 · 决断 6「导入入口统一」） ----------

/// 暂存目录的最长保留期：暂存文件只服务于「选择文件 → 弹窗预览 → 导入」这一小段
/// 流程，超过 24 小时的遗留（导入中途关应用等）在下次暂存时顺手清掉。
const STAGE_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// 把前端交来的文件字节落成 DataHub/imports/ 下的暂存文件，返回路径。
/// 移动端 / HTML 文件选择器拿不到本地路径，字节交后端后既有 path 版
/// preview/import 命令原样可用；桌面拖放双通道保留不受影响。
#[tauri::command]
pub fn stage_import(filename: String, data: Vec<u8>) -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    if data.is_empty() {
        return Err("文件内容为空".into());
    }
    if data.len() > 512 * 1024 * 1024 {
        return Err("文件超过 512MB 上限".into());
    }
    let root = root();
    let dir = root.join("imports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 顺手清掉过期暂存（明文数据层的自洁，失败不阻塞本次暂存）
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if let Ok(meta) = entry.metadata() {
            let age = now.saturating_sub(meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(now));
            if age > STAGE_MAX_AGE_SECS {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    // 文件名只取末段 + sanitize（防路径成分），加时间戳前缀防同名覆盖
    let base = crate::stimport::sanitize_dir_name(&filename);
    let stem = format!("{}-{}", now, base);
    let path = dir.join(&stem);
    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
    crate::diag::record(
        "import",
        format!("文件选择器暂存：{}（{} 字节）→ {}", base, data.len(), path.display()),
    );
    Ok(path.display().to_string())
}
