//! 角色卡（charcard/1.0，设计 §3）：Lua 沙箱、静态字段解析与 hooks 运行。
//!
//! 沙箱 v0（设计 §3.1/§3.2）：
//! - 库白名单（table/string/math/bit；luajit 下 coroutine 属 base 库），
//!   不加载 os/io/package/debug/ffi/jit；
//! - LuaJIT 关闭 JIT——JIT 编译的 trace 不经过解释器钩子，死循环会杀不死；
//! - 指令计数上限（超限终止本次执行）与内存上限；
//! - 宿主侧白名单 API：`ctx.inject` / `ctx.window` / `api.memory` / `api.blackboard`
//!   / `api.ui.emit` / `api.random` / `api.dice`；
//! - 一切执行都在错误边界内：卡片崩溃只记日志并降级为静态卡，不崩主程序。
//!
//! 每次执行新建 Lua 实例（卡源码很小，重编译成本可忽略；实例不跨线程持有）。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use mlua::{HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value, VmState};
use serde::{Deserialize, Serialize};

use crate::store::Message;

/// 沙箱载入的标准库（无 os/io/package/debug/ffi/jit；luajit 的 coroutine 随 base 载入）
fn sandbox_libs() -> StdLib {
    StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::BIT
}
/// 指令计数上限（设计 §3.2：超时杀掉）
const INSTRUCTION_LIMIT: u32 = 10_000_000;
/// 每多少条 VM 指令触发一次计数钩子
const HOOK_STEP: u32 = 2_000;
/// 内存上限
const MEMORY_LIMIT: usize = 32 * 1024 * 1024;

/// 生命周期钩子名（设计 §3.1）
const HOOK_NAMES: [&str; 3] = ["on_load", "on_context", "on_message"];

/// Card 的静态字段名（serde 反序列化只喂这些：mlua 对忽略字段仍会遍历
/// 到 function 值而报错，hooks/state_tree 不能进入）
const STATIC_FIELDS: [&str; 10] = [
    "spec",
    "name",
    "avatar",
    "creator",
    "tags",
    "world",
    "scenario",
    "personality",
    "first_mes",
    "example_dialogue",
];

// ---------- 静态卡片结构（serde 兼容层）----------

/// 示例对话中的一行（role: user | char）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleLine {
    pub role: String,
    pub content: String,
}

/// 按情绪/场景分组的示例对话（状态化 few-shot，设计 §3）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleTurn {
    #[serde(default)]
    pub tag: Option<String>,
    pub messages: Vec<ExampleLine>,
}

/// card.lua 顶层表的静态子集；行为层（state/hooks/state_tree）
/// 留在 Lua 侧执行，不进入本结构（serde 忽略未知字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub spec: String,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub world: Option<String>,
    pub scenario: String,
    pub personality: String,
    pub first_mes: String,
    #[serde(default)]
    pub example_dialogue: Vec<ExampleTurn>,
}

impl Card {
    /// Lua 层失败时的降级占位卡（设计 §3.2）
    pub fn degraded(dir_name: &str, reason: &str) -> Card {
        Card {
            spec: "charcard/1.0".into(),
            name: dir_name.into(),
            avatar: None,
            creator: None,
            tags: vec!["降级".into()],
            world: None,
            scenario: format!("（角色卡加载失败，已降级为占位静态卡。原因：{reason}）"),
            personality: String::new(),
            first_mes: "……（她似乎完全无法回应。）".into(),
            example_dialogue: Vec::new(),
        }
    }
}

// ---------- 沙箱 ----------

fn new_sandbox() -> Result<Lua, mlua::Error> {
    let lua = Lua::new_with(sandbox_libs(), LuaOptions::default())?;
    // LuaJIT：关 JIT（`jit` 库未载入时此句无害）
    let _ = lua.load("if jit then jit.off() end").exec();
    // 危险/越权的 base 全局清空（os/io/package 等本就未载入，此处兜底）
    let g = lua.globals();
    for name in [
        "load", "loadstring", "dofile", "loadfile", "require", "print", "newproxy",
        "collectgarbage", "jit", "os", "io", "debug", "package", "ffi",
    ] {
        let _ = g.set(name, mlua::Nil);
    }
    lua.set_memory_limit(MEMORY_LIMIT)?;
    let count = Rc::new(Cell::new(0u32));
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_STEP),
        move |_, _| {
            if count.get() >= INSTRUCTION_LIMIT / HOOK_STEP {
                return Err(mlua::Error::runtime("指令计数超限，卡片执行被终止"));
            }
            count.set(count.get() + 1);
            Ok(VmState::Continue)
        },
    )?;
    Ok(lua)
}

