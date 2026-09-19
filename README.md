# 化境 Huajing

> 扮谁，便入谁之境。

**本地优先（local-first）的智能体角色扮演客户端**——角色即 Lua 脚本、数据全明文、
剧情由确定性状态机推进、世界设定是活的实体图谱。

## 架构一览

| 层 | 选择 |
|---|---|
| 壳 | Tauri 2（Rust 核心，系统 WebView，~10MB 级安装包） |
| 界面 | Vue 3 + Vite + TypeScript（IM 聊天风打底，CSS 变量主题层） |
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

首次使用：复制 `DataHub/providers.example.json` 为 `DataHub/providers.json`，填入你的 API key。

## 目录结构

```
src/                  # Vue 前端
src-tauri/src/        # Rust 核心
  card.rs             # 角色卡与 Lua 沙箱（设计 §3）
  prompt.rs           # Prompt Builder 双槽位组装（设计 §4）
  llm.rs              # OpenAI 兼容 SSE 客户端（设计 §11）
  store.rs            # DataHub 明文数据层（设计 §12）
DataHub/              # 用户数据（明文，可随身拷贝）
  characters/小雨/    # 示例角色卡
  codex/default/      # 示例世界（设定集）
docs/
  design.md           # 功能设计 v0.12（唯一权威设计文档）
  prompts/ingestion-prompts.md  # 角色卡制作提示词套件 P0–P11
```

## 路线图

- **M1 能聊**：1v1 流式对话 · Lua 静态卡 + hooks · 黑板 v0 与场景快照 · ST 卡导入
- **M2 记得住、走得稳、有始有终**：记忆宫殿 · 状态树 · 设定集 · 剧情线 · 心理运行时
- **M3 热闹、会长大、有节奏、不串台**：群聊/剧场 · 场景隔离与多线 · 设定补全 · 素材规格化
- **M4 好发布**：自动更新 · 角色/世界包 · 移动端 alpha

详见 `docs/design.md §15 里程碑`。
