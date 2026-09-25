# 加固执行计划 · 全库审查清偿（0.3.x → M4 前置）

> 来源：2026-09-25 全库审查（4 个并行审查代理分区通读 + 高危项人工复核），基线 commit `c781713`，
> `cargo test` 428 passed / 4 ignored、`pnpm build` 双绿。
> 一句话目标：**崩溃与数据丢失归零、异步生命周期有守卫、热路径告别 O(n²)。**
> 本文件自包含：执行智能体无需其它上下文即可逐项开工。行号为审查时快照，允许漂移，以描述定位。

## 使用说明（给执行智能体）

1. 按包推进（A→E），**每包一个 commit**，包内逐项独立可验。commit 消息风格照 `git log`：
   中文、动宾结构、括注要点。
2. **每项修复必须配一个钉死行为的单测**（这是本项目从 M2 起的纪律）。测试写失败场景的
   构造用例，不是重述实现。前端无单测设施，验证走 `pnpm build` + `pnpm dev`（mock 后端）
   手动构造用例；若某项值得补 vitest 可另议，不在本计划范围。
3. 每包收口门槛：`cargo test` 全绿（基线 428 + 新增）、`pnpm build` 干净、CHANGELOG.md 记一笔。
4. **只修列出的项**。「明确不动」清单里的东西是审查过确认无问题的，不要顺手"改进"；
   行为不变的重构仅限 E 包明确列出的范围。
5. 不新增第三方依赖。所有修法用现有设施（std/tempfile 除外——tempfile 已在 dev-deps，
   若原子写需要它在正式依赖里，挪到 `[dependencies]` 是唯一允许的清单变更）。

## 验收标准（DoD）

1. 构造用例全部不再崩溃/丢数据：全 `=` 行粘贴、写入中 kill 进程后重启会话完整、
   半行残留后 append 可读回、同毫秒双会话不互覆
2. 并发/竞态用例钉死：生成中并发 send_message 无孤儿消息、总结落盘不被 rewrite 覆盖、
   快速切会话不串台、组件卸载后剧场链停摆
3. LLM 管线挂死/空转归零：非流式有整体超时、流式中断保留部分文本、推理模型空正文有棘轮
4. 性能包落地后：200 轮压测会话的轮末耗时曲线不再随会话长度线性恶化（投影次数从
   每轮 10+ 次降到 ≤2 次）；`edit_message` 不再冻结窗口
5. 流程 DoD：CHANGELOG 更新 + 本文件勾选进度 + `cargo test`/`pnpm build` 双绿 +
   真机验收过一遍 M3.11 的 CDP 夹具确认无回归

---

## 包 A · 崩溃与数据安全（P0，最先做）

### A1 粘贴含全 `=` 的行 panic

- 位置：`src-tauri/src/ingest.rs` `heading_of`（约 381-385 行）
- 问题：`line[lead..line.len() - tail]`——纯 `====` 行使 `lead == tail == line.len()`，
  切片 start > end 直接 panic。调用链：`commands.rs` 把用户粘贴原文直接喂
  `clean_source → segment_sections → heading_of`。
- 修法：切片前 `if lead + tail >= line.len() { return None; }`（或改用 `line.get(..)?`）。
- 验证：单测 `heading_of("====")` 返回 None、`heading_of("== X ==")` 照常、
  纯粘贴构造用例进 `segment_sections` 不 panic。

### A2 事件流重写非原子：崩溃毁整个会话

- 位置：`src-tauri/src/store.rs` `EventLog::rewrite`（约 757-777 行，`std::fs::write`）
- 问题：`messages.jsonl` 是唯一事实来源，编辑/删除/重roll 走 truncate+写，中途崩溃/
  断电留半截文件，派生文件（state/palace/summary）无法反向重建。同款问题：
  `save_world` / `save_grown` / `save_providers`（损失小，顺带修）。
- 修法：写同目录临时文件（如 `messages.jsonl.tmp`）→ `fsync` → `rename` 原子替换。
  提一个小工具函数 `atomic_write(path, bytes)` 复用到所有落盘点。
- 验证：单测：rewrite 后文件内容正确且无 `.tmp` 残留；手动 kill 用例（可选）。

### A3 上次崩溃残留半行会把新消息粘成坏行

