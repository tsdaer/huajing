<script setup lang="ts">
import Icon from "./Icon.vue";
import { avatarStyle, initial } from "../avatar";
import type { Blackboard, Scene, SessionMeta } from "../types";

// 舞台栏（M5.1）：会话头与场景条并进左侧竖栏，聊天流拿回全部纵向空间。
// 多角色适配：阵容逐行展示（状态/点名），不再只是「主角色 + N 同台」徽标。
// 纯展示组件：数据进、动作出（沿用 SceneBar 的组件契约，API 调用留在会话视图）。

const props = defineProps<{
  meta: SessionMeta;
  blackboard: Blackboard | null;
  /** 阵容成员显示名（目录名 → 卡名；未取到时回落目录名） */
  castNames: Record<string, string>;
  /** 正在生成中的署名（流式气泡的名字），用于行内「生成中」状态 */
  activeNames: string[];
  scenes: Scene[];
  activeScene: string;
  busy: boolean;
  /** 点名的发言角色（目录名；"" = 导演调度） */
  speaker: string;
  /** 面板开关状态（board / inspector / ""），底部按钮高亮用 */
  panel: string;
  /** 表情位（M1 占位）：卡内 ui.emit("emotion") 的最近值，会话级徽标 */
  mood: string;
  collapsed: boolean;
}>();

const emit = defineEmits<{
  /** 点名该角色接话（目录名）；单角色会话父层自行忽略 */
  speak: [dir: string];
  switch: [sc: Scene];
  edit: [];
  create: [];
  split: [];
  merge: [];
  "toggle-panel": [p: "board" | "inspector"];
  "export-script": [];
  "toggle-collapse": [];
}>();

function castName(dir: string): string {
  return props.castNames[dir] || dir;
}

/** 阵容行的状态：生成中 > 在场（黑板 actors）> 候场 */
function castState(dir: string): { label: string; cls: string } {
  if (props.activeNames.includes(castName(dir))) {
    return { label: "生成中", cls: "status-warning animate-pulse" };
  }
  if (props.blackboard?.actors.includes(dir)) {
    return { label: "在场", cls: "status-success" };
  }
  return { label: "候场", cls: "status-ghost" };
}

