# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本段与里程碑的对应关系见 [ROADMAP.md](ROADMAP.md)。

## [0.3.0] - 进行中

> M3「热闹、会长大、有节奏、不串台」进行中。执行计划见 [docs/plan/m3.md](docs/plan/m3.md)。

### Added

- **M3.10 语义关联匹配器（设计 §6.13）——代词与「穿旧毛衣的那位」不再漏，未配嵌入服务时行为一字不差**：
  - **`semantic.rs`（纯算法，纪律同 codex.rs——不碰网络）**：余弦相似度、实体嵌入索引
    （top-K 过阈值，分数降序 id 升序破平）、实体检索文档（名 + 一句话 + facts 择要，
    **secrets 不入文档**）、Qwen3-Embedding 查询侧指令前缀；嵌入候选只是**候选 id**——
    canon/when/lifecycle/known_by 门控与 B3 预算降级阶梯照常生效，没有任何绕过纪律的通路
  - **`llm.rs` 非流式 embeddings 客户端**：OpenAI 兼容 `/v1/embeddings`（Ollama 同协议），
    按 `index` 排序回填、断档/缺 embedding 的脏响应拒绝（向量对错实体是静默串台，宁可报错）
  - **第六激活源（可选）**：`providers.toml` 新增 `role = "embed"`（未配置 = 整层关闭，
    全部既有单测不改语义通过——降级即设计）；实体索引挂在设定集指纹缓存旁（实体文件 /
    grown.json / 模型名任一变了才重嵌），每轮扫描窗口嵌一次取 top-5 过 0.45 阈值；
    权重与「提及」同档、深度 1 行起，混合计分取 max（确定性命中总是压得过语义分）；
    激活原因带「语义:0.62」进检查器；首次启用把「provider/模型」记进会话元数据
    （可回放语义随版本声明；预览干跑不写盘）
  - **关联审计（总结管线第七类产物）**：便宜档对照「本批消息 + 设定集实体清单 + 当轮
    激活记录」报告疑似漏激活（代词/描述性指称）/ 缺失 facet / 该立的新事实——前两类是
    收件箱提示条目（kind="audit"，确认与否决只做记录），第三类并入标准 codex 提案链路
    （anchors 冲突自动驳回 + 分级 + 物化，与管线产物同一条路）；产物事件化，重放不重调模型
  - **门禁评测先行（M3.10a）**：`docs/eval/semantic-gate.jsonl`——M2 压测语料里 14 条
    trie 盲区人工标注（代词/描述性指称/转述三类，text 全取原句）；`EvalReport` 给出
    分盲区类别的召回率与误报率；真机跑分一条命令
    （`HUAJING_NET_TEST=1 cargo test -- --ignored`，DataHub 配 embed 档即跑），
    **净收益不显著就不把语义源开进热路径**；默认阈值 0.45 / top-5 据跑分再调
  - 前端：设置页接入点新增「embed · 语义关联」用途档 + Ollama 嵌入预设；收件箱渲染
    审计条目（发现类型 / 目标实体 / 引源）；mock 同步审计样例
  - 累计 423 例 Rust 单测 + `pnpm build` 双绿（M3.10 新增 17 例：semantic.rs 9
    ——含 1 例真机门禁跑分默认忽略、llm.rs 3、codex.rs 源 6 语义 2、commands.rs
    接线与审计 2、summarize.rs 1）

- **M3.9 素材规格化管线（设计 §6.7）——wiki 角色页粘贴导入，十分钟产出可开聊角色**：
  - **`ingest.rs`（纯数据与算法，纪律同 complete.rs）+「素材导入」页四步向导**：粘贴 →
    清洗分段（确定性）→ 分节分类（LLM P1）→ 语义归纳（LLM P3–P8，逐条附原文引源）→
    草稿包审阅 → 切入点落盘；mechanics（面板/圣痕/数值）在选材层一律过滤，拿不准的节归
    待定（宁漏勿错）
  - **清洗分段（② 确定性）**：`clean_source` 剥 HTML 注释/ref/模板壳——信息框类模板
    （含 ≥2 个 `k=v` 参数）展开成行保留给机械映射、其余模板丢弃；`extract_spoilers` 在
    清洗**前**扫剧透标记（`{{黑幕|…}}`/spoiler/heimu class）收秘密候选；
    `segment_sections` 按 MediaWiki/Markdown 标题切节，空父节的标题以面包屑并入子节
    （「经历 · 往世乐土」）
  - **机械映射（④ 确定性）**：信息框字段→schema（本名/别称/外貌/所属/相关人士/个人状态，
    拿不准的键进待定）；个人状态→lifecycle（死亡是末段正史，不阻碍从更早切入点扮演）；
    台词表按竖线/冒号拆行；**P9 装配不走 LLM**——P2–P8 的结构化产物由 `assemble_pack`
    确定性拼装（占位实体、事件实体、卡侧字段、待定与质检一次成形），比让模型吐 Lua
    更可靠也更便宜；dotted facet 路径折叠成嵌套 facts（与 codex schema 同构）
  - **查重冲突（⑥）**：commit 时对既有设定集复用 `validate_proposal`/anchors 驳回链路
    ——id 重复跳过、同名实体警告、悬空关系警告，全部带进落盘报告
  - **切入点向导（⑧ · DoD 7）**：`build_canon_points` 从世界线阶段 + 死亡时点推导可扮演
    时间点（死亡前必有选项；死亡后的选项自动带「记忆体/残留」剧本前提——「与已死者对话」
    双处理）；选定后 `apply_canon_point` 烘焙切面：**该时点未揭示的秘密 known_by=本人、
    已揭示的公开**（解析不出揭示时点的秘密永远按未揭示处理，宁可开局少一条公开事实）；
    切入点早于最早史变记录的 facet 出告警（早期取值未知，按素材终值注入）；lifecycle
    带生效天落值，缺时点的死亡不落值（防「任何时点都不在场」）
  - **落盘三路产物**：卡走 ST 导入同一条路（`characters/<名>/card.lua`，可解析性验证、
    同内容复用）；实体走 `grown.json` 正史增量（与收件箱确认同一条物化链，确认即进注入）；
    世界线写 `worldline.lua`（stages 糖，渲染产物过归一化验证，已有主线默认不覆盖）；
    可选把世界时钟拨到切入点
  - **P0–P11 提示词套件内置**：`include_str!` 打包进产物，`ingest_prompts` 命令 +
    「手动模式提示词」弹窗复制——手动·分步/一键流零代码可用（套件文档仍是双用途权威）
  - **ST 世界书导入补课（M2.2 欠账 · §6.10）**：`import_worldbook` JSON → note 实体——
    标题→name、关键词→别名（提及激活）、正文→注入文本、constant 保留、禁用条目按
    draft 收档；order/sticky/cooldown/keysecondary 留档 `facts.st` 不执行不丢弃；同书
    重导按 id 幂等跳过；`{entries:{…}}`/`{entries:[…]}`/裸数组三种形态都认
  - 累计 407 例 Rust 单测 + `pnpm build` 双绿（M3.9 新增 24 例：ingest.rs 20、
    stimport.rs 2、commands.rs 2）；浏览器 mock 走通四步向导冒烟