- 位置：`src-tauri/src/store.rs` `append`（约 726-753 行）与 `sync_entry`（794-827）
- 问题：读侧跳过尾部半行（只吃完整行），append 不看它——新行接在半行后粘成非法 JSON，
  且 `entry.pos` 按理想位置推进，缓存错位；这条消息 invalidate/重启后从磁盘读不回。
- 修法：append 在锁内若发现 `pos < 文件实际长度`（存在半行），先追加一个 `"\n"`
  封口（半行作为坏行被跳过），并把 `pos` 对齐到封口后，再写新行。
- 验证：单测：手工构造「末行无换行 + 半截 JSON」的文件 → append 一条 → 重新
  `invalidate` + read，新消息完整读回、坏行被跳过。

### A4 后台总结与前台重写竞态：总结产物被静默清掉

- 位置：写侧 `commands.rs` `spawn_summary`（约 2212 行）与 `apply_summary_outcome`
  （约 7088-7506，一批发 10-30 条事件）；覆盖侧 `regenerate`（5015 读 → 5034 rewrite）、
  `edit_message`（700 读 → 709）、`delete_message`（725 → 732）。
- 问题：前台先 `log.read` 拿快照，重放计算（百 ms 级窗口）后整体 `rewrite`；后台总结
  用独立 `EventLog` 实例在这期间 append 的 Summary/Memory 事件被旧快照覆盖。情景记忆/
  转述是模型产物，重放不可再生。
- 修法（与 B1 统一设计，**建议一起做**）：把 `CancelFlags` 升级为 per-session
  「写入互斥」——前台命令（send/regenerate/edit/delete/stop）全程持有；`apply_summary_outcome`
  落盘前尝试获取，拿不到就把整批事件暂存（内存队列或落 `pending_summary.jsonl`）稍后重试。
  退化方案（不引入新状态）：rewrite 落盘前在锁内重新 `sync_entry`，若发现 last seq 前进，
  把新增记录接在 kept 尾部再落盘——能用但语义粗糙，仅在统一锁方案受阻时采用。
- 验证：单测：模拟「read 快照 → 外部 append 若干条 → rewrite」序列，断言外部新增记录
  不丢；总结批次遇锁推迟重试的路径有测试。

### A5 palace 天数减法可溢出（debug panic）

- 位置：`src-tauri/src/palace.rs` 约 447 行 `decay_factor((q.now_day - m.story_day) as f64)`
- 问题：`story_day` 来自 palace.jsonl 反序列化，手写 `i64::MIN` 即触发。threads.rs 同场景
  已用 `saturating_sub`（420、1107 行），此处漏了。
- 修法：`q.now_day.saturating_sub(m.story_day)`。全库 grep 一遍 `story_day`/`now_day`
  的减法，同类全改。
- 验证：单测 `story_day = i64::MIN` 不 panic、衰减因子饱和到边界值。

### A6 Lua 深层嵌套 state 打穿 Rust 栈（整进程 abort）

- 位置：`src-tauri/src/card.rs` `lua.from_value` 的全部调用点：约 545、983、246、746、
  310（`eval_lua_value`，codex 实体同险）。
- 问题：mlua 0.12 serde 只有环检测、无深度上限——卡里几千层非环嵌套表（远在指令计数
  限内可构造）让宿主递归下钻直到栈溢出；栈溢出是 abort 不是 panic，错误边界拦不住。
- 修法：state/任意 `from_value` 前做**带深度上限的预检 walk**（递归下降计数，超如 64 层
  即报错降级：丢该值 + `diag::record` 日志）。实现成一个 `check_depth(&serde_json::Value, u32)`
  不适用（问题在 Lua Value 侧）——对 `mlua::Value` 写同构 walk，或先 `to_value` 成
  `serde_json::Value` 时用自定义 Visitor 限深。选实现上最省事的，深度上限常量化。
- 验证：单测：构造 100 层嵌套 Lua 表的卡，hook 写入 state，断言得到错误日志而非 abort
  （注意：栈溢出测试本身有风险，用 100 层而非 10 万层验证降级路径即可）。

### A7 数值输入硬边界（三个溢出/碰撞点）

