//! SillyTavern 角色卡导入（M1.8 · 设计 §13「导入生态」）。
//!
//! 支持两种输入：
//! - PNG：tEXt chunk `chara`（值为 base64 编码的角色 JSON），兼容 `ccv3` 键；
//! - 纯 JSON：V2（`{"spec":"chara_card_v2","data":{…}}`）与 V3（`chara_card_v3`），
//!   以及没有 spec 包裹的裸字段对象。
//!
//! 映射目标是 `card.lua`（charcard/1.0，设计 §3）：ST 的扁平字段落到静态提示词层，
//! `mes_example` 里成对的 `<START>` 段拆成 `example_dialogue`（情绪化 few-shot），
//! 拆不动的原文折进 `notes` 供卡作者参考。
//!
//! 世界书（character_book / 世界书 JSON）与 codex 实体的拆分在 M2（设计 §2.1），
//! 本模块把顶层字段计数后作为提醒回传，不静默丢数据。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::card::{self, ExampleLine, ExampleTurn};

/// 导入结果报告（前端展示：落盘位置、识别到的字段、提醒）
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    /// 生成的目录名（`characters/<dir_name>`）
    pub dir_name: String,
    pub card_path: String,
    pub draft: CardDraft,
    /// 提醒（未映射字段、被截断的示例对话等）
    pub warnings: Vec<String>,
}

/// 从 ST 卡解析出的草稿（落盘前可预览/改名）
#[derive(Debug, Clone, Serialize)]
pub struct CardDraft {
    pub name: String,
    pub creator: Option<String>,
    pub tags: Vec<String>,
    pub world: Option<String>,
    pub scenario: String,
    pub personality: String,
    pub first_mes: String,
    pub example_dialogue: Vec<ExampleTurn>,
    /// 系统提示词 + 作者备注 + 未能拆分的示例对话原文（进 scenario 的补充块）
    pub notes: String,
    /// 原卡 spec（v2 / v3 / 未知），仅作展示
    pub source_spec: String,
    /// 解析期的提醒（未映射字段、拆不动的示例对话等）
    pub warnings: Vec<String>,
}

/// 从文件导入：按扩展名与内容分派 PNG / JSON
#[tauri::command]
pub fn import_st_card(path: String, overwrite: Option<bool>) -> Result<ImportReport, String> {
    let path = PathBuf::from(path.trim());
    let root = crate::store::data_root();
    let outcome = import_file_to(&root, &path, overwrite.unwrap_or(false));
    // 留痕：安装版没有控制台，「拖了没反应」只能靠诊断面板说清是哪一步断的
    crate::diag::record(
        if outcome.is_ok() { "import" } else { "error" },
        match &outcome {
            Ok(r) => format!(
                "导入成功：{} ← {}（写入 {}",
                r.dir_name,
                path.display(),
                root.display()
            ),
            Err(e) => format!("导入失败：{} ← {}：{e}", path.display(), root.display()),
        },
    );
    outcome
}

/// 只解析不落盘（导入向导的预览步骤）
#[tauri::command]
pub fn preview_st_card(path: String) -> Result<CardDraft, String> {
    let path = PathBuf::from(path.trim());
    let outcome = std::fs::read(&path)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))
        .and_then(|bytes| parse_st_card(&bytes, path.file_stem().and_then(|s| s.to_str())));
    crate::diag::record(
        if outcome.is_ok() { "import" } else { "error" },
        match &outcome {
            Ok(d) => format!(
                "解析成功：{}（{}，示例 {} 组）",
                path.display(),
                d.source_spec,
                d.example_dialogue.len()
            ),
            Err(e) => format!("解析失败：{}：{e}", path.display()),
        },
    );
    outcome
}

/// 解析 PNG / JSON 为草稿；`fallback_name` 用于卡里没有名字的情况（多为文件名）
pub fn parse_st_card(bytes: &[u8], fallback_name: Option<&str>) -> Result<CardDraft, String> {
    let json = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        extract_png_chara(bytes)?
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    parse_st_json(&json, fallback_name)
}

// ---------- PNG：tEXt chunk ----------

