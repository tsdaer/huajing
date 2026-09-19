// Tauri 命令封装：字段/参数与 commands.rs 对齐
// （v2 默认 JS 侧 camelCase 参数名映射 Rust 侧 snake_case 形参）

import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  Blackboard,
  CardDetail,
  CardSummary,
  Message,
  Persona,
  PromptAssembly,
  Provider,
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
  appInfo: () => invoke<{ name: string; slogan: string; version: string; dataRoot: string }>("app_info"),

  listProviders: () => invoke<Provider[]>("list_providers"),
  /** 按名称 upsert，返回更新后的全量列表 */
  saveProvider: (provider: Provider) => invoke<Provider[]>("save_provider", { provider }),
  deleteProvider: (name: string) => invoke<Provider[]>("delete_provider", { name }),

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
};
