// 浏览器开发 mock：`pnpm dev` 纯前端调试（无 Tauri 后端）时提供内存数据。
// 仅在 window.__TAURI_INTERNALS__ 缺失时启用（见 main.ts）；打包进 Tauri 后不生效。

import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  Blackboard,
  CardDetail,
  CardSummary,
  HookReport,
  InspectorThreads,
  MemRecord,
  Message,
  Persona,
  PromptAssembly,
  Provider,
  Scene,
  SceneView,
  SessionMeta,
  Settings,
  StreamEvent,
} from "./types";

const sessionId = "20260919-101500-000";

const providers: Provider[] = [
  {
    name: "deepseek",
    base_url: "https://api.deepseek.com/v1",
    api_key: "sk-mock",
    model: "deepseek-chat",
    temperature: 0.8,
    role: "chat",
  },
];

const settings: Settings = {
  locale: "zh-CN",
  theme: "huajing",
  narrative_mode: "台词体",
  wizard_done: true,
  proxy: null,
};

const personas: Persona[] = [
  { name: "夜读者", description: "安静的图书馆常客，话少，观察细。" },
];

const cards: CardSummary[] = [
  {
    dir_name: "小雨",
    name: "小雨",
    tags: ["图书馆", "安静"],
    creator: "化境示例",
    has_hooks: true,
    degraded: false,
  },
];

const blackboard: Blackboard = {
  day: 3,
  clock: "21:30",
  place: "图书馆自习区",
  actors: ["小雨", "夜读者"],
};

const messages: Message[] = [
  {
    turn: 0,
    role: "char",
    content:
      "（她把面前的书往旁边挪了挪，腾出半边桌面）这个位置光线好一些。\n\n要闭馆了才过来，又是来赶稿的？",
    ts: 1758200000,
  },
  {
    turn: 1,
    role: "user",
    content: "嗯，截稿日快到了。你还没走？",
    ts: 1758200100,
  },
  {
    turn: 1,
    role: "char",
    content:
      "「我在等雨停。」她朝窗外扬了扬下巴，玻璃上全是细密的水痕。\n\n「不过现在看起来，还要再下一会儿。」",
    ts: 1758200160,
  },
];

// ---------- 场景与多线（M3.2 · 设计 §10.3）：mock 双场景，演示「与此同时」 ----------
const mockScenes: Scene[] = [
  {
    id: "scene.main",
    title: "开场",
    place: "图书馆自习区",
    actors: ["小雨", "夜读者"],
    day: 3,
    clock: "21:30",
    created_turn: 0,
    origin: "default",
    parent: null,
    status: "active",
    ts: 1758200000,
  },
  {
    id: "scene.b",
    title: "旧书店",
    place: "坡下的旧书店",
    actors: ["夜读者"],
    day: 3,
    clock: "21:20",
    created_turn: 1,
    origin: "split",
    parent: "scene.main",
    status: "frozen",
    ts: 1758200050,
  },
];
let activeScene = "scene.main";

function sceneView(): SceneView {
  return { scenes: mockScenes.map((sc) => ({ ...sc, flags: {} })), active: activeScene };
}

/** mock 的卡内状态与记忆流（M1.6 面板数据） */
const mockCardState: Record<string, unknown> = { favorability: 52 };

const memoryRecords: MemRecord[] = [
  {
    kind: "fact",
    key: "last_thanked",
    value: 1,
    source: "hook.on_message",
    turn: 1,
    ts: 1758200160,
  },
];

/** mock 的剧情线投影（M3.0 ① 手动开/收线与之共享同一份状态） */
const opened = { turn: 2, story_day: 3, story_clock: "21:30" };
const mockThreads: InspectorThreads = {
  active: [
    {
      id: "thread.周五还书",
      title: "周五还书的约定",
      cause: "玩家忘带借书卡，小雨破例让他先把书拿走。",
      actors: ["小雨", "玩家"],
      importance: 0.7,
      opened,
      state: "active",
      scope: "session",
      linked_intent: null,
      progress: [{ turn: 2, note: "立约", memory: null }],
      resurface: {
        grade: "natural",
        windows: [{ mention: ["还书", "借书卡"] }],
        deadline: { day: 5, escalate: "eager" },
        cooldown: 5,
        framing: "她在意但不好意思催。",
        last_mentioned_turn: null,
      },
      resolution: null,
    },
  ],
  resolved: [],
  abandoned: [],
  pending: ["周五还书的约定（active）"],
  inWindow: [
    {
      id: "thread.周五还书",
      title: "周五还书的约定",
      grade: "natural",
      framing: "她在意但不好意思催。",
      reason: "提及:还书",
    },
  ],
  eventCount: 1,
};

