// 资产页状态：侧栏「资产」子条目与页内页签共享同一份页签状态，
// 侧栏点「角色 / 设定集」时跳资产页并落到对应页签。

import { ref } from "vue";

export type AssetTab = "characters" | "codex";

export const assetTab = ref<AssetTab>("characters");
