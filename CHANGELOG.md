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

### Changed

- 配置文件格式 JSON → TOML：`providers.json`→`providers.toml`、`settings.json`→`settings.toml`、`personas/*.json`→`*.toml`（手改友好、支持注释）；示例文件随之替换；运行时数据（session/messages/state/blackboard）保持 JSON/JSONL（追加与机器读写语义）
- 会话消息读取改 `MessageLog` 增量缓存：按字节偏移 seek 续读，每轮开销只与新增行数相关（高轮次性能）；半行（崩溃残留）留待补全后消费；文件被外部截断/重写时缓存自动重置；提供 `invalidate` 供消息编辑/删除场景使用
- `send_message` 发给 LLM 的历史加最近 40 条窗口上限（M1.4 正式预算分配的前置保护）

### Fixed

- `data_root()` 在 `tauri dev`（cwd 为 src-tauri）下解析到 `src-tauri/DataHub`，现自 cwd 逐级向上查找已有 DataHub 目录