/// 在沙箱中执行 card.lua，取回顶层 table
fn eval_card(lua: &Lua, source: &str) -> Result<Table, mlua::Error> {
    lua.load(source).set_name("card.lua").eval::<Table>()
}

// ---------- 解析与加载 ----------

/// card.lua 解析结果：静态字段 + 行为层描述
#[derive(Debug, Clone)]
pub struct CardSource {
    pub card: Card,
    /// 卡上 `state` 表（会话初始状态；无则空对象）
    pub default_state: serde_json::Value,
    /// 存在的 hook 函数名
    pub hook_names: Vec<String>,
}

/// 解析 card.lua 源码（沙箱内执行；任何失败返回错误原因）
pub fn parse_card(source: &str) -> Result<CardSource, String> {
    let lua = new_sandbox().map_err(|e| format!("沙箱初始化失败：{e}"))?;
    let table = eval_card(&lua, source).map_err(|e| format!("card.lua 执行失败：{e}"))?;
    // 只拷贝静态字段进新表再反序列化（避免忽略字段里的 function 触发遍历错误）
    let static_table = lua.create_table().map_err(|e| format!("建表失败：{e}"))?;
    for key in STATIC_FIELDS {
        if let Ok(v) = table.get::<Value>(key) {
            if !matches!(v, Value::Nil) {
                static_table.set(key, v).map_err(|e| format!("拷贝字段失败：{e}"))?;
            }
        }
    }
    let card: Card = lua
        .from_value(Value::Table(static_table))
        .map_err(|e| format!("静态字段解析失败：{e}"))?;

    let mut hook_names = Vec::new();
    if let Ok(hooks) = table.get::<Table>("hooks") {
        for name in HOOK_NAMES {
            if matches!(hooks.get::<Value>(name), Ok(Value::Function(_))) {
                hook_names.push(name.to_string());
            }
        }
    }
    let default_state = match table.get::<Value>("state") {
        Ok(Value::Table(t)) => lua
            .from_value(Value::Table(t))
            .unwrap_or(serde_json::json!({})),
        _ => serde_json::json!({}),
    };
    Ok(CardSource {
        card,
        default_state,
        hook_names,
    })
}

/// 已加载的卡：静态字段 + 原始源码（hooks 每次运行时重新装载）
#[derive(Debug, Clone)]
pub struct LoadedCard {
    pub dir_name: String,
    pub source: String,
    pub card: Card,
    pub default_state: serde_json::Value,
    pub hook_names: Vec<String>,
    /// true：Lua 层不可用，已降级为静态卡（hooks 不再执行）
    pub degraded: bool,
    pub degrade_reason: Option<String>,
}

/// 解析失败降级为静态占位卡（设计 §3.2 错误边界）
pub fn load_card_source(dir_name: &str, source: &str) -> LoadedCard {
    match parse_card(source) {
        Ok(cs) => LoadedCard {
            dir_name: dir_name.into(),
            source: source.into(),
            card: cs.card,
            default_state: cs.default_state,
            hook_names: cs.hook_names,
            degraded: false,
            degrade_reason: None,
        },
        Err(reason) => LoadedCard {
            dir_name: dir_name.into(),
            source: source.into(),
            card: Card::degraded(dir_name, &reason),
            default_state: serde_json::json!({}),
            hook_names: Vec::new(),
            degraded: true,
            degrade_reason: Some(reason),
        },
    }
}

/// 从 DataHub 读取并加载一张卡
pub fn load_card(root: &Path, dir_name: &str) -> Result<LoadedCard, String> {
    let path = root.join("characters").join(dir_name).join("card.lua");
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    Ok(load_card_source(dir_name, &source))
}

/// 卡片目录清单（characters/*/card.lua；单卡失败按降级卡收录，不拖垮整体）
pub fn list_cards(root: &Path) -> Vec<CardSummary> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("characters")) else {
        return out;
    };
    for entry in rd.flatten() {
        let card_path = entry.path().join("card.lua");
        if !card_path.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        if let Ok(source) = std::fs::read_to_string(&card_path) {
            let lc = load_card_source(&dir_name, &source);
            out.push(CardSummary {
                has_hooks: !lc.hook_names.is_empty(),
                degraded: lc.degraded,
                dir_name: lc.dir_name,
                name: lc.card.name,
                tags: lc.card.tags,
                creator: lc.card.creator,
            });
        }
    }
    out.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    out
}

