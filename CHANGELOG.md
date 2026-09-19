# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本段与里程碑的对应关系见 [ROADMAP.md](ROADMAP.md)。

## [0.2.0] - 2026-09-20

> M2「记得住、走得稳、有始有终」进行中。执行计划见 [docs/plan/m2.md](docs/plan/m2.md)。

### Added

- **M2.0 事件日志与投影（设计 §7.3「可回放」/ §6.9 / §12）**：新增 `event.rs`——`messages.jsonl` 从「消息流」升级为**类型化事件流**，一行一个事件：`message`（消息）/ `effect`（钩子副作用：state 顶层键补丁 + 黑板写入 + 记忆写入）/ `blackboard`（init·manual·clock）/ `transition`（状态树转移，M2.3）/ `thread`（剧情线，M2.4）/ `codex`（设定揭示与确认，M2.2）。事件带 `seq`（按位置分配）与 `kind` 判别字段；**M1 写的无 kind 行照旧按消息读**，旧会话不做破坏性迁移。新增 `Projection`（投影）：消息列表、各角色 state、黑板、宫殿、转移、剧情线快照、秘密揭示集全部由事件流**纯函数折叠**得出，`state.json` / `blackboard.json` / `palace.jsonl` 一律由投影写出——**落盘只剩一条路径**。`store::MessageLog` 升级为 `store::EventLog`（字节偏移增量读不变，新增 `append` / `rewrite`）。25 例新测试（事件往返、旧行兼容、投影折叠、确定性重放、state 补丁、genesis 与老会话基线、手动事件与派生事件的区分、半行/坏行/序号重排、揭示集、线快照后写覆盖）

- **M2.0 消息级操作可回滚**：编辑/删除历史消息现在会**重放重算**（`rebuild_from`）——改写或移除该消息事件后，丢掉它所在轮次起的派生事件，再按卡顺序重跑这些轮的 `on_context` / `on_message`（含轮末时钟步进），钩子是 (消息, 黑板, state) 的纯函数，因此重放得到一致状态。**M1 的同源遗留就此关闭**：改掉那句「谢谢」，好感度会跟着退回去，记忆也一并消失。老会话（事件流里没有 genesis）首次消息级操作时**从头全量重放并补上 init 事件**，就地升级为事件溯源会话（此后回滚精确；老会话的黑板已是终态，故重放不再叠加时钟步进）。重roll 的截断抽成 `truncate_turn`（命令与单测共用），「不重复计分」从启发式判据 `turn_has_reply` 变成结构性保证——旧效果已不在流里，重放恰好一次

- **M2.1 记忆宫殿（引擎）**：新增 `palace.rs`——记忆对象 v1（kind/content/time/place/actors/witnesses/salience/emotion/links/thread/rehearsals，手写反序列化以兼容 M1 的 `fact` 行与设计 §5.2 的嵌套时间形态）、显著度打分（salience × 0.5^(Δ故事天/7) × 关联加成(≤3.0) × 再提及加成(≤1.5)，权重全部具名）、**视角过滤**（viewer 必须 ∈ witnesses，设计 §10.4 硬约束）、四类关联命中（recall 提示/地点/在场者/最近提及 + 活跃线）、`render_memory_block`（「回忆·第3天 23:40」框架 + 转述标注）、三视图（房间/时间线/关联图）与 `search`。31 例单测（衰减半衰期、视角过滤、四类命中、权重上限、legacy 兼容、同分确定性排序、预算截断等）

- **M2.2 设定集（引擎）**：新增 `codex.rs`——实体 schema（char/place/item/event/org/rule/concept/note + facts/secrets/live/relations/lifecycle/variants/versions）、自建别名 trie（最长优先、CJK 精确、拉丁大小写不敏感，未引 aho-corasick 依赖）、**五激活源**（提及/在场/揭示/关系牵引/常驻，全部宿主侧确定性）、**三级注入深度**（1 行/卡片/深卡 + `look.anchors` 恒注入）、滞回（上一轮激活者保底卡片）、预算降级阶梯（深卡→卡片→精简卡→1 行→裁撤，**anchors 最后被裁**）、`variants` 条件变体与 `versions` 史变（按故事天解析：第 10 天与第 30 天取到不同事实）、`fill_placeholders`、`anchors_conflict`（提案与辨识点冲突即驳回）。32 例单测

