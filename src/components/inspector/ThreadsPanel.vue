<script setup lang="ts">
// 剧情线面板（M2.8 · 设计 §8）：C1 未决事项 + 活跃线（梯度/窗口命中/framing/经过）+ 已了结/已放弃
// M3.0 ①：手动开线（表单）与收线（内联结果输入）——后端命令 open_thread / resolve_thread 的 UI 入口
import { reactive, ref } from "vue";
import Icon from "../Icon.vue";
import type { InspectorThread, InspectorThreads } from "../../types";
import { fixed2, gradeClass, gradeLabel, pct } from "./util";

const props = defineProps<{ threads: InspectorThreads; busy?: boolean }>();

const emit = defineEmits<{
  /** 开线：payload = { title, cause, actors, importance } */
  open: [draft: { title: string; cause: string; actors: string[]; importance: number }];
  /** 收线：id + 结果文本 */
  resolve: [id: string, outcome: string];
}>();

/** 这条线此刻是否落在可提及窗口内；命中则给出激活原因（设计 §8.4） */
function windowReason(t: InspectorThread): string {
  return props.threads.inWindow.find((w) => w.id === t.id)?.reason ?? "";
}

function deadlineText(t: InspectorThread): string {
  const d = t.resurface.deadline;
  return d ? `期限：第 ${d.day} 天（到期升格「${gradeLabel(d.escalate)}」）` : "";
}

// ---------- 手动开线（设计 §8.3：玩家给这段关系记一笔欠账） ----------
const showOpen = ref(false);
const openForm = reactive({ title: "", cause: "", actors: "", importance: 0.6 });

function submitOpen() {
  if (!openForm.title.trim() || props.busy) return;
  emit("open", {
    title: openForm.title.trim(),
    cause: openForm.cause.trim(),
    actors: openForm.actors
      .split(/[,，、]/)
      .map((a) => a.trim())
      .filter(Boolean),
    importance: Math.min(1, Math.max(0.05, Number(openForm.importance) || 0.6)),
  });
  Object.assign(openForm, { title: "", cause: "", actors: "", importance: 0.6 });
  showOpen.value = false;
}

// ---------- 手动收线（设计 §8.3：结果入宫殿 + 事件落流 + 驱动状态树） ----------
const resolvingId = ref("");
const outcomeDraft = ref("");

function toggleResolve(t: InspectorThread) {
  if (resolvingId.value === t.id) {
    resolvingId.value = "";
    return;
  }
  resolvingId.value = t.id;
  outcomeDraft.value = "";
}

function submitResolve(t: InspectorThread) {
  if (!outcomeDraft.value.trim() || props.busy) return;
  emit("resolve", t.id, outcomeDraft.value.trim());
  resolvingId.value = "";
  outcomeDraft.value = "";
}
</script>

