<script setup lang="ts">
// M1.8 导入向导：解析预览 → 确认落盘。
// Tauri 的 webview 不把拖入文件的路径交给浏览器 File API（拿不到路径），
// 因此路径来源有两个：① 应用级拖放（App.vue 监听 onDragDropEvent 后写入 store）；
// ② 手动粘贴/输入绝对路径。解析与落盘都在后端，前端只展示草稿与提醒。
import { ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration, importRequest } from "../cards";
import type { CardDraft, ImportReport } from "../types";
import Icon from "./Icon.vue";

const open = defineModel<boolean>({ required: true });
const emit = defineEmits<{ imported: [report: ImportReport] }>();

const pathInput = ref("");
const draft = ref<CardDraft | null>(null);
const report = ref<ImportReport | null>(null);
const busy = ref(false);
const error = ref("");
const el = ref<HTMLDialogElement | null>(null);

// App 级拖放送来的路径：打开弹窗并直接解析
watch(importRequest, (req) => {
  if (!req) return;
  pathInput.value = req.path;
  open.value = true;
  void preview();
});

watch(open, (v) => {
  const d = el.value;
  if (!d) return;
  if (v && !d.open) d.showModal();
  else if (!v && d.open) d.close();
});

function reset() {
  draft.value = null;
  report.value = null;
  error.value = "";
}

/** 解析预览：只读不落盘 */
async function preview() {
  const path = pathInput.value.trim();
  if (!path) return;
  busy.value = true;
  error.value = "";
  report.value = null;
  try {
    draft.value = await api.previewStCard(path);
  } catch (e) {
    draft.value = null;
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

/** 落盘：生成 card.lua 并刷新卡片墙 */
async function importCard() {
  const path = pathInput.value.trim();
  if (!path || !draft.value) return;
  busy.value = true;
  error.value = "";
  try {
    report.value = await api.importStCard(path);
    draft.value = report.value.draft;
    cardGeneration.value += 1; // 卡片墙与会话页立即跟上
    emit("imported", report.value);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

function done() {
  open.value = false;
  reset();
}
</script>

<template>
  <dialog ref="el" class="modal" @close="open = false">
    <div class="modal-box max-w-2xl">
      <h3 class="text-base font-semibold">导入 SillyTavern 角色卡</h3>
      <p class="mt-1 text-xs text-base-content/50">
        支持 PNG（内嵌 chara 数据）与 JSON（V2 / V3）。解析结果会生成
        <code class="text-[11px]">characters/&lt;名字&gt;/card.lua</code>，同名卡自动加后缀，不覆盖。
      </p>

      <div class="mt-4 flex flex-col gap-3">
        <div>
          <label class="label" for="st-path">卡文件路径</label>
          <div class="join w-full">
            <input
              id="st-path"
              v-model="pathInput"
              class="input join-item input-sm flex-1"
              placeholder="把 PNG / JSON 拖进窗口，或粘贴绝对路径"
              @keydown.enter.prevent="preview"
            />
            <button class="btn join-item btn-sm" :disabled="busy || !pathInput.trim()" @click="preview">
              <span v-if="busy" class="loading loading-spinner loading-xs"></span>
              <Icon v-else name="search" :size="14" />
              解析预览
            </button>
          </div>
          <p class="mt-1 mb-0 text-[11px] text-base-content/45">
            也可以直接把卡文件拖到窗口任意位置。
          </p>
        </div>

        <div v-if="error" class="alert alert-error alert-soft py-2 text-xs whitespace-pre-wrap">
          {{ error }}
        </div>

        <template v-if="draft">
          <div class="rounded-box flex flex-wrap items-center gap-2 bg-base-200 px-3 py-2">
            <span class="badge badge-sm badge-primary">{{ draft.name }}</span>
            <span class="badge badge-xs badge-ghost">{{ draft.source_spec }}</span>
            <span v-if="draft.creator" class="text-xs text-base-content/50">by {{ draft.creator }}</span>
            <span v-for="t in draft.tags" :key="t" class="badge badge-xs badge-soft">{{ t }}</span>
          </div>

          <div class="grid grid-cols-3 gap-2 text-center">
            <div class="rounded-box bg-base-200 px-2 py-1.5">
              <p class="m-0 text-[11px] text-base-content/45">开场白</p>
              <p class="m-0 text-sm font-semibold">{{ draft.first_mes ? "有" : "无" }}</p>
            </div>
            <div class="rounded-box bg-base-200 px-2 py-1.5">
              <p class="m-0 text-[11px] text-base-content/45">示例对话组</p>
              <p class="m-0 text-sm font-semibold">{{ draft.example_dialogue.length }}</p>
            </div>
            <div class="rounded-box bg-base-200 px-2 py-1.5">
              <p class="m-0 text-[11px] text-base-content/45">保留素材</p>
              <p class="m-0 text-sm font-semibold">{{ draft.notes ? "有" : "无" }}</p>
            </div>
          </div>

          <div v-if="draft.warnings.length" class="flex flex-col gap-1">
            <div
              v-for="(w, i) in draft.warnings"
              :key="i"
              class="alert alert-warning alert-soft py-1.5 text-xs"
            >
              {{ w }}
            </div>
          </div>

          <div class="collapse collapse-arrow rounded-box bg-base-200">
            <input type="checkbox" />
            <div class="collapse-title min-h-0 py-2 text-[13px]">设定（scenario + 人设）</div>
            <div class="collapse-content px-3">
              <pre class="m-0 rounded-box border border-base-300 bg-base-100 p-2 text-xs whitespace-pre-wrap">{{ draft.scenario }}</pre>
              <pre class="m-0 mt-2 rounded-box border border-base-300 bg-base-100 p-2 text-xs whitespace-pre-wrap">{{ draft.personality }}</pre>
            </div>
          </div>

          <div class="collapse collapse-arrow rounded-box bg-base-200">
            <input type="checkbox" />
            <div class="collapse-title min-h-0 py-2 text-[13px]">开场白</div>
            <div class="collapse-content px-3">
              <pre class="m-0 rounded-box border border-base-300 bg-base-100 p-2 text-xs whitespace-pre-wrap">{{ draft.first_mes || "（空）" }}</pre>
            </div>
          </div>

          <div v-if="report" class="alert alert-success alert-soft py-2 text-xs">
            已导入为「{{ report.dir_name }}」：{{ report.card_path }}
          </div>
        </template>
      </div>

      <div class="modal-action">
        <button class="btn btn-sm" type="button" @click="done">
          {{ report ? "完成" : "取消" }}
        </button>
        <button
          v-if="draft && !report"
          class="btn btn-primary btn-sm"
          type="button"
          :disabled="busy"
          @click="importCard"
        >
          <span v-if="busy" class="loading loading-spinner loading-xs"></span>
          导入这张卡
        </button>
      </div>
    </div>
    <form method="dialog" class="modal-backdrop">
      <button>关闭</button>
    </form>
  </dialog>
</template>