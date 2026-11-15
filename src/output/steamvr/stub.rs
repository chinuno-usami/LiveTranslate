use super::{SteamVrPoseUpdate, SteamVrStatus};
use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use std::sync::Arc;
use std::thread::JoinHandle;

pub struct SteamVrSink {
    status: SteamVrStatus,
    notify: Arc<tokio::sync::Notify>,
}

impl SteamVrSink {
    pub fn new(config: &SteamVrConfig, notify: Arc<tokio::sync::Notify>) -> Self {
        let message = if cfg!(target_os = "windows") {
            "当前构建未包含 SteamVR 支持（请下载 Full 版）".to_string()
        } else {
            "SteamVR 仅支持 Windows".to_string()
        };
        Self {
            status: SteamVrStatus {
                available: false,
                enabled: config.enabled,
                connected: false,
                message,
                gesture_locked: config.gesture.locked,
                dragging: false,
                // 3C：不屏蔽游戏输入；非 Windows / 标准版无运行时
                input_blocked: false,
                input_block_message: String::new(),
            },
            notify,
        }
    }

    pub fn apply_config(&mut self, config: &SteamVrConfig) -> Option<JoinHandle<()>> {
        let mut changed = false;
        if self.status.enabled != config.enabled {
            self.status.enabled = config.enabled;
            changed = true;
        }
        if self.status.gesture_locked != config.gesture.locked {
            self.status.gesture_locked = config.gesture.locked;
            changed = true;
        }
        // 3C：input_blocked 恒 false
        if self.status.input_blocked {
            self.status.input_blocked = false;
            changed = true;
        }
        if changed {
            self.notify.notify_one();
        }
        None
    }

    pub fn publish(&self, _lines: &[DisplayLine]) {}

    pub fn status(&self) -> SteamVrStatus {
        self.status.clone()
    }

    pub fn take_pose_update(&self) -> Option<SteamVrPoseUpdate> {
        None
    }

    pub fn set_gesture_locked(&mut self, locked: bool) {
        if self.status.gesture_locked != locked {
            self.status.gesture_locked = locked;
            self.status.dragging = false;
            self.notify.notify_one();
        }
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        None
    }
}
