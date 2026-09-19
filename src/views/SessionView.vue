<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import type { Blackboard, Message, PromptAssembly, SessionMeta, StreamEvent } from "../types";

// M1.5 聊天界面：气泡流 + 流式打字机 + 停止 + 消息编辑/重roll/删除。
// 黑板与记忆检查器收进右侧抽屉，聊天流为主。

const props = defineProps<{ meta: SessionMeta }>();

const error = ref("");
const messages = ref<Message[]>([]);
const blackboard = ref<Blackboard | null>(null);
const assembly = ref<PromptAssembly | null>(null);
const assemblySource = ref<"last" | "preview">("preview");
const cardName = ref(props.meta.characters[0] ?? "角色");

const panel = ref<"" | "board" | "inspector">("");
const bbForm = reactive({ day: 1, clock: "", place: "", actors: "" });
const savingBb = ref(false);

const draft = ref("");
const generating = ref(false);
const streamText = ref("");
const editingIndex = ref(-1);
const editDraft = ref("");

const streamEl = ref<HTMLElement | null>(null);
const composerEl = ref<HTMLTextAreaElement | null>(null);

const lastIndex = computed(() => messages.value.length - 1);

function whoFor(m: Message): string {
  if (m.role === "user") return props.meta.persona || "我";
  if (m.role === "char") return cardName.value;
  return "系统";
}

/** 重roll 仅对"跟在用户消息后的末尾角色回复"开放 */
function canReroll(i: number, m: Message): boolean {
  return (
    !generating.value &&
    i === lastIndex.value &&
    m.role === "char" &&
    messages.value[i - 1]?.role === "user"
  );
}

function togglePanel(p: "board" | "inspector") {
  panel.value = panel.value === p ? "" : p;
}

async function scrollToBottom() {
  await nextTick();
  const el = streamEl.value;
  if (el) el.scrollTop = el.scrollHeight;
}

async function loadAll() {
  error.value = "";
  generating.value = false;
  streamText.value = "";
  editingIndex.value = -1;
  draft.value = "";
  const id = props.meta.id;
  try {
    const [bb, msgs] = await Promise.all([api.getBlackboard(id), api.readMessages(id)]);
    blackboard.value = bb;
    Object.assign(bbForm, {
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors.join(", "),
    });
    messages.value = [...msgs];
    void refreshInspector();
    void scrollToBottom();
    // 气泡署名用卡片显示名；读取失败退回目录名
    api
      .getCard(props.meta.characters[0])
      .then((d) => {
        if (d.card.name) cardName.value = d.card.name;
      })
      .catch(() => {});
  } catch (e) {
    error.value = String(e);
  }
}

/** 检查器：优先"最近一次发送"，无记录时干跑预览 */
async function refreshInspector() {
  try {
    const last = await api.lastPrompt(props.meta.id);
    if (last) {
      assembly.value = last;
      assemblySource.value = "last";
    } else {
      assembly.value = await api.previewPrompt(props.meta.id);
      assemblySource.value = "preview";
    }
  } catch {
    /* 检查器读取失败不阻塞聊天 */
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
      actors: bbForm.actors
        .split(/[,，]/)
        .map((a) => a.trim())
        .filter(Boolean),
    });
    blackboard.value = bb;
    await refreshInspector(); // 场景快照随黑板变化
  } catch (e) {
    error.value = String(e);
  } finally {
    savingBb.value = false;
  }
}

// ---------- 发送 / 重roll / 停止 ----------

async function send() {
  const content = draft.value.trim();
  if (!content || generating.value) return;
  error.value = "";
  draft.value = "";
  resetComposerHeight();
  generating.value = true;
  streamText.value = "";
  // 乐观上屏；终态后以磁盘为准重读
  messages.value = [...messages.value, { turn: -1, role: "user", content, ts: 0 }];
  void scrollToBottom();
  let final: StreamEvent;
  try {
    final = await api.sendMessage(props.meta.id, content, onDelta);
  } catch (e) {
    final = { event: "error", message: String(e) };
  }
  await finishGeneration(final);
}

async function reroll() {
  if (generating.value) return;
  error.value = "";
  generating.value = true;
  streamText.value = "";
  let final: StreamEvent;
  try {
    final = await api.regenerate(props.meta.id, onDelta);
  } catch (e) {
    final = { event: "error", message: String(e) };
  }
  await finishGeneration(final);
}