- **M3.8 设定补全管线（设计 §6.8）——设定随剧情长大，但每一笔都过人眼、有出处、可回滚**：
  - **模板驱动的手动补全（§6.8-1）**：类型模板（§6.2 的 facet 清单）知道每个实体「缺什么」
    ——检查器里缺失 facet 琥珀色高亮 +「补全」按钮；`codex_complete` 读该实体 + 一跳关系 +
    世界概览 + 故事前提 + **世界主线时代基调（M3.7 联动）**，为缺失 facet 生成草稿；生成规范
    内嵌写作论约束（anchors 2–4 宁少而精、口癖必须能落进示例对话、tells 覆盖高频情绪、
    by_affect 与气质一致）；diff 卡片逐条呈现（确定性校验驳回的灰显剔除、警告的带现值），
    接受走提案通道（origin=complete，动作可溯源）
  - **正史物化（DoD 8「确认写正史进注入」的兑现）**：收件箱确认的提案物化进世界级
    `codex/<世界>/grown.json`——新实体 = 全量骨架、既有实体 = facts 深合并 / aliases 追加 /
    relations 去重；`apply_grown` 合并**幂等**（重放安全，单测钉死）；**玩家手写的实体文件
    永不被机器改写**（增量与手稿分离，撤一行即回滚）；设定集缓存指纹混入 grown.json
    （确认后下一次加载即生效）
  - **一致性校验升级（§6.8-3）**：`validate_proposal` 确定性四查——id 重复（驳回）、悬空关系
    （警告）、字段冲突带**正史现值**（警告）、anchors 冲突（驳回）；补**可选的 LLM 语义矛盾
    检测**（「前文黑发后文棕发」类，收件箱按需触发，便宜档对照实体现状判断，结论随确认/
    否决时的备注留档进事件流——重放不重调模型）
  - **运行期捕获分级自动接受（§6.8-2）**：总结管线的设定提案从四类扩为五类（新增
    `transient`，判断标准「明天还成立吗」）——瞬时状态直接写场景 flags（不进收件箱）；
    既有实体的小事实按设置「自动接受设定小事实」（默认关）自动确认并物化；全新实体 /
    关系 / 改写一律进收件箱人工裁决
  - **即兴模式（§6.8-4，默认关）**：会话输入区「即兴」开关——本轮被提及的实体过薄
    （缺 3 项以上模板 facet）时，便宜模型顺着会话种子现场补一条「设定·暂定」注入 B2；
    注入行从提案投影回读（重放/重建同一行还在，**不重调模型**）；anchors 冲突丢弃、
    失败静默不挡说话；收件箱带「暂定」徽标，确认即转正史
  - **收件箱 UI（DoD 8）**：diff 双源呈现（正史现值 vs 提案值，各带出处）、来源徽标
    （管线/补全/暂定）、批量全部确认/否决、语义检查按钮；提案投影补 origin 字段
  - 累计 383 例 Rust 单测 + `pnpm build` 双绿（M3.8 新增 13 例）

- **M3.7 世界主线与世界时钟（设计 §6.6）——世界不随会话生灭：大势跨会话持久，压着所有角色**：
  - **`worldline.rs`（纯数据与算法）+ `codex/<世界>/worldline.lua`（可选层）**：世界主线 =
    **世界作用域的状态树**——与卡内状态树/M3.6 导演树同一套引擎，零新代码。设计 §6.6 的
    `stages` 列表糖由 card.rs 沙箱内的归一化适配器折成 `state_tree`（声明式
    `when = { day = N }` → 世界时钟判据函数；声明式 `on_enter = { reveal, open_thread }` →
    动作收集函数；直接声明 `state_tree` 的剧本包写法原样透传）。**缺文件/坏文件回落
    「无主线」**——纯日常世界照常运转，不让一个手滑的配置瘫痪整个世界
  - **世界时钟持久（world.json）**：`codex/<世界>/world.json` 是世界级单例——故事天、
    主线进度（路径 + 推进轮次 + 哪个会话推的）、世界级线快照、未知键原样保留（flatten）。
    会话**轮末回写**（而非等「会话结束」）：`max(世界, 本会话最远场景)`——语义与结束回写
    完全一致（max 单调）且崩溃/强退不丢；**多线并行不回退、flashback 会话不拉低**由
    纯合并函数 `worldline::sync` 保证（时钟只增、进度只前进、线按 id upsert，单测钉死）。
    新会话建会话表单不填天 = **开局基准取世界时钟**（显式填天 = flashback/指定时点）
  - **阶段转移落流（第 13 类事件 `WorldlineEvent`）**：from/to 路径 + 理由，折叠进
    `Projection.worldline`（当前活跃路径 = 最后一条的 to），is_derived=false——
    **世界大势的走位史重建不丢**。会话首次轮末「承袭」世界进度落第一条走位事件，
    **不重跑该阶段的 on_enter**（揭示/开线在推进到这里的会话里已经发生过，换会话不重演）；
    三级回落定当前路径：会话走位史 → world.json 进度 → 树根
  - **世界层动作白名单**：阶段钩子只有 `api.reveal(…)`（无见证者 = 全局知情——大势对
    所有人可见）与 `api.open_thread { id/title/cause/importance }`（**世界级线
    scope=world，没有 actor，任何会话可见可推进**；轮末回写 world.json，别的会话经
    B1「心里有事」/C1 未决清单感知并接着推）两个动作——主线是世界层元层，
    不碰任何角色的私有 state/memory/黑板
  - **注入（大势压着小情绪）**：B1 现状卡新增「时代」行（「公告期——公告已贴出…」，
    叶阶段 directive 摘要）；B2 指令层**世界段拼在角色 directive 之前**（世界是指令树
    的超根）；无 worldline 的世界两项全无，注入与 M3.6 一字不差
  - **导演树联动（M3.6 遗留清偿）**：判据环境补 `worldline_stage` 字段——「公告期不排
    纯搞笑日常」写成 `st.worldline_stage == "公告期"` 一条 when 即可（树写法不变）；
    母层查询子层，轮末主线先推进、剧场后求值
  - **史变收尾（设计 §6.5）**：`codex_resolve_preview` 命令——按故事时钟回答
    「第 N 天的事实」：versions 逐条标注在第 N 天是否生效、生命周期按生效时刻判定
    （**flashback 回到生效前逝者仍在场**）、retired 留档照常列出（死亡是正史变更，
    不是删除）；设定集面板新增「史变预览」区（天数控输入，缺省当前故事天）
  - **UI**：检查器新增「世界」页签（主线阶段/时代行/世界时钟与会话时钟对照/校准入口/
    世界级线含「大势将至」声明占位）；新建会话「第几天」可空（占位「续接世界时钟」）
  - **单测钉死（DoD 第 6 项单测侧，真机归 M3.11 收口）**：
    `worldline_stage_transition_reveals_opens_and_injects`（day 门槛转移 + 揭示全局知情 +
    开世界级线 + B1 时代行/B2 世界段进注入 + world.json 双回写 + 走位史重建不丢）、
    `world_state_persists_across_sessions_and_flashback_never_regresses`（新会话承袭阶段
    与世界级线、承袭不重演 on_enter、flashback 拉不低时钟、更浅进度不覆盖世界）、
    `without_worldline_the_world_still_runs_and_clocks_persist`（无主线零注入差异但时钟
    照常回写）、`director_tree_judges_on_the_worldline_stage`（自定义导演树靠联动转段）、
    `resolve_preview_serves_versions_and_lifecycle_by_story_clock`（第 10 天取旧版事实/
    第 25 天取新版/生效前后在场判定/retired 留档）；worldline.rs 另有 sync 合并语义、
    era 行渲染、树解析五例；card.rs 另有 stages 归一化、世界时钟判据（19 天不转 20 天
    含当日转）、钩子动作收集三例。
    累计 370 例单测 + `pnpm build` 双绿

