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

/// 松手后待主线程 clamp + 持久化的面板位姿（仅最终值，非每帧）。
#[derive(Debug, Clone, PartialEq)]
pub struct SteamVrPoseUpdate {
    pub distance_m: f32,
    pub height_offset_m: f32,
    pub horizontal_offset_m: f32,
    pub world_position: [f32; 3],
}

#[derive(Debug, Clone, Serialize)]
pub struct SteamVrStatus {
    pub available: bool,
    pub enabled: bool,
    pub connected: bool,
    pub message: String,
    /// 当前手势是否锁定
    pub gesture_locked: bool,
    /// 是否正在拖动
    pub dragging: bool,
    /// 移动模式下是否真的成功屏蔽了游戏输入（3C 恒 false）
    pub input_blocked: bool,
    /// 屏蔽能力/配置状态说明（3C 下为空；3A 未实现时不声称已屏蔽）
    pub input_block_message: String,
}

#[allow(clippy::derivable_impls)]
impl Default for SteamVrStatus {
    fn default() -> Self {
        Self {
            available: cfg!(all(feature = "steamvr", target_os = "windows")),
            enabled: false,
            connected: false,
            message: String::new(),
            // 与 GestureConfig::default().locked 一致
            gesture_locked: true,
            dragging: false,
            input_blocked: false,
            input_block_message: String::new(),
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

    /// 取走 VR 线程松手后的待持久化位姿（若有）。
    pub fn take_pose_update(&self) -> Option<SteamVrPoseUpdate> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_pose_update()
    }

    /// 立即更新手势锁定状态（供快捷键 / 浮窗按钮；写回配置由调用方负责）。
    pub fn set_gesture_locked(&self, locked: bool) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_gesture_locked(locked);
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

/// 手势定位纯函数（矩阵 / 射线 / 状态机）；跨平台可测，不依赖 openvr。
/// imp（Windows）接入前，非 Windows 构建仅单测引用，故允许 dead_code。
#[allow(dead_code)]
pub mod gesture;

/// HMD 局部坐标系下的面板 3×4 变换（行主序，与 OpenVR Matrix3x4 一致）。
///
/// 轴向约定：+X 右、+Y 上、-Z 前。`horizontal_offset_m` 右正；
/// 为 0 时与 v0.2 硬编码 `x = 0.0` 的矩阵完全一致。
///
/// 非 Windows 构建没有真实 `imp`，仅单测引用；允许 dead_code。
#[cfg_attr(not(all(feature = "steamvr", target_os = "windows")), allow(dead_code))]
pub(crate) fn hmd_local_matrix(config: &SteamVrConfig) -> [[f32; 4]; 3] {
    [
        [1.0, 0.0, 0.0, config.horizontal_offset_m],
        [0.0, 1.0, 0.0, config.height_offset_m],
        [0.0, 0.0, 1.0, -config.distance_m],
    ]
}

#[cfg(test)]
mod hmd_matrix_tests {
    use super::hmd_local_matrix;
    use crate::config::SteamVrConfig;

    /// v0.2 `hmd_matrix` 的硬编码形态（x 恒为 0）。
    fn v02_hmd_matrix(config: &SteamVrConfig) -> [[f32; 4]; 3] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, config.height_offset_m],
            [0.0, 0.0, 1.0, -config.distance_m],
        ]
    }

    #[test]
    fn horizontal_offset_default_zero_matches_v02() {
        let config = SteamVrConfig::default();
        assert_eq!(config.horizontal_offset_m, 0.0);
        assert_eq!(hmd_local_matrix(&config), v02_hmd_matrix(&config));
    }

    #[test]
    fn horizontal_offset_maps_to_matrix_x_right_positive() {
        let config = SteamVrConfig {
            horizontal_offset_m: 0.35,
            height_offset_m: -0.1,
            distance_m: 2.0,
            ..SteamVrConfig::default()
        };
        let m = hmd_local_matrix(&config);
        assert_eq!(m[0][3], 0.35);
        assert_eq!(m[1][3], -0.1);
        assert_eq!(m[2][3], -2.0);
        // 旋转部分仍为单位矩阵
        assert_eq!(m[0][..3], [1.0, 0.0, 0.0]);
        assert_eq!(m[1][..3], [0.0, 1.0, 0.0]);
        assert_eq!(m[2][..3], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn missing_horizontal_offset_in_toml_stays_v02_equivalent() {
        let config: SteamVrConfig = toml::from_str(
            r#"
            enabled = false
            "#,
        )
        .expect("parse");
        assert_eq!(config.horizontal_offset_m, 0.0);
        assert_eq!(hmd_local_matrix(&config), v02_hmd_matrix(&config));
    }
}

#[cfg(feature = "steamvr")]
pub mod render;

#[cfg(test)]
mod status_api_tests {
    use super::{SteamVrPoseUpdate, SteamVrSink, SteamVrStatus};
    use crate::config::SteamVrConfig;
    use std::sync::Arc;

    #[test]
    fn status_exposes_gesture_fields_and_3c_input_unblocked() {
        let config = SteamVrConfig::default();
        assert!(config.gesture.locked);
        let sink = SteamVrSink::new(&config, Arc::new(tokio::sync::Notify::new()));
        let status = sink.status();
        assert!(status.gesture_locked);
        assert!(!status.dragging);
        assert!(!status.input_blocked);
        assert!(status.input_block_message.is_empty());
        assert!(sink.take_pose_update().is_none());
        // 保证类型可构造（主线程 clamp 后写回同结构）
        let pose = SteamVrPoseUpdate {
            distance_m: 1.5,
            height_offset_m: -0.1,
            horizontal_offset_m: 0.2,
            world_position: [1.0, 1.5, -2.0],
        };
        assert_eq!(pose.distance_m, 1.5);
        let defaults = SteamVrStatus::default();
        assert!(defaults.gesture_locked);
        assert!(!defaults.input_blocked);
    }

    #[test]
    fn set_gesture_locked_updates_status_and_clears_dragging_flag() {
        let mut config = SteamVrConfig::default();
        config.gesture.locked = true;
        let sink = SteamVrSink::new(&config, Arc::new(tokio::sync::Notify::new()));
        sink.set_gesture_locked(false);
        let status = sink.status();
        assert!(!status.gesture_locked);
        assert!(!status.dragging);
        assert!(!status.input_blocked);
        // 再次锁定
        sink.set_gesture_locked(true);
        assert!(sink.status().gesture_locked);
    }

    #[test]
    fn apply_config_syncs_gesture_locked_from_config() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let sink = SteamVrSink::new(&SteamVrConfig::default(), notify);
        assert!(sink.status().gesture_locked);
        let mut config = SteamVrConfig::default();
        config.gesture.locked = false;
        let _ = sink.apply_config_detached(&config);
        assert!(!sink.status().gesture_locked);
    }
}
