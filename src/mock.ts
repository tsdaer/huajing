// 浏览器开发 mock：`pnpm dev` 纯前端调试（无 Tauri 后端）时提供内存数据。
// 仅在 window.__TAURI_INTERNALS__ 缺失时启用（见 main.ts）；打包进 Tauri 后不生效。

import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  Blackboard,
  CardDetail,
  CardSummary,
  HookReport,
  IngestPack,
  IngestPrep,
  IngestSection,
  InspectorThreads,
  MemRecord,
  Message,
  Persona,
  PromptAssembly,
  Provider,
  ResolvePreview,
  Scene,
  SceneView,
  SessionMeta,
  Settings,
  StreamEvent,
  TheaterView,
  WorldlineView,
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
    tools: "off",
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

/** mock 的剧场视图（M3.6；浏览器模式只示意，不驱动对话逻辑） */
let mockTheaterView: TheaterView = {
  on: false,
  budget: 0,
  used: 0,
  start_turn: 0,
  last_turn: 0,
  path: [],
  stage_directive: "",
  custom_tree: false,
};

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

/** mock 的素材清洗分段（M3.9）：按空行粗切两节 + 示意剧透候选 */
function mockIngestPrep(text: string): IngestPrep {
  const paras = text.split(/\n{2,}/).filter((p) => p.trim());
  const sections: IngestSection[] = paras.length
    ? paras.map((p, i) => ({ id: `s${i + 1}`, title: i === 0 ? "开头" : `小节 ${i}`, text: p.trim() }))
    : [{ id: "s1", title: "全文", text: "（mock）空素材占位一节。" }];
  return { world: "default", sections, spoilers: ["（mock）实为人之律者"] };
}

/** mock 的草稿包（M3.9 审阅界面浏览器示意） */
function mockIngestPack(world: string): IngestPack {
  return {
    world,
    char_id: "char.小雨",
    card: {
      first_mes: "（mock）「你来了。今天也一起等雨停吗？」",
      scenario: "（mock）梅雨季的大学图书馆。",
      personality: "动机：替母亲读完她没读完的书；需要：被理解",
      tags: ["素材规格化"],
      example_dialogue: [
        {
          tag: "初见",
          messages: [
            { role: "user", content: "这么晚还在？" },
            { role: "char", content: "……嗯。闭馆前看完这一章就好。" },
          ],
        },
      ],
      sources: { first_mes: { section: "s2", quote: "「你来了。」" } },
    },
    entity: {
      id: "char.小雨",
      type: "char",
      name: "小雨",
      aliases: ["夜班管理员"],
      one_liner: "左眼角有泪痣的图书馆夜班管理员。",
      facts: {
        look: { impression: "总披着一件旧毛衣", anchors: ["左眼角一颗泪痣"] },
        speech: { style: "短句、轻声", tics: ["……嗯。"] },
        tells: { 忐忑: "指尖轻敲桌面" },
        motivation: "替母亲读完她没读完的书",
      },
      relations: [{ to: "place.图书馆", kind: "值班", always_with: true }],
      sources: { "look.anchors": { section: "s1", quote: "左眼角一颗泪痣" } },
      include: true,
      stub: false,
    },
    others: [
      {
        id: "place.图书馆",
        type: "place",
        name: "图书馆",
        aliases: [],
        one_liner: "（占位）她值班的地方。",
        facts: {},
        relations: [],
        sources: {},
        include: true,
        stub: true,
      },
    ],
    events: [],
    secrets: [
      {
        key: "工作牌",
        content: "她随身带的旧胸牌是母亲的遗物",
        revealed_by: null,
        known_by_advice: ["小雨"],
        source: { section: "s2", quote: "母亲的遗物" },
        include: true,
        origin: "llm",
      },
    ],
    lifecycle: null,
    versions: [],
    worldline: null,
    canon_points: [{ name: "素材开篇", day: 1, stage: null, note: "（mock）从第 1 天开始", after_death: false, premise: null }],
    pending: [{ title: "开场白", detail: "（mock）素材里没有合适的开场白", source: null }],
    qc: [{ severity: "info", at: "mannerisms", problem: "描写四法缺「动作」块——可用实体补全接力" }],
  };
}

