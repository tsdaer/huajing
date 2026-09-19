// daisyUI 预设主题：来自 docs/theme_test/theme.css（以 ?raw 字符串内联，运行时解析）。
// 单独成模块是为了让这段字符串只随「主题」页一起按需加载，不进首屏包。

import rawPresets from "../docs/theme_test/theme.css?raw";
import type { ThemePreset } from "./theme";

/** 解析 @plugin "daisyui/theme" 块；跳过其它内容 */
export function parsePresets(css: string): ThemePreset[] {
  const out: ThemePreset[] = [];
  for (const block of css.matchAll(/@plugin\s+"daisyui\/theme"\s*\{([\s\S]*?)\n\}/g)) {
    const body = block[1];
    const name = /name:\s*"([^"]+)"/.exec(body)?.[1];
    if (!name) continue;
    const colorScheme = /color-scheme:\s*"?dark"?/.test(body) ? "dark" : "light";
    const vars: Record<string, string> = {};
    for (const v of body.matchAll(/(--[a-z0-9-]+):\s*([^;]+);/g)) vars[v[1]] = v[2].trim();
    out.push({ name, colorScheme, vars });
  }
  return out;
}

export const PRESETS: ThemePreset[] = parsePresets(rawPresets);