- **M3.6 剧场模式与导演树（设计 §8.5/§10.5）——起承转合控节奏，交叉剪辑控场面，目标函数是「预算内走完一条完整的开线→收线弧」**：
  - **导演树（会话级状态机，Lua 走卡沙箱）**：与卡内状态树同一套机制——`sessions/<id>/director.lua`
    声明 `state_tree`（剧本包 v0 形态），缺省用**内置起承转合树**：起（铺陈 + 开线）→
    承（生长）→ 转（主动制造反转线）→ 合（并场收束）。判据环境是导演专属的合成
    state 表：`turns_left`（预算余量）/ `stage_turns`（本段已走轮数）/ `threads_active`
    ——**小预算也能走完弧**由 `turns_left` 阈值的预算压力转移保证（目标函数的兑现形式）
  - **调度动作白名单**：导演树钩子（on_enter/on_exit）只有四个动作——`api.open_thread`/
    `api.resolve_threads`/`api.resurface`/`api.merge_scenes`，全部只收集不执行，宿主逐条
    落事件：开/收线走线的生命周期（origin=director，**与 manual 同为元层动作、重建不丢**，
    收线三件事——结果记忆/线事件/驱动角色状态树——对导演收线一视同仁）；resurface 落
    retune 事件（**窗口调度权**：earlier = grade 升 eager 很想找机会说，later = 降 dormant
    先放着别提）；合场走场景内核（origin=director）。开线缺省标题取**在场者最强的未外化
    意图**——主线从角色心里长出来，不凭空杜撰；收束只动导演自己开的线（管线/心理外化的
    线有各自的生命周期）
  - **阶段转移落流（第 12 类事件 `DirectorTreeEvent`）**：from/to 路径 + 理由，折叠进
    `Projection.director_tree`（当前活跃路径 = 最后一条的 to），is_derived=false——
    **起承转合的走位史回放可重现**；剧场开场先「播种」进树根（跑 on_enter 并落第一条
    走位事件），当轮不再求值转移（刚进的状态站得住一轮）
  - **交叉剪辑（intercut）**：宿主侧确定性轮换——同一场景连续推进 3 轮（`INTERCUT_CADENCE`）
    后切下一路（候选 = 全部未归档场景，冻结的分路可被切回——切回即解冻，§10.3「剧场模式
    下可由导演继续自动推进」）；每次转场先落 `director` 事件（op=cut，记缘由）再切场
    （origin=director，带「与此同时——」过渡插页）。**合场时机由树掌管**（合段的
    `api.merge_scenes` 把全部分路收回），轮换在只剩一路后自然停止
  - **剧场模式 UI**：会话内「剧场」一键开跑（`set_theater`，轮数预算缺省 20、记开场轮次进
    session.json）——剧场条显示当前阶段、导演指令与 `used/budget` 轮进度，可随时「收棚」；
    预算内每轮结束自动推进下一轮（前端循环，中性拍点「（剧场继续）」与草稿互不干扰），
    导演切场后自动对齐聚焦场景；`theater_view` 命令返回进度/阶段/是否自定义树
  - **单测钉死（DoD 第 2 项单测侧，真机归 M3.11 收口）**：
    `theater_twenty_rounds_completes_a_full_arc`（20 轮内起承转合走位齐全 + 导演开的线全部
    在合收束且结果记忆进宫殿 + 走位史/导演线事件重建不丢）、
    `intercut_rotates_between_two_scenes_and_merges_on_the_final_act`（分场后两路按节奏轮换
    ≥2 次、合段并成一路归档另一路、弧完整性不受交叉剪辑影响）、
    `custom_director_tree_resurface_tunes_grade_and_survives_rebuild`（director.lua 覆盖默认树
    + 调窗落 retune 事件 + 延后=dormant 且导演收束不动 manual 线 + 重建不丢不翻倍）；
    director.rs 另有默认树四段求值预算压力、intercut 轮换节奏、钩子声明四例。
    累计 354 例单测 + `pnpm build` 双绿

