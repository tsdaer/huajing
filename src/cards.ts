// 卡片热加载的共享通知：后端 watch_cards 推 `card_changed` 事件，
// 这里把「第几代」暴露成响应式计数，任何视图 watch 它即可在卡片改动后刷新。

import { ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";

/** 卡片代次：每次收到 `card_changed` 自增（初值 0 = 尚未收到任何变更） */
export const cardGeneration = ref(0);

/** 最近一次变更的卡片路径（展示用，可为空） */
export const lastCardChange = ref("");

/// 导入弹窗的开合状态（应用级：导入入口在顶栏、会话页、拖放三处，弹窗必须常驻）。
/// 曾经把弹窗挂在会话页且只靠 watch 触发，结果在别的页拖入文件没人接——现在请求本身就会开。
export const importOpen = ref(false);

/// 待导入文件：App.vue 收到拖放后写入，导入弹窗 watch 到即自动解析。
/// `nonce` 保证「同一个文件再拖一次」也能触发。
export const importRequest = ref<{ path: string; nonce: number } | null>(null);

let importNonce = 0;

/** 请求导入某个卡文件（拖放或其它入口调用） */
export function requestImport(path: string): void {
  importNonce += 1;
  importRequest.value = { path, nonce: importNonce };
  importOpen.value = true;
  void api.recordDiagnostic("import", `收到导入请求：${path}`);
}

/// 待导入的卡文件扩展名（拖放过滤）
export const CARD_FILE_RE = /\.(png|json)$/i;

/// 拖放被拒绝时的提示（非 PNG/JSON 的文件）：静默忽略会让人以为「拖了没反应」
export const importNotice = ref("");

let started = false;
// E4：监听句柄保存下来，卸载时解除（原来直接丢弃）
let unlistenCardChanged: (() => void) | null = null;

/** 启动监听（应用挂载时调一次；浏览器 mock 下静默降级） */
export async function startCardWatch(): Promise<void> {
  if (started) return;
  started = true;
  try {
    unlistenCardChanged = await listen<{ dir_name: string | null; path: string }>(
      "card_changed",
      (e) => {
        lastCardChange.value = e.payload?.path ?? "";
        cardGeneration.value += 1;
      },
    );
    await api.watchCards();
  } catch {
    /* 无 Tauri 运行时（浏览器 mock）：不监听也不报错 */
  }
}

/** 解除卡片变更监听（应用卸载时调一次） */
export function stopCardWatch(): void {
  unlistenCardChanged?.();
  unlistenCardChanged = null;
}