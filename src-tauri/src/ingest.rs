//! 素材规格化管线（M3.9 · 设计 §6.7）：wiki 角色页 / 剧情记录 / 台词集 → 卡与设定集草稿包。
//!
//! 模块纪律与 complete.rs 相同：纯数据与算法，不碰文件不碰网络（各阶段的 LLM 调用由
//! 宿主 commands 侧拿 prompt 去跑，util 档接入点），单测直接钉住。
//!
//! 八步流程的落点（设计 §6.7）：
//! - ① 导入 / ② 清洗分段（确定性）：`clean_source` 去 wiki 标记、`extract_spoilers`
//!   取剧透候选、`segment_sections` 按标题切节；
//! - ③ 分节分类（LLM P1）：`build_classify_prompt` / `parse_classifications`，
//!   拿不准归 unknown = 待定（宁漏勿错）；
//! - ④ 机械映射（确定性）：`parse_infobox` 信息框→schema、剧透→秘密候选、
//!   个人状态→lifecycle、台词表→行切片、mechanics 一律过滤不进任何下游；
//! - ⑤ 语义归纳（LLM P3–P8）：秘密与生命周期 / 描写四法 / 倾向性 / 事件年表与世界线 /
//!   关系网 / 示例对话，逐条附原文引源（`SourceRef`，审阅可跳转）；
//! - ⑥ 查重冲突（⑥在 commands 侧复用 `complete::validate_proposal` / anchors 驳回）；
//! - ⑦ 草稿包审阅：`IngestPack` 前后端往返（全部结构 Serialize+Deserialize），
//!   前端逐条 include/剔除；产物质量由 `qc_pack` 确定性清单把关（P10 的机械子集）；
//! - ⑧ 切入点向导：`build_canon_points` 推导可扮演时点，`apply_canon_point` 按选择
//!   初始化切面（known_by 秘密知情集 / lifecycle / versions 告警 / 世界线阶段）；
//!   「与已死者对话」双处理（更早时点 / 记忆体前提）落在 CanonPoint::premise。
//!
//! P0–P11 提示词套件经 [`SUITE`]（include_str!）打包进产物：管线阶段提示词内嵌同一套
//! 铁律，手动模式整段复制（双用途，套件文档原文照旧是权威）。App 内 P9 装配不走 LLM：
//! P2–P8 的结构化产物由 [`assemble_pack`] 确定性拼装——比让模型吐 Lua 更可靠也更便宜。

use crate::card::ExampleTurn;
use crate::codex::{Relation, Secret};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// P0–P11 提示词套件全文（docs/prompts/ingestion-prompts.md，打包进产物）。
/// `ingest_prompts` 命令把它原样交给界面展示/复制，供「手动·分步/一键」路径使用。
pub const SUITE: &str = include_str!("../../docs/prompts/ingestion-prompts.md");

/// 所有阶段共享的铁律总则（套件 §0 原文，进每条阶段提示词的置顶）。
const SYSTEM_RULES: &str = "\
你是「角色规格化师」，任务是把原始素材（wiki 角色页、剧情记录、台词集、对话场景）\
转写为结构化角色卡与设定实体。\n\
铁律：\n\
1. 只归纳，不创作。素材里没有的信息一律不输出；确有必要的合理推断必须标 \"inference\": true \
并附置信度（high/mid/low），且推断不得进入 anchors 与 secrets。\n\
2. 逐条引源。每个输出字段带 \"source\"（小节标题 + 原文关键句，≤30 字）。引不出源的字段不要输出。\n\
3. 宁漏勿错。拿不准的内容放进 \"pending\" 待定桶，不要编造填充。\n\
4. 忽略游戏机制与数值（面板、圣痕、装备、技能数值、强度评价）——不属于叙事事实。\n\
5. 保留原文风格。台词归纳不得书面化；口癖、语气词、特殊符号（♪、~、……）原样保留。\n\
6. 剧透即秘密：剧透标记（黑幕/spoiler）或明显属于\"后期揭示\"的信息一律进 secrets，\
并注明在哪段剧情被揭示。\n\
7. 输出严格 JSON，不输出 JSON 以外的任何文字。";

// ---------- ①② 导入 / 清洗分段（确定性）----------

/// 清洗分段后的一个小节：id 是全管线里的引源锚点（审阅跳转用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub text: String,
}

/// 剧透候选的标记名/样式名关键词（模板名与 HTML class 共用一套判据）。
const SPOILER_MARKS: [&str; 6] = ["黑幕", "剧透", "spoiler", "hide", "heimu", "mask"];

/// 扫出剧透标记包裹的内容（设计 §6.7「剧透标记→秘密候选」）。
///
/// 支持 wiki 模板（`{{黑幕|内容}}`、`{{Hide|标题|内容}}`）与带 spoiler/heimu class 的
/// HTML 标签。模板体按花括号配对截取，参数去掉模板名后拼接——宁多收一条给人工审，
/// 不漏一条终局反转。
pub fn extract_spoilers(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = raw;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with("{{") {
            if let Some((body, end)) = brace_body(bytes, i) {
                let head = body.split('|').next().unwrap_or("").trim().to_lowercase();
                if SPOILER_MARKS.iter().any(|m| head.contains(m)) {
                    let content: String = body
                        .split('|')
                        .skip(1)
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        // k=v 形态的参数取值（如 {{Hide|标题=xxx|内容=yyy}}）
                        .map(|p| {
                            p.split_once('=')
                                .map(|(_, v)| v.trim().to_string())
                                .unwrap_or_else(|| p.to_string())
                        })
                        .collect::<Vec<_>>()
                        .join("：");
                    if !content.is_empty() && !out.contains(&content) {
                        out.push(content);
                    }
                }
                i = end;
                continue;
            }
        }
        // HTML：class 属性含 spoiler/heimu 的标签，取到对应闭标签为止
        if bytes[i..].starts_with('<') {
            if let Some(gt) = bytes[i..].find('>') {
                let tag = &bytes[i + 1..i + gt];
                let name = tag.split_whitespace().next().unwrap_or("");
                let lower = tag.to_lowercase();
                if !name.is_empty()
                    && (lower.contains("spoiler") || lower.contains("heimu"))
                    && !name.starts_with('/')
                    && !name.ends_with('/')
                {
                    let close = format!("</{}", name.split('(').next().unwrap_or(name));
                    if let Some(rel) = bytes[i + gt + 1..].find(&close) {
                        let inner = bytes[i + gt + 1..i + gt + 1 + rel].trim();
                        if !inner.is_empty() && !out.iter().any(|s| s.contains(inner)) {
                            out.push(strip_tags(inner));
                        }
                    }
                    i += gt + 1;
                    continue;
                }
            }
        }
        // 按字符推进（UTF-8 安全：只在 ASCII 边界特殊处理，其余 +1 个 char）
        let ch_len = bytes[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        i += ch_len;
    }
    out
}

/// 取 `{{` 起始的花括号配对体：返回（体内文本、结束位置）。
fn brace_body(text: &str, start: usize) -> Option<(&str, usize)> {
    let mut depth = 0usize;
    let b = text.as_bytes();
    let mut i = start;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            depth += 1;
            i += 2;
        } else if b[i] == b'}' && b[i + 1] == b'}' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Some((&text[start + 2..i - 2], i));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// 清洗素材：去 HTML 注释/ref/style 块、展开或丢弃 wiki 模板、脱链接与加粗壳、压空行。
///
/// 模板的取舍规则（确定性）：体内含 ≥2 个 `k = v` 参数的（信息框类）展开成 `k = v` 行
/// 保留——机械映射要靠它们；其余模板整体丢弃（引用/导航/结算模板是纯噪声）。
pub fn clean_source(raw: &str) -> String {
    let text = raw;
    // ① 块级噪声：注释 / ref / style / script
    let text = strip_blocks(text, "<!--", "-->");
    let text = strip_blocks(&text, "<ref", "</ref>");
    let text = strip_blocks_selfclosed(&text, "<ref");
    let text = strip_blocks(&text, "<style", "</style>");
    let text = strip_blocks(&text, "<script", "</script>");
    // ② 模板：花括号配对，信息框类展开 k=v 行
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let b = text.as_bytes();
    while i < b.len() {
        if b[i] == b'{' && i + 1 < b.len() && b[i + 1] == b'{' {
            if let Some((body, end)) = brace_body(&text, i) {
                if let Some(kv) = template_kv_lines(body) {
                    out.push('\n');
                    out.push_str(&kv);
                    out.push('\n');
                }
                i = end;
                continue;
            }
        }
        let ch_len = text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    // ③ 链接与强调壳：[[a|b]]→b、[[a]]→a、'''x'''→x、''x''→x
    let out = strip_links(&out);
    let out = strip_tags(&out);
    // ④ 压缩空行（3 连以上 → 2 连）
    let mut compact = String::with_capacity(out.len());
    let mut blanks = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            compact.push('\n');
        } else {
            blanks = 0;
            compact.push_str(line);
            compact.push('\n');
        }
    }
    compact.trim().to_string()
}

/// 模板体 → `k = v` 行（≥2 个命名参数才算信息框类）；其余返回 None（丢弃）。
fn template_kv_lines(body: &str) -> Option<String> {
    let mut params: Vec<String> = Vec::new();
    let mut depth = 0i32; // [[ ]] 嵌套不拆参
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                if chars.peek() == Some(&'[') {
                    depth += 1;
                    cur.push_str("[[");
                    chars.next();
                } else {
                    cur.push(c);
                }
            }
            ']' => {
                if chars.peek() == Some(&']') {
                    depth -= 1;
                    cur.push_str("]]");
                    chars.next();
                } else {
                    cur.push(c);
                }
            }
            '|' if depth == 0 => {
                params.push(cur.trim().to_string());
                cur = String::new();
            }
            _ => cur.push(c),
        }
    }
    params.push(cur.trim().to_string());
    let named: Vec<String> = params
        .iter()
        .skip(1)
        .filter(|p| p.contains('=') && !p.starts_with("http"))
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| format!("{} = {}", k.trim(), v.trim()))
                .unwrap_or_else(|| p.clone())
        })
        .collect();
    if named.len() >= 2 {
        Some(named.join("\n"))
    } else {
        None
    }
}

