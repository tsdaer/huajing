<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import type { Blackboard, Message, PromptAssembly, SessionMeta, StreamEvent } from "../types";
import ErrorToast from "../components/ErrorToast.vue";
import Icon from "../components/Icon.vue";

// M1.5 聊天界面：气泡流 + 流式打字机 + 停止 + 消息编辑/重roll/删除。
// 黑板与记忆检查器收进右侧抽屉，聊天流为主。界面全部由 daisyUI 组件构成。

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

// ---------- 分页：多轮对话按页翻，每页 20 条消息 ----------
const PAGE_SIZE = 20;
const page = ref(1);

const pageCount = computed(() => Math.max(1, Math.ceil(messages.value.length / PAGE_SIZE)));
const isLastPage = computed(() => page.value >= pageCount.value);
const pagedMessages = computed(() => {
  const start = (page.value - 1) * PAGE_SIZE;
  return messages.value
    .slice(start, start + PAGE_SIZE)
    .map((m, offset) => ({ m, index: start + offset }));
});
/** 页码窗口：最多 5 个 */
const pageNumbers = computed(() => {
  const total = pageCount.value;
  const span = 5;
  const start = Math.max(1, Math.min(page.value - 2, total - span + 1));
  const end = Math.min(total, start + span - 1);
  return Array.from({ length: Math.max(0, end - start + 1) }, (_, i) => start + i);
});

/** 翻页：回到最后一页时贴底，往回翻时从头看 */
async function goPage(target: number) {
  page.value = Math.min(Math.max(1, target), pageCount.value);
  await nextTick();
  const el = streamEl.value;
  if (el) el.scrollTop = isLastPage.value ? el.scrollHeight : 0;
}

/** 有新消息时贴到最后一页 */
async function jumpToLastPage() {
  await nextTick();
  page.value = pageCount.value;
  await scrollToBottom();
}

watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

function whoFor(m: Message): string {
  if (m.role === "user") return props.meta.persona || "我";
  if (m.role === "char") return cardName.value;
  return "系统";
}

function initial(name: string): string {
  return name.trim().slice(0, 1) || "?";
}

function avatarClass(m: Message): string {
  return m.role === "user" ? "bg-primary text-primary-content" : "bg-neutral text-neutral-content";
}

