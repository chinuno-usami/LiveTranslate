use super::{InputBlockStatus, SteamVrPoseUpdate, SteamVrStatus};
use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use std::path::PathBuf;
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
        let input_block = InputBlockStatus::unsupported_build();
        let input_block_message = if config.gesture.block_game_input {
            input_block.experimental_message.clone()
        } else {
            String::new()
        };
        Self {
            status: SteamVrStatus {
                available: false,
                enabled: config.enabled,
                connected: false,
                message,
                gesture_locked: config.gesture.locked,
                dragging: false,
                // 非 Windows / 标准版：永不声称已屏蔽
                input_blocked: false,
                input_block_message,
                input_block,
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
        // 非 Windows / 标准版：input_blocked 恒 false；状态只提示不支持
        if self.status.input_blocked {
            self.status.input_blocked = false;
            changed = true;
        }
        let expected_msg = if config.gesture.block_game_input {
            self.status.input_block.experimental_message.clone()
        } else {
            String::new()
        };
        if self.status.input_block_message != expected_msg {
            self.status.input_block_message = expected_msg;
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

    /// 非 Windows / 标准版没有 VR 手势，永远不会请求解锁。
    pub fn take_unlock_request(&self) -> bool {
        false
    }

    pub fn set_gesture_locked(&mut self, locked: bool) {
        if self.status.gesture_locked != locked {
            self.status.gesture_locked = locked;
            self.status.dragging = false;
            self.notify.notify_one();
        }
    }

    pub fn set_active_config_path(&self, _path: Option<PathBuf>) {}

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        None
    }
}
