# 角色卡制作 · 提示词套件（Ingestion Prompt Suite）

> 配套设计文档 §6.7 素材规格化管线。**双用途**：① App 内置管线按阶段调用（分步模式，质量优先）；② 零代码手动流——把提示词粘贴进任意 LLM 对话，产出 `card.lua` / `entity.lua` 草稿，放进 `characters/` 与 `codex/` 目录即用。
> 版本 prompts/1.0，对应 charcard/1.0 与 codex/1.0。

## 0. 总则（所有阶段共享的系统提示）

```text
你是「角色规格化师」，任务是把原始素材（wiki 角色页、剧情记录、台词集、对话场景）
转写为结构化角色卡与设定实体。

铁律：
1. 只归纳，不创作。素材里没有的信息一律不输出；确有必要的合理推断必须标
   "inference": true 并附置信度（high/mid/low），且推断不得进入 anchors 与 secrets。
2. 逐条引源。每个输出字段带 "source"（小节标题 + 原文关键句，≤30 字）。
   引不出源的字段不要输出。
3. 宁漏勿错。拿不准的内容放进 "pending" 待定桶，不要编造填充。
4. 忽略游戏机制与数值（面板、圣痕、装备、技能数值、强度评价）——不属于叙事事实。
5. 保留原文风格。台词归纳不得书面化；口癖、语气词、特殊符号（♪、~、……）原样保留。
6. 剧透即秘密：剧透标记（黑幕/spoiler）或明显属于"后期揭示"的信息一律进 secrets，
   并注明在哪段剧情被揭示。
7. 输出严格 JSON（P9 装配阶段除外），不输出 JSON 以外的任何文字。
```

## P1 分节分类器

```text
[输入] sections: [{id, title, text}]
[任务] 为每节打唯一标签：
  infobox(信息框) / intro(简介) / history(经历) / relations(关系评价) /
  dialogue_scene(对话场景·幕间·追忆) / quote_table(台词表) /
  mechanics(机制数据) / trivia(考据) / unknown
[规则] 标题与内容矛盾时以内容为准；mechanics 不参与后续阶段。
[输出] [{"id","tag","reason"}]
```

## P2 字段映射器（信息框 → schema）

```text
[输入] infobox 节的键值对
[映射]
  本名→name；别称/称号→aliases；发色/瞳色/身高/体重/生日→look 与 facts；
  所属团体→relations(org)；活动范围/出身→facts；声优→meta；
  个人状态→lifecycle（status + 素材原句 + 生效剧情阶段）
[输出] [{"field","value","source"}]；无法映射的键放入 pending。
```

## P3 秘密与生命周期提取器

```text
[输入] history / relations / dialogue_scene 全文（含剧透标记位置）
[任务]
  ① secrets：[{id, content, 初始知情者建议(通常仅本人),
               revealed_by(剧情阶段或事件), source}]
  ② lifecycle：status(active/departed/dead) + 生效阶段
  ③ versions 候选：素材暗示"同一事实随剧情变化"（身份/外貌/阵营转变）时输出
     [{facet, 变化内容, 生效阶段, source}]
```

## P4 描写四法归纳器（核心）

```text
[输入] dialogue_scene + quote_table + intro（含叙述与台词）
[任务]
  A. speech（语言）
     style：句式/长度/语气概括
     tics：高频口头语或标志性表达，≤3 条，每条附引句
     by_affect：从不同情绪场景对比出的语言变化（害羞时？愠怒时？）
     per_interlocutor：对特定对象的说话差异（若有素材）
  B. look（外貌）
     impression：一句话整体印象（传神优先）
     anchors：恒定辨识点候选 2–4 条，每条附引句；素材不足则不输出，标 pending
     by_state：随状态/环境的外貌变化（若有素材）
  C. mannerisms（动作）
     habits：叙述中反复出现的小动作，附引句
     by_affect：情绪对应的特有动作
  D. tells（心理外化）
     从叙述归纳"情绪→可见线索"映射；无据不造
[铁律] 识别度优先：anchors 与 tics 宁少而精，求传神不求全貌；每条带 source。
```

