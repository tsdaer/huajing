<script setup lang="ts">
import { onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

// 主题变量层从 M1 就预留（设计 §14 决策 4：IM 聊天风打底，galgame 皮肤后续叠加）
const info = ref<{
  name: string;
  slogan: string;
  version: string;
  dataRoot: string;
} | null>(null);

onMounted(async () => {
  info.value = await invoke("app_info");
});
</script>

<template>
  <main class="stage">
    <h1 class="title">化境</h1>
    <p class="slogan">{{ info?.slogan ?? "扮谁，便入谁之境。" }}</p>

    <section class="card" v-if="info">
      <div class="row"><span class="k">核心</span><span>Rust (Tauri 2) ↔ Vue 3 通道已打通</span></div>
      <div class="row"><span class="k">版本</span><span>v{{ info.version }} · M1 施工中</span></div>
      <div class="row"><span class="k">数据</span><span class="path">{{ info.dataRoot }}</span></div>
    </section>

    <p class="hint">
      M1 目标：1v1 流式对话 · Lua 角色卡热加载 · 场景快照注入 —— 见
      <code>docs/design.md §15</code>
    </p>
  </main>
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
</style>

<style scoped>
.stage {
  min-height: 100vh;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
  background: var(--hj-bg);
  color: var(--hj-fg);
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
}
.title {
  font-size: 64px;
  letter-spacing: 0.35em;
  margin: 0 0 0 0.35em;
  font-weight: 300;
  color: var(--hj-accent);
}
.slogan {
  color: var(--hj-dim);
  letter-spacing: 0.2em;
  margin: 0;
}
.card {
  margin-top: 18px;
  min-width: 420px;
  padding: 16px 20px;
  border-radius: 12px;
  background: var(--hj-panel);
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 14px;
}
.row {
  display: flex;
  gap: 12px;
  align-items: baseline;
}
.k {
  color: var(--hj-dim);
  flex: 0 0 3em;
}
.path {
  font-family: Consolas, monospace;
  font-size: 12px;
}
.hint {
  color: var(--hj-dim);
  font-size: 12px;
}
code {
  color: var(--hj-accent);
}
</style>