/// 剥 [[..]] 链接壳，取显示文字（`[[a|b]]`→b，`[[a]]`→a）。
fn strip_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("[[") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                let shown = inner.rsplit('|').next().unwrap_or(inner);
                out.push_str(shown.trim());
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("[[");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 剥 HTML 标签壳（保留标签间文本）。
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 删除 `open…close` 块（全部出现）。
fn strip_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(open) {
        out.push_str(&rest[..pos]);
        match rest[pos..].find(close) {
            Some(end) => rest = &rest[pos + end + close.len()..],
            None => {
                rest = &rest[pos + open.len()..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 删除 `<ref …/>` 自闭合形式（strip_blocks 找不到 `</ref>` 时兜底）。
fn strip_blocks_selfclosed(text: &str, open: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(open) {
        match rest[pos..].find("/>") {
            Some(end) => {
                out.push_str(&rest[..pos]);
                rest = &rest[pos + end + 2..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// 按标题行切节（MediaWiki `== 标题 ==` 与 Markdown `# 标题` 两种）。
/// 首个标题前的内容归「开头」节；整篇无标题 = 单节「全文」。
/// 空的父节（标题下直接是子标题）不立节——标题并入子节的面包屑（「经历 · 往世乐土」），
/// 分类与引源显示都靠标题认路。
pub fn segment_sections(cleaned: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut cur_title = String::from("开头");
    let mut cur = String::new();
    let mut seen_heading = false;
    // 标题栈：(层级, 标题)。当前标题 = 栈中同层以上的标题用「·」串起来
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in cleaned.lines() {
        let t = line.trim();
        if let Some((level, title)) = heading_of(t) {
            if !cur.trim().is_empty() || seen_heading {
                let id = format!("s{}", sections.len() + 1);
                sections.push(Section {
                    id,
                    title: cur_title.clone(),
                    text: cur.trim().to_string(),
                });
                cur = String::new();
            }
            seen_heading = true;
            while stack.last().map(|(l, _)| *l >= level).unwrap_or(false) {
                stack.pop();
            }
            stack.push((level, title.clone()));
            cur_title = stack
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(" · ");
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() || sections.is_empty() {
        sections.push(Section {
            id: format!("s{}", sections.len() + 1),
            title: if seen_heading { cur_title } else { "全文".into() },
            text: cur.trim().to_string(),
        });
    }
    sections.retain(|s| !s.text.trim().is_empty());
    sections
}

/// 标题行 → (层级, 标题文字)（`== X ==` 层 2 / `=== X ===` 层 3 / `# X` 层 1）；非标题返回 None。
fn heading_of(line: &str) -> Option<(usize, String)> {
    if line.starts_with('=') && line.ends_with('=') && line.len() >= 4 {
        let lead = line.chars().take_while(|c| *c == '=').count();
        let tail = line.chars().rev().take_while(|c| *c == '=').count();
        if lead >= 2 && lead == tail {
            let title = line[lead..line.len() - tail].trim();
            if !title.is_empty() {
                return Some((lead, title.to_string()));
            }
        }
        return None;
    }
    let mut hashes = 0usize;
    for c in line.chars() {
        if c == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if (1..=6).contains(&hashes) {
        let rest = line[hashes..].trim();
        if !rest.is_empty() && rest.chars().next() != Some('#') {
            return Some((hashes, rest.to_string()));
        }
    }
    None
}

// ---------- ③ 分节分类（LLM P1）----------

/// 每节输入上限（确定性截断，防超长 wiki 撑爆 util 档上下文）。
pub const MAX_SECTION_CHARS: usize = 6000;

/// 分节分类标签（与套件 P1 一致；mechanics 不参与后续阶段）。
pub const TAGS: [&str; 9] = [
    "infobox",
    "intro",
    "history",
    "relations",
    "dialogue_scene",
    "quote_table",
    "mechanics",
    "trivia",
    "unknown",
];

/// 单节裁剪到 [`MAX_SECTION_CHARS`]（超出加截断标记）。
pub fn clip_section(text: &str) -> String {
    if text.chars().count() <= MAX_SECTION_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_SECTION_CHARS).collect();
    format!("{cut}\n…（本节超长，已截断）")
}

/// 把选中的小节排进提示词（id/标题/正文）。
fn render_sections(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections {
        out.push_str(&format!("【{} · {}】\n{}\n\n", s.id, s.title, clip_section(&s.text)));
    }
    out
}

/// P1 分节分类提示词（套件 §P1 + 总则）。
pub fn build_classify_prompt(sections: &[Section]) -> String {
    let mut p = String::new();
    p.push_str(SYSTEM_RULES);
    p.push_str("\n\n【任务：分节分类】为下面每一节打唯一标签：\n");
    p.push_str("infobox(信息框) / intro(简介) / history(经历) / relations(关系评价) / ");
    p.push_str("dialogue_scene(对话场景·幕间·追忆) / quote_table(台词表) / ");
    p.push_str("mechanics(机制数据) / trivia(考据) / unknown(拿不准)。\n");
    p.push_str("规则：标题与内容矛盾时以内容为准；mechanics 不参与后续阶段；拿不准归 unknown（宁漏勿错）。\n\n");
    p.push_str(&render_sections(sections));
    p.push_str("【输出格式】一个 JSON 数组：[{\"id\":\"s1\",\"tag\":\"intro\",\"reason\":\"一句话\"}, …]，每节恰好一条。");
    p
}

/// P1 回复解析：宽容取 JSON 数组；未知 id / 非法 tag 丢弃（缺失的节由宿主归 unknown）。
pub fn parse_classifications(raw: &str) -> BTreeMap<String, String> {
    // P1 的输出是数组——先按 `[…]` 取，取不到再按对象兜底
    let Some(frag) = extract_json_array(raw).or_else(|| crate::complete::extract_json_object(raw))
    else {
        return BTreeMap::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return BTreeMap::new();
    };
    let list = match v {
        Value::Array(a) => a,
        Value::Object(o) => o.get("sections").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut out = BTreeMap::new();
    for item in list {
        let id = item.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let tag = item
            .get("tag")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("unknown")
            .to_lowercase();
        if id.is_empty() || !TAGS.contains(&tag.as_str()) {
            continue;
        }
        out.insert(id, tag);
    }
    out
}

/// 兜底：取 `[` 到最后一个 `]` 的片段（P1 的回复是数组，不是对象）。
fn extract_json_array(raw: &str) -> Option<&str> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    (end > start).then_some(&raw[start..=end])
}

// ---------- ④ 机械映射（确定性）----------

/// 信息框机械映射的产物（设计 §6.7 映射表：信息框字段→schema）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InfoboxFacts {
    pub name: Option<String>,
    pub aliases: Vec<String>,
    /// look.* 与其他可直落的 facet（点分路径 → 值）
    pub facts: BTreeMap<String, Value>,
    /// 所属团体（→ org 关系与占位实体）
    pub orgs: Vec<String>,
    /// 个人状态原句（→ lifecycle 判定）
    pub status: Option<String>,
    /// 相关人士名单（→ 关系候选）
    pub relations_names: Vec<String>,
    /// 无法映射的键值（进待定，宁漏勿错）
    pub unmapped: Vec<(String, String)>,
}

/// 从信息框节（可多节）做确定性字段映射。
pub fn parse_infobox(sections: &[Section]) -> InfoboxFacts {
    let mut out = InfoboxFacts::default();
    for s in sections {
        for (key, value) in kv_lines(&s.text) {
            let k = key.trim().to_string();
            let v = value.trim().to_string();
            if k.is_empty() || v.is_empty() {
                continue;
            }
            match k.as_str() {
                "本名" | "姓名" | "名字" | "角色名" => {
                    if out.name.is_none() {
                        out.name = Some(v);
                    }
                }
                "别称" | "别名" | "称号" | "昵称" => {
                    out.aliases.extend(split_names(&v));
                }
                "个人状态" | "状态" | "现状" | "结局" => {
                    if out.status.is_none() {
                        out.status = Some(v);
                    }
                }
                "所属" | "所属团体" | "所属组织" | "组织" | "阵营" | "势力" | "隶属" => {
                    out.orgs.extend(split_names(&v));
                }
                "相关人士" | "人际关系" | "亲属" | "家人" => {
                    out.relations_names.extend(split_names(&v));
                }
                _ => {
                    let path = facet_path_of(&k);
                    match path {
                        Some(path) => {
                            out.facts.insert(path, Value::String(v));
                        }
                        None => out.unmapped.push((k, v)),
                    }
                }
            }
        }
    }
    out.aliases.dedup();
    out.orgs.dedup();
    out.relations_names.dedup();
    out
}

/// 信息框键 → facts 点分路径；None = 不认识（进待定）。
fn facet_path_of(key: &str) -> Option<String> {
    const LOOK_KEYS: [&str; 9] = [
        "发色", "瞳色", "发型", "身高", "体重", "生日", "年龄", "瞳", "三围",
    ];
    const FACT_KEYS: [&str; 9] = [
        "职业", "出身", "出生地", "活动范围", "种族", "物种", "代表作品", "声优", "配音",
    ];
    if LOOK_KEYS.contains(&key) {
        let norm = if key == "瞳" { "瞳色" } else if key == "配音" { "声优" } else { key };
        return Some(format!("look.{norm}"));
    }
    if FACT_KEYS.contains(&key) {
        let norm = if key == "配音" { "声优" } else { key };
        return Some(norm.to_string());
    }
    None
}

/// 个人状态原句 → lifecycle（死亡是末段正史，不阻碍从更早切入点扮演）。
/// 返回 (status, note)；无法判定返回 None。
pub fn lifecycle_from_status(status: &str) -> Option<(String, String)> {
    let s = status.trim();
    if s.is_empty() {
        return None;
    }
    const DEAD: [&str; 7] = ["死亡", "已死", "已故", "阵亡", "牺牲", "殒命", "身亡"];
    const GONE: [&str; 6] = ["离开", "离任", "退场", "隐退", "毕业", "失踪"];
    if DEAD.iter().any(|w| s.contains(w)) {
        return Some(("dead".into(), s.to_string()));
    }
    if GONE.iter().any(|w| s.contains(w)) {
        return Some(("departed".into(), s.to_string()));
    }
    Some(("active".into(), s.to_string()))
}

/// 从文本行里抽 `k = v` / `k：v` / `k: v` 键值对（信息框展开行与台词表的公共形态）。
/// wiki 表格行（`| 场合 | 台词 |`）按竖线拆格：首格为键、余格合并为值。
fn kv_lines(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('}') || t.starts_with('!') {
            continue;
        }
        let cells: Vec<&str> = t.trim_start_matches('|').split('|').map(str::trim).collect();
        let cells: Vec<&str> = cells.iter().filter(|c| !c.is_empty() && *c != &"-").copied().collect();
        if cells.len() >= 2 {
            out.push((cells[0].to_string(), cells[1..].join("；")));
            continue;
        }
        let t = t.trim_start_matches('|').trim();
        if t.starts_with('-') {
            continue;
        }
        if let Some((k, v)) = split_kv(t) {
            out.push((k, v));
        }
    }
    out
}

/// 一行 → (键, 值)：优先 ` = `，其次全角/半角冒号（冒号要求键短，避免把正文当键）。
pub fn split_kv(line: &str) -> Option<(String, String)> {
    if let Some((k, v)) = line.split_once(" = ") {
        return Some((k.trim().to_string(), v.trim().to_string()));
    }
    for sep in ['：', ':'] {
        if let Some((k, v)) = line.split_once(sep) {
            let k = k.trim().trim_start_matches('|').trim();
            if !k.is_empty() && k.chars().count() <= 12 && !v.trim().is_empty() {
                return Some((k.to_string(), v.trim().to_string()));
            }
        }
    }
    None
}

/// 台词表行切片：`场合 | 台词`（wiki 表格行）或 `场合：台词`。
/// 只做结构切分——哪句进示例对话、哪句进口癖语料由 P4/P8 语义挑选。
pub fn parse_quote_rows(text: &str) -> Vec<(String, String)> {
    kv_lines(text)
        .into_iter()
        .filter(|(k, v)| {
            !v.is_empty()
                && k.chars().count() <= 16
                && v.chars().count() >= 2
                && v.chars().count() <= 400
        })
        .collect()
}

/// 名单值拆分（、，,/・· 间隔），去空去重；剥括注（「姐姐（筆頭）」→「姐姐」）。
fn split_names(v: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in v.split(['、', '，', ',', '/', '・', '·', ';', '；']) {
        let name = strip_paren(part.trim()).trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// 剥一层中/英文括注。
fn strip_paren(s: &str) -> String {
    let mut out = s.to_string();
    for (open, close) in [("（", "）"), ("(", ")"), ("[", "]")] {
        if let Some(p) = out.find(open) {
            if let Some(q) = out[p..].find(close) {
                out = format!("{}{}", &out[..p], &out[p + q + close.len()..]);
            }
        }
    }
    out
}

// ---------- ⑤ 语义归纳（LLM P3–P8）：提示词与解析 ----------

/// 引源（审阅跳转的锚点）：小节 id + 原文关键句。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub section: String,
    pub quote: String,
}

/// 把 JSON 里的 source 对象/数组宽容读成 SourceRef。
fn source_of(v: &Value) -> Option<SourceRef> {
    let obj = match v {
        Value::Object(o) => Some(o),
        Value::Array(a) => a.first().and_then(Value::as_object),
        Value::String(s) => {
            return Some(SourceRef { section: String::new(), quote: s.trim().to_string() });
        }
        _ => None,
    }?;
    let section = obj.get("section").and_then(Value::as_str).unwrap_or("").trim();
    let quote = obj
        .get("quote")
        .or_else(|| obj.get("source"))
        .or_else(|| obj.get("evidence"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if section.is_empty() && quote.is_empty() {
        return None;
    }
    Some(SourceRef {
        section: section.to_string(),
        quote: quote.chars().take(60).collect(),
    })
}

/// 阶段提示词的公共头：铁律总则 + 输入小节。
fn stage_prompt_head(sections: &[Section]) -> String {
    format!("{}\n\n【素材小节】\n{}\n", SYSTEM_RULES, render_sections(sections))
}

/// 取某几个 tag 的小节（mechanics 永不入选——④ 的一律过滤在选材层就已生效）。
pub fn sections_of<'a>(
    sections: &'a [Section],
    tags: &BTreeMap<String, String>,
    wanted: &[&str],
) -> Vec<Section> {
    sections
        .iter()
        .filter(|s| {
            tags.get(&s.id)
                .map(|t| wanted.contains(&t.as_str()))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

// ----- P3 秘密与生命周期 -----

/// 一条秘密候选（机械剧透或 P3 归纳；known_by 在 commit 时按切入点解析）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretDraft {
    pub key: String,
    pub content: String,
    /// 剧情阶段/事件名（揭示时点；解析不成天数的保持 None = 永远按未揭示处理）
    pub revealed_by: Option<String>,
    /// 模型的初始知情者建议（展示用；实际写入以切入点解析为准）
    pub known_by_advice: Vec<String>,
    pub source: Option<SourceRef>,
    pub include: bool,
    /// "spoiler"（机械剧透候选）| "llm"（P3 归纳）
    pub origin: String,
}

/// lifecycle 草稿（infobox 状态或 P3 判定；at_day 由阶段名解析，0 = 未知时点）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleDraft {
    pub status: String,
    pub at_day: i64,
    pub note: Option<String>,
    pub source: Option<SourceRef>,
}

/// 史变候选（素材暗示「同一事实随剧情变化」；day 由阶段名解析）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionDraft {
    pub facet: String,
    pub value: Value,
    pub day: i64,
    pub note: Option<String>,
    pub source: Option<SourceRef>,
    pub include: bool,
}

pub fn build_secrets_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P3：秘密与生命周期】（角色：{char_name}）\n\
1. secrets：素材里剧透标记或明显属于后期揭示的信息 → \
[{{\"key\": \"短标识\", \"content\": \"秘密内容\", \"known_by\": [\"初始知情者，通常只有本人\"], \
\"revealed_by\": \"在哪段剧情/阶段被揭示\", \"source\": {{\"section\": \"小节id\", \"quote\": \"原文关键句\"}}}}]。\n\
2. lifecycle：角色最终状态（active/departed/dead）+ 生效阶段名 → \
{{\"status\": \"dead\", \"at_stage\": \"阶段名或事件名\", \"note\": \"素材原句\", \"source\": {{…}}}}；\
活着且未离场给 {{\"status\": \"active\"}}。\n\
3. versions：素材暗示同一事实随剧情变化（身份/外貌/阵营转变）→ \
[{{\"facet\": \"facts 路径如 look.impression\", \"value\": \"变化后的值\", \"at_stage\": \"生效阶段名\", \
\"note\": \"变什么\", \"source\": {{…}}}}]。\n\
拿不准的一律放 \"pending\": [\"…\"]。\n\n\
【输出格式】一个 JSON 对象：{{\"secrets\": […], \"lifecycle\": {{…}}, \"versions\": […], \"pending\": […]}}。"
    ));
    p
}

/// P3 回复解析（宽容：各槽独立成败，互不拖垮）。
pub fn parse_secrets(
    raw: &str,
    stage_days: &BTreeMap<String, i64>,
) -> (Vec<SecretDraft>, Option<LifecycleDraft>, Vec<VersionDraft>, Vec<String>) {
    let Some(frag) = crate::complete::extract_json_object(raw) else {
        return (Vec::new(), None, Vec::new(), Vec::new());
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return (Vec::new(), None, Vec::new(), Vec::new());
    };
    let empty = Value::Object(Default::default());
    let obj = v.as_object().unwrap_or(empty.as_object().unwrap());

    let mut secrets = Vec::new();
    for (i, item) in obj
        .get("secrets")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let content = item.get("content").and_then(Value::as_str).unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let revealed = item
            .get("revealed_by")
            .or_else(|| item.get("at_stage"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        secrets.push(SecretDraft {
            key: slug_key(
                item.get("key")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            .unwrap_or_else(|| format!("secret{}", i + 1)),
            content: content.to_string(),
            revealed_by: revealed,
            known_by_advice: text_list(item.get("known_by")),
            source: source_of(item.get("source").unwrap_or(&Value::Null)),
            include: true,
            origin: "llm".into(),
        });
    }

    let lifecycle = obj.get("lifecycle").and_then(|l| {
        let status = l.get("status").and_then(Value::as_str)?.trim().to_lowercase();
        if !["active", "departed", "dead"].contains(&status.as_str()) {
            return None;
        }
        let stage = l
            .get("at_stage")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        Some(LifecycleDraft {
            status,
            at_day: stage_days
                .iter()
                .find(|(name, _)| fold_contains(name, stage))
                .map(|(_, d)| *d)
                .unwrap_or(0),
            note: l.get("note").and_then(Value::as_str).map(str::to_string),
            source: source_of(l.get("source").unwrap_or(&Value::Null)),
        })
    });

    let mut versions = Vec::new();
    for item in obj
        .get("versions")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let facet = item.get("facet").and_then(Value::as_str).unwrap_or("").trim();
        let value = item.get("value").cloned().unwrap_or(Value::Null);
        if facet.is_empty() || value.is_null() {
            continue;
        }
        let stage = item
            .get("at_stage")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let day = stage_days
            .iter()
            .find(|(name, _)| fold_contains(name, stage))
            .map(|(_, d)| *d);
        let (day, note) = match day {
            Some(d) => (d, item.get("note").and_then(Value::as_str).map(str::to_string)),
            None => (
                0,
                Some(
                    item.get("at_stage")
                        .and_then(Value::as_str)
                        .map(|s| format!("生效阶段「{s}」解析不出故事天，暂缓"))
                        .or_else(|| item.get("note").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| "生效阶段解析不出故事天，暂缓".into()),
                ),
            ),
        };
        versions.push(VersionDraft {
            facet: facet.to_string(),
            value,
            day,
            note,
            source: source_of(item.get("source").unwrap_or(&Value::Null)),
            include: day > 0,
        });
    }

    let pending = obj
        .get("pending")
        .map(|x| text_list(Some(x)))
        .unwrap_or_default();
    (secrets, lifecycle, versions, pending)
}

// ----- P4 描写四法 -----

/// 四法产物：facts 点分路径 → 值 + 逐路径引源（one_liner 一并在此产出，供装配）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FourMethods {
    /// facts 路径 → 值（look.impression / speech.tics / …）
    pub facets: BTreeMap<String, Value>,
    pub sources: BTreeMap<String, SourceRef>,
    /// ≤40 字含最强辨识点的一句话（App 装配需要；对应套件 P9 的活）
    pub one_liner: String,
    pub source: Option<SourceRef>,
    pub pending: Vec<String>,
}

pub fn build_four_methods_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P4：描写四法归纳】（角色：{char_name}）\n\
A. speech（语言）：style 句式/长度/语气；tics 高频口癖 ≤3 条每条附引句；\
by_affect 不同情绪下的语言变化；per_interlocutor 对特定对象的说话差异（若有素材）。\n\
B. look（外貌）：impression 一句话整体印象（传神优先）；anchors 恒定辨识点候选 2–4 条\
每条附引句，素材不足就放 pending；by_state 随状态的外貌变化（若有素材）。\n\
C. mannerisms（动作）：habits 反复出现的小动作附引句；by_affect 情绪对应的特有动作。\n\
D. tells（心理外化）：从叙述归纳「情绪→可见线索」映射，无据不造。\n\
再给 one_liner：一句 ≤40 字、含最强辨识点的整体介绍（这是她被提起时最先出现的一行）。\n\
铁律：anchors 与 tics 宁少而精，求传神不求全貌；每个路径带 source；识别度优先。\n\n\
【输出格式】一个 JSON 对象：\n\
{{\"one_liner\": \"…\", \"facets\": {{\"look.impression\": \"…\", \"look.anchors\": [\"…\"], \
\"speech.style\": \"…\", \"speech.tics\": [\"…\"], \"speech.by_affect\": {{\"害羞\": \"…\"}}, \
\"mannerisms.habits\": [\"…\"], \"tells\": {{\"喜\": \"…\", \"怒\": \"…\"}}}}, \
\"sources\": {{\"look.impression\": {{\"section\": \"s2\", \"quote\": \"…\"}}}}, \
\"pending\": [\"…\"]}}。facets 只写有据的路径。"
    ));
    p
}

pub fn parse_four_methods(raw: &str) -> FourMethods {
    let mut out = FourMethods::default();
    let Some(frag) = crate::complete::extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return out;
    };
    out.one_liner = v
        .get("one_liner")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    out.source = source_of(v.get("source").unwrap_or(&Value::Null));
    if let Some(obj) = v.get("facets").and_then(Value::as_object) {
        for (path, value) in obj {
            let path = path.trim();
            if path.is_empty() || is_empty_value(value) {
                continue;
            }
            out.facets.insert(path.to_string(), value.clone());
        }
    }
    if let Some(obj) = v.get("sources").and_then(Value::as_object) {
        for (path, s) in obj {
            if let Some(r) = source_of(s) {
                out.sources.insert(path.trim().to_string(), r);
            }
        }
    }
    out.pending = v.get("pending").map(|x| text_list(Some(x))).unwrap_or_default();
    out
}

// ----- P5 倾向性与心理 -----

pub fn build_psyche_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P5：倾向性与心理画像】（角色：{char_name}）\n\
motivation 核心动机 1 条；needs 底层需要 2–4 条；values 价值观 2–3 条；interests 兴趣；\n\
temperament {{rise, decay, threshold, impulsiveness}} 四参数（0–1 的数，从行为证据推断，全部标 inference）；\n\
「他证」（他人评价）是重要证据，引用评价人原句；每条带 source；拿不准放 pending。\n\n\
【输出格式】一个 JSON 对象：\n\
{{\"facets\": {{\"motivation\": \"…\", \"needs\": [\"…\"], \"values\": [\"…\"], \"interests\": \"…\", \
\"temperament\": {{\"rise\": 0.5, \"decay\": 0.5, \"threshold\": 0.5, \"impulsiveness\": 0.5}}}}, \
\"sources\": {{\"motivation\": {{\"section\": \"s3\", \"quote\": \"…\"}}}}, \"pending\": [\"…\"]}}。"
    ));
    p
}

pub fn parse_psyche(raw: &str) -> FourMethods {
    parse_four_methods(raw)
}

// ----- P6 事件年表与世界线 -----

/// P6 产物：事件实体草稿 + 世界线候选（premise + stages）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventsOut {
    /// one 事件一条：name/cause/process/outcome/stage + source
    pub events: Vec<EventDraft>,
    pub premise: String,
    /// 阶段弧候选：id/名/生效天（when.day）/directive 草案
    pub stages: Vec<StageDraft>,
    pub source: Option<SourceRef>,
    pub pending: Vec<String>,
}

/// 一条事件（→ event 实体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    pub title: String,
    pub cause: String,
    pub process: String,
    pub outcome: String,
    /// 所属阶段（映射不成天数的进待定）
    pub stage: Option<String>,
    pub actors: Vec<String>,
    pub source: Option<SourceRef>,
    pub include: bool,
}

/// 世界线阶段候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageDraft {
    pub id: String,
    /// 人读名（「序章」——秘密 revealed_by/事件 stage 常用它锚时点）
    #[serde(default)]
    pub name: String,
    pub day: i64,
    pub directive: String,
    pub include: bool,
}

pub fn build_events_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P6：事件年表与世界线】（角色：{char_name}）\n\
1. events：按时间整理事件，每件 {{\"title\", \"cause\", \"process\"(≤2句), \"outcome\", \
\"stage\": \"所属阶段名\", \"actors\": […], \"source\": {{…}}}}；起因/经过/结果缺则该件放 pending。\n\
2. worldline：给 premise（世界级起因一句）+ stages 3–6 段：\
{{\"id\": \"英文短标识\", \"name\": \"阶段名\", \"when\": {{{{\"day\": N}}}}（N 递增整数故事天）, \
\"directive\": \"该阶段大势 ≤2 句\"}}。阶段要覆盖素材的时间跨度：开头 1、此后递增，\
最后一阶段 = 素材终态。\n\
阶段名同时是切入点断点与秘密揭示时点的锚——命名要能被 P3 的 revealed_by 对上。\n\n\
【输出格式】一个 JSON 对象：\n\
{{\"events\": […], \"premise\": \"…\", \"stages\": [{{\"id\": \"opening\", \"name\": \"序章\", \
\"when\": {{\"day\": 1}}, \"directive\": \"…\"}}], \"pending\": [\"…\"]}}。"
    ));
    p
}

pub fn parse_events(raw: &str) -> EventsOut {
    let mut out = EventsOut::default();
    let Some(frag) = crate::complete::extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return out;
    };
    for item in v
        .get("events")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let title = item.get("title").and_then(Value::as_str).unwrap_or("").trim();
        if title.is_empty() {
            continue;
        }
        let f = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let (cause, process, outcome) = (f("cause"), f("process"), f("outcome"));
        let stage = item
            .get("stage")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.events.push(EventDraft {
            title: title.to_string(),
            cause,
            process,
            outcome,
            stage,
            actors: text_list(item.get("actors")),
            source: source_of(item.get("source").unwrap_or(&Value::Null)),
            include: true,
        });
    }
    out.premise = v
        .get("premise")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    for item in v
        .get("stages")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let id = slug_key(
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let day = day_of_when(item.get("when"));
        if id.is_none() || day == 0 {
            continue; // 无 id / 无生效天的阶段立不住（when.day 是世界时钟判据）
        }
        out.stages.push(StageDraft {
            id: id.unwrap(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            day,
            directive: item
                .get("directive")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            include: true,
        });
    }
    out.stages.sort_by_key(|s| s.day);
    out.source = source_of(v.get("source").unwrap_or(&Value::Null));
    out.pending = v.get("pending").map(|x| text_list(Some(x))).unwrap_or_default();
    out
}

/// when 字段宽容取天：{"day": N} | N（字符串数字也认）。
fn day_of_when(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::Object(o)) => o
            .get("day")
            .and_then(Value::as_i64)
            .or_else(|| o.get("day").and_then(Value::as_str).and_then(|s| s.trim().parse().ok()))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

// ----- P7 关系网 -----

/// P7 产物：关系边 + 他证画像（占位实体的 one_liner 素材）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelationsOut {
    pub relations: Vec<RelationEdge>,
    /// 每位评价者的一句话画像（他证摘要）
    pub portraits: Vec<Portrait>,
    pub pending: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationEdge {
    pub to: String,
    pub kind: String,
    pub valence: String,
    pub evidence: Option<SourceRef>,
    pub include: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Portrait {
    pub who: String,
    pub line: String,
    pub source: Option<SourceRef>,
}

pub fn build_relations_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P7：关系网提取】（角色：{char_name}）\n\
relations：{{\"to\": \"对方名字\", \"kind\": \"关系类型\", \"valence\": \"正|负|复杂\", \
\"evidence\": {{\"section\", \"quote\"}}（评价原句或互动证据）}}；\n\
他证摘要 portraits：每位评价者对该角色的一句话画像 {{\"who\": \"评价者\", \"line\": \"画像\", \"source\": {{…}}}}。\n\
to 用素材里的称呼原名；每条带引源；拿不准放 pending。\n\n\
【输出格式】一个 JSON 对象：{{\"relations\": […], \"portraits\": […], \"pending\": […]}}。"
    ));
    p
}

pub fn parse_relations(raw: &str) -> RelationsOut {
    let mut out = RelationsOut::default();
    let Some(frag) = crate::complete::extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return out;
    };
    for item in v
        .get("relations")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let to = item.get("to").and_then(Value::as_str).unwrap_or("").trim();
        if to.is_empty() {
            continue;
        }
        out.relations.push(RelationEdge {
            to: strip_paren(to),
            kind: item
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("related")
                .trim()
                .to_string(),
            valence: item
                .get("valence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            evidence: source_of(item.get("evidence").or_else(|| item.get("source")).unwrap_or(&Value::Null)),
            include: true,
        });
    }
    for item in v
        .get("portraits")
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let who = item.get("who").and_then(Value::as_str).unwrap_or("").trim();
        let line = item.get("line").and_then(Value::as_str).unwrap_or("").trim();
        if who.is_empty() || line.is_empty() {
            continue;
        }
        out.portraits.push(Portrait {
            who: strip_paren(who),
            line: line.to_string(),
            source: source_of(item.get("source").unwrap_or(&Value::Null)),
        });
    }
    out.pending = v.get("pending").map(|x| text_list(Some(x))).unwrap_or_default();
    out
}

// ----- P8 示例对话 -----

/// P8 产物：示例对话（卡片 few-shot）+ 开场白建议。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExamplesOut {
    pub turns: Vec<ExampleTurn>,
    /// 开场白（素材里她的第一段台词/名场面转写的对玩家第一句话）；缺省空
    pub opening: String,
    pub pending: Vec<String>,
}

