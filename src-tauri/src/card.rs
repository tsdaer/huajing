//! 角色卡（charcard/1.0，设计 §3）。
//!
//! M1 范围：静态字段加载 + 基础 hooks；Lua 运行时（mlua 沙箱：
//! 剥离 os/io/require，指令计数上限）接入后行为层生效。

use serde::{Deserialize, Serialize};

/// 示例对话中的一行（role: user | char）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleLine {
    pub role: String,
    pub content: String,
}

/// 按情绪/场景分组的示例对话（状态化 few-shot，设计 §3）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleTurn {
    #[serde(default)]
    pub tag: Option<String>,
    pub messages: Vec<ExampleLine>,
}

/// card.lua 顶层表的静态子集；行为层（state/hooks/state_tree）
/// 由 Lua 沙箱运行时承载，不进入本结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub spec: String,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub world: Option<String>,
    pub scenario: String,
    pub personality: String,
    pub first_mes: String,
    #[serde(default)]
    pub example_dialogue: Vec<ExampleTurn>,
}

// TODO(M1): card.rs — mlua 加载 card.lua（返回 table → Card + hooks 表）；
//   沙箱白名单 API（ctx/api，设计 §3.1）；指令计数与错误边界（设计 §3.2）；
//   热加载（文件变更即重载，M1 验收项）。