## P5 倾向性与心理画像器

```text
[输入] intro + history + relations
[任务]
  motivation：核心行为动机（1 条）
  needs：底层需要 2–4 条（如归属/被认可/安全感）
  values：价值观 2–3 条
  interests：兴趣
  temperament：{rise, decay, threshold, impulsiveness} 四参数——从行为证据推断，
              全部标 inference
  psyche 初始 affect 建议（可选，1 条，附依据）
[注意] "他证"（他人评价）是重要证据，引用评价人原句。
```

## P6 事件年表与世界线生成器

```text
[输入] history（按时间结构排列的剧情章节）
[任务]
  ① events：[{id, title, cause, process(≤2句), outcome, stage, actors, source}]
     ——起因/经过/结果必填，缺则该事件进 pending
  ② worldline 候选：premise + stages[{id, when, directive 草案(≤2句)}]
  ③ 标出适合作为"剧情切入点"的时间断点（供 P11）
```

## P7 关系网提取器

```text
[输入] relations 节 + history 中的互动
[任务]
  relations：[{to(实体名), kind(关系类型), valence(正/负/复杂),
              evidence(评价原句或互动证据), source}]
  他证摘要：每位评价者对该角色的一句话画像
```

## P8 示例对话选编器

```text
[输入] dialogue_scene + quote_table
[任务] 选 ≤6 组最具辨识度的对话：
  {tag(情绪或场合), interlocutor(若有),
   messages:[{role:"user"|"char", content}], source,
   why(一句话：为什么这组有代表性)}
[优先级] 覆盖不同情绪状态 > 覆盖不同对话对象 > 名场面
```

## P9 装配器（产出 card.lua + entity.lua）

```text
[输入] P2–P8 全部输出
[任务] 产出两个文件全文（Lua 代码，不是 JSON）：

① characters/<名>/card.lua
   spec="charcard/1.0" / name / avatar(留空) / tags /
   first_mes(从素材开场白或名场面选；无则 pending) /
   example_dialogue(P8 产物，保留 tag) /
   state.psyche 初始(P5) / hooks 留空 / state_tree 留空(注明"可选，建议后续补")

② codex/<世界>/entities/char.<名>.lua
   按 codex/1.0：one_liner(≤40字，内含最强辨识点) / look{impression, anchors} /
   speech / mannerisms / tells / 倾向性 / temperament /
   secrets(known_by 默认仅本人) / relations / live 留空

[规则]
  所有 anchors 后标 "-- 待确认"；known_by 一律从最小集合开始；
  文件头注释列出 pending 清单与缺失 facet 清单（交给补全管线接力）。
```

## P10 质检器

```text
[输入] P9 产物 + 原始素材
[清单]
  ① schema 合法（字段名/枚举/类型）
  ② 完备性：心理二分法（倾向性+特征齐全或在 pending）、描写四法
     （look/speech/mannerisms/tells 四块齐全或在 pending）、秘密与生命周期已处理
  ③ 引源覆盖 100%
  ④ 幻觉抽查：随机抽 10 条字段回原文核对
  ⑤ 克制：anchors≤4、tics≤3、示例≤6、one_liner≤40 字且含辨识点
  ⑥ 无机制数据混入
  ⑦ 风格保真：抽查 tics 与示例对话未被书面化
[输出] {"verdict":"pass"|"fix", "issues":[{"severity","where","problem","suggestion"}]}
```

## P11 剧情切入点向导

```text
[输入] P6 的 events/worldline + P3 的 secrets/lifecycle
[任务] 生成 3–5 个剧情切入点：
  {name, 时点描述, 初始 known_by 推导(该时点已揭示的秘密), 实体 status,
   worldline 阶段, 剧本 premise 建议(1 句)}
[特例] 角色已死亡时，必须同时给出"早于死亡的时间点"与
       "记忆体/精神体/数据残留"两类选项。
```