- **M2.1/M2.2 接入组装与运行时**：`prompt.rs` 新增 **B3 设定集**与 **B4 回忆**槽（`<world>` / `<memory>` 标签，空层省略；`PromptLayer.sources` 逐卡记录激活原因）；`commands.rs` 新增世界加载（`codex/<世界>/entities/*.lua|*.json`，`.lua` 走同一套 Lua 沙箱解析、坏文件跳过并留诊断）与**目录指纹缓存**（改文件即生效，与热加载同款判据）、会话级跨轮运行时（上一轮激活集合，供滞回）、召回查询构造（视角=本角色、故事天/地点/在场者/提及窗口来自黑板与别名扫描）；`api.memory` 读侧（`HookEnv.memory`）接上宫殿（M1 里这一侧恒空）；**黑板扩展出实体作用域键**（`api.blackboard.set('char.小雨.mood', …)` → `live` 字段拼出 `▸当前:心情不错`，读侧同时给平铺与嵌套两种形态）。新增端到端单测：别名提及激活进 B3 + anchors 恒注入 + 逐卡激活原因 + 卡写记忆进 B4 + 作用域键进 `▸当前`。记忆检查器逐层展示激活原因（`SessionView`）

### Changed

- **预览组装改为纯干跑**：`preview_prompt` 不再落盘、不再记事件（M1 让预览也落盘是为了避免「预览一次状态变了、正式发送又变一次」的漂移；事件化之后正式发送自己会跑一次并留下事件，预览再落盘反而是多算一次）
- **钩子运行抽成与 Tauri 无关的内核**：`assemble_prompt_core` / `run_load_hook_core` / `run_message_hook_core` / `commit_reply` 生产与单测共用同一份代码（M1 曾因单测另写等价逻辑而漏掉生产的半步），单测的 `simulate_turn` 现在直接调用这些内核
- 手改黑板（`update_blackboard`）进事件流（reason=manual）——它不是派生结果，重放历史时不会被抹掉

### Fixed

- **编辑/删除历史消息不再留下已发生的状态变化**（M1 遗留，见 docs/plan/m1.md 同源记录）：事件日志落地后，任何消息级操作都能得到与事件流一致的状态（设计 §15「同一事件流重放状态路径一致」）

- **文档**：新增 M2 执行计划 [docs/plan/m2.md](docs/plan/m2.md)（验收标准、关键架构决断、M2.0–M2.8 任务分解、风险与进度日志）

## [0.1.0] - 2026-09-19

### Added

- **M1.6 hooks 接入运行时**：hooks 从「能跑」变成「参与对话」——`card.rs` 引入 `HookEnv` / `HookRun` / `UiSink`：调用方传入本轮可见的 state 与黑板快照，hook 的原地改动与 `api.memory`/`api.blackboard`/`api.ui.emit` 写入按三类增量回传宿主；`api.blackboard.set` 按黑板 v0 字段白名单（day/clock/place/actors）校验，越权键报 Lua 错误；`on_message` 不再接收多余窗口参数（设计 §3 签名只有 msg）。调用时机：`on_load` 建会话（角色入席、初始化 state）、`on_context` 每轮组装（B5 注入 + 顺带改状态）、`on_message` 每条消息落盘后。落盘分工：state → `state.json`（重启不丢）、`api.memory.set` → `palace.jsonl`（`MemRecord` 追加流，读侧召回留待 M2 记忆宫殿）、黑板写入 → `blackboard.json`；`api.ui.emit` 实时经 Tauri 事件推前端并在 `done` 事件的 `HookReport` 里留一份。新增命令 `get_card_state` / `list_card_memory`；示例卡「小雨」补行为层（好感度 + 情绪 + 记忆写入，与设计 §3 示例同构）。新增 8 例测试（HookEnv 读写、未定义 hook 不动状态、ui.emit 实时回调、同 key 后写覆盖、越权键拒绝），累计 46 例全绿（含 3 例「钩子副作用真的落盘」端到端用例：好感度 50→51→52 跨轮演进 + state.json 读回 + palace.jsonl 追加 + 黑板写入）

