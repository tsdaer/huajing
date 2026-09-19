<script setup lang="ts">
// daisyUI toast + alert：页面右上角的错误提示条，三个视图共用。
defineProps<{ message: string }>();
const emit = defineEmits<{ dismiss: [] }>();
</script>

<template>
  <div class="toast toast-top toast-end z-50">
    <div v-if="message" role="alert" class="alert alert-error alert-soft max-w-sm">
      <span class="text-sm break-words">{{ message }}</span>
      <button class="btn btn-ghost btn-xs" aria-label="关闭提示" @click="emit('dismiss')">✕</button>
    </div>
  </div>
</template>
