//! SteamVR Overlay 输出（仅 Windows + `steamvr` feature 有真实实现）。
//!
//! OpenVR 的 `IVROverlay` 有线程亲和性，因此真实实现把 init / 渲染 / 上传
//! 全部放在一个专用 std 线程里，tokio 侧只做 `try_send`，永不阻塞流水线。

use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use serde::Serialize;
use std::sync::Mutex;

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
        if let Ok(mut inner) = self.inner.lock() {
            inner.apply_config(config);
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
        if let Ok(mut inner) = self.inner.lock() {
            inner.shutdown();
        }
    }
}

#[cfg(feature = "steamvr")]
pub mod render;
