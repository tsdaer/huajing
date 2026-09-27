import type { Directive } from "vue";

// v-tilt（M5.2）：零依赖的 hover-3d 倾斜指令。
// pointermove 把倾斜角与高光位置写进 CSS 变量（--tilt-x/--tilt-y/--glow-x/--glow-y），
// 视觉由 style.css 的 .tilt-card 类承接——指令只算数，样式集中在全局层。
// prefers-reduced-motion 或粗指针（触屏）环境自禁用：动效是增强，缺席不损功能；
// prefers-reduced-motion 另有 style.css 的全局 transition 钳制兜底。

const MAX_DEG = 7;

/** 环境判定不缓存：挂载是低频事件，现查让「系统动效开关」的改动即时生效 */
function enabled(): boolean {
  return (
    typeof window !== "undefined" &&
    !window.matchMedia("(prefers-reduced-motion: reduce)").matches &&
    !window.matchMedia("(pointer: coarse)").matches
  );
}

function onMove(e: PointerEvent) {
  const el = e.currentTarget as HTMLElement;
  const r = el.getBoundingClientRect();
  if (r.width < 1 || r.height < 1) return;
  // 归一位置（0..1，左上原点）；px 右移 → 绕 Y 正转，py 下移 → 绕 X 反转：
  // 光标压向哪边，哪边微微下沉，跟手不突兀
  const px = (e.clientX - r.left) / r.width;
  const py = (e.clientY - r.top) / r.height;
  el.style.setProperty("--tilt-y", `${((px - 0.5) * 2 * MAX_DEG).toFixed(2)}deg`);
  el.style.setProperty("--tilt-x", `${((0.5 - py) * 2 * MAX_DEG).toFixed(2)}deg`);
  el.style.setProperty("--glow-x", `${(px * 100).toFixed(1)}%`);
  el.style.setProperty("--glow-y", `${(py * 100).toFixed(1)}%`);
}

function onLeave(e: PointerEvent) {
  const el = e.currentTarget as HTMLElement;
  el.style.setProperty("--tilt-x", "0deg");
  el.style.setProperty("--tilt-y", "0deg");
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
