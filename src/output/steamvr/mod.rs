//! SteamVR Overlay 输出（仅 Windows + `steamvr` feature 有真实实现）。
//!
//! OpenVR 的 `IVROverlay` 有线程亲和性，因此真实实现把 init / 渲染 / 上传
//! 全部放在一个专用 std 线程里，tokio 侧只做 `try_send`，永不阻塞流水线。

use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 停止 VR 线程时的最长等待
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

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
    inner: Mutex<imp::SteamVrSink>,
}

impl SteamVrSink {
    pub fn new(config: &SteamVrConfig) -> Self {
        Self {
            inner: Mutex::new(imp::SteamVrSink::new(config)),
        }
    }

    pub fn apply_config(&self, config: &SteamVrConfig) {
        // 先取走线程句柄再释放外层锁，否则 join 期间 publish/shutdown 都会阻塞
        let handle = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.apply_config(config)
        };
        if let Some(handle) = handle {
            join_bounded(handle, STOP_TIMEOUT);
        }
    }

    /// 传入完整字幕快照；内部只保留最新一帧
    pub fn publish(&self, lines: &[DisplayLine]) {
        if let Ok(inner) = self.inner.lock() {
            inner.publish(lines);
        }
    }

    pub fn status(&self) -> SteamVrStatus {
        self.inner
            .lock()
            .map(|inner| inner.status())
            .unwrap_or_default()
    }

    pub fn shutdown(&self) {
        let handle = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.shutdown()
        };
        if let Some(handle) = handle {
            join_bounded(handle, STOP_TIMEOUT);
        }
    }
}

#[cfg(feature = "steamvr")]
pub mod render;
