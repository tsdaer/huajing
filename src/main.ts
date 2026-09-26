import { createApp } from "vue";
import App from "./App.vue";
import "./style.css";
import { applyVars, initCustomTheme } from "./theme";

// 界面主题：先用 daisyUI 默认主题（light / dark，留空即跟随系统），
// 再叠加「主题」页保存的自定义令牌（:root 内联变量优先于主题规则）。
const savedTheme = localStorage.getItem("huajing.ui-theme");
if (savedTheme === "light" || savedTheme === "dark") {
  document.documentElement.dataset.theme = savedTheme;
}

// 纯浏览器调试（pnpm dev）时启用内存 mock；Tauri 环境走真实后端。
// mock 必须先于主题加载就位——浏览器模式下自定义主题也从 mock 后端读。
if (!("__TAURI_INTERNALS__" in window)) {
  const { setupMock } = await import("./mock");
  setupMock();
}

// 自定义主题落 DataHub/themes/custom.json（M4.3 · 决断 7），挂载前加载避免主题闪变；
// 后端没有而 localStorage 有旧值时在这里自动迁移。
const savedCustom = await initCustomTheme();
if (savedCustom) {
  applyVars(savedCustom.vars);
  document.documentElement.style.colorScheme = savedCustom.colorScheme;
}

createApp(App).mount("#app");
