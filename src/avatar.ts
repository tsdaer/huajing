// 头像工具（M5.1 从 SessionView 提出共享）：气泡与舞台栏同一套稳定取色，
// 同一角色在聊天流和阵容栏永远同色，多人同台一眼可分。

/** 名字首字（空名兜底 "?"） */
export function initial(name: string): string {
  return name.trim().slice(0, 1) || "?";
}

/** 角色气泡的稳定颜色（M3.4 群聊：按署名取色，同一角色恒同色）。
 *  色相由名字哈希决定，底色/字色与主题令牌 color-mix——明暗与 33 预设下都协调，不裸写白字 */
export function avatarStyle(name: string): string {
  let h = 0;
  for (const ch of name) h = (h * 31 + (ch.codePointAt(0) ?? 0)) % 360;
  const hue = `hsl(${h} 70% 55%)`;
  return (
    `background: color-mix(in oklab, ${hue} 26%, var(--color-base-200));` +
    ` color: color-mix(in oklab, ${hue} 58%, var(--color-base-content))`
  );
}