pub fn build_examples_prompt(sections: &[Section], char_name: &str) -> String {
    let mut p = stage_prompt_head(sections);
    p.push_str(&format!(
        "\n【任务 P8：示例对话选编】（角色：{char_name}）\n\
选 ≤6 组最具辨识度的对话：{{\"tag\": \"情绪或场合\", \"interlocutor\": \"对象（若有）\", \
\"messages\": [{{\"role\": \"user\"|\"char\", \"content\": \"…\"}}], \
\"source\": {{\"section\", \"quote\"}}, \"why\": \"一句话：为什么这组有代表性\"}}。\n\
优先级：覆盖不同情绪状态 > 覆盖不同对话对象 > 名场面。台词保留原风格（口癖/语气词/♪ 原样），\
不得书面化；char 的台词就是 {char_name} 说的。\n\
另给 opening：从素材里选/改写一段她对玩家说的开场白（她主动开口的第一句话，1–3 句）；\
素材里没有合适的就留空。\n\n\
【输出格式】一个 JSON 对象：{{\"opening\": \"…\", \"examples\": […], \"pending\": […]}}。"
    ));
    p
}

pub fn parse_examples(raw: &str) -> ExamplesOut {
    let mut out = ExamplesOut::default();
    let Some(frag) = crate::complete::extract_json_object(raw) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(frag) else {
        return out;
    };
    out.opening = v
        .get("opening")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    for item in v
        .get("examples")
        .or_else(|| v.get("turns"))
        .and_then(Value::as_array)
        .map(|a| a.clone())
        .unwrap_or_default()
    {
        let mut messages = Vec::new();
        for m in item.get("messages").and_then(Value::as_array).map(|a| a.clone()).unwrap_or_default() {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("char");
            let content = m.get("content").and_then(Value::as_str).unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let role = if role.contains("user") || role.contains("玩家") {
                "user"
            } else {
                "char"
            };
            messages.push(crate::card::ExampleLine {
                role: role.to_string(),
                content: content.to_string(),
            });
        }
        if messages.is_empty() {
            continue;
        }
        out.turns.push(ExampleTurn {
            tag: item
                .get("tag")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            messages,
        });
    }
    if out.turns.len() > 6 {
        out.turns.truncate(6);
    }
    out.pending = v.get("pending").map(|x| text_list(Some(x))).unwrap_or_default();
    out
}

