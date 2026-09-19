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
  InspectorData,
  InspectorProposal,
  MemRecord,
  Message,
  Persona,
  PromptAssembly,
  Provider,
  ProviderTest,
  RuntimeInfo,
  SessionMeta,
  Settings,
  StreamEvent,
} from "./types";

export type NewSessionOptions = {
  character: string;
  persona?: string;
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

  listPersonas: () => invoke<Persona[]>("list_personas"),

  listCards: () => invoke<CardSummary[]>("list_cards"),
  getCard: (dirName: string) => invoke<CardDetail>("get_card", { dirName }),

  newSession: (opts: NewSessionOptions) => invoke<SessionMeta>("new_session", opts),
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  readMessages: (sessionId: string) => invoke<Message[]>("read_messages", { sessionId }),
  /** 编辑指定下标的消息，返回更新后的全量列表 */
  editMessage: (sessionId: string, index: number, content: string) =>
    invoke<Message[]>("edit_message", { sessionId, index, content }),
  /** 删除指定下标的消息，返回更新后的全量列表 */
  deleteMessage: (sessionId: string, index: number) =>
    invoke<Message[]>("delete_message", { sessionId, index }),

  /** 发送消息并流式接收（delta/done/error 经 onEvent 推送；返回值为终态事件） */
  sendMessage: (sessionId: string, content: string, onEvent: (e: StreamEvent) => void) => {
    const channel = new Channel<StreamEvent>();
    channel.onmessage = onEvent;
    return invoke<StreamEvent>("send_message", { sessionId, content, onEvent: channel });
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

  /** 预览组装（干跑，不发送） */
  previewPrompt: (sessionId: string) => invoke<PromptAssembly>("preview_prompt", { sessionId }),
  /** 最近一次实际发送的组装 */
  lastPrompt: (sessionId: string) => invoke<PromptAssembly | null>("last_prompt", { sessionId }),

  /** 记忆检查器数据（M2.8）：一次拉全状态树 / 剧情线 / 心理 / 宫殿 / 设定集 / 收件箱 */
  inspectorData: (sessionId: string) => invoke<InspectorData>("inspector_data", { sessionId }),
  /** 设定收件箱：确认或否决一条提案，返回更新后的提案 */
  decideProposal: (sessionId: string, id: string, accept: boolean, note?: string) =>
    invoke<InspectorProposal>("decide_proposal", { sessionId, id, accept, note }),
  /** 手动触发一次总结（可能较慢；正常路径是消息滑出窗口后自动触发） */
  summarizeNow: (sessionId: string) => invoke<string>("summarize_now", { sessionId }),

  /** 角色私有 state 现状（卡内状态面板） */
  getCardState: (sessionId: string) => invoke<Record<string, unknown>>("get_card_state", { sessionId }),
  /** 卡内长期记忆写入流（palace.jsonl） */
  listCardMemory: (sessionId: string) => invoke<MemRecord[]>("list_card_memory", { sessionId }),

  /** 解析 SillyTavern 卡（PNG/JSON）为草稿，不落盘（导入向导预览） */
  previewStCard: (path: string) => invoke<CardDraft>("preview_st_card", { path }),
  /** 导入 ST 卡：生成 characters/<名字>/card.lua（同名自动 -2；overwrite 时覆盖） */
  importStCard: (path: string, overwrite = false) =>
    invoke<ImportReport>("import_st_card", { path, overwrite }),

  /** 启动 DataHub 热加载监听（M1.7；幂等） */
  watchCards: () => invoke<void>("watch_cards"),
  /** 停止热加载监听（幂等） */
  unwatchCards: () => invoke<void>("unwatch_cards"),
};
