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
  /** 工具调用能力位（增强 A1）："off" | "on"，缺省 off */
  tools: string;
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
  /** 运行期自动接受既有实体的小事实（M3.8 · §6.8-2 分级；缺省关 = 进收件箱人工） */
  auto_accept_minor_facts?: boolean;
  /** 每轮工具调用上限（增强 A5，缺省 6） */
  tool_calls_per_turn?: number | null;
  /** 阶段转移旁白（增强 E2）：状态树转移时生成氛围旁白（便宜档+模板兜底） */
  stage_narration?: boolean;
  /** 签名自动更新（M4.2 · §13 决断 1）：`[updater]` 区，缺省关 */
  updater?: UpdaterConfig;
}

/** `[updater]` 区（M4.2 · 决断 1）：静态清单地址 + 开关；双缺 = 未启用（UI 不报错） */
export interface UpdaterConfig {
  /** latest.json 的完整 URL（GitHub Releases 或任意静态托管，发布流程见 docs/release.md） */
  endpoint?: string | null;
  /** 缺省 false：v1 自用，端点由用户显式启用 */
  enabled?: boolean;
}

/** 检查更新的结论（check_update；state 四态见后端 commands/updater.rs） */
export interface UpdateStatus {
  /** disabled | up_to_date | available | error；updater_info 另有 configured */
  state: string;
  current_version: string;
  /** 可用的新版本号（available 时有值） */
  version?: string | null;
  /** 发布说明（清单 notes 原文） */
  notes?: string | null;
  /** 人类可读结论：成功给版本关系，失败给分类原因 */
  message: string;
}

/** 下载进度事件（download_and_install 经 Channel 推送；installing 后应用退出） */
export type UpdateProgress =
  | { event: "started"; total?: number | null }
  | { event: "progress"; downloaded: number; total?: number | null }
  | { event: "downloaded"; bytes: number }
  | { event: "installing" };

/** 睡眠整理的结论（consolidate_now · M4.4 · 设计 §5.4） */
export interface ConsolidateReport {
  /** 分到的组数 */
  groups: number;
  /** 成功落合并稿的组数 */
  merged: number;
  /** 归档的原记忆条数 */
  archived: number;
  /** 放弃的组与原因 */
  skipped: string[];
}

/** 整理进度事件（consolidate_now 经 Channel 推送：组级进度） */
export type ConsolidateProgress =
  | { event: "started"; groups: number }
  | { event: "group_done"; done: number; total: number };

/** 段末走向选项（增强 F2 · 小说模式） */
export interface StoryOption {
  label: string;
  gist: string;
}

/** 某一轮的走向选项组（Options 事件的投影） */
export interface OptionsEvent {
  turn: number;
  options: StoryOption[];
  origin: string;
  ts: number;
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
  /** 每轮发言数上限（M3.4 群聊；缺省 = 2，0 视为恢复缺省） */
  max_speakers?: number | null;
  /** 剧场模式（M3.6：自动轮次 + 导演树；null = 关闭） */
  theater?: TheaterConfig | null;
  /** 即兴模式（M3.8 · §6.8-4：薄实体现场补「设定·暂定」；缺省关） */
  improv?: boolean;
  /** 小说模式（增强 F1 · 决断 9：互动式散文剧；v1 仅 1v1 单场景可开） */
  novel_mode?: boolean;
  /** 语义源嵌入模型（M3.10 · §6.13：首次启用语义召回时记录「provider/模型」） */
  embed_model?: string | null;
}

/** 剧场模式配置（M3.6 · 设计 §10.5） */
export interface TheaterConfig {
  /** 轮数预算（目标：预算内完成完整的开线→收线弧） */
  budget: number;
  /** 开场时的最后轮次（进度 = 当前轮 − start_turn） */
  start_turn: number;
}