/// PNG 签名（8 字节）
const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// 取出 PNG 里承载角色卡的文本块，优先 `chara`（V2 惯例），其次 `ccv3`（V3）
fn extract_png_chara(bytes: &[u8]) -> Result<String, String> {
    if !bytes.starts_with(&PNG_SIG) {
        return Err("不是 PNG 文件（签名不匹配）".into());
    }
    let mut found: Vec<(String, String)> = Vec::new();
    let mut pos = PNG_SIG.len();
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = match data_start.checked_add(len) {
            Some(end) if end + 4 <= bytes.len() => end,
            // 截断的文件：不再往下走，看看已拿到的块够不够
            _ => break,
        };
        if kind == b"tEXt" {
            let data = &bytes[data_start..data_end];
            if let Some(nul) = data.iter().position(|&b| b == 0) {
                let key = String::from_utf8_lossy(&data[..nul]).into_owned();
                let value = String::from_utf8_lossy(&data[nul + 1..]).into_owned();
                found.push((key, value));
            }
        }
        if kind == b"IEND" {
            break;
        }
        pos = data_end + 4; // 跳过 CRC
    }
    for wanted in ["chara", "ccv3"] {
        if let Some((_, value)) = found.iter().find(|(k, _)| k == wanted) {
            return decode_base64(value.trim())
                .and_then(|raw| String::from_utf8(raw).map_err(|e| format!("角色数据不是 UTF-8：{e}")));
        }
    }
    let keys: Vec<String> = found.into_iter().map(|(k, _)| k).collect();
    if keys.is_empty() {
        Err("PNG 里没有 tEXt 文本块——这不像是 SillyTavern 角色卡".into())
    } else {
        Err(format!(
            "PNG 里没有 chara/ccv3 文本块（找到：{}）",
            keys.join("、")
        ))
    }
}

/// 标准 base64 解码（忽略空白；缺省填充容忍）
fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = val(c) else {
            if c.is_ascii_whitespace() {
                continue;
            }
            return Err(format!("base64 数据里有非法字符：{}", c as char));
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if out.is_empty() {
        return Err("base64 解码结果为空".into());
    }
    Ok(out)
}

// ---------- JSON：V2 / V3 字段映射 ----------

/// 读取 `data` 子对象（V2/V3 规范位置），没有就退回顶层
fn card_object(root: &serde_json::Value) -> &serde_json::Value {
    root.get("data")
        .filter(|d| d.is_object())
        .unwrap_or(root)
}

