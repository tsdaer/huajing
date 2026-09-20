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
//! M1.6 起 hooks 接入运行时（`HookEnv`）：调用方传入本轮可见的角色 state 与
//! 黑板快照，hook 的原地修改与 `api.*` 写入经 `HookRun` 的 state / blackboard /
//! memory 三组增量回传给宿主落盘；`api.ui.emit` 除进报告外，还经回调实时推给
//! 界面（表情等）。调用时机见 commands.rs：`on_load` 建会话、`on_context` 每轮
//! 组装、`on_message` 用户消息落盘后。
//!
//! M2.3 起同一条沙箱还给状态树用（设计 §7）：[`eval_state_tree`] 收集「叶 + 祖先继承」的
//! 转移、按 priority 升序求 when（函数式 → `pcall` 先 3 参再 5 参；字符串简写 → `event:名字`），
//! 首个命中即返回；[`state_tree_shape`] 把整棵树折成纯数据 JSON（函数值折成布尔位）供宿主注入
//! 与面板使用；[`run_state_hook_full`] 另跑转移的 `on_enter` / `on_exit` 副作用（§7.3-3），
//! `card_has_state_hook` 让宿主先问有没有。转移目标不做越界校验——宿主拿
//! `statetree::StateTree::active_path` 校验。
//!
//! 每次执行新建 Lua 实例（卡源码很小，重编译成本可忽略；实例不跨线程持有）。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;

use mlua::{Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, MultiValue, StdLib, Table, Value, VmState};
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

// ---------- hooks 运行时状态（M1.6：设计 §3「hooks 是反应」）----------

/// 被 `api.blackboard.set` 允许写入的黑板键（黑板 v0 字段；设计 §4.1 B1）
pub const BLACKBOARD_KEYS: [&str; 4] = ["day", "clock", "place", "actors"];

/// 一次 hook 运行期间卡片可见的宿主状态（读侧快照）。
///
/// - `state`：角色私有 state（设计 §3：持久化在会话而非卡里）。Lua 侧原地改这张表，
///   运行结束由 [`HookRun::state`] 回传宿主落盘；
/// - `blackboard`：黑板只读快照（`api.blackboard.get` 走这里），
///   写入走 `api.blackboard.set` 并进 [`HookRun::blackboard`]；
/// - `memory`：`api.memory.get` 的历史值。M1 恒空——长期记忆由记忆宫殿（M2）提供读侧，
///   本版只保证写入不丢（落 `palace.jsonl`）。
#[derive(Debug, Clone, Default)]
pub struct HookEnv {
    pub state: serde_json::Value,
    pub blackboard: std::collections::BTreeMap<String, serde_json::Value>,
    pub memory: std::collections::BTreeMap<String, serde_json::Value>,
}

/// `api.ui.emit` 的实时回调（宿主转推前端）。报告里另留一份，供调用方汇总。
/// 必须 `Send`：闭包经 mlua 转为 `'static`，持锁的宿主状态不能进。
pub type UiSink = std::sync::Arc<dyn Fn(&UiEvent) + Send + Sync>;

/// 一次 hook 运行的完整结果：错误边界内的报告 + 需要宿主落盘的三组增量。
#[derive(Debug, Clone, Default)]
pub struct HookRun {
    pub result: HookResult,
    /// hook 运行后的完整 state（`None` = 卡片没有这个 hook，未运行）
    pub state: Option<serde_json::Value>,
    /// `api.blackboard.set` 的写入（同 key 后写覆盖前写；已按 [`BLACKBOARD_KEYS`] 过滤）
    pub blackboard: Vec<KvSet>,
    /// `api.memory.set` 的写入（同 key 后写覆盖前写）
    pub memory: Vec<KvSet>,
}

impl HookRun {
    /// 本次是否真的跑过 hook（卡片未定义该 hook 时为 false，不应产生任何状态变化）
    pub fn ran(&self) -> bool {
        self.state.is_some()
    }
}

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
/// 解析一个 Lua **数据文件**（设定集实体：`return { ... }`）为 JSON（M2.2 · 设计 §6.2 的双格式之一）。
///
/// 与 card.lua 共用同一套沙箱（剥离 os/io、指令计数上限、内存上限），因此社区实体文件同样
/// 不能作恶；文件里出现函数值会转换失败——实体本来就该是纯数据。
pub fn eval_lua_value(source: &str) -> Result<serde_json::Value, String> {
    let lua = new_sandbox().map_err(|e| format!("沙箱初始化失败：{e}"))?;
    let table = eval_card(&lua, source).map_err(|e| format!("Lua 执行失败：{e}"))?;
    lua.from_value(Value::Table(table))
        .map_err(|e| format!("实体转 JSON 失败：{e}"))
}

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

