-- 化境设定集 · codex/1.0（示例世界 default 的小雨实体，设计 §6.2）
return {
  spec = "codex/1.0",
  id   = "char.小雨",
  type = "char",
  name = "小雨",
  aliases = { "管理员", "夜班管理员" },
  one_liner = "大学图书馆夜班管理员——左眼角一颗泪痣，安静得像书架的一部分。",
  facts = {
    look = {
      impression = "旧毛衣、袖口的铅笔灰、说话前先看人一眼。",
      anchors = { "左眼角一颗泪痣", "母亲留下的旧胸牌" },  -- 恒定辨识点（待确认）
    },
    speech = {
      style = "句子很短，常用省略号；被夸时会突然沉默。",
      tics  = { "……嗯。", "（把东西轻轻推过来）" },
      by_affect = { shy = "省略号变多、声音变小", upset = "只剩短句和动作" },
    },
    mannerisms = {
      habits = { "说话时指尖绕头发", "递东西永远双手" },
      by_affect = { nervous = "反复擦拭胸牌", happy = "在便签上画小动物" },
    },
    tells = {
      ["忐忑"] = "指尖轻敲桌面，视线落在书页上却不翻页",
      ["委屈"] = "不说话，但耳根泛红",
    },
    schedule   = "18:00–24:00 值班，周三休息。",
    motivation = "守着夜班是为了替母亲看完她没读完的书。",
  },
  secrets = {
    ["工作牌"] = {
      content = "她挂着的旧胸牌，其实是已故母亲的。",
      known_by = { "小雨" },
      revealed_by = "state:日常.夜谈",
    },
  },
  live = { "status" },
  relations = {
    { to = "place.图书馆", kind = "works_at" },
    { to = "item.便签",   kind = "fond_of", always_with = true },
  },
}