fn str_field(obj: &serde_json::Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// `{{char}}` / `<BOT>` → 角色名；`{{user}}` / `<USER>` → 第二人称
fn normalize_placeholders(text: &str, name: &str) -> String {
    text.replace("{{char}}", name)
        .replace("{{user}}", "你")
        .replace("<BOT>", name)
        .replace("<USER>", "你")
}

/// 解析 ST 角色卡 JSON（V2/V3/裸字段）为草稿
pub fn parse_st_json(json: &str, fallback_name: Option<&str>) -> Result<CardDraft, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("角色卡 JSON 解析失败：{e}"))?;
    if !root.is_object() {
        return Err("角色卡 JSON 顶层不是对象".into());
    }
    let obj = card_object(&root);
    let source_spec = root
        .get("spec")
        .and_then(|v| v.as_str())
        .unwrap_or("未知")
        .to_string();

    let name = {
        let n = str_field(obj, &["name", "char_name"]);
        if n.is_empty() {
            fallback_name.unwrap_or("导入角色").trim().to_string()
        } else {
            n
        }
    };
    let raw_first = str_field(obj, &["first_mes", "first_message"]);
    let raw_scenario = str_field(obj, &["scenario", "world_scenario"]);
    let raw_personality = str_field(obj, &["personality"]);
    let raw_description = str_field(obj, &["description", "char_persona"]);
    let raw_system = str_field(obj, &["system_prompt", "system_persona"]);
    let raw_post = str_field(obj, &["post_history_instructions"]);
    let raw_examples = str_field(obj, &["mes_example", "example_dialogue"]);
    let raw_creator_notes = str_field(obj, &["creator_notes", "creatorcomment"]);

    let mut warnings = Vec::new();
    let (examples, script_examples) = split_examples(&raw_examples, &name);
    if !script_examples.is_empty() {
        warnings.push("示例对话里有拆不成问答对的片段，已折进 scenario 的参考块".into());
    }

    // scenario：ST 的场景 + 角色描述（人设写在描述里的卡占多数）
    let mut scenario = String::new();
    if !raw_scenario.is_empty() {
        scenario.push_str(&raw_scenario);
    }
    if !raw_description.is_empty() {
        if !scenario.is_empty() {
            scenario.push('\n');
        }
        scenario.push_str(&raw_description);
    }

    // personality：ST 的 personality 字段为主；为空时用描述兜底（卡不空转）
    let personality = if raw_personality.is_empty() {
        raw_description.clone()
    } else {
        raw_personality.clone()
    };

    // notes：系统提示词/作者备注/拆不动的示例原文，落进卡内参考块而非静默丢弃
    let mut notes = String::new();
    let push_block = |label: &str, body: &str, notes: &mut String| {
        if body.is_empty() {
            return;
        }
        if !notes.is_empty() {
            notes.push_str("\n\n");
        }
        notes.push_str(&format!("[{label}]\n{body}"));
    };
    push_block("原始系统提示词", &raw_system, &mut notes);
    push_block("对话后指令", &raw_post, &mut notes);
    push_block("示例对话原文（未拆分部分）", &script_examples, &mut notes);
    push_block("作者备注", &raw_creator_notes, &mut notes);

    let tags = obj
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::trim))
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let world = {
        let w = str_field(obj, &["character_book", "world"]);
        // character_book 是对象而非字符串时不算世界名
        if obj.get("character_book").map(|v| v.is_object()).unwrap_or(false) || w.is_empty() {
            None
        } else {
            Some(w)
        }
    };

    let mut draft = CardDraft {
        name: name.clone(),
        creator: {
            let c = str_field(obj, &["creator", "author"]);
            (!c.is_empty()).then_some(c)
        },
        tags,
        world,
        scenario: normalize_placeholders(&scenario, &name).trim().to_string(),
        personality: normalize_placeholders(&personality, &name).trim().to_string(),
        first_mes: normalize_placeholders(&raw_first, &name).trim().to_string(),
        example_dialogue: examples,
        notes,
        source_spec,
        warnings,
    };

    // 未映射字段点名（M2 的 codex 拆分与设定补全接手）
    let deferred: Vec<&str> = ["character_book", "extensions", "alternate_greetings"]
        .into_iter()
        .filter(|k| obj.get(*k).map(|v| !v.is_null()).unwrap_or(false))
        .collect();
    if !deferred.is_empty() {
        draft.warnings.push(format!(
            "卡内还有 {} 未导入（世界书/扩展字段由 M2 的设定集拆分接手）",
            deferred.join("、")
        ));
    }
    if draft.first_mes.is_empty() {
        draft
            .warnings
            .push("这张卡没有 first_mes 开场白，会话开场为空".into());
    }

    Ok(draft)
}

// ---------- 示例对话拆分 ----------

/// 把 ST 的 `mes_example` 拆成问答对；拆不动的原文原样返回
fn split_examples(raw: &str, name: &str) -> (Vec<ExampleTurn>, String) {
    if raw.trim().is_empty() {
        return (Vec::new(), String::new());
    }
    // 每行「角色名: 台词」或「你: 台词」；名字里的正则元字符要转义
    let escaped = regex::escape(name);
    // 注意：`{{` 直接写会同时踩到 format! 的转义与 regex 的非法转义，改用 \x7B 十六进制转义
    let pat = format!(
        "^\\s*(?:{escaped}|\\x7B\\x7Bchar\\x7D\\x7D|<BOT>|你|\\x7B\\x7Buser\\x7D\\x7D|<USER>)\\s*[:：]\\s*(.+)$"
    );
    let Ok(re) = regex::Regex::new(&pat) else {
        return (Vec::new(), raw.trim().to_string());
    };
    let mut turns = Vec::new();
    let mut leftover: Vec<String> = Vec::new();
    for seg in raw.split("<START>") {
        let mut messages: Vec<ExampleLine> = Vec::new();
        for line in seg.lines() {
            let line = line.trim().trim_matches('"').trim();
            if line.is_empty() {
                continue;
            }
            match re.captures(line) {
                Some(caps) => {
                    let speaker = line.split([':', '：']).next().unwrap_or("").trim().to_string();
                    let role = if speaker == name || speaker == "<BOT>" || speaker == "{{char}}" {
                        "char"
                    } else {
                        "user"
                    };
                    let content = normalize_placeholders(caps[1].trim(), name);
                    messages.push(ExampleLine {
                        role: role.into(),
                        content,
                    });
                }
                None => leftover.push(line.to_string()),
            }
        }
        // 至少一问一答才算一组 few-shot
        if messages.len() >= 2 {
            turns.push(ExampleTurn {
                tag: None,
                messages,
            });
        } else {
            leftover.extend(messages.into_iter().map(|m| m.content));
        }
    }
    (turns, leftover.join("\n"))
}

