<script setup lang="ts">
/**
 * 旁白/插页的散文行（增强 E1 · 决断 8）：narration 与旧 system 插页归入同一
 * 散文通道——不是任何角色的言行，永远不进气泡。无头像、无署名、全宽，
 * 衬线斜体弱化显示，两侧细线居中（老插页的样式泛化复用）。
 */
defineProps<{ content: string; serif?: boolean }>();
</script>

<template>
  <div class="mx-auto my-1 flex max-w-full items-center gap-3">
    <span class="h-px flex-1 bg-base-content/15"></span>
    <span
      class="flex-none whitespace-pre-wrap text-sm italic tracking-wide text-base-content/50"
      :class="serif === false ? '' : 'font-serif'"
    >
      {{ content }}
    </span>
    <span class="h-px flex-1 bg-base-content/15"></span>
  </div>
</template>
