// Tauri 命令封装：字段/参数与 commands.rs 对齐
// （v2 默认 JS 侧 camelCase 参数名映射 Rust 侧 snake_case 形参）

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  Blackboard,
  CardDetail,
  CardDraft,
  CardSummary,
  DiagRecord,
  ImportReport,
  CompletionResult,
  IngestCommitReport,
  IngestPack,
  IngestPrep,
  IngestSection,
  InspectorData,
  InspectorProposal,
  MemRecord,
  Message,
  OptionsEvent,
  PackPreview,
  PackImportReport,
  ExportedPack,
  ScriptSummary,
  ScriptTemplate,
  Persona,
  PromptAssembly,
  Provider,
  ProviderTest,
  ResolvePreview,
  RuntimeInfo,
  SceneView,
  SemanticCheck,
  SessionMeta,
  Settings,
  StreamEvent,
  UpdateProgress,
  UpdateStatus,
  TheaterView,
  TimelineEntry,
  WorldbookReport,
  WorldlineView,
  WorldSummary,
  InspectorEntity,
} from "./types";
import type { CustomTheme, ParsedThemeImport } from "./theme";
import type { ConsolidateProgress, ConsolidateReport } from "./types";

export type NewSessionOptions = {
  character: string;
  /** 角色阵容（M3.1 群聊）：空/缺省 = 单角色；首个是主角色（默认发言人） */
  characters?: string[];
  persona?: string;
  /** 剧本模板（M4.1）：scripts/ 下的模板名；初始起因与局面随模板生效 */
  script?: string;
  /** 启用的设定集（世界）名（M5.3）：空/缺省 = default（DataHub/codex/ 下须存在该目录） */
  world?: string;
  day?: number;
  clock?: string;
  place?: string;
  premise?: string;
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  /** 运行环境速览（设置页「运行环境」） */
  runtimeInfo: () => invoke<RuntimeInfo>("runtime_info"),
  /** 最近的运行时诊断（钩子/发送的关键决策） */
  recentDiagnostics: (limit = 60) => invoke<DiagRecord[]>("recent_diagnostics", { limit }),
  /** 前端侧诊断（拖放被忽略等）；失败静默——诊断绝不能影响主流程 */
  recordDiagnostic: (kind: string, detail: string) =>
    invoke<void>("record_diagnostic", { kind, detail }).catch(() => {}),

  listProviders: () => invoke<Provider[]>("list_providers"),
  /** 按名称 upsert，返回更新后的全量列表 */
  saveProvider: (provider: Provider) => invoke<Provider[]>("save_provider", { provider }),
  deleteProvider: (name: string) => invoke<Provider[]>("delete_provider", { name }),

  /** 接入点连通性自检（发一条最小请求，非流式） */
  testProvider: (provider: Provider) => invoke<ProviderTest>("test_provider", { provider }),

  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),

  // ---------- 签名自动更新（M4.2 · 设计 §13 · 决断 1） ----------
  /** 更新器现状：configured/disabled（设置页决定检查按钮是否置灰） */
  updaterInfo: () => invoke<UpdateStatus>("updater_info"),
  /** 检查更新（不下载）；disabled = 未启用（不是错误） */
  checkUpdate: () => invoke<UpdateStatus>("check_update"),
  /** 下载并安装：进度经 onEvent 推送；签名不符在下载后拒绝；installing 后应用退出 */
  downloadAndInstall: (onEvent: (e: UpdateProgress) => void) => {
    const channel = new Channel<UpdateProgress>();
    channel.onmessage = onEvent;
    return invoke<void>("download_and_install", { onEvent: channel });
  },

  // ---------- 主题持久化（M4.3 · 决断 7：DataHub/themes/<名>.json） ----------
  /** 保存主题（覆盖同名）；返回落盘文件 stem */
  themeSave: (name: string, theme: CustomTheme) => invoke<string>("theme_save", { name, theme }),
  /** 读主题；不存在 = null（启动加载依赖这个语义） */
  themeLoad: (name: string) => invoke<CustomTheme | null>("theme_load", { name }),
  /** 主题库清单（themes/*.json 的 stem，字典序，含活动主题 custom） */
  themeList: () => invoke<string[]>("theme_list"),
  /** 删除主题文件；返回是否真的删了 */
  themeDelete: (name: string) => invoke<boolean>("theme_delete", { name }),
  /** 主题块导入解析（CSS 块或 JSON）；键不在 allowedKeys 里拒绝并列出非法键 */
  themeParseImport: (payload: string, allowedKeys: string[]) =>
    invoke<ParsedThemeImport>("theme_parse_import", { payload, allowedKeys }),
  /** 主题 CSS 落文件（导出「保存文件」出口）：exports/<名>.theme.css，返回完整路径 */
  themeExportFile: (name: string, css: string) => invoke<string>("theme_export_file", { name, css }),

  listPersonas: () => invoke<Persona[]>("list_personas"),

  listCards: () => invoke<CardSummary[]>("list_cards"),
  getCard: (dirName: string) => invoke<CardDetail>("get_card", { dirName }),

  /** 世界浏览（M5.2）：设定集按世界分组——世界概览 + 会话无关的实体只读清单 */
  listWorlds: () => invoke<WorldSummary[]>("list_worlds"),
  codexWorldEntities: (world: string) => invoke<InspectorEntity[]>("codex_world_entities", { world }),

  newSession: (opts: NewSessionOptions) => invoke<SessionMeta>("new_session", opts),
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  readMessages: (sessionId: string) => invoke<Message[]>("read_messages", { sessionId }),
  /** 编辑指定下标的消息，返回更新后的全量列表 */
  editMessage: (sessionId: string, index: number, content: string) =>
    invoke<Message[]>("edit_message", { sessionId, index, content }),
  /** 删除指定下标的消息，返回更新后的全量列表 */
  deleteMessage: (sessionId: string, index: number) =>
    invoke<Message[]>("delete_message", { sessionId, index }),
  /** 开场白润色（体验修复批）：会话还全新时用 util 档 LLM 把 first_mes 润成自然开场，
   *  没配接入点/已开演/失败都原样返回现有消息，调用方无感 */
  polishOpening: (sessionId: string) => invoke<Message[]>("polish_opening", { sessionId }),

  /** 发送消息并流式接收（delta/done/director/error 经 onEvent 推送；返回值为终态事件）。
   *  speaker（群聊 · M3.4）：显式点名 = 只他一人接话；缺省 = 导演调度（多角色）
   *  或主角色（1v1）。多角色时每位发言人按自己的隔离视角组装 */
  sendMessage: (sessionId: string, content: string, onEvent: (e: StreamEvent) => void, speaker?: string) => {
    const channel = new Channel<StreamEvent>();
    channel.onmessage = onEvent;
    return invoke<StreamEvent>("send_message", { sessionId, content, speaker, onEvent: channel });
  },
  /** 重roll：移除末尾角色回复并重新流式生成（流事件同 sendMessage） */
  regenerate: (sessionId: string, onEvent: (e: StreamEvent) => void) => {
    const channel = new Channel<StreamEvent>();
    channel.onmessage = onEvent;
    return invoke<StreamEvent>("regenerate", { sessionId, onEvent: channel });
  },
  /** 中断生成（保留已生成的部分文本）；返回是否存在进行中的生成 */
  stopGeneration: (sessionId: string) => invoke<boolean>("stop_generation", { sessionId }),

  getBlackboard: (sessionId: string) => invoke<Blackboard>("get_blackboard", { sessionId }),
  updateBlackboard: (sessionId: string, bb: Blackboard) =>
    invoke<Blackboard>("update_blackboard", {
      sessionId,
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors,
    }),

  /** 预览组装（干跑，不发送）。speaker（M3.11）：以谁的视角看注入层，缺省 = 主角色 */
  previewPrompt: (sessionId: string, speaker?: string) =>
    invoke<PromptAssembly>("preview_prompt", { sessionId, speaker: speaker || undefined }),
  /** 最近一次实际发送的组装 */
  lastPrompt: (sessionId: string) => invoke<PromptAssembly | null>("last_prompt", { sessionId }),

  /** 记忆检查器数据（M2.8）：一次拉全状态树 / 剧情线 / 心理 / 宫殿 / 设定集 / 收件箱。
   *  character（M3.11）：以谁的视角看（状态树/心理/宫殿/揭示集按角色分道），缺省 = 主角色 */
  inspectorData: (sessionId: string, character?: string) =>
    invoke<InspectorData>("inspector_data", { sessionId, character: character || undefined }),
  /** 设定收件箱：确认或否决一条提案，返回更新后的提案 */
  decideProposal: (sessionId: string, id: string, accept: boolean, note?: string) =>
    invoke<InspectorProposal>("decide_proposal", { sessionId, id, accept, note }),
  /** 设定收件箱批量处理（M3.8）：全部确认 / 全部否决，返回处理条数 */
  decideAllProposals: (sessionId: string, accept: boolean) =>
    invoke<number>("decide_all_proposals", { sessionId, accept }),
  /** 手动补全（M3.8 · 设计 §6.8-1）：为缺失 facet 生成草稿（未落流，diff 卡片审阅用） */
  codexComplete: (sessionId: string, target: string) =>
    invoke<CompletionResult>("codex_complete", { sessionId, target }),
  /** 手动补全的接受侧（M3.8）：propose+accept 落流并物化进正史，返回写入条数 */
  codexCompleteApply: (
    sessionId: string,
    target: string,
    facets: Record<string, unknown>,
    note?: string,
  ) => invoke<number>("codex_complete_apply", { sessionId, target, facets, note }),
  /** 语义矛盾检测（M3.8 · §6.8-3 可选）：对一条待审提案跑便宜档对照判断 */
  codexSemanticCheck: (sessionId: string, id: string) =>
    invoke<SemanticCheck>("codex_semantic_check", { sessionId, id }),
  /** 手动触发一次总结（可能较慢；正常路径是消息滑出窗口后自动触发） */
  summarizeNow: (sessionId: string) => invoke<string>("summarize_now", { sessionId }),
  /** 手动触发一次睡眠整理（M4.4 · 设计 §5.4）：每组一次 util 档合并稿，进度经 onEvent 推送 */
  consolidateNow: (sessionId: string, onEvent: (e: ConsolidateProgress) => void) => {
    const channel = new Channel<ConsolidateProgress>();
    channel.onmessage = onEvent;
    return invoke<ConsolidateReport>("consolidate_now", { sessionId, onEvent: channel });
  },

  /** 手动开线（设计 §8.3）：玩家给这段关系记一笔欠账，origin=manual 不随重放丢弃 */
  openThread: (
    sessionId: string,
    title: string,
    cause: string,
    actors: string[],
    importance?: number,
  ) => invoke<unknown>("open_thread", { sessionId, title, cause, actors, importance }),
  /** 手动收线（设计 §8.3）：结果入宫殿 + 线事件落流 + 驱动一次状态树转移 */
  resolveThread: (sessionId: string, id: string, outcome: string) =>
    invoke<unknown>("resolve_thread", { sessionId, id, outcome }),
  /** 类型化事件流视图（最新在前，默认 200 条） */
  sessionTimeline: (sessionId: string, limit?: number) =>
    invoke<TimelineEntry[]>("session_timeline", { sessionId, limit }),

  // ---------- 小说模式（增强 F：互动式散文剧） ----------
  /** 小说模式开关（v1 仅 1v1 单场景可开） */
  setNovelMode: (sessionId: string, novel: boolean) =>
    invoke<boolean>("set_novel_mode", { sessionId, novel }),
  /** 某轮的段末走向选项（不传 turn = 最新一轮；None = 无选项/降级纯自由输入） */
  latestOptions: (sessionId: string, turn?: number) =>
    invoke<OptionsEvent | null>("latest_options", { sessionId, turn }),
  /** 小说化导出：事件流 → novel.md（只读导出，返回文件路径） */
  exportNovel: (sessionId: string) => invoke<string>("export_novel", { sessionId }),

  // ---------- 场景与多线（M3.2 · 设计 §10.3：「与此同时」） ----------
  /** 场景列表 + 当前聚焦场景 */
  listScenes: (sessionId: string) => invoke<SceneView>("list_scenes", { sessionId }),
  /** 新建场景（另起舞台；视角随即切过去并插入过渡插页） */
  createScene: (sessionId: string, title: string, place: string, actors: string[], note?: string) =>
    invoke<SceneView>("create_scene", { sessionId, title, place, actors, note }),
  /** 切场：被切走的场景冻结，目标场景插入小说式过渡 */
  switchScene: (sessionId: string, sceneId: string, note?: string) =>
    invoke<SceneView>("switch_scene", { sessionId, sceneId, note }),
  /** 分场：moving 里的角色离场另立新场景，视角跟过去 */
  splitScene: (
    sessionId: string,
    title: string,
    place: string,
    moving: string[],
    note?: string,
  ) => invoke<SceneView>("split_scene", { sessionId, title, place, moving, note }),
  /** 合场：from 里的场景并进聚焦场景（在场者并集、时间取较晚、flags 冲突聚焦方赢） */
  mergeScenes: (sessionId: string, from: string[], note?: string) =>
    invoke<SceneView>("merge_scenes", { sessionId, from, note }),
  /** 编辑场景分区（标题/地点/在场者/局部时钟） */
  updateScene: (
    sessionId: string,
    sceneId: string,
    patch: { title?: string; place?: string; actors?: string[]; day?: number; clock?: string },
  ) => invoke<SceneView>("update_scene", { sessionId, sceneId, ...patch }),

  /** 配置每轮发言数上限（M3.4 群聊 · 导演调度的限流旋钮；0 = 恢复缺省 2） */
  setMaxSpeakers: (sessionId: string, maxSpeakers: number) =>
    invoke<number>("set_max_speakers", { sessionId, maxSpeakers }),
  /** 即兴模式开关（M3.8 · 设计 §6.8-4，默认关） */
  setImprov: (sessionId: string, improv: boolean) =>
    invoke<boolean>("set_improv", { sessionId, improv }),

  // ---------- 剧场模式（M3.6 · 设计 §10.5：自动轮次 + 导演树起承转合 + 交叉剪辑） ----------
  /** 剧场视图（进度指示：当前阶段 / 已用与剩余轮数） */
  theaterView: (sessionId: string) => invoke<TheaterView>("theater_view", { sessionId }),
  /** 开/关剧场模式（on 时给轮数预算，缺省 20） */
  setTheater: (sessionId: string, on: boolean, budget?: number) =>
    invoke<TheaterView>("set_theater", { sessionId, on, budget }),

  // ---------- 世界主线与世界时钟（M3.7 · 设计 §6.6） ----------
  /** 世界主线视图（检查器「世界」面板：阶段 / 世界时钟 / 世界级线） */
  worldlineView: (sessionId: string) => invoke<WorldlineView>("worldline_view", { sessionId }),
  /** 手动校准世界时钟（flashback 布景 / 纠偏；只认 ≥1 的天数） */
  worldSetClock: (world: string, day: number) => invoke<number>("world_set_clock", { world, day }),
  /** 设定史变的解析预览（第 N 天的事实；day 缺省 = 会话当前故事天） */
  codexResolvePreview: (sessionId: string, day?: number) =>
    invoke<ResolvePreview>("codex_resolve_preview", { sessionId, day }),

  /** 角色私有 state 现状（卡内状态面板）。character（M3.11）：看谁的 state，缺省主角色 */
  getCardState: (sessionId: string, character?: string) =>
    invoke<Record<string, unknown>>("get_card_state", { sessionId, character: character || undefined }),
  /** 卡内长期记忆写入流（palace.jsonl） */
  listCardMemory: (sessionId: string) => invoke<MemRecord[]>("list_card_memory", { sessionId }),

  /** 解析 SillyTavern 卡（PNG/JSON）为草稿，不落盘（导入向导预览） */
  previewStCard: (path: string) => invoke<CardDraft>("preview_st_card", { path }),
  /** 导入 ST 卡：生成 characters/<名字>/card.lua（同名自动 -2；overwrite 时覆盖） */
  importStCard: (path: string, overwrite = false) =>
    invoke<ImportReport>("import_st_card", { path, overwrite }),
  /**
   * 导入统一入口的暂存步（M4.5 · 决断 6）：把文件选择器拿到的字节落到
   * DataHub/imports/（24h 过期自洁），返回路径——既有 path 版预览/导入原样可用。
   * 移动端拿不到本地路径，拖放通道不存在，这是它的替代通道（桌面双通道并存）。
   */
  stageImport: (filename: string, data: number[]) =>
    invoke<string>("stage_import", { filename, data }),

  // ---------- 包格式与导入导出（M4.1 · 设计 §13：三包 zip 往返） ----------
  /** 包预览（zip：pack.json 清单 + 文件清单 + 提醒 + 冲突标记；只读不落盘） */
  previewPack: (path: string) => invoke<PackPreview>("preview_pack", { path }),
  /** 包导入：角色进 characters/、世界进 codex/、剧本进 scripts/；overwrite 覆盖同名 */
  importPack: (path: string, overwrite = false) =>
    invoke<PackImportReport>("import_pack", { path, overwrite }),
  /** 角色包导出：卡目录整打包 → DataHub/exports/ */
  exportCardPack: (dirName: string) => invoke<ExportedPack>("export_card_pack", { dirName }),
  /** 世界包导出：codex/<世界>/ 整打包 → DataHub/exports/ */
  exportWorldPack: (world: string) => invoke<ExportedPack>("export_world_pack", { world }),
  /** 剧本包导出：从会话抽取 premise/初始黑板/导演树（不含消息历史） */
  exportScriptPack: (sessionId: string, name?: string) =>
    invoke<ExportedPack>("export_script_pack", { sessionId, name: name ?? null }),
  /** ST 世界书反向导出：canon 实体拍平（仅静态字段，§6.10） */
  exportWorldbookSt: (world: string) => invoke<ExportedPack>("export_worldbook_st", { world }),
  /** 已安装剧本清单（建会话向导选择器） */
  listScripts: () => invoke<ScriptSummary[]>("list_scripts"),
  /** 剧本模板全文（选中后预填向导） */
  getScript: (name: string) => invoke<ScriptTemplate>("get_script", { name }),

  // ---------- 素材规格化管线（M3.9 · 设计 §6.7：wiki 页十分钟成卡） ----------
  /** P0–P11 提示词套件全文（手动模式的文本源；双用途） */
  ingestPrompts: () => invoke<string>("ingest_prompts"),
  /** ①② 粘贴素材 → 清洗分段（确定性）+ 剧透候选 */
  ingestPrepare: (world: string, text: string) =>
    invoke<IngestPrep>("ingest_prepare", { world, text }),
  /** ③ 分节分类（LLM P1） */
  ingestClassify: (sections: IngestSection[]) =>
    invoke<Array<{ id: string; tag: string }>>("ingest_classify", { sections }),
  /** ④⑤ 机械映射 + 语义归纳（LLM P3–P8）→ 完整草稿包（较慢：约 6 次调用） */
  ingestExtract: (
    world: string,
    nameHint: string | null,
    sections: IngestSection[],
    tags: Array<{ id: string; tag: string }>,
    spoilers: string[],
  ) =>
    invoke<IngestPack>("ingest_extract", {
      world,
      nameHint,
      sections,
      tags,
      spoilers,
    }),
  /** ⑥⑦⑧ 审阅后的落盘：查重冲突 + 切入点切面 + 卡/正史增量/世界线三路产物 */
  ingestCommit: (
    world: string,
    pack: IngestPack,
    day: number,
    overwriteWorldline = false,
    setWorldDay = false,
  ) =>
    invoke<IngestCommitReport>("ingest_commit", {
      world,
      pack,
      day,
      overwriteWorldline,
      setWorldDay,
    }),
  /** ST 世界书导入（M2.2 欠账补课）：JSON → note 实体（粘贴或文件路径） */
  importWorldbook: (world: string, jsonText: string | null, path: string | null, bookName?: string | null) =>
    invoke<WorldbookReport>("import_worldbook", { world, jsonText, path, bookName: bookName ?? null }),

  /** 启动 DataHub 热加载监听（M1.7；幂等） */
  watchCards: () => invoke<void>("watch_cards"),
  /** 停止热加载监听（幂等） */
  unwatchCards: () => invoke<void>("unwatch_cards"),
};