export function setupMock() {
  mockIPC(async (cmd, args) => {
    const a = args as Record<string, never> & {
      sessionId?: string;
      content?: string;
      index?: number;
      character?: string;
      speaker?: string;
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
          slogan: "化万千相，随心入境。",
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
        // M3.11：mock 会话升级为双角色阵容——群聊 UI（发言权/视角切换/同台徽标）浏览器模式可走查
        return [
          {
            id: sessionId,
            created_at: "2026-09-19T10:15:00Z",
            characters: ["小雨", "阿澈"],
            persona: "夜读者",
            seed: 42,
          } satisfies SessionMeta,
        ];
      case "new_session": {
        const q = args as { characters?: string[] };
        return {
          id: sessionId,
          created_at: "2026-09-19T10:15:00Z",
          characters: q.characters?.length ? q.characters : ["小雨", "阿澈"],
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
        // M3.11 视角切换：mock 示意数据不分角色，带一条标记以示来源
        return a.character
          ? { ...mockCardState, [`【${a.character}的state】`]: "（mock 示意）" }
          : { ...mockCardState };
      case "list_card_memory":
        return memoryRecords;
      // ---------- 场景与多线（M3.2 · 设计 §10.3） ----------
      case "set_max_speakers":
        return (args as { maxSpeakers: number }).maxSpeakers;
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
      case "split_scene": {
        // M3.11：mock 实装分场语义——moving 离场另立新场景，原场景冻结，视角跟过去
        const q = a as unknown as { title: string; place: string; moving: string[] };
        const here = mockScenes.find((x) => x.id === activeScene);
        const id = `scene.${Date.now()}`;
        mockScenes.push({
          id,
          title: q.title,
          place: q.place,
          actors: [...(q.moving ?? [])],
          day: here?.day ?? blackboard.day,
          clock: here?.clock ?? blackboard.clock,
          created_turn: 0,
          origin: "split",
          parent: activeScene,
          status: "active",
          ts: Math.floor(Date.now() / 1000),
        });
        if (here) {
          here.actors = here.actors.filter((x) => !(q.moving ?? []).includes(x));
          here.status = "frozen";
        }
        activeScene = id;
        messages.push({
          turn: 0,
          role: "system",
          content: `与此同时，${q.place || q.title}——`,
          ts: Math.floor(Date.now() / 1000),
          scene_id: id,
        });
        return sceneView();
      }
      case "merge_scenes": {
        // M3.11：mock 实装合场语义——在场者并集、时间取较晚、被并入方归档，记忆零写入
        const from = ((a as unknown as { from: string[] }).from ?? []) as string[];
        const here = mockScenes.find((x) => x.id === activeScene);
        for (const id of from) {
          const sc = mockScenes.find((x) => x.id === id);
          if (!sc || !here) continue;
          for (const who of sc.actors) if (!here.actors.includes(who)) here.actors.push(who);
          if (sc.day > here.day || (sc.day === here.day && sc.clock > here.clock)) {
            here.day = sc.day;
            here.clock = sc.clock;
          }
          sc.status = "merged";
        }
        return sceneView();
      }
      case "update_scene": {
        const q = a as unknown as {
          sceneId: string;
          title?: string;
          place?: string;
          actors?: string[];
          day?: number;
          clock?: string;
        };
        const sc = mockScenes.find((x) => x.id === q.sceneId);
        if (sc) {
          if (q.title !== undefined) sc.title = q.title;
          if (q.place !== undefined) sc.place = q.place;
          if (q.actors !== undefined) sc.actors = [...q.actors];
          if (q.day !== undefined) sc.day = q.day;
          if (q.clock !== undefined) sc.clock = q.clock;
        }
        return sceneView();
      }
      // ---------- 剧场模式（M3.6 · 设计 §10.5）：浏览器 mock 只做视图示意 ----------
      case "theater_view":
        return mockTheaterView;
      case "set_theater": {
        const on = (a as unknown as { on: boolean }).on;
        mockTheaterView = {
          on,
          budget: on ? (a as unknown as { budget?: number }).budget ?? 20 : 0,
          used: 0,
          start_turn: 0,
          last_turn: 0,
          path: on ? ["起"] : [],
          stage_directive: on ? "起：铺陈日常与人物，让张力自然登场。" : "",
          custom_tree: false,
        };
        return mockTheaterView;
      }
      // ---------- 世界主线与世界时钟（M3.7 · 设计 §6.6）：浏览器 mock 只做视图示意 ----------
      case "worldline_view":
        return {
          configured: true,
          id: "worldline.图书馆拆迁",
          premise: "老图书馆月底拆除，所有人都在倒数。",
          path: ["传闻期", "公告期"],
          stage: "公告期",
          stage_directive: "公告已贴出，空气里有告别的味道；各角色心怀不同的盘算。",
          era: "公告期——公告已贴出，空气里有告别的味道…",
          world_day: 20,
          session_day: blackboard.day,
          world_threads: [
            {
              id: "thread.最后一个月",
              title: "最后一个月",
              cause: "世界大势：闭馆倒计时开始。",
              state: "active",
              scope: "world",
            },
          ],
          updated_by: "mock-session",
        } satisfies WorldlineView;
      case "world_set_clock":
        return (a as unknown as { day: number }).day;
      case "codex_resolve_preview": {
        const day = (a as unknown as { day?: number }).day ?? blackboard.day;
        return {
          day,
          entities: [
            {
              id: "char.小雨",
              name: "小雨",
              type: "char",
              status: "canon",
              lifecycle: { status: "active", present: true },
              versions: [
                { from_day: 15, facet: "one_liner", value: "剪了短发的小雨。", note: "第15天剪发", active: day >= 15 },
              ],
              one_liner: day >= 15 ? "剪了短发的小雨。" : "长发的小雨。",
            },
            {
              id: "place.旧书店",
              name: "旧书店",
              type: "place",
              status: "retired",
              lifecycle: { status: "active", present: true },
              versions: [],
              one_liner: "已经关门的旧书店。",
            },
          ],
        } satisfies ResolvePreview;
      }
      // 记忆检查器（M2.8）：形状与 commands.rs 的 inspector_data 对齐，
      // 让浏览器调试路径也能看到 M2 的七个面板（数据是示意值，不参与对话逻辑）。
      case "inspector_data":
        // M3.11 视角切换：多角色时按 character 回显视角（示意数据本身不分视角）
        return {
          session: a.sessionId ?? "mock",
          character: (a as { character?: string }).character || "小雨",
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
            intents: [
              {
                name: "惦记着说再见",
                strength: 0.4,
                linked_thread: null,
                since_turn: 2,
                triggered: {
                  turn: 3,
                  threshold: 0.52,
                  strength: 0.62,
                  action: "主动想说：「惦记着说再见」",
                },
              },
            ],
            scheduled: [{ text: "惦记着说再见", turn: 3 }],
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
                missing: ["speech.by_affect", "tells", "motivation"],
              },
            ],
          },
          summary: "第一段：她记住了那个约定。",
          proposals: [
            {
              id: "codex.char.小雨.2.0",
              status: "propose",
              kind: "new_fact",
              turn: 2,
              origin: "pipeline",
              payload: { target: "char.小雨", value: { facet: "schedule", value: "周三休息" }, reason: "剧情里提到" },
              currentValue: { facet: "schedule", value: "夜班", source: "char.小雨（正史）" },
            },
            {
              id: "improv.char.小雨.3",
              status: "propose",
              kind: "new_fact",
              turn: 3,
              origin: "improv",
              payload: { target: "char.小雨", value: { facet: "facts.日常", value: "养了一只叫墨墨的猫" }, text: "她养了一只叫墨墨的猫。", provisional: true, reason: "第 3 轮即兴补一条暂定设定" },
            },
            {
              id: "audit.char.小雨.4.0",
              status: "propose",
              kind: "audit",
              turn: 4,
              origin: "pipeline",
              payload: { finding: "missed", target: "char.小雨", facet: "", evidence: "第 4 轮「她今晚不在」用代词指小雨，但激活记录里没有她" },
            },
          ],
          known: ["char.小雨.secrets.工作牌"],
          blackboard: { day: 3, clock: "23:40", place: "图书馆", actors: ["小雨"] },
          activeEntities: ["char.小雨", "place.图书馆"],
        };
      case "decide_proposal":
        return { id: (args as { id?: string }).id ?? "mock", status: (args as { accept?: boolean }).accept ? "accept" : "reject" };
      // M3.8：批量处理 / 手动补全 / 即兴开关（mock 给形状一致的示意返回）
      case "decide_all_proposals":
        return 2;
      case "codex_complete":
        return {
          target: (args as { target?: string }).target ?? "char.小雨",
          items: [
            { facet: "motivation", value: "守着夜班是为了替母亲看完她没读完的书。" },
            { facet: "speech.by_affect", value: { shy: "省略号变多、声音变小", upset: "只剩短句和动作" } },
            { facet: "tells", value: { "忐忑": "指尖轻敲桌面，视线落在书页上却不翻页", "委屈": "不说话，但耳根泛红" } },
          ],
          note: "宁少而精：口癖已并入示例对话的语气。",
          provider: "mock",
        };
      case "codex_complete_apply":
        return (args as { facets?: Record<string, unknown> }).facets
          ? Object.keys((args as { facets: Record<string, unknown> }).facets).length
          : 0;
      case "set_improv":
        return (args as { improv?: boolean }).improv ?? false;
      case "codex_semantic_check":
        return { id: (args as { id?: string }).id ?? "mock", contradictions: [], provider: "mock" };
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
      // ---------- 素材规格化管线（M3.9 · 设计 §6.7）：浏览器示意流 ----------
      case "ingest_prompts":
        return "（mock）P0–P11 提示词套件全文——浏览器模式不打包文档，真机可见。";
      case "ingest_prepare": {
        const text = (args as { text?: string }).text ?? "";
        return mockIngestPrep(text);
      }
      case "ingest_classify":
        return (args as { sections?: Array<{ id: string }> }).sections?.map((s, i) => ({
          id: s.id,
          tag: i === 0 ? "infobox" : "intro",
        })) ?? [];
      case "ingest_extract":
        return mockIngestPack((args as { world?: string }).world ?? "default");
      case "ingest_commit":
        return {
          world: (args as { world?: string }).world ?? "default",
          cardDir: "小雨（mock）",
          cardPath: "DataHub（浏览器 mock）/characters/小雨/card.lua",
          entitiesWritten: ["char.小雨", "place.图书馆"],
          worldlineWritten: true,
          canonDay: (args as { day?: number }).day ?? 1,
          skipped: [],
          warnings: [],
        };
      case "import_worldbook":
        return {
          world: (args as { world?: string }).world ?? "default",
          imported: 3,
          disabled: 1,
          skipped: 0,
          files: ["note.mock书.0.json", "note.mock书.1.json", "note.mock书.2.json", "note.mock书.3.json"],
          warnings: ["条目的 ST 专属字段（order/sticky/cooldown 等）已保留在 facts.st 作参考"],
        };

      // ---------- 包格式与导入导出（M4.1）：浏览器 mock 无本地文件系统，给可读的降级 ----------
      case "preview_pack":
      case "import_pack":
        throw new Error("浏览器 mock 模式没有本地文件系统——包导入/导出请运行打包版或 pnpm tauri dev");
      case "export_card_pack":
      case "export_world_pack":
      case "export_script_pack":
      case "export_worldbook_st":
        throw new Error("浏览器 mock 模式不支持导出——请运行打包版或 pnpm tauri dev");
      case "list_scripts":
        return [] as unknown[];
      case "get_script":
        return {
          name: (args as { name?: string } | undefined)?.name ?? "",
          premise: "",
          blackboard: null,
          has_director: false,
        };
      default:
        throw new Error(`mock 未覆盖命令：${cmd}`);
    }
  });
}
