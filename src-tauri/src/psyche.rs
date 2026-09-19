// 心理运行时（Psyche Runtime）：把心理学二分法变成数据结构（设计 §9）
//
// 「人」的完备清单里（设计 §9.1），认知归记忆宫殿、倾向性归 codex 的
// needs/values/interests facet；而**情绪情感**与**意志**跨轮连续的那部分由本模块承载：
//
//   state.psyche = { affects[≤3]（名称 / 强度 / 来源 / 起始轮 / 历史采样）
//                    intents[]  （名称 / 强度 / 绑定剧情线 / 起始轮）
//                    temperament（rise / decay / threshold / impulsiveness）
//                    last_turn }
//
// 闭环（设计 §9.2）：
//   事件（消息 / 剧情线进展 / 被提及）
//     ─► 评价：需要满足 / 受挫 / 无关 —— need_hit 做规则命中，极性判断留给宿主/hooks
//     ─► 情绪：feel(名称, 强度, 来源) —— 槽位 ≤ 3，按气质参数衰减
//     ─► 意图：intend(名称, ±增量) —— 受挫可削弱，也可强化「想解释」
//     ─► 行为：台词倾向（summary_line 注入）／主动消息（wants_to_act）／表情（auto_emotion）
//     └► 外化：bind_thread —— 说出口的目的才是剧情线（意志的外显形态，§8）
//
// 数据归属：psyche 是**卡私有**的（设计 §2.1、§9.2），住在角色 state 的 psyche 子键里，
// 群聊互不可见——「她的忐忑只有她自己知道，直到演出来」。宿主每轮把 state 传进来
// （from_state）、把结果写回去（write_into），state 的其余键一律不动。
//
// 本模块是**纯数据与算法**（m2.md 决断 3）：不碰文件、不碰网络、不跑 Lua，
// 因而全部行为可被单测钉死。宿主只做三件事：传 state、调方法、写回 state。
//
// 行为规格（设计 §9.2，常量具名）：
//
// 1. feel：同名情绪 → intensity = clamp(max(旧, 新) + FEEL_STACK_STEP × rise, 0, 1)
//    （同向叠加但不爆表；来源更新为最近一次，since_turn 保持首次）；
//    无同名且有空槽 → 插入；槽满 → **替换最弱者**（同强度按 name 升序取最弱；新强度 <
//    最弱者则拒绝，accepted = false）；任何被接受的情绪都记一条 AffectTick{turn, intensity}
//    （面板的衰减轨迹）。
// 2. tick：affect.intensity ×= (1 − decay)^elapsed（elapsed ≤ 0 或非有限值按 1 轮），
//    跌破 AFFECT_FLOOR 即移除并计入 faded；intent.strength ×= (1 − INTENT_DECAY_PER_ROUND)
//    ^elapsed——意图比情绪慢得多（设计 §9 的「意志」是持续压力），其移除只走 intend(负增量)；
//    最强情绪进 strongest（同分按 name 升序）。
// 3. summary_line：B5 一格的主观世界（客观世界是 B1 现状卡），形如
//    「【小雨·内心】害羞0.7 忐忑0.6 ▸ 想解释(0.8,被害羞压着)!」——意图 strength ≥
//    temperament.threshold 加 ! 标记；存在 intensity ≥ PRESSURE_AFFECT_MIN 的情绪时给意图
//    加「被<情绪>压着」；无情绪且无意图返回空串（宿主据此省略 B5 空层）。
// 4. auto_emotion：最强情绪名（宿主映射 ui.emit("emotion", …)，情绪→差分表由卡/实体提供）。
// 5. wants_to_act：strength ≥ threshold − IMPULSE_THRESHOLD_STEP × impulsiveness
//    − ACTION_THRESHOLD_MARGIN 的意图名，按强度降序、同分按名字升序（阈值随冲动性下降）。
// 6. from_state / write_into 往返一致（write_into 后 from_state 得到同一 Psyche），
//    且不破坏 state 的其他键；坏形状一律宽容降级为默认，绝不 panic。
// 7. 全部输出确定：模块内不用 HashMap/HashSet，凡排序必显式写死规则
//    （强度降序 + 名字升序）——同输入两次调用同一结果（设计 §7.3 的可回放前提）。
//
// 读侧兼容：规范键是复数 psyche.affects / psyche.intents；M1 遗留的单数 psyche.affect
// （prompt.rs §4.1 A3 与 codex §6.3 状态词表读过的形态）也能读进来，值可以是情绪名、
// 名字数组或 {name, intensity} 对象数组。**写回只写规范键**，刻意不写双份数据；宿主若
// 还要用单数键，请改读 affects。
//
// 一个刻意的取舍：summary_line() 没有角色名参数（宿主集成的签名已钉死），它返回不带
// 角色名的【内心】形态；想要设计 §9.2 的完整形态（【小雨·内心】…）请调 summary_line_for(名)。
#![allow(dead_code)]

use std::cmp::Ordering;

use serde::Serialize;
use serde_json::{Map, Value};

// ---------- 常量：槽位与评价规则具名化（设计 §9.2）----------

/// 情绪槽位上限（设计 §9.2：槽位 ≤ 3）——一个人同时「惦记」的情绪就这么多。
pub const MAX_AFFECTS: usize = 3;

/// 情绪消退底线：强度低于此值即在本轮 tick 被移除，不再占槽（设计 §9.2 的衰减）。
pub const AFFECT_FLOOR: f32 = 0.05;

/// 同名情绪叠加步长：新强度 = clamp(max(旧, 新) + STEP × rise, 0, 1)（设计 §9.2 的「升速」）。
pub const FEEL_STACK_STEP: f32 = 0.1;

/// 意图每轮的衰减比例（设计 §9：「意志」是持续压力）——5%/轮，比情绪慢得多。
pub const INTENT_DECAY_PER_ROUND: f32 = 0.05;

/// 冲动性对主动行为阈值的下调步长（设计 §9.2：impulsiveness 参与触发阈值）。
pub const IMPULSE_THRESHOLD_STEP: f32 = 0.1;

/// 主动行为的固定余量：生效阈值 = threshold − STEP × impulsiveness − MARGIN。
pub const ACTION_THRESHOLD_MARGIN: f32 = 0.05;

/// 「被…压着」的情绪门槛（设计 §9.2 的例：惦记着坦白(0.4,被害羞压着)）。
pub const PRESSURE_AFFECT_MIN: f32 = 0.5;

/// 气质参数默认值（设计 §9.2：缺字段即 0.5 / 0.15 / 0.6 / 0.3）。
pub const DEFAULT_RISE: f32 = 0.5;
pub const DEFAULT_DECAY: f32 = 0.15;
pub const DEFAULT_THRESHOLD: f32 = 0.6;
pub const DEFAULT_IMPULSIVENESS: f32 = 0.3;

/// 每条情绪保留的历史采样上限（面板画衰减曲线用；防止卡 state 随轮次无界膨胀）。
pub const MAX_HISTORY_SAMPLES: usize = 32;

/// 读侧缺强度时的默认值（legacy psyche.affect: ["害羞"] 这类名字数组形态）。
pub const DEFAULT_AFFECT_INTENSITY: f32 = 0.5;

/// 读侧缺强度时的意图默认值（legacy 名字形态）。
pub const DEFAULT_INTENT_STRENGTH: f32 = 0.5;

/// 四组气质预设的名字（设计 §9.2；补全引擎照单要账，§9.3）。
pub const TEMPERAMENT_PRESETS: [&str; 4] = ["胆汁质", "多血质", "粘液质", "抑郁质"];

/// 卡 state 里 psyche 的子键（设计 §9.2：卡私有 state.psyche）。
pub const STATE_KEY: &str = "psyche";
/// 情绪槽数组键（规范形态，复数）。
pub const AFFECTS_KEY: &str = "affects";
/// 情绪槽数组键（M1 legacy 单数形态，只读兼容，见模块头注释）。
pub const LEGACY_AFFECT_KEY: &str = "affect";
/// 意图表键（规范形态）。
pub const INTENTS_KEY: &str = "intents";
/// 意图表键（legacy 单数形态，只读兼容）。
pub const LEGACY_INTENT_KEY: &str = "intent";
/// 气质参数键。
pub const TEMPERAMENT_KEY: &str = "temperament";
/// 最后推进到的轮次键。
pub const LAST_TURN_KEY: &str = "last_turn";