- **M3.5 主动消息与意图动态（设计 §9.2）——意志从内隐到外显的完整闭环，每一步可溯源**：
  - **`api.schedule_say(text)` 接线（§3.1 声明至此落地）**：卡在 hooks 里说一句
    「让她下一轮主动发消息」——心里话进**卡私有 state 的 psyche.scheduled 队列**
    （条目带憋下的轮次，持久化与重放走既有 state patch 通道，零新事件类型）。
    她真正开口的回复落盘时队列才消费，且只消费**更早憋下的话**——同一轮刚憋下的
    留到下一轮，「下一轮主动发消息」由此精确成立（1v1 与群聊同一语义）
  - **意图动态闭环**：`intent.strength ≥ 生效阈值`（temperament.threshold − 冲动性加成
    − 余量）且**状态窗口开启**（她在场、场景未冻结）→ 轮末宿主替她把心里话入队；
    意图说出口（消费心里话）→ 宿主为最强的未外化意图**开线并绑定 `linked_thread`**
    （origin = psyche——说出口是历史事实，消息级重建不丢；线已活跃则直接绑上去）；
    **线被收结 → 回流评价**：绑定了这条线的意图削弱（−0.35，削到 0 即移除）、
    意图主人记一条「受挫」情绪（来源标注哪条线了结）——评价闭环合拢
  - **主动行为可溯源（DoD 3）**：触发那一刻的「哪一轮、何阈值、多强、触发了什么」
    记在意图的 `triggered` 字段上；一条意图只催一次（不轰炸）；心理面板意图卡带
    「主动行为」触发记录，**主动消息能指回意图条目**；新增「憋着没说的心里话」小节
    （队列逐条可查）
  - **导演调度同槽加权（M3.4 预留位兑现）**：心里话队列非空 = `want_to_speak` 满票
    （×2.0 权重），憋着话的人在下一轮优先拿到发言权；B5 同槽注入「【心里话】你憋着
    一句话想说：…」——发言权优先（调度侧）与说出口的样子（说话侧）分头到位
  - **修复既有缺陷**：`apply_message_hook` 此前把 on_message 效果事件归给主角色
    （`first_character`）——群聊里她人的钩子写入会记错人；现归给真正跑钩子的角色
    （M3.1 隔离的题中之义，群聊心里话用例当场暴露）
  - 验收单测：`intent_loop_triggers_speaks_and_externalizes`（阈值下不触发 → 剧情推过
    阈值 → 触发记录齐全 → B5 注入 → 消费 + 外化开线 + 重放一致）、
    `scheduled_say_earns_the_floor_in_group_chat`（心里话满票拿发言权 + 无意图不开线）、
    `resolving_linked_thread_refluxes_frustration`（受挫 + 削弱恰为 0.35、未绑线意图
    不动）；psyche/card 模块另有 schedule/consume/触发记录/沙箱 API 八例。
    累计 347 例单测 + `pnpm build` 双绿

- **M3.4 群聊与导演调度（设计 §10.5）——发言权是宿主确定性算法，不是提示词恳求**：
  - **发言权打分**：新增 `director.rs`（纯数据与算法，不碰文件不碰网络）——每轮对在场成员
    按四路信号打分：**最近提及**（本轮点名 +3.0，最近窗口被谈到 +1.0）∪ **场景黑板关联**
    （在场名单 +1.0）∪ **活跃线关联**（线 actor +0.8，线正被谈到加码 +1.5×重要度），
    加 **`want_to_speak` 投票**（M3.4 取意图强度 ×2.0；M3.5 的 `api.schedule_say` 接进来后
    同槽加权），减**冷却**（最近 3 条回复里每说过一次 −1.2）。逐条理由可读
    （「被点名提及」「剧情线「X」正被谈到」「刚说过话（冷却）」）——「为何轮到她」说得清；
    同分按「更久没说话」再按阵容次序收尾，同样的输入必然得到同样的计划
  - **不冷场、不打架是结构保证**：即使所有信号平平照样按稳定次序产出计划（导演永不沉默），
    冷却让刚说完话的人自然排到没说过话的人后面；计划互不重复、按每轮上限截断
    ——**两人同抢一条发言权在数据结构上不可能**
  - **发送路径**：`send_message` 的发言权三分——显式 `speaker` = **点名直通**（只他一人接话，
    M3.1 语义不变）；`None` 且阵容多人 = **导演调度**（打分选出 1–N 位**按序**接话，
    后发言者的组装含先发言者刚落盘的回复）；`None` 且单角色 = 主角色（1v1 不走导演，
    行为与 M2 一致）。`commit_reply` 拆为回复级（落盘 + 时钟 + on_message）与轮末级
    （心理 / 状态树 / 总结）——群聊一轮多人时**轮末推进恰好一次**，「默认转移推迟到轮末」
    的轮是用户的一轮，不是某条回复
  - **调度落流**：新增 `director` 类事件（`DirectorEvent`：op / picks（dir + name + score +
    reasons）/ direct）——与场景事件同理**不随消息级重建丢弃**，回放可重现整场调度史；
    fold 对它是 no-op（调度只决定谁说话，不产生会话状态）。时间线摘要（「调度 →
    小雨（被点名提及 · 剧情线「X」正被谈到）、阿澈（想说话）」）与检查器新增**导演面板**
    （调度史逐条可查）
  - **每轮发言数可配**：`SessionMeta.max_speakers`（缺省 2）+ `set_max_speakers` 命令 +
    发言权选择旁的限流旋钮——导演调度天然限流，对冲隔离模式的请求成本
  - **前端**：发言权选择（**导演调度** / 逐人**点名** 页签，多角色缺省导演调度）；
    **调度指示**（生成前先出「导演：小雨（被点名提及）接话」的插页行——第一个字出现前
    「谁在说话、为何轮到她」就有答案）；流式气泡按发言人分组（`Delta` 事件带署名，
    换人自动开新气泡）；气泡头像按署名取稳定色相，多人同台一眼可分
  - **重roll 适配一轮多回复**：重roll 末位发言人——同轮更早的回复保留为上下文
    （不换人重roll，M3.1 语义延伸）；「重roll 不重复计分」的结构保证不变
  - 验收单测：`director_schedules_speakers_logs_and_survives_rebuild`（三人阵容：点名提及
    优先 + 冷却轮换 + 调度事件时间线可读 + 重建不丢 + 重放投影一致）、
    `director_event_roundtrips_and_is_never_derived`（JSON 往返与派生判定），
    `plan_speakers` 六例（点名压冷却 / 平凡轮换 / 线关联与投票排序 / 冻结场景成员
    无发言权 / 确定性 + 截断 + 不重复 / 零信号仍产出）。累计 339 例单测 + `pnpm build` 双绿

