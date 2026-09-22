# 化境 Huajing

> 化万千相，随心入境。

**本地优先（local-first）的智能体角色扮演客户端**——角色即 Lua 脚本、数据全明文、
剧情由确定性状态机推进、世界设定是活的实体图谱。

## 架构一览

| 层 | 选择 |
|---|---|
| 壳 | Tauri 2（Rust 核心，系统 WebView，~10MB 级安装包） |
| 界面 | Vue 3 + Vite + TypeScript + daisyUI 5（IM 聊天风打底，主题即 daisyUI `data-theme` 变量层） |
| 角色卡运行时 | mlua 沙箱（剥离 os/io、指令计数上限） |
| 引擎 | 状态树 / 剧情线 / 记忆宫殿 / 设定集 —— 全部 Rust 宿主侧确定性实现 |
| LLM | OpenAI 兼容协议 + SSE 流式（DeepSeek / GLM / Ollama 通吃） |

核心分工：**Rust 管 Lua 沙箱、引擎、上下文组装、流式转发、落盘；前端只做展示；Lua 只活在角色卡与设定集里。**

## 快速开始

```bash
pnpm install          # 安装前端依赖
pnpm tauri dev        # 开发模式（首次会编译 Rust，需几分钟）
pnpm tauri build      # 产出安装包
```

首次使用：启动后会看到三步向导——选一家服务（DeepSeek / 本地 Ollama 预设）、填 API key、建会话开聊；
也可以直接复制 `DataHub/providers.example.toml` 为 `DataHub/providers.toml` 手改。

**导入角色卡**：把 SillyTavern 的 PNG / JSON 卡拖进窗口即可（或「会话 → 导入 ST 卡」粘贴路径），
解析预览确认后生成 `DataHub/characters/<名字>/card.lua`，改卡保存即生效，无需重启。

## 目录结构

```
src/                  # Vue 前端
  main.ts             # 入口：恢复主题偏好 → 挂载应用
  style.css           # 唯一的全局样式：Tailwind + daisyUI 主题层
  theme.ts            # 主题令牌定义 / 应用 / 导出
  theme-presets.ts    # 解析 docs/theme_test/theme.css 的预设（随主题页按需加载）
  sessions.ts         # 会话列表与选中态（侧栏子菜单与会话页共用）
  cards.ts            # 卡片热加载通知 + 拖入导入请求（共享 store）
  window.ts           # 窗口控制（自定义标题栏用，浏览器下降级为空操作）
  api.ts / types.ts   # Tauri 命令封装与数据类型
  mock.ts             # 纯浏览器调试用的内存后端
  components/         # Icon / TitleBar / ErrorToast / ImportCardDialog
  views/              # 概览 · 会话 · 主题 · 设置
src-tauri/src/        # Rust 核心
  card.rs             # 角色卡与 Lua 沙箱、hooks 运行时（设计 §3）
  event.rs            # 事件日志：类型化事件流、投影与重放（M2.0 · 设计 §7.3）
  prompt.rs           # Prompt Builder 双槽位组装（设计 §4）
  llm.rs              # OpenAI 兼容 SSE 客户端（设计 §11）
  store.rs            # DataHub 明文数据层（设计 §12）
  watch.rs            # DataHub 热加载监听（notify）
  stimport.rs         # SillyTavern 角色卡导入（PNG tEXt / JSON V2·V3）
DataHub/              # 用户数据（明文，可随身拷贝）
  characters/小雨/    # 示例角色卡
  codex/default/      # 示例世界（设定集）
docs/
  design.md           # 功能设计 v0.12（唯一权威设计文档）
  plan/               # 里程碑执行计划与进度日志（m1 / m2 / m3）
  prompts/ingestion-prompts.md  # 角色卡制作提示词套件 P0–P11
  theme_test/theme.css          # 主题预设来源（运行时解析，不进编译产物）
  testdata/st-card-v2.png|json  # 真机验收第 6 项用的 SillyTavern 卡样本
```

## 界面与主题

