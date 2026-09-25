//! DataHub 热加载（M1.7 · 设计 §3「改 card.lua 保存即生效」）。
//!
//! 监听范围（M1）：
//! - `characters/`（含子目录，递归）：卡与随卡资产；
//! - `settings.toml` / `personas/`：界面与人格配置。
//!
//! 解析本身不需要缓存——每轮对话都从磁盘重读 card.lua——所以热加载要做的是
//! 「通知」而不是「重载」：把变更推给前端，让卡片清单、会话头署名与卡内状态
//! 面板立刻跟上。
//!
//! 事件队列是「去重且只保留最新」的：连续保存（编辑器常见的写临时文件再改名）
//! 在 300ms 窗口里合并成一次推送，避免前端被刷新风暴打爆。

use std::collections::VecDeque;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// 变更事件合并窗口：窗口内的多次保存只推一次
const DEBOUNCE: Duration = Duration::from_millis(300);

/// 推给前端的卡片变更事件
#[derive(Debug, Clone, Serialize)]
pub struct CardChanged {
    /// 命中的角色目录名（`characters/<dir_name>`）；非卡目录变更为 None
    pub dir_name: Option<String>,
    /// 变更的绝对路径（便于日志与排错）
    pub path: String,
}

/// 热加载状态（Tauri State）：持有着 watcher，drop 即停止监听
#[derive(Default)]
pub struct CardWatch(Mutex<Option<WatcherHandle>>);

struct WatcherHandle {
    _watcher: notify::RecommendedWatcher,
    /// pusher 线程的终止信号（加固 E3）：unwatch 置位后线程退出，
    /// 不再持着 `Arc<Queue>` 死循环泄漏
    stop: Arc<AtomicBool>,
}

/// 事件队列（去重、保序、只留最新队列头）；watcher 线程与推送线程之间共享
#[derive(Default)]
struct Queue {
    items: Mutex<VecDeque<CardChanged>>,
    signal: Condvar,
}

impl Queue {
    fn push(&self, item: CardChanged) {
        if let Ok(mut items) = self.items.lock() {
            // 去重：同一路径已在队列里就只保留最新一次
            items.retain(|it| it.path != item.path);
            items.push_back(item);
            self.signal.notify_all();
        }
    }

    /// 等一条变更；超时返回 None（调用方据此决定是否继续等）
    fn pop(&self, timeout: Duration) -> Option<CardChanged> {
        let mut items = self.items.lock().ok()?;
        if items.is_empty() {
            let (guard, _) = self
                .signal
                .wait_timeout(items, timeout)
                .ok()?;
            items = guard;
        }
        items.pop_front()
    }
}

/// 变更路径是否在监听范围内，以及它属于哪个角色目录
fn classify(root: &Path, path: &Path) -> Option<CardChanged> {
    let rel = path.strip_prefix(root).ok()?;
    let mut comps = rel.components();
    let head = comps.next()?.as_os_str().to_string_lossy().into_owned();
    let dir_name = match head.as_str() {
        // characters/<角色>/… ：卡与随卡资产
        "characters" => comps.next().map(|c| c.as_os_str().to_string_lossy().into_owned()),
        // 人格与全局配置：无角色归属
        "personas" => None,
        _ => {
            let is_settings = rel == Path::new("settings.toml");
            if !is_settings {
                return None; // sessions/ 等运行时数据不触发（写入频繁且与界面无关）
            }
            None
        }
    };
    Some(CardChanged {
        dir_name,
        path: path.to_string_lossy().into_owned(),
    })
}

/// 启动监听（幂等：已在监听则直接返回）
#[tauri::command]
pub fn watch_cards(app: AppHandle, state: State<'_, CardWatch>) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "热加载状态锁 poisoned".to_string())?;
    if slot.is_some() {
        return Ok(());
    }
    let root = crate::store::data_root();
    let queue = Arc::new(Queue::default());
    let watcher = spawn_watcher(&root, Arc::clone(&queue))?;
    let stop = Arc::new(AtomicBool::new(false));
    spawn_pusher(app, Arc::clone(&queue), Arc::clone(&stop));
    *slot = Some(WatcherHandle {
        _watcher: watcher,
        stop,
    });
    Ok(())
}

/// 停止监听（幂等）：置终止信号让 pusher 线程退出（加固 E3），drop watcher 停底层监听
#[tauri::command]
pub fn unwatch_cards(state: State<'_, CardWatch>) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "热加载状态锁 poisoned".to_string())?;
    if let Some(handle) = slot.as_ref() {
        handle.stop.store(true, Ordering::Relaxed);
    }
    *slot = None;
    Ok(())
}