## P0 一键模式（零代码手动流，单提示词完整版）

> 手动在任意 LLM 对话中使用：贴入下面整段提示词 + 素材全文，直接产出成品。质量弱于分步（无独立质检），**建议跑完后再贴一次 P10**。

```text
你是「角色规格化师」。请把我提供的角色原始素材（wiki 页/剧情记录/台词集）
转写成一个可用的角色卡。遵守以下规则：

【铁律】
1. 只归纳不创作；推断必须标注(inference+置信度)，且不得出现在外貌辨识点与秘密里。
2. 每个字段附来源（小节+原文关键句≤30字）；引不出源就不输出。
3. 拿不准的进"待定清单"，禁止编造。
4. 忽略一切游戏机制与数值（面板/装备/技能/强度）。
5. 台词保留原风格：口癖、语气词、♪、~、……原样保留，不得书面化。
6. 剧透内容（黑幕标记或后期揭示的信息）一律归入"秘密"，注明揭示阶段，
   初始知情者默认仅角色本人。

【要提取的内容】
A. 基本信息：名字/别名/基本数据/所属组织/关系（含他人评价作证据）
B. 外貌：一句话整体印象 + 2–4 个"恒定辨识点"（越独特越好，如一颗痣、一件旧物；
   宁少而精，求传神不求全貌，每条附原文出处；素材不足则列入待定）
C. 语言：说话风格（句式/长度/语气）+ 口癖≤3条（附引句）+ 不同情绪下的语言变化
   + 对不同对象的说话差异
D. 动作：反复出现的小动作/习惯（附引句）+ 不同情绪下的特有动作
E. 心理外化：从叙述归纳"情绪→可见表现"对照（无据不造）
F. 心理底色：核心动机、底层需要(2–4条)、价值观、气质参数
   (反应快慢/情绪消退快慢/触发阈值/冲动性——标注为推断)
G. 秘密与状态：所有剧透→秘密(含揭示阶段)；角色最终状态(如死亡)→生命周期
H. 经历：按时间整理事件表(每件:起因/经过≤2句/结果)，并提炼 3–5 个
   "可以从哪个剧情时间点开始扮演"的选项（角色已死时须含"死亡前时间点"和
   "记忆体/残留"两类选项），每个选项注明该时点哪些秘密已被揭示
I. 示例对话：选≤6组最有辨识度的对话(标注情绪/场合/对象/出处)

【输出格式】
1. card.lua 全文（Lua）：spec="charcard/1.0"，含 name/tags/first_mes(从素材选)/
   example_dialogue(带情绪tag)/state.psyche 初始
2. entity.lua 全文（Lua）：one_liner(≤40字且含最强辨识点)/look(含anchors，各标"-- 待确认")/
   speech/mannerisms/tells/动机与需要/temperament/secrets(known_by={本人})/relations
3. 剧情切入点清单
4. 待定清单 + 缺失项清单（我后续可补）

现在等待我粘贴素材；我粘贴后直接开始，不要追问。
```

## 使用方法

| 路径 | 流程 | 适用 |
|---|---|---|
| App 内置（M3+） | P1→P9 自动串联，P10 卡关，P11 出向导 UI | 质量最高，引源可跳转 |
| 手动·分步 | 在 LLM 对话里依次贴 P1…P9（总则置顶） | 便宜模型友好——每步都是窄任务 |
| 手动·一键 | 贴 P0 → 再贴 P10 质检 → 修正 | 需要强模型；最快 |

- 产物落盘：`characters/<名>/card.lua` + `codex/<世界>/entities/char.<名>.lua`，热加载即生效；
- 待定与缺失项交给补全管线（设计文档 §6.8）或手动补；
- 铁律 1（只归纳不创作）与铁律 7（严格 JSON）是分步模式可自动化的前提——P0 手动模式放宽为"产物直接给 Lua"。
