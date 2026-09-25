<script setup lang="ts">
// daisyUI toast + alert：页面右上角的错误提示条，三个视图共用。
// 空消息时不渲染，避免常驻占位；关闭钮走 Icon 收编文本字符。
import Icon from "./Icon.vue";
defineProps<{ message: string }>();
const emit = defineEmits<{ dismiss: [] }>();
</script>

<template>
  <Transition name="slide-up">
    <div v-if="message" class="toast toast-top toast-end z-50">
      <div role="alert" class="alert alert-error alert-soft max-w-sm">
        <span class="text-sm break-words">{{ message }}</span>
        <button class="btn btn-circle btn-ghost btn-xs" aria-label="关闭提示" @click="emit('dismiss')">
          <Icon name="close" :size="13" />
        </button>
      </div>
    </div>
  </Transition>
</template>