- 位置与修法（各自独立小修）：
  - `store.rs` 约 192-194 `input_budget`：`context_window.unwrap_or(32768) * 75` 改
    `saturating_mul` + clamp（如 context_window 本身 clamp 到 1..=10_000_000）。
  - `commands.rs` `world_set_clock`（约 4000-4007，只 `max(1)`）与 `update_blackboard`
    （约 5161，day 完全不校验）：clamp 到 `1..=999_999`——`prompt::advance_clock`
    （prompt.rs 约 1155 `day + total/1440`）在 debug 构建会 panic。
  - `store.rs` 约 933-950 会话 ID = 秒+毫秒：同毫秒两次 `new_session` 同 ID、目录互覆。
    加冲突检测循环（存在即重滚毫秒/加序号）。
- 验证：三个单测分别钉：超大 context_window 不 panic 且预算合理；`i64::MAX` day 被
  clamp；同毫秒两次建会话得到不同 ID。

---

## 包 B · 互斥与生命周期守卫（P1）

### B1 生成互斥检查太晚 + flag 提前释放（一个改动修三处）

- 位置：`commands.rs` `acquire_flag`（约 2193-2208）只在 `stream_reply`（2266）里拿；
  用户消息 append（4883）与钩子落盘都在它之前。`regenerate` 的 `truncate_turn` +
  `rewrite`（5034-5036）也在 acquire 之前。flag 在 2288-2290 流结束就释放，而
  `commit_reply`（回复落盘、时钟、钩子、状态树、世界回写）在释放之后才跑。
- 问题：并发 `send_message`（双击/脚本）会完整走完 append + 钩子，到 acquire 才报错，
  留下一条无回复的用户消息且好感度等被多算一次；生成中点重roll 当场重写事件流，
  在途回复随后 append 进已截断的流；commit 阶段可插入下一轮使事件顺序错乱。
- 修法：acquire 提到 `send_message` / `regenerate` 入口（拿到 flag 再动事件流，失败直接
  返回错误）；flag 生命周期延长到 `commit_reply`/`finalize_turn` 完成后再释放。与 A4 的
  写入互斥统一成同一把 per-session 锁。
- 验证：单测：模拟并发两个 send_message（第二个在第一个 commit 完成前进入），断言流的
  事件序列严格串行、无孤儿用户消息；regenerate 在生成中被拒绝。

### B2 阵容非相邻重复不去重

- 位置：`commands.rs` 约 197-199 构造 cast，`store.rs` 486-491 只有相邻 `dedup()`。
- 问题：`["b","a","b"]` 让同一角色每轮跑两次钩子、双份记忆写入、黑板 actors 重复。
- 修法：`new_session` 用保序去重（`Vec` + `contains` 或索引集）。
- 验证：单测：非相邻重复名单建会话，cast 无重复。

### B3 前端：编辑期间删除更早的消息 → 保存改写错误的消息

- 位置：`src/views/SessionView.vue` 约 874-900（`startEdit`/`removeMsg`）；后端
  `edit_message` 按全量下标定位（commands.rs:691 `locate_message`）。
- 问题：编辑第 5 条 → 删第 2 条（列表前移）→ 保存落在原第 4 条上，错误内容被改写且
  从该轮重放。
- 修法：`messages` 被任何外部路径替换时（`removeMsg`、`switchTo`、`onSceneSubmit`）一律
  `editingIndex.value = -1`。更稳的做法是编辑目标改用消息身份（turn+role+内容哈希），
  但涉及后端协议，本轮先做守卫，身份定位记为后续。
- 验证：mock 后端手测：开始编辑 → 删除更早一条 → 编辑框关闭而非错存。

### B4 前端：快速切换会话串台

- 位置：`SessionView.vue` `loadAll`（约 640-668，await 后直接写 `messages` 不校验 id）、
  `loadScenes`（469-481）；`SessionsView.vue` 约 104 行 `<SessionView :meta=…>` 无 `:key`。
- 问题：A 的迟到响应覆盖 B 的消息/场景；`loadAll(A)` 恢复执行时读到 B 的
  `props.meta.id`，产生 A 消息 + B 场景混合态。
- 修法：每次 `await` 后 `if (id !== props.meta.id) return;`；`SessionsView` 加
  `:key="selectedSession.id"` 强制按会话重建（双保险）。
- 验证：mock 手测：连点两个会话来回切，消息与场景始终属于当前会话。

### B5 前端：生成中切会话，旧流收尾掐灭新会话流式区