/// api.memory / api.blackboard 的一次写入（同 key 后写覆盖前写）。
/// 同时要能反序列化：钩子副作用现在作为事件流落盘（M2.0 · event.rs），重启后要读回来。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// 每条新消息落地后（只有 msg：卡按需自取，窗口只在 on_context 给）
    OnMessage { msg: &'a Message },
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

/// 在沙箱中运行卡的一个 hook，并回传需要落盘的增量（M1.6 运行时调用入口）。
///
/// - `env`：卡片本轮可见的状态快照（state / 黑板 / 长期记忆读侧）；
/// - `on_ui`：`api.ui.emit` 的实时回调（宿主转推前端；报告里也留一份）；
/// - 任何失败都只进 `HookRun::result.logs`（错误边界），不外抛、不 panic。
///
/// 卡片没有定义该 hook 时 `ran()` 为 false，`state` 为 `None`——调用方据此跳过落盘。
pub fn run_hook_full(
    source: &str,
    call: HookCall<'_>,
    env: &HookEnv,
    seed: u64,
    on_ui: &UiSink,
) -> HookRun {
    let mut run = HookRun::default();
    let lua = match new_sandbox() {
        Ok(l) => l,
        Err(e) => {
            run.result.logs.push(format!("沙箱初始化失败：{e}"));
            return run;
        }
    };
    let table = match eval_card(&lua, source) {
        Ok(t) => t,
        Err(e) => {
            run.result.logs.push(format!("card.lua 执行失败：{e}"));
            return run;
        }
    };
    let hook = match table
        .get::<Table>("hooks")
        .and_then(|h| h.get::<Value>(call.name()))
    {
        Ok(Value::Function(f)) => f,
        _ => return run, // 无此 hook：正常静默，不产生任何状态变化
    };

    let injections = Rc::new(RefCell::new(Vec::new()));
    let ui_events = Rc::new(RefCell::new(Vec::new()));
    let memory_log = Rc::new(RefCell::new(Vec::new()));
    let blackboard_log = Rc::new(RefCell::new(Vec::new()));

    let state_value = match lua.to_value(&env.state) {
        Ok(v) => v,
        Err(e) => {
            run.result.logs.push(format!("state 转换失败：{e}"));
            return run;
        }
    };

    let call_result = match &call {
        HookCall::OnLoad => {
            let api = make_api(
                &lua,
                seed,
                env,
                &ui_events,
                &memory_log,
                &blackboard_log,
                on_ui,
            );
            hook.call::<()>((state_value.clone(), api))
        }
        HookCall::OnContext { window } => {
            let ctx = make_ctx(&lua, window, &injections);
            hook.call::<()>((ctx, state_value.clone()))
        }
        HookCall::OnMessage { msg } => {
            let msg_table = make_msg_table(&lua, msg);
            let api = make_api(
                &lua,
                seed,
                env,
                &ui_events,
                &memory_log,
                &blackboard_log,
                on_ui,
            );
            hook.call::<()>((msg_table, state_value.clone(), api))
        }
    };
    if let Err(e) = call_result {
        run.result
            .logs
            .push(format!("hook「{}」执行失败：{e}", call.name()));
    }

    // hooks 原地修改 state 表；失败后也读回部分修改（卡作者可在日志里看到错误）
    run.result.state = Some(lua.from_value(state_value).unwrap_or(env.state.clone()));
    run.result.injections = injections.borrow().clone();
    run.result.ui_events = ui_events.borrow().clone();
    run.result.memory = memory_log.borrow().clone();
    run.result.blackboard = blackboard_log.borrow().clone();
    run.state = run.result.state.clone();
    run.blackboard = run.result.blackboard.clone();
    run.memory = run.result.memory.clone();
    run
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
///
/// memory / blackboard 都是「读快照 + 记增量」：读侧来自 [`HookEnv`]
/// （memory 读侧 M1 恒空——长期记忆的读由记忆宫殿在 M2 提供），写侧进各自的
/// 增量日志，由调用方在运行结束后落盘。
#[allow(clippy::too_many_arguments)]
fn make_api(
    lua: &Lua,
    seed: u64,
    env: &HookEnv,
    ui_events: &Rc<RefCell<Vec<UiEvent>>>,
    memory_log: &Rc<RefCell<Vec<KvSet>>>,
    blackboard_log: &Rc<RefCell<Vec<KvSet>>>,
    on_ui: &UiSink,
) -> Table {
    let api = lua.create_table().expect("create api");

    let mem_ns = make_kv_ns(
        lua,
        Rc::new(env.memory.clone()),
        Rc::clone(memory_log),
        None,
        "api.memory",
    );
    let _ = api.set("memory", mem_ns);

    // 黑板：白名单 = 世界层四字段 + 实体作用域键（设计 §6.4 的 `char.小雨.status`），
    // 越权键报 Lua 错误。作用域键的规则写在白名单校验里（见 make_kv_ns 的 dotted 分支）。
    let allowed: Vec<String> = BLACKBOARD_KEYS.iter().map(|k| k.to_string()).collect();
    let bb_ns = make_kv_ns(
        lua,
        Rc::new(env.blackboard.clone()),
        Rc::clone(blackboard_log),
        Some(allowed),
        "api.blackboard",
    );
    let _ = api.set("blackboard", bb_ns);

    // ui.emit(kind, value)：进报告 + 实时推给界面
    let ui = lua.create_table().expect("create ui");
    let ev = Rc::clone(ui_events);
    let sink = std::sync::Arc::clone(on_ui);
    let emit_fn = lua
        .create_function(move |_, (kind, value): (String, String)| {
            let event = UiEvent { kind, value };
            sink(&event);
            ev.borrow_mut().push(event);
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

/// 构造 `api.<ns>.get/set` 一对函数：读侧取自 [`HookEnv`] 快照，写侧进增量日志
/// （同 key 后写覆盖前写）。`allowed` 为 Some 时拒绝白名单外的键。
fn make_kv_ns(
    lua: &Lua,
    read: Rc<std::collections::BTreeMap<String, serde_json::Value>>,
    log: Rc<RefCell<Vec<KvSet>>>,
    allowed: Option<Vec<String>>,
    ns_name: &'static str,
) -> Table {
    let ns = lua.create_table().expect("create ns");
    let get_store = Rc::clone(&read);
    let get_fn = lua
        .create_function(move |lua, k: String| {
            Ok(match get_store.get(&k) {
                Some(v) => lua.to_value(v)?,
                None => Value::Nil,
            })
        })
        .expect("api.get");
    let _ = ns.set("get", get_fn);

    let set_fn = lua
        .create_function(move |lua, (k, v): (String, Value)| {
            if let Some(allowed) = &allowed {
                // 白名单外的键一律拒绝，但**实体作用域键**放行：
                // 形如 `char.小雨.status`（设计 §6.2/§6.4 的 live 数据源），
                // 至少两段、每段非空，避免 `.` 这类噪声键混进黑板。
                let dotted_ok = k.contains('.')
                    && k.split('.').count() >= 2
                    && k.split('.').all(|seg| !seg.trim().is_empty());
                if !dotted_ok && !allowed.iter().any(|a| a == &k) {
                    return Err(mlua::Error::runtime(format!(
                        "{ns_name}.set 不支持的键「{k}」（可用：{} 或实体作用域键 char.小雨.status）",
                        allowed.join(" / ")
                    )));
                }
            }
            let json = lua.from_value::<serde_json::Value>(v)?;
            let mut log = log.borrow_mut();
            log.retain(|s| s.key != k); // 同 key 后写覆盖
            log.push(KvSet { key: k, value: json });
            Ok(())
        })
        .expect("api.set");
    let _ = ns.set("set", set_fn);
    ns
}

// ---------- 状态树求值（M2.3 · 设计 §7）----------

/// 状态树一轮求值的输入（宿主拼好；codex/threads 只以「可查询的判据」形式暴露给 when）。
///
/// - `event`：本轮事件名（on_message / on_turn_end / on_timer / thread:resolved / 卡片自定义）；
/// - `blackboard` / `state`：只读快照（转移求值不该改状态——on_enter/on_exit 才是副作用，
///   由宿主另跑）；
/// - `known` / `codex_active`：codex.known("char.小雨.secrets.工作牌") / codex.active("place.图书馆")
///   的判据集（M2.2 的揭示集与在场集，宿主投影好后传进来）；
/// - `threads_active` / `threads_resolved`：threads.active(id) / threads.resolved(id) 的判据集
///   （M2.4）。
#[derive(Debug, Clone, Default)]
pub struct TreeEnv {
    pub event: String,
    pub blackboard: BTreeMap<String, serde_json::Value>,
    pub state: serde_json::Value,
    pub known: BTreeSet<String>,
    pub codex_active: BTreeSet<String>,
    pub threads_active: BTreeSet<String>,
    pub threads_resolved: BTreeSet<String>,
}

/// 一次命中的转移：从哪个活跃叶到哪个目标状态，以及给事件流/面板看的中文原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeDecision {
    /// 转移前的活跃叶（宿主用 crate::statetree::StateTree::active_path 展开成 from 路径）
    pub from: String,
    /// 目标状态 id（**未做越界校验**：可能没在 states 里声明，宿主应先用 active_path 校验）
    pub to: String,
    /// 形如 `日常 → 日常.夜谈（priority 10）` / `日常 → 疏远（priority 20；when「event:提及过去伤疤」）`
    pub reason: String,
}

/// 状态树一轮求值的沙箱入口（设计 §7.3；语义逐条如下）。
///
/// 1. **候选**：当前活跃叶的转移 + 沿活跃路径从祖先继承的转移（叶的转移先于祖先的）；
///    按 `priority` **升序稳定排序**——同 `priority` 保持「叶先、祖先后、各自声明顺序」，
///    未声明 priority 视为 0（§7.3-1/2）；
/// 2. **首个命中即返回**，一轮只转移一次（§7.3-2：默认转移推迟到轮末评估，一轮内状态稳定）。
///    目标就是当前叶自身的候选视为无效（继续往后求值）：继承来的父转移常在叶上命中自己，
///    照转会把 on_exit/on_enter 白跑一遍；「转移」只认到别的状态；
/// 3. **when 是函数**：先按 `(ev, bb, st)` 调用（§7.2 主流签名），出错再按
///    `(ev, bb, st, codex, threads)` 调用（多给判据表）；两次都失败 = 这条不命中
///    （错误边界：只记诊断日志，不外抛、不 panic）。函数的返回值按 Lua 真值判定
///    （`nil`/`false` 为假，0 与空串为真）；
/// 4. **when 是字符串简写**：只有 `"event:事件名"` 形态有意义——与 `env.event` 精确相等即命中；
///    其它字符串一律不命中（不猜）；
/// 5. **其它形态的 when**（缺省 / 数字 / 表 …）一律不命中（不猜）；
/// 6. **沙箱边界**：卡内 state_tree 的函数在既有沙箱里跑（剥离 os/io、指令计数上限、
///    内存上限）——死循环的 when 会被杀掉并降级为「不命中」（M2.3 验收：恶意转移原地不动）；
/// 7. **卡上没有 state_tree** → `Ok(None)`；`active` 为空 / 叶没在树上 → `Ok(None)`。
///
/// 判据表在 Lua 侧是只读查询：`codex.known/active`、`threads.active/resolved`；
/// `ev` 是事件名字符串，`bb` / `st` 是快照副本（when 对形参的写入不回传宿主，
/// 每个候选各拿一份新副本，候选之间互不影响）。
///
/// 返回 `Err` 只代表沙箱初始化 / 卡源码执行失败（宿主记日志并按「本轮不转移」处理）。
pub fn eval_state_tree(
    source: &str,
    active: &[String],
    env: &TreeEnv,
) -> Result<Option<TreeDecision>, String> {
    let Some(leaf) = active.last().cloned() else {
        return Ok(None); // 宿主还没播种活跃叶
    };
    let lua = new_sandbox().map_err(|e| format!("沙箱初始化失败：{e}"))?;
    let table = eval_card(&lua, source).map_err(|e| format!("card.lua 执行失败：{e}"))?;
    let tree = match table.get::<Value>("state_tree") {
        Ok(Value::Table(t)) => t,
        _ => return Ok(None), // 纯反应型卡：没有状态树，正常静默
    };

    // 候选收集放 Lua 侧（transitions 可能是数组/缺省，判据也在 Lua 里最省事），
    // 但**不排序**——排序由 Rust 的稳定排序做，保证「priority 升序 + 叶先」确定。
    let collect: Function = lua
        .load(COLLECT_TREE_LUA)
        .set_name("state_tree.collect")
        .eval()
        .map_err(|e| format!("转移收集脚本装载失败：{e}"))?;
    let path = lua.create_table().map_err(|e| format!("建表失败：{e}"))?;
    for (i, id) in active.iter().rev().enumerate() {
        // 叶先、祖先后
        path.set(i + 1, id.clone())
            .map_err(|e| format!("写活跃路径失败：{e}"))?;
    }
    let candidates: Table = collect
        .call((tree, path))
        .map_err(|e| format!("转移收集失败：{e}"))?;
    let pcall: Function = lua
        .globals()
        .get("pcall")
        .map_err(|e| format!("pcall 不可用：{e}"))?;

    // 判据集：闭包共享的只读快照（每轮固定）
    let known = Rc::new(env.known.clone());
    let codex_active = Rc::new(env.codex_active.clone());
    let threads_active = Rc::new(env.threads_active.clone());
    let threads_resolved = Rc::new(env.threads_resolved.clone());

    let mut list: Vec<(String, i64, String, Value)> = Vec::new();
    for cand in candidates.sequence_values::<Table>() {
        let Ok(cand) = cand else { continue };
        let Ok(to) = cand.get::<String>("to") else {
            continue;
        };
        let priority = cand.get::<i64>("priority").unwrap_or(0);
        let src = cand.get::<String>("src").unwrap_or_default();
        let when = cand.get::<Value>("when").unwrap_or(Value::Nil);
        list.push((to, priority, src, when));
    }
    list.sort_by_key(|(_, p, _, _)| *p); // 稳定排序：同 priority 保持「叶先于祖先，各自声明顺序」

    for (to, priority, src, when) in &list {
        let hit = match when {
            Value::Function(f) => {
                let args = make_when_args(
                    &lua,
                    env,
                    &known,
                    &codex_active,
                    &threads_active,
                    &threads_resolved,
                )?;
                eval_when_fn(pcall.clone(), f, &args)
            }
            Value::String(s) => when_shorthand_hits(&s.to_string_lossy(), &env.event),
            _ => false, // 其它形态一律不命中（不猜）
        };
        if hit {
            // 到当前叶自身的候选算「无效」：继承来的父转移常常指向叶自己（如「日常 → 日常.夜谈」
            // 在叶已经是 日常.夜谈 时命中），照转会把 on_exit/on_enter 白跑一遍（原地重进）。
            // 所以跳过它继续求值——转移 = 到**别的**状态。
            if to == &leaf {
                continue;
            }
            return Ok(Some(TreeDecision {
                from: leaf.clone(),
                to: to.clone(),
                reason: transition_reason(&leaf, to, *priority, src, when),
            }));
        }
    }
    Ok(None)
}

/// 状态树的进入/退出钩子（设计 §7.2 / §7.3-3）：取 `state_tree.states[state_id][kind]`，
/// 在**同一套沙箱**里按 `(api, state)` 调用，副作用照旧经 [`HookRun`] 回传落盘。
///
/// - `kind`：`"on_enter"` / `"on_exit"`（设计 §7.2 的两个钩子；传别的键只会在卡上找不到函数）；
/// - **实参**：设计 §7.2 写的是 `on_enter(api)`——第一个实参永远是 api，原样可用；
///   第二个实参是角色 state 表（想改 state 就写 `function(api, state)`，Lua 允许多传实参，
///   单参签名不受影响），运行结束按原地修改读回进 [`HookRun::state`]；
/// - `api.memory` / `api.blackboard` / `api.ui.emit` / `api.random` / `api.dice` 与 hooks 完全一致
///   （`api.memory` **读侧为空**：[`TreeEnv`] 里没有 memory 快照，写入照常回传）；
///   状态钩子不收 `ctx`，所以 [`HookResult::injections`] 恒为空；
/// - 错误边界与 [`run_hook_full`] 相同：剥离 os/io、指令计数上限、内存上限，
///   抛错/死循环只进 `result.logs`，不外抛、不 panic；
/// - 卡上没有这个钩子（状态不存在 / 值不是函数）→ 默认 [`HookRun`]（`ran()` 为 false，
///   不产生任何副作用，宿主据此跳过落盘）。
///
/// 宿主接入（设计 §7.3-3）：转移执行 = 跑旧路径的 `on_exit` → 切换活跃路径 → 跑新路径的
/// `on_enter`（reveal 揭示、黑板写入、开线/收线都在这一步的 api 写入里），
/// 三组增量与转移事件一起落事件流。
pub fn run_state_hook_full(
    source: &str,
    state_id: &str,
    kind: &str,
    env: &TreeEnv,
    seed: u64,
    on_ui: &UiSink,
) -> HookRun {
    let mut run = HookRun::default();
    let lua = match new_sandbox() {
        Ok(l) => l,
        Err(e) => {
            run.result.logs.push(format!("沙箱初始化失败：{e}"));
            return run;
        }
    };
    let table = match eval_card(&lua, source) {
        Ok(t) => t,
        Err(e) => {
            run.result.logs.push(format!("card.lua 执行失败：{e}"));
            return run;
        }
    };
    let Some(hook) = find_state_hook(&table, state_id, kind) else {
        return run; // 没这个钩子：正常静默，不产生任何状态变化
    };

    // TreeEnv 有三组读侧里的两组（state / 黑板），memory 快照补一张空表给 api.memory
    let hook_env = HookEnv {
        state: env.state.clone(),
        blackboard: env.blackboard.clone(),
        memory: BTreeMap::new(),
    };
    let state_value = match lua.to_value(&hook_env.state) {
        Ok(v) => v,
        Err(e) => {
            run.result.logs.push(format!("state 转换失败：{e}"));
            return run;
        }
    };
    let ui_events = Rc::new(RefCell::new(Vec::new()));
    let memory_log = Rc::new(RefCell::new(Vec::new()));
    let blackboard_log = Rc::new(RefCell::new(Vec::new()));
    let api = make_api(
        &lua,
        seed,
        &hook_env,
        &ui_events,
        &memory_log,
        &blackboard_log,
        on_ui,
    );
    if let Err(e) = hook.call::<()>((api, state_value.clone())) {
        run.result
            .logs
            .push(format!("状态钩子「{state_id}.{kind}」执行失败：{e}"));
    }
    // 与 hooks 一致：钩子原地改 state 表，失败后也读回部分修改（卡作者可在日志里看到错误）
    run.result.state = Some(lua.from_value(state_value).unwrap_or(hook_env.state.clone()));
    run.result.ui_events = ui_events.borrow().clone();
    run.result.memory = memory_log.borrow().clone();
    run.result.blackboard = blackboard_log.borrow().clone();
    run.state = run.result.state.clone();
    run.blackboard = run.result.blackboard.clone();
    run.memory = run.result.memory.clone();
    run
}

/// 卡上有没有这个状态钩子（宿主按 key 找函数用：面板/干跑预览要列某状态有哪些钩子，
/// 转移执行前也可先问一句——问一次要跑一遍 card.lua（Lua 没法静态看函数），但不会调用钩子、
/// 不产生任何副作用）。
///
/// 只认函数：状态不存在、键不是函数、state_tree 缺省都返回 false（不 panic、不报错）。
pub fn card_has_state_hook(source: &str, state_id: &str, kind: &str) -> bool {
    let Ok(lua) = new_sandbox() else {
        return false;
    };
    let Ok(table) = eval_card(&lua, source) else {
        return false;
    };
    find_state_hook(&table, state_id, kind).is_some()
}

/// 从卡里取 `state_tree.states[state_id][kind]`（只认函数；取不到一律 None）
fn find_state_hook(table: &Table, state_id: &str, kind: &str) -> Option<Function> {
    let tree = table.get::<Table>("state_tree").ok()?;
    let states = tree.get::<Table>("states").ok()?;
    let state = states.get::<Table>(state_id).ok()?;
    match state.get::<Value>(kind) {
        Ok(Value::Function(f)) => Some(f),
        _ => None,
    }
}

/// 取某个状态的声明式数据（directive/recall/reveal）与整棵树的结构，供宿主注入与面板使用。
///
/// 返回 JSON：`{ root, states: { id: { parent, directive, recall: [...], reveal: [...],
/// has_enter, has_exit, transitions: [ { to, priority, when?, when_is_fn? } ] } }, warnings: [...] }`
///
/// - `root`：树根 id；没有 state_tree 的卡返回空串（`states` 为空对象）——宿主据此跳过注入；
/// - `parent` / `directive`：没写时是 `null`；`recall` / `reveal`：数组（缺失即空）；
/// - `has_enter` / `has_exit`：Lua 函数折成的布尔位（**绝不能**让 mlua 的 from_value 撞上
///   function 值——那会整体解析失败，所以先在 Lua 侧遍历成纯数据表，再交给 Rust）；
/// - `transitions`：声明式转移。「when` 只在字符串简写时出现；函数式 when 只留
///   `when_is_fn: true`（纯数据看不见函数体）；
/// - `warnings`：结构上的可疑之处（如 recall 不是数组、转移缺 to），中文一句话，已排序。
///
/// 产物可直接喂给 crate::statetree::StateTree::from_value（round-trip 有单测钉住）。
pub fn state_tree_shape(source: &str) -> Result<serde_json::Value, String> {
    let lua = new_sandbox().map_err(|e| format!("沙箱初始化失败：{e}"))?;
    let table = eval_card(&lua, source).map_err(|e| format!("card.lua 执行失败：{e}"))?;
    let tree = match table.get::<Value>("state_tree") {
        Ok(Value::Table(t)) => t,
        _ => return Ok(empty_shape()),
    };
    let extract: Function = lua
        .load(SHAPE_TREE_LUA)
        .set_name("state_tree.shape")
        .eval()
        .map_err(|e| format!("结构提取脚本装载失败：{e}"))?;
    let pure: Table = extract
        .call(tree)
        .map_err(|e| format!("state_tree 结构提取失败：{e}"))?;
    shape_from_pure(&pure)
}

/// 没有状态树的卡的 shape（空树；宿主以 `root` 为空判「无树」）
fn empty_shape() -> serde_json::Value {
    serde_json::json!({ "root": "", "states": {}, "warnings": [] })
}

/// 把 Lua 侧遍历出的纯数据表读成 shape JSON（只含 string/bool/数组，函数已折成布尔位）
fn shape_from_pure(pure: &Table) -> Result<serde_json::Value, String> {
    let root: String = pure.get("root").unwrap_or_default();
    let mut warnings: Vec<String> = pure.get("warnings").unwrap_or_default();
    // Lua 的 pairs 顺序不保证 → 排序，保证同一张卡每次拿到的 shape 一致
    warnings.sort();
    let raw_states: Table = pure
        .get("states")
        .map_err(|e| format!("states 读取失败：{e}"))?;
    let mut states = serde_json::Map::new();
    for item in raw_states.sequence_values::<Table>() {
        let item = item.map_err(|e| format!("状态项读取失败：{e}"))?;
        let id: String = item.get("id").map_err(|e| format!("状态 id 读取失败：{e}"))?;
        let text = |key: &str| item.get::<String>(key).unwrap_or_default();
        let flag = |key: &str| item.get::<bool>(key).unwrap_or(false);
        let list = |key: &str| item.get::<Vec<String>>(key).unwrap_or_default();
        let parent = text("parent");
        let directive = text("directive");
        let mut transitions: Vec<serde_json::Value> = Vec::new();
        if let Ok(raw) = item.get::<Table>("transitions") {
            for tr in raw.sequence_values::<Table>().flatten() {
                let to: String = tr.get("to").unwrap_or_default();
                if to.is_empty() {
                    continue;
                }
                let mut t = serde_json::Map::new();
                t.insert("to".into(), serde_json::json!(to));
                t.insert(
                    "priority".into(),
                    serde_json::json!(tr.get::<i64>("priority").unwrap_or(0)),
                );
                let when: String = tr.get("when").unwrap_or_default();
                if !when.is_empty() {
                    t.insert("when".into(), serde_json::json!(when));
                }
                t.insert(
                    "when_is_fn".into(),
                    serde_json::json!(tr.get::<bool>("when_is_fn").unwrap_or(false)),
                );
                transitions.push(serde_json::Value::Object(t));
            }
        }
        states.insert(
            id,
            serde_json::json!({
                "parent": if parent.trim().is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(parent)
                },
                "directive": if directive.trim().is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(directive)
                },
                "recall": list("recall"),
                "reveal": list("reveal"),
                "has_enter": flag("has_enter"),
                "has_exit": flag("has_exit"),
                "transitions": transitions,
            }),
        );
    }
    Ok(serde_json::json!({
        "root": root,
        "states": serde_json::Value::Object(states),
        "warnings": warnings,
    }))
}

/// when 函数的实参（每次求值现造：bb/st 是快照副本，判据表是只读查询）
struct WhenArgs {
    ev: Value,
    bb: Value,
    st: Value,
    codex: Value,
    threads: Value,
}

fn make_when_args(
    lua: &Lua,
    env: &TreeEnv,
    known: &Rc<BTreeSet<String>>,
    codex_active: &Rc<BTreeSet<String>>,
    threads_active: &Rc<BTreeSet<String>>,
    threads_resolved: &Rc<BTreeSet<String>>,
) -> Result<WhenArgs, String> {
    let ev = lua
        .create_string(&env.event)
        .map_err(|e| format!("事件名转换失败：{e}"))?;
    let bb = lua
        .to_value(&env.blackboard)
        .map_err(|e| format!("黑板转换失败：{e}"))?;
    let st = lua
        .to_value(&env.state)
        .map_err(|e| format!("state 转换失败：{e}"))?;
    let codex = make_lookup(
        lua,
        vec![
            ("known", Rc::clone(known)),
            ("active", Rc::clone(codex_active)),
        ],
    );
    let threads = make_lookup(
        lua,
        vec![
            ("active", Rc::clone(threads_active)),
            ("resolved", Rc::clone(threads_resolved)),
        ],
    );
    Ok(WhenArgs {
        ev: Value::String(ev),
        bb,
        st,
        codex: Value::Table(codex),
        threads: Value::Table(threads),
    })
}

/// 只读判据查询表：`codex.known(id)` / `threads.resolved(id)` 这类（设计 §7.2 的 5 参 when）
fn make_lookup(lua: &Lua, entries: Vec<(&'static str, Rc<BTreeSet<String>>)>) -> Table {
    let t = lua.create_table().expect("create lookup");
    for (name, set) in entries {
        let f = lua
            .create_function(move |_, id: String| Ok(set.contains(&id)))
            .expect("create lookup fn");
        let _ = t.set(name, f);
    }
    t
}

/// when 函数求值：先 3 参 `(ev, bb, st)`，出错再 5 参 `(ev, bb, st, codex, threads)`。
/// 两次都失败 = 这条不命中（错误边界：记一条诊断，不 panic、不外抛）。
fn eval_when_fn(pcall: Function, f: &Function, args: &WhenArgs) -> bool {
    match pcall_when(&pcall, f, &[&args.ev, &args.bb, &args.st]) {
        Ok(hit) => hit,
        Err(e3) => match pcall_when(
            &pcall,
            f,
            &[&args.ev, &args.bb, &args.st, &args.codex, &args.threads],
        ) {
            Ok(hit) => hit,
            Err(e5) => {
                crate::diag::record(
                    "statetree",
                    format!("when 求值失败（3 参：{e3}；5 参：{e5}），按不命中处理"),
                );
                false
            }
        },
    }
}

/// 用 Lua 的 pcall 包一层：函数体内抛错（含指令计数超限被杀）只是「这次没命中」。
/// 返回 Ok(Lua 真值) / Err(错误描述)。
fn pcall_when(pcall: &Function, f: &Function, args: &[&Value]) -> Result<bool, String> {
    let mv = match args.len() {
        5 => pcall.call::<MultiValue>((
            f.clone(),
            args[0].clone(),
            args[1].clone(),
            args[2].clone(),
            args[3].clone(),
            args[4].clone(),
        )),
        _ => pcall.call::<MultiValue>((
            f.clone(),
            args[0].clone(),
            args[1].clone(),
            args[2].clone(),
        )),
    }
    .map_err(|e| e.to_string())?;
    if !matches!(mv.get(0), Some(Value::Boolean(true))) {
        let detail = mv
            .get(1)
            .and_then(|v| v.to_string().ok())
            .unwrap_or_else(|| "when 执行失败（无错误详情）".to_string());
        return Err(detail);
    }
    Ok(lua_truthy(mv.get(1)))
}

/// Lua 真值：`nil` / `false` 为假，其余（含 0、空串）为真
fn lua_truthy(v: Option<&Value>) -> bool {
    !matches!(v, None | Some(Value::Nil) | Some(Value::Boolean(false)))
}

/// 字符串简写 when：只有 `"event:名字"` 形态有意义，与本次事件精确相等即命中
fn when_shorthand_hits(when: &str, event: &str) -> bool {
    when.strip_prefix("event:")
        .map(|name| name == event)
        .unwrap_or(false)
}

/// 命中原因（进事件流与面板）：`日常 → 日常.夜谈（priority 10）`；
/// 继承自祖先时补 `；继承自 X`，字符串简写时补 `；when「event:…」`
fn transition_reason(from: &str, to: &str, priority: i64, src: &str, when: &Value) -> String {
    let mut s = format!("{from} → {to}（priority {priority}");
    if src != from {
        s.push_str(&format!("；继承自 {src}"));
    }
    if let Value::String(w) = when {
        s.push_str(&format!("；when「{}」", w.to_string_lossy()));
    }
    s.push('）');
    s
}

/// Lua 侧结构提取：把 state_tree 遍历成**纯数据表**（只有 string/bool/数组），
/// 函数值折成 has_enter/has_exit/when_is_fn 布尔位。
/// 走这一趟的原因：state_tree 里有 function 值，直接 lua.from_value 会整体失败。
const SHAPE_TREE_LUA: &str = r#"
return function(tree)
  local out = { root = "", states = {}, warnings = {} }
  if type(tree) ~= "table" then return out end
  if type(tree.root) == "string" then out.root = tree.root end
  local states = tree.states
  if states == nil then return out end
  if type(states) ~= "table" then
    out.warnings[#out.warnings + 1] = "state_tree.states 不是表"
    return out
  end
  local function strings(id, key, v)
    local acc = {}
    if v == nil then return acc end
    if type(v) ~= "table" then
      out.warnings[#out.warnings + 1] = "状态「" .. id .. "」的 " .. key .. " 不是数组"
      return acc
    end
    for _, x in ipairs(v) do
      if type(x) == "string" and x ~= "" then
        acc[#acc + 1] = x
      else
        out.warnings[#out.warnings + 1] = "状态「" .. id .. "」的 " .. key .. " 里有非字符串项"
      end
    end
    return acc
  end
  for id, st in pairs(states) do
    if type(id) ~= "string" or id == "" then
      out.warnings[#out.warnings + 1] = "states 里有一个空 id / 非字符串 id 的状态"
    elseif type(st) ~= "table" then
      out.warnings[#out.warnings + 1] = "状态「" .. id .. "」不是表"
    else
      local transitions = {}
      if st.transitions ~= nil then
        if type(st.transitions) ~= "table" then
          out.warnings[#out.warnings + 1] = "状态「" .. id .. "」的 transitions 不是数组"
        else
          for _, tr in ipairs(st.transitions) do
            if type(tr) ~= "table" or type(tr.to) ~= "string" or tr.to == "" then
              out.warnings[#out.warnings + 1] = "状态「" .. id .. "」有一条转移缺少非空的 to"
            else
              transitions[#transitions + 1] = {
                to = tr.to,
                priority = tonumber(tr.priority) or 0,
                when = type(tr.when) == "string" and tr.when or nil,
                when_is_fn = type(tr.when) == "function",
              }
            end
          end
        end
      end
      if st.parent ~= nil and type(st.parent) ~= "string" then
        out.warnings[#out.warnings + 1] = "状态「" .. id .. "」的 parent 不是字符串"
      end
      if st.directive ~= nil and type(st.directive) ~= "string" then
        out.warnings[#out.warnings + 1] = "状态「" .. id .. "」的 directive 不是字符串"
      end
      out.states[#out.states + 1] = {
        id = id,
        parent = type(st.parent) == "string" and st.parent or "",
        directive = type(st.directive) == "string" and st.directive or "",
        recall = strings(id, "recall", st.recall),
        reveal = strings(id, "reveal", st.reveal),
        has_enter = type(st.on_enter) == "function",
        has_exit = type(st.on_exit) == "function",
        transitions = transitions,
      }
    end
  end
  return out
end
"#;

/// Lua 侧转移收集：按「叶先、祖先后」把候选摊平（**不排序**，排序交给 Rust 的稳定排序；
/// when 原样保留——字符串简写与函数都要能带回 Rust）。
const COLLECT_TREE_LUA: &str = r#"
return function(tree, path)
  local out = {}
  if type(tree) ~= "table" then return out end
  local states = tree.states
  if type(states) ~= "table" then return out end
  for _, id in ipairs(path) do
    local st = states[id]
    if type(st) == "table" and type(st.transitions) == "table" then
      for _, tr in ipairs(st.transitions) do
        if type(tr) == "table" and type(tr.to) == "string" and tr.to ~= "" then
          out[#out + 1] = {
            src = id,
            to = tr.to,
            priority = tonumber(tr.priority) or 0,
            when = tr.when,
          }
        end
      end
    end
  end
  return out
end
"#;

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
        name: None,
            turn: 1,
            role: "user".into(),
            content: content.into(),
            ts: 0,
            scene_id: None,
        }
    }

    /// 运行 hook 并丢弃界面事件回调（单测不关心实时推送）
    fn run(source: &str, call: HookCall<'_>, state: serde_json::Value, seed: u64) -> HookResult {
        run_hook_full(source, call, &env(state), seed, &sink()).result
    }

    fn env(state: serde_json::Value) -> HookEnv {
        HookEnv {
            state,
            ..HookEnv::default()
        }
    }

    fn sink() -> UiSink {
        std::sync::Arc::new(|_: &UiEvent| {})
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
        let r = run(TEST_CARD, HookCall::OnMessage { msg: &m }, state, 42);
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
        let r = run(TEST_CARD, HookCall::OnMessage { msg: &m }, state, 42);
        assert_eq!(r.state.unwrap()["favorability"], 100);
        assert_eq!(r.ui_events[0].value, "shy");

        // 未命中关键词：不加好感
        let r2 = run(
            TEST_CARD,
            HookCall::OnMessage {
                msg: &msg("今天好冷。"),
            },
            serde_json::json!({ "favorability": 50 }),
            42,
        );
        assert_eq!(r2.state.unwrap()["favorability"], 50);
        assert!(r2.memory.is_empty());
    }

    #[test]
    fn on_context_injects_internal_state() {
        let r = run(
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
        let r = run(
            evil,
            HookCall::OnMessage { msg: &msg("hi") },
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
    fn card_without_hook_leaves_state_untouched() {
        let plain = r#"
return { name = "静卡", scenario = "s", personality = "p", first_mes = "f" }
"#;
        let m = msg("你好");
        let r = run_hook_full(
            plain,
            HookCall::OnMessage { msg: &m },
            &env(serde_json::json!({ "a": 1 })),
            1,
            &sink(),
        );
        assert!(!r.ran(), "未定义 on_message 时不应算作跑过");
        assert!(r.state.is_none());
        assert!(r.result.logs.is_empty());
    }

    #[test]
    fn ui_emit_reaches_sink_and_report() {
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let sink_seen = std::sync::Arc::clone(&seen);
        let sink: UiSink = std::sync::Arc::new(move |e: &UiEvent| {
            sink_seen.lock().unwrap().push(format!("{}={}", e.kind, e.value));
        });
        let m = msg("谢谢你");
        let run = run_hook_full(
            TEST_CARD,
            HookCall::OnMessage { msg: &m },
            &env(serde_json::json!({ "favorability": 50 })),
            42,
            &sink,
        );
        assert_eq!(seen.lock().unwrap().as_slice(), ["emotion=calm".to_string()]);
        assert_eq!(run.result.ui_events[0].kind, "emotion");
    }

    #[test]
    fn memory_and_blackboard_writes_logged_once_per_key() {
        let card = r#"
return {
  name = "写卡", scenario = "s", personality = "p", first_mes = "f",
  hooks = { on_message = function(msg, state, api)
    api.memory.set("k", 1)
    api.memory.set("k", 2)
    api.blackboard.set("place", "天台")
    api.blackboard.set("day", 3)
  end },
}
"#;
        let m = msg("x");
        let r = run(card, HookCall::OnMessage { msg: &m }, serde_json::json!({}), 1);
        assert!(r.logs.is_empty(), "logs: {:?}", r.logs);
        assert_eq!(r.memory.len(), 1, "同 key 后写覆盖前写");
        assert_eq!(r.memory[0].value, serde_json::json!(2));
        assert_eq!(r.blackboard.len(), 2);
        assert_eq!(r.blackboard[0].key, "place");
    }

    #[test]
    fn blackboard_rejects_unknown_key() {
        let card = r#"
return {
  name = "越权卡", scenario = "s", personality = "p", first_mes = "f",
  hooks = { on_message = function(msg, state, api)
    api.blackboard.set("weather", "雨")
  end },
}
"#;
        let m = msg("x");
        let r = run(card, HookCall::OnMessage { msg: &m }, serde_json::json!({}), 1);
        assert!(r.blackboard.is_empty());
        assert!(r.logs[0].contains("不支持的键"), "logs: {:?}", r.logs);
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
        let a = run(card, HookCall::OnMessage { msg: &m }, serde_json::json!({}), 99);
        let b = run(card, HookCall::OnMessage { msg: &m }, serde_json::json!({}), 99);
        assert_eq!(a.ui_events, b.ui_events, "同一种子应可回放");
        assert!(!a.ui_events.is_empty());
    }


    #[test]
    fn edited_card_file_takes_effect_without_restart() {
        // M1.7 验收：改 first_mes 保存后，下一次读取即用新值（每轮从磁盘重读，无需重启）
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("characters/热卡");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("card.lua");
        // 一行写完：续行缩进会被折进 first_mes（Rust 的 \ 续行只在行尾紧跟换行时生效）
        let card = |first: &str, fav: i64| {
            format!(
                "return {{ spec='charcard/1.0', name='热卡', scenario='s', personality='p', first_mes='{first}', state={{ favorability={fav} }}, hooks={{ on_message=function(msg, state) state.favorability = state.favorability + 1 end }} }}"
            )
        };
        std::fs::write(&path, card("旧开场", 50)).unwrap();
        let before = load_card(root.path(), "热卡").unwrap();
        assert_eq!(before.card.first_mes, "旧开场");

        std::fs::write(&path, card("新开场", 70)).unwrap();
        let after = load_card(root.path(), "热卡").unwrap();
        assert_eq!(after.card.first_mes, "新开场");
        assert_eq!(after.default_state["favorability"], 70);

        // 钩子也来自新源码（旧实例不会被复用）
        let m = msg("x");
        let r = run_hook_full(
            &after.source,
            HookCall::OnMessage { msg: &m },
            &env(serde_json::json!({ "favorability": 10 })),
            1,
            &sink(),
        );
        assert_eq!(r.state.unwrap()["favorability"], 11);
    }

    #[test]
    fn workspace_codex_entities_are_valid_lua() {
        // 设定集实体同样是用户手写的明文文件：至少要能在沙箱里跑通、且带 id/type。
        // （引用完整性——relations 指向的实体是否存在——留到 M2 的图谱校验做。）
        let codex = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../DataHub/codex");
        if !codex.is_dir() {
            return;
        }
        let mut checked = 0;
        let mut stack = vec![codex.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("lua") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("读实体文件");
                let lua = new_sandbox().expect("沙箱初始化");
                let table = lua
                    .load(&source)
                    .set_name(path.to_string_lossy())
                    .eval::<Table>()
                    .unwrap_or_else(|e| panic!("实体 {} 执行失败：{e}", path.display()));
                for key in ["id", "type"] {
                    let value: String = table
                        .get(key)
                        .unwrap_or_else(|_| panic!("实体 {} 缺 `{key}`", path.display()));
                    assert!(!value.trim().is_empty(), "实体 {} 的 `{key}` 为空", path.display());
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "codex 下应至少有一个实体文件");
    }

    #[test]
    fn every_workspace_card_parses() {
        // 工作区里可能有用户自己导入/手写的卡（含 ST 导入产物）——它们同样必须可用。
        // 坏卡在这里失败，比在真机上开聊时才发现要早得多。
        let datahub = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../DataHub");
        if !datahub.is_dir() {
            return;
        }
        let summaries = list_cards(&datahub);
        assert!(!summaries.is_empty(), "工作区至少应有一张示例卡");
        for s in &summaries {
            let lc =
                load_card(&datahub, &s.dir_name).unwrap_or_else(|e| panic!("{}：{e}", s.dir_name));
            assert!(
                !lc.degraded,
                "卡「{}」解析失败：{:?}",
                s.dir_name,
                lc.degrade_reason
            );
            assert!(!lc.card.name.trim().is_empty(), "卡「{}」没有名字", s.dir_name);
            assert!(
                !lc.card.first_mes.trim().is_empty(),
                "卡「{}」没有开场白（first_mes），建会话会空场",
                s.dir_name
            );
        }
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
        // M1.6：示例卡带行为层（好感度），入席即有初始 state
        assert_eq!(
            lc.hook_names,
            vec![
                "on_load".to_string(),
                "on_context".to_string(),
                "on_message".to_string()
            ]
        );
        assert_eq!(lc.default_state["favorability"], 50);
    }

    // ---------- M2.3 状态树（设计 §7）----------

    /// 与设计 §7.2 同构的示例卡（state_tree 段照抄设计，外层补最小静态字段；
    /// 设计里未声明的转移目标「释然」在这里补成已声明状态，好让结构校验干净）
    const TREE_CARD: &str = r#"
return {
  spec = "charcard/1.0",
  name = "状态树测试卡",
  scenario = "s",
  personality = "p",
  first_mes = "f",
  state = { favorability = 50 },
  state_tree = {
    root = "日常",
    states = {
      ["日常"] = {
        directive = "保持轻松日常的氛围，话题围绕图书馆与学业，不主动推进关系。",
        transitions = {
          { to = "日常.夜谈", priority = 10,
            when = function(ev, bb, st)          -- ev:事件  bb:黑板  st:角色state
              return bb.clock >= 23 and st.favorability >= 60
            end },
          { to = "疏远", priority = 20,
            when = "event:提及过去伤疤" },        -- 字符串简写：事件名匹配
        },
      },
      ["日常.夜谈"] = {
        parent = "日常",                          -- 层级：继承父状态未覆盖的转移
        directive = "夜深人静，两人独处。语速放慢，允许长时间沉默，可以袒露心事。",
        recall  = { "room:图书馆", "topic:过去" },
        reveal  = { "char.小雨.secrets.工作牌" },
        on_enter = function(api)
          api.ui.emit("bgm", "quiet_piano")
        end,
        transitions = {
          { to = "日常", when = function(ev, bb)
              return bb.clock < 22 or bb.place ~= "图书馆"
            end },
          { to = "释然", when = function(ev, bb, st, codex, threads)
              return codex.known("char.小雨.secrets.工作牌")
                 and threads.resolved("thread.工作牌坦白")
            end },
        },
      },
      ["疏远"] = {
        directive = "她突然变得客气而疏离，回答变短。不要主动解释原因。",
        on_enter = function(api) api.ui.emit("emotion", "distant") end,
      },
      ["释然"] = { parent = "日常.夜谈", directive = "心结解开。" },
    },
  },
}
"#;

    /// 分层卡：叶的转移（priority 10）+ 祖先继承的转移（priority 50），用来钉求值顺序
    const LAYERED_CARD: &str = r#"
return {
  name = "分层树", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = {
      ["A"] = {
        transitions = {
          { to = "祖先后", priority = 50, when = function(ev, bb) return bb.day >= 3 end },
        },
      },
      ["A.B"] = {
        parent = "A",
        transitions = {
          { to = "叶先", priority = 10, when = "event:on_turn_end" },
          { to = "叶二", priority = 10, when = function(ev, bb, st) return bb.day >= 99 end },
        },
      },
      ["祖先后"] = {}, ["叶先"] = {}, ["叶二"] = {},
    },
  },
}
"#;

    /// 一轮求值的输入（黑板带 day/clock/place，state 带 favorability）
    fn tree_env(event: &str, clock: i64, favorability: i64) -> TreeEnv {
        TreeEnv {
            event: event.to_string(),
            blackboard: [
                ("day".to_string(), serde_json::json!(3)),
                ("clock".to_string(), serde_json::json!(clock)),
                ("place".to_string(), serde_json::json!("图书馆")),
            ]
            .into_iter()
            .collect(),
            state: serde_json::json!({ "favorability": favorability }),
            ..TreeEnv::default()
        }
    }

    fn daily() -> Vec<String> {
        vec!["日常".to_string()]
    }

    fn night() -> Vec<String> {
        vec!["日常".to_string(), "日常.夜谈".to_string()]
    }

    #[test]
    fn state_tree_shape_reads_design_example() {
        let shape = state_tree_shape(TREE_CARD).expect("提取 shape");
        assert_eq!(shape["root"], "日常");
        assert_eq!(shape["states"].as_object().unwrap().len(), 4);

        let leaf = &shape["states"]["日常.夜谈"];
        assert_eq!(leaf["parent"], "日常");
        assert!(leaf["directive"].as_str().unwrap().contains("夜深人静"));
        assert_eq!(leaf["recall"], serde_json::json!(["room:图书馆", "topic:过去"]));
        assert_eq!(leaf["reveal"], serde_json::json!(["char.小雨.secrets.工作牌"]));
        assert_eq!(leaf["has_enter"], true);
        assert_eq!(leaf["has_exit"], false);

        // 函数值不能让结构提取整体失败：on_enter → has_enter，when 函数 → when_is_fn
        assert_eq!(shape["states"]["疏远"]["has_enter"], true);
        assert_eq!(shape["states"]["日常"]["parent"], serde_json::Value::Null);
        assert_eq!(shape["states"]["日常"]["directive"], serde_json::Value::String("保持轻松日常的氛围，话题围绕图书馆与学业，不主动推进关系。".into()));

        let trs = shape["states"]["日常"]["transitions"].as_array().unwrap();
        assert_eq!(trs.len(), 2);
        assert_eq!(trs[0]["to"], "日常.夜谈");
        assert_eq!(trs[0]["priority"], 10);
        assert_eq!(trs[0]["when_is_fn"], true);
        assert!(trs[0].get("when").is_none(), "函数式 when 不该写成字符串简写");
        assert_eq!(trs[1]["when"], "event:提及过去伤疤");
        assert_eq!(trs[1]["when_is_fn"], false);
        assert!(
            shape["warnings"].as_array().unwrap().is_empty(),
            "示例树不该有结构告警：{:?}",
            shape["warnings"]
        );

        // shape 是 statetree 的正规输入（宿主注入链路：shape → StateTree → directive/recall/reveal）
        let tree = crate::statetree::StateTree::from_value(&shape).expect("shape 应能被 StateTree 解析");
        assert!(tree.validate().is_empty(), "{:?}", tree.validate());
        let path = tree.active_path("日常.夜谈");
        assert_eq!(path, vec!["日常", "日常.夜谈"]);
        let directive = tree.directive_of(&path);
        assert!(directive.starts_with("保持轻松日常的氛围"), "{directive}");
        assert!(directive.ends_with("可以袒露心事。"), "{directive}");
        assert_eq!(tree.recall_of(&path), vec!["room:图书馆", "topic:过去"]);
        assert_eq!(tree.reveal_of(&path), vec!["char.小雨.secrets.工作牌"]);
        assert_eq!(
            tree.transition_targets("日常"),
            vec![
                ("日常.夜谈".to_string(), 10, "日常".to_string()),
                ("疏远".to_string(), 20, "日常".to_string()),
            ]
        );
    }

    #[test]
    fn state_tree_function_when_enters_night_talk() {
        // clock 23 且 favorability 60：命中 priority 10 的 日常 → 日常.夜谈
        let d = eval_state_tree(TREE_CARD, &daily(), &tree_env("on_turn_end", 23, 60))
            .expect("求值不该失败")
            .expect("应命中 日常.夜谈");
        assert_eq!(d.from, "日常");
        assert_eq!(d.to, "日常.夜谈");
        assert_eq!(d.reason, "日常 → 日常.夜谈（priority 10）");

        // 条件差一点就不转移（原地不动）
        assert!(eval_state_tree(TREE_CARD, &daily(), &tree_env("on_turn_end", 23, 59))
            .unwrap()
            .is_none());
        assert!(eval_state_tree(TREE_CARD, &daily(), &tree_env("on_turn_end", 22, 100))
            .unwrap()
            .is_none());
    }

    #[test]
    fn state_tree_event_shorthand_matches_only_that_event() {
        let hit = eval_state_tree(TREE_CARD, &daily(), &tree_env("提及过去伤疤", 20, 10))
            .unwrap()
            .expect("字符串简写应命中");
        assert_eq!(hit.to, "疏远");
        assert_eq!(hit.reason, "日常 → 疏远（priority 20；when「event:提及过去伤疤」）");

        // 精确相等才命中：别的名字、带空格、带前缀的都不算（不猜）
        for ev in [
            "on_message",
            "on_turn_end",
            "event:提及过去伤疤",
            "提及过去伤疤 ",
            "提及过去伤疤x",
            "",
        ] {
            assert!(
                eval_state_tree(TREE_CARD, &daily(), &tree_env(ev, 20, 10))
                    .unwrap()
                    .is_none(),
                "事件「{ev}」不该命中"
            );
        }
    }

    #[test]
    fn state_tree_priority_order_first_hit_wins() {
        // 两条都满足：priority 10 先求值 → 夜谈（不是 priority 20 的疏远）
        let d = eval_state_tree(TREE_CARD, &daily(), &tree_env("提及过去伤疤", 23, 60))
            .unwrap()
            .unwrap();
        assert_eq!(d.to, "日常.夜谈");
        // priority 10 不满足时落到 priority 20
        let d = eval_state_tree(TREE_CARD, &daily(), &tree_env("提及过去伤疤", 23, 59))
            .unwrap()
            .unwrap();
        assert_eq!(d.to, "疏远");
    }

    #[test]
    fn state_tree_one_transition_per_round_and_ancestor_inheritance() {
        let a_b = vec!["A".to_string(), "A.B".to_string()];
        // 叶的 priority 10 命中即返回：同 priority 的另一条与祖先 priority 50 的那条都不再算数
        let d = eval_state_tree(LAYERED_CARD, &a_b, &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .unwrap();
        assert_eq!(d.from, "A.B");
        assert_eq!(d.to, "叶先");
        assert_eq!(d.reason, "A.B → 叶先（priority 10；when「event:on_turn_end」）");

        // 叶的两条都不命中 → 落到祖先继承来的转移（原因里标出继承自谁）
        let d = eval_state_tree(LAYERED_CARD, &a_b, &tree_env("on_message", 23, 60))
            .unwrap()
            .unwrap();
        assert_eq!(d.to, "祖先后");
        assert_eq!(d.reason, "A.B → 祖先后（priority 50；继承自 A）");
    }

    #[test]
    fn state_tree_self_targeting_candidate_is_skipped() {
        // 叶已是 日常.夜谈，继承自「日常」的 priority 10 转移目标正是 日常.夜谈 自己：
        // 命中自己不算转移（否则宿主会把 on_exit/on_enter 白跑一遍），于是本轮不转移。
        let d = eval_state_tree(TREE_CARD, &night(), &tree_env("on_turn_end", 23, 60)).unwrap();
        assert!(d.is_none(), "到当前叶自身的候选应被跳过，而不是当成转移：{d:?}");

        // 同一条转移在叶是「日常」时照常生效（不是被整体禁用）
        let d = eval_state_tree(TREE_CARD, &daily(), &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .unwrap();
        assert_eq!(d.to, "日常.夜谈");
    }

    #[test]
    fn state_tree_five_arg_when_queries_codex_and_threads() {
        // 从「日常.夜谈」出发：clock 23 且 place 图书馆 → 第一条（回日常）不命中
        let mut env = tree_env("on_turn_end", 23, 60);
        assert!(
            eval_state_tree(TREE_CARD, &night(), &env).unwrap().is_none(),
            "判据为空时 5 参 when 不该命中"
        );

        env.known.insert("char.小雨.secrets.工作牌".into());
        env.threads_resolved.insert("thread.工作牌坦白".into());
        let d = eval_state_tree(TREE_CARD, &night(), &env)
            .unwrap()
            .expect("给了判据后 5 参 when 应命中");
        assert_eq!(d.to, "释然");
        assert_eq!(d.reason, "日常.夜谈 → 释然（priority 0）");
    }

    #[test]
    fn state_tree_lookup_tables_expose_all_four_predicates() {
        let card = r#"
return {
  name = "n", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = {
      ["A"] = { transitions = {
        { to = "B", when = function(ev, bb, st, codex, threads)
            return codex.known("k") and codex.active("place.图书馆")
               and threads.active("thread.还书") and threads.resolved("thread.旧账")
               and not codex.known("没有的")
          end },
      } },
      ["B"] = {},
    },
  },
}
"#;
        let mut env = TreeEnv {
            event: "on_turn_end".into(),
            ..TreeEnv::default()
        };
        env.known.insert("k".into());
        env.codex_active.insert("place.图书馆".into());
        env.threads_active.insert("thread.还书".into());
        env.threads_resolved.insert("thread.旧账".into());
        let active = vec!["A".to_string()];
        let d = eval_state_tree(card, &active, &env).unwrap().expect("四个判据都该可查");
        assert_eq!(d.to, "B");

        env.threads_resolved.clear();
        assert!(eval_state_tree(card, &active, &env).unwrap().is_none());
    }

    #[test]
    fn state_tree_absent_or_undeclared_leaf_yields_none() {
        let plain = r#"return { name = "静卡", scenario = "s", personality = "p", first_mes = "f" }"#;
        // 卡上没有 state_tree：正常静默（纯反应型卡）
        assert!(eval_state_tree(plain, &daily(), &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .is_none());
        let shape = state_tree_shape(plain).unwrap();
        assert_eq!(shape["root"], "");
        assert!(shape["states"].as_object().unwrap().is_empty());
        assert!(shape["warnings"].as_array().unwrap().is_empty());
        // 空树不是合法树（宿主以 root 为空判「无树」，别喂给 from_value）
        assert!(crate::statetree::StateTree::from_value(&shape).is_err());

        // 活跃路径为空 / 叶没在树上 → 本轮不转移
        assert!(eval_state_tree(TREE_CARD, &[], &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .is_none());
        assert!(
            eval_state_tree(TREE_CARD, &["没这个状态".to_string()], &tree_env("on_turn_end", 23, 60))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn state_tree_when_other_shapes_never_hit() {
        let card = r#"
return {
  name = "n", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = { ["A"] = { transitions = {
      { to = "缺 when" },
      { to = "数字 when", when = 42 },
      { to = "表 when", when = { event = "on_turn_end" } },
      { to = "近似事件", when = "event:on_turn_end " },
    } } },
  },
}
"#;
        assert!(eval_state_tree(card, &["A".to_string()], &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .is_none());
    }

    #[test]
    fn state_tree_erroring_when_is_a_miss_and_others_still_evaluate() {
        let card = r#"
return {
  name = "n", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = { ["A"] = { transitions = {
      { to = "抛错", priority = 10, when = function(ev, bb, st) error("炸了") end },
      { to = "后者", priority = 20, when = function(ev, bb, st) return true end },
    } }, ["抛错"] = {}, ["后者"] = {} },
  },
}
"#;
        // 抛错的 when 只算「这条不命中」，不 poison 后面的候选
        let d = eval_state_tree(card, &["A".to_string()], &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .expect("应落到后一条");
        assert_eq!(d.to, "后者");
    }

    #[test]
    fn state_tree_malicious_when_is_killed_and_stays_put() {
        // 唯一的转移是死循环：沙箱的指令计数把它杀掉（pcall 兜住错误）→ 降级为「不命中」
        // → 本轮原地不动，不 panic、不外抛。
        let only_evil = r#"
return {
  name = "恶意树", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = { ["A"] = { transitions = {
      { to = "死循环", priority = 10, when = function(ev, bb, st) while true do end end },
    } }, ["死循环"] = {} },
  },
}
"#;
        let r = eval_state_tree(only_evil, &["A".to_string()], &tree_env("on_turn_end", 23, 60));
        assert!(r.is_ok(), "恶意 when 不该让求值入口报错：{r:?}");
        assert!(r.unwrap().is_none(), "死循环的 when 应降级为不命中（原地不动）");

        // 被杀掉的只是这一个 when：本轮后面的候选照常求值（这里用不跑 Lua 的字符串简写钉死）
        let mixed = r#"
return {
  name = "半恶意树", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = { ["A"] = { transitions = {
      { to = "死循环", priority = 10, when = function(ev, bb, st) while true do end end },
      { to = "下一个", priority = 20, when = "event:on_turn_end" },
    } }, ["死循环"] = {}, ["下一个"] = {} },
  },
}
"#;
        let d = eval_state_tree(mixed, &["A".to_string()], &tree_env("on_turn_end", 23, 60))
            .unwrap()
            .expect("恶意 when 之后的候选仍应求值");
        assert_eq!(d.to, "下一个");

        // 结构提取同样不该被函数值拖垮（不执行函数，只认类型）
        let shape = state_tree_shape(only_evil).expect("shape 不该失败");
        assert_eq!(shape["states"]["A"]["transitions"][0]["when_is_fn"], true);
    }

    #[test]
    fn state_tree_shape_reports_structural_warnings() {
        let card = r#"
return {
  name = "n", scenario = "s", personality = "p", first_mes = "f",
  state_tree = {
    root = "A",
    states = {
      ["A"] = {
        recall = "room:图书馆",
        directive = 42,
        transitions = {
          { priority = 1 },
          { to = "B", priority = 2 },
        },
      },
      ["B"] = "不是表",
      [""] = {},
    },
  },
}
"#;
        let shape = state_tree_shape(card).unwrap();
        let w = shape["warnings"].as_array().unwrap();
        let joined = w.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("recall 不是数组"), "{joined}");
        assert!(joined.contains("directive 不是字符串"), "{joined}");
        assert!(joined.contains("有一条转移缺少非空的 to"), "{joined}");
        assert!(joined.contains("不是表"), "{joined}");
        // 坏行被跳过：只有 A 的合法转移与 B 的状态项进 shape
        assert_eq!(shape["states"]["A"]["transitions"].as_array().unwrap().len(), 1);
        assert!(shape["states"]["A"].get("recall").unwrap().as_array().unwrap().is_empty());
    }

    // ---------- M2.3 状态树的进入/退出钩子（§7.3-3）----------

    /// 带状态钩子的卡：on_enter 的两种签名各一例（设计 §7.2 的单参 + 想改 state 的两参）
    const HOOK_TREE_CARD: &str = r#"
return {
  spec = "charcard/1.0",
  name = "状态钩子卡",
  scenario = "s", personality = "p", first_mes = "f",
  state = { favorability = 50 },
  state_tree = {
    root = "日常",
    states = {
      ["日常"] = {
        on_enter = function(api)            -- 设计 §7.2 的形态：只收 api
          api.ui.emit("emotion", "calm")
          api.memory.set("进过日常", true)
        end,
      },
      ["日常.夜谈"] = {
        parent = "日常",
        on_enter = function(api, state)     -- 想改 state 就多收一个实参
          api.ui.emit("bgm", "quiet_piano")
          api.blackboard.set("place.图书馆.status", "闭馆中")
          api.memory.set("夜谈解锁", true)
          state.favorability = state.favorability + 5
          state.in_night = true
        end,
        on_exit = function(api)
          api.ui.emit("bgm", "day")
        end,
      },
      ["抛错"] = { on_enter = function(api) error("炸了") end },
      ["死循环"] = { on_enter = function(api) while true do end end },
      ["非函数"] = { on_enter = 42 },
      ["随机"] = { on_enter = function(api)
        api.ui.emit("roll", string.format("%d/%d", api.random(1000000), api.dice(6)))
      end },
    },
  },
}
"#;

    fn hook_tree_env() -> TreeEnv {
        TreeEnv {
            state: serde_json::json!({ "favorability": 50 }),
            ..TreeEnv::default()
        }
    }

    #[test]
    fn state_tree_on_enter_applies_state_blackboard_memory_and_ui() {
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let s = std::sync::Arc::clone(&seen);
        let sink: UiSink = std::sync::Arc::new(move |e: &UiEvent| {
            s.lock().unwrap().push(format!("{}={}", e.kind, e.value));
        });
        let r = run_state_hook_full(HOOK_TREE_CARD, "日常.夜谈", "on_enter", &hook_tree_env(), 7, &sink);
        assert!(r.ran());
        assert!(r.result.logs.is_empty(), "logs: {:?}", r.result.logs);
        // state 原地改动回传
        let st = r.state.clone().unwrap();
        assert_eq!(st["favorability"], 55);
        assert_eq!(st["in_night"], true);
        // 黑板 / 记忆增量回传
        assert_eq!(
            r.blackboard,
            vec![KvSet {
                key: "place.图书馆.status".into(),
                value: serde_json::json!("闭馆中")
            }]
        );
        assert_eq!(
            r.memory,
            vec![KvSet {
                key: "夜谈解锁".into(),
                value: serde_json::json!(true)
            }]
        );
        // ui 事件既进报告也实时推给界面
        assert_eq!(r.result.ui_events.len(), 1);
        assert_eq!(seen.lock().unwrap().as_slice(), ["bgm=quiet_piano".to_string()]);
        assert!(r.result.injections.is_empty(), "状态钩子不收 ctx，没有注入");
    }

    #[test]
    fn state_tree_on_enter_single_arg_api_form_works() {
        let r = run_state_hook_full(HOOK_TREE_CARD, "日常", "on_enter", &hook_tree_env(), 1, &sink());
        assert!(r.ran());
        assert!(r.result.logs.is_empty(), "logs: {:?}", r.result.logs);
        // 单参签名照常可用（api 表就是第一个实参）：能推 ui、能写记忆
        assert_eq!(
            r.result.ui_events,
            vec![UiEvent {
                kind: "emotion".into(),
                value: "calm".into()
            }]
        );
        assert_eq!(
            r.memory,
            vec![KvSet {
                key: "进过日常".into(),
                value: serde_json::json!(true)
            }]
        );
        assert_eq!(r.state.unwrap()["favorability"], 50, "没碰 state 就原样回传");
    }

    #[test]
    fn state_tree_on_exit_hook_runs() {
        let r = run_state_hook_full(HOOK_TREE_CARD, "日常.夜谈", "on_exit", &hook_tree_env(), 1, &sink());
        assert!(r.ran());
        assert_eq!(r.result.ui_events[0].value, "day");
        assert!(r.blackboard.is_empty() && r.memory.is_empty());
    }

    #[test]
    fn state_tree_missing_hook_is_silent() {
        // 状态存在但没有 on_exit
        let r = run_state_hook_full(HOOK_TREE_CARD, "日常", "on_exit", &hook_tree_env(), 1, &sink());
        assert!(!r.ran(), "没定义钩子时不该算跑过");
        assert!(r.state.is_none());
        assert!(r.blackboard.is_empty() && r.memory.is_empty() && r.result.ui_events.is_empty());
        assert!(r.result.logs.is_empty(), "静默：不是错误");

        // 状态不存在 / 键不是函数 / 键名不对：一律静默
        for (id, kind) in [
            ("没这个状态", "on_enter"),
            ("非函数", "on_enter"),
            ("日常.夜谈", "on_context"),
        ] {
            let r = run_state_hook_full(HOOK_TREE_CARD, id, kind, &hook_tree_env(), 1, &sink());
            assert!(!r.ran(), "「{id}」的 {kind} 不该算跑过");
            assert!(r.result.logs.is_empty());
        }

        // 没有 state_tree 的卡
        let plain = r#"return { name = "静卡", scenario = "s", personality = "p", first_mes = "f" }"#;
        assert!(!run_state_hook_full(plain, "日常", "on_enter", &hook_tree_env(), 1, &sink()).ran());
    }

    #[test]
    fn state_tree_hook_error_and_loop_stay_in_logs() {
        // 抛错：只记日志（部分 state 改动仍读回），不 panic、不外抛
        let r = run_state_hook_full(HOOK_TREE_CARD, "抛错", "on_enter", &hook_tree_env(), 1, &sink());
        assert!(r.ran());
        assert_eq!(r.result.logs.len(), 1, "logs: {:?}", r.result.logs);
        assert!(r.result.logs[0].contains("状态钩子「抛错.on_enter」执行失败"));
        assert!(r.result.logs[0].contains("炸了"));

        // 死循环：被沙箱的指令计数杀掉
        let r = run_state_hook_full(HOOK_TREE_CARD, "死循环", "on_enter", &hook_tree_env(), 1, &sink());
        assert!(!r.result.logs.is_empty(), "死循环应产生错误日志");
        assert!(r.result.logs[0].contains("指令计数超限"), "logs: {:?}", r.result.logs);
    }

    #[test]
    fn state_tree_hook_random_is_replayable() {
        let a = run_state_hook_full(HOOK_TREE_CARD, "随机", "on_enter", &hook_tree_env(), 99, &sink());
        let b = run_state_hook_full(HOOK_TREE_CARD, "随机", "on_enter", &hook_tree_env(), 99, &sink());
        assert!(a.ran() && !a.result.ui_events.is_empty());
        assert_eq!(
            a.result.ui_events, b.result.ui_events,
            "同一种子应可回放（api.random / api.dice）"
        );
    }

    #[test]
    fn card_has_state_hook_finds_functions_only() {
        assert!(card_has_state_hook(HOOK_TREE_CARD, "日常.夜谈", "on_enter"));
        assert!(card_has_state_hook(HOOK_TREE_CARD, "日常.夜谈", "on_exit"));
        assert!(card_has_state_hook(HOOK_TREE_CARD, "日常", "on_enter"));
        assert!(!card_has_state_hook(HOOK_TREE_CARD, "日常", "on_exit"));
        assert!(!card_has_state_hook(HOOK_TREE_CARD, "没这个状态", "on_enter"));
        assert!(!card_has_state_hook(HOOK_TREE_CARD, "非函数", "on_enter"));
        assert!(!card_has_state_hook(HOOK_TREE_CARD, "日常.夜谈", "on_context"));
        // 没有 state_tree / 源码坏掉：false，不 panic
        let plain = r#"return { name = "静卡", scenario = "s", personality = "p", first_mes = "f" }"#;
        assert!(!card_has_state_hook(plain, "日常", "on_enter"));
        assert!(!card_has_state_hook("return {", "日常", "on_enter"));
    }
}