// ---------- 落盘：生成 card.lua ----------

/// 生成 `card.lua` 源码（charcard/1.0）
pub fn render_card_lua(draft: &CardDraft) -> String {
    let mut out = String::new();
    out.push_str("-- 由「导入 SillyTavern 角色卡」生成（化境 charcard/1.0）\n");
    out.push_str(&format!("-- 原卡规范：{}\n", draft.source_spec));
    out.push_str("-- 静态提示词层可直接编辑；加 hooks/state 即成行为卡（设计 §3）\n");
    out.push_str("return {\n");
    out.push_str("  spec = \"charcard/1.0\",\n");
    out.push_str(&format!("  name = {},\n", lua_str(&draft.name)));
    if let Some(creator) = &draft.creator {
        out.push_str(&format!("  creator = {},\n", lua_str(creator)));
    }
    if !draft.tags.is_empty() {
        let tags: Vec<String> = draft.tags.iter().map(|t| lua_str(t)).collect();
        out.push_str(&format!("  tags = {{ {} }},\n", tags.join(", ")));
    }
    if let Some(world) = &draft.world {
        out.push_str(&format!("  world = {},\n", lua_str(world)));
    }
    out.push('\n');
    out.push_str(&format!("  scenario = {},\n", lua_str(&draft.scenario)));
    out.push_str(&format!("  personality = {},\n", lua_str(&draft.personality)));
    out.push_str(&format!("  first_mes = {},\n", lua_str(&draft.first_mes)));
    if !draft.notes.is_empty() {
        out.push('\n');
        out.push_str("  -- 导入时保留的原始素材（系统提示词/备注/未拆分的示例对话）\n");
        out.push_str(&format!("  notes = {},\n", lua_str(&draft.notes)));
    }
    if !draft.example_dialogue.is_empty() {
        out.push('\n');
        out.push_str("  -- 从 mes_example 拆出的状态化示例对话（设计 §3）\n");
        out.push_str("  example_dialogue = {\n");
        for turn in &draft.example_dialogue {
            out.push_str("    { messages = {\n");
            for m in &turn.messages {
                out.push_str(&format!(
                    "      {{ role = {}, content = {} }},\n",
                    lua_str(&m.role),
                    lua_str(&m.content)
                ));
            }
            out.push_str("    } },\n");
        }
        out.push_str("  },\n");
    }
    out.push_str("}\n");
    out
}

/// Rust 字符串 → Lua 双引号字符串字面量（bytes 转义，对任意 UTF-8 安全）
fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for &b in s.as_bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7E => out.push(b as char),
            // 其余（含全部多字节 UTF-8）走十进制转义，Lua 的 \ddd 按字节还原
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out.push('"');
    out
}

/// 目录名清洗：去掉路径分隔与 Windows 保留字符，避免写穿 DataHub
fn sanitize_dir_name(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\t' => '_',
            _ => c,
        })
        .collect();
    // `..` 单独留着会构成上跳路径，换成下划线（目录名里的中文省略不受影响）
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", "_");
    }
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "导入角色".into()
    } else {
        cleaned
    }
}

