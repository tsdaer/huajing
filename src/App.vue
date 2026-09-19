<script setup lang="ts">
import { ref } from "vue";
import HomeView from "./views/HomeView.vue";
import SessionsView from "./views/SessionsView.vue";
import SettingsView from "./views/SettingsView.vue";

// 壳 + 顶层导航。路由细化（单会话地址等）随 M1.5 评估，此处先三页切换。
const view = ref<"home" | "sessions" | "settings">("home");
</script>

<template>
  <div class="app">
    <header class="topbar">
      <span class="brand">化境</span>
      <nav class="nav">
        <button :class="{ active: view === 'home' }" @click="view = 'home'">首页</button>
        <button :class="{ active: view === 'sessions' }" @click="view = 'sessions'">会话</button>
        <button :class="{ active: view === 'settings' }" @click="view = 'settings'">设置</button>
      </nav>
    </header>

    <HomeView v-if="view === 'home'" />
    <SessionsView v-else-if="view === 'sessions'" />
    <SettingsView v-else />
  </div>
</template>

<style>
:root {
  /* 主题变量层：换肤只改这里（IM 风 / galgame 风） */
  --hj-bg: #131518;
  --hj-panel: #1c1f25;
  --hj-panel-2: #242832;
  --hj-fg: #e9e6e0;
  --hj-dim: #98958c;
  --hj-accent: #d4a15e;
  --hj-accent-soft: rgba(212, 161, 94, 0.16);
  --hj-accent-ink: #231b10;
  --hj-line: rgba(233, 230, 224, 0.09);
  --hj-line-strong: rgba(233, 230, 224, 0.16);
  --hj-danger: #c96f6f;
}

* {
  box-sizing: border-box;
}

html,
body,
#app {
  height: 100%;
  margin: 0;
}

body {
  background: var(--hj-bg);
  color: var(--hj-fg);
  font-family: "Segoe UI", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif;
  -webkit-font-smoothing: antialiased;
}

button,
input,
textarea,
select {
  font: inherit;
}

::selection {
  background: rgba(212, 161, 94, 0.32);
}

:focus-visible {
  outline: 2px solid var(--hj-accent);
  outline-offset: 2px;
}

/* 细滚动条，融进底色 */
* {
  scrollbar-width: thin;
  scrollbar-color: rgba(233, 230, 224, 0.18) transparent;
}
*::-webkit-scrollbar {
  width: 8px;
  height: 8px;
}
*::-webkit-scrollbar-thumb {
  background: rgba(233, 230, 224, 0.16);
  border-radius: 4px;
}
*::-webkit-scrollbar-track {
  background: transparent;
}

/* 全局共享控件：各视图共用的按钮与错误条 */
.btn {
  border: 1px solid var(--hj-line-strong);
  background: transparent;
  color: var(--hj-fg);
  border-radius: 8px;
  padding: 5px 14px;
  font-size: 13px;
  cursor: pointer;
  transition: border-color 0.15s, filter 0.15s, color 0.15s, background 0.15s;
}
.btn:hover {
  border-color: var(--hj-accent);
}
.btn:disabled {
  opacity: 0.45;
  cursor: default;
}
.btn.accent {
  background: var(--hj-accent);
  border-color: var(--hj-accent);
  color: var(--hj-accent-ink);
  font-weight: 600;
}
.btn.accent:hover:not(:disabled) {
  filter: brightness(1.08);
}
.btn.danger:hover {
  border-color: var(--hj-danger);
  color: var(--hj-danger);
}

.error {
  padding: 10px 14px;
  border-radius: 8px;
  background: rgba(201, 111, 111, 0.14);
  color: #e2a9a9;
  font-size: 13px;
}

@media (prefers-reduced-motion: reduce) {
  *,
  *::before,
  *::after {
    animation-duration: 0.01ms !important;
    transition-duration: 0.01ms !important;
  }
}
</style>

<style scoped>
.app {
  height: 100%;
  display: flex;
  flex-direction: column;
}
.topbar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 24px;
  padding: 10px 20px;
  background: var(--hj-panel);
  border-bottom: 1px solid var(--hj-line);
}
.brand {
  color: var(--hj-accent);
  font-weight: 300;
  letter-spacing: 0.3em;
  margin-right: 0.3em;
}
.nav {
  display: flex;
  gap: 4px;
}
.nav button {
  border: none;
  background: transparent;
  color: var(--hj-dim);
  font-size: 13px;
  padding: 6px 14px;
  border-radius: 8px;
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}
.nav button:hover {
  color: var(--hj-fg);
}
.nav button.active {
  color: var(--hj-accent);
  background: var(--hj-accent-soft);
}
</style>
