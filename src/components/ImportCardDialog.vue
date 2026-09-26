<script setup lang="ts">
// M1.8 导入向导：解析预览 → 确认落盘。M4.1 起双分支：
// - ST 角色卡（PNG/JSON）：preview/import_st_card，生成 characters/<名字>/card.lua
// - 化境包（zip）：preview/import_pack，按 kind 落 characters/ / codex/ / scripts/
// Tauri 的 webview 不把拖入文件的路径交给浏览器 File API（拿不到路径），
// 因此路径来源有两个：① 应用级拖放（App.vue 监听 onDragDropEvent 后写入 store）；
// ② 手动粘贴/输入绝对路径。解析与落盘都在后端，前端只展示草稿与提醒。
import { computed, ref, watch } from "vue";
import { api } from "../api";
import { cardGeneration, importRequest } from "../cards";
import type { CardDraft, ImportReport, PackImportReport, PackPreview } from "../types";
import Icon from "./Icon.vue";

const open = defineModel<boolean>({ required: true });
const emit = defineEmits<{ imported: [report: ImportReport | PackImportReport] }>();

const pathInput = ref("");
const draft = ref<CardDraft | null>(null);
const report = ref<ImportReport | null>(null);
const pack = ref<PackPreview | null>(null);
const packReport = ref<PackImportReport | null>(null);
const overwrite = ref(false);
const busy = ref(false);
const error = ref("");
const el = ref<HTMLDialogElement | null>(null);

/** 当前输入是否是化境包（zip）：决定走哪条导入分支 */
const isPack = computed(() => /\.zip$/i.test(pathInput.value.trim()));

/** 最近一次预览的路径：拖入流程里 pathInput 赋值与预览并发，靠它避免 reset 误清新结果 */
const lastPreviewed = ref("");

// App 级拖放送来的路径：打开弹窗并直接解析
watch(importRequest, (req) => {
  if (!req) return;
  pathInput.value = req.path;
  open.value = true;
  void preview();
});