/// 落盘：`characters/<名字>/card.lua`；名字冲突时追加 `-2`、`-3`…
pub fn save_card_draft(
    root: &Path,
    draft: &CardDraft,
    overwrite: bool,
) -> Result<ImportReport, String> {
    crate::store::ensure_layout(root).map_err(|e| format!("数据目录初始化失败：{e}"))?;
    let base = sanitize_dir_name(&draft.name);
    let mut dir_name = base.clone();
    if !overwrite {
        let mut n = 2;
        while root.join("characters").join(&dir_name).join("card.lua").exists() {
            dir_name = format!("{base}-{n}");
            n += 1;
        }
    }
    let dir = root.join("characters").join(&dir_name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
    let card_path = dir.join("card.lua");
    let source = render_card_lua(draft);
    std::fs::write(&card_path, source).map_err(|e| format!("写入 card.lua 失败：{e}"))?;

    // 生成物必须能被自家解析器读回：读不回就是 bug（导入不该产出坏卡）
    let loaded = card::load_card_source(&dir_name, &std::fs::read_to_string(&card_path).unwrap_or_default());
    if loaded.degraded {
        let reason = loaded.degrade_reason.clone().unwrap_or_default();
        let _ = std::fs::remove_file(&card_path);
        return Err(format!("生成的卡无法被解析（已回滚）：{reason}"));
    }

    Ok(ImportReport {
        dir_name,
        card_path: card_path.to_string_lossy().into_owned(),
        draft: draft.clone(),
        warnings: draft.warnings.clone(),
    })
}

/// 从文件导入到指定 root（[`import_st_card`] 的可测内核：命令层只负责拿默认 DataHub）
pub fn import_file_to(root: &Path, path: &Path, overwrite: bool) -> Result<ImportReport, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    let fallback = path.file_stem().and_then(|s| s.to_str());
    let draft = parse_st_card(&bytes, fallback)?;
    save_card_draft(root, &draft, overwrite)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张 V2 规范的最小 ST 卡（含多行台词、占位符、示例对话、世界书字段）
    const V2_JSON: &str = r#"{
  "spec": "chara_card_v2",
  "spec_version": "2.0",
  "data": {
    "name": "月见",
    "description": "{{char}}是天文社的学姐，话不多。",
    "personality": "安静、敏锐",
    "scenario": "深夜的天文台",
    "first_mes": "「你来了。」{{char}}推了推眼镜。\n\n星图还开着。",
    "mes_example": "<START>\n{{user}}: 今晚看什么？\n{{char}}: 木星。肉眼就能看到。\n<START>\n{{user}}: 好冷。\n{{char}}: ……披上这个。",
    "creator": "tester",
    "tags": ["校园", "夜色"],
    "system_prompt": "保持克制。",
    "creator_notes": "测试卡",
    "character_book": { "entries": [] }
  }
}"#;

    #[test]
    fn parse_v2_maps_fields_and_placeholders() {
        let d = parse_st_json(V2_JSON, None).unwrap();
        assert_eq!(d.name, "月见");
        assert_eq!(d.creator.as_deref(), Some("tester"));
        assert_eq!(d.tags, vec!["校园", "夜色"]);
        assert_eq!(d.source_spec, "chara_card_v2");
        // 描述折进 scenario；占位符换成名字
        assert!(d.scenario.contains("深夜的天文台"));
        assert!(d.scenario.contains("月见是天文社的学姐"), "scenario: {}", d.scenario);
        assert!(d.first_mes.contains("「你来了。」月见推了推眼镜。"));
        assert!(d.first_mes.ends_with("星图还开着。"), "多行台词应完整保留");
        assert_eq!(d.personality, "安静、敏锐");
        // 系统提示词与备注进 notes
        assert!(d.notes.contains("[原始系统提示词]"));
        assert!(d.notes.contains("保持克制。"));
        // character_book 未导入要点名
        assert!(d.warnings.iter().any(|w| w.contains("character_book")), "{:?}", d.warnings);
    }

    #[test]
    fn split_examples_into_pairs() {
        let d = parse_st_json(V2_JSON, None).unwrap();
        assert_eq!(d.example_dialogue.len(), 2, "两段 <START> 各成一组");
        let first = &d.example_dialogue[0];
        assert_eq!(first.messages.len(), 2);
        assert_eq!(first.messages[0].role, "user");
        assert_eq!(first.messages[0].content, "今晚看什么？");
        assert_eq!(first.messages[1].role, "char");
        assert_eq!(first.messages[1].content, "木星。肉眼就能看到。");
        assert!(
            !d.notes.contains("今晚看什么"),
            "拆成功的原文不该再进 notes：{}",
            d.notes
        );
    }

    #[test]
    fn unmapped_example_text_goes_to_notes() {
        let json = r#"{ "data": { "name": "甲", "mes_example": "随便写的一句话\n\n又一句" } }"#;
        let d = parse_st_json(json, None).unwrap();
        assert!(d.example_dialogue.is_empty());
        assert!(d.notes.contains("随便写的一句话"), "notes: {}", d.notes);
        assert!(d.warnings.iter().any(|w| w.contains("拆不成问答对")));
    }

    #[test]
    fn v3_and_bare_json_are_accepted() {
        let v3 = r#"{ "spec": "chara_card_v3", "data": { "name": "乙", "first_mes": "嗨" } }"#;
        assert_eq!(parse_st_json(v3, None).unwrap().name, "乙");
        let bare = r#"{ "name": "丙", "first_mes": "喂" }"#;
        let d = parse_st_json(bare, None).unwrap();
        assert_eq!(d.name, "丙");
        assert_eq!(d.source_spec, "未知");
        // 没有名字时退回文件名
        let nameless = r#"{ "first_mes": "喂" }"#;
        assert_eq!(parse_st_json(nameless, Some("文件名")).unwrap().name, "文件名");
        assert!(parse_st_json("[]", None).is_err());
        assert!(parse_st_json("不是 json", None).is_err());
    }

    #[test]
    fn render_lua_and_load_back() {
        let d = parse_st_json(V2_JSON, None).unwrap();
        let source = render_card_lua(&d);
        let loaded = card::load_card_source("月见", &source);
        assert!(!loaded.degraded, "降级原因：{:?}", loaded.degrade_reason);
        assert_eq!(loaded.card.name, "月见");
        assert_eq!(loaded.card.first_mes, d.first_mes);
        assert_eq!(loaded.card.example_dialogue.len(), 2);
        assert_eq!(loaded.card.tags, vec!["校园", "夜色"]);
        assert_eq!(loaded.card.creator.as_deref(), Some("tester"));
    }

    #[test]
    fn lua_str_escapes_hostile_text() {
        // 引号、反斜杠、换行、制表与多字节混排都要能原样回读
        let hostile = "引号\" 反斜杠\\ 换行\n制表\t 结束";
        let source = format!(
            "return {{ spec='charcard/1.0', name='x', scenario={}, personality='', first_mes='' }}",
            lua_str(hostile)
        );
        let loaded = card::load_card_source("x", &source);
        assert!(!loaded.degraded, "{:?}", loaded.degrade_reason);
        assert_eq!(loaded.card.scenario, hostile);
    }

    #[test]
    fn png_text_chunk_roundtrip() {
        // 造一张最小 PNG：签名 + IHDR + tEXt(chara) + IEND
        let card_json = r#"{ "name": "PNG卡", "first_mes": "从 PNG 来" }"#;
        let png = tiny_png_with_chara(card_json);
        assert!(png.starts_with(&PNG_SIG));
        let d = parse_st_card(&png, None).unwrap();
        assert_eq!(d.name, "PNG卡");
        assert_eq!(d.first_mes, "从 PNG 来");

        // 没有文本块的 PNG：报错而不是崩
        let bare = tiny_png_with_chara_raw(None);
        assert!(parse_st_card(&bare, None).is_err());
    }

    #[test]
    fn base64_tolerates_whitespace_and_padding() {
        let raw = b"{\"a\":1}";
        let encoded = encode_base64(raw);
        assert_eq!(decode_base64(&encoded).unwrap(), raw.to_vec());
        // 折行（SillyTavern 导出的 PNG 常见）
        let wrapped = encoded
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(decode_base64(&wrapped).unwrap(), raw.to_vec());
        assert!(decode_base64("!!!").is_err());
    }

    #[test]
    fn save_creates_card_and_dedupes_name() {
        let root = tempfile::tempdir().unwrap();
        let d = parse_st_json(V2_JSON, None).unwrap();
        let first = save_card_draft(root.path(), &d, false).unwrap();
        assert_eq!(first.dir_name, "月见");
        assert!(root.path().join("characters/月见/card.lua").is_file());

        // 同名再来一张：自动 -2，不覆盖已有卡
        let second = save_card_draft(root.path(), &d, false).unwrap();
        assert_eq!(second.dir_name, "月见-2");

        // 显式覆盖：仍写回原目录
        let third = save_card_draft(root.path(), &d, true).unwrap();
        assert_eq!(third.dir_name, "月见");
        // 目录里的卡能被自家解析器读回
        let loaded = card::load_card(root.path(), "月见").unwrap();
        assert!(!loaded.degraded);
    }

    #[test]
    fn import_from_png_file_end_to_end() {
        // 模拟「拖入一张社区卡」：真实 PNG 文件 → import_st_card → characters/<名字>/card.lua
        let root = tempfile::tempdir().unwrap();
        let png_path = root.path().join("社区卡.png");
        let card_json = r#"{ "spec": "chara_card_v2", "data": { "name": "星野", "first_mes": "「来了？」", "description": "天文社的学姐", "mes_example": "<START>\n{{user}}: 在吗\n{{char}}: 在。" } }"#;
        std::fs::write(&png_path, tiny_png_with_chara(card_json)).unwrap();
        let report = import_file_to(root.path(), &png_path, false).unwrap();
        assert_eq!(report.dir_name, "星野");
        let loaded = card::load_card(root.path(), "星野").unwrap();
        assert!(!loaded.degraded, "{:?}", loaded.degrade_reason);
        assert_eq!(loaded.card.first_mes, "「来了？」");
        assert_eq!(loaded.card.example_dialogue.len(), 1, "示例对话应被拆出来");
        assert!(report.card_path.ends_with("card.lua"));
    }

    #[test]
    fn acceptance_samples_import_cleanly() {
        // docs/testdata/ 下的 ST 样本是真机验收第 6 项要拖的素材：
        // 它们必须始终能解析并落成可用的卡（样本腐烂 = 验收当天才发现问题）。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/testdata");
        if !dir.is_dir() {
            return;
        }
        for name in ["st-card-v2.png", "st-card-v2.json"] {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            // 每个样本用独立的临时 DataHub：否则前一个样本留下的同名卡会改变后缀
            let root = tempfile::tempdir().unwrap();
            let report = import_file_to(root.path(), &path, false)
                .unwrap_or_else(|e| panic!("样本 {name} 导入失败：{e}"));
            assert_eq!(report.dir_name, "苏眠", "{name}");
            let loaded = card::load_card(root.path(), &report.dir_name).unwrap();
            assert!(!loaded.degraded, "{name}：{:?}", loaded.degrade_reason);
            assert!(loaded.card.first_mes.contains("雨这么大"), "{name} 多行开场白应完整");
            assert!(
                loaded.card.scenario.contains("苏眠是旧书店"),
                "{name}：description 的 {{{{char}}}} 应替换为角色名（实际：{}）",
                loaded.card.scenario
            );
            assert!(loaded.card.scenario.contains("梅雨季"), "{name}：scenario 应保留");
            assert_eq!(loaded.card.example_dialogue.len(), 2, "{name}：示例对话应拆成两组");
            assert_eq!(loaded.card.tags, vec!["日常", "治愈", "书店"], "{name}");
            // 两次导入同名卡：自动加后缀，不覆盖
            let again = import_file_to(root.path(), &path, false).unwrap();
            assert_eq!(again.dir_name, "苏眠-2", "{name}");
        }
    }

    #[test]
    fn sanitize_rejects_path_traversal() {
        let json = r#"{ "name": "../../逃逸", "first_mes": "x" }"#;
        let d = parse_st_json(json, None).unwrap();
        let root = tempfile::tempdir().unwrap();
        let report = save_card_draft(root.path(), &d, false).unwrap();
        assert!(!report.dir_name.contains('/'), "目录名不应含分隔符");
        assert!(!report.dir_name.contains(".."));
        assert!(root
            .path()
            .join("characters")
            .join(&report.dir_name)
            .join("card.lua")
            .is_file());
    }

    // ---------- 测试辅助：最小 PNG 与 base64 编码 ----------

    fn tiny_png_with_chara(card_json: &str) -> Vec<u8> {
        tiny_png_with_chara_raw(Some(card_json))
    }

    fn tiny_png_with_chara_raw(card_json: Option<&str>) -> Vec<u8> {
        let mut png = PNG_SIG.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes()); // width
        ihdr.extend_from_slice(&1u32.to_be_bytes()); // height
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8bit RGBA，无隔行
        push_chunk(&mut png, b"IHDR", &ihdr);
        if let Some(json) = card_json {
            let mut text = b"chara".to_vec();
            text.push(0);
            text.extend_from_slice(encode_base64(json.as_bytes()).as_bytes());
            push_chunk(&mut png, b"tEXt", &text);
        }
        push_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(data);
        png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    fn encode_base64(raw: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in raw.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
}
