// Tauri 命令封装：字段/参数与 commands.rs 对齐
// （v2 默认 JS 侧 camelCase 参数名映射 Rust 侧 snake_case 形参）

import { invoke } from "@tauri-apps/api/core";
import type { Message, Persona, Provider, SessionMeta, Settings } from "./types";

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

  newSession: (opts: NewSessionOptions) => invoke<SessionMeta>("new_session", opts),
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  readMessages: (sessionId: string) => invoke<Message[]>("read_messages", { sessionId }),
};