/** 剧场模式视图（theater_view · 进度指示与自动轮次的数据源） */
export interface TheaterView {
  on: boolean;
  budget: number;
  /** 已走掉的剧场轮数 */
  used: number;
  start_turn: number;
  last_turn: number;
  /** 导演树当前活跃路径（根→叶） */
  path: string[];
  /** 当前阶段的导演指令（这一幕该是什么调子） */
  stage_directive: string;
  /** 用的是会话自带的 director.lua（false = 内置起承转合树） */
  custom_tree: boolean;
}

/** 世界主线视图（worldline_view · M3.7 · 设计 §6.6：检查器「世界」面板的数据源） */
export interface WorldlineView {
  /** 这个世界配了 worldline.lua（false = 可选层缺席，其余字段为空档） */
  configured: boolean;
  id: string;
  /** 世界级起因（新会话三问的世界版） */
  premise: string;
  /** 当前活跃路径（根→叶） */
  path: string[];
  /** 当前阶段名 */
  stage: string;
  /** 当前阶段的 directive（B2 世界段的同一份数据） */
  stage_directive: string;
  /** B1 时代行（「公告期——公告已贴出…」） */
  era: string;
  /** 世界时钟（world.json 持久；会话轮末 max 回写） */
  world_day: number;
  /** 本会话的故事时钟 */
  session_day: number;
  /** 世界级线（scope=world；含声明未开的占位 state=declared） */
  world_threads: Array<Record<string, unknown>>;
  /** 世界时钟最近由谁推进（溯源） */
  updated_by?: string | null;
}

/** 设定史变的解析预览（codex_resolve_preview · M3.7 · 设计 §6.5：第 N 天的事实） */
export interface ResolvePreview {
  day: number;
  entities: ResolvePreviewEntity[];
}

/** 单个实体在指定故事天的解析切片（retired 留档照常列出） */
export interface ResolvePreviewEntity {
  id: string;
  name: string;
  type: string;
  /** canon | draft | retired */
  status: string;
  /** { status, at_day, in_effect, present, note } */
  lifecycle: Record<string, unknown>;
  /** [{ from_day, facet, value, note, active }] */
  versions: Array<Record<string, unknown>>;
  /** 按 day 解析出的一句话简介 */
  one_liner: string;
}

/** messages.jsonl 中的一行 */
export interface Message {
  turn: number;
  role: string;
  content: string;
  ts: number;
  scene_id?: string;
  /** 这条消息是谁说的（群聊：char 消息的角色署名；缺省 = 会话首个角色） */
  name?: string | null;
}

/** 场景（scene.rs · M3.2 · 设计 §10.3）：「与此同时」的隔离顶层单元 */
export interface Scene {
  id: string;
  title: string;
  place: string;
  /** 在场者（角色目录名；空 = 不设限） */
  actors: string[];
  /** 场景局部故事时钟（被冻结的场景停在这一刻） */
  day: number;
  clock: string;
  /** 场景 flags（仅本场景成立的临时事实） */
  flags?: Record<string, unknown>;
  created_turn: number;
  /** default | manual | split | merge */
  origin: string;
  /** 分场来源场景 id */
  parent?: string | null;
  /** active | frozen | merged（合并进他场的归档留档） */
  status: string;
  ts: number;
}

/** 场景视图（list_scenes / 切场分场合场命令的返回） */
export interface SceneView {
  scenes: Scene[];
  /** 当前聚焦场景（None = 无场景会话） */
  active?: string | null;
}

/** 场景操作向导（M3.11）的一次提交：新建/分场/合场/编辑各带各的字段 */
export type SceneSubmit =
  | { kind: "create"; title: string; place: string; actors: string[] }
  | { kind: "split"; title: string; place: string; moving: string[] }
  | { kind: "merge"; from: string }
  | {
      kind: "edit";
      sceneId: string;
      title: string;
      place: string;
      actors: string[];
      day: number;
      clock: string;
    };

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
  /** 内容指纹（重复导入识别用） */
  content_hash?: string;
}

