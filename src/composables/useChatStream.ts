import { nextTick, ref, type Ref } from "vue";
import { api } from "../api";
import type { Blackboard, HookReport, MemRecord, Message, StreamEvent } from "../types";
import type { TheaterApi } from "./useTheater";

/** 聊天流之外的会话状态接线（SessionView 持有，这里只读写） */
export interface UseChatStreamDeps {
  /** props.meta.id 的响应式取值（B5 守卫用它比对「流开始时的会话」） */
  sessionId: () => string;
  error: Ref<string>;
  generating: Ref<boolean>;
  messages: Ref<Message[]>;
  blackboard: Ref<Blackboard | null>;
  /** 黑板编辑表单：收尾重读黑板后回填（reactive 对象，直接 Object.assign） */
  bbForm: { day: number; clock: string; place: string; actors: string };
  editingIndex: Ref<number>;
  /** 乐观上屏的用户消息带场景归属（切着场景时不出戏） */
  activeScene: Ref<string>;
  /** "" = 导演调度；否则点名该角色 */
  speaker: Ref<string>;
  /** 卡内状态按检查器视角读（M3.11） */
  inspView: Ref<string>;
  cardName: Ref<string>;
  streamEl: Ref<HTMLElement | null>;
  composerEl: Ref<HTMLTextAreaElement | null>;
  jumpToLastPage: () => Promise<void> | void;
  refreshInspector: () => Promise<void> | void;
  theaterApi: TheaterApi;
  /** 轮末收尾里「面板相关」的刷新（检查器开着就重拉、事件流页签即时可见），SessionView 接线 */
  onRoundEnd: () => Promise<void> | void;
}

/** 聊天流（M1.5 + M3.4 群聊）：发送/重roll/停止、流式打字机、钩子报告落地、
 *  卡内状态与界面事件。B5 世代守卫在这里落：每次开流自增世代号，
 *  增量与收尾回调校验会话 id + 世代号，旧流一律丢弃。 */
