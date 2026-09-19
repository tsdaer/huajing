//! 运行时诊断：把宿主侧的关键决策记成环形缓冲，供界面「运行环境」直接查看。
//!
//! 为什么需要它：本应用是本地明文程序，用户看不到 stdout（安装版根本没有控制台），
//! 而「钩子到底跑没跑、state 改了没、卡是从哪个路径加载的」这类问题靠猜代价极高。
//! 记一条的代价是一次加锁 + 一次 push，只保留最近 N 条，长期开着也不心疼。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// 环形缓冲上限（够用即可，超出丢最旧的）
const CAPACITY: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct DiagRecord {
    /// 事件类型：hook / import / watch / error
    pub kind: String,
    /// 一句话摘要（不含用户消息正文，避免把剧情写进诊断）
    pub detail: String,
    /// 发生时刻（unix 秒）
    pub ts: u64,
}

fn buffer() -> &'static Mutex<VecDeque<DiagRecord>> {
    static BUF: OnceLock<Mutex<VecDeque<DiagRecord>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// 记一条（锁 poisoned 时静默丢弃：诊断本身绝不能影响主流程）
pub fn record(kind: &str, detail: impl Into<String>) {
    let Ok(mut buf) = buffer().lock() else { return };
    if buf.len() >= CAPACITY {
        buf.pop_front();
    }
    buf.push_back(DiagRecord {
        kind: kind.to_string(),
        detail: detail.into(),
        ts: crate::store::unix_now(),
    });
}

/// 最近的诊断记录（新的在前）
pub fn recent(limit: usize) -> Vec<DiagRecord> {
    let Ok(buf) = buffer().lock() else {
        return Vec::new();
    };
    buf.iter().rev().take(limit).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_newest_first_and_caps_capacity() {
        for i in 0..(CAPACITY + 5) {
            record("hook", format!("第 {i} 条"));
        }
        let newest = recent(3);
        assert_eq!(newest.len(), 3);
        assert_eq!(newest[0].detail, format!("第 {} 条", CAPACITY + 4));
        assert_eq!(newest[2].detail, format!("第 {} 条", CAPACITY + 2));
        assert_eq!(recent(0).len(), 0);
    }
}