// ---------- 草稿包（⑦ 审阅的前后端契约）----------

/// 一条待定项（宁漏勿错的落点；审阅界面可见）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingItem {
    pub title: String,
    pub detail: String,
    pub source: Option<SourceRef>,
}

/// 一条确定性质检结论（P10 的机械子集；severity: warn | info）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QcIssue {
    pub severity: String,
    /// 出问题的位置（实体/facet/卡字段）
    pub at: String,
    pub problem: String,
}

/// 卡侧草稿（落盘 = characters/<名>/card.lua，与 ST 导入同一条路）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardSide {
    pub first_mes: String,
    pub scenario: String,
    pub personality: String,
    pub tags: Vec<String>,
    pub example_dialogue: Vec<ExampleTurn>,
    pub sources: BTreeMap<String, SourceRef>,
}

/// 一条实体草稿（角色本体 / 占位 / 事件共用；commit 时转 JSON 写 grown.json）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDraft {
    pub id: String,
    pub ty: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub one_liner: String,
    pub facts: BTreeMap<String, Value>,
    pub relations: Vec<Relation>,
    pub sources: BTreeMap<String, SourceRef>,
    pub include: bool,
    /// 占位实体（关系目标/组织）：只有名字与（可能的）他证画像，交给补全管线接力
    pub stub: bool,
}

/// 完整草稿包（⑦ 审阅的往返对象）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestPack {
    pub world: String,
    pub char_id: String,
    pub card: CardSide,
    pub entity: EntityDraft,
    /// 占位与其他实体（组织、关系目标）
    pub others: Vec<EntityDraft>,
    /// 事件实体
    pub events: Vec<EntityDraft>,
    /// 秘密候选（commit 时按切入点解析 known_by）
    pub secrets: Vec<SecretDraft>,
    pub lifecycle: Option<LifecycleDraft>,
    /// 史变候选
    pub versions: Vec<VersionDraft>,
    pub worldline: Option<WorldlineDraft>,
    /// ⑧ 切入点候选
    pub canon_points: Vec<CanonPoint>,
    pub pending: Vec<PendingItem>,
    pub qc: Vec<QcIssue>,
}

/// 世界线候选（commit 时渲染成 codex/<世界>/worldline.lua）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldlineDraft {
    pub id: String,
    pub premise: String,
    pub stages: Vec<StageDraft>,
}

/// 切入点候选（⑧ 向导的一行选项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonPoint {
    pub name: String,
    pub day: i64,
    /// 对应世界线阶段 id（无主线为 None）
    pub stage: Option<String>,
    pub note: String,
    /// 此点在角色死亡之后（「与已死者对话」）
    pub after_death: bool,
    /// 该点建议的剧本前提（死亡后 = 记忆体/残留框架）
    pub premise: Option<String>,
}

// ---------- 草稿包装配（P2–P8 结构化产物 → IngestPack，确定性）----------

/// 装配输入：④ 的机械映射产物 + ⑤ 的各步产物。
pub struct AssembleInputs<'a> {
    pub world: &'a str,
    pub infobox: &'a InfoboxFacts,
    /// 机械剧透候选（内容列表）
    pub spoilers: &'a [String],
    /// P3 产物（lifecycle 已带 infobox 状态回落）
    pub secrets: Vec<SecretDraft>,
    pub lifecycle: Option<LifecycleDraft>,
    pub versions: Vec<VersionDraft>,
    pub four: &'a FourMethods,
    pub psyche: &'a FourMethods,
    pub events: &'a EventsOut,
    pub relations: &'a RelationsOut,
    pub examples: &'a ExamplesOut,
    /// 阶段名 → 故事天（P6 stages 解析产物，秘密/史变/lifecycle 的时点锚）
    pub stage_days: &'a BTreeMap<String, i64>,
}