/// 前端用的卡片摘要
#[derive(Debug, Clone, Serialize)]
pub struct CardSummary {
    pub dir_name: String,
    pub name: String,
    pub tags: Vec<String>,
    pub creator: Option<String>,
    pub has_hooks: bool,
    pub degraded: bool,
}

/// 前端用的卡片详情（不含源码）
#[derive(Debug, Clone, Serialize)]
pub struct CardDetail {
    pub dir_name: String,
    pub card: Card,
    pub default_state: serde_json::Value,
    pub hook_names: Vec<String>,
    pub degraded: bool,
    pub degrade_reason: Option<String>,
}

impl From<LoadedCard> for CardDetail {
    fn from(lc: LoadedCard) -> Self {
        CardDetail {
            dir_name: lc.dir_name,
            card: lc.card,
            default_state: lc.default_state,
            hook_names: lc.hook_names,
            degraded: lc.degraded,
            degrade_reason: lc.degrade_reason,
        }
    }
}

// ---------- hooks 运行 ----------

/// ctx.inject 收集到的一条注入（role: system/user/char）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InjectedText {
    pub role: String,
    pub text: String,
}

/// api.ui.emit 收集到的一条界面事件
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiEvent {
    pub kind: String,
    pub value: String,
}

/// api.memory / api.blackboard 的一次写入（同 key 后写覆盖前写）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KvSet {
    pub key: String,
    pub value: serde_json::Value,
}

/// 一次 hook 调用的全部结果（错误边界：失败只记 logs，不外抛）
#[derive(Debug, Clone, Default, Serialize)]
pub struct HookResult {
    /// hook 运行后的角色 state（None = hook 未运行/未修改）
    pub state: Option<serde_json::Value>,
    pub injections: Vec<InjectedText>,
    pub ui_events: Vec<UiEvent>,
    pub memory: Vec<KvSet>,
    pub blackboard: Vec<KvSet>,
    pub logs: Vec<String>,
}

/// hook 调用（签名见设计 §3：`on_context(ctx, state)` / `on_message(msg, state, api)`）
pub enum HookCall<'a> {
    OnLoad,
    OnContext { window: &'a [Message] },
    OnMessage { msg: &'a Message, window: &'a [Message] },
}

impl HookCall<'_> {
    fn name(&self) -> &'static str {
        match self {
            HookCall::OnLoad => "on_load",
            HookCall::OnContext { .. } => "on_context",
            HookCall::OnMessage { .. } => "on_message",
        }
    }
}

/// 会话种子 xorshift（api.random/api.dice 可回放，设计 §3.1）
struct SeedableRng(Cell<u64>);

impl SeedableRng {
    fn new(seed: u64) -> Self {
        SeedableRng(Cell::new(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        }))
    }
    fn next(&self) -> u64 {
        let mut x = self.0.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0.set(x);
        x
    }
}

/// 在沙箱中运行卡的一个 hook。任何失败都只进 `logs`（错误边界）。
pub fn run_hook(
    source: &str,
    call: HookCall<'_>,
    state: serde_json::Value,
    seed: u64,
) -> HookResult {
    let mut result = HookResult::default();
    let lua = match new_sandbox() {
        Ok(l) => l,
        Err(e) => {
            result.logs.push(format!("沙箱初始化失败：{e}"));
            return result;
        }
    };
    let table = match eval_card(&lua, source) {
        Ok(t) => t,
        Err(e) => {
            result.logs.push(format!("card.lua 执行失败：{e}"));
            return result;
        }
    };
    let hook = match table
        .get::<Table>("hooks")
        .and_then(|h| h.get::<Value>(call.name()))
    {
        Ok(Value::Function(f)) => f,
        _ => return result, // 无此 hook：正常静默
    };

    let injections = Rc::new(RefCell::new(Vec::new()));
    let ui_events = Rc::new(RefCell::new(Vec::new()));
    let memory_log = Rc::new(RefCell::new(Vec::new()));
    let blackboard_log = Rc::new(RefCell::new(Vec::new()));

    let state_value = match lua.to_value(&state) {
        Ok(v) => v,
        Err(e) => {
            result.logs.push(format!("state 转换失败：{e}"));
            return result;
        }
    };

    let call_result = match &call {
        HookCall::OnLoad => {
            let api = make_api(&lua, seed, &ui_events, &memory_log, &blackboard_log);
            hook.call::<()>((state_value.clone(), api))
        }
        HookCall::OnContext { window } => {
            let ctx = make_ctx(&lua, window, &injections);
            hook.call::<()>((ctx, state_value.clone()))
        }
        HookCall::OnMessage { msg, window: _ } => {
            let msg_table = make_msg_table(&lua, msg);
            let api = make_api(&lua, seed, &ui_events, &memory_log, &blackboard_log);
            hook.call::<()>((msg_table, state_value.clone(), api))
        }
    };
    if let Err(e) = call_result {
        result
            .logs
            .push(format!("hook「{}」执行失败：{e}", call.name()));
    }

    // hooks 原地修改 state 表；失败后也读回部分修改（卡作者可在日志里看到错误）
    result.state = Some(lua.from_value(state_value).unwrap_or(state));
    result.injections = injections.borrow().clone();
    result.ui_events = ui_events.borrow().clone();
    result.memory = memory_log.borrow().clone();
    result.blackboard = blackboard_log.borrow().clone();
    result
}

