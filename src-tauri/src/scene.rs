//! 场景与多线（M3.2 · 设计 §10.3）：「与此同时」的隔离顶层单元。
//!
//! 场景 = 同一世界里并行推进的独立舞台：各有一份黑板分区（地点/在场者/场景局部时钟/
//! 场景 flags）、各自的消息流分段（`Message.scene_id`）、各自的摘要分卷。世界层黑板
//! （时钟基准、实体作用域键）全局共享。**串台在数据结构上不可能**：给 B 场景角色组装时，
//! 只看得到 B 场景的消息与分区，A 场景发生的一切对他是「与此同时」的叙事盲区。
//!
//! 纯数据与算法：本模块不碰文件不碰网络；事件结构在 event.rs（SceneEvent），折叠与
//! 世界层/场景分区的合并规则也在 event.rs（fold），这里只放场景本体与生命周期运算。
//!
//! **向后兼容**：老会话没有场景事件，`Message.scene_id: None` 读侧归一到
//! [DEFAULT_SCENE_ID]；投影里没有场景时一切读侧路径退化为世界层黑板（M2 行为不变）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 缺省场景 id：`scene_id: None`（老会话与新会话开场）读侧归一到它
pub const DEFAULT_SCENE_ID: &str = "scene.main";

/// 场景状态：活跃（可推进）/ 冻结（被切走，不参与轮转）/ 已合并（归档留档）
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_FROZEN: &str = "frozen";
pub const STATUS_MERGED: &str = "merged";

/// 消息的场景归一：None / 空 = 缺省场景（老会话兼容，读侧单一路径）
pub fn normalize(scene_id: Option<&str>) -> &str {
    match scene_id {
        Some(s) if !s.is_empty() => s,
        _ => DEFAULT_SCENE_ID,
    }
}

/// 场景（设计 §10.3：`scene = { id, 黑板场景分区, 消息流分段, 摘要分卷, … }`）。
///
/// 黑板场景分区就存在这张表里：地点、在场者、**场景局部时钟**（被冻结的场景时间停住，
/// 切回来从原地继续）、场景 flags（`scene.*` 键；世界层 extra 里的实体键不在这里）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: String,
    /// 展示名（「图书馆东侧」）；缺省用地点
    pub title: String,
    pub place: String,
    /// 在场者（角色目录名；玩家人格不进来）。空 = 不设限（兼容手改黑板的旧会话）
    pub actors: Vec<String>,
    /// 场景局部故事时钟（天）
    pub day: i64,
    /// 场景局部故事时钟（HH:MM，可空）
    pub clock: String,
    /// 场景 flags（仅本场景成立的临时事实，如「停电」）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub flags: BTreeMap<String, serde_json::Value>,
    pub created_turn: u64,
    /// default（缺省场景落地）/ manual（界面）/ split（分场）/ merge（合场产物）
    pub origin: String,
    /// 分场来源场景 id（split 时记录；合场目标不记）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// active | frozen | merged（合并进他场的场景留档不再推进）
    pub status: String,
    pub ts: u64,
}