// 手动改路径时清掉上一份结果（防串：卡草稿和包预览不共存）
watch(pathInput, (v) => {
  if (v !== lastPreviewed.value) reset();
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
  pack.value = null;
  packReport.value = null;
  overwrite.value = false;
  error.value = "";
}

/** 解析预览：只读不落盘（按扩展名分流 ST 卡 / 化境包） */
async function preview() {
  const path = pathInput.value.trim();
  if (!path) return;
  lastPreviewed.value = path;
  busy.value = true;
  error.value = "";
  report.value = null;
  packReport.value = null;
  draft.value = null;
  pack.value = null;
  try {
    if (isPack.value) {
      pack.value = await api.previewPack(path);
    } else {
      draft.value = await api.previewStCard(path);
    }
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

/** 落盘：生成 card.lua / 安装包内容，并刷新卡片墙 */
async function importNow() {
  const path = pathInput.value.trim();
  if (!path || (!draft.value && !pack.value)) return;
  busy.value = true;
  error.value = "";
  try {
    if (isPack.value) {
      packReport.value = await api.importPack(path, overwrite.value);
      emit("imported", packReport.value);
    } else {
      report.value = await api.importStCard(path, overwrite.value);
      draft.value = report.value.draft;
      emit("imported", report.value);
    }
    cardGeneration.value += 1; // 卡片墙与会话页立即跟上
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

const KIND_LABEL: Record<string, string> = {
  character: "角色包",
  world: "世界包",
  script: "剧本包",
};

function fileSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
</script>

<template>
  <dialog ref="el" class="modal" @close="open = false">
    <div class="modal-box max-w-2xl">
      <h3 class="text-base font-semibold">导入</h3>
      <p class="mt-1 text-xs text-base-content/50">
        角色卡：PNG（内嵌 chara 数据）与 JSON（V2 / V3）；化境包：zip
        <code class="text-[11px]">(pack.json)</code>，按类型落
        <code class="text-[11px]">characters/ · codex/ · scripts/</code>。
        重复导入同名默认并存为「-2」，都不覆盖（可显式勾选）。
      </p>

      <div class="mt-4 flex flex-col gap-3">
        <div>
          <label class="label" for="st-path">{{ isPack ? "化境包路径" : "卡文件路径" }}</label>
          <div class="join w-full">
            <input
              id="st-path"
              v-model="pathInput"
              class="input join-item input-sm flex-1"
              placeholder="把文件拖进窗口，或粘贴绝对路径"
              @keydown.enter.prevent="preview"
            />
            <button class="btn join-item btn-sm" :disabled="busy || !pathInput.trim()" @click="preview">
              <span v-if="busy" class="loading loading-spinner loading-xs"></span>
              <Icon v-else name="search" :size="14" />
              解析预览
            </button>
          </div>
          <p class="mt-1 mb-0 text-[11px] text-base-content/45">
            也可以直接把文件拖到窗口任意位置。
          </p>
        </div>

        <div v-if="error" class="alert alert-error alert-soft py-2 text-xs whitespace-pre-wrap">
          {{ error }}
        </div>

        <!-- 化境包分支（M4.1） -->
        <template v-if="pack">
          <div class="rounded-box flex flex-wrap items-center gap-2 bg-base-200 px-3 py-2">
            <span class="badge badge-sm badge-primary">{{ pack.manifest.name }}</span>
            <span class="badge badge-xs badge-secondary">{{ KIND_LABEL[pack.manifest.kind] ?? pack.manifest.kind }}</span>
            <span class="badge badge-xs badge-ghost">{{ pack.manifest.spec }}</span>
            <span v-if="pack.manifest.creator" class="text-xs text-base-content/50">by {{ pack.manifest.creator }}</span>
          </div>
          <p v-if="pack.manifest.description" class="m-0 text-xs text-base-content/60">
            {{ pack.manifest.description }}
          </p>

          <div class="collapse collapse-arrow rounded-box bg-base-200">
            <input type="checkbox" />
            <div class="collapse-title min-h-0 py-2 text-[13px]">包内文件（{{ pack.files.length }} 个）</div>
            <div class="collapse-content px-3">
              <ul class="m-0 flex flex-col gap-0.5 p-0 text-xs">
                <li v-for="f in pack.files" :key="f.name" class="flex justify-between gap-2">
                  <code class="truncate text-[11px]">{{ f.name }}</code>
                  <span class="flex-none text-base-content/45">{{ fileSize(f.size) }}</span>
                </li>
              </ul>
            </div>
          </div>

          <div v-for="(w, i) in pack.warnings" :key="i" class="alert alert-warning alert-soft py-1.5 text-xs">
            {{ w }}
          </div>

          <label v-if="pack.conflict && !packReport" class="flex cursor-pointer items-center gap-2 text-xs">
            <input type="checkbox" class="checkbox checkbox-xs" v-model="overwrite" />
            覆盖同名{{ KIND_LABEL[pack.manifest.kind] ?? "内容" }}（不勾则并存为「-2」）
          </label>

          <div
            v-if="packReport"
            class="alert py-2 text-xs"
            :class="packReport.overwritten ? 'alert-info alert-soft' : 'alert-success alert-soft'"
          >
            <template v-if="packReport.overwritten">
              已覆盖「{{ packReport.target }}」（{{ packReport.files }} 个文件）。
            </template>
            <template v-else>
              已安装到「{{ packReport.target }}」（{{ packReport.files }} 个文件）。
              <template v-if="packReport.kind === 'script'">新建会话时可在向导里选择这个剧本。</template>
            </template>
          </div>
        </template>

        <!-- ST 角色卡分支（M1.8 原样） -->
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

          <div
            v-if="report"
            class="alert py-2 text-xs"
            :class="report.reused ? 'alert-info alert-soft' : 'alert-success alert-soft'"
          >
            <template v-if="report.reused">
              这张卡已经在了（内容完全一致）——已复用「{{ report.dir_name }}」，没有新建副本。
            </template>
            <template v-else>
              已导入为「{{ report.dir_name }}」：{{ report.card_path }}
            </template>
          </div>
        </template>
      </div>

      <div class="modal-action">
        <button class="btn btn-sm" type="button" @click="done">
          {{ report || packReport ? "完成" : "取消" }}
        </button>
        <button
          v-if="(draft || pack) && !report && !packReport"
          class="btn btn-primary btn-sm"
          type="button"
          :disabled="busy"
          @click="importNow"
        >
          <span v-if="busy" class="loading loading-spinner loading-xs"></span>
          {{ isPack ? "导入这个包" : "导入这张卡" }}
        </button>
      </div>
    </div>
    <form method="dialog" class="modal-backdrop">
      <button>关闭</button>
    </form>
  </dialog>
</template>