<template>
  <!-- C1 未决事项：全部活跃线的只读投影（设计 §8.5：低注意力区负责「查得到全部欠账」） -->
  <section class="flex flex-col gap-1.5">
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">C1 未决事项（只读投影）</p>
      <span class="badge badge-xs badge-soft font-mono">{{ threads.pending.length }}</span>
    </div>
    <ul v-if="threads.pending.length" class="m-0 flex list-none flex-col gap-1 p-0">
      <li
        v-for="(p, i) in threads.pending"
        :key="p + i"
        class="rounded-box bg-base-200 px-2.5 py-1.5 text-xs break-words"
      >
        {{ p }}
      </li>
    </ul>
    <p v-else class="m-0 text-xs text-base-content/50">没有欠着的线——未决事项为空。</p>
  </section>

  <!-- 活跃线 -->
  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <p class="m-0 text-xs text-base-content/50">活跃线</p>
      <div class="flex items-center gap-2">
        <span class="text-[11px] text-base-content/40">
          窗口内 {{ threads.inWindow.length }} · 事件 {{ threads.eventCount }}
        </span>
        <button
          class="btn btn-xs btn-soft"
          :disabled="busy"
          @click="showOpen = !showOpen"
        >
          <Icon name="plus" :size="12" />开线
        </button>
      </div>
    </div>

    <!-- 手动开线表单（设计 §8.3：premise/手动开线也是合法开线来源） -->
    <form
      v-if="showOpen"
      class="rounded-box flex flex-col gap-2 bg-base-100 p-3"
      @submit.prevent="submitOpen"
    >
      <label class="flex flex-col gap-1 text-[11px] text-base-content/60">
        标题（欠着的事，一句话）
        <input
          v-model="openForm.title"
          class="input input-sm input-bordered bg-base-200"
          placeholder="例如：周五还书的约定"
          required
        />
      </label>
      <label class="flex flex-col gap-1 text-[11px] text-base-content/60">
        起因（可空）
        <input
          v-model="openForm.cause"
          class="input input-sm input-bordered bg-base-200"
          placeholder="这件事是怎么欠下的"
        />
      </label>
      <div class="flex gap-2">
        <label class="flex min-w-0 flex-1 flex-col gap-1 text-[11px] text-base-content/60">
          涉及（逗号分隔，可空）
          <input
            v-model="openForm.actors"
            class="input input-sm input-bordered bg-base-200"
            placeholder="小雨, 我"
          />
        </label>
        <label class="flex w-24 flex-none flex-col gap-1 text-[11px] text-base-content/60">
          重要度
          <input
            v-model.number="openForm.importance"
            class="input input-sm input-bordered bg-base-200 font-mono"
            type="number"
            min="0.05"
            max="1"
            step="0.05"
          />
        </label>
      </div>
      <div class="flex justify-end gap-2">
        <button type="button" class="btn btn-ghost btn-xs" @click="showOpen = false">取消</button>
        <button type="submit" class="btn btn-primary btn-xs" :disabled="busy || !openForm.title.trim()">
          开线
        </button>
      </div>
    </form>

    <div
      v-for="t in threads.active"
      :key="t.id"
      class="rounded-box flex flex-col gap-2 bg-base-200 p-3"
    >
      <div class="flex items-start gap-2">
        <span class="min-w-0 flex-1 text-sm leading-snug font-semibold break-words">{{ t.title }}</span>
        <span class="badge badge-xs flex-none" :class="gradeClass(t.resurface.grade)">
          {{ gradeLabel(t.resurface.grade) }}
        </span>
      </div>

      <p class="m-0 text-xs break-words text-base-content/60">
        起因：{{ t.cause || "（未记起因）" }}
      </p>

      <div class="flex items-center gap-2">
        <span class="flex-none text-[11px] text-base-content/45">重要度</span>
        <progress class="progress progress-primary h-1 min-w-0 flex-1" :value="pct(t.importance)" max="100"></progress>
        <span class="flex-none font-mono text-[11px] text-base-content/60">{{ fixed2(t.importance) }}</span>
      </div>

      <div
        v-if="windowReason(t)"
        role="alert"
        class="alert alert-success alert-soft py-1.5 text-[11px] break-words"
      >
        窗口命中：{{ windowReason(t) }}
      </div>

      <p v-if="t.resurface.framing" class="m-0 text-[11px] break-words text-base-content/60">
        framing：{{ t.resurface.framing }}
      </p>

      <p class="m-0 flex flex-wrap gap-x-2 gap-y-0.5 text-[11px] text-base-content/40">
        <span>开线：第 {{ t.opened.turn }} 轮</span>
        <span v-if="t.actors.length">涉及：{{ t.actors.join("、") }}</span>
        <span v-if="t.resurface.last_mentioned_turn != null">上次提及：第 {{ t.resurface.last_mentioned_turn }} 轮</span>
        <span v-if="deadlineText(t)">{{ deadlineText(t) }}</span>
        <span v-if="t.linked_intent" class="font-mono">意图：{{ t.linked_intent }}</span>
      </p>

      <div class="flex flex-col gap-1">
        <p class="m-0 text-[11px] text-base-content/45">经过（{{ t.progress.length }} 个节点）</p>
        <ul v-if="t.progress.length" class="m-0 flex list-none flex-col gap-1 p-0">
          <li
            v-for="(n, i) in t.progress"
            :key="n.turn + '-' + i"
            class="flex items-start gap-1.5 text-[11px] text-base-content/60"
          >
            <Icon name="chevron" :size="10" class="mt-0.5 flex-none text-base-content/30" />
            <span class="min-w-0 break-words">第 {{ n.turn }} 轮 · {{ n.note || "（无说明）" }}</span>
          </li>
        </ul>
        <p v-else class="m-0 text-[11px] text-base-content/40">还没有经过节点。</p>
      </div>

      <!-- 手动收线：输入结果（了结的台词/事件），提交后三件事一次做完 -->
      <div class="flex flex-col gap-1.5">
        <button
          v-if="resolvingId !== t.id"
          class="btn btn-outline btn-xs self-start"
          :disabled="busy"
          @click="toggleResolve(t)"
        >
          <Icon name="check" :size="12" />收线
        </button>
        <form v-else class="flex flex-col gap-1.5" @submit.prevent="submitResolve(t)">
          <textarea
            v-model="outcomeDraft"
            class="textarea textarea-bordered textarea-sm bg-base-100"
            rows="2"
            placeholder="结果是什么？（将成为一条高显著记忆，并可驱动状态转移）"
          ></textarea>
          <div class="flex justify-end gap-2">
            <button type="button" class="btn btn-ghost btn-xs" @click="resolvingId = ''">取消</button>
            <button type="submit" class="btn btn-primary btn-xs" :disabled="busy || !outcomeDraft.trim()">
              了结这条线
            </button>
          </div>
        </form>
      </div>
    </div>

    <p v-if="!threads.active.length" class="m-0 text-xs text-base-content/50">
      没有活跃的线——这段关系目前没有欠着的事。
    </p>
  </section>

  <!-- 已了结 -->
  <div class="collapse collapse-arrow rounded-box bg-base-200">
    <input type="checkbox" />
    <div class="collapse-title min-h-0 px-3 py-2 text-xs">
      已了结（{{ threads.resolved.length }}）
    </div>
    <div class="collapse-content px-3">
      <ul v-if="threads.resolved.length" class="m-0 flex list-none flex-col gap-2 p-0">
        <li v-for="t in threads.resolved" :key="t.id" class="flex flex-col gap-0.5">
          <span class="text-xs font-medium break-words">{{ t.title }}</span>
          <span v-if="t.resolution" class="text-[11px] break-words text-base-content/50">
            第 {{ t.resolution.turn }} 轮 · {{ t.resolution.outcome || "（未记结果）" }}
          </span>
          <span v-else class="text-[11px] text-base-content/40">（未记结果）</span>
        </li>
      </ul>
      <p v-else class="m-0 text-[11px] text-base-content/50">还没有了结的线。</p>
    </div>
  </div>

  <!-- 已放弃 -->
  <div class="collapse collapse-arrow rounded-box bg-base-200">
    <input type="checkbox" />
    <div class="collapse-title min-h-0 px-3 py-2 text-xs">
      已放弃（{{ threads.abandoned.length }}）
    </div>
    <div class="collapse-content px-3">
      <ul v-if="threads.abandoned.length" class="m-0 flex list-none flex-col gap-2 p-0">
        <li v-for="t in threads.abandoned" :key="t.id" class="flex flex-col gap-0.5">
          <span class="text-xs font-medium break-words">{{ t.title }}</span>
          <span v-if="t.resolution" class="text-[11px] break-words text-base-content/50">
            第 {{ t.resolution.turn }} 轮 · {{ t.resolution.outcome || "（不了了之）" }}
          </span>
          <span v-else class="text-[11px] text-base-content/40">不了了之。</span>
        </li>
      </ul>
      <p v-else class="m-0 text-[11px] text-base-content/50">还没有放弃的线。</p>
    </div>
  </div>
</template>