/** 导入结果（导入向导展示） */
export interface ImportReport {
  dir_name: string;
  card_path: string;
  draft: CardDraft;
  warnings: string[];
  /** true = 与已有卡内容一致，本次复用了已有目录（没有新建） */
  reused: boolean;
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
  /** 一段增量文本；name = 生成它的角色署名（M3.4 群聊一轮多人发言） */
  | { event: "delta"; text: string; name?: string }
  | { event: "done"; full: string; cancelled: boolean; report?: HookReport | null }
  | { event: "error"; message: string }
  /** 卡片经 api.ui.emit 推来的界面事件；turn = 产生它的钩子轮次 */
  | { event: "hook_event"; kind: string; value: string; turn: number }
  /** 导演调度指示（M3.4 · §10.5）：这一轮谁接话、为何轮到她 */
  | { event: "director"; names: string[]; brief: string };

/** 类型化事件流视图的一条（session_timeline · M3.0 ④） */
export interface TimelineEntry {
  seq: number;
  kind: string;
  turn: number;
  brief: string;
}

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
  /** 逐卡激活原因（形如「小雨·人 ← 在场:图书馆 / 滞回」），只有 B3/B4 这类逐卡层才有 */
  sources?: string[];
}

/** 单层预算用量（M2.7 · 设计 §4.2：逐层可见，含实际 token） */
export interface LayerUsage {
  id: string;
  name: string;
  /** 实际占用（估算 token） */
  tokens: number;
  /** 该层所属预算组的 token 上限（A1/A2/A3 共享 A 组，B5 的内心与 hook 共享） */
  limit: number;
  /** 本层被裁/被截断的中文说明；没被动过就没有这个字段 */
  trimmed?: string;
}

/** 一轮组装的预算总账（M2.7） */
export interface BudgetReport {
  /** 输入预算 = 模型上下文 × 75% */
  input_tokens: number;
  /** 各层实际之和 */
  used_tokens: number;
  layers: LayerUsage[];
}

/** 一轮组装的完整结果 */
export interface PromptAssembly {
  layers: PromptLayer[];
  messages: ChatMessage[];
  total_tokens: number;
  /** 预算总账（M2.7；老后端不带这个字段） */
  budget?: BudgetReport;
}

// ---------- 记忆检查器（M2.8 · commands.rs · inspector_data）----------

/** 状态树视图：活跃路径 + 当前 directive + recall/reveal + 校验告警 */
export interface InspectorStateTree {
  root: string;
  /** 活跃路径（根→叶） */
  path: string[];
  /** 根→叶拼接后的导演指令（没写 directive 的状态跳过） */
  directive: string;
  /** 当前状态声明的召回提示（“回到事发地点才想起那件事”） */
  recall: string[];
  /** 当前状态声明的揭示集 */
  reveal: string[];
  /** 树上声明过的全部状态 id */
  states: string[];
  /** 树的校验告警（坏父链 / 环 / 重复 id 等） */
  warnings: string[];
}

/** 一次状态转移（event.rs · TransitionEvent） */
export interface InspectorTransition {
  turn: number;
  from: string[];
  to: string[];
  reason: string;
  ts: number;
}

/** 剧情线的一个经过节点 */
export interface ThreadProgressNode {
  turn: number;
  note: string;
  memory?: string | null;
}

/** 提及时机（设计 §8.4：克制梯度 × 可提及窗口 × 冷却 × framing） */
export interface ThreadResurface {
  /** dormant | natural | eager */
  grade: string;
  /** 窗口条件（宿主确定性求值，界面只做只读展示） */
  windows: unknown[];
  deadline?: { day: number; escalate: string } | null;
  cooldown: number;
  framing: string;
  last_mentioned_turn?: number | null;
}

/** 一条剧情线（threads.rs · Thread::to_value） */
export interface InspectorThread {
  id: string;
  title: string;
  /** 起因 */
  cause: string;
  actors: string[];
  /** 重要度 0–1 */
  importance: number;
  opened: { turn: number; story_day: number; story_clock: string };
  /** active | resolved | abandoned */
  state: string;
  progress: ThreadProgressNode[];
  resurface: ThreadResurface;
  resolution?: {
    turn: number;
    story_day: number;
    story_clock: string;
    outcome: string;
    memory?: string | null;
  } | null;
  scope: string;
  linked_intent?: string | null;
}