- 位置：`SessionView.vue` `sendText`（734-755）、`onDelta`（773-788）、
  `finishGeneration`（828-866，结尾无条件 `generating=false; streams=[]`）。
- 修法：`sendText`/`reroll` 开头捕获 `const sid = props.meta.id` 与自增世代号（组件级
  `ref` 或普通变量）；`onDelta` 与 `finishGeneration` 开头校验 `sid === props.meta.id &&
  世代号未变`，否则丢弃。
- 验证：mock 手测：A 生成中切 B 并发消息，B 的流式不被掐灭、A 的 token 不出现在 B。

### B6 前端：剧场自动轮次在组件卸载后继续烧预算

- 位置：`SessionView.vue` `autoAdvance`（406-412）、`finishGeneration` 尾部自续链
  （852-858）。
- 问题：开着剧场切走页面，promise 链继续驱动 `THEATER_CONTINUE` 真实调用 LLM 直到预算
  耗尽，无 UI 可停。
- 修法：组件加 `disposed` 标记，`onUnmounted(() => { disposed = true;
  void api.stopGeneration(props.meta.id).catch(() => {}); })`；
  `autoAdvance`/`finishGeneration` 开头检查断链。
- 验证：mock 手测：剧场开着切到概览页，网络面板确认不再发轮次请求。

### B7 前端：世界时钟校准写到错误的世界

- 位置：`SessionView.vue` 约 111-121 `calibrateWorld` 调 `api.worldSetClock("", day)`。
- 问题：后端把空 world 落 `"default"`，而面板显示 `session_world(meta)`（会话自己的
  世界）——非 default 世界校准无效。
- 修法：传 `props.meta.world ?? ""`。
- 验证：mock 手测非 default 世界会话的校准落对文件（真机或检查 invoke 参数即可）。

---

## 包 C · LLM 健壮性（P1/P2）

### C1 非流式调用无整体超时：管线永久挂死

- 位置：`src-tauri/src/llm.rs` `build_client` 只设 connect_timeout(15s)（约 657-660）；
  `chat_once`（277-309）、`chat_once_full`（376-430）、`embeddings`（480-515）的
  send+read 无超时。commands.rs 各调用点（总结/补全/嵌入管线）均无外层 timeout。
- 问题：服务端回 200 后停住不回 body → 永久 await，不报错不重试，总结批次卡死到重启。
  注意 builder 上加 `.timeout()` 会误伤流式——不要那么做。
- 修法：在这三个函数内用 `tokio::time::timeout(Duration::from_secs(120), ...)` 包住
  整个 send+body 读取，超时映射为可读错误。
- 验证：单测：mock 一个「回 200 挂住」的服务端（本地 TcpListener），断言 120s 超时……
  120s 太久——把超时值参数化（私有常量 + 测试注入小值），测试用 100ms。

### C2 流式中途失败丢弃已生成文本

- 位置：`llm.rs` `chat_stream`（约 224-254，循环里 `?` 直接返回 Err 丢弃累积的 `full`）；
  调用方 commands.rs（约 2279-2290）Err 时只发 `StreamEvent::Error`。
- 问题：网络抖动时代码里已显示的流式文字凭空消失、不落盘。
- 修法：中途失败把部分文本带回——错误类型带 `partial: String`（如
  `struct StreamError { message: String, partial: String }`），调用方按 cancelled 语义
  决定落盘或至少在前端保留已显示文本 + 错误提示。
- 验证：单测：SSE 流中途喂一个 io 错误，断言 partial 携带此前全部增量；调用方落盘
  部分回复的事件流用例。

### C3 流式路径没有空正文棘轮

- 位置：`llm.rs` 约 224-254 `StreamChunk` 完全忽略 `finish_reason`，也不设 `max_tokens`。
- 问题：推理模型在 provider 默认预算内把 token 耗在 reasoning 上，正文为空但流正常
  [DONE] 结束 → `Ok("")` → 不落盘、前端无回复无错误。非流式 `self_or_retry`
  （349-373）专门处理了这个场景，流式没有等价物。
- 修法：解析末 chunk 的 `finish_reason` 带出（`StreamOutcome` 加字段）；空正文 +
  `finish_reason == "length"` 时由调用方带更大 `max_tokens` 重试一次（复用
  `retry_budget`）。
- 验证：单测：构造「reasoning 耗尽、finish=length、正文空」的 SSE 流，断言触发重试；
  重试成功后正文非空。