/// 条目字段名（读写两侧共用，避免手写字符串漂移）。
const NAME_KEY: &str = "name";
const INTENSITY_KEY: &str = "intensity";
const SOURCE_KEY: &str = "source";
const STRENGTH_KEY: &str = "strength";
const LINKED_THREAD_KEY: &str = "linked_thread";
const SINCE_TURN_KEY: &str = "since_turn";
const HISTORY_KEY: &str = "history";
const TURN_KEY: &str = "turn";
const RISE_KEY: &str = "rise";
const DECAY_KEY: &str = "decay";
const THRESHOLD_KEY: &str = "threshold";
const IMPULSIVENESS_KEY: &str = "impulsiveness";

// ---------- 数值兜底：一切外部输入都先过这里 ----------

/// 夹进 0..=1（NaN / 无穷等非法值按 0 处理）。
fn clamp01(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 读侧取数：接受 JSON 数字与数字字符串（手写的卡常把 0.8 写成 "0.8"）。
fn num_at(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.trim().parse::<f32>().ok(),
        _ => None,
    }
}

/// 读侧取轮次：接受数字（含浮点）与数字字符串。
fn u64_at(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// 把「数组 / 单个对象 / 单个字符串」统一成条目列表（坏形状给空表）。
fn as_entries(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) | Value::String(_) => vec![v],
        _ => Vec::new(),
    }
}

/// 序列化兜底：这些类型不可能失败，失败也只降级为空值，绝不 panic。
fn to_value_or<T: Serialize>(v: &T, fallback: Value) -> Value {
    serde_json::to_value(v).unwrap_or(fallback)
}

// ---------- 气质参数（设计 §9.2）----------

/// 气质参数：参数化情绪与意志的动态（升速 / 衰减 / 阈值 / 冲动性）。
///
/// 写在 codex char 实体或随卡默认；四组古典预设见 Temperament::preset。
/// 四个字段都在 0..=1，读侧一律夹取。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Temperament {
    /// 情绪升速：同名情绪再次被激起时的叠加步长（乘 FEEL_STACK_STEP）。
    pub rise: f32,
    /// 情绪衰减：每轮乘 (1 − decay)。
    pub decay: f32,
    /// 主动行为触发阈值：intent.strength 达到它才「想说出口」（还要减去冲动性加成）。
    pub threshold: f32,
    /// 冲动性：越高，主动行为阈值越低。
    pub impulsiveness: f32,
}

impl Default for Temperament {
    fn default() -> Self {
        Temperament {
            rise: DEFAULT_RISE,
            decay: DEFAULT_DECAY,
            threshold: DEFAULT_THRESHOLD,
            impulsiveness: DEFAULT_IMPULSIVENESS,
        }
    }
}

impl Temperament {
    /// 从 temperament = { rise, decay, threshold, impulsiveness } 读参数：
    /// 缺字段 / 非数字 / 越界一律回落到默认值或夹进 0..=1。
    pub fn from_value(v: &Value) -> Temperament {
        let d = Temperament::default();
        let obj = match v {
            Value::Object(m) => m,
            _ => return d,
        };
        Temperament {
            rise: num_at(obj.get(RISE_KEY)).map(clamp01).unwrap_or(d.rise),
            decay: num_at(obj.get(DECAY_KEY)).map(clamp01).unwrap_or(d.decay),
            threshold: num_at(obj.get(THRESHOLD_KEY))
                .map(clamp01)
                .unwrap_or(d.threshold),
            impulsiveness: num_at(obj.get(IMPULSIVENESS_KEY))
                .map(clamp01)
                .unwrap_or(d.impulsiveness),
        }
    }

    /// 四组气质预设（设计 §9.2）。未知名字回落默认参数（宽容，不 panic）。
    ///
    /// | 预设 | rise | decay | threshold | impulsiveness | 行为侧写 |
    /// |---|---|---|---|---|---|
    /// | 胆汁质 choleric | 0.80 | 0.20 | 0.45 | 0.75 | 快起、消退慢、阈值低、最冲动 |
    /// | 多血质 sanguine | 0.70 | 0.40 | 0.55 | 0.55 | 快起快落、爱搭话 |
    /// | 粘液质 phlegmatic | 0.30 | 0.15 | 0.70 | 0.20 | 慢起慢落、沉得住气 |
    /// | 抑郁质 melancholic | 0.50 | 0.10 | 0.65 | 0.10 | 起得不慢、最持久、最少主动 |
    pub fn preset(name: &str) -> Temperament {
        match name.trim() {
            "胆汁质" | "choleric" => Temperament {
                rise: 0.80,
                decay: 0.20,
                threshold: 0.45,
                impulsiveness: 0.75,
            },
            "多血质" | "sanguine" => Temperament {
                rise: 0.70,
                decay: 0.40,
                threshold: 0.55,
                impulsiveness: 0.55,
            },
            "粘液质" | "phlegmatic" => Temperament {
                rise: 0.30,
                decay: 0.15,
                threshold: 0.70,
                impulsiveness: 0.20,
            },
            "抑郁质" | "melancholic" => Temperament {
                rise: 0.50,
                decay: 0.10,
                threshold: 0.65,
                impulsiveness: 0.10,
            },
            _ => Temperament::default(),
        }
    }

    /// 主动行为的生效阈值：基础阈值 − 冲动性加成 − 固定余量（设计 §9.2），下限 0。
    fn action_threshold(&self) -> f32 {
        (clamp01(self.threshold)
            - IMPULSE_THRESHOLD_STEP * clamp01(self.impulsiveness)
            - ACTION_THRESHOLD_MARGIN)
            .max(0.0)
    }
}

// ---------- 情绪（设计 §9.2 的 affect）----------

/// 一条历史采样（面板的衰减轨迹：第 turn 轮时强度为 intensity）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AffectTick {
    pub turn: u64,
    pub intensity: f32,
}

/// 一个情绪槽位：名称 + 强度 + 来源 + 起始轮 + 历史采样。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Affect {
    /// 情绪名（如「忐忑」）——渲染与差分表命中的键。
    pub name: String,
    /// 强度 0..=1。
    pub intensity: f32,
    /// 来源（事件/消息/剧情线的可读标签，面板溯源用）。
    pub source: String,
    /// 起始轮：情绪跨轮连续的锚（同名叠加不重置它）。
    pub since_turn: u64,
    /// 历史采样（最近 MAX_HISTORY_SAMPLES 条）。
    pub history: Vec<AffectTick>,
}

impl Affect {
    /// 采样当前强度（超过上限丢最旧的一条）。
    fn push_tick(&mut self, turn: u64) {
        if self.history.len() >= MAX_HISTORY_SAMPLES {
            self.history.remove(0);
        }
        self.history.push(AffectTick {
            turn,
            intensity: self.intensity,
        });
    }
}

/// 意图（设计 §9.2 的意志内隐形态）：强度 + 可选绑定的剧情线。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Intent {
    /// 意图名（如「惦记着坦白」）。
    pub name: String,
    /// 强度 0..=1（≤ 0 即移除）。
    pub strength: f32,
    /// 外化目标：说出口后由宿主 api.threads.open 建线并绑上（设计 §9.2）。
    pub linked_thread: Option<String>,
    /// 起始轮。
    pub since_turn: u64,
}

// ---------- 心理状态（卡私有 state.psyche）----------

/// 一个角色的心理状态（卡私有，群聊互不可见）。
///
/// 宿主每轮：let mut p = Psyche::from_state(&state); …; p.write_into(&mut state);
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Psyche {
    /// 情绪槽（≤ MAX_AFFECTS）。
    pub affects: Vec<Affect>,
    /// 意图表（意志的内隐形态）。
    pub intents: Vec<Intent>,
    /// 气质参数。
    pub temperament: Temperament,
    /// 最后推进到的轮次（宿主判断是否已 tick 过这一轮）。
    pub last_turn: u64,
}

/// feel 的结果：宿主据此决定要不要发 ui.emit 表情事件。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeelOutcome {
    /// 是否被接受（槽满且新强度不够 → false）。
    pub accepted: bool,
    /// 被挤掉的情绪名（槽满替换时）。
    pub replaced: Option<String>,
    /// 可读说明（入槽 / 叠加 / 替换 / 拒绝的原因）。
    pub reason: String,
    /// 落定后的强度（被拒时为 0.0）。
    pub intensity: f32,
}