/** 此刻落在可提及窗口内的一条线（B1「心里有事」的候选，带激活原因） */
export interface InspectorResurfacePick {
  id: string;
  title: string;
  grade: string;
  framing: string;
  /** 激活原因（如「黑板:day≥5+在场:小雨」） */
  reason: string;
}

/** 剧情线视图（M2.4 · 设计 §8） */
export interface InspectorThreads {
  active: InspectorThread[];
  resolved: InspectorThread[];
  abandoned: InspectorThread[];
  /** C1 未决事项：全部活跃线的只读投影（仅标题与状态） */
  pending: string[];
  inWindow: InspectorResurfacePick[];
  eventCount: number;
}

/** 情绪槽的一条历史采样（psyche.rs · AffectTick） */
export interface AffectTick {
  turn: number;
  intensity: number;
}

/** 一个情绪槽（强度 + 来源 + 起始轮 + 历史采样） */
export interface PsycheAffect {
  name: string;
  intensity: number;
  source: string;
  since_turn: number;
  history: AffectTick[];
}

/** 主动行为的触发记录（M3.5 · psyche.rs · TriggerRecord）：可溯源到意图 */
export interface PsycheTrigger {
  turn: number;
  /** 触发时的生效阈值（threshold − 冲动性加成 − 余量） */
  threshold: number;
  /** 触发时的意图强度 */
  strength: number;
  /** 触发了什么 */
  action: string;
}

/** 一条意图（意志的内隐形态；说出口后外化为剧情线） */
export interface PsycheIntent {
  name: string;
  strength: number;
  linked_thread?: string | null;
  since_turn: number;
  /** 主动行为触发记录（M3.5）：None = 还没触发过 */
  triggered?: PsycheTrigger | null;
}

/** 一条憋着没说的心里话（M3.5 · psyche.rs · ScheduledSay） */
export interface PsycheScheduledSay {
  text: string;
  /** 憋下的轮次（0 = 未知/旧形态） */
  turn: number;
}

/** 心理运行时视图（M2.5 · 设计 §9） */
export interface InspectorPsyche {
  /** B5 注入用的「内心」一行摘要 */
  summary: string;
  affects: PsycheAffect[];
  intents: PsycheIntent[];
  /** 心里话队列（M3.5）：憋着没说出口的话，下一轮主动说 */
  scheduled: PsycheScheduledSay[];
  /** 衰减轨迹：[情绪名, 采样…]（界面只画小条，不引图表库） */
  trail: [string, AffectTick[]][];
  /** 自动表情（情绪 → 差分表命中） */
  auto_emotion?: string | null;
}

/** 记忆对象摘要（palace.rs · MemBrief；每条都能溯源到轮次） */
export interface InspectorMemory {
  id: string;
  content: string;
  turn: number;
  story_day: number;
  story_clock: string;
  /** 显著度 0–1 */
  salience: number;
  emotion?: string | null;
  place?: string | null;
  source: string;
  /** 睡眠整理归档标记（M4.4）：true = 已被合并稿替代（面板「已归档」过滤） */
  archived?: boolean;
}

/** 记忆宫殿三视图 + 最近记忆（设计 §5.5） */
export interface InspectorPalace {
  count: number;
  /** 睡眠整理归档的记忆总数（M4.4；全量口径，不受 recent 窗口限制） */
  archivedCount?: number;
  rooms: { place: string; count: number; top: InspectorMemory[] }[];
  timeline: { label: string; count: number; top: InspectorMemory[] }[];
  /** 节点是 link 标签，边是共现 [a, b, 次数] */
  graph: { nodes: string[]; edges: [string, string, number][] };
  recent: InspectorMemory[];
}

/** 设定集实体清单条目（codex.rs · 设计 §6） */
export interface InspectorEntity {
  id: string;
  name: string;
  /** char | place | item | event | org | rule | concept | note */
  type: string;
  /** draft | canon | retired */
  status: string;
  oneLiner: string;
  /** 恒注入的辨识锚点（跨 200 轮不漂移） */
  anchors: string[];
  /** 缺失的模板 facet 路径（M3.8：编辑器高亮 +「补全」按钮） */
  missing: string[];
}