界面层是 **Tailwind 4 + daisyUI 5**，主题即 daisyUI 的 `data-theme` 变量层：默认 `light` / `dark`，深色跟随系统。

- **无边框窗口 + 自绘标题栏**：窗口关掉原生装饰（`decorations: false`），标题栏的品牌、拖拽区与窗口按钮颜色全部走主题令牌，换肤时与内容一致；浏览器里调试时窗口按钮自动隐藏
- **侧栏**：可收成 64px 图标栏（悬停显示名称）；「会话」是可折叠子菜单，直接列出最近几场，点选即切换右侧聊天
- **主题页**：33 个预设（来自 `docs/theme_test/theme.css`）+ 28 个令牌逐项编辑，改动实时生效并持久化，可导出标准 `@plugin "daisyui/theme"` 块固化进 `src/style.css`
- **聊天**：气泡流 + 流式打字机，底部按每页 20 条分页，黑板与记忆检查器收在右侧抽屉

纯前端调试：`pnpm dev` 在浏览器里走 `src/mock.ts` 的内存 mock，只调界面时不必编译 Rust；`pnpm build` 做类型检查与产物构建。

## 当前状态

**M1 能聊完成**（`0.1.0`，2026-09-19）：1v1 流式对话 · Lua 沙箱与基础 hooks（state/memory 落盘、
`api.ui.emit` 到界面）· 黑板 v0 与场景快照 · 热加载 · SillyTavern 卡导入 · 首启向导 · NSIS/MSI 安装包。

**M2 记得住、走得稳、有始有终 完成**（`0.2.0`，2026-09-20，执行计划 [docs/plan/m2.md](docs/plan/m2.md)）：

- **事件日志与投影**：`messages.jsonl` 是类型化事件流（消息 / 钩子副作用 / 黑板 / 状态转移 / 剧情线 /
  设定确认），state、黑板、宫殿、剧情线一律由事件流**投影**写出——编辑或删除历史消息会重放重算，
  M1 遗留的「改历史不回滚状态」就此关闭。
- **六大引擎**：记忆宫殿（视角过滤 + 显著度衰减 + 召回打分）· 设定集（实体 schema、别名 trie、
  五激活源、三级注入、anchors 恒注入、variants/versions 史变）· 状态树 v1（卡内 Lua 状态机，
  转移/reveal/recall 全宿主侧）· 剧情线（生命周期 + 提及时机窗口 + 克制梯度）· 心理运行时
  （情绪槽/衰减/意图/自动表情）· 自动总结管线（便宜档 provider，六类产物落事件流）。
- **双槽位预算组装**（A 身份 / B 场景 / C 历史，逐层记账与确定性降级）与**记忆检查器**十页签。
- 自动化真机验收（200 轮压测 + 手动动线 + 编辑重放）通过：注入 token 相比 M1 基线 **-75.4%**，
  第 203 轮仍召回第 2 轮埋下的约定。

**M3 热闹、会长大、有节奏、不串台 进行中**（`0.3.x`，执行计划 [docs/plan/m3.md](docs/plan/m3.md)）：
群聊/剧场/导演树 · 场景隔离与多线「与此同时」 · 视角记忆与转述 · 设定补全管线 · 素材规格化管线
（wiki 一键成卡）· 世界主线 worldline。

## 路线图

- **M1 能聊**：1v1 流式对话 · Lua 静态卡 + hooks · 黑板 v0 与场景快照 · ST 卡导入
- **M2 记得住、走得稳、有始有终**：记忆宫殿 · 状态树 · 设定集 · 剧情线 · 心理运行时
- **M3 热闹、会长大、有节奏、不串台**：群聊/剧场 · 场景隔离与多线 · 设定补全 · 素材规格化
- **M4 好发布**：自动更新 · 角色/世界包 · 移动端 alpha

详见 [ROADMAP.md](ROADMAP.md)（版本策略与状态）· [docs/plan/](docs/plan/)（各里程碑执行计划）· `docs/design.md §15`（里程碑验收标准）· [CHANGELOG.md](CHANGELOG.md)。
