/** 卡内界面事件（api.ui.emit）的 kind → 显示名。会话头徽标与事件流页签共用。 */
export const KIND_LABEL: Record<string, string> = {
  emotion: "表情",
  bgm: "音效",
  sprite: "立绘",
  effect: "特效",
};

export function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}
