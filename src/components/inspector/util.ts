// 记忆检查器面板共用的小工具（纯展示层格式化，不做业务判断）
import type { InspectorMemory } from "../../types";

/** 故事时刻：story_clock 已含「第N天」时原样用，否则拼上 story_day（与 palace.rs 的 story_stamp 同口径） */
export function storyStamp(m: Pick<InspectorMemory, "story_day" | "story_clock">): string {
  const clock = (m.story_clock ?? "").trim();
  if (!clock) return `第${m.story_day}天`;
  if (clock.startsWith("第")) return clock;
  return `第${m.story_day}天 ${clock}`;
}

/** 克制梯度（设计 §8.4）的中文标签 */
export function gradeLabel(grade: string): string {
  if (grade === "dormant") return "深埋";
  if (grade === "eager") return "急切";
  if (grade === "natural") return "自然";
  return grade || "未定";
}

/** 克制梯度的徽标配色：越急越显眼 */
export function gradeClass(grade: string): string {
  if (grade === "eager") return "badge-soft badge-warning";
  if (grade === "dormant") return "badge-ghost";
  return "badge-soft badge-info";
}

/** 实体生命周期（设计 §6.9）：草稿 / 正史 / 已废止 */
export function statusLabel(status: string): string {
  if (status === "canon") return "正史";
  if (status === "draft") return "草稿";
  if (status === "retired") return "已废止";
  return status || "未定";
}

export function statusClass(status: string): string {
  if (status === "canon") return "badge-soft badge-success";
  if (status === "draft") return "badge-soft badge-warning";
  if (status === "retired") return "badge-soft badge-neutral";
  return "badge-ghost";
}

/** 提案类型（event.rs · ProposalEvent.kind）的中文标签 */
export function proposalKind(kind: string): string {
  const map: Record<string, string> = {
    new_entity: "新实体",
    new_fact: "新事实",
    fact_change: "事实变更",
    relation: "关系",
    episode: "情景记忆",
    thread: "剧情线",
    psyche: "心理评价",
  };
  return map[kind] ?? (kind || "提案");
}

/** 提案状态 */
export function proposalStatus(status: string): string {
  if (status === "propose") return "待处理";
  if (status === "accept") return "已确认";
  if (status === "reject") return "已否决";
  return status || "未知";
}

/** 提案 payload 的单行摘要（对象压成 JSON；异常一律降级成字符串） */
export function payloadBrief(payload: unknown): string {
  if (payload === null || payload === undefined) return "";
  if (typeof payload === "string") return payload;
  try {
    return JSON.stringify(payload);
  } catch {
    return String(payload);
  }
}

/** 情绪强度 / 显著度的百分比（0–1 → 0–100，顺手挡住坏值） */
export function pct(v: number): number {
  if (!Number.isFinite(v)) return 0;
  return Math.round(Math.min(Math.max(v, 0), 1) * 100);
}

/** 两位小数的强度显示 */
export function fixed2(v: number): string {
  return Number.isFinite(v) ? v.toFixed(2) : "-";
}
