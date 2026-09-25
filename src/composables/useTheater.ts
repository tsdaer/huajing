import { ref, type Ref } from "vue";
import { api } from "../api";
import type { TheaterView } from "../types";

/** 剧场自动轮次与聊天流的接缝：autoAdvance 驱动一轮发送，finishGeneration 驱动下一轮 */
export interface TheaterApi {
  theater: Ref<TheaterView | null>;
  refreshTheater: (sessionId: string) => Promise<void>;
  autoAdvance: () => Promise<void>;
}

export interface UseTheaterDeps {
  /** 全局错误条（ErrorToast 的数据源）；开关失败写这里 */
  error: Ref<string>;
  /** 生成互斥：生成中不开跑下一轮 */
  generating: Ref<boolean>;
  /** B6：组件卸载后 promise 链就地断链 */
  isDisposed: () => boolean;
  /** 轮与轮之间场景可能被导演切过——先对齐聚焦场景再发 */
  loadScenes: () => Promise<void>;
  /** 发送一轮（手动输入与自动轮次同一条路） */
  sendText: (content: string) => Promise<void>;
}

/** 剧场模式（M3.6 · 设计 §10.5）：自动轮次 + 导演树进度。
 *  状态与动作收在这里；轮末推进的接力在 finishGeneration（useChatStream）。 */
export function useTheater(deps: UseTheaterDeps) {
  const theater = ref<TheaterView | null>(null);
  /** 剧场推进轮的用户位提示词：固定的中性拍点，与草稿互不干扰 */
  const THEATER_CONTINUE = "（剧场继续）";

  async function refreshTheater(sessionId: string) {
    try {
      theater.value = await api.theaterView(sessionId);
    } catch {
      /* 剧场视图读取失败不阻塞聊天 */
    }
  }

  /** 开/关剧场。开启后立刻从当前轮次起自动跑（预算内） */
  async function setTheater(sessionId: string, on: boolean) {
    deps.error.value = "";
    try {
      theater.value = await api.setTheater(sessionId, on);
      if (on) void autoAdvance();
    } catch (e) {
      deps.error.value = String(e);
    }
  }

  /** 剧场自动轮次：导演调度接话。导演可能刚切场，先对齐聚焦场景再推进 */
  async function autoAdvance() {
    if (deps.isDisposed() || deps.generating.value) return;
    const t = theater.value;
    if (!t?.on || t.used >= t.budget) return;
    await deps.loadScenes();
    await deps.sendText(THEATER_CONTINUE);
  }

  return { theater, THEATER_CONTINUE, refreshTheater, setTheater, autoAdvance };
}