- **M3.3 视角记忆与转述（设计 §10.4）——信息跨视角流动的唯一通道**：
  - **转述（hearsay）生成**：总结管线新增 `hearsays` 产物——本批剧情里 A 把某事告诉了 B 时，
    记录「谁讲的（source）、听的人现在知道了什么（content）、原事件显著度（salience）」；
    宿主为**每个听众各写一条** `kind = hearsay` 的记忆对象（witnesses = 她自己），
    **salience 折半**（`palace::HEARSAY_SALIENCE_FACTOR`，听来的不如亲历的刻骨）、
    links 从原事件继承、盖**告知发生时刻**的故事章（`story_time_at_turn`，与 M3.0 ⑤ 同口径）。
    召回行尾自带「转述自X」来源标注（B4 渲染既有能力，M2 预留），翻旧账有据可查；
    没在 listeners 名单里的角色召不回这件事——视角过滤是硬约束
  - **揭示闭环**：转述提案可带 `reveals`（这番话顺带揭示的秘密路径）——宿主落
    `origin = pipeline` 的 reveal 事件、见证者 = 听众，听众的视角知情集（`known_of`）增项，
    M3.1 的深卡判定随之闭环：**听过秘密的人深卡对她展开，没听过的人照旧关门**。
    `is_derived` 相应收窄：Codex 事件只有 `origin = tree` 算派生（重建时由状态树重导），
    pipeline 揭示是模型产物，与转述记忆同理编辑历史不丢
  - **管线提示词**：`hearsays` 进输出骨架与产物规格（转告/坦白/透露的判别要点：
    content 从听者视角写、听众不含告知者本人、salience 填原事件值宿主才折半、
    只讲给一个人听的事不写别人）；解析容错与 sanitize 归一（空正文/无听众丢弃、
    听众剔除告知者、夹紧去重、上限 6 条）各有单测钉死
  - 验收单测：`hearsay_reaches_only_the_listener_with_halved_salience`（串台用例扩展到三人）——
    小雨目击事件（亲历记忆 salience 0.8、witnesses=[小雨]）、阿澈通过对话得知
    → 阿澈的 B4 出现「转述自小雨」且渲染显著度 0.40（约为亲历一半）、
    转述揭示的工作牌秘密对阿澈展开深卡；小玲的 B4 召不回转述、B3 深卡不展开（始终不知情）。
    累计 331 例单测 + `pnpm build` 双绿

- **M3.2 场景与多线（设计 §10.3）——「与此同时」的隔离顶层单元**：
  - **场景模型与事件**：新增 `scene.rs`（纯数据与算法，不碰文件不碰网络）与 `SceneEvent`
    （create / switch / split / merge / freeze / resume / update，不随消息级重建丢弃，
    回放可重现整场切场史）。场景本体即黑板分区：地点、在场者、**场景局部时钟**（被切走的场景
    时间停住，切回原地继续）、场景 flags。事件流新增 `scene` 类事件，投影折叠出场景表、
    聚焦场景与摘要分卷；派生文件新增 `scenes.json`（明文可查）
  - **黑板分世界层 + 场景分区**（向后兼容）：时间基准与实体作用域键（`char.小雨.status`）归
    世界层——设定集是世界的知识，不随场景走；地点/在场者是**这个舞台**的属性，落场景分区。
    `apply_blackboard_sets_scoped` 按 `EffectEvent.scene_id` 路由钩子写入；世界层黑板保持
    「聚焦场景的镜像」供旧读侧与派生文件使用。**老会话零迁移**：`scene_id: None` 读侧归一到
    缺省场景 `scene.main`，无场景事件的会话一切读侧退化为世界层（M2 行为不变）
  - **消息流分段**：新会话建会话即落地缺省场景；用户消息 / 角色回复 / 过渡插页都带 `scene_id`。
    组装的历史只取**发言人所在场景**的分段——别的场景发生的事是「与此同时」的叙事盲区；
    发言人必须在本场景（被切走的场景冻结，不在场的人不能开口）
  - **冻结语义**：切场自动冻结被切走的场景——不在场的角色不跑钩子、不推心理、不求值状态树
    （`present_members` 一处判据）；时钟步进只推进**发言场景的局部时钟**，经投影同步世界层镜像
  - **场景命令**：`list_scenes` / `create_scene` / `switch_scene` / `split_scene` / `merge_scenes` /
    `update_scene`。切场与分场插入**小说式过渡插页**（「与此同时，图书馆东侧——」，system 消息、
    归属目标场景、重放不重跑钩子）；分场扣走离场者（原场景至少留一人）；**合场 = 在场者并集 +
    时间取较晚一路 + flags 冲突聚焦方赢 + 被并入场景归档留档，各角色记忆不合并**（各自记得
    自己那条线里的事——多线的戏剧价值），合场对齐经 UI 确认
  - **摘要按场景分卷**（设计 §10.4）：`SummaryEvent` 带 `scene_id`，投影按场景记账、水位按场景
    独立；批次判定场景维度（最老的场景先沉淀；冻结场景的消息本来就不在任何上下文窗口里，
    过水位即可总结）；总结管线新增 `chronicle` 产物——从场景批次提取**仅公开事件**的世界层
    大事记，跨场景组装时可读；组装注入 = 世界层大事记 + 本场景分卷，别的场景的分卷不进请求。
    `summary.md` 派生文件分卷排版
  - **前端**：会话页新增**场景切换条**——场景卡（标题 / 局部时间线 / 冻结·已并入徽标）点击切场、
    新建 / 分场 / 合场入口；过渡插页居中衬线斜体渲染；消息流按聚焦场景分段显示（编辑/删除仍按
    全量下标）；黑板面板编辑的地点/在场者路由到聚焦场景分区；浏览器 mock 同步双场景演示
  - 验收单测：`two_scenes_never_leak_into_each_other`（双场景黑板/消息流/摘要三路互不污染）、
    `split_then_merge_keeps_memory_per_viewer_and_replay_identical`（分场扣人、合场并集与归档、
    合场零记忆写入、投影重放一致）、`scene_events_survive_message_level_rebuild_semantics`
    （场景事件是手动事件，编辑历史后场景表原样保留）、`rebuild_reroutes_hook_writes_to_their_scene`
    （编辑重建时钩子写入按消息所在场景重演）等；累计 326 例单测 + `pnpm build` 双绿

- **M3.0a 面板补全（M2 真机验收遗留 ①–④ 清偿）**：
  - **剧情线手动开/收线 UI**（设计 §8.3）——剧情线面板接上 `open_thread` / `resolve_thread` 命令：「开线」按钮展开表单（标题 / 起因 / 涉及 / 重要度），每条活跃线带内联「收线」（输入结果文本，收线三件事一次做完）；收线后现状卡与 C1 即时刷新
  - **宫殿记忆溯源跳转**——记忆行（房间图 / 时间线 / 最近记忆三处）可点击，跳回产生这条记忆的原文轮次并翻到对应分页（M2 DoD 第 6 项的缺口补上）
  - **界面事件轮次修正**——`StreamEvent::HookEvent` 现在带上产生它的钩子轮次（后端知道，此前前端从乐观上屏的 `turn: -1` 消息推算，显示「第 -1 轮」）；报告落地路径同样带 `report.turn`
  - **类型化事件流视图**——新增 `session_timeline` 命令：把 `messages.jsonl` 的九类事件摊成人读视图（`seq / kind / turn / brief`，最新在前、默认 200 条，消息署名、转移带原因、线带标题与 origin）；检查器「事件流」页签改以它为主视图，`api.ui.emit` 界面事件收编为折叠子段。含单测（最新在前 / seq 递减 / limit 截取）
  - 浏览器 mock 同步覆盖三个新命令，并补全 mock 线对象缺的 `opened` / `scope` 字段（此前浏览器模式打开剧情线面板会渲染报错）

