import { onScopeDispose, ref, watch, type Ref } from "vue";
import { api } from "../api";
import type { TheaterView } from "../types";

/** 剧场自动轮次与聊天流的接缝：autoAdvance 驱动一轮发送，轮末回报驱动下一轮 */
export interface TheaterApi {
  theater: Ref<TheaterView | null>;
  refreshTheater: (sessionId: string) => Promise<void>;
  autoAdvance: () => Promise<void>;
  /** 轮末回报（M4.1 加固）：用户停了生成 → 暂停自动推进，直到下一轮干净收尾或重开剧场 */
  notifyRoundEnd: (cancelled: boolean) => void;
}

export interface UseTheaterDeps {
  /** 全局错误条（ErrorToast 的数据源）；开关失败写这里，出错时不自动推进 */
  error: Ref<string>;
  /** 生成互斥：生成中不开跑下一轮 */
  generating: Ref<boolean>;
  /** B6：组件卸载后 promise 链就地断链 */
  isDisposed: () => boolean;
  /** 当前会话 id（推进前重读剧场视图，以磁盘为准） */
  sessionId: () => string;
  /** 会话不处于会被自动轮次打断的操作（消息编辑中不推进） */
  isIdle: () => boolean;
  /** 轮与轮之间场景可能被导演切过——先对齐聚焦场景再发 */
  loadScenes: () => Promise<void>;
  /** 发送一轮（手动输入与自动轮次同一条路） */
  sendText: (content: string) => Promise<void>;
}

/** 剧场模式（M3.6 · 设计 §10.5）：自动轮次 + 导演树进度。
 *
 * 推进采用**收敛式调度**（M4.1 走查加固）：每一处推进机会（开启剧场、轮末收尾、
 * 看门狗心跳）都调 autoAdvance，而 autoAdvance 总是先重读磁盘上的剧场视图、再按
 * 当下状态决定跑不跑——不依赖上一轮 promise 链的存续。旧实现是一条 hand-rolled
 * promise 链，中途任何一个守卫（世代/错误/取消/进度）判定失败就**静默断链**，
 * 进度条停在半路且无自愈，真机复现为「跑两轮就没动静」。 */
export function useTheater(deps: UseTheaterDeps) {
  const theater = ref<TheaterView | null>(null);
  /** 剧场推进轮的用户位提示词：固定的中性拍点，与草稿互不干扰 */
  const THEATER_CONTINUE = "（剧场继续）";

  /** 单飞守卫：autoAdvance 贯穿整轮（sendText 到收尾才返回），在飞时其余调用就地让路 */
  let advancing = false;
  /** 用户停了生成后的暂停位：停一次只暂停，不关剧场；下一轮干净收尾或重开即解除 */
  const pausedByStop = ref(false);

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
    if (on) pausedByStop.value = false;
    try {
      theater.value = await api.setTheater(sessionId, on);
      if (on) void autoAdvance();
    } catch (e) {
      deps.error.value = String(e);
    }
  }

  /** 尝试推进一轮。可从任何时机安全调用：守卫全做在门口，跑不跑以当下状态与
   * 磁盘上的剧场视图为准（进度/开关/预算都以重读结果为准，不用调用方的旧账）。 */
  async function autoAdvance() {
    if (advancing || deps.isDisposed() || deps.generating.value) return;
    if (pausedByStop.value || deps.error.value || !deps.isIdle()) return;
    // 廉价预检挡掉看门狗的常态空转：过期视图只会让该跑的轮次晚到（下个机会补上），不会多跑
    const cached = theater.value;
    if (!cached?.on || cached.used >= cached.budget) return;
    await refreshTheater(deps.sessionId());
    const t = theater.value;
    if (!t?.on || t.used >= t.budget) return;
    advancing = true;
    try {
      // 导演可能刚切场，先对齐聚焦场景再推进
      await deps.loadScenes();
      await deps.sendText(THEATER_CONTINUE);
    } finally {
      advancing = false;
    }
  }

  /** 轮末回报（finishGeneration 接线）：cancelled=true 只暂停不关棚 */
  function notifyRoundEnd(cancelled: boolean) {
    pausedByStop.value = cancelled;
  }

  // 看门狗（2s 心跳）：剧场开着且闲置就尝试推进。轮末接力的直接调用给 0ms 响应，
  // 这里兜住一切静默断链（旧世代/迟到守卫/挂载恢复），最多慢一个心跳。
  const WATCHDOG_MS = 2000;
  let watchdog: ReturnType<typeof setInterval> | null = null;
  watch(
    () => theater.value?.on === true,
    (on) => {
      if (on && !watchdog) {
        watchdog = setInterval(() => void autoAdvance(), WATCHDOG_MS);
      } else if (!on && watchdog) {
        clearInterval(watchdog);
        watchdog = null;
      }
    },
  );
  onScopeDispose(() => {
    if (watchdog) {
      clearInterval(watchdog);
      watchdog = null;
    }
  });

  return { theater, THEATER_CONTINUE, refreshTheater, setTheater, autoAdvance, notifyRoundEnd };
}