### C4 client 无缓存 + `chat_complete` 开头死代码

- 位置：`llm.rs` `chat_complete`（316-333，`client`/`url`/`body` 建了不用）；`build_client`
  每次调用重复代理探测——Windows 上每次 spawn `reg` 进程（约 544 行，阻塞调用还在
  async fn 里）+ 300ms TCP 探测（607-613）；预算棘轮一次重试最多 5 遍完整探测。
- 修法：删掉 `chat_complete` 开头死代码；按 `extra_proxy` 参数缓存 client
  （`OnceLock<Mutex<HashMap<Option<String>, Client>>>` 或 Tauri State）；`proxy_from_system`
  里的 reg 查询挪进 `spawn_blocking` 或缓存结果。
- 验证：单测：两次 `build_client` 同参数返回复用实例（可从探测次数/或直接断言缓存
  命中）；行为级验证——chat_complete 结果不变。

### C5 SSE 缓冲无上限 + 头部 drain O(n²)

- 位置：`llm.rs` `SseParser`（约 136-163）。
- 问题：服务端持续输出不含 `\n` 的字节（坏网关 200 非 SSE 体）→ `buf` 无界增长直到内存
  耗尽；`buf.drain(..=pos)` 从头部逐行 drain，单 chunk 多行时 O(n²)。
- 修法：feed 后检查 `buf.len()` 上限（1-4MB）超限报错断开；扫描时记已消费偏移一次性
  `drain(..consumed)`。
- 验证：单测：喂 >上限的无换行字节断言报错；多行单 chunk 的解析结果与逐 chunk 一致。

### C6 `lua_str_utf8` 变长十进制转义有歧义，静默损坏 worldline.lua

- 位置：`src-tauri/src/ingest.rs` 约 1899-1917（1911 行
  `out.push_str(&format!("\\{}", c as u32))` 生成 `\1` 而非 `\001`）。
- 问题：Lua 的 `\ddd` 最多吞 3 位——控制字符后跟数字时被吞并错位（`\x0c` + `"3"` →
  `\123` → 读回成 `!`）。stimport.rs 566-583 的 `lua_str` 用定宽 `format!("\\{b:03}")`
  是正确写法。
- 修法：统一为定宽 `{:03}`；两处如有重复可提取公共函数。
- 验证：单测：含 `\x0c` 后跟数字的字符串往返（生成 lua → mlua 读回）无损。

### C7 `improv_candidates` 提及判定写反一半

- 位置：`src-tauri/src/complete.rs` 约 126-142
  `e.name.to_lowercase().contains(&hay) || hay.contains(&e.name.to_lowercase())`。
- 问题：第一个条件是「实体名包含整段对话文本」——短文本（「猫」）误激活一片实体；
  空名实体 `hay.contains("")` 恒真全入候选，触发无谓即兴 LLM 调用。
- 修法：删第一个反向 contains，只留 `hay.contains(name)` 与别名匹配；跳过空名实体。
- 验证：单测：「猫」不再激活名字含猫的实体；空名实体不入候选。

### C8 预算棘轮注释与实现漂移

- 位置：`llm.rs` 约 336-339 注释说「换 4 倍预算（封顶 16384）重试一次」，实现是 ×2、
  封顶 32768、`depth < 4`。
- 修法：只改注释对齐实现（行为不动，测试 llm.rs:701-708 钉的是实现）。

---

## 包 D · 热路径性能（P2，收益最大但动结构）

### D1 每条事件全量投影 + 6 派生文件全量重写（最大热点）

- 位置：`commands.rs` `commit`（约 472-480）= append + `sync_now`（461-469）=
  `project_session`（全量 `project_over`，克隆全部消息/记忆）+ `sync_derived`
  （423-458，写 state.json/blackboard.json/palace.jsonl/summary.md/proposals.jsonl/
  scenes.json 六个文件）。一轮对话 commit 10-30 次；后台总结一批再叠 10-30 次。
  投影 O(流长度)，单轮 O(n)，全会话累计 O(n²)。