- **M1.6 前端：卡内可观测面**：会话页检查器扩成四页签——注入层 / **卡内状态**（state.json 逐项）/ **卡内记忆**（palace.jsonl 倒序，带轮次与来源）/ **事件流**（`api.ui.emit` 最近 50 条 + 沙箱错误日志）；会话头部新增表情徽标（`ui.emit("emotion", …)` 的 M1 占位显示，立绘差分留待资产规范）；`StreamEvent` 增 `hook_event` 形态，`done` 带 `report`

- **M1.7 热加载**：新增 `watch.rs`——notify 监听 `characters/`（递归）与 `personas/`、`settings.toml`，事件去重 + 300ms 合并窗口后推 `card_changed` 给前端；`Classify` 只认这三处，`sessions/` 等每轮都在写的运行时数据不触发刷新。命令 `watch_cards`/`unwatch_cards`（幂等，watcher 由 Tauri State 持有）。前端新增 `src/cards.ts`：共享「卡片代次」计数器，概览卡片墙、会话页署名与卡内状态随变更自动重扫。说明：解析本身不缓存（每轮从磁盘重读 card.lua），所以热加载做的是「通知」而非「重载」——改卡保存后下一条消息即用新值。新增 5 例测试（路径分类三例、队列去重、改卡即生效）

- **M1.8 SillyTavern 卡导入**：新增 `stimport.rs`——PNG 的 tEXt `chara` 块（兼容 `ccv3`，自写 base64 解码，容忍折行）与 JSON（V2/V3/裸字段）解析为草稿；字段映射把 ST 的 description 折进 scenario、personality 空时用描述兜底、`{{char}}`/`{{user}}` 占位符归一化、`mes_example` 按 `<START>` 段拆成 `example_dialogue` 问答对（拆不动的原文与 system_prompt/creator_notes 一起折进 `notes`，不静默丢数据）；生成 `charcard/1.0` 的 `card.lua`（Lua 字面量按字节转义，任意 UTF-8 安全）并在落盘后立刻用自家解析器读回校验，读不回则回滚报错。目录名清洗防路径穿越，同名卡自动 `-2` 后缀不覆盖。`character_book`/`extensions` 等未映射字段在提醒里点名（M2 设定集拆分接手）。前端新增导入向导（`ImportCardDialog.vue`）：拖入文件即弹窗（App 级 `onDragDropEvent`，因为 WebView 的 File API 拿不到本地路径）+ 手动粘贴路径 + 解析预览（设定/开场白/示例对话组数/提醒）+ 确认落盘。新增 10 例测试（V2 字段映射、示例对话拆分与兜底、V3/裸 JSON、生成物读回、恶意文本转义、最小 PNG 往返、base64 容错、同名去重、路径穿越），累计 56 例全绿

- **M1.9 首启向导与打包**：`settings.toml` 增 `wizard_done`；设置页在「还没有可用接入点」时显示三步向导（DeepSeek / Ollama 预设一键填好 Base URL 与模型名，只需补 key，本地服务免 key），保存接入点即自动收尾，也可显式「别再提示」。NSIS 安装包与 MSI 构建通过：`huajing_0.1.0_x64-setup.exe` 与 `huajing_0.1.0_x64_en-US.msi`（引入 rustls 传输层后为 3.37 MB / 4.58 MB，之前无 TLS 的 2.35 MB 版本其实发不出任何请求）

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

### Fixed

- **修复：HTTPS 传输层缺失导致一切请求失败**（用户实测报「请求失败（deepseek）：error sending request for url」）。根因是 `reqwest` 被声明为 `default-features = false` 且只开了 `json`/`stream`——**没有编译进任何 TLS 后端**，任何 `https://` 请求都在发送阶段就失败，且错误只有一句无从下手的 `error sending request`。改为保留默认特性（`default-tls` 提供 HTTPS，`system-proxy` 让请求走系统代理，`charset` 处理非 UTF-8 响应体）。同时把这类问题的可诊断性补齐：`error_chain()` 摊平 reqwest 的完整原因链（DNS / 连接被拒 / TLS 握手 / 证书不受信 一目了然）；新增 `test_provider` 命令（设置页每个接入点的「测试」按钮）发一条最小请求并回报**实际请求的 URL、耗时、机器上检测到的代理、本次实际采用的代理**；`endpoint()` 把补全地址规则收成一处（漏写 `/v1` 的已知云服务主机自动补上，自建网关不猜），自检与真实发送共用同一份规则。留了一条 `#[ignore]` 的真实出网回归测试（`HUAJING_NET_TEST=1 cargo test -- --ignored`），钉住「HTTPS 传输层被真的编译进来」——这个回归只有真实请求能发现

