// 自定义标题栏的窗口控制。
// 浏览器调试（无 Tauri 运行时）时全部降级为无操作，界面照常渲染。

import { getCurrentWindow } from "@tauri-apps/api/window";

export const isTauri = "__TAURI_INTERNALS__" in window;

export async function minimizeWindow(): Promise<void> {
  if (isTauri) await getCurrentWindow().minimize();
}

export async function toggleMaximizeWindow(): Promise<void> {
  if (isTauri) await getCurrentWindow().toggleMaximize();
}

export async function closeWindow(): Promise<void> {
  if (isTauri) await getCurrentWindow().close();
}

export async function isWindowMaximized(): Promise<boolean> {
  return isTauri ? await getCurrentWindow().isMaximized() : false;
}

/** 监听窗口尺寸/最大化变化；返回取消订阅函数 */
export async function onWindowResized(handler: () => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return await getCurrentWindow().onResized(handler);
}
