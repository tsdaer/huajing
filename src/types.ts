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

/** 应用信息（commands.rs · app_info）：buildTs 为编译时刻（unix 秒，界面用它判断构建新旧） */
export interface AppInfo {
  name: string;
  slogan: string;
  version: string;
  buildTs: number;
  dataRoot: string;
}

/** 一条运行时诊断（diag.rs） */
export interface DiagRecord {
  kind: string;
  detail: string;
  ts: number;
}

/** 运行环境（commands.rs · runtime_info） */
export interface RuntimeInfo {
  dataRoot: string;
  cardCount: number;
  sessionCount: number;
  now: number;
  buildTs: number;
}

/** 接入点连通性自检结果（commands.rs · test_provider） */
export interface ProviderTest {
  ok: boolean;
  /** 结论或错误（错误含完整原因链） */
  message: string;
  /** 实际请求的 URL（base_url 写错一眼可见） */
  url: string;
  model: string;
  detail: string;
  elapsed_ms: number;
  /** 机器上配了的代理（出网失败时的第一条线索） */
  proxy: string[];
  /** 本次实际采用的代理（含来源）；null = 直连 */
  proxy_used?: string | null;
}

/** 全局配置（store.rs） */
export interface Settings {
  locale: string;
  theme: string;
  narrative_mode: string;
  /** 首启向导是否已走完（M1.9） */
  wizard_done: boolean;
  /** 出网代理；空则自动（环境变量 → Windows 系统代理 → 直连） */
  proxy?: string | null;
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

// ---------- SillyTavern 卡导入（stimport.rs · M1.8）----------

/** 从 ST 卡解析出的草稿（落盘前可预览） */
export interface CardDraft {
  name: string;
  creator?: string | null;
  tags: string[];
  world?: string | null;
  scenario: string;
  personality: string;
  first_mes: string;
  example_dialogue: ExampleTurn[];
  notes: string;
  /** 原卡规范：chara_card_v2 / v3 / 未知 */
  source_spec: string;
  warnings: string[];
}

/** 导入结果（导入向导展示） */
export interface ImportReport {
  dir_name: string;
  card_path: string;
  draft: CardDraft;
  warnings: string[];
}

// ---------- LLM 流式（llm.rs · 设计 §11）----------

/** OpenAI 格式对话消息 */
export interface ChatMessage {
  role: string;
  content: string;
}

/** `api.memory.set` / `api.blackboard.set` 的一次写入（card.rs · KvSet） */
export interface KvSet {
  key: string;
  value: unknown;
}

/** 一轮 `on_message` 钩子的执行报告（llm.rs · HookReport） */
export interface HookReport {
  turn: number;
  /** 卡片是否定义了该 hook（否 → 其余字段为空） */
  ran: boolean;
  /** 运行后的角色私有 state（面板据此显示卡内状态） */
  card_state: Record<string, unknown>;
  /** `api.memory.set` 的写入（已落 palace.jsonl） */
  memory: KvSet[];
  ui_events: { kind: string; value: string }[];
  /** 卡内错误（沙箱错误边界捕获，不打断对话） */
  logs: string[];
}

/** send_message / regenerate 推送的流事件 */
export type StreamEvent =
  | { event: "delta"; text: string }
  | { event: "done"; full: string; cancelled: boolean; report?: HookReport | null }
  | { event: "error"; message: string }
  /** 卡片经 api.ui.emit 推来的界面事件 */
  | { event: "hook_event"; kind: string; value: string };

/** 卡内长期记忆写入流中的一条（store.rs · MemRecord） */
export interface MemRecord {
  kind: string;
  key: string;
  value: unknown;
  source: string;
  turn: number;
  ts: number;
}

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