/** 消息时间戳（秒）→ HH:MM；乐观上屏的消息没有时间戳，返回空串 */
function fmtTime(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
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
  page.value = 1;
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
    page.value = pageCount.value; // 打开会话时停在最新一页
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
  void jumpToLastPage();
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
  void jumpToLastPage();
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
  <div class="flex h-full min-h-0 flex-col gap-4">
    <ErrorToast :message="error" @dismiss="error = ''" />

    <!-- 会话头：角色 + 状态 + 面板开关 -->
    <header class="card card-border flex-none bg-base-100">
      <div class="card-body flex-row items-center gap-3 p-3">
        <div class="avatar avatar-placeholder">
          <div class="w-10 rounded-full bg-primary/15 text-primary">
            <span class="text-sm">{{ initial(cardName) }}</span>
          </div>
        </div>

        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <h2 class="truncate text-base font-semibold">{{ cardName }}</h2>
            <span class="status status-xs status-success"></span>
            <span class="text-xs text-base-content/50">{{ generating ? "生成中" : "在场" }}</span>
          </div>
          <p class="mt-0.5 mb-0 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-base-content/50">
            <span class="flex items-center gap-1">
              <Icon name="clock" :size="13" />
              第 {{ blackboard?.day ?? 1 }} 天 · {{ blackboard?.clock || "时间未定" }}
            </span>
            <span class="flex items-center gap-1">
              <Icon name="pin" :size="13" />{{ blackboard?.place || "地点未定" }}
            </span>
            <span v-if="blackboard && blackboard.actors.length > 0" class="flex items-center gap-1">
              <Icon name="users" :size="13" />{{ blackboard.actors.join(" · ") }}
            </span>
          </p>
        </div>

        <div class="flex flex-none items-center gap-1">
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="黑板"
            :class="{ 'btn-active': panel === 'board' }"
            @click="togglePanel('board')"
          >
            <Icon name="layers" :size="16" />
          </button>
          <button
            class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
            data-tip="记忆检查器"
            :class="{ 'btn-active': panel === 'inspector' }"
            @click="togglePanel('inspector')"
          >
            <Icon name="cpu" :size="16" />
          </button>
        </div>
      </div>
    </header>

    <div class="relative flex min-h-0 flex-1 gap-4">
      <!-- 聊天流 + 输入区 -->
      <section class="card card-border min-w-0 flex-1 overflow-hidden bg-base-100">
        <ul ref="streamEl" class="m-0 flex min-h-0 flex-1 list-none flex-col gap-5 overflow-y-auto p-5">
          <li
            v-for="{ m, index: i } in pagedMessages"
            :key="i"
            class="chat group"
            :class="m.role === 'user' ? 'chat-end' : 'chat-start'"
          >
            <div class="chat-image avatar avatar-placeholder">
              <div class="w-9 rounded-full" :class="avatarClass(m)">
                <span class="text-xs">{{ initial(whoFor(m)) }}</span>
              </div>
            </div>
            <div class="chat-header mb-0.5 text-xs font-medium text-base-content/60">{{ whoFor(m) }}</div>

            <template v-if="editingIndex === i">
              <div class="chat-bubble w-full max-w-[min(76%,72ch)] bg-base-200 p-2">
                <textarea
                  class="textarea textarea-sm textarea-ghost w-full leading-relaxed"
                  v-model="editDraft"
                  rows="3"
                  @keydown="onEditKeydown"
                ></textarea>
              </div>
              <div class="chat-footer mt-1 flex flex-wrap items-center gap-2">
                <button class="btn btn-ghost btn-xs text-primary" @click="saveEdit">保存</button>
                <button class="btn btn-ghost btn-xs" @click="editingIndex = -1">取消</button>
                <span class="flex items-center gap-1 text-[11px] text-base-content/40">
                  <kbd class="kbd kbd-xs">Ctrl</kbd>+<kbd class="kbd kbd-xs">Enter</kbd> 保存 ·
                  <kbd class="kbd kbd-xs">Esc</kbd> 取消
                </span>
              </div>
            </template>

            <template v-else>
              <div
                class="chat-bubble max-w-[min(76%,72ch)] leading-relaxed break-words whitespace-pre-wrap"
                :class="m.role === 'user' ? 'chat-bubble-primary' : 'bg-base-300 text-base-content'"
              >
                {{ m.content }}
              </div>
              <div class="chat-footer mt-1 flex items-center gap-2 text-[11px] text-base-content/40">
                <span v-if="fmtTime(m.ts)">{{ fmtTime(m.ts) }}</span>
                <span
                  v-if="!generating"
                  class="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 max-lg:opacity-70"
                >
                  <button class="btn btn-ghost btn-xs" @click="startEdit(i)">
                    <Icon name="edit" :size="13" />编辑
                  </button>
                  <button v-if="canReroll(i, m)" class="btn btn-ghost btn-xs" @click="reroll">
                    <Icon name="refresh" :size="13" />重roll
                  </button>
                  <button class="btn btn-ghost btn-xs text-error" @click="removeMsg(i)">
                    <Icon name="trash" :size="13" />删除
                  </button>
                </span>
              </div>
            </template>
          </li>

          <!-- 流式中的角色回复（只在最后一页显示） -->
          <li v-if="generating && isLastPage" class="chat chat-start">
            <div class="chat-image avatar avatar-placeholder">
              <div class="w-9 rounded-full bg-neutral text-neutral-content">
                <span class="text-xs">{{ initial(cardName) }}</span>
              </div>
            </div>
            <div class="chat-header mb-0.5 text-xs font-medium text-base-content/60">{{ cardName }}</div>
            <div
              class="chat-bubble max-w-[min(76%,72ch)] border border-primary/40 bg-base-300 leading-relaxed break-words whitespace-pre-wrap text-base-content"
            >
              {{ streamText }}<span class="ml-0.5 inline-block h-[1em] w-0.5 animate-pulse bg-primary align-[-0.15em]" aria-hidden="true"></span>
            </div>
            <div class="chat-footer mt-1 flex items-center gap-2 text-[11px] text-base-content/45">
              <span class="loading loading-dots loading-xs text-primary"></span>
              正在写…
            </div>
          </li>

          <li v-if="messages.length === 0 && !generating" class="mt-10 self-center text-sm text-base-content/45">
            还没有消息。说点什么，把这场戏开起来。
          </li>
        </ul>

        <!-- 分页：多轮对话按页翻 -->
        <div
          v-if="messages.length > 0"
          class="flex flex-none flex-wrap items-center justify-between gap-2 border-t border-base-300 px-3 py-2"
        >
          <span class="text-[11px] text-base-content/45">
            第 {{ page }}/{{ pageCount }} 页 · 共 {{ messages.length }} 条 · 每页 {{ PAGE_SIZE }} 条
          </span>
          <div class="join">
            <button class="btn join-item btn-xs" :disabled="page <= 1" @click="goPage(page - 1)">
              <Icon name="chevron" :size="13" class="rotate-180" />上一页
            </button>
            <button
              v-for="n in pageNumbers"
              :key="n"
              class="btn join-item btn-xs"
              :class="{ 'btn-active': n === page }"
              :aria-current="n === page ? 'page' : undefined"
              @click="goPage(n)"
            >
              {{ n }}
            </button>
            <button class="btn join-item btn-xs" :disabled="page >= pageCount" @click="goPage(page + 1)">
              下一页<Icon name="chevron" :size="13" />
            </button>
          </div>
        </div>

        <!-- 输入区：daisyUI textarea + 圆形发送键 -->
        <form class="flex-none border-t border-base-300 p-3" @submit.prevent="send">
          <div class="flex items-end gap-2">
            <textarea
              ref="composerEl"
              class="textarea max-h-40 w-full flex-1 resize-none leading-relaxed"
              v-model="draft"
              rows="1"
              placeholder="说点什么…"
              aria-label="消息输入框"
              @keydown="onComposerKeydown"
              @input="autoGrow"
            ></textarea>
            <button
              v-if="!generating"
              class="btn btn-circle btn-sm btn-primary"
              type="submit"
              :disabled="!draft.trim()"
              aria-label="发送"
            >
              <Icon name="send" :size="16" />
            </button>
            <button
              v-else
              class="btn btn-circle btn-sm btn-error"
              type="button"
              aria-label="停止生成"
              @click="stop"
            >
              <span class="size-3 rounded-xs bg-current"></span>
            </button>
          </div>
          <p class="mt-2 mb-0 flex flex-wrap items-center gap-1.5 text-[11px] text-base-content/45">
            <kbd class="kbd kbd-xs">Enter</kbd> 发送 ·
            <kbd class="kbd kbd-xs">Shift</kbd>+<kbd class="kbd kbd-xs">Enter</kbd> 换行
          </p>
        </form>
      </section>

      <!-- 右侧抽屉：黑板 / 记忆检查器 -->
      <aside
        v-if="panel"
        class="card card-border flex w-80 flex-none flex-col overflow-hidden bg-base-100 max-[900px]:absolute max-[900px]:inset-y-0 max-[900px]:right-0 max-[900px]:z-10 max-[900px]:w-[min(340px,92vw)]"
      >
        <header class="flex flex-none items-center justify-between gap-2 border-b border-base-300 px-3 py-2">
          <div role="tablist" class="tabs tabs-box tabs-xs">
            <button role="tab" class="tab" :class="{ 'tab-active': panel === 'board' }" @click="panel = 'board'">
              黑板
            </button>
            <button
              role="tab"
              class="tab"
              :class="{ 'tab-active': panel === 'inspector' }"
              @click="panel = 'inspector'"
            >
              检查器
            </button>
          </div>
          <button class="btn btn-square btn-ghost btn-xs" aria-label="收起面板" @click="panel = ''">
            <Icon name="close" :size="15" />
          </button>
        </header>

        <div v-if="panel === 'board'" class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          <p class="m-0 text-xs text-base-content/50">保存后下一轮组装生效；每轮回复后时钟 +10 分钟。</p>
          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="label" for="bb-day">第几天</label>
              <input id="bb-day" class="input input-sm w-full" v-model.number="bbForm.day" type="number" min="1" />
            </div>
            <div>
              <label class="label" for="bb-clock">时间</label>
              <input id="bb-clock" class="input input-sm w-full" v-model="bbForm.clock" placeholder="21:30" />
            </div>
          </div>
          <div>
            <label class="label" for="bb-place">地点</label>
            <input id="bb-place" class="input input-sm w-full" v-model="bbForm.place" placeholder="图书馆自习区" />
          </div>
          <div>
            <label class="label" for="bb-actors">在场（逗号分隔）</label>
            <input id="bb-actors" class="input input-sm w-full" v-model="bbForm.actors" placeholder="小雨, 玩家" />
          </div>
          <div class="flex justify-end">
            <button class="btn btn-primary btn-sm" :disabled="savingBb" @click="saveBlackboard">
              <span v-if="savingBb" class="loading loading-spinner loading-xs"></span>
              {{ savingBb ? "保存中…" : "保存黑板" }}
            </button>
          </div>
        </div>

        <div v-else class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
          <div class="flex items-center justify-between gap-2">
            <p v-if="assembly" class="m-0 text-xs text-base-content/50">
              {{ assemblySource === "last" ? "最近一次发送" : "预览 · 干跑" }}
            </p>
            <button class="btn btn-ghost btn-xs flex-none" @click="preview">
              <Icon name="refresh" :size="13" />刷新
            </button>
          </div>

          <template v-if="assembly">
            <div class="grid grid-cols-3 gap-2">
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">注入层</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.layers.length }}</p>
              </div>
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">估算 token</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.total_tokens }}</p>
              </div>
              <div class="rounded-box bg-base-200 px-3 py-2">
                <p class="m-0 text-[11px] text-base-content/45">消息数</p>
                <p class="m-0 text-lg leading-tight font-semibold">{{ assembly.messages.length }}</p>
              </div>
            </div>

            <div
              v-for="l in assembly.layers"
              :key="l.id + l.name"
              class="collapse collapse-arrow rounded-box bg-base-200"
            >
              <input type="checkbox" />
              <div class="collapse-title flex min-h-0 items-center gap-2.5 px-3 py-2 text-[13px]">
                <span class="badge badge-xs badge-soft badge-primary font-mono">{{ l.id }}</span>
                <span class="flex-1 truncate">{{ l.name }}</span>
                <span class="font-mono text-[11px] text-base-content/45">≈{{ l.tokens }}</span>
              </div>
              <div class="collapse-content px-3">
                <pre class="m-0 rounded-box border border-base-300 bg-base-100 p-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap">{{ l.content }}</pre>
              </div>
            </div>
          </template>
          <p v-else class="m-0 text-xs text-base-content/50">尚无组装数据。</p>
        </div>
      </aside>
    </div>
  </div>
</template>