fn spawn_watcher(root: &Path, queue: Arc<Queue>) -> Result<notify::RecommendedWatcher, String> {
    // 目录不存在时先建骨架，否则 watcher 无法注册（首次启动、尚未落盘的用户）
    let _ = crate::store::ensure_layout(root);
    let watched_root = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if !matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_)
        ) {
            return;
        }
        for path in event.paths {
            if let Some(item) = classify(&watched_root, &path) {
                queue.push(item);
            }
        }
    })
    .map_err(|e| format!("文件监听初始化失败：{e}"))?;

    for target in [root.join("characters"), root.join("personas")] {
        if target.is_dir() {
            watcher
                .watch(&target, RecursiveMode::Recursive)
                .map_err(|e| format!("监听 {} 失败：{e}", target.display()))?;
        }
    }
    let settings = root.join("settings.toml");
    if settings.exists() {
        let _ = watcher.watch(&settings, RecursiveMode::NonRecursive);
    }
    Ok(watcher)
}

fn spawn_pusher(app: AppHandle, queue: Arc<Queue>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        pusher_loop(queue, stop, move |item| {
            let _ = app.emit("card_changed", &item);
        })
    });
}

/// 推送循环（加固 E3）：stop 置位后尽快退出，不再持队列死循环。
/// `emit` 是推送出口（生产 = Tauri 事件；测试 = 计数闭包），抽出便于钉终止行为。
fn pusher_loop<E: FnMut(CardChanged)>(queue: Arc<Queue>, stop: Arc<AtomicBool>, mut emit: E) {
    while !stop.load(Ordering::Relaxed) {
        // 阻塞等第一条（超时即回到终止检查）
        let Some(first) = queue.pop(DEBOUNCE) else {
            continue;
        };
        let mut batch = vec![first];
        // 合并窗口内的后续变更：一次保存动作往往产生多条事件
        let deadline = std::time::Instant::now() + DEBOUNCE;
        while std::time::Instant::now() < deadline {
            match queue.pop(deadline.saturating_duration_since(std::time::Instant::now())) {
                Some(next) => batch.push(next),
                None => break,
            }
        }
        for item in batch {
            emit(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/data")
    }

    #[test]
    fn classify_character_card_and_assets() {
        let hit = classify(&root(), Path::new("/data/characters/小雨/card.lua")).unwrap();
        assert_eq!(hit.dir_name.as_deref(), Some("小雨"));
        // 随卡资产（立绘等）也算这张卡变了
        let asset = classify(&root(), Path::new("/data/characters/小雨/assets/a.png")).unwrap();
        assert_eq!(asset.dir_name.as_deref(), Some("小雨"));
    }

    #[test]
    fn classify_personas_and_settings_without_card() {
        let p = classify(&root(), Path::new("/data/personas/default.toml")).unwrap();
        assert!(p.dir_name.is_none());
        let s = classify(&root(), Path::new("/data/settings.toml")).unwrap();
        assert!(s.dir_name.is_none());
    }

    #[test]
    fn classify_ignores_runtime_data_and_outside_paths() {
        // 会话运行时数据每轮都在写：不能触发前端刷新
        assert!(classify(&root(), Path::new("/data/sessions/s1/messages.jsonl")).is_none());
        assert!(classify(&root(), Path::new("/data/codex/default/world.json")).is_none());
        assert!(classify(&root(), Path::new("/elsewhere/card.lua")).is_none());
    }

    #[test]
    fn queue_dedupes_by_path_and_keeps_latest() {
        let q = Queue::default();
        q.push(CardChanged {
            dir_name: Some("a".into()),
            path: "/d/characters/a/card.lua".into(),
        });
        q.push(CardChanged {
            dir_name: Some("b".into()),
            path: "/d/characters/b/card.lua".into(),
        });
        // 同一路径再次保存：只留最新一份
        q.push(CardChanged {
            dir_name: Some("a".into()),
            path: "/d/characters/a/card.lua".into(),
        });
        let first = q.pop(Duration::from_millis(0)).unwrap();
        let second = q.pop(Duration::from_millis(0)).unwrap();
        assert_eq!(first.dir_name.as_deref(), Some("b"));
        assert_eq!(second.dir_name.as_deref(), Some("a"));
        assert!(q.pop(Duration::from_millis(0)).is_none());
    }

    #[test]
    fn pusher_exits_after_stop_signal() {
        // E3：终止信号置位后 pusher 循环必须退出——unwatch 不再泄漏线程
        let q = Arc::new(Queue::default());
        let stop = Arc::new(AtomicBool::new(false));
        q.push(CardChanged {
            dir_name: Some("a".into()),
            path: "/d/characters/a/card.lua".into(),
        });
        let stop_in_emit = Arc::clone(&stop);
        let seen = Arc::new(Mutex::new(0usize));
        let seen_in_emit = Arc::clone(&seen);
        pusher_loop(Arc::clone(&q), Arc::clone(&stop), move |item| {
            assert_eq!(item.dir_name.as_deref(), Some("a"));
            *seen_in_emit.lock().unwrap() += 1;
            // 推完第一条就停：循环必须就此返回，而不是吞掉后续继续死等
            stop_in_emit.store(true, Ordering::Relaxed);
        });
        assert_eq!(*seen.lock().unwrap(), 1);

        // 预先停止：一条都不处理，立即返回
        stop.store(true, Ordering::Relaxed);
        q.push(CardChanged {
            dir_name: Some("b".into()),
            path: "/d/characters/b/card.lua".into(),
        });
        pusher_loop(q, stop, |_| panic!("停止后不得再推送"));
    }
}
