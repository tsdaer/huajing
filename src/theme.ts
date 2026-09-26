// daisyUI 主题令牌层。
// 预设来自 docs/theme_test/theme.css（以字符串内联，运行时解析，不加进编译产物）；
// 自定义主题以 :root 内联变量覆盖当前主题，导出仍是标准 @plugin "daisyui/theme" 块。
// 持久化（M4.3 · 决断 7）：自定义主题落 DataHub/themes/<名>.json（拷走 DataHub 即
// 主题跟走），localStorage 只作旧值的迁移源。

import { api } from "./api";

export interface ThemePreset {
  name: string;
  colorScheme: "light" | "dark";
  vars: Record<string, string>;
}

export interface CustomTheme {
  preset: string;
  colorScheme: "light" | "dark";
  vars: Record<string, string>;
}

/** 主题块导入的解析结果（后端 parse_theme_import 的返回） */
export interface ParsedThemeImport {
  name: string;
  colorScheme: string;
  vars: Record<string, string>;
}

export type TokenKind = "color" | "value";

export interface TokenDef {
  key: string;
  label: string;
  kind: TokenKind;
  hint?: string;
}

export interface TokenGroup {
  title: string;
  hint: string;
  tokens: TokenDef[];
}

const color = (key: string, hint?: string): TokenDef => ({ key: `--color-${key}`, label: key, kind: "color", hint });

/** 编辑器分组：顺序即界面顺序 */
export const TOKEN_GROUPS: TokenGroup[] = [
  {
    title: "基础色",
    hint: "页面、卡片、边框三层底色与正文色",
    tokens: [color("base-100"), color("base-200"), color("base-300"), color("base-content")],
  },
  {
    title: "品牌色",
    hint: "primary 是页面最重要的动作色，尽量只用在一处",
    tokens: [
      color("primary"),
      color("primary-content"),
      color("secondary"),
      color("secondary-content"),
      color("accent"),
      color("accent-content"),
    ],
  },
  {
    title: "中性色与状态色",
    hint: "neutral 用于低饱和区域，info/success/warning/error 用于状态反馈",
    tokens: [
      color("neutral"),
      color("neutral-content"),
      color("info"),
      color("info-content"),
      color("success"),
      color("success-content"),
      color("warning"),
      color("warning-content"),
      color("error"),
      color("error-content"),
    ],
  },
  {
    title: "形状与质感",
    hint: "圆角、控件基础尺寸、边框粗细、立体感与噪点",
    tokens: [
      { key: "--radius-selector", label: "radius-selector", kind: "value", hint: "badge/checkbox 等选择器" },
      { key: "--radius-field", label: "radius-field", kind: "value", hint: "按钮/输入框等字段" },
      { key: "--radius-box", label: "radius-box", kind: "value", hint: "卡片/弹窗等容器" },
      { key: "--size-selector", label: "size-selector", kind: "value" },
      { key: "--size-field", label: "size-field", kind: "value" },
      { key: "--border", label: "border", kind: "value" },
      { key: "--depth", label: "depth", kind: "value", hint: "0 或 1" },
      { key: "--noise", label: "noise", kind: "value", hint: "0 或 1" },
    ],
  },
];

export const ALL_KEYS = TOKEN_GROUPS.flatMap((g) => g.tokens.map((t) => t.key));
export const COLOR_KEYS = TOKEN_GROUPS.flatMap((g) => g.tokens.filter((t) => t.kind === "color").map((t) => t.key));

export const CUSTOM_KEY = "huajing.theme.custom";
export const BASE_KEY = "huajing.ui-theme";
/** 当前生效的自定义主题在 DataHub/themes/ 下的固定文件 stem（后端同款常量） */
export const ACTIVE_STEM = "custom";

/** 写入 :root 内联变量（内联样式优先于主题规则，因此能覆盖 light/dark） */
export function applyVars(vars: Record<string, string>): void {
  const root = document.documentElement;
  for (const [key, value] of Object.entries(vars)) {
    if (value) root.style.setProperty(key, value);
    else root.style.removeProperty(key);
  }
}

/** 清掉本编辑器写过的全部变量，回到基础主题 */
export function clearVars(): void {
  const root = document.documentElement;
  for (const key of ALL_KEYS) root.style.removeProperty(key);
  root.style.removeProperty("color-scheme");
}