function onDelta(e: StreamEvent) {
  if (e.event === "delta") {
    streamText.value += e.text;
    void scrollToBottom();
  }
}

/** 生成收尾：错误上报、消息与黑板重读（时钟已步进）、检查器刷新 */
async function finishGeneration(final: StreamEvent) {
  if (final.event === "error") error.value = final.message;
  try {
    const [msgs, bb] = await Promise.all([
      api.readMessages(props.meta.id),
      api.getBlackboard(props.meta.id),
    ]);
    messages.value = [...msgs];
    blackboard.value = bb;
    Object.assign(bbForm, {
      day: bb.day,
      clock: bb.clock,
      place: bb.place,
      actors: bb.actors.join(", "),
    });
  } catch (e) {
    error.value = String(e);
  }
  generating.value = false;
  streamText.value = "";
  void refreshInspector();
  void scrollToBottom();
  composerEl.value?.focus();
}

async function stop() {
  await api.stopGeneration(props.meta.id);
}

// ---------- 消息编辑 / 删除 ----------

function startEdit(i: number) {
  editingIndex.value = i;
  editDraft.value = messages.value[i].content;
}

async function saveEdit() {
  const i = editingIndex.value;
  if (i < 0 || !editDraft.value.trim()) return;
  try {
    messages.value = await api.editMessage(props.meta.id, i, editDraft.value.trim());
    editingIndex.value = -1;
    void refreshInspector(); // 历史变了，预览随之更新
  } catch (e) {
    error.value = String(e);
  }
}

async function removeMsg(i: number) {
  if (!window.confirm("删除这条消息？")) return;
  try {
    messages.value = await api.deleteMessage(props.meta.id, i);
    if (editingIndex.value === i) editingIndex.value = -1;
    void refreshInspector();
  } catch (e) {
    error.value = String(e);
  }
}

// ---------- 输入框 ----------

/** Enter 发送、Shift+Enter 换行；IME 组词中的 Enter 不发送（中文输入关键路径） */
function onComposerKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send();
  }
}

function onEditKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.isComposing) {
    e.preventDefault();
    void saveEdit();
  }
  if (e.key === "Escape") editingIndex.value = -1;
}

function autoGrow() {
  const el = composerEl.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
}

function resetComposerHeight() {
  if (composerEl.value) composerEl.value.style.height = "auto";
}

onMounted(loadAll);
watch(() => props.meta.id, loadAll);
</script>

