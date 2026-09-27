import type { Directive } from "vue";

// v-tilt（M5.2，体验修复批二次修订）：零依赖的 hover-3d 倾斜指令，对齐 daisyUI
// hover-3d 组件的观感——光标压向哪边，哪边微微抬起迎向手指（与 daisyUI 的
// 分区 rotate3d 同号），配合 .tilt-card 的抬升/方向投影/跟手高光成一套。
// 不直接用 daisyUI 纯 CSS 版的原因：它的 8 个透明分区 div 会盖住卡片内按钮
// （官方文档也要求内容不可交互），而角色卡上恰有详情/开一场/导出三枚按钮。
// pointermove 把倾斜角、高光与投影位置写进 CSS 变量（--tilt-x/--tilt-y/
// --glow-x/--glow-y/--shadow-x/--shadow-y），视觉由 style.css 的 .tilt-card 承接。
// 粗指针（触屏）环境自禁用：没有 hover 语义。reduced-motion 不再禁用本体——
// 真机走查（temp/m51）实锤本机 WebView2 即报 reduce，禁用等于功能缺席；
// 改由 style.css 的全局 transition 钳制兜底：动效敏感用户看到的是无动画的
// 指针空间反馈（倾斜/高光即时跟随，无弹簧、无抬升过渡），而非效果整体消失。

const MAX_DEG = 10;
/** 投影最大位移（px）：光标越靠边，悬浮感越强 */
const MAX_SHADOW_PX = 8;

/** 环境判定不缓存：挂载是低频事件，现查让「系统指针形态」的改动即时生效 */
function enabled(): boolean {
  return (
    typeof window !== "undefined" &&
    !window.matchMedia("(pointer: coarse)").matches
  );
}

function onMove(e: PointerEvent) {
  const el = e.currentTarget as HTMLElement;
  const r = el.getBoundingClientRect();
  if (r.width < 1 || r.height < 1) return;
  // 归一位置（0..1，左上原点）；px 右移 → 绕 Y 反转，py 下移 → 绕 X 正转：
  // 光标压向哪边，哪边抬起迎向手指（daisyUI hover-3d 同号），投影落向反侧
  const px = (e.clientX - r.left) / r.width;
  const py = (e.clientY - r.top) / r.height;
  el.style.setProperty("--tilt-y", `${((0.5 - px) * 2 * MAX_DEG).toFixed(2)}deg`);
  el.style.setProperty("--tilt-x", `${((py - 0.5) * 2 * MAX_DEG).toFixed(2)}deg`);
  el.style.setProperty("--glow-x", `${(px * 100).toFixed(1)}%`);
  el.style.setProperty("--glow-y", `${(py * 100).toFixed(1)}%`);
  el.style.setProperty("--shadow-x", `${((px - 0.5) * 2 * MAX_SHADOW_PX).toFixed(1)}px`);
  el.style.setProperty("--shadow-y", `${((py - 0.5) * 2 * MAX_SHADOW_PX).toFixed(1)}px`);
}

function onLeave(e: PointerEvent) {
  const el = e.currentTarget as HTMLElement;
  el.style.setProperty("--tilt-x", "0deg");
  el.style.setProperty("--tilt-y", "0deg");
  el.style.setProperty("--shadow-x", "0px");
  el.style.setProperty("--shadow-y", "0px");
}

/** v-tilt：挂载即接管 hover-3d；传 false 显式关闭（如占位/禁用态卡片） */
export const vTilt: Directive<HTMLElement, boolean | undefined> = {
  mounted(el, binding) {
    if (!enabled() || binding.value === false) return;
    el.classList.add("tilt-card");
    el.addEventListener("pointermove", onMove);
    el.addEventListener("pointerleave", onLeave);
  },
  unmounted(el) {
    el.removeEventListener("pointermove", onMove);
    el.removeEventListener("pointerleave", onLeave);
  },
};