fn make_msg_table(lua: &Lua, msg: &Message) -> Table {
    let t = lua.create_table().expect("create msg table");
    let _ = t.set("turn", msg.turn);
    let _ = t.set("role", msg.role.clone());
    let _ = t.set("content", msg.content.clone());
    if let Some(scene) = &msg.scene_id {
        let _ = t.set("scene_id", scene.clone());
    }
    t
}

/// ctx：inject(role, text) + window（只读消息窗口，设计 §3.1）
fn make_ctx(lua: &Lua, window: &[Message], injections: &Rc<RefCell<Vec<InjectedText>>>) -> Table {
    let ctx = lua.create_table().expect("create ctx");
    let inj = Rc::clone(injections);
    let inject = lua
        .create_function(move |_, (role, text): (String, String)| {
            inj.borrow_mut().push(InjectedText { role, text });
            Ok(())
        })
        .expect("ctx.inject");
    let _ = ctx.set("inject", inject);

    let w = lua.create_table().expect("ctx.window");
    for (i, m) in window.iter().enumerate() {
        if let Ok(t) = lua.create_table() {
            let _ = t.set("role", m.role.clone());
            let _ = t.set("content", m.content.clone());
            let _ = w.set(i + 1, t);
        }
    }
    let _ = ctx.set("window", w);
    ctx
}

