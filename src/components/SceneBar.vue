<script setup lang="ts">
import Icon from "./Icon.vue";
import type { Scene } from "../types";

// 场景条（M3.2 · 设计 §10.3）：多场景会话的「与此同时」切换。
// 纯展示组件：场景数据进、动作出；切场/新建等 API 调用留在会话视图。

defineProps<{
  scenes: Scene[];
  activeScene: string;
  busy: boolean;
}>();

const emit = defineEmits<{
  switch: [sc: Scene];
  edit: [];
  create: [];
  split: [];
  merge: [];
}>();

/** 场景条上的一条的提示文字（时间线 + 状态） */
function sceneTip(sc: Scene): string {
  const time = sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天`;
  const status = sc.status === "frozen" ? "（已冻结，切回原地继续）" : sc.status === "merged" ? "（已并入他场）" : "";
  return `${time} · ${sc.place || "地点未定"} · 在场 ${sc.actors.length} 人${status}`;
}
</script>

<template>
  <div class="card card-border flex-none bg-base-100">
    <div class="flex items-center gap-1 overflow-x-auto p-2">
      <button
        v-for="sc in scenes"
        :key="sc.id"
        class="btn btn-sm h-auto min-h-0 flex-col items-start gap-0 px-3 py-1.5 text-left"
        :class="[
          sc.id === activeScene ? 'btn-primary' : 'btn-ghost',
          sc.status === 'merged' ? 'btn-disabled opacity-50' : '',
        ]"
        :disabled="sc.status === 'merged' || busy"
        :data-tip="sceneTip(sc)"
        @click="emit('switch', sc)"
      >
        <span class="flex items-center gap-1 text-xs font-semibold">
          {{ sc.title || sc.place || sc.id }}
          <span v-if="sc.status === 'frozen'" class="badge badge-xs badge-ghost">冻结</span>
          <span v-else-if="sc.status === 'merged'" class="badge badge-xs badge-ghost">已并入</span>
        </span>
        <span class="text-[10px] font-normal opacity-70">
          {{ sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天` }} · {{ sc.place || "地点未定" }}
        </span>
      </button>
      <div class="ml-auto flex flex-none items-center gap-1 pl-2">
        <button
          class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
          data-tip="编辑当前场景：标题/地点/在场者/局部时钟"
          :disabled="busy || !activeScene"
          @click="emit('edit')"
        >
          <Icon name="edit" :size="15" />
        </button>
        <button
          class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
          data-tip="新场景：另起一个舞台（视角随即切过去）"
          :disabled="busy"
          @click="emit('create')"
        >
          <Icon name="plus" :size="15" />
        </button>
        <button
          class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
          data-tip="分场：挑人离场另立场景（「与此同时」）"
          :disabled="busy"
          @click="emit('split')"
        >
          <Icon name="split" :size="15" />
        </button>
        <button
          class="btn btn-square btn-sm btn-ghost tooltip tooltip-bottom"
          data-tip="合场：把另一路场景并进当前场景（对话框里讲清对齐后果）"
          :disabled="busy"
          @click="emit('merge')"
        >
          <Icon name="merge" :size="15" />
        </button>
      </div>
    </div>
  </div>
</template>
