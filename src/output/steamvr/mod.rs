//! SteamVR Overlay 输出（仅 Windows + `steamvr` feature 有真实实现）。
//!
//! OpenVR 的 `IVROverlay` 有线程亲和性，因此真实实现把 init / 渲染 / 上传
//! 全部放在一个专用 std 线程里，tokio 侧只做 `try_send`，永不阻塞流水线。

use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 停止 VR 线程时的最长等待
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) fn join_bounded_default(handle: JoinHandle<()>) {
    join_bounded(handle, STOP_TIMEOUT);
}

/// 有上限地等待 VR 线程退出；超时则放弃（detach），不让退出流程挂死。
///
/// `openvr::init()` 在 SteamVR 正在启动或卡住时没有超时语义，
/// 所以这里不能直接 `join()`。
fn join_bounded(handle: JoinHandle<()>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            tracing::warn!("SteamVR overlay thread did not stop in {timeout:?}; detaching it");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.join();
}

#[derive(Debug, Clone, Serialize)]
pub struct SteamVrStatus {
    pub available: bool,
    pub enabled: bool,
    pub connected: bool,
    pub message: String,
}

#[allow(clippy::derivable_impls)]
impl Default for SteamVrStatus {
    fn default() -> Self {
        Self {
            available: cfg!(all(feature = "steamvr", target_os = "windows")),
            enabled: false,
            connected: false,
            message: String::new(),
        }
    }
}

#[cfg(all(feature = "steamvr", target_os = "windows"))]
#[path = "imp.rs"]
mod imp;

#[cfg(not(all(feature = "steamvr", target_os = "windows")))]
#[path = "stub.rs"]
mod imp;

pub struct SteamVrSink {
    lifecycle: Mutex<()>,
    inner: Mutex<imp::SteamVrSink>,
}

impl SteamVrSink {
    pub fn new(config: &SteamVrConfig, notify: Arc<tokio::sync::Notify>) -> Self {
        Self {
            lifecycle: Mutex::new(()),
            inner: Mutex::new(imp::SteamVrSink::new(config, notify)),
        }
    }

    /// 更新配置并返回需要回收的旧线程句柄（供异步调用方放入 `spawn_blocking` 等待，避免阻塞 Tokio 工作线程）
    pub fn apply_config_detached(&self, config: &SteamVrConfig) -> Option<JoinHandle<()>> {
        let _lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.apply_config(config)
    }

    /// 传入完整字幕快照；内部只保留最新一帧
    pub fn publish(&self, lines: &[DisplayLine]) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.publish(lines);
    }

    pub fn status(&self) -> SteamVrStatus {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status()
    }

    pub fn shutdown(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        let handle = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.shutdown()
        };
        if let Some(handle) = handle {
            join_bounded(handle, STOP_TIMEOUT);
        }
    }
}

#[cfg(feature = "steamvr")]
pub mod render;
