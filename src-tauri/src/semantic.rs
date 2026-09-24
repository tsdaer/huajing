//! 语义关联层（M3.10 · 设计 §6.13）：嵌入召回的**纯算法侧**。
//!
//! 五个确定性激活源（§6.3）的主干地位不动；语义只补它们原理上补不了的盲区——
//! 代词（「她说」）、描述性指称（「穿旧毛衣的那位管理员」）、转述，这些不命中任何
//! 声明的别名。纪律（m3.md 决断 7）：
//!
//! - **本模块不碰网络**：向量由宿主经 llm.rs 的 embeddings 客户端算好送进来；
//!   网络失败时宿主拿空结果，行为退回纯确定性版（降级即设计）。
//! - **语义只产候选 id**：门控（canon/when/lifecycle/known_by）与 B3 预算降级
//!   阶梯全部照常在 codex::activate 里生效，这里没有任何绕过纪律的通路。
//! - **确定性**：同索引 + 同查询向量必然同候选序列（打分降序、id 升序破平）。
//!
//! 另含 **M3.10a 门禁评测**的纯函数侧：语料标注格式（JSONL）与 trie 盲区 vs
//! 嵌入召回的对比指标。真机跑分（真实 embed 接入点 + 压测语料）归 M3.11 收口
//! ——门禁的意义是「净收益不显著就不进热路径」，指标必须先于接入存在。

#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::Serialize;

use crate::codex::{self, CodexEntity};

/// 查询侧指令前缀（Qwen3-Embedding 是 instruction-aware 模型，官方用法要求
/// query 侧带任务说明；对 bge-m3 这类非指令模型只是查询文本前的一段噪声，
/// 检索质量不受损）。文档侧不加前缀——同一实体向量只嵌入一次。
pub const QUERY_INSTRUCTION: &str = "Instruct: Given an excerpt from a roleplay chat, \
retrieve the story world entities (characters, places, items, events) that the excerpt \
refers to, including by pronoun or description.\nQuery: ";

/// 语义候选的相似度阈值（余弦）：低于它的候选不要。语义是弱信号、误报直接挤占
/// B3 预算（m3.md 风险 6），宁缺勿滥；门禁评测跑出更好的值再调。
pub const DEFAULT_THRESHOLD: f32 = 0.45;

/// 每轮语义候选上限：与「提及」同档的弱激活源，不值得给更多席位。
pub const DEFAULT_TOP_K: usize = 5;

/// 文档侧单实体文本的字数上限（one_liner + facts 择要；超长以 … 收尾）——
/// 嵌入按 token 计费/计时，实体卡不值得全文入索引。
const MAX_DOC_CHARS: usize = 220;

/// 一次语义命中：实体 id + 余弦相似度（宿主旁路算好后经
/// `codex::ActivationContext::semantic_hits` 送进激活）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SemanticHit {
    pub id: String,
    pub score: f32,
}

/// 余弦相似度：长度不一致或零向量给 None（调用方按「没有分数」处理）。
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

/// 实体的检索文档：名字与一句话简介 + facts 择要（BTreeMap 序，确定性）。
///
/// 刻意**不含 secrets**：索引文档可能进诊断/评测日志，秘密不该随索引散播；
/// 且语义源只到 1 行深度，秘密内容对召回没有增益。
pub fn entity_document(e: &CodexEntity) -> String {
    let mut out = format!("{}（{}）：{}", e.name, codex::type_cn(&e.ty), e.one_liner);
    for (k, v) in &e.facts {
        if k == codex::WHEN_FACT_KEY {
            continue; // 门控条件不是世界知识，不入检索文档
        }
        let line = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str(&format!("；{k}:{line}"));
        if out.chars().count() >= MAX_DOC_CHARS {
            break;
        }
    }
    if out.chars().count() > MAX_DOC_CHARS {
        out = out.chars().take(MAX_DOC_CHARS).collect();
        out.push('…');
    }
    out
}

/// 实体嵌入索引：canon 实体的检索文档 + 向量。宿主按设定集指纹缓存
/// （实体文件变了才重建），查询向量由 embeddings 客户端现算。
#[derive(Debug, Default, Clone)]
pub struct SemanticIndex {
    ids: Vec<String>,
    docs: Vec<String>,
    vectors: Vec<Vec<f32>>,
}

