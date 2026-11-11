use super::SteamVrStatus;
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
        let message = if cfg!(all(feature = "steamvr", target_os = "windows")) {
            "SteamVR 输出不可用".to_string()
        } else if cfg!(target_os = "windows") {
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
            },
            notify,
        }
    }

    pub fn apply_config(&mut self, config: &SteamVrConfig) -> Option<JoinHandle<()>> {
        if self.status.enabled != config.enabled {
            self.status.enabled = config.enabled;
            self.notify.notify_one();
        }
        None
    }

    pub fn publish(&self, _lines: &[DisplayLine]) {}

    pub fn status(&self) -> SteamVrStatus {
        self.status.clone()
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        None
    }
}