<template>
  <div class="chat">
    <p v-if="error" class="error">{{ error }}</p>

    <header class="chat-head">
      <div class="who-block">
        <h2>{{ cardName }}</h2>
        <span class="scene" v-if="blackboard">
          第 {{ blackboard.day }} 天 · {{ blackboard.clock || "时间未定" }} ·
          {{ blackboard.place || "地点未定" }}
        </span>
      </div>
      <div class="head-ops">
        <button
          class="tab-btn"
          :class="{ active: panel === 'board' }"
          @click="togglePanel('board')"
        >
          黑板
        </button>
        <button
          class="tab-btn"
          :class="{ active: panel === 'inspector' }"
          @click="togglePanel('inspector')"
        >
          检查器
        </button>
      </div>
    </header>

    <div class="chat-body">
      <section class="main">
        <ul class="stream" ref="streamEl">
          <li v-for="(m, i) in messages" :key="i" class="msg" :class="m.role">
            <div class="who">{{ whoFor(m) }}</div>

            <template v-if="editingIndex === i">
              <textarea
                class="editbox"
                v-model="editDraft"
                rows="3"
                @keydown="onEditKeydown"
              ></textarea>
              <div class="ops editing">
                <button class="op accent" @click="saveEdit">保存</button>
                <button class="op" @click="editingIndex = -1">取消</button>
                <span class="op-hint">Ctrl+Enter 保存 · Esc 取消</span>
              </div>
            </template>

            <template v-else>
              <div class="bubble">{{ m.content }}</div>
              <div class="ops" v-if="!generating">
                <button class="op" @click="startEdit(i)">编辑</button>
                <button class="op" v-if="canReroll(i, m)" @click="reroll">重roll</button>
                <button class="op danger" @click="removeMsg(i)">删除</button>
              </div>
            </template>
          </li>

          <!-- 流式中的角色回复 -->
          <li v-if="generating" class="msg char">
            <div class="who">{{ cardName }}</div>
            <div class="bubble streaming">
              {{ streamText }}<span class="cursor" aria-hidden="true"></span>
            </div>
          </li>

          <li v-if="messages.length === 0 && !generating" class="empty-stream">
            还没有消息。说点什么，把这场戏开起来。
          </li>
        </ul>

        <form class="composer" @submit.prevent="send">
          <textarea
            ref="composerEl"
            v-model="draft"
            rows="1"
            placeholder="说点什么… Enter 发送，Shift+Enter 换行"
            aria-label="消息输入框"
            @keydown="onComposerKeydown"
            @input="autoGrow"
          ></textarea>
          <button v-if="!generating" class="send" type="submit" :disabled="!draft.trim()">
            发送
          </button>
          <button v-else class="stop" type="button" @click="stop">停止</button>
        </form>
      </section>

      <!-- 右侧抽屉：黑板 / 记忆检查器 -->
      <aside v-if="panel" class="panel">
        <header class="panel-head">
          <div class="tabs">
            <button
              :class="{ active: panel === 'board' }"
              @click="panel = 'board'"
            >
              黑板
            </button>
            <button
              :class="{ active: panel === 'inspector' }"
              @click="panel = 'inspector'"
            >
              检查器
            </button>
          </div>
          <button class="close" aria-label="收起面板" @click="panel = ''">×</button>
        </header>

        <div v-if="panel === 'board'" class="panel-body">
          <p class="dim">保存后下一轮组装生效；每轮回复后时钟 +10 分钟。</p>
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
        </div>

        <div v-else class="panel-body">
          <div class="insp-head">
            <p class="dim total" v-if="assembly">
              {{ assemblySource === "last" ? "最近一次发送" : "预览 · 干跑" }} ·
              {{ assembly.layers.length }} 层 · 约 {{ assembly.total_tokens }} token ·
              {{ assembly.messages.length }} 条消息
            </p>
            <button class="btn" @click="preview">刷新预览</button>
          </div>
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
        </div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.chat {
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.error {
  flex: none;
}

.chat-head {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 14px 18px;
  background: var(--hj-panel);
  border: 1px solid var(--hj-line);
  border-radius: 12px;
}
.who-block {
  display: flex;
  align-items: baseline;
  gap: 12px;
  min-width: 0;
}
.who-block h2 {
  margin: 0;
  font-size: 17px;
}
.scene {
  color: var(--hj-dim);
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.head-ops {
  display: flex;
  gap: 6px;
  flex: none;
}
.tab-btn {
  border: 1px solid var(--hj-line);
  background: transparent;
  color: var(--hj-dim);
  border-radius: 8px;
  padding: 5px 12px;
  font-size: 12px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.tab-btn:hover {
  color: var(--hj-fg);
}
.tab-btn.active {
  color: var(--hj-accent);
  border-color: var(--hj-accent);
  background: var(--hj-accent-soft);
}

.chat-body {
  flex: 1;
  min-height: 0;
  display: flex;
  gap: 12px;
  position: relative;
}
.main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--hj-panel);
  border: 1px solid var(--hj-line);
  border-radius: 12px;
  overflow: hidden;
}

/* 消息流 */
.stream {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  list-style: none;
  margin: 0;
  padding: 18px 18px 8px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.msg {
  display: flex;
  flex-direction: column;
  max-width: 76%;
  animation: rise 0.18s ease-out;
}
.msg.user {
  align-self: flex-end;
  align-items: flex-end;
}
.msg.char {
  align-self: flex-start;
}
.who {
  font-size: 11px;
  color: var(--hj-dim);
  margin: 0 4px 3px;
}
.bubble {
  padding: 9px 13px;
  border-radius: 14px;
  background: var(--hj-panel-2);
  border: 1px solid var(--hj-line);
  font-size: 13.5px;
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
}
.msg.char .bubble {
  border-bottom-left-radius: 4px;
}
.msg.user .bubble {
  background: var(--hj-accent-soft);
  border-color: rgba(212, 161, 94, 0.35);
  border-bottom-right-radius: 4px;
}
.bubble.streaming {
  border-color: rgba(212, 161, 94, 0.35);
}
.cursor {
  display: inline-block;
  width: 2px;
  height: 1em;
  margin-left: 2px;
  vertical-align: -0.15em;
  background: var(--hj-accent);
  animation: blink 1s steps(1) infinite;
}

/* 消息操作：常显但低对比（触屏可达），悬停提亮 */
.ops {
  display: flex;
  align-items: center;
  gap: 2px;
  margin-top: 3px;
  opacity: 0.55;
  transition: opacity 0.15s;
}
.msg:hover .ops,
.ops.editing {
  opacity: 1;
}
.op {
  border: none;
  background: transparent;
  color: var(--hj-dim);
  font-size: 11px;
  padding: 2px 7px;
  border-radius: 6px;
  cursor: pointer;
}
.op:hover {
  color: var(--hj-fg);
  background: var(--hj-panel-2);
}
.op.accent {
  color: var(--hj-accent);
}
.op.danger:hover {
  color: var(--hj-danger);
}
.op-hint {
  font-size: 11px;
  color: var(--hj-dim);
  margin-left: 4px;
}
.editbox {
  width: 100%;
  min-width: 280px;
  background: var(--hj-bg);
  border: 1px solid var(--hj-line-strong);
  border-radius: 10px;
  color: var(--hj-fg);
  padding: 8px 12px;
  font-size: 13px;
  line-height: 1.6;
  resize: vertical;
}

.empty-stream {
  align-self: center;
  color: var(--hj-dim);
  font-size: 13px;
  margin-top: 40px;
}

/* 输入区 */
.composer {
  flex: none;
  display: flex;
  align-items: flex-end;
  gap: 10px;
  padding: 12px 14px;
  border-top: 1px solid var(--hj-line);
}
.composer textarea {
  flex: 1;
  background: var(--hj-bg);
  border: 1px solid var(--hj-line-strong);
  border-radius: 10px;
  color: var(--hj-fg);
  padding: 9px 12px;
  font-size: 13.5px;
  line-height: 1.6;
  resize: none;
  max-height: 140px;
}
.composer textarea:focus {
  outline: none;
  border-color: var(--hj-accent);
}
.send,
.stop {
  flex: none;
  border-radius: 10px;
  padding: 9px 18px;
  font-size: 13px;
  cursor: pointer;
  border: 1px solid var(--hj-accent);
  transition: filter 0.15s, opacity 0.15s;
}
.send {
  background: var(--hj-accent);
  color: var(--hj-accent-ink);
  font-weight: 600;
}
.send:hover:not(:disabled) {
  filter: brightness(1.08);
}
.send:disabled {
  opacity: 0.4;
  cursor: default;
}
.stop {
  background: transparent;
  color: var(--hj-danger);
  border-color: var(--hj-danger);
}
.stop:hover {
  background: rgba(201, 111, 111, 0.12);
}

/* 右侧抽屉面板 */
.panel {
  flex: 0 0 320px;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background: var(--hj-panel);
  border: 1px solid var(--hj-line);
  border-radius: 12px;
  overflow: hidden;
}
.panel-head {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  border-bottom: 1px solid var(--hj-line);
}
.tabs {
  display: flex;
  gap: 4px;
}
.tabs button {
  border: none;
  background: transparent;
  color: var(--hj-dim);
  font-size: 12px;
  padding: 4px 10px;
  border-radius: 6px;
  cursor: pointer;
}
.tabs button.active {
  color: var(--hj-fg);
  background: var(--hj-panel-2);
}
.close {
  border: none;
  background: transparent;
  color: var(--hj-dim);
  font-size: 16px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 6px;
  cursor: pointer;
}
.close:hover {
  color: var(--hj-fg);
  background: var(--hj-panel-2);
}
.panel-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.dim {
  color: var(--hj-dim);
  font-size: 12px;
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
  border: 1px solid var(--hj-line-strong);
  border-radius: 6px;
  color: var(--hj-fg);
  padding: 7px 10px;
  font-size: 13px;
}
.form-ops {
  display: flex;
  justify-content: flex-end;
}

.insp-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.layers {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.layer summary {
  display: flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  padding: 7px 10px;
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
}

@keyframes rise {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
@keyframes blink {
  50% {
    opacity: 0;
  }
}

/* 窄屏：抽屉浮于聊天之上 */
@media (max-width: 900px) {
  .panel {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(340px, 92vw);
    z-index: 5;
    box-shadow: -12px 0 32px rgba(0, 0, 0, 0.4);
  }
}
</style>
