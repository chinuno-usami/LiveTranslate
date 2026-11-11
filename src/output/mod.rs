//! 统一输出层：桌面浮窗由 Tauri 前端渲染，这里负责 SteamVR 与 OSC。
//!
//! 所有 sink 都是非阻塞的（`try_send` / 覆盖最新快照），输出侧出问题
//! 不会拖慢识别与翻译流水线。

use crate::config::OutputConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub mod osc;
pub mod steamvr;

pub use osc::OscStatus;
pub use steamvr::SteamVrStatus;

#[derive(Debug, Clone, Serialize)]
pub struct OutputStatusPayload {
    pub steamvr: SteamVrStatus,
    pub osc: OscStatus,
}

/// 编译期能力（用于设置页置灰 / 隐藏入口）
pub fn capabilities() -> (bool, bool) {
    (
        cfg!(all(feature = "steamvr", target_os = "windows")),
        cfg!(feature = "osc"),
    )
}

pub struct OutputManager {
    steamvr: steamvr::SteamVrSink,
    osc: osc::OscSink,
    /// 采集是否在运行；停止后 OSC typing 会关闭
    capture_active: AtomicBool,
    /// 后台输出线程状态变化通知（无锁唤醒前端事件推送）
    status_notify: Arc<tokio::sync::Notify>,
}

impl OutputManager {
    pub fn new(config: &OutputConfig) -> Arc<Self> {
        let status_notify = Arc::new(tokio::sync::Notify::new());
        Arc::new(Self {
            steamvr: steamvr::SteamVrSink::new(&config.steamvr, status_notify.clone()),
            osc: osc::OscSink::new(&config.osc, status_notify.clone()),
            capture_active: AtomicBool::new(false),
            status_notify,
        })
    }

    pub async fn status_notified(&self) {
        self.status_notify.notified().await;
    }

    pub fn notify_status(&self) {
        self.status_notify.notify_one();
    }

    /// 发布一份完整字幕快照（按时间从旧到新）
    pub fn publish(&self, snapshot: &[DisplayLine]) {
        // 停止后迟到的快照不要再进入输出，否则重新开启时会立刻发出旧内容
        if !self.capture_active.load(Ordering::SeqCst) {
            return;
        }
        self.steamvr.publish(snapshot);
        self.osc.publish(snapshot);
    }

    /// 设置热更新（旧 VR 线程若有停止等待，放到 blocking 池回收以免阻塞异步锁持有者）
    pub async fn apply_config(&self, config: &OutputConfig) {
        let handle = self.steamvr.apply_config_detached(&config.steamvr);
        self.osc.apply_config(&config.osc);
        self.status_notify.notify_one();
        if let Some(handle) = handle {
            let _ =
                tokio::task::spawn_blocking(move || steamvr::join_bounded_default(handle)).await;
        }
    }

    pub fn set_capture_active(&self, active: bool) {
        self.capture_active.store(active, Ordering::SeqCst);
        self.osc.set_active(active);
        if !active {
            // 停止时清空 SteamVR 面板
            self.steamvr.publish(&[]);
        }
    }

    pub fn status(&self) -> OutputStatusPayload {
        OutputStatusPayload {
            steamvr: self.steamvr.status(),
            osc: self.osc.status(),
        }
    }

    pub fn shutdown(&self) {
        self.steamvr.shutdown();
        self.osc.shutdown();
    }

    pub async fn shutdown_async(self: &Arc<Self>) {
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.shutdown()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 风味本身是否真的被编进去（CI 靠它把“双构建”钉住，而不是只 grep 依赖树）
    #[test]
    fn capabilities_follow_features() {
        let (steamvr, osc) = capabilities();
        assert_eq!(osc, cfg!(feature = "osc"));
        assert_eq!(
            steamvr,
            cfg!(all(feature = "steamvr", target_os = "windows"))
        );
    }
}
