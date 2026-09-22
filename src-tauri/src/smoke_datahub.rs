//! DataHub 数据资产冒烟测试（M2 真机验收的测试夹具，设计 §12）
//!
//! 仓库里的示例角色卡与设定集（`DataHub/characters`、`DataHub/codex`）是 M2
//! 真机验收的操作对象：这里用与生产一致的加载器钉住它们——卡能被沙箱加载、
//! 状态树判据可求值、实体的 variants/versions 按故事时钟解析。任何克隆都能跑。

use crate::{card, codex, commands};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn datahub() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../DataHub")
}

fn xiaoyu_source() -> String {
    std::fs::read_to_string(datahub().join("characters/小雨/card.lua")).expect("读小雨卡源码")
}

fn default_entities() -> Vec<codex::CodexEntity> {
    commands::parse_entities(&datahub().join("codex/default/entities"))
}

fn entity<'a>(list: &'a [codex::CodexEntity], id: &str) -> &'a codex::CodexEntity {
    list.iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("实体 {id} 应存在"))
}

/// 小雨卡：行为层齐活、未降级（hooks + psyche + state_tree 三层都在）
#[test]
fn xiaoyu_card_loads_with_behavior_layers() {
    let loaded = card::load_card(&datahub(), "小雨").expect("加载小雨卡");
    assert!(!loaded.degraded, "不应降级：{:?}", loaded.degrade_reason);
    assert!(loaded.hook_names.contains(&"on_load".to_string()));
    assert!(loaded.hook_names.contains(&"on_context".to_string()));
    assert!(loaded.hook_names.contains(&"on_message".to_string()));
    let psyche = loaded
        .default_state
        .get("psyche")
        .expect("state.psyche 应有种子");
    assert!(psyche.get("affects").is_some());
    assert!(psyche.get("intents").is_some_and(|v| !v.as_array().unwrap().is_empty()));

    let shape = card::state_tree_shape(&loaded.source).expect("状态树可折形");
    assert_eq!(shape["root"], "日常");
    let states = shape["states"].as_object().expect("states 表");
    for id in ["日常", "日常.夜谈", "释然"] {
        assert!(states.contains_key(id), "缺状态 {id}");
    }
    assert!(
        shape["warnings"].as_array().is_none_or(|w| w.is_empty()),
        "状态树结构不应有警告：{:?}",
        shape["warnings"]
    );
}

/// 状态树判据在真实卡源上的求值（M2.3：转移是黑板/state/揭示集/剧情线的纯函数）
#[test]
fn xiaoyu_tree_transitions_follow_blackboard_codex_and_threads() {
    let source = xiaoyu_source();
    let bb = |clock: &str| -> BTreeMap<String, serde_json::Value> {
        let mut m = BTreeMap::new();
        m.insert("clock".into(), serde_json::json!(clock));
        m
    };
    let env = |clock: &str, fav: i64| card::TreeEnv {
        event: "on_turn_end".into(),
        blackboard: bb(clock),
        state: serde_json::json!({ "favorability": fav }),
        ..Default::default()
    };

    // 日常：时钟未到 / 好感不足都不转移
    let daily = vec!["日常".to_string()];
    let decision = card::eval_state_tree(&source, &daily, &env("22:59", 50)).unwrap();
    assert!(decision.is_none(), "时钟未到不应转移：{decision:?}");
    let decision = card::eval_state_tree(&source, &daily, &env("23:10", 59)).unwrap();
    assert!(decision.is_none(), "好感不足不应转移：{decision:?}");

    // 时钟过 23:00 且好感 ≥60 → 夜谈
    let decision = card::eval_state_tree(&source, &daily, &env("23:10", 60)).unwrap();
    assert_eq!(decision.expect("应转入夜谈").to, "日常.夜谈");

    // 夜谈 → 释然：需要揭示集 + 剧情线判据同时满足（第二签名 when）
    let night = vec!["日常".to_string(), "日常.夜谈".to_string()];
    let mut night_env = env("23:40", 80);
    night_env.known = BTreeSet::from(["char.小雨.secrets.工作牌".to_string()]);
    let decision = card::eval_state_tree(&source, &night, &night_env).unwrap();
    assert!(decision.is_none(), "线未了结不应释然：{decision:?}");
    night_env.threads_resolved = BTreeSet::from(["thread.工作牌坦白".to_string()]);
    let decision = card::eval_state_tree(&source, &night, &night_env).unwrap();
    assert_eq!(decision.expect("应释然").to, "释然");

    // 夜谈 → 日常：跨夜时钟回落后回到日常
    let decision = card::eval_state_tree(&source, &night, &env("21:30", 80)).unwrap();
    assert_eq!(decision.expect("应回日常").to, "日常");
}

/// default 设定集：实体齐、schema 字段落位（anchors/secrets/live/variants/versions/relations）
#[test]
fn default_codex_assets_parse_with_time_layers() {
    let list = default_entities();
    for want in ["char.小雨", "place.图书馆", "item.便签"] {
        assert!(list.iter().any(|e| e.id == want), "缺实体 {want}");
    }

    let xiaoyu = entity(&list, "char.小雨");
    assert_eq!(xiaoyu.anchors().len(), 2, "anchors 应为已确认的两条");
    assert!(xiaoyu.secrets.contains_key("工作牌"), "工作牌秘密应存在");
    assert_eq!(xiaoyu.live, vec!["status".to_string()]);
    assert_eq!(xiaoyu.variants.len(), 1, "夜班 variants 应落位");
    assert_eq!(xiaoyu.versions.len(), 1, "第3天 versions 应落位");
    assert!(
        xiaoyu
            .relations
            .iter()
            .any(|r| r.to == "item.便签" && r.always_with),
        "便签应带 always_with 牵引"
    );

    // 三个时间层的解析（facet_at：versions → variants → facts）：
    let bb = BTreeMap::new();
    // 史变：第 2 天还是旧描述，第 3 天起是新描述
    let day2 = xiaoyu
        .facet_at("look.impression", 2, "20:00", &bb)
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let day3 = xiaoyu
        .facet_at("look.impression", 3, "20:00", &bb)
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    assert!(!day2.contains("小红绳"), "第2天应仍是旧描述：{day2}");
    assert!(day3.contains("小红绳"), "第3天应解析出新描述：{day3}");
    // 周期：夜里 23:00 命中夜班变体，白天 14:00 落回正史
    let night = xiaoyu
        .facet_at("speech.style", 1, "23:00", &bb)
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let noon = xiaoyu
        .facet_at("speech.style", 1, "14:00", &bb)
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    assert!(night.contains("夜班尾声"), "夜里应命中变体：{night}");
    assert!(!noon.contains("夜班尾声"), "白天应是正史：{noon}");

    // place.图书馆 / item.便签 的 live 与牵引边
    let library = entity(&list, "place.图书馆");
    assert_eq!(library.live, vec!["status".to_string()]);
    let note = entity(&list, "item.便签");
    assert!(
        note.relations.iter().any(|r| r.to == "char.小雨" && r.always_with),
        "便签 → 小雨 的牵引边应存在"
    );
}