- 修法：分两步——
  1. `commit_batch(&[LogBody])`：一批事件一次文件写 + 一次投影 + 一次派生同步。
     改造调用点：`finalize_turn` 各效果提交、`apply_summary_outcome`、
     `run_message_hook_core` 的钩子效果、`decide_all_proposals`。单事件路径保留
     `commit` 作为 batch=1 的包装。
  2. 增量投影：`event::fold` 本就支持增量——EventLog 缓存 `(last_seq, Projection)`，
     `project_session` 先查缓存、seq 前进只 fold 新记录。注意 rewrite 后缓存失效。
- 验证：既有 428 测试全绿是硬门槛（投影语义不变）；新增：计数器测试断言一轮
  （构造用例）全量 fold 次数 ≤2；200 轮压测前后轮末耗时对比记录进 CHANGELOG。

### D2 finalize 路径重复全量 fold

- 位置：`commands.rs` `finalize_turn`（约 2820、2841）、`advance_worldline`
  （3723/3731/3801）、`advance_theater`（3164/3233/3285/3301）、`sync_world_now`
  （3877）、`run_message_hook_core` 每成员（2457/2469）——单角色轮全量投影 12+ 次。
- 修法：finalize 内传递同一份 `proj`，批量 commit 后用 `event::fold` 增量推进，不重新
  `project_session`。依赖 D1-2 的缓存后此项大半自动消失，残余调用点手动改。
- 验证：同 D1 计数器测试覆盖 finalize 路径。

### D3 非 async 重命令跑在主线程，冻结窗口

- 位置：`commands.rs` `edit_message`（约 691，全历史重放钩子）、`new_session`（180）、
  `ingest_commit`（4284）、`decide_all_proposals`（6727）等为同步命令——Tauri 同步命令
  默认跑主线程。
- 修法：重命令改 `#[tauri::command(async)]`（函数体不用动即移出主线程）。顺带审查
  `lib.rs` 的 invoke_handler 注册表，把纯 IO/重计算命令全部 async 化。async 命令里的
  纯 IO 段若仍重，用 `tauri::async_runtime::spawn_blocking` 包裹（谨慎：注意锁不要跨
  await/spawn 边界——本项目纪律是锁只在同步函数内获取释放，别破坏它）。
- 验证：真机/CDP：长会话（200 轮）里点编辑消息，窗口不冻结、动画不掉帧。

### D4 EventLog 全局单锁且持锁做磁盘 IO

- 位置：`store.rs` 约 683-691 `Mutex<HashMap<...>>` 全会话共享；`read`/`append` 持锁
  做 metadata/open/seek/read/write（704-753、794-827）。
- 问题：会话 A 的大文件增量读阻塞会话 B 的一切命令与后台总结。
- 修法：`HashMap<String, Arc<Mutex<LogEntry>>>` 按会话分锁；锁内只做缓存操作，文件 IO
  移出临界区（append 的「读偏移-写-推进」需保序：每会话一把写锁覆盖，粒度已缩到单
  会话）。与 A3 的封口逻辑一起动 store.rs，建议同批。
- 验证：既有 store 测试全绿；并发压测（两个会话交错 read/append）无死锁、无错位。

### D5 palace 召回三处优化（每轮必跑）

- 位置与修法（`src-tauri/src/palace.rs`）：
  - 449-452：`dedup_tags(&q.hints)` / `present` / `mentions` / `active_threads` 在
    `score_of` 内部被每条记忆重复算（每次 O(k²)）——提升到 `recall` 顶层算一次传入。
  - 410-415：打分阶段整卡 `m.clone()`（含 content 全文）后才截断——先排引用
    （`Vec<(&MemObject, f32, Vec<String>)>`），截断后仅 clone 幸存条。
    `witnesses_or_actors()`（399→132-138）同理避免每条 clone。
  - 396-409：`seen_ids.iter().any()` 线性扫——改 `BTreeSet<String>`（保持确定性，
    不引入 HashMap 迭代序）。
- 验证：召回结果与改前逐字节一致（把既有召回测试的期望快照当基准）；长会话
  （千条记忆）召回耗时对比记录。

### D6 codex 揭示源/在场源全量线性扫

- 位置：`src-tauri/src/codex.rs` 1577-1584（每 reveal 字符串对全部实体全部 secrets 扫
  `revealed_by`）、`find_by_ref`（1363-1365 经 1573 调用，全实体×(id+name+别名)
  `eq_fold`）、1594-1621（在场源每 actor×全实体×别名）——每轮每角色执行。