/// 确定性装配草稿包：全部产物落位、待定与质检结论齐备，前端直接可审。
pub fn assemble_pack(input: &AssembleInputs<'_>) -> IngestPack {
    let name = input
        .infobox
        .name
        .clone()
        .unwrap_or_else(|| "未命名".into());
    let char_id = entity_id("char", &name);
    let mut pending: Vec<PendingItem> = Vec::new();
    let mut sources: BTreeMap<String, SourceRef> = BTreeMap::new();

    // ---- 角色实体 facts：四法 + 倾向性 + 信息框杂项（dotted 路径折叠成嵌套）----
    let mut flat: BTreeMap<String, Value> = BTreeMap::new();
    for batch in [input.four, input.psyche] {
        for (path, value) in &batch.facets {
            flat.insert(path.clone(), value.clone());
        }
        sources.extend(batch.sources.clone());
    }
    for (path, value) in &input.infobox.facts {
        flat.entry(path.clone()).or_insert_with(|| value.clone());
    }
    let mut facts: BTreeMap<String, Value> = BTreeMap::new();
    for (path, value) in flat {
        nest_facet(&mut facts, &path, value);
    }

    // ---- 关系边（P7 + 信息框相关人士；to 与占位实体对齐）----
    let mut relations: Vec<Relation> = Vec::new();
    let mut others: Vec<EntityDraft> = Vec::new();
    let mut known: BTreeSet<String> = BTreeSet::from([char_id.clone()]);
    let mut rel_seen: BTreeSet<String> = BTreeSet::new();
    let mut portraits: BTreeMap<String, String> = input
        .relations
        .portraits
        .iter()
        .map(|p| (p.who.clone(), p.line.clone()))
        .collect();

    // 占位实体登记：返回 None = 已知（不再建）；否则建 stub 并占名
    fn ensure_stub(
        others: &mut Vec<EntityDraft>,
        known: &mut BTreeSet<String>,
        ty: &str,
        name: &str,
        one_liner: String,
    ) -> Option<String> {
        let id = entity_id(ty, name);
        if !known.insert(id.clone()) {
            return None;
        }
        others.push(EntityDraft {
            id: id.clone(),
            ty: ty.to_string(),
            name: name.to_string(),
            aliases: vec![],
            one_liner,
            facts: BTreeMap::new(),
            relations: vec![],
            sources: BTreeMap::new(),
            include: true,
            stub: true,
        });
        Some(id)
    }

    // 关系边登记：按 to 去重；指向角色自己（自环）跳过
    fn add_rel(
        relations: &mut Vec<Relation>,
        seen: &mut BTreeSet<String>,
        self_id: &str,
        to: String,
        kind: String,
        always_with: bool,
    ) {
        if to == self_id || !seen.insert(to.clone()) {
            return;
        }
        relations.push(Relation { to, kind, always_with });
    }

    for edge in &input.relations.relations {
        let ty = if looks_like_org(&edge.to) { "org" } else { "char" };
        let line = portraits.remove(&edge.to).unwrap_or_default();
        if let Some(id) = ensure_stub(&mut others, &mut known, ty, &edge.to, line) {
            add_rel(
                &mut relations,
                &mut rel_seen,
                &char_id,
                id,
                normalize_kind(&edge.kind),
                false,
            );
        }
    }
    for rel_name in &input.infobox.relations_names {
        if let Some(id) = ensure_stub(&mut others, &mut known, "char", rel_name, String::new()) {
            add_rel(&mut relations, &mut rel_seen, &char_id, id, "related".into(), false);
        }
    }
    for org in &input.infobox.orgs {
        if let Some(id) = ensure_stub(&mut others, &mut known, "org", org, String::new()) {
            add_rel(&mut relations, &mut rel_seen, &char_id, id, "所属".into(), true);
        }
    }
    // 没被关系引用的他证画像仍值得一个占位实体（画像就是 one_liner）
    let leftover: Vec<(String, String)> = portraits.into_iter().collect();
    for (who, line) in &leftover {
        ensure_stub(&mut others, &mut known, "char", who, line.clone());
    }

    // ---- 事件实体 ----
    let mut events: Vec<EntityDraft> = Vec::new();
    for ev in &input.events.events {
        let complete = !ev.cause.is_empty() && !ev.outcome.is_empty();
        if !complete {
            pending.push(PendingItem {
                title: format!("事件「{}」六要素不全", ev.title),
                detail: "起因/经过/结果有缺——按 P6 铁律宁漏勿错，未立实体；可在审阅后手动补建".into(),
                source: ev.source.clone(),
            });
            continue;
        }
        let mut efacts: BTreeMap<String, Value> = BTreeMap::new();
        efacts.insert("cause".into(), Value::String(ev.cause.clone()));
        if !ev.process.is_empty() {
            efacts.insert("development".into(), Value::String(ev.process.clone()));
        }
        efacts.insert("outcome".into(), Value::String(ev.outcome.clone()));
        if let Some(stage) = &ev.stage {
            if input.stage_days.contains_key(stage) || !input.stage_days.is_empty() {
                efacts.insert("stage".into(), Value::String(stage.clone()));
            }
        }
        let mut esources = BTreeMap::new();
        if let Some(s) = &ev.source {
            esources.insert("cause".to_string(), s.clone());
        }
        events.push(EntityDraft {
            id: entity_id("event", &ev.title),
            ty: "event".into(),
            name: ev.title.clone(),
            aliases: vec![],
            one_liner: if ev.cause.chars().count() > 4 {
                format!("{}：{}", ev.title, ev.cause)
            } else {
                ev.title.clone()
            },
            facts: efacts,
            relations: vec![],
            sources: esources,
            include: true,
            stub: false,
        });
    }

    // ---- 秘密候选合并：机械剧透 + P3（按内容归一去重；P3 的揭示时点/引源补给剧透候选）----
    let mut secrets: Vec<SecretDraft> = Vec::new();
    for content in input.spoilers {
        if secrets.iter().any(|s| fold_contains(&s.content, content)) {
            continue;
        }
        secrets.push(SecretDraft {
            key: String::new(), // 装配末尾按内容生成
            content: content.clone(),
            revealed_by: None,
            known_by_advice: vec![name.clone()],
            source: None,
            include: true,
            origin: "spoiler".into(),
        });
    }
    for s in &input.secrets {
        if let Some(x) = secrets.iter_mut().find(|x| fold_contains(&x.content, &s.content)) {
            // 同一条秘密：剧透候选只是「原文长这样」，P3 的时点与引源更完整——并进来
            if x.revealed_by.is_none() {
                x.revealed_by = s.revealed_by.clone();
            }
            if x.source.is_none() {
                x.source = s.source.clone();
            }
            if x.key.is_empty() {
                x.key = s.key.clone();
            }
            continue;
        }
        secrets.push(s.clone());
    }
    for (i, s) in secrets.iter_mut().enumerate() {
        if s.key.is_empty() {
            s.key = slug_key(&s.content).unwrap_or_else(|| format!("s{}", i + 1));
        }
    }

    // ---- 史变候选 ----
    let versions: Vec<VersionDraft> = input
        .versions
        .iter()
        .filter(|v| v.day > 0 || !v.include)
        .cloned()
        .collect();

    // ---- 世界线候选 ----
    let worldline = (!input.events.stages.is_empty()).then(|| WorldlineDraft {
        id: "main".into(),
        premise: input.events.premise.clone(),
        stages: input.events.stages.clone(),
    });

    // ---- 卡侧 ----
    let mut card_sources: BTreeMap<String, SourceRef> = BTreeMap::new();
    if let Some(s) = &input.four.source {
        card_sources.insert("example_dialogue".to_string(), s.clone());
    }
    let personality = personality_line(input.psyche);
    let card = CardSide {
        first_mes: input.examples.opening.clone(),
        scenario: input.events.premise.clone(),
        personality,
        tags: vec!["素材规格化".into()],
        example_dialogue: input.examples.turns.clone(),
        sources: card_sources,
    };
    if input.examples.opening.is_empty() {
        pending.push(PendingItem {
            title: "开场白".into(),
            detail: "素材里没有合适的开场白——card.first_mes 为空，可在审阅时手写或开聊后由她自己做".into(),
            source: None,
        });
    }

    // ---- lifecycle（infobox 状态回落 + P3 优先）----
    let lifecycle = input.lifecycle.clone().or_else(|| {
        input.infobox.status.as_deref().and_then(lifecycle_from_status).map(|(status, note)| {
            LifecycleDraft { status, at_day: 0, note: Some(note), source: None }
        })
    });
    if let Some(lc) = &lifecycle {
        if lc.status == "dead" && lc.at_day == 0 {
            pending.push(PendingItem {
                title: "死亡时点".into(),
                detail: "素材记录她已死亡，但解析不出生效故事天——lifecycle 不落值；\
请在世界线里补一阶段锚定，或以「记忆体/残留」前提开场"
                    .into(),
                source: lc.source.clone(),
            });
        }
    }

    // ---- 待定与缺口（信息框 unmapped / 各步 pending 归并）----
    for (k, v) in &input.infobox.unmapped {
        pending.push(PendingItem {
            title: format!("信息框字段「{k}」"),
            detail: v.clone(),
            source: None,
        });
    }
    for item in &input.four.pending {
        pending.push(PendingItem { title: "描写四法".into(), detail: item.clone(), source: None });
    }
    for item in &input.psyche.pending {
        pending.push(PendingItem { title: "倾向性".into(), detail: item.clone(), source: None });
    }
    for item in &input.events.pending {
        pending.push(PendingItem { title: "事件年表".into(), detail: item.clone(), source: None });
    }
    for item in &input.relations.pending {
        pending.push(PendingItem { title: "关系网".into(), detail: item.clone(), source: None });
    }
    for item in &input.examples.pending {
        pending.push(PendingItem { title: "示例对话".into(), detail: item.clone(), source: None });
    }

    let mut qc = Vec::new();
    if input.four.one_liner.is_empty() {
        qc.push(QcIssue {
            severity: "warn".into(),
            at: "one_liner".into(),
            problem: "缺一句话介绍（≤40 字含辨识点）——她被提起时的第一行，强烈建议补上".into(),
        });
    }

    let entity = EntityDraft {
        id: char_id.clone(),
        ty: "char".into(),
        name,
        aliases: input.infobox.aliases.clone(),
        one_liner: input.four.one_liner.clone(),
        facts,
        relations,
        sources,
        include: true,
        stub: false,
    };
    let world_ref = input.world.to_string();
    let mut pack = IngestPack {
        world: world_ref,
        char_id: entity.id.clone(),
        card,
        entity,
        others,
        events,
        secrets,
        lifecycle,
        versions,
        worldline,
        canon_points: Vec::new(),
        pending,
        qc,
    };
    pack.canon_points = build_canon_points(&pack);
    pack.qc.extend(qc_pack(&pack));
    pack
}

/// 一句话性格摘要（card.personality）：动机 + 需要 + 价值观的确定性拼装。
fn personality_line(psyche: &FourMethods) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(Value::String(m)) = psyche.facets.get("motivation") {
        parts.push(format!("动机：{m}"));
    }
    if let Some(Value::Array(a)) = psyche.facets.get("needs") {
        let joined: Vec<String> = a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !joined.is_empty() {
            parts.push(format!("需要：{}", joined.join("、")));
        }
    }
    if let Some(Value::Array(a)) = psyche.facets.get("values") {
        let joined: Vec<String> = a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !joined.is_empty() {
            parts.push(format!("价值观：{}", joined.join("、")));
        }
    }
    parts.join("；")
}

/// 实体 id：`类型.名字`（名字剥路径与键值噪声字符；中文原样保留，与手写实体同构）。
pub fn entity_id(ty: &str, name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_whitespace() && !"[]{}\"'`,;|.#".contains(*c))
        .collect();
    format!("{ty}.{cleaned}")
}

/// 秘密键：内容前几个安全字符（commit 时作 secrets 对象的键）。
fn slug_key(s: &str) -> Option<String> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && !"[]{}\"'`,;|.:：#".contains(*c))
        .take(12)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// 关系类型归一：空/超长回落 related（注入模板按 kind 渲染，要短）。
fn normalize_kind(kind: &str) -> String {
    let k = kind.trim();
    if k.is_empty() || k.chars().count() > 12 {
        "related".into()
    } else {
        k.to_string()
    }
}

/// 名字是否像组织（后缀判据：社/团/会/组织/学院/学园/校/部/教/军/司/局/党/派/门/宗/家）。
fn looks_like_org(name: &str) -> bool {
    const SUF: [&str; 15] = [
        "社", "团", "会", "组织", "組織", "学院", "学园", "學園", "学校", "部", "教", "军", "軍",
        "局", "派",
    ];
    SUF.iter().any(|s| name.ends_with(s))
}

/// 值是否「空」（空串/空数组/空对象/null——与补全管线的槽位判定同口径）。
fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// 把 `look.anchors` 这类点分路径折叠进嵌套 facts（codex 的 anchors()/注入模板都按
/// 嵌套对象读取；中途撞上非对象值时升级为对象，后写覆盖）。
fn nest_facet(facts: &mut BTreeMap<String, Value>, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        facts.insert(parts[0].to_string(), value);
        return;
    }
    let head = parts[0].to_string();
    let rest = parts[1..].join(".");
    let entry = facts
        .entry(head)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    nest_value(entry, &rest, value);
}

fn nest_value(cur: &mut Value, path: &str, value: Value) {
    let mut parts: Vec<&str> = path.split('.').map(str::trim).filter(|s| !s.is_empty()).collect();
    let Some(last) = parts.pop() else { return };
    let Some(obj) = cur.as_object_mut() else { return };
    if parts.is_empty() {
        obj.insert(last.to_string(), value);
        return;
    }
    let next = parts.remove(0);
    let entry = obj
        .entry(next.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    nest_value(entry, &parts.join("."), value);
}

/// Rust 字符串 → Lua 字符串字面量（UTF-8 直排，中文保持可读；只转义引号/反斜杠/控制符）。
/// 与 stimport::lua_str 的字节转义版语义等价——世界线声明是玩家会在编辑器里读的文件。
fn lua_str_utf8(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 宽容读字符串数组。
fn text_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::trim))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => Vec::new(),
    }
}