/** 设定集视图 */
export interface InspectorCodex {
  world: string;
  count: number;
  entities: InspectorEntity[];
}

/** 世界概览（M5.2：资产页设定集页签的世界卡片数据源，commands.rs · WorldSummary） */
export interface WorldSummary {
  name: string;
  entities: number;
  /** 类型前缀 → 数量（char/place/item/...） */
  by_type: Record<string, number>;
  /** 世界时钟当前天数（world.json；缺省 1） */
  day: number;
  /** 有没有世界主线（worldline.lua） */
  has_worldline: boolean;
}

/** 设定收件箱的一条提案（propose 落条目，accept/reject 改状态） */
export interface InspectorProposal {
  id: string;
  /** propose | accept | reject */
  status: string;
  /** new_entity | new_fact | fact_change | relation | transient | episode | thread | psyche */
  kind: string;
  turn: number;
  /** pipeline | complete | improv | manual（M3.8 起投影带出） */
  origin?: string;
  payload?: unknown;
  note?: string | null;
  /** 冲突双源呈现的「正史现值」一侧（M3.8：new_fact/fact_change 才有） */
  currentValue?: { facet: string; value: unknown; source: string } | null;
}

/** 手动补全的一个 facet 草稿（complete.rs 校验结论随条目给出） */
export interface CompletionItem {
  facet: string;
  value: unknown;
  /** 非空 = 该条被确定性校验驳回（anchors 等），不可接受 */
  rejected?: string | null;
  /** 非空 = 警告级冲突（双源呈现，人工裁决） */
  warn?: { detail: string; current?: unknown } | null;
}

/** 语义矛盾检测结果（codex_semantic_check · §6.8-3 可选 LLM 检测） */
export interface SemanticCheck {
  id: string;
  contradictions: string[];
  provider: string;
}

/** 手动补全的生成结果（codex_complete 命令返回；未落流的草稿） */
export interface CompletionResult {
  target: string;
  items: CompletionItem[];
  note: string;
  provider: string;
}

/** 记忆检查器的全量投影（M2.8 面板一次拉全） */
export interface InspectorData {
  session: string;
  character: string;
  stateTree: InspectorStateTree | null;
  transitions: InspectorTransition[];
  threads: InspectorThreads;
  psyche: InspectorPsyche;
  palace: InspectorPalace;
  codex: InspectorCodex;
  summary: string;
  proposals: InspectorProposal[];
  /** 秘密揭示集（“实体.秘密” 路径） */
  known: string[];
  blackboard: Blackboard;
  /** 上一轮在场 / 激活的实体 id */
  activeEntities: string[];
}


// ---------- 素材规格化管线（M3.9 · 设计 §6.7：wiki 页十分钟成卡） ----------

/** 清洗分段后的一个小节（id 是全管线引源跳转的锚点） */
export interface IngestSection {
  id: string;
  title: string;
  text: string;
}

/** ①② 导入与清洗分段的产物（确定性） */
export interface IngestPrep {
  world: string;
  sections: IngestSection[];
  /** 机械剧透候选（秘密的前身，审阅可见） */
  spoilers: string[];
}

/** 逐条引源（审阅可跳转：小节 id + 原文关键句） */
export interface IngestSource {
  section: string;
  quote: string;
}

/** 实体草稿（角色本体 / 占位 / 事件共用） */
export interface IngestEntityDraft {
  id: string;
  type: string;
  name: string;
  aliases: string[];
  one_liner: string;
  /** facts（嵌套对象） */
  facts: Record<string, unknown>;
  relations: Array<{ to: string; kind: string; always_with: boolean }>;
  sources: Record<string, IngestSource>;
  include: boolean;
  /** 占位实体（关系目标/组织）：只有名字，交给补全管线接力 */
  stub: boolean;
}