- **M3.1 隔离模式与 known_by 视角化（设计 §10.1/§10.2/§10.4）——多角色会话的地基**：
  - **角色阵容**：`new_session` 接受 `characters`（全阵容入席、初始黑板 actors 全阵容、
    开场白带署名）；`send_message` 接 `speaker`（本轮由谁回应，缺省主角色）；
    `regenerate` 从被重roll 回复的署名推断发言人。1v1 会话走同一条代码路径，行为与 0.2.0 完全一致
  - **每轮发言 = 发言人独立的上下文组装**：`assemble_prompt_core` 按 `Cast + speaker` 视角化——
    on_context 钩子全阵容各跑（黑板写入是公开事件），B 槽注入只取发言人那份；
    B2 指令层按**该角色**的状态树活跃路径（转移事件带 `character` 字段，路径按角色分道）；
    B3 知情集 = 全局揭示 ∪ 只对她的揭示；B4 召回 viewer = 发言人；A1 多角色时加
    「你只扮演 X；其余角色的言行只是你听到、看到的公开事件」
  - **秘密成为「某些人知道的事」**：reveal 事件带 `witnesses`（在场者 ∪ 揭示者本人），
    投影折叠出按视角的揭示集 `known_of`；`known_by` 名单授予名单内视角（她一直知道自己的秘密）；
    深卡注入按「当前组装视角是否知情」判定。补掉 M2 的潜在缺口：生产组装的 `reveals`
    此前恒为空，秘密内容实际从未进过注入——新增激活源「实体已激活且该视角见证过其秘密 → 升深卡」
    （只升级不激活，先天 known_by 仍只做门控，预算语义不变）
  - **全阵容轮末循环**：on_message / 心理推进 / 状态树求值 / 编辑重建（rebuild_from）/
    手动收线驱动的转移，全部按角色各自执行；char 消息带 `name` 署名（事件流与气泡一致）
  - **前端**：建会话表单可多选同台角色；输入区上方「对谁说」发言人页签（多角色时出现）；
    消息气泡署名取 `message.name`；会话头「+N 同台」徽标；浏览器 mock 同步
  - 验收用例：`isolated_assembly_never_leaks_witness_only_facts_to_the_other`——双卡会话、
    阿澈离场时小雨的树揭示工作牌秘密（witnesses=[小雨]）→ 阿澈视角的 B3 深卡与 B4 均无该秘密，
    小雨视角照常可见；A1 隔离提示与回复署名断言齐全

### Fixed

- **M3.0b 引擎治理（M2 真机验收遗留 ⑤⑥⑦ 清偿）**：
  - ⑤ **批次总结的情景记忆盖事发时刻的故事章**，不再是总结时刻——`story_time_at_turn` 从事件流按轮回放黑板（轮末异步总结时黑板已推进了很多轮，此前时间线视图把记忆排错桶、时间衰减从错误起点淡去）；L3 事实同样改盖批次末的章
  - ⑥ **同 key 的钩子记忆不再逐轮堆条目**——`api.memory.set` 的键值事实在进宫殿时按 key 合并成一条（最新值），写入次数进 `rehearsals`（召回打分的再提及增强本来就用它）；`last_thanked` 类每轮都写的键不再以 0.50 显著度稀释 B4 的有效容量，读侧「同 key 后写覆盖」语义两侧对齐
  - ⑦ **编辑历史不再丢「收线驱动的转移」**——手动收线事件是手动事件（重建保留），但它驱动的转移是派生事件（重建丢弃），钩子重跑不带 `thread:<id>:resolved` 触发器；`rebuild_from` 现在在重放区遇到收线事件时对着它补求值一次状态树（与 `resolve_thread_at` ③ 同构），重放点之前的不补（其转移记录本来就在）。单测钉住「编辑后转移不丢失、不翻倍」

## [0.2.0] - 2026-09-20

> M2「记得住、走得稳、有始有终」完成定版（代码侧 M2.0–M2.8 全部落地，2026-09-20 自动化真机验收通过）。
> 执行计划与验收证据见 [docs/plan/m2.md](docs/plan/m2.md)；真机发现的 6+1 项非阻断遗留归 M3.0 清偿。

### Added

- **M2.0 事件日志与投影（设计 §7.3「可回放」/ §6.9 / §12）**：新增 `event.rs`——`messages.jsonl` 从「消息流」升级为**类型化事件流**，一行一个事件：`message`（消息）/ `effect`（钩子副作用：state 顶层键补丁 + 黑板写入 + 记忆写入）/ `blackboard`（init·manual·clock）/ `transition`（状态树转移，M2.3）/ `thread`（剧情线，M2.4）/ `codex`（设定揭示与确认，M2.2）。事件带 `seq`（按位置分配）与 `kind` 判别字段；**M1 写的无 kind 行照旧按消息读**，旧会话不做破坏性迁移。新增 `Projection`（投影）：消息列表、各角色 state、黑板、宫殿、转移、剧情线快照、秘密揭示集全部由事件流**纯函数折叠**得出，`state.json` / `blackboard.json` / `palace.jsonl` 一律由投影写出——**落盘只剩一条路径**。`store::MessageLog` 升级为 `store::EventLog`（字节偏移增量读不变，新增 `append` / `rewrite`）。25 例新测试（事件往返、旧行兼容、投影折叠、确定性重放、state 补丁、genesis 与老会话基线、手动事件与派生事件的区分、半行/坏行/序号重排、揭示集、线快照后写覆盖）

- **M2.0 消息级操作可回滚**：编辑/删除历史消息现在会**重放重算**（`rebuild_from`）——改写或移除该消息事件后，丢掉它所在轮次起的派生事件，再按卡顺序重跑这些轮的 `on_context` / `on_message`（含轮末时钟步进），钩子是 (消息, 黑板, state) 的纯函数，因此重放得到一致状态。**M1 的同源遗留就此关闭**：改掉那句「谢谢」，好感度会跟着退回去，记忆也一并消失。老会话（事件流里没有 genesis）首次消息级操作时**从头全量重放并补上 init 事件**，就地升级为事件溯源会话（此后回滚精确；老会话的黑板已是终态，故重放不再叠加时钟步进）。重roll 的截断抽成 `truncate_turn`（命令与单测共用），「不重复计分」从启发式判据 `turn_has_reply` 变成结构性保证——旧效果已不在流里，重放恰好一次