/// 不区分大小写的包含（引源/阶段名匹配的公共口径）。
fn fold_contains(hay: &str, needle: &str) -> bool {
    let hay = hay.trim().to_lowercase();
    let needle = needle.trim().to_lowercase();
    !needle.is_empty() && (hay.contains(&needle) || needle.contains(&hay))
}

// ---------- ⑧ 切入点向导（确定性推导）----------

/// 推导可扮演时点（P11 的宿主侧兑现）：世界线各阶段起点 + 死亡后的「记忆体」选项 +
/// 素材终态。死亡前必有至少一个时点（「与已死者对话」的第一种处理）。
pub fn build_canon_points(pack: &IngestPack) -> Vec<CanonPoint> {
    let death_day = pack
        .lifecycle
        .as_ref()
        .filter(|l| l.status == "dead")
        .map(|l| l.at_day)
        .unwrap_or(0);
    let mut out: Vec<CanonPoint> = Vec::new();
    let empty_stages: Vec<StageDraft> = Vec::new();
    let stages: &[StageDraft] = pack
        .worldline
        .as_ref()
        .map(|w| w.stages.as_slice())
        .unwrap_or(&empty_stages);

    for stage in stages.iter().filter(|s| s.include) {
        let after_death = death_day > 0 && stage.day >= death_day;
        out.push(CanonPoint {
            name: stage.id.clone(),
            day: stage.day,
            stage: Some(stage.id.clone()),
            note: if after_death {
                "此点她已死亡——需接受记忆体/残留前提".into()
            } else if stage.directive.is_empty() {
                "自该阶段起登场".into()
            } else {
                stage.directive.chars().take(40).collect()
            },
            after_death,
            premise: after_death.then(|| {
                "记忆体框架：她已不在人世，眼前的是留在世界里的记忆投影/残响——按此前提扮演".into()
            }),
        });
    }

    // 无阶段弧也要有可选项：素材开篇 + （死亡后的话）记忆体选项
    if out.is_empty() {
        out.push(CanonPoint {
            name: "素材开篇".into(),
            day: 1,
            stage: None,
            note: "素材时间轴没有可用的阶段锚点，从第 1 天开始".into(),
            after_death: false,
            premise: None,
        });
        if death_day > 0 || pack.lifecycle.as_ref().map(|l| l.status.as_str()) == Some("dead") {
            out.push(CanonPoint {
                name: "终焉之后（记忆体）".into(),
                day: death_day.max(2),
                stage: None,
                note: "素材记录她已死亡——以记忆体/残留框架扮演".into(),
                after_death: true,
                premise: Some(
                    "记忆体框架：她已不在人世，眼前的是留在世界里的记忆投影/残响——按此前提扮演"
                        .into(),
                ),
            });
        }
    }

    // 死亡日不在任何阶段边界上时补一个「死亡之后」选项（P11 特例：必须双类可选）
    if death_day > 0 && !out.iter().any(|p| p.after_death) {
        out.push(CanonPoint {
            name: "死亡之后（记忆体）".into(),
            day: death_day,
            stage: None,
            note: "此点她已死亡——需接受记忆体/残留前提".into(),
            after_death: true,
            premise: Some(
                "记忆体框架：她已不在人世，眼前的是留在世界里的记忆投影/残响——按此前提扮演".into(),
            ),
        });
    }

    out.sort_by_key(|p| p.day);
    out.dedup_by(|a, b| a.day == b.day);
    out.truncate(6);
    out
}

/// 按切入点解析秘密知情集（commit 时写入实体 secrets 的 known_by）：
/// - 揭示时点可解析且 ≤ 切入点 → 已经公开（known_by = ["*"]）；
/// - 其余（未解析/未揭示）→ 只有本人知道（known_by = [角色名]，§10.4 视角化门控）。
/// 宁可开局少一条公开事实，不可把终局反转提前泄露。
pub fn resolve_secrets_at(
    secrets: &[SecretDraft],
    stage_days: &BTreeMap<String, i64>,
    canon_day: i64,
    char_name: &str,
) -> BTreeMap<String, Secret> {
    let mut out = BTreeMap::new();
    for s in secrets.iter().filter(|s| s.include) {
        let revealed_day = s
            .revealed_by
            .as_deref()
            .and_then(|stage| stage_days.get(stage))
            .copied()
            .unwrap_or(0);
        let known_by = if revealed_day > 0 && revealed_day <= canon_day {
            vec!["*".to_string()]
        } else {
            vec![char_name.to_string()]
        };
        out.insert(
            s.key.clone(),
            Secret {
                content: s.content.clone(),
                known_by,
                revealed_by: None, // 阶段名不是 state: 路径，不写运行时声明（见模块注释）
            },
        );
    }
    out
}

/// 实体草稿 → codex 实体 JSON（commit 写 grown.json 的形态；secrets 由调用方按切入点解析注入）。
pub fn entity_value(draft: &EntityDraft, secrets: BTreeMap<String, Secret>) -> Value {
    let mut obj = serde_json::json!({
        "id": draft.id,
        "type": draft.ty,
        "name": draft.name,
        "one_liner": draft.one_liner,
        "facts": draft.facts,
    });
    let o = obj.as_object_mut().unwrap();
    if !draft.aliases.is_empty() {
        o.insert("aliases".into(), serde_json::json!(draft.aliases));
    }
    if !draft.relations.is_empty() {
        o.insert(
            "relations".into(),
            serde_json::json!(draft
                .relations
                .iter()
                .map(|r| serde_json::json!({ "to": r.to, "kind": r.kind, "always_with": r.always_with }))
                .collect::<Vec<_>>()),
        );
    }
    if !secrets.is_empty() {
        // codex schema：secrets 是 {key: {content, known_by}} 对象（CodexEntity::from_value 只认对象形态）
        let mut smap = serde_json::Map::new();
        for (k, s) in &secrets {
            let mut so = serde_json::Map::new();
            so.insert("content".into(), Value::String(s.content.clone()));
            so.insert("known_by".into(), serde_json::json!(s.known_by));
            smap.insert(k.clone(), Value::Object(so));
        }
        o.insert("secrets".into(), Value::Object(smap));
    }
    obj
}

/// lifecycle 草稿 → 实体 JSON 的 lifecycle 值；死亡/离场无生效天时给 None（宁漏勿错，
/// 立即生效的 dead 会让任何时点都不在场——见 [`assemble_pack`] 的待定项）。
pub fn lifecycle_value(lc: &LifecycleDraft) -> Option<Value> {
    if lc.status != "active" && lc.at_day <= 0 {
        return None;
    }
    if lc.status == "active" {
        return None;
    }
    Some(serde_json::json!({ "status": lc.status, "at_day": lc.at_day }))
}

/// 世界线候选 → worldline.lua 源码（stages 糖形态，M3.7 归一化适配器吃这份）。
pub fn render_worldline_lua(wl: &WorldlineDraft) -> String {
    let mut out = String::new();
    out.push_str("-- 由「素材规格化管线」生成（化境 worldline 声明 · 设计 §6.6）\n");
    out.push_str("-- stages 列表糖：when.day 是世界时钟判据；阶段钩子可后续手补 on_enter\n");
    out.push_str("return {\n");
    out.push_str(&format!("  id = {},\n", lua_str_utf8(&wl.id)));
    if !wl.premise.is_empty() {
        out.push_str(&format!("  premise = {},\n", lua_str_utf8(&wl.premise)));
    }
    out.push_str("  stages = {\n");
    for s in &wl.stages {
        out.push_str(&format!(
            "    {{ id = {}, when = {{ day = {} }}, directive = {} }},\n",
            lua_str_utf8(&s.id),
            s.day,
            lua_str_utf8(&s.directive)
        ));
    }
    out.push_str("  },\n}\n");
    out
}

/// 切入点应用报告：切入后写入的正史补丁（角色实体 JSON）与需要人工留意的告警。
pub struct CanonApplied {
    pub char_entity: Value,
    pub lifecycle: Option<Value>,
    pub warnings: Vec<QcIssue>,
}

/// 按切入点初始化切面（⑧ 的落点）：secrets known_by 解析 + lifecycle + 史变告警。
///
/// 「实体 status 与 versions 取值」的正确性由 §6.5 故事时钟保证：lifecycle.at_day 与
/// versions 的 from_day 都是绝对故事天，会话以第 N 天开局时 `facet_at`/`present_at`
/// 自然取到该时点的值。这里只补两件事：
/// - 把 secrets 解析成该时点的知情集；
/// - 对「切入点早于最早史变记录」的 facet 出告警（素材只有终值，早期取值未知——
///   会按终值注入，请确认或选更晚切入点）。
pub fn apply_canon_point(pack: &IngestPack, day: i64) -> CanonApplied {
    let stage_days = stage_day_map(pack);
    let secrets = resolve_secrets_at(&pack.secrets, &stage_days, day, &pack.entity.name);
    let mut warnings = Vec::new();

    // 史变告警：切入点早于该 facet 最早记录 → 早期取值未知
    let mut facets_seen: BTreeMap<String, i64> = BTreeMap::new();
    for v in pack.versions.iter().filter(|v| v.include) {
        let e = facets_seen.entry(v.facet.clone()).or_insert(v.day);
        if v.day < *e {
            *e = v.day;
        }
    }
    for (facet, first_day) in &facets_seen {
        if *first_day > day {
            warnings.push(QcIssue {
                severity: "warn".into(),
                at: facet.clone(),
                problem: format!(
                    "切入点（第 {day} 天）早于「{facet}」最早的史变记录（第 {first_day} 天）\
——该 facet 在此之前取值未知，将按素材终值注入；请确认或选择更晚的切入点"
                ),
            });
        }
    }

    CanonApplied {
        char_entity: entity_value(&pack.entity, secrets),
        lifecycle: pack.lifecycle.as_ref().and_then(lifecycle_value),
        warnings,
    }
}

/// 阶段名/阶段id → 故事天（来自世界线候选；秘密/史变/lifecycle 的时点锚）。
/// id 与人读名都进映射——P3 的 revealed_by 写哪个的都有。
pub fn stage_day_map(pack: &IngestPack) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    if let Some(wl) = &pack.worldline {
        for s in &wl.stages {
            out.insert(s.id.clone(), s.day);
            if !s.name.is_empty() {
                out.entry(s.name.clone()).or_insert(s.day);
            }
        }
    }
    out
}

// ---------- 确定性质检（P10 的机械子集；幻觉抽查与风格保真归人工审阅）----------

