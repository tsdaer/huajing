//! Prompt Builder（设计 §4）：双槽位分层组装 + 预算分配。
//!
//! 槽位 A 系统头部（契约/人格/身份锚）｜槽位 B 动态锚（现状卡/directive/
//! 实体卡/召回/hook，紧邻最新用户消息）｜槽位 C 历史区（摘要/事实/窗口）。

use serde::{Deserialize, Serialize};

/// B1 故事现状卡（设计 §4.1）：黑板投影，每轮强制、无条件保底。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SceneSnapshot {
    pub story_clock: String,
    pub place: String,
    pub actors: Vec<String>,
    #[serde(default)]
    pub weather: Option<String>,
    /// 心里有事：仅在提及窗口内的活跃剧情线（设计 §8.4）
    #[serde(default)]
    pub concerns: Vec<String>,
}

impl SceneSnapshot {
    /// 紧凑单行渲染（预算 ≤80 token，六要素中"时间/地点/人物/起因"的投影）
    pub fn render(&self) -> String {
        let actors = self.actors.join(",");
        let mut s = format!("{} · {} · 在场:{}", self.story_clock, self.place, actors);
        if let Some(w) = &self.weather {
            s.push_str(&format!(" · {}", w));
        }
        if !self.concerns.is_empty() {
            s.push_str(&format!(" · 心里有事:{}", self.concerns.join("；")));
        }
        s
    }
}

// TODO(M1): prompt.rs — 分层组装器 + 预算表（设计 §4.2）+ 显式标签包裹
//   （<scene>/<world>/<memory>）；A1 表达契约与克制契约；空层省略不产生空标签。
