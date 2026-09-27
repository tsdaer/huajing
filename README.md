# 化境 Huajing

> 化万千相，随心入境。

**本地优先（local-first）的智能体角色扮演客户端**——角色即 Lua 脚本、数据全明文、
剧情由确定性状态机推进、世界设定是活的实体图谱。

[![CI](https://github.com/tsdaer/huajing/actions/workflows/ci.yml/badge.svg)](https://github.com/tsdaer/huajing/actions/workflows/ci.yml)

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

**导入角色卡**：把 SillyTavern 的 PNG / JSON 卡拖进窗口即可（或顶栏「导入」按钮走文件选择器），
解析预览确认后生成 `DataHub/characters/<名字>/card.lua`，改卡保存即生效，无需重启。

## 安装与自动更新

**安装**：从 [GitHub Releases](https://github.com/tsdaer/huajing/releases) 下载
`huajing_<版本>_x64-setup.exe`（NSIS 安装器，Windows 10+ x64），双击安装即用。
所有数据都在安装目录旁的 `DataHub/`（明文 JSON/TOML/Lua），整目录拷走即完成备份与迁移。

**自动更新**（缺省关闭）：更新包经 minisign 签名，客户端下载后先验签再进安装器，
签名不符直接拒绝。启用只需在 `DataHub/settings.toml` 写：

```toml
[updater]
enabled = true
endpoint = "https://<托管域名>/huajing/latest.json"   # 静态更新清单的完整 URL
```

之后在设置页「关于与更新」检查更新即可（下载带进度条，装完自动重启）。
签名机制与发布者发布流程见 [docs/release.md](docs/release.md)。

## 导入与导出总览

| 内容 | 导入 | 导出 |
|---|---|---|
| SillyTavern 角色卡 | PNG（tEXt）/ JSON（V2·V3）拖入窗口，或顶栏「导入」 | ——（卡是静态文本，改 `card.lua` 即改卡） |
| SillyTavern 世界书 | JSON 导入为设定集 note 实体（资产页「导入世界书」） | 反向导出回 ST 世界书 JSON（仅静态字段，史变取现行版本） |
| 角色包 / 世界包 / 剧本包 | zip 拖入或「导入」，预览确认后落盘 | 资产页「导出角色包 / 世界包」；会话页「存为剧本包」 |
| 会话小说 | —— | 会话页「小说导出」：事件流 → 分章 `novel.md` |

同名冲突一律**先预览再决定**：默认并存（目录名加 `-2` 后缀），勾选覆盖则写回同名原目录。

## 包制作与分享指南

三包都是普通 zip（内含 `pack.json` 清单），拖进化境窗口即可安装，适合分享给其他化境用户：

- **角色包**：资产页角色卡上的「导出」——整目录打包（`card.lua` + 附属文件）。
  对方导入后得到同名角色卡，可立即开聊。
- **世界包**：设定集下钻页的「世界包」——实体 + 正史增量（grown）+ 世界时钟 + 世界线
  一次带走；对方导入即得到完整世界设定，新会话向导里可直接选它当设定集。
- **剧本包**：会话页「存为剧本包」——抽取起因（premise）+ 初始黑板 + 导演树。
  **剧本是会话模板不是存档**：不含消息历史与角色记忆，对方导入后在新建会话向导选中
  即可按同一起点开局（阵容永远用新会话自己的）。

## 移动端 alpha

桌面半场已落地：窄窗（≤480px）无横向溢出、输入区与发送键在视口内、导入走文件选择器
（拖放双通道保留）、移动 UA 下自动隐藏自定义标题栏（480×800 视口真机走查 12 断言全过）。
Android APK 构建与模拟器全链走查待环境就绪补录（本机无 SDK/NDK）；iOS 明示推迟（无 macOS 环境）。

## 持续集成与发布

自动化全部在 GitHub Actions（`.github/workflows/`），本地不装任何钩子；工具链版本（pnpm 10 / Node 22 / Rust stable）钉在 workflow 里，依赖安装统一 `--frozen-lockfile` 保证可复现。

| 工作流 | 触发 | 做什么 |
|---|---|---|
| **CI**（`ci.yml`） | push 到 main、PR、手动 dispatch | 前端构建门禁：`pnpm build`（vue-tsc 类型检查 + vite）；`cargo test` 不上云（mock 用例在无代理 runner 上有连接池死锁），496 例全绿由本地 DoD 保证 |
| **Release**（`release.yml`） | push tag `v*`、手动 dispatch | Windows NSIS + MSI 构建 → **草稿 Release**；tag 与 `tauri.conf.json` 版本不一致直接红 |

发布流程：版本三处 bump + CHANGELOG 定版段（照旧人肉写）→ `git tag vX.Y.Z && git push origin vX.Y.Z` → Actions 构建完挂**草稿** Release（安装包 + CHANGELOG 定版段为 body）→ 检查无误后手动点 **Publish** 才对外可见。试车不碰发布面：Actions 页手动跑 Release 并勾选 `build_only`，产物只存 workflow artifacts。

**签名自动更新**（M4.2）：更新包用 minisign 签名（私钥只在发布者手里，公钥内置于客户端），客户端从 `settings.toml` `[updater]` 配置的静态 `latest.json` 清单做检查 → 下载 → 签名校验 → 重启安装，缺省关闭。签名、清单生成与托管的完整流程见 [docs/release.md](docs/release.md)。

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
  plan/               # 里程碑执行计划与进度日志（m1–m5）
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
  （情绪槽/衰减/意图/自动表情）· 自动总结管线（便宜档 provider，七类产物落事件流）。
- **双槽位预算组装**（A 身份 / B 场景 / C 历史，逐层记账与确定性降级）与**记忆检查器**十页签。
- 自动化真机验收（200 轮压测 + 手动动线 + 编辑重放）通过：注入 token 相比 M1 基线 **-75.4%**，
  第 203 轮仍召回第 2 轮埋下的约定。

**M3 热闹、会长大、有节奏、不串台 完成**（`0.3.0`，2026-09-25，执行计划 [docs/plan/m3.md](docs/plan/m3.md)）：
群聊/剧场/导演树 · 场景隔离与多线「与此同时」 · 视角记忆与转述 · 设定补全管线 · 素材规格化管线
（wiki 一键成卡）· 世界主线 worldline · 语义关联层（嵌入召回作为可选第六激活源，未配置时行为不变）。
自动化真机验收（CDP 驱动真实 exe + 真实接入点）通过 DoD 全部十一项，当场修复 4 个引擎缺陷；
语义关联的门禁真机跑分待有嵌入接入点时补跑（降级路径已由单测钉死）。

已落地（M3.0–M3.10，详见 ROADMAP 与 CHANGELOG）：M2 遗留清偿 · **隔离模式与 known_by 视角化**
（每轮发言 = 发言人独立组装，仅 A 见过的事实不进 B 的任何注入层）· **场景与多线**
（「与此同时」的隔离顶层单元，切场/分场/合场/冻结全事件化）· **视角记忆与转述**
（A 告知 B → B 得到显著度折半的 hearsay 记忆并标注「转述自X」，对话听来的秘密与状态树揭示
在视角判定上闭环——串台用例已扩展到三人钉死）· **群聊与导演调度**
（发言权打分全宿主侧确定性：最近提及/场景/剧情线/想说话投票−冷却，调度史落事件流可回放，
点名直通、每轮发言数可配——不冷场不打架由计划的结构性质保证）· **主动消息与意图动态**
（`api.schedule_say` 心里话队列：意图强度过阈值 → 轮末触发入队并留触发记录，下一轮导演
优先给发言权 + B5 注入；说出口 → 意图外化开线绑定；线收结 → 受挫回流——主动消息可溯源
到意图条目）· **剧场模式与导演树**
（会话级起承转合树与卡内状态树同一套机制：director.lua 声明/缺省内置，钩子只有
开线/收线/调窗/合场四个调度动作；交叉剪辑按节奏在多路场景间轮换、合场时机归树；
阶段转移落事件流可回放，20 轮完成完整开线→收线弧单测钉死）· **世界主线与世界时钟**
（worldline.lua 声明世界阶段弧——与状态树同一套引擎，阶段转移可揭示世界设定/开世界级线；
世界时钟住 world.json 跨会话持久：轮末回写 max、多线并行不回退、flashback 不拉低、
新会话缺省续接世界时钟；注入 B1 时代行 + B2 世界段「大势压小情绪」；史变解析预览按
故事时钟回答第 N 天的事实）· **设定补全管线**
（类型模板知道每个实体缺什么：缺失 facet 高亮 +「补全」按钮生成 diff 草稿，写作论约束
内嵌提示词；收件箱确认即物化进 grown.json 正史——手写实体文件永不被机器改写；一致性
校验确定性四查 + 可选 LLM 语义矛盾检测；运行期捕获分级：瞬时状态写黑板、小事实按设置
自动、全新实体必人工；即兴模式默认关——薄实体现场补「设定·暂定」，收件箱确认转正）·
**素材规格化管线**（wiki 角色页粘贴导入 → 清洗分段/分节分类/语义归纳（P0–P11 套件内置，
逐条原文引源）→ 草稿包审阅 → 剧情切入点向导：选定时间点后未揭示的秘密只有她本人知道、
死亡后的时点以「记忆体」前提开场；落盘三路产物——card.lua / grown.json 正史增量 /
worldline.lua；ST 世界书 JSON → note 实体）·
**语义关联层**（可选第六激活源：配一条 `role = "embed"` 接入点（首选 Ollama 的
qwen3-embedding），实体卡预嵌入 + 扫描窗口实时召回，代词/描述性指称/转述这类不命中
别名的盲区也能激活对应实体——语义只产候选 id，门控与预算纪律照常；未配置时行为与
纯确定性版一字不差。总结管线新增关联审计：疑似漏激活/缺失设定进收件箱、该立的新事实
走 anchors 驳回链路；门禁评测夹具就位，净收益不显著不进热路径）·
**界面与验收收口（M3.11）**（记忆检查器多角色视角切换——状态路径/心理/宫殿/揭示集/注入层
以谁的视角看；场景操作向导：新建/分场/合场/编辑走应用内对话框，合场确认前讲清
「在场者并集、时间取较晚、记忆不合并」；自动化真机验收 DoD 十一项——群聊调度/剧场 20 拍
完整弧/串台三视角/wiki 357 秒成卡/世界时钟跨会话各就位，真机当场修复场外成员组装 panic、
卡作者 `goal` 键意图丢失、提案 codex kind 落盘被覆盖致收件箱确认从未物化、推理型模型
吃空非流式调用四个引擎缺陷，各有单测钉死）。

**M4 好发布 / M5 好用 进行中**（`0.4.x`，执行计划 [docs/plan/m4.md](docs/plan/m4.md) / [m5.md](docs/plan/m5.md)，2026-09-27）：

- **已落地**：包格式与导入导出（角色/世界/剧本三包 + ST 世界书反向导出）· 签名自动更新
  （篡改包验签即拒，真机 0.3.1→0.3.2 闭环走查）· 主题系统收尾（自定义主题落 DataHub、
  localStorage 自动迁移、主题块导入导出）· 宫殿睡眠整理（宿主确定性分组 + util 档合并稿 +
  归档不复活）· 导入文件选择器统一入口与窄窗/移动 UA 适配 · 舞台栏（阵容逐行可点名，
  聊天流拿回纵向空间）· 卡片 hover-3d 与设定集世界卡片网格 · 新建会话向导改版
  （卡片多选阵容 + 设定集选择）· 设定集 char 条目自动绑定进角色身份层。
- **进行中**：Android alpha（待 SDK/NDK 环境）与 0.4.0 发布收口。

## 路线图

- **M1 能聊**：1v1 流式对话 · Lua 静态卡 + hooks · 黑板 v0 与场景快照 · ST 卡导入
- **M2 记得住、走得稳、有始有终**：记忆宫殿 · 状态树 · 设定集 · 剧情线 · 心理运行时
- **M3 热闹、会长大、有节奏、不串台**：群聊/剧场 · 场景隔离与多线 · 设定补全 · 素材规格化 · 语义关联
- **M4 好发布** 🔄：包格式与导入导出 ✅ · 签名自动更新 ✅ · 主题收尾 ✅ · 宫殿睡眠整理 ✅ · 移动端 alpha（桌面半场 ✅）· 0.4.0 收口
- **M5 好用**：舞台栏 ✅ · hover-3d 世界卡片 ✅ · 新建会话向导 ✅ · char 绑定身份层 ✅

详见 [ROADMAP.md](ROADMAP.md)（版本策略与状态）· [docs/plan/](docs/plan/)（各里程碑执行计划）· `docs/design.md §15`（里程碑验收标准）· [CHANGELOG.md](CHANGELOG.md)。