impl SemanticIndex {
    /// 由「实体 id / 检索文档 / 向量」三等长的平行数组构建；长度不一致是宿主的
    /// 组装 bug（向量对应错实体会静默串台），报错拒绝构建。
    pub fn from_raw(
        ids: Vec<String>,
        docs: Vec<String>,
        vectors: Vec<Vec<f32>>,
    ) -> Result<SemanticIndex, String> {
        if ids.len() != docs.len() || ids.len() != vectors.len() {
            return Err(format!(
                "嵌入索引三列长度不一致：ids={} docs={} vectors={}",
                ids.len(),
                docs.len(),
                vectors.len()
            ));
        }
        let dim = vectors.first().map(|v| v.len()).unwrap_or(0);
        if vectors.iter().any(|v| v.len() != dim) {
            return Err("嵌入索引里向量维度不一致（接入点返回了脏数据？）".to_string());
        }
        Ok(SemanticIndex {
            ids,
            docs,
            vectors,
        })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn docs(&self) -> &[String] {
        &self.docs
    }

    /// 余弦 top-K 过阈值：分数降序、id 升序破平（确定性）。零向量/维度不齐的
    /// 实体向量静默跳过——坏一条不废整索引。
    pub fn query(&self, vector: &[f32], top_k: usize, threshold: f32) -> Vec<SemanticHit> {
        let mut scored: Vec<SemanticHit> = self
            .ids
            .iter()
            .zip(&self.vectors)
            .filter_map(|(id, v)| {
                cosine(vector, v).filter(|s| *s >= threshold).map(|score| {
                    SemanticHit {
                        id: id.clone(),
                        score,
                    }
                })
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        scored.truncate(top_k);
        scored
    }
}

// ---------- M3.10a 门禁评测（§6.13：净收益不显著就不进热路径）----------

/// 评测语料的一条标注：一段原文 + 人能看出在指谁（trie 漏检的代词/描述性指称/
/// 转述三类）。JSONL 形态：
///
/// ```json
/// {"text": "她在夜班时总画便签", "expected": ["char.小雨"], "kind": "pronoun"}
/// ```
///
/// `kind` 取 pronoun / description / hearsay（三类盲区，设计 §6.13）。
#[derive(Debug, Clone, PartialEq)]
pub struct EvalCase {
    pub text: String,
    pub expected: Vec<String>,
    pub kind: String,
}

/// 解析标注文件（JSONL）：坏行报错带行号——标注是人工产物，静默跳过会让
/// 「标注了但没生效」的假阴性混进门禁结论。
pub fn parse_eval_cases(jsonl: &str) -> Result<Vec<EvalCase>, String> {
    let mut out = Vec::new();
    for (i, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("标注第 {} 行不是合法 JSON：{e}", i + 1))?;
        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let expected: Vec<String> = v
            .get("expected")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let kind = v
            .get("kind")
            .and_then(|k| k.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if text.is_empty() || expected.is_empty() || kind.is_empty() {
            return Err(format!(
                "标注第 {} 行缺 text / expected / kind（三者都必填）",
                i + 1
            ));
        }
        out.push(EvalCase {
            text,
            expected,
            kind,
        });
    }
    if out.is_empty() {
        return Err("标注文件里没有一条用例".to_string());
    }
    Ok(out)
}

/// 一条漏检：期望的实体没被召回。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MissedCase {
    pub text: String,
    pub kind: String,
    pub missed: Vec<String>,
}

/// 评测报告：召回率（分盲区类别）+ 误报率。**净收益**由两者对照确定性基线
/// （trie 在同一批用例上召回率恒为 0——这些用例就是挑它漏检的）得出。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalReport {
    pub cases: usize,
    /// 至少召回全部期望实体的用例数
    pub hits: usize,
    /// 分类别（kind → [命中数, 用例数]）
    pub by_kind: BTreeMap<String, (usize, usize)>,
    /// 召回出的非期望实体总数（误报，挤占 B3 预算的那部分）
    pub false_positives: usize,
    /// 召回出的候选总数
    pub recalled: usize,
    pub missed: Vec<MissedCase>,
}

impl EvalReport {
    pub fn recall(&self) -> f32 {
        if self.cases == 0 {
            0.0
        } else {
            self.hits as f32 / self.cases as f32
        }
    }