- **M2.1 记忆宫殿（引擎）**：新增 `palace.rs`——记忆对象 v1（kind/content/time/place/actors/witnesses/salience/emotion/links/thread/rehearsals，手写反序列化以兼容 M1 的 `fact` 行与设计 §5.2 的嵌套时间形态）、显著度打分（salience × 0.5^(Δ故事天/7) × 关联加成(≤3.0) × 再提及加成(≤1.5)，权重全部具名）、**视角过滤**（viewer 必须 ∈ witnesses，设计 §10.4 硬约束）、四类关联命中（recall 提示/地点/在场者/最近提及 + 活跃线）、`render_memory_block`（「回忆·第3天 23:40」框架 + 转述标注）、三视图（房间/时间线/关联图）与 `search`。31 例单测（衰减半衰期、视角过滤、四类命中、权重上限、legacy 兼容、同分确定性排序、预算截断等）

- **M2.2 设定集（引擎）**：新增 `codex.rs`——实体 schema（char/place/item/event/org/rule/concept/note + facts/secrets/live/relations/lifecycle/variants/versions）、自建别名 trie（最长优先、CJK 精确、拉丁大小写不敏感，未引 aho-corasick 依赖）、**五激活源**（提及/在场/揭示/关系牵引/常驻，全部宿主侧确定性）、**三级注入深度**（1 行/卡片/深卡 + `look.anchors` 恒注入）、滞回（上一轮激活者保底卡片）、预算降级阶梯（深卡→卡片→精简卡→1 行→裁撤，**anchors 最后被裁**）、`variants` 条件变体与 `versions` 史变（按故事天解析：第 10 天与第 30 天取到不同事实）、`fill_placeholders`、`anchors_conflict`（提案与辨识点冲突即驳回）。32 例单测

- **M2.1/M2.2 接入组装与运行时**：`prompt.rs` 新增 **B3 设定集**与 **B4 回忆**槽（`<world>` / `<memory>` 标签，空层省略；`PromptLayer.sources` 逐卡记录激活原因）；`commands.rs` 新增世界加载（`codex/<世界>/entities/*.lua|*.json`，`.lua` 走同一套 Lua 沙箱解析、坏文件跳过并留诊断）与**目录指纹缓存**（改文件即生效，与热加载同款判据）、会话级跨轮运行时（上一轮激活集合，供滞回）、召回查询构造（视角=本角色、故事天/地点/在场者/提及窗口来自黑板与别名扫描）；`api.memory` 读侧（`HookEnv.memory`）接上宫殿（M1 里这一侧恒空）；**黑板扩展出实体作用域键**（`api.blackboard.set('char.小雨.mood', …)` → `live` 字段拼出 `▸当前:心情不错`，读侧同时给平铺与嵌套两种形态）。新增端到端单测：别名提及激活进 B3 + anchors 恒注入 + 逐卡激活原因 + 卡写记忆进 B4 + 作用域键进 `▸当前`。记忆检查器逐层展示激活原因（`SessionView`）

- **M2.4 剧情线（引擎 + B1/C1 接入）**：新增 `threads.rs`——线模型（起因/经过/结果 + `resurface` 提及时机）、生命周期（开线/推进/升格/收线/放弃）、四种窗口（黑板键值·话题擦边·在场者·状态路径，多键对象按「与」组合）、克制梯度 `dormant/natural/eager`、冷却抑制、deadline 到期升格（`due_threads` 保证 `thread:due` 只抛一次）、确定性排序（档位 → 重要度 → 开线轮次 → id）。29 例单测。接入：`prompt.rs` 的 **B1** 新增「心里有事(时机合适时可自然提起)」与「了结未远」，**C1 未决事项**作为历史区的只读投影（全量欠账随时可查，注意力位只放窗口内的线）；`commands.rs` 从事件流投影出线、构造窗口查询（故事天/地点/在场者/话题词表=设定集命中实体 + 最近窗口原文片段）。端到端单测钉住 M2 验收第 3 条：**窗口未命中绝不进 B1、dormant 即使被提及也不进、收线后进「了结未远」并从 C1 消失**

- **M2.5 心理运行时（引擎 + B5 接入）**：新增 `psyche.rs`——情绪槽（≤3，同向叠加、槽满替换最弱者、过低即消退）、气质参数（升降速/阈值/冲动性，四组预设）、意图强度（慢衰减、可绑定剧情线）、自动表情映射、`summary_line_for(角色)` 的内心摘要与衰减轨迹（历史采样环形截断）。37 例单测。接入：B5 新增「内心」层（`【小雨·内心】喜悦0.6 ▸ …`，空则省略）；**轮末推进**（情绪按气质衰减、意图慢衰减）结果作为 effect 事件落盘，因此消息级重放会重新长出同一份心理状态；自动表情走既有 `ui.emit` 通道。顺带修掉读侧不一致：`prompt.rs` 的 A3 情绪差分与 `codex.rs` 的状态词表此前读 M1 的单数键 `psyche.affect`，现在同时认规范复数键 `psyche.affects`（心理运行时只写复数，避免 state 两份数据）。新增验收单测：强情绪三轮后仍有余波（0.9 → 逐轮衰减）、B5 内心层带角色名与情绪名、衰减轨迹随轮次增长。另：**测试脆弱点修复**——「钩子必须看到用户消息本身」不再依赖全局诊断环形缓冲（进程共享，并发跑测试会被冲掉），改为行为断言

- **M2.3 状态树 v1（引擎 + 运行时接入）**：新增 `statetree.rs`（状态节点/转移/路径/directive/recall/reveal/校验，纯数据）+ `card.rs` 沙箱求值入口（`eval_state_tree` 按 priority 升序求值 `when`，函数式与 `event:` 简写；`state_tree_shape` 取结构；`run_state_hook_full` 跑 on_enter/on_exit）。实现要点：候选 = 叶 + 祖先继承（同 priority 叶先）、**首个命中即转移且一轮只转一次**（轮末求值，一轮内状态稳定）、目标为当前叶自身视为无效、`when` 可查黑板/state/`codex.known·active`/`threads.active·resolved`、死循环 `when` 被沙箱杀掉且原地不动。运行时接入：**B2 指令层**（活跃路径 directive 根→叶拼装，父在前子覆盖父）、`recall` 提示喂宫殿召回权重、转移执行按 §7.3-3（exit 钩子 → 切路径 → enter 钩子）且每步副作用都作为事件落盘、进入新路径即 `reveal`（写 codex 事件 → 投影的揭示集）。23 例新单测（10 statetree + 13 card）
- **M2.3 接入验收单测**：`state_tree_drives_directive_and_transitions`——条件不满足不转移且 B2 是根指令 → 钩子把好感度/时钟推过阈值后**轮末**恰好转移一次（本轮组装仍是旧状态，下轮换新）→ B2 变根→叶拼接（父在前）→ `reveal` 进揭示集 → on_enter 的 state/黑板作用域键/ui 事件都落定 → **重放同一事件流得到同一批转移与同一份揭示集**