/** 秘密候选（known_by 在 commit 时按切入点解析） */
export interface IngestSecretDraft {
  key: string;
  content: string;
  revealed_by: string | null;
  known_by_advice: string[];
  source: IngestSource | null;
  include: boolean;
  /** spoiler（机械剧透）| llm（P3 归纳） */
  origin: string;
}

/** 史变候选（素材暗示「同一事实随剧情变化」） */
export interface IngestVersionDraft {
  facet: string;
  value: unknown;
  day: number;
  note: string | null;
  source: IngestSource | null;
  include: boolean;
}

/** 世界线阶段候选 */
export interface IngestStageDraft {
  id: string;
  name: string;
  day: number;
  directive: string;
  include: boolean;
}

/** ⑧ 切入点候选（死亡后的点带记忆体前提） */
export interface IngestCanonPoint {
  name: string;
  day: number;
  stage: string | null;
  note: string;
  after_death: boolean;
  premise: string | null;
}

/** 待定项（宁漏勿错的落点） */
export interface IngestPendingItem {
  title: string;
  detail: string;
  source: IngestSource | null;
}

/** 确定性质检结论（P10 机械子集；severity: warn | info） */
export interface IngestQcIssue {
  severity: string;
  at: string;
  problem: string;
}

/** ⑦ 草稿包（审阅的前后端往返对象；commit 只认提交上来的这一份） */
export interface IngestPack {
  world: string;
  char_id: string;
  card: {
    first_mes: string;
    scenario: string;
    personality: string;
    tags: string[];
    example_dialogue: Array<{ tag: string | null; messages: Array<{ role: string; content: string }> }>;
    sources: Record<string, IngestSource>;
  };
  entity: IngestEntityDraft;
  others: IngestEntityDraft[];
  events: IngestEntityDraft[];
  secrets: IngestSecretDraft[];
  lifecycle: { status: string; at_day: number; note: string | null; source: IngestSource | null } | null;
  versions: IngestVersionDraft[];
  worldline: { id: string; premise: string; stages: IngestStageDraft[] } | null;
  canon_points: IngestCanonPoint[];
  pending: IngestPendingItem[];
  qc: IngestQcIssue[];
}

/** commit 的落盘报告 */
export interface IngestCommitReport {
  world: string;
  cardDir: string;
  cardPath: string;
  entitiesWritten: string[];
  worldlineWritten: boolean;
  canonDay: number;
  skipped: string[];
  warnings: string[];
}

/** ST 世界书导入报告（JSON → note 实体） */
export interface WorldbookReport {
  world: string;
  imported: number;
  disabled: number;
  skipped: number;
  files: string[];
  warnings: string[];
}

// ---------- 包格式与导入导出（M4.1 · 设计 §13：三包 zip 往返） ----------

/** pack.json 清单（huajing-pack/1） */
export interface PackManifest {
  spec: string;
  kind: "character" | "world" | "script";
  name: string;
  creator?: string | null;
  description?: string | null;
  requires?: { world?: string } | null;
}

/** 包内文件（预览清单用） */
export interface PackFileInfo {
  name: string;
  size: number;
}

/** 包预览（导入弹窗：清单 + 文件 + 提醒 + 冲突标记） */
export interface PackPreview {
  manifest: PackManifest;
  files: PackFileInfo[];
  warnings: string[];
  /** 目标位置已有同名安装物（默认并存 -2，可勾选覆盖） */
  conflict: boolean;
}

/** 包导入报告 */
export interface PackImportReport {
  kind: string;
  name: string;
  /** 落盘位置（相对 DataHub，如 characters/月见） */
  target: string;
  files: number;
  overwritten: boolean;
  warnings: string[];
}

/** 导出产物（zip 或 ST 世界书 JSON） */
export interface ExportedPack {
  kind: string;
  name: string;
  path: string;
}

/** 剧本模板清单条目（建会话向导选择器用） */
export interface ScriptSummary {
  name: string;
  premise: string;
  has_director: boolean;
}

/** 剧本模板全文（选中后预填向导） */
export interface ScriptTemplate {
  name: string;
  premise: string;
  blackboard: import("./types").Blackboard | null;
  has_director: boolean;
}