- 修法：`Codex::build` 时建两个倒排：`revealed_by 值 → 实体 idx`、`fold(名字/别名) →
  idx`（by_id 已有，补折叠键）。注意 build 缓存按指纹走的既有机制照常。
- 验证：激活结果与改前一致（既有 codex 激活测试为基准）。

### D7 prompt 裁剪整层反复重渲染

- 位置：`prompt.rs` C3 循环（379-388，每步重 join 剩余全部行 + 全量 estimate_tokens）、
  entity_layer/recall_layer 每降级一条整层重渲染（940-955 / 985-990）、codex.rs 预算
  循环每轮重算 total（1777-1831）。
- 修法：C3 改累加（每收一条只算该行 token + 换行）；卡层维护「整层 token 总量」增量
  增减。有界常数优化，排 D5/D6 之后。
- 验证：组装结果与改前一致（prompt 组装快照测试为基准）。

### D8 `story_time_at_turn` 每条记忆 O(n) 全量扫

- 位置：`commands.rs` 7045-7061；`apply_summary_outcome` 每条 episode/hearsay/fact 各
  调一次（7129、7180、7241）→ O(n×k)。
- 修法：一次遍历构建 `Vec<(turn, day, clock)>` 前缀表后二分。
- 验证：与改前取值一致（对拍单测）。

### D9 `decide_all_proposals` 循环内重复投影

- 位置：`commands.rs` 约 6755：每确认一条提案 `project_session` 全量 fold 只为取
  last turn——turn 根本不变。
- 修法：turn 取一次；配合 D1 批量落。
- 验证：既有提案测试全绿。

### D10 前端流式渲染：每 token 强制布局 + 全组件重渲染

- 位置：`SessionView.vue` `onDelta`（约 780，每 delta `void scrollToBottom()` → 617-621
  `await nextTick(); el.scrollTop = el.scrollHeight` 强制布局）；`streams` 是根组件级
  ref（328），模板 1080-1192 全部同一渲染上下文，每 delta 整个 1797 行组件重渲染。
- 修法：滚动用 `requestAnimationFrame` 合帧（一帧只滚一次），且仅当用户已接近底部时
  自动跟随；把「消息列表 + 流式区」抽成子组件（与 E1 的拆分一起做最划算），使 delta
  只重渲染流式气泡子树。
- 验证：mock 下长会话流式肉眼顺滑；Chrome DevTools Performance 无逐 token 布局尖峰。

---

## 包 E · 结构与边缘（P3，最后做，行为不变）

### E1 大文件拆分（两个）

- `commands.rs` 11991 行：先做零风险的提取——命令前奏三连 `root()`+`load_session`+
  `project_session` 重复约 20 处（5144、5260、5287、5352、5408、5464、5541、6013、
  6104…）提 `fn session_ctx(id)`；代理设置读取块重复 8 处（115、1271、2274、4265、
  5659、5769、5855、7580）提 `fn proxy_of(root)`；`commit(...LogBody::Proposal(...))`
  骨架重复 ~10 处提小构造器。然后按域拆文件（providers/settings、scenes、
  theater/worldline、summary、ingest 已有 `*_core` 天然分界）。**分批小步，每步双绿。**
- `SessionView.vue` 1797 行：抽 `InspectorDrawer.vue`（script 124-330 + template
  1348-1783）、`SceneBar.vue`（434-552 + 1016-1075）、`useChatStream` composable
  （725-870，顺带落 B5 世代守卫）、`useTheater`（381-412，顺带落 B6 断链）、
  `CardStatePanel.vue`（277-323 + 1659-1781）。

### E2 死代码与重复加载

- `commands.rs` 6132 `resolve_thread_at` 里 `loaded` 加载后从未用（每次手动收线白读
  一次卡 + Lua 解析）——删。
- `commands.rs` 4086 `let _ = &name;` 无操作——查证后删。
- `commands.rs` 6535 + 6589 `inspector_payload` 对同一世界 `load_codex` 两次——合并。
- `card.rs` 1014-1022 `card_has_state_hook` 每问一次重跑整卡；树缓存（TreeCache，经
  commands.rs:1440）的 `state_tree_shape` 产物里有 `has_enter/has_exit`——改用缓存。

### E3 watch pusher 线程泄漏

- 位置：`watch.rs` 159-178 pusher 线程持 `Arc<Queue>` 死循环；`unwatch_cards`
  （117-121）只 drop watcher。