- **M2.6 自动总结管线**：新增 `summarize.rs`（引擎侧，33 例单测：六类产物的提示词构建、json 围栏与前后杂文容错解析、夹紧与截断、windows 可解析性过滤、跨模块断言）。运行时接入：**批次判定**（滑出 L0 窗口 40 条之外、且未被此前摘要覆盖的消息，OOC/system 不进剧情记忆）→ **便宜档 provider**（util 档，缺则回退 chat）→ 产物落事件：摘要增量（→ `summary.md`，进 **C1**）、情景记忆（结构化 `MemObject` → 宫殿 **B4** 召回）、L3 事实键值、**设定提案**（写入前先过 `anchors_conflict`，与辨识点冲突自动驳回并留诊断）、**剧情线提案**（含提及时机起草）与心理评价提案（草稿→确认，`decide_proposal` 落事件）。触发为**轮末异步**（不阻塞对话，同会话并发去重），另有 `summarize_now` 手动触发。关键裁决：摘要/提案/情景记忆是**模型产物**，重放（编辑历史的重建）永不丢弃它们——可回放性承诺针对状态/转移/心理，不针对模型输出
- **M2.7 双槽位预算与降级**：`prompt.rs` 按设计 §4.2 落地预算表（A 8% · B1 2% · B2 3% · B3 12% · B4 8% · B5 2% · C1 10% · C2 5% · C3 取剩余 ≈50%；输入预算 = `Settings.context_window`（缺省 32768）× 75%）。降级顺序确定可回放：**B1 无条件保底** → A/B2/B5/C2 截断文本（带省略号与记账）→ B3 从激活弱的开始先降辨识点行再裁撤（**anchors 最后被裁**）→ B4 按召回序裁尾 → **C1 未决事项优先于摘要正文** → C3 整条丢且至少保留最近 6 条。组装结果新增 `BudgetReport` 逐层记账（used/limit/trimmed），会话页注入层顶部显示预算总账与被裁标记
- **记忆检查器数据命令**：`inspector_data` 一次给全状态树路径与转移历史、剧情线（活跃/了结/放弃/C1 投影/窗口内命中原因）、心理（内心摘要/情绪槽/意图/衰减轨迹）、宫殿（三视图 + 最近记忆可溯源）、设定集（实体清单含状态与 anchors）、摘要、提案、揭示集与黑板

- **M2.8 记忆检查器面板**：会话页抽屉从 4 个页签扩成 10 个——**状态路径**（根→叶路径条 + directive + recall/reveal + 校验告警 + 转移历史）、**剧情线**（C1 只读投影 + 活跃线卡片带梯度/窗口命中原因/framing + 已了结与放弃折叠）、**心理**（内心摘要 + 情绪槽 + 意图 + 衰减轨迹迷你柱）、**宫殿**（房间/时间线/关联图三视图 + 最近记忆可溯源「第 N 轮」）、**设定集**（实体清单按状态着色 + anchors + 揭示集）、**摘要·收件箱**（滚动摘要 + 「立即总结」+ 提案确认/否决）——外加原有的注入层/卡内状态/卡内记忆/事件流。新增 `inspector_data` 命令一次性提供全部投影视图，`decide_proposal` 落确认/否决事件，`summarize_now` 手动触发总结；浏览器 mock 路径同步覆盖三个新命令
- **剧情线手动开/收线**：新增 `open_thread` / `resolve_thread` 命令（设计 §8.3）。**收线一次做三件事**：结果作为高显著记忆入宫殿（挂 thread 链接）、线事件落流（现状卡与 C1 同步更新）、以 `thread.<id>:resolved` 为事件名求值一次状态树转移（设计 §8.5）。验收级单测钉住全链路：开线 → C1 列出 → 收线 → 结果入宫殿且挂在线 id 上 → C1 消失、B1 出现「了结未远」、状态树被驱动转移
- **示例资产补齐（真机验收夹具）**：示例卡「小雨」对齐设计 §3/§7.2 参考卡——补 `state_tree`（日常→夜谈→释然：时钟+好感度转移、`reveal` 工作牌秘密、`recall` 召回加权、`codex.known`+`threads.resolved` 复合判据）、`psyche` 情绪种子与 on_message 触发词直写情绪槽、黑板时钟播种（黑板 clock 默认为空且不自动步进，不播种则时钟判据永不可达）；顺带修掉 on_context `string.format` 丢失 `%s` 占位符。设定集 default 世界新增 `place.图书馆`（在场激活 + live ▸当前）与 `item.便签`（关系牵引 always_with），`char.小雨` 实体补 `variants`（夜班时段语言差分）/`versions`（第 3 天起史变）并将 anchors 置为已确认。新增 `smoke_datahub.rs` 冒烟测试 4 例：仓库示例卡与设定集用生产加载器钉住——卡可加载未降级、状态树判据在真实卡源上可求值、时间层按故事时钟解析

### Changed：`preview_prompt` 不再落盘、不再记事件（M1 让预览也落盘是为了避免「预览一次状态变了、正式发送又变一次」的漂移；事件化之后正式发送自己会跑一次并留下事件，预览再落盘反而是多算一次）
- **钩子运行抽成与 Tauri 无关的内核**：`assemble_prompt_core` / `run_load_hook_core` / `run_message_hook_core` / `commit_reply` 生产与单测共用同一份代码（M1 曾因单测另写等价逻辑而漏掉生产的半步），单测的 `simulate_turn` 现在直接调用这些内核
- 手改黑板（`update_blackboard`）进事件流（reason=manual）——它不是派生结果，重放历史时不会被抹掉

### Fixed

- **M2.6 自动总结管线两个真机压测暴露的缺陷（2026-09-20 真机验收发现）**：① `spawn_summary` 的并发标记**从不释放**（`SummaryFlags::end` 定义了但无人调用）——首次总结尝试（无论成败）后该会话的管线永久停摆，此后每轮轮末触发都被静默跳过；现在 `SummaryFlags` 改为 `Arc` 共享，后台任务结束时无论成败都释放标记，失败下一轮自动重试。② 总结调用 `max_tokens=1600` 对**推理型模型不够**（思考 token 计入 max_tokens：deepseek-flash 在 1600 下稳定返回空正文），且一次失败后未总结的消息全部积压进下一次调用、批次越滚越大形成棘轮；现在按 **16 条/批分块**（`SUMMARY_CHUNK`，最老的先补，失败也有进度）+ `max_tokens=8192`。真机 200 轮复测：管线自动产出 L1 摘要 23 批、宫殿记忆 98 条、提案 57 条，第 203 轮的 B4 仍召回第 2 轮埋下的「《城南旧志》周五还书」约定（显著度 0.80）

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