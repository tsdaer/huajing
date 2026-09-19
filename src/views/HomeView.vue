<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);

onMounted(async () => {
  info.value = await api.appInfo();
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

<style scoped>
.stage {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
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