/// tick 的结果：本轮变化。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TickOutcome {
    /// 本轮消退（跌破 AFFECT_FLOOR）的情绪名，按槽位顺序。
    pub faded: Vec<String>,
    /// 本轮最强的情绪名（同分按 name 升序）；无情绪为 None。
    pub strongest: Option<String>,
}

impl Psyche {
    /// 读 state["psyche"]：缺失、非对象或坏形状一律宽容降级为默认（绝不 panic）。
    ///
    /// 规范键是 affects / intents；缺失时回落到 legacy 单数键 affect / intent
    /// （见模块头注释的读侧兼容）。
    pub fn from_state(state: &Value) -> Psyche {
        let raw = match state.get(STATE_KEY) {
            Some(v) if v.is_object() => v,
            _ => return Psyche::default(),
        };
        let affects = match raw.get(AFFECTS_KEY) {
            Some(v) => read_affects(v),
            None => raw
                .get(LEGACY_AFFECT_KEY)
                .map(read_affects)
                .unwrap_or_default(),
        };
        let intents = match raw.get(INTENTS_KEY) {
            Some(v) => read_intents(v),
            None => raw
                .get(LEGACY_INTENT_KEY)
                .map(read_intents)
                .unwrap_or_default(),
        };
        Psyche {
            affects,
            intents,
            temperament: raw
                .get(TEMPERAMENT_KEY)
                .map(Temperament::from_value)
                .unwrap_or_default(),
            last_turn: u64_at(raw.get(LAST_TURN_KEY)).unwrap_or(0),
        }
    }

    /// 写回 state["psyche"]（整段覆盖，state 的其余键一律不动）。
    ///
    /// state 不是对象时先就地补成对象——宿主传 null 也不会丢数据。
    pub fn write_into(&self, state: &mut Value) {
        if !state.is_object() {
            *state = Value::Object(Map::new());
        }
        let root = match state.as_object_mut() {
            Some(m) => m,
            None => return, // 上面刚保证是对象，这里只是不肯 unwrap
        };
        let mut psyche = Map::new();
        psyche.insert(
            AFFECTS_KEY.to_string(),
            to_value_or(&self.affects, Value::Array(Vec::new())),
        );
        psyche.insert(
            INTENTS_KEY.to_string(),
            to_value_or(&self.intents, Value::Array(Vec::new())),
        );
        psyche.insert(
            TEMPERAMENT_KEY.to_string(),
            to_value_or(&self.temperament, Value::Null),
        );
        psyche.insert(LAST_TURN_KEY.to_string(), Value::from(self.last_turn));
        root.insert(STATE_KEY.to_string(), Value::Object(psyche));
    }

    /// 情绪进入（设计 §9.2 的 api.psyche.feel(名称, 强度, 来源)）。
    ///
    /// - 同名：intensity = clamp(max(旧, 新) + FEEL_STACK_STEP × rise, 0, 1)，
    ///   来源更新为最近一次，since_turn 保持首次；
    /// - 空槽：插入；
    /// - 槽满：替换最弱者（同强度按 name 升序取最弱）；新强度 < 最弱者则拒绝；
    /// - 被接受的情绪都会补一条历史采样。
    ///
    /// 无论接受与否，last_turn 都推进到 turn（轮次记账，单调不回退）。
    pub fn feel(&mut self, name: &str, intensity: f32, source: &str, turn: u64) -> FeelOutcome {
        if turn > self.last_turn {
            self.last_turn = turn;
        }
        let name = name.trim();
        if name.is_empty() {
            return reject("情绪名为空，忽略");
        }
        let value = clamp01(intensity);
        if value <= 0.0 {
            return reject("强度非正（或非法），忽略");
        }
        let rise = clamp01(self.temperament.rise);
        let source = source.trim().to_string();

        // 同名：同向叠加（升速由气质参数决定），不爆表
        if let Some(a) = self.affects.iter_mut().find(|a| a.name == name) {
            a.intensity = clamp01(a.intensity.max(value) + FEEL_STACK_STEP * rise);
            a.source = source;
            a.push_tick(turn);
            return FeelOutcome {
                accepted: true,
                replaced: None,
                reason: format!("同名情绪叠加至 {:.2}", a.intensity),
                intensity: a.intensity,
            };
        }

        let fresh = |v: f32| {
            let mut a = Affect {
                name: name.to_string(),
                intensity: v,
                source: source.clone(),
                since_turn: turn,
                history: Vec::new(),
            };
            a.push_tick(turn);
            a
        };

        // 有空槽：直接入座
        if self.affects.len() < MAX_AFFECTS {
            let a = fresh(value);
            let settled = a.intensity;
            self.affects.push(a);
            return FeelOutcome {
                accepted: true,
                replaced: None,
                reason: format!("情绪入槽（{}/{}）", self.affects.len(), MAX_AFFECTS),
                intensity: settled,
            };
        }

        // 槽满：按强度互斥（同强度按 name 升序取最弱）
        let idx = self.weakest_affect_index().unwrap_or(0);
        let weakest = &self.affects[idx];
        if value < weakest.intensity {
            return reject(&format!(
                "槽位已满，新强度 {:.2} 低于最弱者「{}」{:.2}，未替换",
                value, weakest.name, weakest.intensity
            ));
        }
        let old = std::mem::replace(&mut self.affects[idx], fresh(value));
        FeelOutcome {
            accepted: true,
            replaced: Some(old.name.clone()),
            reason: format!(
                "槽位已满，替换最弱者「{}」（{:.2} → {:.2}）",
                old.name, old.intensity, value
            ),
            intensity: value,
        }
    }

    /// 每轮推进（设计 §9.2 的确定性维护部分）：情绪与意图各自衰减，返回本轮变化。
    ///
    /// elapsed_rounds ≤ 0（含 NaN）视作 1 轮；故事时钟大跳时宿主可传多轮一次推进。
    /// 意图只衰减不移除——「意志」是持续压力，移除只走 intend 的负增量。
    pub fn tick(&mut self, turn: u64, elapsed_rounds: f32) -> TickOutcome {
        if turn > self.last_turn {
            self.last_turn = turn;
        }
        let elapsed = if elapsed_rounds.is_finite() && elapsed_rounds > 0.0 {
            elapsed_rounds
        } else {
            1.0
        };
        let affect_factor = (1.0 - clamp01(self.temperament.decay)).powf(elapsed);
        let intent_factor = (1.0 - INTENT_DECAY_PER_ROUND).powf(elapsed);

        let mut faded = Vec::new();
        let mut kept = Vec::with_capacity(self.affects.len());
        for mut a in std::mem::take(&mut self.affects) {
            a.intensity = clamp01(a.intensity * affect_factor);
            if a.intensity < AFFECT_FLOOR {
                faded.push(a.name); // 消退：不再占槽
            } else {
                a.push_tick(turn); // 衰减轨迹采样
                kept.push(a);
            }
        }
        self.affects = kept;

        for it in &mut self.intents {
            it.strength = clamp01(it.strength * intent_factor);
        }

        TickOutcome {
            faded,
            strongest: self.strongest_affect().map(|a| a.name.clone()),
        }
    }

    /// 意图强度增减：strength = clamp(strength + delta, 0, 1)，≤ 0 即移除。
    ///
    /// 受挫给负增量（削弱），被误解时给正增量（强化「想解释」，设计 §9.2）。
    /// 不存在同名意图时，只有正增量才建新意图（负增量无对象可削，返回 None）。
    pub fn intend(&mut self, name: &str, delta: f32, turn: u64) -> Option<Intent> {
        if turn > self.last_turn {
            self.last_turn = turn;
        }
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let delta = if delta.is_finite() { delta } else { 0.0 };
        if let Some(pos) = self.intents.iter().position(|i| i.name == name) {
            let strength = clamp01(self.intents[pos].strength + delta);
            if strength <= 0.0 {
                self.intents.remove(pos);
                return None;
            }
            self.intents[pos].strength = strength;
            return Some(self.intents[pos].clone());
        }
        if delta <= 0.0 {
            return None;
        }
        let it = Intent {
            name: name.to_string(),
            strength: clamp01(delta),
            linked_thread: None,
            since_turn: turn,
        };
        self.intents.push(it.clone());
        Some(it)
    }