/** 场景行的提示文字（时间线 + 状态），沿用原场景条口径 */
function sceneTip(sc: Scene): string {
  const time = sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天`;
  const status = sc.status === "frozen" ? "（已冻结，切回原地继续）" : sc.status === "merged" ? "（已并入他场）" : "";
  return `${time} · ${sc.place || "地点未定"} · 在场 ${sc.actors.length} 人${status}`;
}
</script>

<template>
  <!-- 收起态：图标条（窄屏不渲染——窄屏用细条唤出浮层） -->
  <div
    v-if="collapsed"
    class="card card-border flex w-12 flex-none flex-col items-center gap-2 bg-base-100 py-2 max-[900px]:hidden"
  >
    <button
      class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
      data-tip="展开舞台栏"
      aria-label="展开舞台栏"
      @click="emit('toggle-collapse')"
    >
      <Icon name="chevron" :size="16" class="rotate-180" />
    </button>
    <div class="h-px w-6 bg-base-300"></div>
    <button
      v-for="dir in meta.characters"
      :key="dir"
      class="avatar avatar-placeholder tooltip tooltip-right"
      :data-tip="castName(dir)"
      @click="emit('speak', dir)"
    >
      <div class="w-8 rounded-full" :style="avatarStyle(castName(dir))">
        <span class="text-xs">{{ initial(castName(dir)) }}</span>
      </div>
    </button>
    <div class="mt-auto flex flex-col items-center gap-1">
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="存为剧本包"
        @click="emit('export-script')"
      >
        <Icon name="download" :size="15" />
      </button>
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="黑板"
        :class="{ 'btn-active': panel === 'board' }"
        @click="emit('toggle-panel', 'board')"
      >
        <Icon name="layers" :size="15" />
      </button>
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="记忆检查器"
        :class="{ 'btn-active': panel === 'inspector' }"
        @click="emit('toggle-panel', 'inspector')"
      >
        <Icon name="cpu" :size="15" />
      </button>
    </div>
  </div>

  <!-- 展开态：完整舞台栏。不设 overflow-hidden——它会把操作钮的长注释气泡
       整段裁死在栏内，注释就"消失"了 -->
  <div v-else class="card card-border flex w-56 flex-none flex-col bg-base-100">
    <!-- 会话信息 -->
    <header class="flex-none border-b border-base-300 px-3 py-2.5">
      <div class="flex items-center gap-1.5">
        <Icon name="film" :size="14" class="flex-none text-base-content/45" />
        <h2 class="m-0 min-w-0 flex-1 truncate text-sm font-semibold">舞台</h2>
        <!-- 唯一的收合钮：窄屏关浮层、宽屏切折叠，语义由父层裁决 -->
        <button
          class="btn btn-square btn-ghost btn-xs tooltip tooltip-right max-[900px]:tooltip-bottom"
          data-tip="收起舞台栏"
          aria-label="收起舞台栏"
          @click="emit('toggle-collapse')"
        >
          <Icon name="chevron" :size="14" />
        </button>
      </div>
      <p class="mb-0 mt-1.5 flex flex-col gap-0.5 text-xs text-base-content/55">
        <span class="flex items-center gap-1">
          <Icon name="clock" :size="12" />
          第 {{ blackboard?.day ?? 1 }} 天 · {{ blackboard?.clock || "时间未定" }}
        </span>
        <span class="flex items-center gap-1">
          <Icon name="pin" :size="12" />{{ blackboard?.place || "地点未定" }}
        </span>
      </p>
      <span
        v-if="mood"
        class="badge badge-xs badge-soft badge-secondary tooltip tooltip-right mt-1.5"
        data-tip="角色表情（卡内 ui.emit，立绘差分留待资产规范落地）"
      >
        {{ mood }}
      </span>
    </header>

    <!-- 在场角色：全员逐行，点击点名 -->
    <section class="flex min-h-0 flex-col overflow-y-auto border-b border-base-300 py-1.5">
      <h3 class="m-0 flex items-center gap-1 px-3 pb-1 text-[11px] font-medium tracking-wide text-base-content/45">
        <Icon name="users" :size="12" />角色
        <span class="ml-auto tabular-nums">{{ meta.characters.length }}</span>
      </h3>
      <button
        v-for="dir in meta.characters"
        :key="dir"
        class="group flex w-full items-center gap-2 px-2 py-1.5 text-left transition-colors hover:bg-base-200/70"
        :class="speaker === dir ? 'bg-primary/10' : ''"
        :data-tip="`点名 ${castName(dir)} 接话`"
        @click="emit('speak', dir)"
      >
        <div class="avatar avatar-placeholder flex-none">
          <div class="w-7 rounded-full" :style="avatarStyle(castName(dir))">
            <span class="text-[11px]">{{ initial(castName(dir)) }}</span>
          </div>
        </div>
        <span class="min-w-0 flex-1 truncate text-xs font-medium">{{ castName(dir) }}</span>
        <span
          v-if="speaker === dir"
          class="badge badge-xs badge-primary flex-none"
          :title="`点名 ${castName(dir)}：只让她接话，不经调度`"
        >
          点名
        </span>
        <span class="flex flex-none items-center gap-1 text-[10px] text-base-content/50">
          {{ castState(dir).label }}
          <span class="status status-xs" :class="castState(dir).cls"></span>
        </span>
      </button>
    </section>

    <!-- 场景：区头操作固定，行列表独立滚动（按钮的注释气泡不能住进滚动容器——
         溢出方向会被滚动裁切，注释就只剩半截了） -->
    <div class="flex min-h-0 flex-1 flex-col">
      <div class="flex flex-none items-center gap-0.5 px-2 pb-1 pt-1.5">
        <h3 class="m-0 flex items-center gap-1 px-1 text-[11px] font-medium tracking-wide text-base-content/45">
          <Icon name="film" :size="12" />场景
          <span class="ml-1 tabular-nums">{{ scenes.length }}</span>
        </h3>
        <div class="ml-auto flex items-center">
          <button
            class="btn btn-square btn-ghost btn-xs tooltip tooltip-right"
            data-tip="编辑当前场景：标题/地点/在场者/局部时钟"
            :disabled="busy || !activeScene"
            @click="emit('edit')"
          >
            <Icon name="edit" :size="13" />
          </button>
          <button
            class="btn btn-square btn-ghost btn-xs tooltip tooltip-right"
            data-tip="新场景：另起一个舞台（视角随即切过去）"
            :disabled="busy"
            @click="emit('create')"
          >
            <Icon name="plus" :size="13" />
          </button>
          <button
            class="btn btn-square btn-ghost btn-xs tooltip tooltip-right"
            data-tip="分场：挑人离场另立场景（「与此同时」）"
            :disabled="busy"
            @click="emit('split')"
          >
            <Icon name="split" :size="13" />
          </button>
          <button
            class="btn btn-square btn-ghost btn-xs tooltip tooltip-right"
            data-tip="合场：把另一路场景并进当前场景（对话框里讲清对齐后果）"
            :disabled="busy"
            @click="emit('merge')"
          >
            <Icon name="merge" :size="13" />
          </button>
        </div>
      </div>
      <section class="flex min-h-0 flex-1 flex-col overflow-y-auto py-1.5">
      <button
        v-for="sc in scenes"
        :key="sc.id"
        class="mx-1.5 flex w-[calc(100%-12px)] flex-col items-start gap-0 rounded-box px-2 py-1.5 text-left transition-colors"
        :class="[
          sc.id === activeScene ? 'bg-primary/12 text-primary' : 'hover:bg-base-200/70',
          sc.status === 'merged' ? 'opacity-50' : '',
        ]"
        :disabled="sc.status === 'merged' || busy"
        :data-tip="sceneTip(sc)"
        @click="emit('switch', sc)"
      >
        <span class="flex w-full items-center gap-1 text-xs font-semibold">
          <span class="min-w-0 flex-1 truncate">{{ sc.title || sc.place || sc.id }}</span>
          <span v-if="sc.status === 'frozen'" class="badge badge-xs badge-ghost gap-0.5">
            <Icon name="pin" :size="9" />冻结
          </span>
          <span v-else-if="sc.status === 'merged'" class="badge badge-xs badge-ghost gap-0.5">
            <Icon name="merge" :size="9" />已并入
          </span>
        </span>
        <span class="text-[10px] font-normal opacity-70">
          {{ sc.clock ? `第${sc.day}天 ${sc.clock}` : `第${sc.day}天` }} · {{ sc.place || "地点未定" }}
        </span>
      </button>
      </section>
    </div>

    <!-- 底部工具 -->
    <footer class="flex flex-none items-center gap-1 border-t border-base-300 px-2 py-1.5">
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="存为剧本包：premise + 初始黑板 + 导演树（不含聊天记录），可分享"
        @click="emit('export-script')"
      >
        <Icon name="download" :size="15" />
      </button>
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="黑板"
        :class="{ 'btn-active': panel === 'board' }"
        @click="emit('toggle-panel', 'board')"
      >
        <Icon name="layers" :size="15" />
      </button>
      <button
        class="btn btn-square btn-sm btn-ghost tooltip tooltip-right"
        data-tip="记忆检查器"
        :class="{ 'btn-active': panel === 'inspector' }"
        @click="emit('toggle-panel', 'inspector')"
      >
        <Icon name="cpu" :size="15" />
      </button>
      <span class="ml-auto flex items-center gap-1 pr-1 text-[10px] text-base-content/40">
        <span class="status status-xs" :class="activeNames.length ? 'status-warning animate-pulse' : 'status-success'"></span>
        {{ activeNames.length ? "生成中" : "在场" }}
      </span>
    </footer>
  </div>
</template>