/// api：memory / blackboard / ui.emit / random / dice（设计 §3.1 白名单）
fn make_api(
    lua: &Lua,
    seed: u64,
    ui_events: &Rc<RefCell<Vec<UiEvent>>>,
    memory_log: &Rc<RefCell<Vec<KvSet>>>,
    blackboard_log: &Rc<RefCell<Vec<KvSet>>>,
) -> Table {
    let api = lua.create_table().expect("create api");

    // memory / blackboard：本-run 内可读写的键值（持久化在 M1.6 接入）
    for (log, key) in [
        (Rc::clone(memory_log), "memory"),
        (Rc::clone(blackboard_log), "blackboard"),
    ] {
        let ns = lua.create_table().expect("create ns");
        let store: Rc<RefCell<HashMap<String, serde_json::Value>>> = Rc::default();
        let st_get = Rc::clone(&store);
        let get_fn = lua
            .create_function(move |lua, k: String| {
                Ok(match st_get.borrow().get(&k) {
                    Some(v) => lua.to_value(v)?,
                    None => Value::Nil,
                })
            })
            .expect("api.get");
        let _ = ns.set("get", get_fn);
        let st_set = Rc::clone(&store);
        let log_set = Rc::clone(&log);
        let set_fn = lua
            .create_function(move |lua, (k, v): (String, Value)| {
                let json = lua.from_value::<serde_json::Value>(v)?;
                st_set.borrow_mut().insert(k.clone(), json.clone());
                let mut log = log_set.borrow_mut();
                log.retain(|s| s.key != k); // 同 key 后写覆盖
                log.push(KvSet { key: k, value: json });
                Ok(())
            })
            .expect("api.set");
        let _ = ns.set("set", set_fn);
        let _ = api.set(key, ns);
    }

    // ui.emit(kind, value)
    let ui = lua.create_table().expect("create ui");
    let ev = Rc::clone(ui_events);
    let emit_fn = lua
        .create_function(move |_, (kind, value): (String, String)| {
            ev.borrow_mut().push(UiEvent { kind, value });
            Ok(())
        })
        .expect("ui.emit");
    let _ = ui.set("emit", emit_fn);
    let _ = api.set("ui", ui);

    // random / dice：会话种子（可回放）
    let rng = SeedableRng::new(seed);
    let random_fn = lua
        .create_function(move |_, n: Option<i64>| {
            let x = rng.next();
            match n {
                None => Ok(Value::Number((x >> 11) as f64 / (1u64 << 53) as f64)),
                Some(n) if n > 0 => Ok(Value::Integer(1 + (x % n as u64) as i64)),
                Some(_) => Err(mlua::Error::runtime("api.random(n) 需 n >= 1")),
            }
        })
        .expect("api.random");
    let _ = api.set("random", random_fn);

    let rng2 = SeedableRng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let dice_fn = lua
        .create_function(move |_, n: i64| {
            if n < 1 {
                return Err(mlua::Error::runtime("api.dice(n) 需 n >= 1"));
            }
            Ok(1 + (rng2.next() % n as u64) as i64)
        })
        .expect("api.dice");
    let _ = api.set("dice", dice_fn);
    api
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 DataHub/characters/小雨/card.lua 同构的测试卡（含行为层）
    const TEST_CARD: &str = r#"
return {
  spec = "charcard/1.0",
  name = "测试雨",
  tags = { "温柔", "日常" },
  scenario = "s",
  personality = "p",
  first_mes = "f",
  example_dialogue = {
    { tag = "平静", messages = { { role = "user", content = "u" } } },
  },
  state = { favorability = 50 },
  hooks = {
    on_context = function(ctx, state)
      ctx.inject("system", string.format("【角色内部状态】好感度 %d/100", state.favorability))
    end,
    on_message = function(msg, state, api)
      if msg.role == "user" and msg.content:find("谢谢") then
        state.favorability = math.min(100, state.favorability + 1)
        api.memory.set("last_thanked", msg.turn)
      end
      api.ui.emit("emotion", state.favorability >= 80 and "shy" or "calm")
    end,
  },
}
"#;

    fn msg(content: &str) -> Message {
        Message {
            turn: 1,
            role: "user".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        }
    }

    #[test]
    fn parse_static_fields_and_behavior_layer() {
        let cs = parse_card(TEST_CARD).expect("解析测试卡");
        assert_eq!(cs.card.name, "测试雨");
        assert_eq!(cs.card.tags, vec!["温柔", "日常"]);
        assert_eq!(cs.card.example_dialogue.len(), 1);
        assert_eq!(cs.card.example_dialogue[0].tag.as_deref(), Some("平静"));
        assert_eq!(
            cs.hook_names,
            vec!["on_context".to_string(), "on_message".to_string()]
        );
        assert_eq!(cs.default_state["favorability"], 50);
    }

    #[test]
    fn on_message_favorability_and_side_effects() {
        let state = serde_json::json!({ "favorability": 50 });
        let m = msg("谢谢你。");
        let r = run_hook(
            TEST_CARD,
            HookCall::OnMessage { msg: &m, window: &[] },
            state,
            42,
        );
        assert!(r.logs.is_empty(), "logs: {:?}", r.logs);
        assert_eq!(r.state.unwrap()["favorability"], 51);
        assert_eq!(
            r.memory,
            vec![KvSet {
                key: "last_thanked".into(),
                value: serde_json::json!(1)
            }]
        );
        assert_eq!(
            r.ui_events,
            vec![UiEvent {
                kind: "emotion".into(),
                value: "calm".into()
            }]
        );
    }

    #[test]
    fn favorability_clamped_at_100_and_emotion_threshold() {
        let state = serde_json::json!({ "favorability": 100 });
        let m = msg("谢谢");
        let r = run_hook(
            TEST_CARD,
            HookCall::OnMessage { msg: &m, window: &[] },
            state,
            42,
        );
        assert_eq!(r.state.unwrap()["favorability"], 100);
        assert_eq!(r.ui_events[0].value, "shy");

        // 未命中关键词：不加好感
        let r2 = run_hook(
            TEST_CARD,
            HookCall::OnMessage {
                msg: &msg("今天好冷。"),
                window: &[],
            },
            serde_json::json!({ "favorability": 50 }),
            42,
        );
        assert_eq!(r2.state.unwrap()["favorability"], 50);
        assert!(r2.memory.is_empty());
    }

    #[test]
    fn on_context_injects_internal_state() {
        let r = run_hook(
            TEST_CARD,
            HookCall::OnContext {
                window: &[msg("早")],
            },
            serde_json::json!({ "favorability": 82 }),
            7,
        );
        assert_eq!(r.injections.len(), 1);
        assert_eq!(r.injections[0].role, "system");
        assert!(r.injections[0].text.contains("好感度 82"));
    }

    #[test]
    fn malicious_toplevel_loop_is_killed() {
        let evil =
            "while true do end\nreturn { name='x', scenario='s', personality='p', first_mes='f' }";
        // 指令上限杀掉执行 → 解析失败 → 降级为静态卡，不 panic
        let lc = load_card_source("恶意卡", evil);
        assert!(lc.degraded);
        assert!(lc.degrade_reason.unwrap().contains("指令计数超限"));
    }

    #[test]
    fn malicious_hook_loop_is_killed_not_panicking() {
        let evil = r#"
return {
  name = "x", scenario = "s", personality = "p", first_mes = "f",
  hooks = { on_message = function(msg, state, api)
    while true do end
  end },
}
"#;
        let r = run_hook(
            evil,
            HookCall::OnMessage {
                msg: &msg("hi"),
                window: &[],
            },
            serde_json::json!({}),
            1,
        );
        assert!(!r.logs.is_empty(), "死循环 hook 应产生错误日志");
        assert!(r.logs[0].contains("指令计数超限"));
    }

    #[test]
    fn os_and_io_are_stripped() {
        let card =
            "return { name='x', scenario='s', personality='p', first_mes=tostring(os.time()) }";
        let lc = load_card_source("偷窥卡", card);
        assert!(lc.degraded);
        assert!(lc.card.first_mes.contains("无法回应")); // 降级占位文案

        let card2 =
            "local f = io.open('x', 'w')\nreturn { name='x', scenario='s', personality='p', first_mes='f' }";
        assert!(parse_card(card2).is_err());
    }

    #[test]
    fn syntax_error_degrades_to_static_card() {
        let lc = load_card_source("坏卡", "return { name = ");
        assert!(lc.degraded);
        assert_eq!(lc.card.name, "坏卡");
        assert!(lc.hook_names.is_empty());
    }

    #[test]
    fn list_cards_degraded_entry_included() {
        let root = tempfile::tempdir().unwrap();
        for (dir, src) in [("好卡", TEST_CARD), ("坏卡", "not lua at all {{")] {
            let dir_path = root.path().join("characters").join(dir);
            std::fs::create_dir_all(&dir_path).unwrap();
            std::fs::write(dir_path.join("card.lua"), src).unwrap();
        }
        let mut list = list_cards(root.path());
        list.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(list.len(), 2);
        assert!(list
            .iter()
            .any(|c| c.name == "测试雨" && c.has_hooks && !c.degraded));
        assert!(list.iter().any(|c| c.name == "坏卡" && c.degraded));
    }

    #[test]
    fn seeded_random_is_replayable() {
        let card = r#"
return {
  name = "r", scenario = "s", personality = "p", first_mes = "f",
  hooks = { on_message = function(msg, state, api)
    api.ui.emit("roll", tostring(api.random(1000000)))
  end },
}
"#;
        let m = msg("roll");
        let a = run_hook(
            card,
            HookCall::OnMessage {
                msg: &m,
                window: &[],
            },
            serde_json::json!({}),
            99,
        );
        let b = run_hook(
            card,
            HookCall::OnMessage {
                msg: &m,
                window: &[],
            },
            serde_json::json!({}),
            99,
        );
        assert_eq!(a.ui_events, b.ui_events, "同一种子应可回放");
        assert!(!a.ui_events.is_empty());
    }

    #[test]
    fn repo_example_card_parses() {
        let datahub = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../DataHub");
        if !datahub.is_dir() {
            return; // 无示例数据的环境跳过
        }
        let lc = load_card(&datahub, "小雨").expect("加载示例卡");
        assert!(!lc.degraded, "降级原因：{:?}", lc.degrade_reason);
        assert_eq!(lc.card.name, "小雨");
        assert_eq!(lc.card.example_dialogue.len(), 2);
    }
}