pub fn qc_pack(pack: &IngestPack) -> Vec<QcIssue> {
    let mut out = Vec::new();
    let e = &pack.entity;

    // 克制（P10 ⑤）：anchors≤4、tics≤3、示例≤6、one_liner≤40 字
    let anchors = e
        .facts
        .get("look")
        .and_then(|l| l.get("anchors"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if anchors > 4 {
        out.push(QcIssue {
            severity: "warn".into(),
            at: "look.anchors".into(),
            problem: format!("辨识点 {anchors} 条超出上限 4——宁少而精，请裁到最传神的几条"),
        });
    }
    let tics = e
        .facts
        .get("speech")
        .and_then(|s| s.get("tics"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if tics > 3 {
        out.push(QcIssue {
            severity: "warn".into(),
            at: "speech.tics".into(),
            problem: format!("口癖 {tics} 条超出建议上限 3——保留最有辨识度的"),
        });
    }
    if pack.card.example_dialogue.len() > 6 {
        out.push(QcIssue {
            severity: "warn".into(),
            at: "example_dialogue".into(),
            problem: format!("示例对话 {} 组超出上限 6", pack.card.example_dialogue.len()),
        });
    }
    if e.one_liner.chars().count() > 40 {
        out.push(QcIssue {
            severity: "warn".into(),
            at: "one_liner".into(),
            problem: format!(
                "一句话介绍 {} 字超出 40 字上限——它会以 1 行深度注入，超长挤占预算",
                e.one_liner.chars().count()
            ),
        });
    }

    // 完备性（P10 ②）：四法齐全或进待定；倾向性；机制数据过滤说明
    for (path, label) in [
        ("look", "外貌"),
        ("speech", "语言"),
        ("mannerisms", "动作"),
        ("tells", "心理外化"),
    ] {
        let missing = match e.facts.get(path) {
            None => true,
            Some(v) => is_empty_value(v),
        };
        if missing {
            out.push(QcIssue {
                severity: "info".into(),
                at: path.into(),
                problem: format!("描写四法缺「{label}」块——可在审阅后用实体补全接力"),
            });
        }
    }
    if !e.facts.contains_key("motivation") {
        out.push(QcIssue {
            severity: "info".into(),
            at: "motivation".into(),
            problem: "缺核心动机——她是为什么而动的，建议补全".into(),
        });
    }

    // 引源覆盖（P10 ③）：有引源的字段占比（info 级，提示审阅重点）
    let facet_count = e.facts.len() + 1; // + one_liner
    let sourced = e.sources.len() + usize::from(e.sources.contains_key("one_liner"));
    if facet_count > 0 && sourced * 2 < facet_count {
        out.push(QcIssue {
            severity: "info".into(),
            at: "sources".into(),
            problem: format!(
                "引源覆盖偏低（{sourced}/{facet_count}）——引不出源的字段按铁律不该存在，请重点核对"
            ),
        });
    }
    out
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WIKI_SAMPLE: &str = r#"
{{角色信息框
| 本名 = 爱莉希雅
| 别称 = 逐火十三英桀之二 · 人之律者
| 发色 = 粉色
| 瞳色 = 蓝
| 身高 = 165cm
| 所属 = 逐火十三英桀、前文明文明纪
| 个人状态 = 已死亡（终焉之战）
| 相关人士 = 凯文（战友）、梅比乌斯（同事）
}}

== 简介 ==
爱莉希雅是前文明纪的战士，逐火十三英桀的二号位。<ref>出处：官方设定集</ref>
她总是笑着说「要像花一样绽放哦♪」。

{{黑幕|实为人之律者，早在乐土建立之前就已知晓终焉}}

== 经历 ==
=== 往世乐土 ===
在乐土中迎接访客，指引他们见证逐火的记忆。

== 台词表 ==
| 初见 | 你好呀，我是爱莉希雅♪ |
| 告别 | 下次见啦，要像花一样绽放哦~ |

== 面板与圣痕 ==
攻击力 1200，圣痕套装「花绽」效果如下……
"#;

    #[test]
    fn clean_strips_markup_but_keeps_infobox_kv() {
        let cleaned = clean_source(WIKI_SAMPLE);
        assert!(cleaned.contains("本名 = 爱莉希雅"), "信息框 kv 要展开成行：{cleaned}");
        assert!(!cleaned.contains("<ref>"), "ref 要剥掉");
        assert!(!cleaned.contains("官方设定集"), "ref 内容要剥掉");
        assert!(!cleaned.contains("{{"), "模板壳要剥掉");
        assert!(cleaned.contains("要像花一样绽放哦♪"), "正文要保留");
        assert!(cleaned.contains("攻击力 1200"), "机制段清洗后仍在（由分类层过滤）");
    }

    #[test]
    fn spoilers_are_extracted_from_templates_and_html() {
        let mut spoilers = extract_spoilers(WIKI_SAMPLE);
        assert_eq!(spoilers.len(), 1, "{spoilers:?}");
        assert!(spoilers[0].contains("人之律者"));
        let html = "<p>她看起来很轻松。</p><span class=\"heimu\">其实她早就知道结局</span> 完";
        spoilers = extract_spoilers(html);
        assert_eq!(spoilers.len(), 1, "{spoilers:?}");
        assert!(spoilers[0].contains("结局"));
        assert!(extract_spoilers("没有标记的文本").is_empty());
    }

    #[test]
    fn segments_by_wiki_and_markdown_headings() {
        let sections = segment_sections(&clean_source(WIKI_SAMPLE));
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.contains(&"简介"), "{titles:?}");
        // 空父节的标题以面包屑并入子节
        assert!(titles.contains(&"经历 · 往世乐土"), "{titles:?}");
        assert!(titles.contains(&"台词表"));
        assert!(titles.contains(&"面板与圣痕"));
        // 首个标题前有内容（信息框展开行）→「开头」节
        assert_eq!(sections[0].title, "开头");
        let md = "# 开场\n正文一\n## 后续\n正文二";
        let md_sections = segment_sections(md);
        assert_eq!(md_sections.len(), 2);
        assert_eq!(md_sections[1].title, "开场 · 后续");
        let plain = segment_sections("只有一段没有标题的文本");
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].title, "全文");
    }

    #[test]
    fn infobox_maps_schema_fields_deterministically() {
        let sections = segment_sections(&clean_source(WIKI_SAMPLE));
        let head: Vec<Section> = sections[..2].to_vec(); // 开头 + 简介（信息框行在开头节）
        let info = parse_infobox(&head);
        assert_eq!(info.name.as_deref(), Some("爱莉希雅"));
        assert!(info.aliases.iter().any(|a| a.contains("人之律者")), "{:?}", info.aliases);
        assert_eq!(info.facts.get("look.发色").and_then(Value::as_str), Some("粉色"));
        assert_eq!(info.facts.get("look.身高").and_then(Value::as_str), Some("165cm"));
        assert!(info.orgs.iter().any(|o| o == "逐火十三英桀"), "{:?}", info.orgs);
        assert!(info.relations_names.contains(&"凯文".to_string()), "{:?}", info.relations_names);
        let (status, _) = lifecycle_from_status(info.status.as_deref().unwrap()).unwrap();
        assert_eq!(status, "dead");
    }

    #[test]
    fn status_words_map_to_lifecycle() {
        assert_eq!(lifecycle_from_status("已死亡（终焉之战）").unwrap().0, "dead");
        assert_eq!(lifecycle_from_status("已故").unwrap().0, "dead");
        assert_eq!(lifecycle_from_status("已从学园毕业").unwrap().0, "departed");
        assert_eq!(lifecycle_from_status("活跃中").unwrap().0, "active");
        assert!(lifecycle_from_status("  ").is_none());
    }

    #[test]
    fn quote_rows_split_occasion_and_line() {
        let sections = segment_sections(&clean_source(WIKI_SAMPLE));
        let table = sections.iter().find(|s| s.title == "台词表").unwrap();
        let rows = parse_quote_rows(&table.text);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0, "初见");
        assert!(rows[0].1.contains("♪"));
    }

    #[test]
    fn classify_parse_tolerates_fences_and_unknown_tags() {
        let raw = "```json\n[{\"id\":\"s1\",\"tag\":\"infobox\",\"reason\":\"键值对\"},{\"id\":\"s2\",\"tag\":\"王德发\",\"reason\":\"乱写\"}]\n```";
        let tags = parse_classifications(raw);
        assert_eq!(tags.get("s1").map(String::as_str), Some("infobox"));
        assert!(!tags.contains_key("s2"), "非法 tag 丢弃：{tags:?}");
        assert!(parse_classifications("模型抽风").is_empty());
    }

    fn stage_days() -> BTreeMap<String, i64> {
        BTreeMap::from([
            ("序章".into(), 1),
            ("乐土".into(), 10),
            ("终焉".into(), 30),
        ])
    }

    #[test]
    fn secrets_parse_resolves_stage_days() {
        let raw = r#"{"secrets": [
            {"key": "律者", "content": "实为人之律者", "known_by": ["爱莉希雅"], "revealed_by": "终焉",
             "source": {"section": "s2", "quote": "实为人之律者"}},
            {"key": "", "content": "无名秘密", "revealed_by": "从未揭示的阶段"}
        ], "lifecycle": {"status": "dead", "at_stage": "终焉", "note": "终焉之战"},
        "versions": [{"facet": "look.impression", "value": "光芒收敛", "at_stage": "乐土", "note": "终局前"}],
        "pending": ["存疑的一条"]}"#;
        let (secrets, lifecycle, versions, pending) = parse_secrets(raw, &stage_days());
        assert_eq!(secrets.len(), 2);
        assert_eq!(secrets[0].key, "律者");
        assert!(secrets[1].key.starts_with("secret"), "空 key 自动编号：{:?}", secrets[1].key);
        let lc = lifecycle.unwrap();
        assert_eq!(lc.status, "dead");
        assert_eq!(lc.at_day, 30, "阶段名要解析成故事天");
        assert_eq!(versions[0].day, 10);
        assert!(versions[0].include);
        assert_eq!(pending, vec!["存疑的一条"]);
        // 解析不出的阶段 → day 0、include false（宁漏勿错）
        assert!(!secrets[1].revealed_by.as_deref().map(|s| stage_days().contains_key(s)).unwrap_or(false));
    }

    #[test]
    fn four_methods_parse_collects_facets_and_sources() {
        let raw = r#"{"one_liner": "笑起来像花的战士。", "facets": {
            "look.impression": "粉色长发的战士", "look.anchors": ["发间的花"],
            "speech.tics": ["♪"], "tells": {"喜": "眼睛弯成月牙"}, "look.anchors_dup": []
        }, "sources": {"look.impression": {"section": "s2", "quote": "粉色长发"}},
        "pending": ["by_state 素材不足"]}"#;
        let four = parse_four_methods(raw);
        assert_eq!(four.one_liner, "笑起来像花的战士。");
        assert!(four.facets.contains_key("look.anchors"));
        assert!(!four.facets.contains_key("look.anchors_dup"), "空数组丢弃");
        assert_eq!(four.sources.get("look.impression").unwrap().section, "s2");
        assert_eq!(four.pending, vec!["by_state 素材不足"]);
        assert!(parse_four_methods("乱码").facets.is_empty());
    }

    #[test]
    fn events_parse_normalizes_stages_sorted_by_day() {
        let raw = r#"{"events": [
            {"title": "加入英桀", "cause": "文明将毁", "process": "她拿起了火种", "outcome": "成为二号位",
             "stage": "序章", "actors": ["爱莉希雅"], "source": {"section": "s3", "quote": "拿起了火种"}},
            {"title": "残缺事件", "cause": "", "process": "", "outcome": ""}
        ], "premise": "前文明纪，文明在终焉面前倒计时",
        "stages": [
            {"id": "finale", "name": "终焉", "when": {"day": 30}, "directive": "最后的战斗"},
            {"id": "opening", "name": "序章", "when": {"day": 1}, "directive": "日常的延续"},
            {"when": {"day": 5}, "directive": "没有 id"}
        ], "pending": []}"#;
        let out = parse_events(raw);
        assert_eq!(out.events.len(), 2, "解析层宽容收下；六要素过滤在装配层");
        assert_eq!(out.premise, "前文明纪，文明在终焉面前倒计时");
        assert_eq!(out.stages.len(), 2, "无 id 的阶段丢弃");
        assert_eq!(out.stages[0].id, "opening", "按天生排序");
        assert_eq!(out.stages[1].day, 30);
    }

    #[test]
    fn relations_and_examples_parse() {
        let rel = parse_relations(
            r#"{"relations": [{"to": "凯文（战友）", "kind": "战友", "valence": "正",
            "evidence": {"section": "s4", "quote": "凯文是值得托付的人"}}],
            "portraits": [{"who": "凯文", "line": "她比谁都温柔", "source": null}], "pending": []}"#,
        );
        assert_eq!(rel.relations[0].to, "凯文");
        assert_eq!(rel.portraits[0].line, "她比谁都温柔");

        let ex = parse_examples(
            r#"{"opening": "你好呀，我是爱莉希雅♪", "examples": [
            {"tag": "初见", "messages": [
                {"role": "user", "content": "你是谁？"},
                {"role": "char", "content": "我是爱莉希雅哦♪"}],
             "source": {"section": "s5", "quote": "初见"}},
            {"tag": "空的", "messages": []}
        ], "pending": []}"#,
        );
        assert_eq!(ex.opening.contains("爱莉希雅"), true);
        assert_eq!(ex.turns.len(), 1, "空消息组丢弃");
        assert_eq!(ex.turns[0].messages[1].role, "char");
    }

    fn sample_pack() -> IngestPack {
        let cleaned = clean_source(WIKI_SAMPLE);
        let sections = segment_sections(&cleaned);
        let infobox = parse_infobox(&sections);
        let four = FourMethods {
            one_liner: "笑起来像花的逐火战士。".into(),
            facets: BTreeMap::from([
                ("look.impression".into(), json!("粉色长发的战士")),
                ("look.anchors".into(), json!(["发间的花"])),
                ("speech.tics".into(), json!(["♪", "要像花一样绽放哦"])),
                ("tells".into(), json!({"喜": "眼睛弯成月牙"})),
                ("motivation".into(), json!("让所有人都被温柔以待")),
            ]),
            sources: BTreeMap::from([
                ("look.impression".into(), SourceRef { section: "s2".into(), quote: "粉色长发".into() }),
            ]),
            pending: vec![],
            source: None,
        };
        let psyche = FourMethods {
            facets: BTreeMap::from([
                ("motivation".into(), json!("让所有人都被温柔以待")),
                ("needs".into(), json!(["被记住", "守护笑容"])),
                (
                    "temperament".into(),
                    json!({"rise": 0.4, "decay": 0.7, "threshold": 0.6, "impulsiveness": 0.3}),
                ),
            ]),
            ..Default::default()
        };
        let events = EventsOut {
            events: vec![EventDraft {
                title: "加入英桀".into(),
                cause: "文明将毁".into(),
                process: "她拿起了火种".into(),
                outcome: "成为二号位".into(),
                stage: Some("序章".into()),
                actors: vec![],
                source: None,
                include: true,
            }],
            premise: "前文明纪，终焉倒计时".into(),
            stages: vec![
                StageDraft { id: "opening".into(), name: "序章".into(), day: 1, directive: "日常的延续".into(), include: true },
                StageDraft { id: "elysis".into(), name: "乐土".into(), day: 10, directive: "乐土的黄金时代".into(), include: true },
                StageDraft { id: "finale".into(), name: "终焉".into(), day: 30, directive: "最后的战斗".into(), include: true },
            ],
            source: None,
            pending: vec![],
        };
        let relations = RelationsOut {
            relations: vec![RelationEdge {
                to: "凯文".into(),
                kind: "战友".into(),
                valence: "正".into(),
                evidence: None,
                include: true,
            }],
            portraits: vec![Portrait { who: "凯文".into(), line: "她比谁都温柔".into(), source: None }],
            pending: vec![],
        };
        let examples = ExamplesOut {
            turns: vec![ExampleTurn {
                tag: Some("初见".into()),
                messages: vec![
                    crate::card::ExampleLine { role: "user".into(), content: "你是谁？".into() },
                    crate::card::ExampleLine { role: "char".into(), content: "我是爱莉希雅哦♪".into() },
                ],
            }],
            opening: "你好呀，我是爱莉希雅♪".into(),
            pending: vec![],
        };
        let mut spoilers = extract_spoilers(WIKI_SAMPLE);
        spoilers.push("实为人之律者".into()); // 与 P3 重复，装配应去重
        let (llm_secrets, lifecycle, versions, _pending) = parse_secrets(
            r#"{"secrets": [{"key": "律者", "content": "实为人之律者", "known_by": ["爱莉希雅"],
            "revealed_by": "finale", "source": {"section": "s2", "quote": "人之律者"}}],
            "lifecycle": {"status": "dead", "at_stage": "finale", "note": "终焉之战"},
            "versions": [{"facet": "look.impression", "value": "光芒收敛", "at_stage": "elysis"}]}"#,
            &BTreeMap::from([("finale".into(), 30), ("elysis".into(), 10)]),
        );
        let stage_days = BTreeMap::from([
            ("opening".into(), 1),
            ("elysis".into(), 10),
            ("finale".into(), 30),
            ("序章".into(), 1),
        ]);
        assemble_pack(&AssembleInputs {
            world: "hi3",
            infobox: &infobox,
            spoilers: &spoilers,
            secrets: llm_secrets,
            lifecycle,
            versions,
            four: &four,
            psyche: &psyche,
            events: &events,
            relations: &relations,
            examples: &examples,
            stage_days: &stage_days,
        })
    }

    #[test]
    fn assemble_builds_reviewable_pack_with_stubs_events_and_worldline() {
        let pack = sample_pack();
        assert_eq!(pack.entity.id, "char.爱莉希雅");
        assert_eq!(pack.entity.one_liner, "笑起来像花的逐火战士。");
        // dotted 路径折叠成嵌套（codex 的 anchors()/注入模板按嵌套读取）
        assert!(pack
            .entity
            .facts
            .get("look")
            .and_then(|l| l.get("anchors"))
            .is_some());
        assert!(pack.entity.facts.contains_key("motivation"));
        assert!(pack.entity.facts.contains_key("temperament"));
        // 关系：凯文（战友）+ 组织（所属，always_with）
        assert!(pack.entity.relations.iter().any(|r| r.to == "char.凯文" && r.kind == "战友"));
        assert!(pack
            .entity
            .relations
            .iter()
            .any(|r| r.to.starts_with("org.") && r.always_with));
        // 占位实体：凯文带他证画像
        let kevin = pack.others.iter().find(|o| o.id == "char.凯文").unwrap();
        assert!(kevin.stub);
        assert_eq!(kevin.one_liner, "她比谁都温柔");
        // 事件实体
        assert_eq!(pack.events.len(), 1);
        assert_eq!(pack.events[0].facts.get("cause").and_then(Value::as_str), Some("文明将毁"));
        // 秘密去重（剧透候选与 P3 同内容只留一条）
        assert_eq!(pack.secrets.len(), 1, "{:?}", pack.secrets.iter().map(|s| &s.content).collect::<Vec<_>>());
        // lifecycle：dead @ 30
        let lc = pack.lifecycle.as_ref().unwrap();
        assert_eq!((lc.status.as_str(), lc.at_day), ("dead", 30));
        // 世界线与卡
        assert_eq!(pack.worldline.as_ref().unwrap().stages.len(), 3);
        assert_eq!(pack.worldline.as_ref().unwrap().premise, "前文明纪，终焉倒计时");
        assert_eq!(pack.card.first_mes, "你好呀，我是爱莉希雅♪");
        assert_eq!(pack.card.example_dialogue.len(), 1);
        assert!(pack.card.personality.contains("动机"));
        // 切入点候选覆盖终态与死亡后
        assert!(pack.canon_points.iter().any(|p| p.day == 30));
    }

    #[test]
    fn canon_points_offer_pre_death_and_memory_frame_options() {
        let pack = sample_pack();
        let points = build_canon_points(&pack);
        assert!(points.iter().any(|p| !p.after_death && p.day < 30), "死亡前时点必在：{points:?}");
        let after = points.iter().find(|p| p.after_death).expect("死亡后要有记忆体选项");
        assert!(after.premise.as_deref().unwrap().contains("记忆体"));
        // 无世界线 + 已死亡：也要给出双类选项
        let mut bare = pack.clone();
        bare.worldline = None;
        bare.canon_points = Vec::new();
        let points = build_canon_points(&bare);
        assert!(points.iter().any(|p| !p.after_death));
        assert!(points.iter().any(|p| p.after_death && p.premise.is_some()));
    }

    #[test]
    fn canon_point_resolves_secret_knowledge_per_day() {
        let pack = sample_pack();
        let stage_days = stage_day_map(&pack);
        // 死亡前：秘密只有本人知道
        let before = resolve_secrets_at(&pack.secrets, &stage_days, 10, "爱莉希雅");
        assert_eq!(before.get("律者").unwrap().known_by, vec!["爱莉希雅"]);
        // 揭示后（revealed_by=finale@30，切入点 30）：公开
        let after = resolve_secrets_at(&pack.secrets, &stage_days, 30, "爱莉希雅");
        assert_eq!(after.get("律者").unwrap().known_by, vec!["*"]);
        // 无 from_day 的阶段锚（unknown 阶段名）：永远按未揭示处理
        let mut unknown = pack.secrets.clone();
        unknown[0].revealed_by = Some("不存在的阶段".into());
        let never = resolve_secrets_at(&unknown, &stage_days, 99, "爱莉希雅");
        assert_eq!(never.get("律者").unwrap().known_by, vec!["爱莉希雅"]);
    }

    #[test]
    fn apply_canon_point_bakes_secrets_and_warns_on_early_versions() {
        let mut pack = sample_pack();
        pack.versions.push(VersionDraft {
            facet: "look.impression".into(),
            value: json!("光芒收敛"),
            day: 20,
            note: None,
            source: None,
            include: true,
        });
        // 切入点 5 早于第 10 天的最早史变 → 告警
        let applied = apply_canon_point(&pack, 5);
        assert!(applied
            .warnings
            .iter()
            .any(|w| w.at == "look.impression" && w.problem.contains("早于")));
        let known_by = applied
            .char_entity
            .pointer("/secrets/律者/known_by/0")
            .and_then(Value::as_str)
            .expect("秘密进切面");
        assert_eq!(known_by, "爱莉希雅");
        // lifecycle dead@30 随包落值
        let lc = applied.lifecycle.unwrap();
        assert_eq!(lc.get("at_day").and_then(Value::as_i64), Some(30));
        // 切入点 30：秘密公开
        let applied = apply_canon_point(&pack, 30);
        let known_by = applied
            .char_entity
            .pointer("/secrets/律者/known_by/0")
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(known_by, "*");
    }

    #[test]
    fn lifecycle_value_skips_dead_without_day() {
        assert!(lifecycle_value(&LifecycleDraft {
            status: "dead".into(),
            at_day: 0,
            note: None,
            source: None,
        })
        .is_none());
        assert_eq!(
            lifecycle_value(&LifecycleDraft {
                status: "dead".into(),
                at_day: 30,
                note: None,
                source: None,
            })
            .unwrap()
            .get("at_day")
            .and_then(Value::as_i64),
            Some(30)
        );
        assert!(lifecycle_value(&LifecycleDraft {
            status: "active".into(),
            at_day: 0,
            note: None,
            source: None,
        })
        .is_none());
    }

    #[test]
    fn worldline_lua_renders_stages_sugar() {
        let pack = sample_pack();
        let lua = render_worldline_lua(pack.worldline.as_ref().unwrap());
        assert!(lua.contains("id = \"main\""));
        assert!(lua.contains("premise = \"前文明纪，终焉倒计时\""));
        assert!(lua.contains("{ id = \"opening\", when = { day = 1 }, directive = \"日常的延续\" },"));
        assert!(lua.contains("{ id = \"finale\", when = { day = 30 }"));
        // 产物要能被沙箱归一化读回（与手写 worldline.lua 同一条路）
        let shape = crate::card::worldline_shape(&lua).expect("渲染产物必须可解析");
        assert_eq!(shape.get("id").and_then(Value::as_str), Some("main"));
        assert_eq!(shape.get("premise").and_then(Value::as_str), Some("前文明纪，终焉倒计时"));
    }

    #[test]
    fn qc_flags_over_limits_and_missing_blocks() {
        let mut pack = sample_pack();
        pack.entity.one_liner = "这是一句远远超过四十个字上限的一句话介绍，故意写这么长来触发质检告警，看看它到底会不会被如实报告出来。".into();
        // anchors 在嵌套 look 对象里（与 codex schema 同构）
        let look = pack
            .entity
            .facts
            .entry("look".to_string())
            .or_insert_with(|| json!({}));
        look["anchors"] = json!(["一", "二", "三", "四", "五"]);
        pack.qc.clear();
        let issues = qc_pack(&pack);
        assert!(issues.iter().any(|i| i.at == "one_liner" && i.severity == "warn"), "{issues:?}");
        assert!(issues.iter().any(|i| i.at == "look.anchors"));
        assert!(issues.iter().any(|i| i.at == "mannerisms"), "缺的动作块要提示");
    }

    #[test]
    fn ids_and_slugs_are_lua_safe() {
        assert_eq!(entity_id("char", "爱莉希雅"), "char.爱莉希雅");
        // 空白与危险字符被清掉（id 要能当 Lua 键与文件名用）
        let id = entity_id("char", "a b|c#d");
        assert!(!id.contains(' ') && !id.contains('|') && !id.contains('#'));
        assert_eq!(entity_id("char", "  "), "char.", "空名不兜底（装配层有「未命名」兜底）");
    }

    #[test]
    fn pack_roundtrips_through_json() {
        let pack = sample_pack();
        let text = serde_json::to_string(&pack).unwrap();
        let back: IngestPack = serde_json::from_str(&text).unwrap();
        assert_eq!(back, pack, "草稿包要能前后端无损往返");
    }
}
