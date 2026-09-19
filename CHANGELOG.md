# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本段与里程碑的对应关系见 [ROADMAP.md](ROADMAP.md)。

## [未发布]

### Added

- 项目初始化：Tauri 2 + Vue 3 + Vite + TypeScript 脚手架；Rust 核心模块桩（card / prompt / llm / store / commands）；DataHub 示例数据（小雨角色卡、default 世界）；设计文档 v0.12 与角色卡制作提示词套件入库
- 工程文档：ROADMAP、CHANGELOG、M1 执行计划（docs/plan/m1.md）
- **M1.1 配置与会话骨架**：`store.rs` 数据层——providers.json 按名 upsert/删除、settings.json 读写（缺省回退）、personas 扫描（坏文件容错）；会话目录骨架（session.json / messages.jsonl / state.json / blackboard.json）与消息追加/读取；无外部依赖的时间工具（会话 id、ISO 时间戳）；命令层注册 list_providers / save_provider / delete_provider / get_settings / save_settings / list_personas / new_session / list_sessions / read_messages；前端壳导航与设置页（接入点增删改、用户人格与全局配置展示）；含 5 例数据层单元测试

### Fixed

- `data_root()` 在 `tauri dev`（cwd 为 src-tauri）下解析到 `src-tauri/DataHub`，现自 cwd 逐级向上查找已有 DataHub 目录