    /// 意志外化为剧情线（设计 §9.2）：把意图绑到 thread_id，返回是否绑上。
    ///
    /// 意图不存在或 thread_id 为空则返回 false；重复绑定即改绑（外化目标可换）。
    pub fn bind_thread(&mut self, intent: &str, thread_id: &str) -> bool {
        let intent = intent.trim();
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            return false;
        }
        match self.intents.iter_mut().find(|i| i.name == intent) {
            Some(i) => {
                i.linked_thread = Some(thread_id.to_string());
                true
            }
            None => false,
        }
    }

    /// 最强情绪（intensity 最大，同分按 name 升序）。
    pub fn strongest_affect(&self) -> Option<&Affect> {
        self.affects.iter().max_by(|a, b| {
            a.intensity
                .partial_cmp(&b.intensity)
                .unwrap_or(Ordering::Equal)
                .then_with(|| b.name.cmp(&a.name)) // 同分时让 name 小者胜出
        })
    }

    /// 自动表情（设计 §9.2）：最强情绪的名字；无情绪返回 None。
    ///
    /// 宿主据此 ui.emit("emotion", …)，情绪 → 立绘差分表由卡/实体提供。
    pub fn auto_emotion(&self) -> Option<String> {
        self.strongest_affect().map(|a| a.name.clone())
    }

    /// 主动行为触发（设计 §9.2：intent.strength ≥ 阈值 且冲动性加成）。
    ///
    /// 生效阈值 = temperament.threshold − IMPULSE_THRESHOLD_STEP × impulsiveness
    /// − ACTION_THRESHOLD_MARGIN（阈值随冲动性下降）。
    /// 返回意图名，按 strength 降序、同分按名字升序。
    pub fn wants_to_act(&self) -> Vec<String> {
        let threshold = self.temperament.action_threshold();
        let mut hits: Vec<&Intent> = self
            .intents
            .iter()
            .filter(|i| i.strength > 0.0 && i.strength >= threshold)
            .collect();
        hits.sort_by(|a, b| {
            b.strength
                .partial_cmp(&a.strength)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        hits.into_iter().map(|i| i.name.clone()).collect()
    }

    /// B5 一行摘要（设计 §9.2 的「主观世界」；无角色名的退化形态，见 summary_line_for）。
    pub fn summary_line(&self) -> String {
        self.summary_line_for("")
    }

    /// B5 一行摘要（带角色名，设计 §9.2 的例：【小雨·内心】忐忑0.6 ▸ 惦记着坦白(0.4)）。
    ///
    /// 槽内全部情绪按强度降序（同分按名字升序）列出；意图另起一段（▸ 之后），
    /// strength ≥ threshold 加 ! 标记，有 ≥ PRESSURE_AFFECT_MIN 的情绪时加「被<情绪>压着」。
    /// 无情绪且无意图（或都低于 AFFECT_FLOOR 的噪声）返回空串——宿主据此省略 B5 空层。
    pub fn summary_line_for(&self, owner: &str) -> String {
        let mut affects: Vec<&Affect> = self
            .affects
            .iter()
            .filter(|a| a.intensity >= AFFECT_FLOOR)
            .collect();
        affects.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut intents: Vec<&Intent> = self
            .intents
            .iter()
            .filter(|i| i.strength >= AFFECT_FLOOR)
            .collect();
        intents.sort_by(|a, b| {
            b.strength
                .partial_cmp(&a.strength)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });

        let pressure = affects
            .iter()
            .find(|a| a.intensity >= PRESSURE_AFFECT_MIN)
            .map(|a| a.name.clone());
        let threshold = clamp01(self.temperament.threshold);

        let affect_part = affects
            .iter()
            .map(|a| format!("{}{:.1}", a.name, a.intensity))
            .collect::<Vec<_>>()
            .join(" ");
        let intent_part = intents
            .iter()
            .map(|i| {
                let mut s = format!("{}({:.1}", i.name, i.strength);
                if let Some(p) = &pressure {
                    s.push_str(&format!(",被{p}压着"));
                }
                s.push(')');
                if i.strength >= threshold {
                    s.push('!');
                }
                s
            })
            .collect::<Vec<_>>()
            .join(" ");

        let body = match (affect_part.is_empty(), intent_part.is_empty()) {
            (true, true) => return String::new(),
            (false, true) => affect_part,
            (true, false) => intent_part,
            (false, false) => format!("{affect_part} ▸ {intent_part}"),
        };
        let owner = owner.trim();
        if owner.is_empty() {
            format!("【内心】{body}")
        } else {
            format!("【{owner}·内心】{body}")
        }
    }

    /// 面板用：每条情绪的衰减轨迹（历史采样）。
    ///
    /// 返回 (情绪名, 采样)，按当前强度降序、同分按名字升序（与摘要同序）。
    pub fn decay_trail(&self) -> Vec<(String, Vec<AffectTick>)> {
        let mut slots: Vec<&Affect> = self.affects.iter().collect();
        slots.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        slots
            .into_iter()
            .map(|a| (a.name.clone(), a.history.clone()))
            .collect()
    }

    /// api.psyche.get：按名字取情绪（宿主 L2 沙箱 API 与面板用）。
    pub fn affect(&self, name: &str) -> Option<&Affect> {
        let name = name.trim();
        self.affects.iter().find(|a| a.name == name)
    }

    /// api.psyche.get：按名字取意图。
    pub fn intent(&self, name: &str) -> Option<&Intent> {
        let name = name.trim();
        self.intents.iter().find(|i| i.name == name)
    }

    /// 当前情绪槽里最弱者的下标（同强度按 name 升序取第一个）。
    fn weakest_affect_index(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, a) in self.affects.iter().enumerate() {
            match best {
                None => best = Some(i),
                Some(j) => {
                    let cur = &self.affects[j];
                    if a.intensity < cur.intensity
                        || (a.intensity == cur.intensity && a.name < cur.name)
                    {
                        best = Some(i);
                    }
                }
            }
        }
        best
    }
}

fn reject(reason: &str) -> FeelOutcome {
    FeelOutcome {
        accepted: false,
        replaced: None,
        reason: reason.to_string(),
        intensity: 0.0,
    }
}

// ---------- 读侧解析（宽容降级）----------

fn read_affects(v: &Value) -> Vec<Affect> {
    let mut out: Vec<Affect> = Vec::new();
    for entry in as_entries(v) {
        let (name, intensity, source, since_turn, history) = match entry {
            Value::String(s) => (
                s.trim().to_string(),
                DEFAULT_AFFECT_INTENSITY,
                String::new(),
                0,
                Vec::new(),
            ),
            Value::Object(m) => (
                m.get(NAME_KEY)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                num_at(m.get(INTENSITY_KEY))
                    .map(clamp01)
                    .unwrap_or(DEFAULT_AFFECT_INTENSITY),
                m.get(SOURCE_KEY)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                u64_at(m.get(SINCE_TURN_KEY)).unwrap_or(0),
                read_history(m.get(HISTORY_KEY)),
            ),
            _ => continue, // 数字/布尔/null 之类的噪声条目直接忽略
        };
        if name.is_empty() || intensity <= 0.0 {
            continue;
        }
        if out.iter().any(|a| a.name == name) {
            continue; // 同名只留第一条（槽位互斥的不变式）
        }
        out.push(Affect {
            name,
            intensity,
            source,
            since_turn,
            history,
        });
    }
    if out.len() > MAX_AFFECTS {
        // 坏数据/旧版本超编：按强度降序取前 MAX_AFFECTS（确定性规则）
        out.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        out.truncate(MAX_AFFECTS);
    }
    out
}

fn read_history(v: Option<&Value>) -> Vec<AffectTick> {
    let arr = match v.and_then(Value::as_array) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for e in arr {
        match e {
            Value::Object(m) => out.push(AffectTick {
                turn: u64_at(m.get(TURN_KEY)).unwrap_or(0),
                intensity: num_at(m.get(INTENSITY_KEY)).map(clamp01).unwrap_or(0.0),
            }),
            Value::Number(_) => out.push(AffectTick {
                turn: 0,
                intensity: num_at(Some(e)).map(clamp01).unwrap_or(0.0),
            }),
            _ => {}
        }
    }
    if out.len() > MAX_HISTORY_SAMPLES {
        let cut = out.len() - MAX_HISTORY_SAMPLES;
        out.drain(0..cut);
    }
    out
}

