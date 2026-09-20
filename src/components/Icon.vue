<script setup lang="ts">
// 内联 SVG 线性图标集（24×24，stroke=currentColor）。
// 只收录界面用得到的图标，内容为静态常量，不含任何用户输入。
withDefaults(defineProps<{ name: string; size?: number | string }>(), { size: 16 });

const ICONS: Record<string, string> = {
  home: '<path d="M3 10.4 12 3.5l9 6.9"/><path d="M5.6 9.2V19a1.5 1.5 0 0 0 1.5 1.5h3.1V15h3.6v5.5h3.1A1.5 1.5 0 0 0 18.4 19V9.2"/>',
  chat: '<path d="M20.5 12a7.9 7.9 0 0 1-11.6 7L4.2 20.3l1.4-4.4A8 8 0 1 1 20.5 12Z"/>',
  settings:
    '<path d="M4 7h9"/><path d="M17.5 7H20"/><circle cx="15.2" cy="7" r="2.2"/><path d="M4 17h3"/><path d="M11.5 17H20"/><circle cx="9.2" cy="17" r="2.2"/>',
  users:
    '<circle cx="9" cy="8" r="3.2"/><path d="M3.5 19.5c0-3 2.5-5.2 5.5-5.2s5.5 2.2 5.5 5.2"/><path d="M16.6 5.6a3.2 3.2 0 0 1 0 6.3"/><path d="M18.2 14.9c2 .6 3.3 2.4 3.3 4.6"/>',
  cpu: '<rect x="6.5" y="6.5" width="11" height="11" rx="2.4"/><rect x="10.2" y="10.2" width="3.6" height="3.6" rx="1"/><path d="M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3"/>',
  database:
    '<ellipse cx="12" cy="6.2" rx="7" ry="2.8"/><path d="M5 6.2v11.6c0 1.5 3.1 2.8 7 2.8s7-1.3 7-2.8V6.2"/><path d="M5 12c0 1.5 3.1 2.8 7 2.8s7-1.3 7-2.8"/>',
  search: '<circle cx="10.8" cy="10.8" r="6.3"/><path d="m15.4 15.4 4.1 4.1"/>',
  plus: '<path d="M12 5.5v13M5.5 12h13"/>',
  refresh: '<path d="M20 11a8 8 0 1 0-2.6 6"/><path d="M20 5.5V11h-5.5"/>',
  trash:
    '<path d="M4.5 7h15"/><path d="M9.5 7V5.2A1.2 1.2 0 0 1 10.7 4h2.6a1.2 1.2 0 0 1 1.2 1.2V7"/><path d="M6.6 7l.7 12.1A1.4 1.4 0 0 0 8.7 20.4h6.6a1.4 1.4 0 0 0 1.4-1.3L17.4 7"/>',
  edit: '<path d="M4.5 19.5h4L19.4 8.6a2.2 2.2 0 0 0-3.1-3.1L5.5 16.4z"/><path d="m14.8 6.9 2.9 2.9"/>',
  send: '<path d="M4.8 11.8 19.5 4.5l-6.2 15-2-6.4z"/><path d="m11.3 13.1 8.2-8.6"/>',
  close: '<path d="M6 6l12 12M18 6 6 18"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.6v2.2M12 19.2v2.2M4.3 4.3l1.6 1.6M18.1 18.1l1.6 1.6M2.6 12h2.2M19.2 12h2.2M4.3 19.7l1.6-1.6M18.1 5.9l1.6-1.6"/>',
  moon: '<path d="M20 14.2A8.4 8.4 0 0 1 9.8 4a8.5 8.5 0 1 0 10.2 10.2Z"/>',
  menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
  sparkle:
    '<path d="M11 3.2l1.7 4.6 4.6 1.7-4.6 1.7L11 15.8 9.3 11.2 4.7 9.5l4.6-1.7z"/><path d="M18.4 15.4l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"/>',
  clock: '<circle cx="12" cy="12" r="8.2"/><path d="M12 7.6V12l2.9 2"/>',
  pin: '<path d="M12 21s6.4-6 6.4-10.4A6.4 6.4 0 0 0 5.6 10.6C5.6 15 12 21 12 21Z"/><circle cx="12" cy="10.4" r="2.4"/>',
  layers: '<path d="m12 3.6 8.4 4.2L12 12 3.6 7.8z"/><path d="m4.6 12.1 7.4 3.7 7.4-3.7"/><path d="m4.6 16.1 7.4 3.7 7.4-3.7"/>',
  bolt: '<path d="M13.6 3 6 13.4h4.8L10.2 21l8.3-10.9h-4.9z"/>',
  chevron: '<path d="m9.5 5.5 6.5 6.5-6.5 6.5"/>',
  check: '<path d="m5 12.5 4.6 4.6L19 7.4"/>',
  dots: '<circle cx="12" cy="5.6" r="1.3" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="1.3" fill="currentColor" stroke="none"/><circle cx="12" cy="18.4" r="1.3" fill="currentColor" stroke="none"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2.2"/><path d="M15 5.6A1.6 1.6 0 0 0 13.4 4H6.6A1.6 1.6 0 0 0 5 5.6v6.8A1.6 1.6 0 0 0 6.6 14"/>',
  minimize: '<path d="M5.5 12h13"/>',
  maximize: '<rect x="5.5" y="5.5" width="13" height="13" rx="2"/>',
  restore: '<rect x="8" y="8" width="10.5" height="10.5" rx="2"/><path d="M15.5 5.5h-8A2 2 0 0 0 5.5 7.5v8"/>',
  palette:
    '<path d="M12 3.5a8.5 8.5 0 0 0 0 17c1.3 0 2-.9 2-1.9 0-1.5-1.4-1.7-1.4-2.8 0-.8.7-1.4 1.6-1.4h1.5a4.8 4.8 0 0 0 4.8-4.8c0-3.4-3.4-6.1-8.5-6.1Z"/><circle cx="7.6" cy="11.4" r="1.1" fill="currentColor" stroke="none"/><circle cx="11.4" cy="7.8" r="1.1" fill="currentColor" stroke="none"/><circle cx="16.2" cy="9.6" r="1.1" fill="currentColor" stroke="none"/>',
  dot: '<circle cx="12" cy="12" r="4"/>',
  split:
    '<path d="M7 3.5 3.5 7 7 10.5"/><path d="M3.5 7H14a4 4 0 0 1 4 4v0"/><path d="M7 13.5 3.5 17 7 20.5"/><path d="M3.5 17h6a4 4 0 0 0 4-4"/>',
  merge:
    '<path d="M3.5 5.5h6.5a4 4 0 0 1 4 4v0a4 4 0 0 0 4 4h2.5"/><path d="M17.5 10.5l3 3-3 3"/><path d="M3.5 18.5h6.5a4 4 0 0 0 3.2-1.6"/>',
};
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    class="shrink-0"
    v-html="ICONS[name] ?? ''"
  />
</template>