- **修复：走不了本机代理导致连接超时**。`reqwest` 的 system-proxy 只读 `HTTP(S)_PROXY` 等环境变量，**读不到 Windows「Internet 选项」里的系统代理**，而国内用户恰恰常在那里开着 Clash/v2ray 之类（本机实测 `ProxyEnable=1 ProxyServer=127.0.0.1:7890`）——于是直连必然超时。现在代理解析链为：设置页手填 > 环境变量 > Windows 系统代理（读注册表，并先用 300ms TCP 探测确认真的在监听，避免代理软件没开时把本来能通的直连也弄断）> 直连。`settings.toml` 增 `proxy` 字段，设置页新增「出网代理」小节。实测：自动识别 `http://127.0.0.1:7890（Windows 系统代理）` 并成功请求 `https://www.example.com/` 得到 200

- **改进：生成失败后可直接重试**。此前 `regenerate` 要求末尾必须是角色回复，而「请求失败」时用户消息已落盘、没有回复，于是只能再发一条新消息。现在末尾是用户消息时即为**重试本轮**（`plan_regenerate` 纯函数 + 单测覆盖两种裁剪）；前端在末尾用户消息上也给出「重试生成」按钮

- **修复：`on_message` 未在用户消息落盘后触发**（真机 #4「说谢谢不加好感度」的真因）。`send_message` 在用户消息落盘后直接进入流式生成，钩子只在回复落盘后跑一次——卡看到的最新消息永远是它自己的回复，于是关键词判断永远不成立，卡的 sticky 状态也来不及影响当轮生成（违背设计 §3「每条新消息落地后调用」）。修复：用户消息落盘即跑一次钩子，报告随本轮一起回给前端。该 bug 自 M1.6 起就在而单测一直是绿的——因为测试自己写了一份「用户消息后也跑一次」的等价逻辑，把生产漏掉的半步掩盖了；根治办法是把钩子流程抽成与 Tauri 无关的 `run_message_hook_core`，**生产与测试走同一份代码**，并新增断言「钩子必须看到用户消息本身」

- **修复：重roll 会重复计分**（真机反馈「点重roll 好感度一直涨」）。`regenerate` 重放那一轮的用户消息，钩子随之重放，每点一次 +1。修复：用 `turn_has_reply` 判据区分「重放」（该轮已生成过回复 → 跳过）与「生成失败后重试」（该轮没有回复 → 补跑），两种情况都符合设计 §3。同源遗留：编辑/删除历史消息仍不回滚已发生的 state 变化——彻底解法是 M2 的事件日志（设计 §7.3 可回放 / §15「同一事件流重放状态一致」）

- **修复：导入弹窗未常驻，在非「会话」页拖入文件无人接手**（真机 #6「拖进去没反应」）。弹窗原挂在 `SessionsView` 内部且只靠 `watch` 触发，仅该页存在。修复：提到应用级（`App.vue` 常驻），`cards.ts` 增显式 `importOpen`，拖放请求本身即开弹窗

- **修复：重复导入同一张卡会堆出「名字-2」「名字-3」**。`save_card_draft` 只看目录名。修复：先按**内容指纹**比对（name/scenario/personality/first_mes/tags/示例对话，不含注释——加一行注释不该算新卡）：内容完全一致即复用已有目录并报告 `reused`；同名不同内容才并存为 `-2`；`overwrite` 仍可显式覆盖。预览阶段即写进提醒，导入向导区分「新建 / 已存在并复用」

