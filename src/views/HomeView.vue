<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";
import type { CardSummary } from "../types";

const info = ref<Awaited<ReturnType<typeof api.appInfo>> | null>(null);
const cards = ref<CardSummary[]>([]);

onMounted(async () => {
  info.value = await api.appInfo();
  try {
    cards.value = await api.listCards();
  } catch {
    cards.value = []; // 卡目录为空或读取失败时静默
  }
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

    <section class="card" v-if="cards.length > 0">
      <div class="row head"><span class="k">角色卡</span><span>{{ cards.length }} 张已装载</span></div>
      <div class="row" v-for="c in cards" :key="c.dir_name">
        <span class="k">{{ c.name }}</span>
        <span class="tags">
          <span v-if="c.degraded" class="tag warn">降级</span>
          <span v-if="c.has_hooks" class="tag">hooks</span>
          <span v-for="t in c.tags" :key="t" class="tag">{{ t }}</span>
        </span>
      </div>
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
.row.head {
  padding-bottom: 4px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.tags {
  display: inline-flex;
  gap: 6px;
  flex-wrap: wrap;
}
.tag {
  font-size: 11px;
  padding: 1px 8px;
  border-radius: 999px;
  background: rgba(212, 161, 94, 0.14);
  color: var(--hj-accent);
}
.tag.warn {
  background: rgba(200, 90, 90, 0.2);
  color: #d89090;
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
