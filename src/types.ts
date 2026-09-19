// 与 src-tauri 各模块的 serde 结构一一对应（字段名为 Rust 侧声明的 snake_case）

/** LLM 接入点（llm.rs · 设计 §11） */
export interface Provider {
  name: string;
  base_url: string;
  api_key: string;
  model: string;
  temperature: number;
  /** chat 主对话 / util 总结捕获（便宜档） */
  role: string;
}

/** 全局配置（store.rs） */
export interface Settings {
  locale: string;
  theme: string;
  narrative_mode: string;
}

/** 用户人格（personas/*.json） */
export interface Persona {
  name: string;
  description: string;
}

/** 会话元数据（sessions/<id>/session.json · 设计 §12） */
export interface SessionMeta {
  id: string;
  created_at: string;
  characters: string[];
  persona?: string | null;
  world?: string | null;
  seed: number;
  premise?: string | null;
}

/** messages.jsonl 中的一行 */
export interface Message {
  turn: number;
  role: string;
  content: string;
  ts: number;
  scene_id?: string;
}

// ---------- 角色卡（card.rs · 设计 §3）----------

/** 示例对话中的一行（role: user | char） */
export interface ExampleLine {
  role: string;
  content: string;
}

/** 按情绪/场景分组的示例对话 */
export interface ExampleTurn {
  tag?: string | null;
  messages: ExampleLine[];
}

/** card.lua 静态字段（行为层留在 Lua 侧，不进此结构） */
export interface Card {
  spec: string;
  name: string;
  avatar?: string | null;
  creator?: string | null;
  tags: string[];
  world?: string | null;
  scenario: string;
  personality: string;
  first_mes: string;
  example_dialogue: ExampleTurn[];
}

/** 卡片目录清单条目 */
export interface CardSummary {
  dir_name: string;
  name: string;
  tags: string[];
  creator?: string | null;
  has_hooks: boolean;
  degraded: boolean;
}

/** 卡片详情（不含源码） */
export interface CardDetail {
  dir_name: string;
  card: Card;
  default_state: Record<string, unknown>;
  hook_names: string[];
  degraded: boolean;
  degrade_reason?: string | null;
}

// ---------- LLM 流式（llm.rs · 设计 §11）----------

/** OpenAI 格式对话消息 */
export interface ChatMessage {
  role: string;
  content: string;
}

/** send_message 推送的流事件 */
export type StreamEvent =
  | { event: "delta"; text: string }
  | { event: "done"; full: string; cancelled: boolean }
  | { event: "error"; message: string };

// ---------- 黑板与 Prompt 组装（store.rs / prompt.rs · 设计 §4）----------

/** 黑板 v0：时间/地点/人物 */
export interface Blackboard {
  day: number;
  clock: string;
  place: string;
  actors: string[];
}

/** 记忆检查器中的一个注入层 */
export interface PromptLayer {
  id: string;
  name: string;
  content: string;
  tokens: number;
}

/** 一轮组装的完整结果 */
export interface PromptAssembly {
  layers: PromptLayer[];
  messages: ChatMessage[];
  total_tokens: number;
}
