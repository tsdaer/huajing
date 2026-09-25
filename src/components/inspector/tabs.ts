/** 检查器页签定义（会话视图持有当前页签，抽屉渲染页签条——共享这份清单） */

export const INSP_TABS = [
  { id: "layers", label: "注入层", icon: "layers" },
  { id: "statetree", label: "状态路径", icon: "split" },
  { id: "threads", label: "剧情线", icon: "bolt" },
  { id: "psyche", label: "心理", icon: "pulse" },
  { id: "palace", label: "宫殿", icon: "home" },
  { id: "codex", label: "设定集", icon: "book" },
  { id: "outbox", label: "摘要·收件箱", icon: "copy" },
  { id: "world", label: "世界", icon: "globe" },
  { id: "director", label: "导演", icon: "film" },
  { id: "state", label: "卡内状态", icon: "cpu" },
  { id: "memory", label: "卡内记忆", icon: "database" },
  { id: "events", label: "事件流", icon: "sparkle" },
] as const;

export type InspTab = (typeof INSP_TABS)[number]["id"];

/** M2.8 面板组：共享一份 inspectorData（注入层仍走 preview/last_prompt） */
export const M2_TABS: InspTab[] = ["statetree", "threads", "psyche", "palace", "codex", "outbox"];
