<script setup lang="ts">
// M3.11 场景操作向导：新建 / 分场 / 合场 / 编辑（设计 §10.3 的 UI 收口）。
// 替换 M3.2 的 window.prompt/confirm 系统对话框——合场的对齐后果（在场者并集、
// 时间取较晚、各角色记忆不合并、被并入方归档）要在确认前讲清楚，而不是事后弹窗。
import { computed, reactive, ref, watch } from "vue";
import type { Scene, SceneSubmit } from "../types";
import Icon from "./Icon.vue";

const props = defineProps<{
  kind: "create" | "split" | "merge" | "edit";
  scenes: Scene[];
  /** 当前聚焦场景（合场的目的地、分场的出发点、编辑的对象） */
  activeSceneId: string;
  /** 角色阵容（目录名） */
  cast: string[];
  busy: boolean;
}>();
const emit = defineEmits<{ submit: [payload: SceneSubmit] }>();

const open = defineModel<boolean>({ required: true });
const el = ref<HTMLDialogElement | null>(null);

watch(open, (v) => {
  const d = el.value;
  if (!d) return;
  if (v && !d.open) {
    reset();
    d.showModal();
  } else if (!v && d.open) {
    d.close();
  }
});

const activeScene = computed(
  () => props.scenes.find((sc) => sc.id === props.activeSceneId) ?? null,
);
/** 合场候选：非当前、未被并入的场景（冻结的分路可以切回式并入） */
const mergeCandidates = computed(() =>
  props.scenes.filter((sc) => sc.id !== props.activeSceneId && sc.status !== "merged"),
);

const form = reactive({
  title: "",
  place: "",
  day: 1,
  clock: "",
  actors: [] as string[],
  moving: [] as string[],
  fromId: "",
});

function reset() {
  form.title = "";
  form.place = activeScene.value?.place ?? "";
  form.day = activeScene.value?.day ?? 1;
  form.clock = activeScene.value?.clock ?? "";
  // 新建：全阵容默认入席；分场：默认无人离场（逐个挑）；编辑：在场者原样
  form.actors = props.kind === "create" ? [...props.cast] : [...(activeScene.value?.actors ?? [])];
  form.moving = [];
  form.fromId = mergeCandidates.value[0]?.id ?? "";
}

const TITLE: Record<"create" | "split" | "merge" | "edit", string> = {
  create: "新建场景 · 另起一个舞台",
  split: "分场 · 另立「与此同时」",
  merge: "合场 · 并进当前场景",
  edit: "编辑场景",
};

const canSubmit = computed(() => {
  switch (props.kind) {
    case "create":
      return form.title.trim().length > 0;
    case "split":
      return form.title.trim().length > 0 && form.moving.length > 0;
    case "merge":
      return form.fromId.length > 0;
    case "edit":
      return form.title.trim().length > 0;
  }
});

const mergeChosen = computed(() =>
  mergeCandidates.value.find((sc) => sc.id === form.fromId) ?? null,
);

function toggle(list: string[], name: string) {
  const i = list.indexOf(name);
  if (i >= 0) list.splice(i, 1);
  else list.push(name);
}

function submit() {
  if (!canSubmit.value || props.busy) return;
  switch (props.kind) {
    case "create":
      emit("submit", {
        kind: "create",
        title: form.title.trim(),
        place: form.place.trim(),
        actors: [...form.actors],
      });
      break;
    case "split":
      emit("submit", {
        kind: "split",
        title: form.title.trim(),
        place: form.place.trim(),
        moving: [...form.moving],
      });
      break;
    case "merge":
      emit("submit", { kind: "merge", from: form.fromId });
      break;
    case "edit":
      emit("submit", {
        kind: "edit",
        sceneId: props.activeSceneId,
        title: form.title.trim(),
        place: form.place.trim(),
        actors: [...form.actors],
        day: Math.max(1, Math.floor(form.day) || 1),
        clock: form.clock.trim(),
      });
      break;
  }
  open.value = false;
}

function sceneLabel(sc: Scene): string {
  return `${sc.title || sc.place || sc.id}（${sc.place || "地点未定"} · 第${sc.day}天${sc.clock ? ` ${sc.clock}` : ""}）`;
}
</script>