impl Scene {
    /// 从世界层黑板落地一个场景（建会话的缺省场景 / 老会话首次用到场景时补落地）
    pub fn from_board(id: &str, title: &str, board: &crate::store::Blackboard, turn: u64, origin: &str, ts: u64) -> Scene {
        Scene {
            id: id.to_string(),
            title: title.to_string(),
            place: board.place.clone(),
            actors: board.actors.clone(),
            day: board.day,
            clock: board.clock.clone(),
            flags: BTreeMap::new(),
            created_turn: turn,
            origin: origin.to_string(),
            parent: None,
            status: STATUS_ACTIVE.to_string(),
            ts,
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == STATUS_ACTIVE
    }

    /// 某角色是否在本场景（actors 空 = 不设限）
    pub fn has_actor(&self, dir: &str) -> bool {
        self.actors.is_empty() || self.actors.iter().any(|a| a == dir)
    }

    /// 分场：一部分角色离场另立场景（设计 §10.3「角色离场 → 分出新场景」）。
    /// 时间/flags 从原场景继承（离开那一刻是同一时刻）；地点由调用方给（新舞台）。
    /// 原场景至少要留一人——全走光就不是「分场」而是「搬家」，直接报错。
    pub fn split_from(&self, new_id: &str, title: &str, place: &str, moving: &[String], ts: u64) -> Result<(Scene, Scene), String> {
        let moved: Vec<String> = moving
            .iter()
            .filter(|a| self.actors.iter().any(|x| x == *a))
            .cloned()
            .collect();
        if moved.is_empty() {
            return Err("分场名单里没有本场景在场角色".into());
        }
        let remaining: Vec<String> = self
            .actors
            .iter()
            .filter(|a| !moving.contains(a))
            .cloned()
            .collect();
        if remaining.is_empty() {
            return Err("不能把所有角色都移出场景——至少留一人".into());
        }
        let mut next = self.clone();
        next.actors = remaining;
        let scene = Scene {
            id: new_id.to_string(),
            title: title.to_string(),
            place: place.to_string(),
            actors: moved,
            day: self.day,
            clock: self.clock.clone(),
            flags: BTreeMap::new(),
            created_turn: self.created_turn,
            origin: "split".into(),
            parent: Some(self.id.clone()),
            status: STATUS_ACTIVE.into(),
            ts,
        };
        Ok((next, scene))
    }

    /// 合场：把多路场景并进本场景（设计 §10.3「黑板分区合并」）。
    /// 在场者取并集（保序：本场景在前）；flags 冲突本场景赢；
    /// 时间取**较晚**一路（故事继续往下走，不回拨）——「对齐需确认」由 UI 先行确认。
    pub fn merge_into(&self, sources: &[Scene], ts: u64) -> Scene {
        let mut merged = self.clone();
        for src in sources {
            for a in &src.actors {
                if !merged.actors.contains(a) {
                    merged.actors.push(a.clone());
                }
            }
            for (k, v) in &src.flags {
                merged.flags.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if (src.day, src.clock.as_str()) > (merged.day, merged.clock.as_str()) {
                merged.day = src.day;
                merged.clock = src.clock.clone();
            }
        }
        merged.origin = "merge".into();
        merged.status = STATUS_ACTIVE.into();
        merged.ts = ts;
        merged
    }
}

/// 场景 id 生成：`scene.<毫秒>`（与线/提案 id 同风格；调用方也可自带语义 id）
pub fn new_scene_id(ts: u64) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(ts);
    format!("scene.{millis}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Blackboard;

    fn board(place: &str, actors: &[&str]) -> Blackboard {
        Blackboard {
            day: 1,
            clock: "20:00".into(),
            place: place.into(),
            actors: actors.iter().map(|s| s.to_string()).collect(),
            extra: Default::default(),
        }
    }

    fn scene_a() -> Scene {
        Scene::from_board(DEFAULT_SCENE_ID, "开场", &board("图书馆", &["阿澈", "小雨"]), 0, "default", 1)
    }

    #[test]
    fn normalize_maps_none_and_empty_to_default_scene() {
        assert_eq!(normalize(None), DEFAULT_SCENE_ID);
        assert_eq!(normalize(Some("")), DEFAULT_SCENE_ID);
        assert_eq!(normalize(Some("scene.x")), "scene.x");
    }

    #[test]
    fn default_scene_materializes_from_board() {
        let s = scene_a();
        assert_eq!(s.id, DEFAULT_SCENE_ID);
        assert_eq!(s.place, "图书馆");
        assert_eq!(s.actors, vec!["阿澈", "小雨"]);
        assert!(s.is_active());
        assert!(s.has_actor("阿澈"));
    }

    #[test]
    fn empty_actors_means_everyone_is_present() {
        let mut s = scene_a();
        s.actors.clear();
        assert!(s.has_actor("任何人"), "actors 空 = 不设限（旧会话手改黑板的兼容语义）");
    }

    #[test]
    fn split_moves_actors_and_keeps_time() {
        let (rest, new) = scene_a()
            .split_from("scene.b", "天台", "天台", &["阿澈".to_string()], 2)
            .unwrap();
        assert_eq!(rest.actors, vec!["小雨"], "原场景留下其余人");
        assert_eq!(new.actors, vec!["阿澈"]);
        assert_eq!(new.parent.as_deref(), Some(DEFAULT_SCENE_ID));
        assert_eq!(new.day, 1);
        assert_eq!(new.clock, "20:00", "分场那一刻是同一时刻");
        assert!(new.is_active());
    }

    #[test]
    fn split_rejects_empty_moving_and_full_evacuation() {
        let s = scene_a();
        assert!(s.split_from("scene.b", "x", "y", &["外人".to_string()], 2).is_err());
        assert!(
            s.split_from("scene.b", "x", "y", &["阿澈".to_string(), "小雨".to_string()], 2)
                .is_err(),
            "全走光不是分场"
        );
    }

    #[test]
    fn merge_unions_actors_later_time_wins_and_target_flags_win() {
        let mut a = scene_a();
        a.day = 2;
        a.clock = "21:00".into();
        a.flags.insert("停电".into(), serde_json::json!(true));
        let mut b = Scene::from_board("scene.b", "天台", &board("天台", &["阿澈"]), 0, "split", 2);
        b.day = 3;
        b.clock = "09:00".into();
        b.flags.insert("停电".into(), serde_json::json!(false));
        b.flags.insert("起雾".into(), serde_json::json!(true));

        let merged = a.merge_into(std::slice::from_ref(&b), 5);
        // 并集保序：本场景的名单在前，并入方只补差额
        assert_eq!(merged.actors, vec!["阿澈", "小雨"]);
        // 时间取较晚一路
        assert_eq!((merged.day, merged.clock.as_str()), (3, "09:00"));
        // flags 冲突本场景赢，无冲突的并入
        assert_eq!(merged.flags["停电"], serde_json::json!(true));
        assert_eq!(merged.flags["起雾"], serde_json::json!(true));
        assert!(merged.is_active());
    }

    #[test]
    fn scene_id_is_unique_enough_for_manual_use() {
        let a = new_scene_id(1000);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = new_scene_id(1000);
        assert_ne!(a, b);
        assert!(a.starts_with("scene."));
    }
}
