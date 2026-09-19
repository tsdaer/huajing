// daisyUI 主题令牌层。
// 预设来自 docs/theme_test/theme.css（以字符串内联，运行时解析，不加进编译产物）；
// 自定义主题以 :root 内联变量覆盖当前主题，导出仍是标准 @plugin "daisyui/theme" 块。

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

export function loadCustom(): CustomTheme | null {
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

export function saveCustom(theme: CustomTheme): void {
  localStorage.setItem(CUSTOM_KEY, JSON.stringify(theme));
}

export function removeCustom(): void {
  localStorage.removeItem(CUSTOM_KEY);
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