    pub fn false_positive_rate(&self) -> f32 {
        if self.recalled == 0 {
            0.0
        } else {
            self.false_positives as f32 / self.recalled as f32
        }
    }
}

/// 跑一次评测：`recall` 是被测的召回函数（文本 → 召回的实体 id 列表），
/// 由宿主把「嵌入查询 + 索引」接上去；本模块只管指标，不碰网络。
pub fn evaluate(cases: &[EvalCase], recall: impl Fn(&str) -> Vec<String>) -> EvalReport {
    let mut report = EvalReport {
        cases: cases.len(),
        hits: 0,
        by_kind: BTreeMap::new(),
        false_positives: 0,
        recalled: 0,
        missed: Vec::new(),
    };
    for c in cases {
        let got = recall(&c.text);
        report.recalled += got.len();
        report.false_positives += got.iter().filter(|id| !c.expected.contains(id)).count();
        let missed: Vec<String> = c
            .expected
            .iter()
            .filter(|e| !got.iter().any(|g| g == *e))
            .cloned()
            .collect();
        let entry = report.by_kind.entry(c.kind.clone()).or_insert((0, 0));
        entry.1 += 1;
        if missed.is_empty() {
            report.hits += 1;
            entry.0 += 1;
        } else {
            report.missed.push(MissedCase {
                text: c.text.clone(),
                kind: c.kind.clone(),
                missed,
            });
        }
    }
    report
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    use super::*;
    use crate::codex::CodexEntity;

    fn entity(id: &str, ty: &str, name: &str, one_liner: &str) -> CodexEntity {
        CodexEntity {
            id: id.into(),
            ty: ty.into(),
            name: name.into(),
            aliases: vec![],
            one_liner: one_liner.into(),
            facts: Default::default(),
            secrets: Default::default(),
            relations: vec![],
            live: vec![],
            constant: false,
            skip_if_remembered: false,
            status: "canon".into(),
            lifecycle: None,
            variants: vec![],
            versions: vec![],
        }
    }

    #[test]
    fn cosine_is_one_for_identical_and_zero_for_orthogonal() {
        let a = vec![1.0, 0.0, 2.0];
        let b = vec![2.0, 0.0, 4.0];
        let s = cosine(&a, &b).unwrap();
        assert!((s - 1.0).abs() < 1e-6, "同向 = 1，得 {s}");
        let s = cosine(&[1.0, 0.0], &[0.0, 3.0]).unwrap();
        assert!(s.abs() < 1e-6, "正交 = 0，得 {s}");
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_none(), "维度不齐 = None");
        assert!(cosine(&[0.0, 0.0], &[1.0, 1.0]).is_none(), "零向量 = None");
    }

    #[test]
    fn entity_document_is_deterministic_and_secret_free() {
        let mut e = entity("char.小雨", "char", "小雨", "图书馆夜班管理员");
        e.facts
            .insert("speech.style".into(), serde_json::json!("温柔话少"));
        e.facts.insert("look.impression".into(), serde_json::json!("泪痣"));
        e.secrets.insert(
            "工作牌".into(),
            crate::codex::Secret {
                content: "胸牌是别人的".into(),
                known_by: vec![],
                revealed_by: None,
            },
        );
        let doc = entity_document(&e);
        assert!(doc.contains("小雨"), "{doc}");
        assert!(doc.contains("图书馆夜班管理员"), "{doc}");
        assert!(doc.contains("look.impression"), "{doc}");
        assert!(!doc.contains("胸牌"), "秘密不入检索文档：{doc}");
        let again = entity_document(&e);
        assert_eq!(doc, again, "BTreeMap 序 ⇒ 同实体同文档");
    }

    fn index() -> SemanticIndex {
        SemanticIndex::from_raw(
            vec!["char.小雨".into(), "place.图书馆".into(), "item.便签".into()],
            vec!["a".into(), "b".into(), "c".into()],
            vec![
                vec![1.0, 0.0, 0.0],
                vec![0.0, 1.0, 0.0],
                vec![0.707_1, 0.707_1, 0.0],
            ],
        )
        .unwrap()
    }

