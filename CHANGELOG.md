# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本段与里程碑的对应关系见 [ROADMAP.md](ROADMAP.md)。

## [未发布]

### Added

- 项目初始化：Tauri 2 + Vue 3 + Vite + TypeScript 脚手架；Rust 核心模块桩（card / prompt / llm / store / commands）；DataHub 示例数据（小雨角色卡、default 世界）；设计文档 v0.12 与角色卡制作提示词套件入库
- 工程文档：ROADMAP、CHANGELOG、M1 执行计划（docs/plan/m1.md）
- **M1.1 配置与会话骨架**：`store.rs` 数据层——providers.json 按名 upsert/删除、settings.json 读写（缺省回退）、personas 扫描（坏文件容错）；会话目录骨架（session.json / messages.jsonl / state.json / blackboard.json）与消息追加/读取；无外部依赖的时间工具（会话 id、ISO 时间戳）；命令层注册 list_providers / save_provider / delete_provider / get_settings / save_settings / list_personas / new_session / list_sessions / read_messages；前端壳导航与设置页（接入点增删改、用户人格与全局配置展示）；含 5 例数据层单元测试

- **M1.2 Lua 沙箱与卡片加载**：引入 mlua 0.12（luajit + vendored，MSVC 构建验证通过）；沙箱 v0——库白名单（table/string/math/bit）、base 危险函数清空（load/require/print…）、LuaJIT 关闭 JIT（保证指令钩子对热循环生效）、指令计数上限 10^7、内存上限 32MB；card.lua 解析为静态字段 + hooks/state 探测（白名单字段提取，规避 mlua serde 遍历 function 报错）；解析/执行失败降级占位静态卡；`run_hook` 错误边界运行时（on_load/on_context/on_message，白名单 API ctx.inject/ctx.window/api.memory/api.blackboard/api.ui.emit/api.random/api.dice，会话种子可回放）；命令 list_cards/get_card；首页卡片清单展示；11 例卡片单元测试（含恶意死循环卡被终止、os/io 剥离验证）

- **M1.3 LLM 流式通道**：reqwest + SSE 流式补全（OpenAI 兼容 `/chat/completions`，bearer 认证，连接超时 15s、逐块超时 180s、无整体超时）；字节级 SSE 解析器（跨块事件重组、CRLF、多字节 UTF-8 截断安全、`data:` 单空格剥离不 trim、非 data 行忽略）；`send_message` 命令——chat 档 provider 选择、用户消息先落盘、Channel 流事件（delta/done/error）推送前端、同会话并发防重、回复（含中断部分文本）落盘；`stop_generation` 中断命令；前端 sendMessage/stopGeneration 封装；M1.3 最小组装（卡 scenario/personality 系统头 + 历史窗口，M1.4 换正式 Builder）；SSE 解析 7 例单元测试

- **M1.4 Prompt Builder v0 与黑板 v0**：`prompt.rs` 双槽位组装器——A1 全局契约（表达契约/克制契约/资料优先级声明，叙事模式台词体/小说体/独白体三态）、A2 用户人格、A3 身份锚（scenario/personality + 按 psyche.affect 情绪命中排序的示例对话）、B1 场景快照（黑板投影，`<scene>` 标签，每轮强制、空黑板退化为"第N天 · 地点未定"）、B5 hook 注入（on_context 收集，降级卡跳过）、C3 最近 40 条消息窗口；空层省略不产生空标签；组装层明细带 token 估算（CJK≈1:1、其余≈4:1）。黑板 v0：load/save + `get_blackboard`/`update_blackboard` 命令（UI 手动编辑）+ 每轮回复后时钟 +10 分钟步进（跨日进位）。`send_message` 换用组装器并留存每会话最近一次组装；新增 `preview_prompt`（干跑预览）与 `last_prompt` 检查器命令。前端新增会话页（列表 + 最小新建表单）与会话详情（黑板编辑、记忆检查器 v0 逐层展示注入内容与 token、消息只读列表）
- **M1.5 会话与聊天 UI**：`new_session` 写入 first_mes 开场白（turn 0 角色消息，卡片读取失败不阻塞建会话）；消息级操作命令 `edit_message` / `delete_message`（store 新增 `write_messages` 全量重写，配合 `MessageLog::invalidate` 保持缓存一致）与 `regenerate` 重roll（先删末尾角色回复再重新流式生成，失败不留并列回复；`send_message` 流式后半程抽出 `stream_reply` 共用）。前端会话详情重写为聊天界面：IM 气泡流（用户右/角色左、角落收口）、流式打字机（增量上屏 + 呼吸光标）、生成中可停止、消息悬停操作（编辑/重roll/删除，编辑支持 Ctrl+Enter 保存与 Esc 取消）、输入框 Enter 发送 / Shift+Enter 换行（跳过 IME 组词，中文输入安全）与自适应高度；黑板与记忆检查器收进右侧抽屉面板（窄屏浮层）；会话页布局改为撑满高度、消息流内部滚动。新增 `write_messages` 数据层测试（等长改写须 invalidate、重写后追加不串行），累计 37 例全绿

