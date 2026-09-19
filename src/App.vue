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
  --hj-bg: #14161a;
  --hj-panel: #1d2026;
  --hj-fg: #e8e6e3;
  --hj-dim: #9a9a94;
  --hj-accent: #d4a15e;
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
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
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
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
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
}
.nav button:hover {
  color: var(--hj-fg);
}
.nav button.active {
  color: var(--hj-fg);
  background: rgba(255, 255, 255, 0.08);
}
</style>