<template>
  <dialog ref="el" class="modal" @close="open = false">
    <div class="modal-box max-w-lg">
      <h3 class="text-base font-semibold">{{ TITLE[kind] }}</h3>

      <!-- 合场：先选哪一路，再讲清后果 -->
      <template v-if="kind === 'merge'">
        <p v-if="!mergeCandidates.length" class="mt-3 text-sm text-base-content/60">
          没有可以并进来的场景——「与此同时」的另一路都已在场或已归档。
        </p>
        <template v-else>
          <p class="mt-2 mb-2 text-xs text-base-content/50">
            把哪一路并进「{{ activeScene?.title || "当前场景" }}」？
          </p>
          <ul class="m-0 flex list-none flex-col gap-1 p-0">
            <li v-for="sc in mergeCandidates" :key="sc.id">
              <label class="flex cursor-pointer items-center gap-2 rounded-box bg-base-200 px-3 py-2 text-sm">
                <input
                  type="radio"
                  name="merge-from"
                  class="radio radio-primary radio-xs"
                  :checked="form.fromId === sc.id"
                  @change="form.fromId = sc.id"
                />
                {{ sceneLabel(sc) }}
              </label>
            </li>
          </ul>
          <div v-if="mergeChosen" class="alert alert-info alert-soft mt-3 py-2 text-xs leading-relaxed">
            <div>
              合场后：「{{ mergeChosen.title }}」的在场者并入当前场景，故事时间取较晚的一路
              （当前 第{{ activeScene?.day }}天{{ activeScene?.clock ? ` ${activeScene.clock}` : "" }} vs
              第{{ mergeChosen.day }}天{{ mergeChosen.clock ? ` ${mergeChosen.clock}` : "" }}），
              被并入的场景归档留档。<strong>各角色记忆不合并</strong>——合场只并舞台。
            </div>
          </div>
        </template>
      </template>

      <!-- 新建 / 分场 / 编辑：标题 + 地点 + 在场者 -->
      <template v-else>
        <p v-if="kind === 'split'" class="mt-2 mb-0 text-xs text-base-content/50">
          从「{{ activeScene?.title || "当前场景" }}」挑人离场另立新场景；视角随即切过去，原场景冻结。
        </p>
        <div class="mt-3 flex flex-col gap-3">
          <div>
            <label class="label" for="scene-title">{{ kind === "split" ? "新场景的标题" : "标题" }}</label>
            <input
              id="scene-title"
              v-model="form.title"
              class="input input-sm w-full"
              :placeholder="kind === 'split' ? '天台' : '坡下的旧书店'"
            />
          </div>
          <div>
            <label class="label" for="scene-place">地点</label>
            <input
              id="scene-place"
              v-model="form.place"
              class="input input-sm w-full"
              placeholder="图书馆东侧"
            />
          </div>
          <div v-if="kind === 'edit'" class="grid grid-cols-2 gap-3">
            <div>
              <label class="label" for="scene-day">局部时钟 · 第几天</label>
              <input id="scene-day" v-model.number="form.day" class="input input-sm w-full" type="number" min="1" />
            </div>
            <div>
              <label class="label" for="scene-clock">时间</label>
              <input id="scene-clock" v-model="form.clock" class="input input-sm w-full" placeholder="21:30" />
            </div>
          </div>
          <div>
            <p class="label mb-1">{{ kind === "split" ? "离场的角色（至少一位）" : "在场角色" }}</p>
            <div class="flex flex-wrap gap-1.5">
              <button
                v-for="name in kind === 'split' ? activeScene?.actors ?? [] : cast"
                :key="name"
                type="button"
                class="btn btn-xs"
                :class="
                  (kind === 'split' ? form.moving.includes(name) : form.actors.includes(name))
                    ? 'btn-primary'
                    : 'btn-ghost btn-outline'
                "
                @click="toggle(kind === 'split' ? form.moving : form.actors, name)"
              >
                <Icon v-if="(kind === 'split' ? form.moving : form.actors).includes(name)" name="check" :size="12" />
                {{ name }}
              </button>
            </div>
            <p v-if="kind === 'split' && !activeScene?.actors.length" class="mt-1 mb-0 text-[11px] text-base-content/45">
              当前场景没有登记在场者——先在黑板或场景编辑里补上，再分场。
            </p>
          </div>
        </div>
      </template>

      <div class="modal-action">
        <button class="btn btn-sm" type="button" @click="open = false">取消</button>
        <button
          class="btn btn-primary btn-sm"
          type="button"
          :disabled="!canSubmit || busy"
          @click="submit"
        >
          <span v-if="busy" class="loading loading-spinner loading-xs"></span>
          {{ kind === "merge" ? "确认合场" : kind === "edit" ? "保存" : kind === "create" ? "创建并切过去" : "分场" }}
        </button>
      </div>
    </div>
    <form method="dialog" class="modal-backdrop">
      <button>关闭</button>
    </form>
  </dialog>
</template>