export function useChatStream(deps: UseChatStreamDeps) {
  /** 流式中的各位发言人（M3.4 群聊）：一位一个气泡，按发言顺序排列 */
  const streams = ref<{ name: string; text: string }[]>([]);
  /** 导演调度指示（这轮谁接话、为何轮到她；本轮结束后清掉） */
  const scheduleNote = ref("");

  /** 卡内状态 / 记忆 / 界面事件（M1.6 可观测面；面板与收尾共用） */
  const cardState = ref<Record<string, unknown>>({});
  const memory = ref<MemRecord[]>([]);
  /** 卡内界面事件（api.ui.emit）：最近 50 条，最新的在前 */
  const hookEvents = ref<{ kind: string; value: string; turn: number }[]>([]);
  /** 卡内错误日志（沙箱错误边界捕获；只在面板里显示） */
  const hookLogs = ref<string[]>([]);
  /** 本轮生成期间收到的钩子报告（done 事件另带一份） */
  const lastReport = ref<HookReport | null>(null);

  // ---------- 滚动（D10：合帧 + 近底跟随） ----------

  async function scrollToBottom() {
    await nextTick();
    const el = deps.streamEl.value;
    if (el) el.scrollTop = el.scrollHeight;
  }

  /** D10：流式滚动合帧——requestAnimationFrame 每帧最多滚一次，且仅当用户已接近
   *  底部（80px 内）才自动跟随。逐 token 强制贴底 + 强制布局曾是长会话掉帧的来源；
   *  回看历史时流式 token 也不再拽走视线。消息重读等主动滚动仍走 scrollToBottom。 */
  let followScrollQueued = false;
  function requestFollowScroll() {
    if (followScrollQueued) return;
    followScrollQueued = true;
    requestAnimationFrame(() => {
      followScrollQueued = false;
      const el = deps.streamEl.value;
      if (!el) return;
      if (el.scrollHeight - el.scrollTop - el.clientHeight < 80) {
        el.scrollTop = el.scrollHeight;
      }
    });
  }

  // ---------- 卡内数据 ----------

  /** 读卡内状态与记忆流（面板数据源）；状态按当前视角（M3.11） */
  async function refreshCard() {
    try {
      const [state, mem] = await Promise.all([
        api.getCardState(deps.sessionId(), deps.inspView.value || undefined),
        api.listCardMemory(deps.sessionId()),
      ]);
      cardState.value = state;
      memory.value = mem;
    } catch {
      /* 卡内数据读取失败不阻塞聊天 */
    }
  }

  /** 界面事件进列表；轮次缺省时从消息流推算（乐观消息 turn=-1 不算数，M3.0 ③） */
  function pushHookEvent(kind: string, value: string, turn = -1) {
    const hint = turn >= 0 ? turn : currentTurnHint();
    // E4：实时推送（on_delta）与报告落地（applyReport 的 ui_events）会带同一条事件
    // ——按 turn+kind+value 去重，事件流页签里每条只出现一次
    if (hookEvents.value.some((h) => h.turn === hint && h.kind === kind && h.value === value)) {
      return;
    }
    hookEvents.value = [{ kind, value, turn: hint }, ...hookEvents.value].slice(0, 50);
  }

  /** 推算「此刻在第几轮」：跳过乐观上屏的 turn=-1，取最后一条真实消息的轮次 */
  function currentTurnHint(): number {
    for (let i = deps.messages.value.length - 1; i >= 0; i--) {
      const t = deps.messages.value[i].turn;
      if (t >= 0) return t;
    }
    return 0;
  }

  /** 钩子报告落地：卡内状态、记忆增量、日志 */
  function applyReport(report: HookReport) {
    lastReport.value = report;
    if (report.card_state) cardState.value = report.card_state;
    hookLogs.value = report.logs ?? [];
    for (const e of report.ui_events) pushHookEvent(e.kind, e.value, report.turn);
  }

  function clearHookEvents() {
    hookEvents.value = [];
  }

  // ---------- 发送 / 重roll / 停止 ----------

  /** B5：流式世代号——sendText/reroll 每次开流自增；增量与收尾回调校验会话 id + 世代号，
   *  生成中切会话/开新流后旧流一律丢弃（不串台、旧流收尾不掐灭新会话的流式区） */
  let streamGen = 0;

  /** 发送一段文本并流式接收（手动输入与剧场自动轮次共用） */
  async function sendText(content: string) {
    if (!content || deps.generating.value) return;
    const sid = deps.sessionId();
    const gen = ++streamGen; // B5：本轮流式的世代
    deps.error.value = "";
    deps.generating.value = true;
    streams.value = [];
    scheduleNote.value = "";
    // 乐观上屏；终态后以磁盘为准重读（带场景归属，切着场景时不出戏）
    deps.messages.value = [
      ...deps.messages.value,
      { turn: -1, role: "user", content, ts: 0, scene_id: deps.activeScene.value || undefined },
    ];
    void deps.jumpToLastPage();
    let final: StreamEvent;
    try {
      // speaker 为空 = 导演调度（多角色自动选人）；点名则直通该角色
      final = await api.sendMessage(sid, content, (e) => onDelta(e, sid, gen), deps.speaker.value || undefined);
    } catch (e) {
      final = { event: "error", message: String(e) };
    }
    await finishGeneration(final, sid, gen);
  }

  async function reroll() {
    if (deps.generating.value) return;
    const sid = deps.sessionId();
    const gen = ++streamGen; // B5：本轮流式的世代
    deps.error.value = "";
    deps.generating.value = true;
    streams.value = [];
    scheduleNote.value = "";
    void deps.jumpToLastPage();
    let final: StreamEvent;
    try {
      final = await api.regenerate(sid, (e) => onDelta(e, sid, gen));
    } catch (e) {
      final = { event: "error", message: String(e) };
    }
    await finishGeneration(final, sid, gen);
  }

  function onDelta(e: StreamEvent, sid: string, gen: number) {
    // B5：旧流（会话已切/世代已换代）的增量一律丢弃
    if (sid !== deps.sessionId() || gen !== streamGen) return;
    if (e.event === "delta") {
      // 换人了就开新气泡（M3.4 群聊：先发言者的话落定后，下一位接着流式）
      const last = streams.value[streams.value.length - 1];
      const name = e.name ?? last?.name ?? deps.cardName.value;
      if (last && last.name === name) last.text += e.text;
      else streams.value.push({ name, text: e.text });
      requestFollowScroll(); // D10：合帧滚动替代逐 token 强制布局
    } else if (e.event === "hook_event") {
      // turn 由后端带上（产生该事件的钩子轮次）；没有就回退到本地推算
      pushHookEvent(e.kind, e.value, e.turn ?? -1);
    } else if (e.event === "director") {
      // 调度指示：第一个字出现前，「谁在说话、为何轮到她」就有答案
      scheduleNote.value = `导演：${e.brief} 接话`;
    }
  }

  /** 生成收尾：错误上报、消息与黑板重读（时钟已步进）、检查器刷新 */
  async function finishGeneration(final: StreamEvent, sid: string, gen: number) {
    // B5：旧流收尾不落地——组件已换会话/换代时，它自己的 generating 已由 loadAll 或新流重置
    if (sid !== deps.sessionId() || gen !== streamGen) return;
    if (final.event === "error") deps.error.value = final.message;
    if (final.event === "done" && final.report) applyReport(final.report);
    try {
      const [msgs, bb] = await Promise.all([
        api.readMessages(deps.sessionId()),
        api.getBlackboard(deps.sessionId()),
      ]);
      deps.messages.value = [...msgs];
      deps.blackboard.value = bb;
      Object.assign(deps.bbForm, {
        day: bb.day,
        clock: bb.clock,
        place: bb.place,
        actors: bb.actors.join(", "),
      });
    } catch (e) {
      deps.error.value = String(e);
    }
    deps.editingIndex.value = -1; // B3：消息以磁盘为准重读，编辑框的旧下标不再可信
    deps.generating.value = false;
    streams.value = [];
    scheduleNote.value = "";
    void deps.refreshInspector();
    void refreshCard();
    // 剧场模式（M4.1 加固）：轮末回报调度器（用户手动停止 → 暂停到下一轮干净收尾），
    // 推进本身交给 autoAdvance——它内部重读进度、复核守卫，出错/取消/超预算就原地不动
    const cancelled = final.event === "done" && final.cancelled;
    if (deps.theaterApi.theater.value?.on) {
      deps.theaterApi.notifyRoundEnd(cancelled);
      void deps.theaterApi.autoAdvance();
    }
    // 刚聊完一轮：状态树/剧情线/心理/宫殿多半都变了，抽屉开着就顺手重拉（设计 §8/§9 的「面板可查」）
    await deps.onRoundEnd();
    void scrollToBottom();
    deps.composerEl.value?.focus();
  }

  async function stop() {
    try {
      await api.stopGeneration(deps.sessionId());
    } catch (e) {
      // E4：中断失败不该是未处理的 promise 拒绝
      deps.error.value = String(e);
    }
  }

  return {
    streams,
    scheduleNote,
    cardState,
    memory,
    hookEvents,
    hookLogs,
    lastReport,
    sendText,
    reroll,
    stop,
    refreshCard,
    clearHookEvents,
    scrollToBottom,
  };
}
