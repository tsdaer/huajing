// 浏览器开发 mock：`pnpm dev` 纯前端调试（无 Tauri 后端）时提供内存数据。
// 仅在 window.__TAURI_INTERNALS__ 缺失时启用（见 main.ts）；打包进 Tauri 后不生效。

import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  Blackboard,
  CardDetail,
  CardSummary,
  HookReport,
  MemRecord,
  Message,
  Persona,
  PromptAssembly,
  Provider,
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
      if (report) onEvent?.onmessage?.({ event: "hook_event", ...report.ui_events[0] });
      resolve({ event: "done", full, cancelled, report });
    };
    const timer = setInterval(() => {
      i = Math.min(i + 3, reply.length);
      if (i >= reply.length) {
        msgs.push({ turn, role: "char", content: reply, ts: Math.floor(Date.now() / 1000) });
        clearInterval(timer);
        timerMap.delete(timerKey);
        const report = finishHook();
        onEvent?.onmessage?.({ event: "hook_event", ...report.ui_events[0] });
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
      case "new_session":
        return {
          id: sessionId,
          created_at: "2026-09-19T10:15:00Z",
          characters: ["小雨"],
          persona: "夜读者",
          seed: 42,
        } satisfies SessionMeta;
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
        const turn = (messages[messages.length - 1]?.turn ?? 0) + 1;
        messages.push({ turn, role: "user", content: a.content!, ts: Math.floor(Date.now() / 1000) });
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
      default:
        throw new Error(`mock 未覆盖命令：${cmd}`);
    }
  });
}