fn read_intents(v: &Value) -> Vec<Intent> {
    let mut out: Vec<Intent> = Vec::new();
    for entry in as_entries(v) {
        let (name, strength, linked_thread, since_turn) = match entry {
            Value::String(s) => (s.trim().to_string(), DEFAULT_INTENT_STRENGTH, None, 0),
            Value::Object(m) => (
                m.get(NAME_KEY)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                num_at(m.get(STRENGTH_KEY)).map(clamp01).unwrap_or(0.0),
                m.get(LINKED_THREAD_KEY)
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                u64_at(m.get(SINCE_TURN_KEY)).unwrap_or(0),
            ),
            _ => continue,
        };
        if name.is_empty() || strength <= 0.0 {
            continue;
        }
        if out.iter().any(|i| i.name == name) {
            continue;
        }
        out.push(Intent {
            name,
            strength,
            linked_thread,
            since_turn,
        });
    }
    out
}

// ---------- 需要评价（设计 §9.2 闭环的第一步）----------

/// 需要命中（设计 §9.2：需要满足 / 受挫 / 无关）。
///
/// 宿主把 codex char 的 needs/values 清单与待评价文本交给它，做**确定性规则命中**：
/// 命中 = 文本里出现了该需要的名字（去空白后大小写不敏感的包含匹配）。
/// 返回命中的需要（返回去空白后的原名，按 needs 顺序、同名去重）。
///
/// 极性（满足还是受挫）不在这里判断——那是 hooks 规则或管线提案的事（§9.2「维护分工」），
/// 本函数只回答「这条消息碰到了哪些需要」。
pub fn need_hit(needs: &[String], text: &str) -> Vec<String> {
    let haystack = text.to_lowercase();
    if haystack.trim().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for need in needs {
        let key = need.trim();
        if key.is_empty() {
            continue; // 空串是任何文本的子串，必须挡掉
        }
        if haystack.contains(&key.to_lowercase()) && !out.iter().any(|o| o == key) {
            out.push(key.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- 测试脚手架 ----------

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "期望 {b}，实际 {a}");
    }

    fn names(p: &Psyche) -> Vec<String> {
        p.affects.iter().map(|a| a.name.clone()).collect()
    }

    fn intent_names(p: &Psyche) -> Vec<String> {
        p.intents.iter().map(|i| i.name.clone()).collect()
    }

    fn with(t: Temperament) -> Psyche {
        Psyche {
            temperament: t,
            ..Psyche::default()
        }
    }

    /// 一段可重放的脚本：确定性测试与往返测试共用。
    fn run_script() -> Psyche {
        let mut p = with(Temperament::preset("多血质"));
        p.feel("忐忑", 0.7, "便签", 1);
        p.feel("害羞", 0.6, "被看穿", 1);
        p.feel("烦躁", 0.2, "被打断", 2);
        p.intend("惦记着坦白", 0.5, 1);
        p.intend("想解释", 0.6, 2);
        p.tick(2, 1.0);
        p.tick(3, 1.5);
        p.feel("期待", 0.4, "约定", 3);
        p.intend("想解释", -0.2, 3);
        p.bind_thread("想解释", "thread.周五还书");
        p
    }

    // ---------- from_state / write_into ----------

    #[test]
    fn from_state_defaults_when_psyche_missing() {
        let d = Psyche::default();
        for state in [json!({}), json!({"affection": 10}), json!(null)] {
            let p = Psyche::from_state(&state);
            assert_eq!(p, d, "缺 psyche 时给默认");
        }
        assert_eq!(d.temperament, Temperament::default());
        assert_eq!(d.last_turn, 0);
        assert!(d.affects.is_empty() && d.intents.is_empty());
    }

    #[test]
    fn from_state_tolerates_bad_shapes() {
        // psyche 本身不是对象
        for bad in [json!({"psyche": 7}), json!({"psyche": "nope"}), json!({"psyche": []})] {
            assert_eq!(Psyche::from_state(&bad), Psyche::default());
        }
        // 子键形状全错
        let p = Psyche::from_state(&json!({
            "psyche": {
                "affects": [1, null, true, {"nope": 1}, {"name": "   "}, "  "],
                "intents": 5,
                "temperament": 3,
                "last_turn": -4
            }
        }));
        assert!(p.affects.is_empty());
        assert!(p.intents.is_empty());
        assert_eq!(p.temperament, Temperament::default());
        assert_eq!(p.last_turn, 0);

        // 单个对象 / 单个字符串 / 数字字符串 / 越界强度也吃得下
        let p = Psyche::from_state(&json!({
            "psyche": {
                "affects": {"name": "害羞", "intensity": 9.0},
                "intents": {"name": "想解释", "strength": "0.8"},
                "last_turn": "12"
            }
        }));
        assert_eq!(names(&p), vec!["害羞"]);
        approx(p.affects[0].intensity, 1.0);
        assert_eq!(intent_names(&p), vec!["想解释"]);
        approx(p.intents[0].strength, 0.8);
        assert_eq!(p.last_turn, 12);

        let p = Psyche::from_state(&json!({
            "psyche": {"affects": "害羞", "intents": "想解释"}
        }));
        assert_eq!(names(&p), vec!["害羞"]);
        assert_eq!(intent_names(&p), vec!["想解释"]);
    }

    #[test]
    fn from_state_reads_legacy_singular_affect() {
        let p = Psyche::from_state(&json!({
            "psyche": {"affect": ["害羞", {"name": "忐忑", "intensity": 0.7}]}
        }));
        assert_eq!(names(&p), vec!["害羞", "忐忑"]);
        approx(p.affects[0].intensity, DEFAULT_AFFECT_INTENSITY);
        approx(p.affects[1].intensity, 0.7);
        // 规范键在场时优先，legacy 键被忽略
        let p = Psyche::from_state(&json!({
            "psyche": {"affects": [{"name": "忐忑", "intensity": 0.3}], "affect": ["害羞"]}
        }));
        assert_eq!(names(&p), vec!["忐忑"]);
    }

    #[test]
    fn from_state_caps_slots_and_dedupes() {
        let p = Psyche::from_state(&json!({
            "psyche": {"affects": [
                {"name": "一", "intensity": 0.2},
                {"name": "二", "intensity": 0.9},
                {"name": "三", "intensity": 0.4},
                {"name": "四", "intensity": 0.7},
                {"name": "一", "intensity": 0.95}
            ]}
        }));
        // 同名只留第一条，超编按强度降序取前 3
        assert_eq!(names(&p), vec!["二", "四", "三"]);
        assert_eq!(p.affects.len(), MAX_AFFECTS);
    }

    #[test]
    fn from_state_reads_temperament_and_history() {
        let p = Psyche::from_state(&json!({
            "psyche": {
                "affects": [{"name": "忐忑", "intensity": 0.6, "source": "便签",
                             "since_turn": 3,
                             "history": [{"turn": 3, "intensity": 0.6}, {"turn": 4, "intensity": 0.5}]}],
                "intents": [{"name": "想解释", "strength": 0.8, "linked_thread": "thread.x", "since_turn": 4}],
                "temperament": {"rise": 0.8, "decay": 0.2, "threshold": 0.45, "impulsiveness": 0.75},
                "last_turn": 4
            }
        }));
        assert_eq!(p.temperament, Temperament::preset("胆汁质"));
        assert_eq!(p.affects[0].source, "便签");
        assert_eq!(p.affects[0].since_turn, 3);
        assert_eq!(p.affects[0].history.len(), 2);
        assert_eq!(p.affects[0].history[1].turn, 4);
        approx(p.affects[0].history[1].intensity, 0.5);
        assert_eq!(p.intents[0].linked_thread.as_deref(), Some("thread.x"));
        assert_eq!(p.last_turn, 4);
    }

    #[test]
    fn write_into_round_trips_and_keeps_other_keys() {
        let p = run_script();
        let mut state = json!({
            "affection": 42,
            "flags": ["a", "b"],
            "psyche": {"junk": true, "affect": ["旧"]}
        });
        p.write_into(&mut state);
        // 其余顶层键原样不动
        assert_eq!(state["affection"], json!(42));
        assert_eq!(state["flags"], json!(["a", "b"]));
        // psyche 整段覆盖，不残留坏数据
        assert_eq!(state["psyche"].get("junk"), None);
        assert_eq!(state["psyche"].get("affect"), None);
        assert!(state["psyche"][AFFECTS_KEY].is_array());
        // 往返一致
        assert_eq!(Psyche::from_state(&state), p);
        // 再写一次也不漂移（幂等）
        let mut again = state.clone();
        Psyche::from_state(&state).write_into(&mut again);
        assert_eq!(again, state);
    }

    #[test]
    fn write_into_makes_non_object_state_usable() {
        let mut p = with(Temperament::preset("抑郁质"));
        p.feel("忐忑", 0.8, "便签", 1);
        let mut state = json!(null);
        p.write_into(&mut state);
        assert!(state.is_object());
        assert_eq!(Psyche::from_state(&state), p);
    }

    // ---------- feel：入槽 / 叠加 / 替换 / 拒绝 ----------

    #[test]
    fn feel_inserts_new_affect() {
        let mut p = Psyche::default();
        let out = p.feel("忐忑", 0.6, "便签", 3);
        assert!(out.accepted);
        assert_eq!(out.replaced, None);
        approx(out.intensity, 0.6);
        assert_eq!(names(&p), vec!["忐忑"]);
        assert_eq!(p.affects[0].source, "便签");
        assert_eq!(p.affects[0].since_turn, 3);
        assert_eq!(p.affects[0].history.len(), 1);
        assert_eq!(p.affects[0].history[0].turn, 3);
        assert_eq!(p.last_turn, 3);
    }

    #[test]
    fn feel_same_name_stacks_without_overflow() {
        let mut p = Psyche::default(); // rise = 0.5
        p.feel("忐忑", 0.4, "便签", 1);
        let out = p.feel("忐忑", 0.9, "被看穿", 2);
        assert!(out.accepted);
        assert_eq!(out.replaced, None);
        approx(out.intensity, 0.9 + FEEL_STACK_STEP * DEFAULT_RISE);
        assert_eq!(p.affects.len(), 1, "同名不占第二个槽");
        assert_eq!(p.affects[0].since_turn, 1, "起始轮保持首次");
        assert_eq!(p.affects[0].source, "被看穿", "来源更新为最近一次");
        assert_eq!(p.affects[0].history.len(), 2);
        approx(p.affects[0].history[0].intensity, 0.4);
        approx(p.affects[0].history[1].intensity, 0.95);

        // 取 max 而不是相加：旧值更高时不会被新弱值拉低
        let out = p.feel("忐忑", 0.2, "旧事重提", 3);
        approx(out.intensity, 0.95 + FEEL_STACK_STEP * DEFAULT_RISE);

        // 反复叠加不爆表
        for turn in 4..40 {
            p.feel("忐忑", 1.0, "x", turn);
        }
        approx(p.affects[0].intensity, 1.0);
    }

    #[test]
    fn feel_replaces_weakest_when_full() {
        let mut p = Psyche::default();
        p.feel("害羞", 0.9, "", 1);
        p.feel("忐忑", 0.8, "", 1);
        p.feel("烦躁", 0.2, "", 1);
        let out = p.feel("期待", 0.5, "约定", 2);
        assert!(out.accepted);
        assert_eq!(out.replaced.as_deref(), Some("烦躁"));
        assert!(out.reason.contains("替换"));
        approx(out.intensity, 0.5);
        assert_eq!(names(&p), vec!["害羞", "忐忑", "期待"], "替换发生在原槽位");
        assert_eq!(p.affects[2].since_turn, 2);
        assert_eq!(p.affects[2].history.len(), 1);
        assert_eq!(p.affects[2].source, "约定");
    }

    #[test]
    fn feel_replacement_tie_goes_to_lower_name() {
        let mut p = Psyche::default();
        p.feel("忐忑", 0.5, "", 1);
        p.feel("害羞", 0.5, "", 1);
        p.feel("烦躁", 0.9, "", 1);
        // 同强度按 name 升序取最弱：害羞（U+5BB3）< 忐忑（U+5FD0）
        let out = p.feel("期待", 0.5, "", 2);
        assert!(out.accepted);
        assert_eq!(out.replaced.as_deref(), Some("害羞"));
        // 新强度 == 最弱者强度：允许替换（只有「低于」才拒绝）
        assert_eq!(names(&p), vec!["忐忑", "期待", "烦躁"]);
    }

    #[test]
    fn feel_rejects_weak_affect_when_full() {
        let mut p = Psyche::default();
        p.feel("害羞", 0.9, "", 1);
        p.feel("忐忑", 0.8, "", 1);
        p.feel("烦躁", 0.5, "", 1);
        let before = p.clone();
        let out = p.feel("失落", 0.3, "被拒绝", 2);
        assert!(!out.accepted);
        assert_eq!(out.replaced, None);
        approx(out.intensity, 0.0);
        assert!(out.reason.contains("未替换"), "理由要说明拒绝原因：{}", out.reason);
        assert_eq!(p.affects, before.affects, "拒绝不改变任何槽位");
        assert!(p.affects.iter().all(|a| a.history.len() == 1));
    }

    #[test]
    fn feel_rejects_empty_name_and_bad_intensity() {
        let mut p = Psyche::default();
        for (name, value) in [("   ", 0.5), ("忐忑", 0.0), ("忐忑", -1.0), ("忐忑", f32::NAN)] {
            let out = p.feel(name, value, "x", 1);
            assert!(!out.accepted, "{name}/{value} 应被拒绝");
            assert_eq!(out.replaced, None);
        }
        assert!(p.affects.is_empty());
        assert_eq!(p.last_turn, 1, "被拒绝也推进了轮次记账");
        // 越界强度夹进 0..=1
        let out = p.feel("忐忑", 5.0, "x", 1);
        approx(out.intensity, 1.0);
    }

    #[test]
    fn affect_slots_never_exceed_max() {
        let mut p = Psyche::default();
        let feed = ["一", "二", "三", "四", "五", "六"];
        for (i, name) in feed.iter().enumerate() {
            p.feel(name, 0.60 + 0.01 * i as f32, "", i as u64);
            assert!(p.affects.len() <= MAX_AFFECTS, "槽位上限不可破");
        }
        assert_eq!(p.affects.len(), MAX_AFFECTS);
        assert_eq!(names(&p), vec!["四", "五", "六"], "逐个挤掉最弱者");
    }

    // ---------- tick：衰减 / 消退 / 意图慢衰减 ----------

    #[test]
    fn tick_decays_by_temperament() {
        let mut p = with(Temperament {
            decay: 0.5,
            ..Temperament::default()
        });
        p.feel("忐忑", 0.8, "", 7);
        let out = p.tick(8, 1.0);
        approx(p.affects[0].intensity, 0.4);
        assert!(out.faded.is_empty());
        assert_eq!(out.strongest.as_deref(), Some("忐忑"));
        approx(p.affects[0].history.last().unwrap().intensity, 0.4);

        p.tick(9, 2.0); // elapsed = 2 → ×0.25
        approx(p.affects[0].intensity, 0.1);
        p.tick(10, 1.0); // 恰好落在底线上：仍占槽
        approx(p.affects[0].intensity, AFFECT_FLOOR);
        assert_eq!(p.affects.len(), 1);

        let out = p.tick(11, 1.0); // 跌破底线 → 消退
        assert_eq!(out.faded, vec!["忐忑"]);
        assert!(p.affects.is_empty());
        assert!(out.strongest.is_none());
        assert_eq!(p.last_turn, 11);
    }

    #[test]
    fn tick_elapsed_nonpositive_counts_as_one_round() {
        let mut p = Psyche::default();
        p.feel("忐忑", 0.8, "", 1);
        p.tick(1, 0.0);
        approx(p.affects[0].intensity, 0.8 * (1.0 - DEFAULT_DECAY));
        p.tick(1, -3.0);
        approx(p.affects[0].intensity, 0.8 * (1.0 - DEFAULT_DECAY).powf(2.0));
        p.tick(1, f32::NAN);
        approx(p.affects[0].intensity, 0.8 * (1.0 - DEFAULT_DECAY).powf(3.0));
    }

    #[test]
    fn tick_strongest_tie_breaks_by_name() {
        let mut p = Psyche::default();
        p.feel("甲", 0.5, "", 1);
        p.feel("乙", 0.5, "", 1);
        // 同强度按 name 升序：乙（U+4E59）< 甲（U+7532）
        assert_eq!(p.auto_emotion().as_deref(), Some("乙"));
        let out = p.tick(2, 0.0);
        assert_eq!(out.strongest.as_deref(), Some("乙"));
    }

    #[test]
    fn tick_decays_intents_slower_than_affects() {
        let mut p = Psyche::default();
        p.feel("忐忑", 0.8, "", 1);
        p.intend("想解释", 0.8, 1);
        let out = p.tick(2, 1.0);
        approx(p.affects[0].intensity, 0.8 * (1.0 - DEFAULT_DECAY));
        approx(p.intents[0].strength, 0.8 * (1.0 - INTENT_DECAY_PER_ROUND));
        assert!(p.intents[0].strength > p.affects[0].intensity, "意志比情绪持久");
        assert!(out.faded.is_empty(), "意图衰竭不冒充情绪消退");
        assert_eq!(intent_names(&p), vec!["想解释"], "tick 不移除意图");
        // 再走 10 轮：0.8 × 0.95^11
        p.tick(12, 10.0);
        approx(
            p.intents[0].strength,
            0.8 * (1.0 - INTENT_DECAY_PER_ROUND).powf(11.0),
        );
    }

    #[test]
    fn acceptance_three_turn_afterglow() {
        // M2.5 验收：3 轮前 0.9 的情绪仍有可测余波
        let mut p = Psyche::default();
        p.feel("忐忑", 0.9, "便签", 1);
        p.tick(2, 1.0);
        p.tick(3, 1.0);
        p.tick(4, 1.0);
        let now = p.affects[0].intensity;
        approx(now, 0.9 * (1.0 - DEFAULT_DECAY).powf(3.0));
        assert!(now > 0.5, "余波要可测：{now}");
        assert_eq!(p.auto_emotion().as_deref(), Some("忐忑"));
        assert!(p.summary_line().contains("忐忑0.6"));
    }

    #[test]
    fn acceptance_temperament_changes_rise_and_decay() {
        // 升速：胆汁质（rise 0.8）比粘液质（rise 0.3）叠得快
        let mut hot = with(Temperament::preset("胆汁质"));
        let mut cold = with(Temperament::preset("粘液质"));
        for p in [&mut hot, &mut cold] {
            p.feel("忐忑", 0.4, "", 1);
            p.feel("忐忑", 0.4, "", 1);
        }
        assert!(
            hot.affects[0].intensity > cold.affects[0].intensity,
            "胆汁质升速更快"
        );
        // 衰减：同强度起步，decay 大的余波更弱
        let mut fast = with(Temperament {
            decay: 0.4,
            ..Temperament::default()
        });
        let mut slow = with(Temperament {
            decay: 0.1,
            ..Temperament::default()
        });
        for p in [&mut fast, &mut slow] {
            p.feel("忐忑", 0.9, "", 1);
            for turn in 2..5 {
                p.tick(turn, 1.0);
            }
        }
        assert!(
            slow.affects[0].intensity > fast.affects[0].intensity,
            "decay 小的余波更持久"
        );
    }

    // ---------- intend / bind_thread ----------

    #[test]
    fn intend_adds_clamps_and_removes() {
        let mut p = Psyche::default();
        let it = p.intend("惦记着坦白", 0.4, 3).expect("新意图");
        approx(it.strength, 0.4);
        assert_eq!(it.since_turn, 3);
        assert_eq!(it.linked_thread, None);

        approx(p.intend("惦记着坦白", 0.3, 4).unwrap().strength, 0.7);
        approx(p.intend("惦记着坦白", 5.0, 5).unwrap().strength, 1.0);
        assert_eq!(p.intents[0].since_turn, 3, "起始轮不变");
        assert_eq!(intent_names(&p), vec!["惦记着坦白"], "同名只有一条");

        approx(p.intend("惦记着坦白", -0.95, 6).unwrap().strength, 0.05);
        assert!(p.intend("惦记着坦白", -0.1, 7).is_none(), "≤0 即移除");
        assert!(p.intents.is_empty());
        assert_eq!(p.last_turn, 7);
    }

    #[test]
    fn intend_negative_without_existing_is_none() {
        let mut p = Psyche::default();
        assert!(p.intend("不存在", -0.2, 1).is_none());
        assert!(p.intend("   ", 0.5, 1).is_none());
        assert!(p.intend("想解释", f32::NAN, 1).is_none(), "NaN 增量不建意图");
        assert!(p.intend("想解释", 0.0, 2).is_none(), "0 增量不建意图");
        assert!(p.intents.is_empty());
    }

    #[test]
    fn bind_thread_links_and_reports_missing() {
        let mut p = Psyche::default();
        p.intend("想解释", 0.7, 1);
        assert!(p.bind_thread("想解释", "thread.周五还书"));
        assert_eq!(p.intents[0].linked_thread.as_deref(), Some("thread.周五还书"));
        assert!(p.bind_thread("想解释", "thread.另一条"), "改绑成立");
        assert_eq!(p.intents[0].linked_thread.as_deref(), Some("thread.另一条"));
        assert!(!p.bind_thread("没这个意图", "thread.x"));
        assert!(!p.bind_thread("想解释", "   "), "空线 id 不绑定");
        assert_eq!(p.intents[0].linked_thread.as_deref(), Some("thread.另一条"));
        approx(p.intents[0].strength, 0.7);
        assert_eq!(p.intent("想解释").map(|i| i.strength), Some(0.7));
        assert!(p.affect("想解释").is_none());
    }

    // ---------- auto_emotion ----------

    #[test]
    fn auto_emotion_picks_strongest() {
        let mut p = Psyche::default();
        assert_eq!(p.auto_emotion(), None);
        p.feel("害羞", 0.4, "", 1);
        p.feel("忐忑", 0.7, "", 1);
        assert_eq!(p.auto_emotion().as_deref(), Some("忐忑"));
        assert_eq!(p.strongest_affect().map(|a| a.name.as_str()), Some("忐忑"));
        // 强度反超后换人
        p.feel("害羞", 0.9, "", 2);
        assert_eq!(p.auto_emotion().as_deref(), Some("害羞"));
    }

    // ---------- wants_to_act ----------

    #[test]
    fn wants_to_act_takes_threshold_minus_impulsiveness() {
        // 默认：0.6 − 0.1×0.3 − 0.05 = 0.52
        let mut p = Psyche::default();
        p.intend("想解释", 0.55, 1);
        assert_eq!(p.wants_to_act(), vec!["想解释"]);
        p.intend("想解释", -0.04, 2); // 0.51 < 0.52
        assert!(p.wants_to_act().is_empty());

        // 抑郁质：0.65 − 0.01 − 0.05 = 0.59 → 不触发
        let mut quiet = with(Temperament::preset("抑郁质"));
        quiet.intend("想解释", 0.55, 1);
        assert!(quiet.wants_to_act().is_empty());

        // 胆汁质：0.45 − 0.075 − 0.05 = 0.325 → 触发
        let mut hot = with(Temperament::preset("胆汁质"));
        hot.intend("想解释", 0.4, 1);
        assert_eq!(hot.wants_to_act(), vec!["想解释"]);
    }

    #[test]
    fn wants_to_act_sorted_by_strength_then_name() {
        let mut p = Psyche::default();
        p.intend("甲", 0.6, 1);
        p.intend("乙", 0.8, 1);
        p.intend("丙", 0.6, 1);
        // 强度降序；0.6 同分按名字升序（丙 U+4E19 < 甲 U+7532）
        assert_eq!(p.wants_to_act(), vec!["乙", "丙", "甲"]);
        // 名次随衰减变化，但规则不变
        p.intend("乙", -0.25, 2); // 0.55 < 0.6
        assert_eq!(p.wants_to_act(), vec!["丙", "甲", "乙"]);
    }

    // ---------- summary_line ----------

    #[test]
    fn summary_line_empty_when_nothing() {
        let p = Psyche::default();
        assert_eq!(p.summary_line(), "");
        assert_eq!(p.summary_line_for("小雨"), "", "空心理不占 B5 层");
    }

    #[test]
    fn summary_line_affects_and_intents_only() {
        let mut p = Psyche::default();
        p.feel("忐忑", 0.6, "", 1);
        assert_eq!(p.summary_line(), "【内心】忐忑0.6");

        // 情绪消退后只剩意图
        let mut only_intent = Psyche::default();
        only_intent.feel("微尘", 0.06, "", 1);
        only_intent.tick(2, 1.0); // 0.051：还在
        only_intent.tick(3, 1.0); // 0.043：消退
        assert!(only_intent.affects.is_empty(), "0.06 两轮后跌破底线");
        only_intent.intend("惦记着坦白", 0.4, 3);
        assert_eq!(only_intent.summary_line(), "【内心】惦记着坦白(0.4)");
    }

    #[test]
    fn summary_line_marker_and_pressure() {
        let mut p = Psyche::default(); // threshold 0.6
        p.feel("害羞", 0.7, "", 1);
        p.feel("忐忑", 0.6, "", 1);
        p.intend("惦记着坦白", 0.4, 1);
        p.intend("想解释", 0.8, 1);
        assert_eq!(
            p.summary_line_for("小雨"),
            "【小雨·内心】害羞0.7 忐忑0.6 ▸ 想解释(0.8,被害羞压着)! 惦记着坦白(0.4,被害羞压着)"
        );
        assert_eq!(
            p.summary_line(),
            "【内心】害羞0.7 忐忑0.6 ▸ 想解释(0.8,被害羞压着)! 惦记着坦白(0.4,被害羞压着)"
        );
        // 情绪全部低于 0.5 → 不再「压着」，但阈值标记照旧
        let mut free = Psyche::default();
        free.feel("平静", 0.3, "", 1);
        free.intend("想解释", 0.8, 1);
        assert_eq!(free.summary_line(), "【内心】平静0.3 ▸ 想解释(0.8)!");
    }

    #[test]
    fn summary_line_ignores_sub_floor_noise() {
        let mut p = Psyche::default();
        p.feel("微尘", 0.02, "", 1);
        p.intend("一闪念", 0.03, 1);
        assert_eq!(p.summary_line(), "", "噪声不进 B5");
    }

    // ---------- decay_trail ----------

    #[test]
    fn decay_trail_lists_samples_in_order() {
        let mut p = Psyche::default();
        p.feel("害羞", 0.6, "被看穿", 1);
        p.feel("忐忑", 0.8, "便签", 1);
        p.tick(2, 1.0);
        p.tick(3, 1.0);
        let trail = p.decay_trail();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].0, "忐忑", "按当前强度降序");
        assert_eq!(trail[1].0, "害羞");
        let ticks = &trail[0].1;
        assert_eq!(
            ticks.iter().map(|t| t.turn).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "feel 与每轮 tick 都采样"
        );
        approx(ticks[0].intensity, 0.8);
        approx(ticks[2].intensity, 0.8 * (1.0 - DEFAULT_DECAY).powf(2.0));
        assert!(ticks.windows(2).all(|w| w[1].intensity < w[0].intensity));
        // 空态
        assert!(Psyche::default().decay_trail().is_empty());
    }

    #[test]
    fn decay_trail_history_is_capped() {
        let mut p = Psyche::default();
        for turn in 1..=40u64 {
            p.feel("忐忑", 0.3, "", turn);
        }
        let trail = p.decay_trail();
        assert_eq!(trail[0].1.len(), MAX_HISTORY_SAMPLES, "采样有上限，state 不膨胀");
        assert_eq!(trail[0].1.first().unwrap().turn, 9, "丢最旧的");
        assert_eq!(trail[0].1.last().unwrap().turn, 40);
    }

    // ---------- need_hit ----------

    #[test]
    fn need_hit_matches_substring_case_insensitively() {
        let needs = vec!["被理解".to_string(), "安全感".to_string(), "Trust".to_string()];
        assert_eq!(
            need_hit(&needs, "这一次，她选择了安全感而不是被认可"),
            vec!["安全感"]
        );
        assert_eq!(
            need_hit(&needs, "她终于愿意被理解了，也有了安全感"),
            vec!["被理解", "安全感"],
            "按 needs 顺序返回"
        );
        assert_eq!(need_hit(&needs, "i trust you"), vec!["Trust"]);
        assert_eq!(need_hit(&needs, "毫不相干的一句话"), Vec::<String>::new());
    }

    #[test]
    fn need_hit_skips_empty_and_preserves_order() {
        let needs = vec![
            "".to_string(),
            "   ".to_string(),
            "被认可".to_string(),
            "被理解".to_string(),
            "被认可".to_string(),
        ];
        // 空需要不能命中一切；同名去重
        assert_eq!(
            need_hit(&needs, "任何文本都会被认可与被理解"),
            vec!["被认可", "被理解"]
        );
        assert_eq!(need_hit(&needs, ""), Vec::<String>::new());
        assert_eq!(need_hit(&needs, "   "), Vec::<String>::new());
        assert_eq!(need_hit(&[], "随便"), Vec::<String>::new());
    }

    // ---------- 气质参数 ----------

    #[test]
    fn temperament_presets_differ_and_unknown_falls_back() {
        assert_eq!(Temperament::preset("不存在的类型"), Temperament::default());
        assert_eq!(Temperament::preset("   "), Temperament::default());
        assert_eq!(
            Temperament::preset(" 胆汁质 "),
            Temperament::preset("choleric")
        );
        assert!(Temperament::preset("胆汁质").rise > Temperament::preset("粘液质").rise);
        assert!(
            Temperament::preset("胆汁质").impulsiveness
                > Temperament::preset("抑郁质").impulsiveness
        );
        assert!(Temperament::preset("抑郁质").decay < Temperament::preset("多血质").decay);
        assert!(Temperament::preset("抑郁质").threshold > Temperament::preset("胆汁质").threshold);
        for name in TEMPERAMENT_PRESETS {
            let t = Temperament::preset(name);
            assert_ne!(t, Temperament::default(), "{name} 不应等于默认");
            for v in [t.rise, t.decay, t.threshold, t.impulsiveness] {
                assert!((0.0..=1.0).contains(&v), "{name} 参数越界：{v}");
            }
        }
    }

    #[test]
    fn temperament_from_value_defaults_and_clamps() {
        assert_eq!(Temperament::from_value(&json!(null)), Temperament::default());
        assert_eq!(Temperament::from_value(&json!({})), Temperament::default());
        let t = Temperament::from_value(&json!({"rise": 0.8}));
        approx(t.rise, 0.8);
        approx(t.decay, DEFAULT_DECAY);
        approx(t.threshold, DEFAULT_THRESHOLD);
        approx(t.impulsiveness, DEFAULT_IMPULSIVENESS);

        let t = Temperament::from_value(&json!({
            "rise": "0.9", "decay": 3.0, "threshold": -1.0, "impulsiveness": "x"
        }));
        approx(t.rise, 0.9);
        approx(t.decay, 1.0);
        approx(t.threshold, 0.0);
        approx(t.impulsiveness, DEFAULT_IMPULSIVENESS);
    }

    // ---------- 确定性 ----------

    #[test]
    fn same_input_same_output() {
        let a = run_script();
        let b = run_script();
        assert_eq!(a, b);
        assert_eq!(a.summary_line(), b.summary_line());
        assert_eq!(a.decay_trail(), b.decay_trail());
        assert_eq!(a.wants_to_act(), b.wants_to_act());
        assert_eq!(a.auto_emotion(), b.auto_emotion());
        assert_eq!(a.strongest_affect(), b.strongest_affect());

        let mut sa = json!({});
        let mut sb = json!({});
        a.write_into(&mut sa);
        b.write_into(&mut sb);
        assert_eq!(sa, sb, "同输入写出的 state 逐字节一致");

        // 再跑一段含替换与消退的路径：仍然稳定
        let mut c = run_script();
        for turn in 4..12 {
            c.feel("期待", 0.6, "约定", turn);
            c.feel("新情绪", 0.4, "", turn);
            c.tick(turn, 1.0);
        }
        let mut d = run_script();
        for turn in 4..12 {
            d.feel("期待", 0.6, "约定", turn);
            d.feel("新情绪", 0.4, "", turn);
            d.tick(turn, 1.0);
        }
        assert_eq!(c, d);
        assert_eq!(c.summary_line(), d.summary_line());
    }
}