const cardDetail: CardDetail = {
  dir_name: "小雨",
  card: {
    spec: "huajing.card/1",
    name: "小雨",
    avatar: null,
    creator: "化境示例",
    tags: ["图书馆", "安静"],
    world: null,
    scenario: "闭馆前的图书馆自习区",
    personality: "安静、克制、观察细",
    first_mes: messages[0].content,
    example_dialogue: [],
  },
  default_state: { favorability: 50 },
  hook_names: ["on_context", "on_message"],
  degraded: false,
  degrade_reason: null,
};

function assembly(source: Message[], userContent?: string): PromptAssembly {
  void source;
  void userContent;
  return {
    layers: [
      {
        id: "A1",
        name: "全局契约",
        content: "<contract>\n表达契约：台词体…\n</contract>",
        tokens: 180,
      },
      {
        id: "A3",
        name: "身份锚",
        content: "<identity>\n小雨。安静、克制、观察细。\n</identity>",
        tokens: 96,
      },
      {
        id: "B1",
        name: "场景快照",
        content: `<scene>\n第 ${blackboard.day} 天 · ${blackboard.clock} · ${blackboard.place}\n在场：${blackboard.actors.join("、")}\n</scene>`,
        tokens: 40,
      },
    ],
    messages: [
      { role: "system", content: "…" },
      { role: "user", content: "嗯，截稿日快到了。你还没走？" },
    ],
    total_tokens: 640,
  };
}

/** 流式生成一段 mock 回复；返回 Promise 在 done 时 resolve（与真后端行为一致） */
function streamReply(
  msgs: Message[],
  content: string,
  onEvent: { onmessage?: (e: StreamEvent) => void } | undefined,
  timerMap: Map<string, { timer: ReturnType<typeof setInterval>; partial: () => string; done: (cancelled: boolean) => void }>,
  timerKey: string,
): Promise<StreamEvent> {
  const reply = `（mock 回复）你说了「${content.slice(0, 24)}」。\n\n她把笔帽轻轻扣上：「那就再坐一会儿吧，反正雨还没停。」`;
  return new Promise<StreamEvent>((resolve) => {
    let i = 0;
    const turn = (msgs[msgs.length - 1]?.turn ?? 0) + 1;
    // 与真后端同形：回复落盘后跑 on_message（mock 里演一遍好感度 +1 与 ui.emit）
    const finishHook = (): HookReport => {
      const thanked = content.includes("谢谢");
      const next = Number(mockCardState.favorability ?? 50) + (thanked ? 1 : 0);
      mockCardState.favorability = next;
      const memory = thanked
        ? [{ key: "last_thanked", value: turn }]
        : [];
      if (memory.length) {
        memoryRecords.push({
          kind: "fact",
          key: memory[0].key,
          value: memory[0].value,
          source: "hook.on_message",
          turn,
          ts: Math.floor(Date.now() / 1000),
        });
      }
      return {
        turn,
        ran: true,
        card_state: { ...mockCardState },
        memory,
        ui_events: [{ kind: "emotion", value: next >= 80 ? "shy" : "calm" }],
        logs: [],
      };
    };

    const finish = (cancelled: boolean) => {
      const full = reply.slice(0, i);
      clearInterval(timer);
      timerMap.delete(timerKey);
      if (full) msgs.push({ turn, role: "char", content: full, ts: Math.floor(Date.now() / 1000) });
      const report = full ? finishHook() : null;
      if (report)
        onEvent?.onmessage?.({ event: "hook_event", ...report.ui_events[0], turn: report.turn });
      resolve({ event: "done", full, cancelled, report });
    };
    const timer = setInterval(() => {
      i = Math.min(i + 3, reply.length);
      if (i >= reply.length) {
        msgs.push({ turn, role: "char", content: reply, ts: Math.floor(Date.now() / 1000) });
        clearInterval(timer);
        timerMap.delete(timerKey);
        const report = finishHook();
        onEvent?.onmessage?.({ event: "hook_event", ...report.ui_events[0], turn: report.turn });
        resolve({ event: "done", full: reply, cancelled: false, report });
        return;
      }
      onEvent?.onmessage?.({ event: "delta", text: reply.slice(i - 3, i) });
    }, 40);
    timerMap.set(timerKey, { timer, partial: () => reply.slice(0, i), done: finish });
  });
}

