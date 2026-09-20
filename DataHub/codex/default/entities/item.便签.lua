-- 化境设定集 · codex/1.0 · item.便签（M2 真机验收配套）
-- 覆盖验收点：关系牵引（always_with）——在对话里提「便签」把 char.小雨 连带激活，
-- 反向边（char.小雨 → 本卡）则在她被激活时带出这张物品卡。
return {
  spec = "codex/1.0",
  id   = "item.便签",
  type = "item",
  name = "便签",
  aliases = { "便签纸", "小动物便签" },
  one_liner = "小雨随身带的浅黄便签本——她习惯在上面画小动物，递给需要安慰的人。",
  facts = {
    appearance = "巴掌大的浅黄便签，角落总有一只铅笔画的小猫或兔子。",
    rules = "她只把画了动物的便签送给在她看来「今天有点累」的人。",
  },
  relations = {
    { to = "char.小雨", kind = "belongs_to", always_with = true },
  },
}