- **界面工程化：全面引入 daisyUI**：前端改用 Tailwind 4 + daisyUI 5，`src/style.css` 只保留主题层声明（默认 `light` / `dark`，深色跟随系统）与三件全局必要事（视口高度、中文栈、动效降级），原先手写的 `--hj-*` 变量层与各页 `<style scoped>` 全部下线。四个页面按组件重写：`drawer` 侧栏（可收成 64px 图标栏，收起态走 `is-drawer-close` 变体 + tooltip）、`navbar` 顶栏、`menu` 导航（「会话」为 `<details>` 可折叠子菜单，列出最近 8 场）、`card` / `stats` / `list` / `badge` / `avatar` 内容区、`chat` 气泡流、`dialog.modal` 新建与删除确认、`toast` + `alert` 错误条、`collapse` 注入层、`join` 分页、`kbd` 快捷键提示
- **无边框窗口与自定义标题栏**：`tauri.conf.json` 关闭原生装饰（`decorations: false`，保留阴影与边缘缩放），capabilities 显式授权窗口操作（`start-dragging` / `minimize` / `toggle-maximize` / `internal-toggle-maximize` / `is-maximized` / `close`）；新增 `TitleBar.vue`（品牌、拖拽区、窗口按钮，双击最大化）与 `window.ts`（无 Tauri 运行时自动降级为空操作），标题栏颜色全部走主题令牌，换肤时与内容一致
- **主题编辑器（新增「主题」页）**：预设主题来自 `docs/theme_test/theme.css`（`?raw` 内联并运行时解析，33 个主题不进 CSS 产物，页面按需分包）；28 个令牌（20 个颜色 + 圆角 / 尺寸 / 边框 / 立体感 / 噪点）逐项编辑，支持 `oklch(...)` 与 hex，原生取色器用浏览器色彩引擎把任意 CSS 颜色折算成 hex；改动实时写入 `:root` 内联变量并持久化，可一键清除；导出标准 `@plugin "daisyui/theme"` 块供固化进 `style.css`
- 聊天界面：气泡加时间戳、操作按钮改为悬停浮现（窄屏常显），底部新增 `join` 分页（每页 20 条，打开会话停在最新页、发送后自动跳末页、往回翻从头看）
- `src/sessions.ts`：会话列表与选中态的共享 store，侧栏子菜单与会话页共用

### Changed

- 配置文件格式 JSON → TOML：`providers.json`→`providers.toml`、`settings.json`→`settings.toml`、`personas/*.json`→`*.toml`（手改友好、支持注释）；示例文件随之替换；运行时数据（session/messages/state/blackboard）保持 JSON/JSONL（追加与机器读写语义）
- 会话消息读取改 `MessageLog` 增量缓存：按字节偏移 seek 续读，每轮开销只与新增行数相关（高轮次性能）；半行（崩溃残留）留待补全后消费；文件被外部截断/重写时缓存自动重置；提供 `invalidate` 供消息编辑/删除场景使用
- `send_message` 发给 LLM 的历史加最近 40 条窗口上限（M1.4 正式预算分配的前置保护）
- 界面主题层扩展：新增 `--hj-panel-2 / --hj-line(-strong) / --hj-accent-soft / --hj-accent-ink / --hj-danger` 变量；按钮、错误条提升为全局共享样式（修复会话页按钮类未定义的问题）；统一细滚动条、选区着色、`:focus-visible` 焦点环与 `prefers-reduced-motion` 降级；顶栏激活态改为金色呼应品牌

- 会话页去掉左侧列表与搜索框（改由侧栏「会话」子菜单切换），「新建会话」按钮移到顶栏 `navbar` 的「会话」标题旁——用共享 store 的开合状态驱动原生 `dialog`，Esc / 点遮罩关闭时状态同步收回
- 界面主题改用 daisyUI 默认主题：`index.html` 去掉写死的 `data-theme`，`main.ts` 启动时先恢复 light/dark 偏好、再叠加「主题」页保存的自定义令牌（`:root` 内联变量优先于主题规则）
- 窗口顶栏改 `navbar`；侧栏收起/展开由 JS 状态驱动（大屏默认展开、窄屏默认收起，窄屏切页后自动收起），导航项与图标集合补 `Icon.vue`（内联 SVG，无图标库依赖）

### Fixed

- `data_root()` 在 `tauri dev`（cwd 为 src-tauri）下解析到 `src-tauri/DataHub`，现自 cwd 逐级向上查找已有 DataHub 目录
- 自定义标题栏后页面多出 36px 空白滚动条：daisyUI `.drawer-side` 固定 `height: 100dvh`，标题栏占掉一条高度后侧栏仍按整窗高撑开——改为跟随父容器高度（`h-full!`）
- 侧栏收起时出现横向滚动条：菜单项 tooltip 的绝对定位伪元素被 `ul.menu` 的 `overflow-y-auto` 当成横向可滚动内容——收起态不再把该菜单当滚动容器（`overflow: visible`），展开态保持纵向滚动
- `src/mock.ts` 的 `clearInterval(timer.timer)` 类型错误（`timer` 本身已是定时器 id），此前会让 `pnpm build` 卡在 `vue-tsc` 阶段
