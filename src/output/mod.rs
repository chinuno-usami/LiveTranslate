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
}

impl OutputManager {
    pub fn new(config: &OutputConfig) -> Arc<Self> {
        Arc::new(Self {
            steamvr: steamvr::SteamVrSink::new(&config.steamvr),
            osc: osc::OscSink::new(&config.osc),
            capture_active: AtomicBool::new(false),
        })
    }

    /// 发布一份完整字幕快照（按时间从旧到新）
    pub fn publish(&self, snapshot: &[DisplayLine]) {
        self.steamvr.publish(snapshot);
        self.osc.publish(snapshot);
    }

    /// 设置热更新
    pub fn apply_config(&self, config: &OutputConfig) {
        self.steamvr.apply_config(&config.steamvr);
        self.osc.apply_config(&config.osc);
    }

    pub fn set_capture_active(&self, active: bool) {
        self.capture_active.store(active, Ordering::SeqCst);
        if !active {
            self.osc.set_active(false);
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
}