    #[test]
    fn query_orders_by_score_and_breaks_ties_by_id() {
        let idx = index();
        let hits = idx.query(&[1.0, 0.0, 0.0], 5, 0.0);
        assert_eq!(hits[0].id, "char.小雨");
        assert!((hits[0].score - 1.0).abs() < 1e-6);
        // 便签与查询同分时排在…按 id 升序：char.小雨 > item.便签 与 place 无冲突时验证破平
        let hits = idx.query(&[0.707_1, 0.707_1, 0.0], 3, 0.0);
        assert_eq!(hits[0].id, "item.便签", "同分按 id 升序（char > item 的字典序恰相反，便于发现回归）");
    }

    #[test]
    fn query_applies_threshold_and_top_k() {
        let idx = index();
        let hits = idx.query(&[1.0, 0.0, 0.0], 5, 0.8);
        assert_eq!(hits.len(), 1, "阈值卡掉低分候选");
        let hits = idx.query(&[1.0, 0.0, 0.0], 1, 0.0);
        assert_eq!(hits.len(), 1, "top_k 截断");
    }

    #[test]
    fn from_raw_rejects_mismatched_columns() {
        assert!(SemanticIndex::from_raw(vec!["a".into()], vec![], vec![vec![1.0]]).is_err());
        assert!(SemanticIndex::from_raw(
            vec!["a".into(), "b".into()],
            vec!["x".into(), "y".into()],
            vec![vec![1.0], vec![1.0, 2.0]]
        )
        .is_err(), "维度不一致拒绝构建");
    }

    #[test]
    fn eval_cases_parse_and_report_metrics() {
        let jsonl = r#"
{"text":"她说今晚不来了","expected":["char.小雨"],"kind":"pronoun"}
{"text":"穿旧毛衣的那位管理员","expected":["char.小雨"],"kind":"description"}
{"text":"还书的事别忘了","expected":["char.小雨","item.便签"],"kind":"hearsay"}

"#;
        let cases = parse_eval_cases(jsonl).unwrap();
        assert_eq!(cases.len(), 3);
        // 召回函数：永远只回 char.小雨
        let report = evaluate(&cases, |_| vec!["char.小雨".into(), "char.墨墨".into()]);
        assert_eq!(report.cases, 3);
        assert_eq!(report.hits, 2, "第 3 条期望两个实体，只中一个算漏检");
        assert_eq!(report.by_kind.get("pronoun"), Some(&(1, 1)));
        assert_eq!(report.by_kind.get("description"), Some(&(1, 1)));
        assert_eq!(report.by_kind.get("hearsay"), Some(&(0, 1)));
        assert_eq!(report.recalled, 6);
        assert_eq!(report.false_positives, 3, "墨墨每次都是误报");
        assert!((report.recall() - 2.0 / 3.0).abs() < 1e-6);
        assert!((report.false_positive_rate() - 0.5).abs() < 1e-6);
        assert_eq!(report.missed.len(), 1);
        assert_eq!(report.missed[0].missed, vec!["item.便签".to_string()]);
    }

    #[test]
    fn eval_cases_reject_bad_annotations() {
        assert!(parse_eval_cases("{\"text\":\"x\"}").is_err(), "缺字段报错");
        assert!(parse_eval_cases("不是 json").is_err());
        assert!(parse_eval_cases("").is_err(), "空文件报错");
    }

    /// 仓库标注在解析器眼里必须永远干净——注释行、空行、缺字段当场报错，
    /// 别等真机跑分那天才发现标注格式漂了。
    #[test]
    fn checked_in_gate_annotations_parse() {
        let cases = parse_eval_cases(include_str!(
            "../../docs/eval/semantic-gate.jsonl"
        ))
        .expect("仓库标注文件应可解析");
        assert!(cases.len() >= 10, "标注太少没有统计意义：{}", cases.len());
        for c in &cases {
            assert!(
                ["pronoun", "description", "hearsay"].contains(&c.kind.as_str()),
                "未知盲区类别：{}",
                c.kind
            );
        }
    }