- 修法：WatcherHandle 持 pusher 侧终止信号（`Arc<AtomicBool>` 或队列弱引用），unwatch
  时置位让线程退出。

### E4 前端小项合集

- 界面事件双份入列：`SessionView.vue` 781-783（实时 `hook_event`）与 806-811
  （`applyReport` 再推 `done.report.ui_events`）——`applyReport` 不再重推，或按
  `turn+kind+value` 去重。事件流页签每条只出现一次。
- `stop()`（868-870）未 catch——try/catch 写 `error.value`；`removeMsg`（892）的
  `window.confirm` 换应用内确认（项目 M3.11 已把 prompt/confirm 收进对话框，见
  SceneDialog.vue 头注释）。
- 分页页脚计数口径（1200 用全量 `messages.length`，分页/空态基于 `sceneMessagesView`
  441-459）——页脚改用 `sceneMessagesView.length`。
- `IngestView.vue` 532 `v-for :key="p.day"` 同天冲突——改 `:key="p.name"` 或加下标。
- `App.vue` 87-111 与 `cards.ts` 41-52 的 listen unlisten 丢弃——保存并在适当时机调用
  （TitleBar.vue:29 是正确范例）。

### E5 Rust 小项合集

- 场景合并时钟按字典序（`scene.rs` 145 `(day, clock.as_str())` 比较；黑板手写 `9:00`
  vs `10:00` 同日判错）——parse 成分钟再比（codex.rs:891 `parse_hhmm` 可复用）。
- A 组裁空仍发空白 system 消息（`prompt.rs` 617-621，三个 A 子层全空时 `join` 出
  `"\n\n"` 仍 push）——空层省略（B 层已这么做）。
- `current_affects` 只去相邻重复（`prompt.rs` 1083）——改全量保序去重。
- default_state 含函数值整体清空（`card.rs` 244-249）——先剔除函数值再转。
- state 写环静默整体回退无日志（`card.rs` 545/983 `unwrap_or`）——失败分支补
  `diag::record`。
- 黑板变更检测用 Debug 字符串整体比对且返回值无人消费（`event.rs` 997-1036、
  1046-1095）——删比对或改逐键比较（二选一，别留半吊子）；注意
  `apply_blackboard_sets_scoped` 只比对 `world` 的语义别改。
- stimport 落盘校验失败回滚残留空目录（`stimport.rs` 632-646）——回滚连带删空目录。
- `ingest.rs` 1596 `contains_key(stage) || !stage_days.is_empty()` 左支恒不起作用——
  按注释意图改 `contains_key(stage)`（先确认意图，别改反语义）。

---

## 明确不动（审查确认无问题，别顺手改）

- **Mutex 无跨 await 持锁**：所有锁在同步函数内获取释放，`.await` 点无守卫存活——保持。
- **trie/嵌入索引不每轮重建**：`Codex::build` 按指纹缓存（commands.rs:1157-1165）——
  已是正确形态。
- **psyche.rs 数值入口**全有 `clamp01`/`is_finite` 兜底、排序带名字破平——干净。
- **statetree 环/悬空父截断、threads 的 saturating 语义、semantic 余弦零向量**——与
  文档一致。
- **codex known_by 视角过滤链**口径一致；mention 源不设生命周期门、semantic 源设
  `presentable` 是文档写明的刻意差异，非缺陷。
- **PNG 解析边界防护、SSE 跨 chunk 拼接（多字节/CRLF/[DONE] 拆半）、LLM 输出解析器
  无 unwrap**——有完整单测，勿动。
- **总结后台任务的字节偏移缓存同步**（截断检测 `len < pos` 重置）——除 A4 的 rewrite
  竞态外自洽。
- `plan_speakers` 非空保证（`picks[0]` 安全）、`plan_regenerate` 的 unwrap 有 match
  守卫、event.rs 的 expect 有前置守卫——保持。

## 进度勾选

- [x] 包 A 崩溃与数据安全（A1–A7）
- [x] 包 B 互斥与生命周期守卫（B1–B7）
- [x] 包 C LLM 健壮性（C1–C8）
- [ ] 包 D 热路径性能（D1–D10）
- [ ] 包 E 结构与边缘（E1–E5）
- [ ] 收口：CHANGELOG + 双绿 + CDP 真机回归
