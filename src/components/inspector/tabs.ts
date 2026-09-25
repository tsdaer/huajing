/** 检查器页签定义（会话视图持有当前页签，抽屉渲染页签条——共享这份清单） */

export const INSP_TABS = [
  { id: "layers", label: "注入层" },
  { id: "statetree", label: "状态路径" },
  { id: "threads", label: "剧情线" },
  { id: "psyche", label: "心理" },
  { id: "palace", label: "宫殿" },
  { id: "codex", label: "设定集" },
  { id: "outbox", label: "摘要·收件箱" },
  { id: "world", label: "世界" },
  { id: "director", label: "导演" },
  { id: "state", label: "卡内状态" },
  { id: "memory", label: "卡内记忆" },
  { id: "events", label: "事件流" },
] as const;

export type InspTab = (typeof INSP_TABS)[number]["id"];

/** M2.8 面板组：共享一份 inspectorData（注入层仍走 preview/last_prompt） */
export const M2_TABS: InspTab[] = ["statetree", "threads", "psyche", "palace", "codex", "outbox"];