// ---------- 持久化：后端 themes/custom.json 是唯一事实源，localStorage 仅作迁移源 ----------

let activeCustom: CustomTheme | null = null;

/** 当前生效的自定义主题（启动时由 initCustomTheme 填充，之后与编辑器同步） */
export function getActiveCustom(): CustomTheme | null {
  return activeCustom;
}

function fromBackend(raw: Awaited<ReturnType<typeof api.themeLoad>>): CustomTheme | null {
  if (!raw || typeof raw !== "object" || !raw.vars) return null;
  return {
    preset: raw.preset ?? "",
    colorScheme: raw.colorScheme === "dark" ? "dark" : "light",
    vars: raw.vars,
  };
}

/** 旧版 localStorage 值（M4.3 之前自定义主题只存这里） */
function readLegacy(): CustomTheme | null {
  try {
    const raw = localStorage.getItem(CUSTOM_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as CustomTheme;
    if (!parsed || typeof parsed !== "object" || !parsed.vars) return null;
    return { preset: parsed.preset ?? "", colorScheme: parsed.colorScheme === "dark" ? "dark" : "light", vars: parsed.vars };
  } catch {
    return null;
  }
}

/**
 * 启动加载（main.ts 挂载前 await，避免主题闪变）：读后端活动主题；
 * 后端没有而 localStorage 有旧值 → 自动迁移（写后端成功才清旧键，失败下次再试）。
 */
export async function initCustomTheme(): Promise<CustomTheme | null> {
  let theme: CustomTheme | null = null;
  try {
    theme = fromBackend(await api.themeLoad(ACTIVE_STEM));
  } catch {
    theme = null;
  }
  if (!theme) {
    const legacy = readLegacy();
    if (legacy) {
      try {
        await api.themeSave(ACTIVE_STEM, legacy);
        localStorage.removeItem(CUSTOM_KEY);
        theme = legacy;
      } catch {
        // 迁移失败不阻塞启动：旧键保留，下次启动再试
      }
    }
  }
  activeCustom = theme;
  return theme;
}

let saveTimer: ReturnType<typeof setTimeout> | undefined;

/** 编辑器实时改动落后端（防抖——每个键入都写盘就太吵了） */
export function persistCustom(theme: CustomTheme): void {
  activeCustom = theme;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api.themeSave(ACTIVE_STEM, theme).catch(() => {});
  }, 400);
}

/** 清除活动主题：删后端文件 + 旧键彻底退役（否则下次启动又被迁移回来） */
export function clearCustom(): void {
  activeCustom = null;
  if (saveTimer) clearTimeout(saveTimer);
  localStorage.removeItem(CUSTOM_KEY);
  api.themeDelete(ACTIVE_STEM).catch(() => {});
}

/** 生成可直接粘进 style.css 的主题块 */
export function toPluginCss(vars: Record<string, string>, name = "custom", colorScheme: "light" | "dark" = "light"): string {
  const lines = ALL_KEYS.filter((key) => vars[key]).map((key) => `  ${key}: ${vars[key]};`);
  const extra = Object.keys(vars)
    .filter((key) => !ALL_KEYS.includes(key))
    .map((key) => `  ${key}: ${vars[key]};`);
  return [
    '@plugin "daisyui/theme" {',
    `  name: "${name}";`,
    "  default: false;",
    "  prefersdark: false;",
    `  color-scheme: ${colorScheme};`,
    ...[...lines, ...extra],
    "}",
  ].join("\n");
}

let probe: HTMLElement | null = null;

/** 用浏览器自己的色彩引擎把任意 CSS 颜色（含 oklch）折算成 #rrggbb，供原生取色器使用 */
export function cssColorToHex(value: string | undefined): string {
  if (!value) return "#000000";
  if (/^#[0-9a-f]{6}$/i.test(value)) return value;
  if (!probe) {
    probe = document.createElement("div");
    probe.style.display = "none";
    document.body.appendChild(probe);
  }
  probe.style.color = value;
  const computed = getComputedStyle(probe).color;
  const m = /rgba?\(([^)]+)\)/.exec(computed);
  if (!m) return "#000000";
  const [r, g, b] = m[1].split(",").map((n) => Number.parseFloat(n));
  const hex = (n: number) => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
  return `#${hex(r)}${hex(g)}${hex(b)}`;
}
