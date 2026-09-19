// 会话列表与当前选中项：侧栏可折叠子菜单与会话页共用同一份状态，
// 因此在侧栏点一场会话，右侧会话页会立刻跟着切换。

import { computed, ref } from "vue";
import { api } from "./api";
import type { SessionMeta } from "./types";

export const sessions = ref<SessionMeta[]>([]);
export const selectedId = ref("");
export const loadingSessions = ref(false);

export const selectedSession = computed(
  () => sessions.value.find((s) => s.id === selectedId.value) ?? null,
);

/** 读取会话列表；已选中的会话若已不存在则清空选中 */
export async function loadSessions(): Promise<void> {
  loadingSessions.value = true;
  try {
    sessions.value = await api.listSessions();
    if (!sessions.value.some((s) => s.id === selectedId.value)) selectedId.value = "";
  } catch {
    sessions.value = [];
    selectedId.value = "";
  } finally {
    loadingSessions.value = false;
  }
}

export function selectSession(id: string): void {
  selectedId.value = id;
}

/** 「新建会话」弹窗开关：顶栏按钮负责开，会话页负责承载弹窗 */
export const newSessionOpen = ref(false);

export function openNewSession(): void {
  newSessionOpen.value = true;
}

export function closeNewSession(): void {
  newSessionOpen.value = false;
}
