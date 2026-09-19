<script setup lang="ts">
import { onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import type { Blackboard, Message, PromptAssembly, SessionMeta } from "../types";

// M1.4 会话详情：黑板手动编辑 + 记忆检查器 v0（逐层注入与 token）+ 消息只读。
// M1.5 在此之上补聊天输入与流式打字机。

const props = defineProps<{ meta: SessionMeta }>();

const error = ref("");
const blackboard = ref<Blackboard | null>(null);
const assembly = ref<PromptAssembly | null>(null);
const assemblySource = ref<"last" | "preview">("preview");
const messages = ref<Message[]>([]);
const savingBb = ref(false);

const bbForm = reactive({ day: 1, clock: "", place: "", actors: "" });

async function loadAll() {
  error.value = "";
  const id = props.meta.id;
  try {
    const [bb, msgs, last] = await Promise.all([
      api.getBlackboard(id),
      api.readMessages(id),
      api.lastPrompt(id),
    ]);
    blackboard.value = bb;
    Object.assign(bbForm, {
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors.join(", "),
    });
    messages.value = [...msgs];
    if (last) {
      assembly.value = last;
      assemblySource.value = "last";
    } else {
      await preview();
    }
  } catch (e) {
    error.value = String(e);
  }
}

async function preview() {
  try {
    assembly.value = await api.previewPrompt(props.meta.id);
    assemblySource.value = "preview";
  } catch (e) {
    error.value = String(e);
  }
}

async function saveBlackboard() {
  savingBb.value = true;
  try {
    const bb = await api.updateBlackboard(props.meta.id, {
      day: bbForm.day || 1,
      clock: bbForm.clock.trim(),
      place: bbForm.place.trim(),
      actors: bbForm.actors.split(/[,，]/).map((a) => a.trim()).filter(Boolean),
    });
    blackboard.value = bb;
    await preview(); // 现状卡随黑板变化，刷新检查器
  } catch (e) {
    error.value = String(e);
  } finally {
    savingBb.value = false;
  }
}

onMounted(loadAll);
watch(() => props.meta.id, loadAll);
</script>

<template>
  <div class="session">
    <p v-if="error" class="error">{{ error }}</p>

    <header class="head">
      <h2>{{ meta.characters.join(" × ") }}</h2>
      <span class="dim">
        第 {{ blackboard?.day ?? "?" }} 天 {{ blackboard?.clock || "时间未定" }} ·
        {{ blackboard?.place || "地点未定" }}
      </span>
    </header>

    <div class="cols">
      <!-- 黑板 v0：手动编辑 -->
      <section class="block">
        <header class="block-head">
          <h3>黑板</h3>
          <span class="dim">保存后下一轮组装生效；每轮回复后时钟 +10 分钟</span>
        </header>
        <div class="bbform">
          <div class="row2">
            <label class="field">
              <span>第几天</span>
              <input v-model.number="bbForm.day" type="number" min="1" />
            </label>
            <label class="field">
              <span>时间</span>
              <input v-model="bbForm.clock" placeholder="21:30" />
            </label>
          </div>
          <label class="field">
            <span>地点</span>
            <input v-model="bbForm.place" placeholder="图书馆自习区" />
          </label>
          <label class="field">
            <span>在场（逗号分隔）</span>
            <input v-model="bbForm.actors" placeholder="小雨, 玩家" />
          </label>
          <div class="form-ops">
            <button class="btn accent" :disabled="savingBb" @click="saveBlackboard">
              {{ savingBb ? "保存中…" : "保存黑板" }}
            </button>
          </div>
        </div>
      </section>

      <!-- 记忆检查器 v0 -->
      <section class="block">
        <header class="block-head">
          <h3>
            记忆检查器
            <span class="src">
              {{ assemblySource === "last" ? "（最近一次发送）" : "（预览·干跑）" }}
            </span>
          </h3>
          <button class="btn" @click="preview">刷新预览</button>
        </header>
        <p class="dim total" v-if="assembly">
          共 {{ assembly.layers.length }} 层 · 约 {{ assembly.total_tokens }} token（估算） ·
          {{ assembly.messages.length }} 条消息
        </p>
        <ul class="layers" v-if="assembly">
          <li v-for="l in assembly.layers" :key="l.id + l.name" class="layer">
            <details>
              <summary>
                <span class="lid">{{ l.id }}</span>
                <span class="lname">{{ l.name }}</span>
                <span class="ltok">≈{{ l.tokens }}</span>
              </summary>
              <pre>{{ l.content }}</pre>
            </details>
          </li>
        </ul>
        <p class="dim" v-else>尚无组装数据。</p>
      </section>
    </div>

    <!-- 消息（只读；聊天输入随 M1.5） -->
    <section class="block msgs">
      <header class="block-head">
        <h3>消息</h3>
        <span class="dim">{{ messages.length }} 条</span>
      </header>
      <ul class="mlist" v-if="messages.length > 0">
        <li v-for="(m, i) in messages" :key="i" :class="m.role">
          <div class="who">{{ m.role === "user" ? "我" : meta.characters[0] }}</div>
          <div class="bubble">{{ m.content }}</div>
        </li>
      </ul>
      <p class="dim" v-else>还没有消息。开场白与聊天输入在 M1.5 上线。</p>
    </section>
  </div>
</template>

<style scoped>
.session {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.error {
  margin: 0;
  padding: 10px 14px;
  border-radius: 8px;
  background: rgba(200, 80, 80, 0.15);
  color: #e0a0a0;
  font-size: 13px;
}
.head {
  display: flex;
  align-items: baseline;
  gap: 12px;
}
.head h2 {
  margin: 0;
  font-size: 18px;
}
.dim {
  color: var(--hj-dim);
  font-size: 12px;
}
.cols {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px;
  align-items: start;
}
.block {
  background: var(--hj-panel);
  border-radius: 12px;
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.block-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.block-head h3 {
  margin: 0;
  font-size: 14px;
}
.src {
  color: var(--hj-dim);
  font-size: 11px;
  font-weight: 400;
}
.total {
  margin: 0;
}

.bbform {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.row2 {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--hj-dim);
}
.field input {
  background: var(--hj-bg);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  color: var(--hj-fg);
  padding: 6px 10px;
  font-size: 13px;
}
.form-ops {
  display: flex;
  justify-content: flex-end;
}

.layers {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 320px;
  overflow-y: auto;
}
.layer summary {
  display: flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  padding: 6px 10px;
  border-radius: 6px;
  background: var(--hj-bg);
  font-size: 13px;
  list-style: none;
}
.layer summary::-webkit-details-marker {
  display: none;
}
.layer summary::before {
  content: "▸";
  color: var(--hj-dim);
  font-size: 11px;
}
.layer details[open] summary::before {
  content: "▾";
}
.lid {
  font-family: Consolas, monospace;
  color: var(--hj-accent);
  font-size: 12px;
}
.lname {
  flex: 1;
}
.ltok {
  color: var(--hj-dim);
  font-size: 11px;
  font-family: Consolas, monospace;
}
.layer pre {
  margin: 6px 0 2px;
  padding: 10px;
  border-radius: 6px;
  background: var(--hj-bg);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 240px;
  overflow-y: auto;
}

.msgs .mlist {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
  max-height: 40vh;
  overflow-y: auto;
}
.mlist li {
  display: flex;
  flex-direction: column;
  max-width: 72%;
}
.mlist li.user {
  align-self: flex-end;
  align-items: flex-end;
}
.mlist li.char {
  align-self: flex-start;
}
.who {
  font-size: 11px;
  color: var(--hj-dim);
  margin-bottom: 3px;
}
.bubble {
  padding: 8px 12px;
  border-radius: 12px;
  background: var(--hj-bg);
  font-size: 13px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
}
.mlist li.user .bubble {
  background: rgba(212, 161, 94, 0.16);
}

@media (max-width: 700px) {
  .cols {
    grid-template-columns: 1fr;
  }
}
</style>
