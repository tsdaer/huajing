import { createApp } from "vue";
import App from "./App.vue";
import "./style.css";
import { applyVars, loadCustom } from "./theme";

// 界面主题：先用 daisyUI 默认主题（light / dark，留空即跟随系统），
// 再叠加「主题」页保存的自定义令牌（:root 内联变量优先于主题规则）。
const savedTheme = localStorage.getItem("huajing.ui-theme");
if (savedTheme === "light" || savedTheme === "dark") {
  document.documentElement.dataset.theme = savedTheme;
}

const savedCustom = loadCustom();
if (savedCustom) {
  applyVars(savedCustom.vars);
  document.documentElement.style.colorScheme = savedCustom.colorScheme;
}

// 纯浏览器调试（pnpm dev）时启用内存 mock；Tauri 环境走真实后端
if (!("__TAURI_INTERNALS__" in window)) {
  const { setupMock } = await import("./mock");
  setupMock();
}

createApp(App).mount("#app");