- **修复：装出来的版本看不出是哪一版**（排查时把 20:51 的旧构建误当成新代码，白绕一圈）。`build.rs` 注入编译时刻 `HUAJING_BUILD_TS`，`app_info` 与新增 `runtime_info` 返回它；侧栏底部显示「v0.1.0 · 构建 MM-DD HH:MM」，设置页新增「运行环境」小节（数据目录 / 构建时间 / 已装载卡与会话数），旧构建因缺该字段会给出明确提示

- `data_root()` 兜底改为用户数据目录（`%APPDATA%\huajing\DataHub`，非 Windows 为 `~/.huajing/DataHub`）：此前四步全落空会写进程 cwd，安装版从开始菜单启动时可能是 system32。安装版因此**不读仓库里的 `DataHub/`**——在仓库改卡不会影响安装版，这一点现在也显示在设置页「运行环境」

- **改进：失败可诊断**。错误信息摊平 reqwest 的完整原因链（DNS / 连接被拒 / TLS 握手 / 证书不受信 一目了然）；接入点新增「测试」按钮（`test_provider`）回报实际 URL、耗时、检测到的代理与本次实际采用的代理；`endpoint()` 统一补全地址规则（漏写 `/v1` 的已知云主机自动补上，自建网关不猜）；`diag.rs` 运行时诊断环形缓冲（钩子/导入/拖放的关键决策）在设置页「运行环境」直接可看——安装版没有控制台，这是唯一入口

- **改进：拖入非 PNG/JSON 文件不再静默忽略**，给出明确提示并留痕

### Changed

- 配置文件格式 JSON → TOML：`providers.json`→`providers.toml`、`settings.json`→`settings.toml`、`personas/*.json`→`*.toml`（手改友好、支持注释）；示例文件随之替换；运行时数据（session/messages/state/blackboard）保持 JSON/JSONL（追加与机器读写语义）

- 会话消息读取改 `MessageLog` 增量缓存：按字节偏移 seek 续读，每轮开销只与新增行数相关（高轮次性能）；半行（崩溃残留）留待补全后消费；文件被外部截断/重写时缓存自动重置；提供 `invalidate` 供消息编辑/删除场景使用

- `send_message` 发给 LLM 的历史加最近 40 条窗口上限（M1.4 正式预算分配的前置保护）

- 界面主题层扩展：新增 `--hj-panel-2 / --hj-line(-strong) / --hj-accent-soft / --hj-accent-ink / --hj-danger` 变量；按钮、错误条提升为全局共享样式（修复会话页按钮类未定义的问题）；统一细滚动条、选区着色、`:focus-visible` 焦点环与 `prefers-reduced-motion` 降级；顶栏激活态改为金色呼应品牌

- 会话页去掉左侧列表与搜索框（改由侧栏「会话」子菜单切换），「新建会话」按钮移到顶栏 `navbar` 的「会话」标题旁——用共享 store 的开合状态驱动原生 `dialog`，Esc / 点遮罩关闭时状态同步收回

- 界面主题改用 daisyUI 默认主题：`index.html` 去掉写死的 `data-theme`，`main.ts` 启动时先恢复 light/dark 偏好、再叠加「主题」页保存的自定义令牌（`:root` 内联变量优先于主题规则）

- 窗口顶栏改 `navbar`；侧栏收起/展开由 JS 状态驱动（大屏默认展开、窄屏默认收起，窄屏切页后自动收起），导航项与图标集合补 `Icon.vue`（内联 SVG，无图标库依赖）

- `data_root()` 在 `tauri dev`（cwd 为 src-tauri）下解析到 `src-tauri/DataHub`，现自 cwd 逐级向上查找已有 DataHub 目录

- 自定义标题栏后页面多出 36px 空白滚动条：daisyUI `.drawer-side` 固定 `height: 100dvh`，标题栏占掉一条高度后侧栏仍按整窗高撑开——改为跟随父容器高度（`h-full!`）

- 侧栏收起时出现横向滚动条：菜单项 tooltip 的绝对定位伪元素被 `ul.menu` 的 `overflow-y-auto` 当成横向可滚动内容——收起态不再把该菜单当滚动容器（`overflow: visible`），展开态保持纵向滚动

- `src/mock.ts` 的 `clearInterval(timer.timer)` 类型错误（`timer` 本身已是定时器 id），此前会让 `pnpm build` 卡在 `vue-tsc` 阶段