const timers = new Map<string, { timer: ReturnType<typeof setInterval>; partial: () => string; done: (cancelled: boolean) => void }>();

export function setupMock() {
  mockIPC(async (cmd, args) => {
    const a = args as Record<string, never> & {
      sessionId?: string;
      content?: string;
      index?: number;
      onEvent?: { onmessage?: (e: StreamEvent) => void };
      provider?: Provider;
      name?: string;
      day?: number;
      clock?: string;
      place?: string;
      actors?: string[];
    };
    switch (cmd) {
      case "app_info":
        return {
          name: "化境 Huajing",
          slogan: "扮谁，便入谁之境。",
          version: "0.1.0-mock",
          buildTs: Math.floor(Date.now() / 1000),
          dataRoot: "DataHub（浏览器 mock）",
        };
      case "recent_diagnostics":
        return [
          { kind: "chat", detail: "send_message 会话=mock 卡=小雨（DataHub（浏览器 mock），钩子=[on_context, on_message]）", ts: Math.floor(Date.now() / 1000) },
          { kind: "hook", detail: "on_message turn=1 ran=true state={\"favorability\":51} 记忆写入=1 日志=[]", ts: Math.floor(Date.now() / 1000) },
        ];
      case "runtime_info":
        return {
          dataRoot: "DataHub（浏览器 mock）",
          cardCount: cards.length,
          sessionCount: 1,
          now: Math.floor(Date.now() / 1000),
          buildTs: Math.floor(Date.now() / 1000),
        };
      case "list_providers":
        return providers;
      case "save_provider": {
        const p = (args as { provider: Provider }).provider;
        const i = providers.findIndex((x) => x.name === p.name);
        if (i >= 0) providers[i] = p;
        else providers.push(p);
        return providers;
      }
      case "delete_provider": {
        const i = providers.findIndex((x) => x.name === (args as { name: string }).name);
        if (i >= 0) providers.splice(i, 1);
        return providers;
      }
      case "get_settings":
        return settings;
      case "save_settings":
        Object.assign(settings, args as Partial<Settings>);
        return settings;
      case "list_personas":
        return personas;
      case "list_cards":
        return cards;
      case "get_card":
        return cardDetail;
      case "list_sessions":
        return [
          {
            id: sessionId,
            created_at: "2026-09-19T10:15:00Z",
            characters: ["小雨"],
            persona: "夜读者",
            seed: 42,
          } satisfies SessionMeta,
        ];
      case "new_session": {
        const q = args as { characters?: string[] };
        return {
          id: sessionId,
          created_at: "2026-09-19T10:15:00Z",
          characters: q.characters?.length ? q.characters : ["小雨"],
          persona: "夜读者",
          seed: 42,
        } satisfies SessionMeta;
      }
      case "read_messages":
        return messages;
      case "edit_message": {
        messages[a.index!].content = a.content!;
        return messages;
      }
      case "delete_message": {
        messages.splice(a.index!, 1);
        return messages;
      }
      case "send_message": {
        // speaker（M3.1 群聊）：mock 里不影响生成内容，只透传；消息归属当前场景（M3.2）
        const turn = (messages[messages.length - 1]?.turn ?? 0) + 1;
        messages.push({ turn, role: "user", content: a.content!, ts: Math.floor(Date.now() / 1000), scene_id: activeScene });
        return streamReply(messages, a.content!, a.onEvent, timers, sessionId);
      }
      case "regenerate": {
        if (messages[messages.length - 1]?.role === "char") messages.pop();
        const lastUser = [...messages].reverse().find((m) => m.role === "user");
        if (!lastUser) return { event: "error", message: "找不到可重roll的用户消息" } satisfies StreamEvent;
        return streamReply(messages, lastUser.content, a.onEvent, timers, sessionId);
      }
      case "stop_generation": {
        const t = timers.get(sessionId);
        if (!t) return false;
        t.done(true);
        return true;
      }
      case "get_blackboard":
        return blackboard;
      case "update_blackboard": {
        blackboard.day = a.day ?? blackboard.day;
        blackboard.clock = a.clock ?? blackboard.clock;
        blackboard.place = a.place ?? blackboard.place;
        blackboard.actors = a.actors ?? blackboard.actors;
        return blackboard;
      }
      case "preview_prompt":
        return assembly(messages);
      case "last_prompt":
        return null;
      case "test_provider":
        return {
          ok: true,
          message: "连接成功（mock 12 ms）",
          url: `${(args as { provider?: Provider }).provider?.base_url ?? ""}/chat/completions`,
          model: (args as { provider?: Provider }).provider?.model ?? "",
          detail: "（mock 不真的发请求）",
          elapsed_ms: 12,
          proxy: [],
          proxy_used: null,
        };
      case "watch_cards":
      case "unwatch_cards":
        return null;
      case "preview_st_card":
      case "import_st_card":
        throw new Error("浏览器 mock 不支持导入：请用 pnpm tauri dev");
      case "get_card_state":
        return { ...mockCardState };
      case "list_card_memory":
        return memoryRecords;
      // ---------- 场景与多线（M3.2 · 设计 §10.3） ----------
      case "list_scenes":
        return sceneView();
      case "create_scene": {
        const id = `scene.${Date.now()}`;
        mockScenes.push({
          id,
          title: (a as Record<string, string>).title ?? "新场景",
          place: (a as Record<string, string>).place ?? "",
          actors: (a as unknown as { actors?: string[] }).actors ?? [],
          day: blackboard.day,
          clock: blackboard.clock,
          created_turn: 0,
          origin: "manual",
          parent: null,
          status: "active",
          ts: Math.floor(Date.now() / 1000),
        });
        for (const sc of mockScenes) sc.status = sc.id === id ? "active" : "frozen";
        activeScene = id;
        messages.push({
          turn: 0,
          role: "system",
          content: `——${(a as Record<string, string>).title ?? ""}·${(a as Record<string, string>).place ?? ""}——`,
          ts: Math.floor(Date.now() / 1000),
          scene_id: id,
        });
        return sceneView();
      }
      case "switch_scene": {
        const target = (a as unknown as { sceneId: string }).sceneId;
        for (const sc of mockScenes) sc.status = sc.id === target ? "active" : "frozen";
        activeScene = target;
        const sc = mockScenes.find((x) => x.id === target);
        messages.push({
          turn: 0,
          role: "system",
          content: `与此同时，${sc?.place ?? "?"}——`,
          ts: Math.floor(Date.now() / 1000),
          scene_id: target,
        });
        return sceneView();
      }
      case "split_scene":
      case "merge_scenes":
        throw new Error("浏览器 mock 不支持分场/合场：请用 pnpm tauri dev");
      case "update_scene":
        return sceneView();
      // 记忆检查器（M2.8）：形状与 commands.rs 的 inspector_data 对齐，
      // 让浏览器调试路径也能看到 M2 的七个面板（数据是示意值，不参与对话逻辑）。
      case "inspector_data":
        return {
          session: a.sessionId ?? "mock",
          character: "小雨",
          stateTree: {
            root: "日常",
            path: ["日常", "日常.夜谈"],
            directive: "保持轻松日常的氛围，话题围绕图书馆与学业。\n夜深人静，两人独处。语速放慢。",
            recall: ["room:图书馆", "topic:过去"],
            reveal: ["char.小雨.secrets.工作牌"],
            states: ["日常", "日常.夜谈", "疏远"],
            warnings: [],
          },
          transitions: [
            {
              turn: 3,
              from: ["日常"],
              to: ["日常", "日常.夜谈"],
              reason: "日常 → 日常.夜谈（priority 10）",
              ts: Math.floor(Date.now() / 1000),
            },
          ],
          threads: mockThreads,
          psyche: {
            summary: "【小雨·内心】喜悦0.6 ▸ 惦记着说再见(0.4)",
            affects: [
              {
                name: "喜悦",
                intensity: 0.6,
                source: "被道谢",
                since_turn: 2,
                history: [
                  { turn: 2, intensity: 0.8 },
                  { turn: 3, intensity: 0.68 },
                  { turn: 4, intensity: 0.6 },
                ],
              },
            ],
            intents: [{ name: "惦记着说再见", strength: 0.4, linked_thread: null, since_turn: 2 }],
            trail: [
              [
                "喜悦",
                [
                  { turn: 2, intensity: 0.8 },
                  { turn: 3, intensity: 0.68 },
                ],
              ],
            ],
            auto_emotion: "喜悦",
          },
          palace: {
            count: 1,
            rooms: [{ place: "图书馆", count: 1, top: [] }],
            timeline: [{ label: "第3天", count: 1, top: [] }],
            graph: { nodes: ["topic:便签", "person:小雨"], edges: [["person:小雨", "topic:便签", 1]] },
            recent: [
              {
                id: "mem_0001",
                content: "深夜闭馆时她把画着猫的便签送给了玩家",
                turn: 2,
                story_day: 3,
                story_clock: "第3天 23:40",
                salience: 0.82,
                emotion: "温暖",
                place: "图书馆",
                source: "hook.on_message",
              },
            ],
          },
          codex: {
            world: "default",
            count: 1,
            entities: [
              {
                id: "char.小雨",
                name: "小雨",
                type: "char",
                status: "canon",
                oneLiner: "大学图书馆夜班管理员。",
                anchors: ["左眼角一颗泪痣", "母亲留下的旧胸牌"],
              },
            ],
          },
          summary: "第一段：她记住了那个约定。",
          proposals: [
            {
              id: "codex.char.小雨.2",
              status: "propose",
              kind: "new_fact",
              turn: 2,
              payload: { target: "char.小雨", value: { facts: { schedule: "周三休息" } }, reason: "剧情里提到" },
            },
          ],
          known: ["char.小雨.secrets.工作牌"],
          blackboard: { day: 3, clock: "23:40", place: "图书馆", actors: ["小雨"] },
          activeEntities: ["char.小雨", "place.图书馆"],
        };
      case "decide_proposal":
        return { id: (args as { id?: string }).id ?? "mock", status: (args as { accept?: boolean }).accept ? "accept" : "reject" };
      case "summarize_now":
        return "（mock）已总结第 1–2 轮，落 3 条事件";
      // M3.0 ①：手动开/收线（mock 只改内存里的 threads 投影，真命令会落事件流）
      case "open_thread": {
        const q = args as { title?: string; cause?: string; actors?: string[]; importance?: number };
        const id = `thread.${q.title ?? "新线"}`;
        mockThreads.active.push({
          id,
          title: q.title ?? "新线",
          cause: q.cause ?? "",
          actors: q.actors ?? [],
          importance: q.importance ?? 0.6,
          opened: { turn: messages[messages.length - 1]?.turn ?? 0, story_day: blackboard.day, story_clock: blackboard.clock },
          state: "active",
          progress: [],
          resurface: {
            grade: "natural",
            windows: [],
            deadline: null,
            cooldown: 3,
            framing: "",
            last_mentioned_turn: null,
          },
          resolution: null,
          scope: "session",
          linked_intent: null,
        });
        mockThreads.pending.push(`${q.title ?? "新线"}（active）`);
        return { id, state: "active" };
      }
      case "resolve_thread": {
        const q = args as { id?: string; outcome?: string };
        const i = mockThreads.active.findIndex((t) => t.id === q.id);
        if (i >= 0) {
          const [t] = mockThreads.active.splice(i, 1);
          mockThreads.resolved.push({
            ...t,
            state: "resolved",
            resolution: {
              turn: messages[messages.length - 1]?.turn ?? 0,
              story_day: blackboard.day,
              story_clock: blackboard.clock,
              outcome: q.outcome ?? "",
              memory: null,
            },
          });
        }
        return { id: q.id ?? "mock", state: "resolved" };
      }
      // M3.0 ④：类型化事件流视图（mock 给一份与真实形态一致的示意）
      case "session_timeline":
        return [
          { seq: 4, kind: "blackboard", turn: 1, brief: "clock → 第3天 21:40 图书馆自习区" },
          { seq: 3, kind: "effect", turn: 1, brief: "on_message:char（小雨）state×1" },
          { seq: 2, kind: "message", turn: 1, brief: "角色：「我在等雨停。」…" },
          { seq: 1, kind: "message", turn: 1, brief: "我：嗯，截稿日快到了。你还没走？" },
        ];
      default:
        throw new Error(`mock 未覆盖命令：${cmd}`);
    }
  });
}
