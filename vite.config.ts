import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [vue(), tailwindcss()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: [
        "**/src-tauri/**",
        // 运行时数据与产物不进模块图，但改动会触发整页 reload——真机上每轮对话
        // 末尾都要回写 world.json，dev 模式下界面就会像浏览器刷新一样闪一下
        // （M4.1 走查实锤：只有 DataHub 下的 .json 重写触发，.jsonl 追加不触发）
        "**/DataHub/**",
        "**/dist/**",
        "**/temp/**",
      ],
    },
  },
}));