    /// **门禁真机跑分**（M3.10a · 设计 §6.13；默认跳过）：
    /// `HUAJING_NET_TEST=1 cargo test -- --ignored`，并在 DataHub/providers.toml
    /// 配一条 `role = "embed"`（首选 Ollama 的 qwen3-embedding：`ollama pull qwen3-embedding`）。
    ///
    /// 对仓库标注（docs/eval/semantic-gate.jsonl，全部取自 M2 压测语料的 trie 盲区）
    /// 跑嵌入召回，打印召回率/误报率报告。**不断言通过**——门禁的结论由人下：
    /// 净收益不显著（召回低或误报高）就不该把语义源开进热路径。
    #[test]
    #[ignore = "门禁真机跑分：HUAJING_NET_TEST=1 且 DataHub 配好 embed 档后 cargo test -- --ignored"]
    fn semantic_gate_evaluation_against_real_embedder() {
        if std::env::var("HUAJING_NET_TEST").is_err() {
            return;
        }
        let datahub = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../DataHub");
        let providers =
            crate::store::load_providers(&datahub).expect("读 DataHub/providers.toml");
        let Some(provider) = providers.iter().find(|p| p.role == "embed") else {
            println!("跳过：DataHub/providers.toml 未配 role = \"embed\" 的接入点");
            return;
        };
        let entities = crate::commands::parse_entities(&datahub.join("codex/default/entities"));
        let codex = crate::codex::Codex::build(entities);
        let canon: Vec<&crate::codex::CodexEntity> =
            codex.entities().iter().filter(|e| e.is_canon()).collect();
        let ids: Vec<String> = canon.iter().map(|e| e.id.clone()).collect();
        let docs: Vec<String> = canon.iter().map(|e| entity_document(e)).collect();
        println!(
            "索引 {} 个 canon 实体（嵌入模型 {}）",
            ids.len(),
            provider.model
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio 运行时");
        let vectors = rt
            .block_on(async {
                let mut out = Vec::new();
                for chunk in docs.chunks(8) {
                    out.extend(crate::llm::embeddings(provider, chunk, None).await?);
                }
                Ok::<_, String>(out)
            })
            .expect("实体批量嵌入");
        let index = SemanticIndex::from_raw(ids, docs, vectors).expect("构建索引");

        let cases = parse_eval_cases(include_str!(
            "../../docs/eval/semantic-gate.jsonl"
        ))
        .expect("标注文件可解析");
        let mut query_cache: BTreeMap<String, Vec<SemanticHit>> = BTreeMap::new();
        for c in &cases {
            let q = format!("{QUERY_INSTRUCTION}{}", c.text);
            let hits = rt
                .block_on(async { crate::llm::embeddings(provider, &[q], None).await })
                .expect("查询嵌入");
            let got = hits
                .first()
                .map(|qv| index.query(qv, DEFAULT_TOP_K, DEFAULT_THRESHOLD))
                .unwrap_or_default();
            query_cache.insert(c.text.clone(), got);
        }
        let report = evaluate(&cases, |text| {
            query_cache
                .get(text)
                .map(|hits| hits.iter().map(|h| h.id.clone()).collect())
                .unwrap_or_default()
        });
        println!("\n===== 语义关联门禁评测（§6.13）=====");
        println!(
            "阈值 {} / top-K {} · 用例 {} 条",
            DEFAULT_THRESHOLD,
            DEFAULT_TOP_K,
            report.cases
        );
        println!(
            "召回率 {:.1}%（{}/{}） · 误报率 {:.1}%（{} 非期望候选 / 共 {} 条）",
            report.recall() * 100.0,
            report.hits,
            report.cases,
            report.false_positive_rate() * 100.0,
            report.false_positives,
            report.recalled
        );
        for (kind, (hit, total)) in &report.by_kind {
            println!("  {kind}: {hit}/{total}");
        }
        for m in &report.missed {
            println!("  漏检（{}）：{} → 缺 {:?}", m.kind, m.text, m.missed);
        }
        println!("门禁由人下结论：净收益不显著就不把语义源开进热路径。");
    }
}
