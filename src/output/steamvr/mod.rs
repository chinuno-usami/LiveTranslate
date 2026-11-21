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
    /// 松手写回；`[1]` 已归一化到 `(-180, 180]`；`[0]`/`[2]` 原样保留不应用。
    pub world_rotation_deg: [f32; 3],
}

/// 3A 输入屏蔽诊断（只读提示；绝不写入 SteamVR 全局设置）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct InputBlockStatus {
    /// `Some(true/false)` 表示已读到实验开关；`None` = 尚未检测或读取失败
    pub experimental_enabled: Option<bool>,
    pub experimental_message: String,
    pub manifest_ok: bool,
    pub manifest_message: String,
    pub app_registered: bool,
    pub app_register_message: String,
    pub bindings_ok: bool,
    pub bindings_message: String,
}

impl InputBlockStatus {
    /// 标准版 / 非 Windows：保留配置字段，运行时不可用。
    pub fn unsupported_build() -> Self {
        const MSG: &str = "当前构建不支持，仅保留配置字段";
        Self {
            experimental_enabled: None,
            experimental_message: "当前构建不支持输入屏蔽（仅 Windows Full 版）".into(),
            manifest_ok: false,
            manifest_message: MSG.into(),
            app_registered: false,
            app_register_message: MSG.into(),
            bindings_ok: false,
            bindings_message: MSG.into(),
        }
    }

    /// 尚未尝试初始化（例如未开启 block_game_input，或尚未进入移动模式）。
    pub fn not_probed() -> Self {
        Self {
            experimental_enabled: None,
            experimental_message: "尚未检测：开启「移动时屏蔽游戏输入」并进入拖动后才会探测".into(),
            manifest_ok: false,
            manifest_message: "尚未检测 action manifest".into(),
            app_registered: false,
            app_register_message: "尚未检测 steamvr.vrmanifest 注册".into(),
            bindings_ok: false,
            bindings_message: "尚未检测默认 binding".into(),
        }
    }
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
    /// 移动模式下是否真的成功屏蔽了游戏输入（仅屏蔽成功时为 true）
    pub input_blocked: bool,
    /// 屏蔽能力/配置状态汇总说明
    pub input_block_message: String,
    /// 实验设置 / manifest / 注册 / binding 可读明细（设置页展示）
    pub input_block: InputBlockStatus,
}

#[allow(clippy::derivable_impls)]
impl Default for SteamVrStatus {
    fn default() -> Self {
        let available = cfg!(all(feature = "steamvr", target_os = "windows"));
        Self {
            available,
            enabled: false,
            connected: false,
            message: String::new(),
            // 与 GestureConfig::default().locked 一致
            gesture_locked: true,
            dragging: false,
            input_blocked: false,
            input_block_message: String::new(),
            input_block: if available {
                InputBlockStatus::not_probed()
            } else {
                InputBlockStatus::unsupported_build()
            },
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

    /// 同步活动配置路径，供 3A 资源解压到配置旁 `steamvr/<version>/`。
    pub fn set_active_config_path(&self, path: Option<std::path::PathBuf>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_active_config_path(path);
    }

    /// 取走 VR 内解锁手势的一次性请求（主线程负责写回 `gesture.locked = false`）。
    pub fn take_unlock_request(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_unlock_request()
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

/// SteamVR Input 2.0 内嵌资源解压（跨平台可测；Windows `input.rs` 取绝对路径）。
#[cfg(feature = "steamvr")]
pub mod assets;

/// SteamVR Input 2.0 输入屏蔽（3A）：仅 Windows 真实 OpenVR；由 imp 在 `block_game_input` 时懒初始化。
#[cfg(all(feature = "steamvr", target_os = "windows"))]
pub mod input;

/// HMD 局部坐标系下的面板 3×4 变换（行主序，与 OpenVR Matrix3x4 一致）。
///
/// 轴向约定：+X 右、+Y 上、-Z 前。`horizontal_offset_m` 右正；
/// 为 0 且 `hmd_face_user=false` 时与 v0.2 硬编码 `x = 0.0` 的矩阵完全一致。
///
/// `hmd_face_user=true` 时仅改偏航（直立）：`yaw = atan2(-x, d)`，使可视面朝头显原点，
/// 避免 `horizontal_offset_m ≠ 0` 时斜视；不写入 `hmd_rotation_deg`。
///
/// 非 Windows 构建没有真实 `imp`，仅单测引用；允许 dead_code。
#[cfg_attr(not(all(feature = "steamvr", target_os = "windows")), allow(dead_code))]
pub(crate) fn hmd_local_matrix(config: &SteamVrConfig) -> [[f32; 4]; 3] {
    let x = config.horizontal_offset_m;
    let y = config.height_offset_m;
    let z = -config.distance_m;
    if !config.hmd_face_user {
        return [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z]];
    }
    // 与文档 §3.4 / face_user_yaw(HMD原点, 面板局部点) 一致：atan2(-x, d)
    let yaw = gesture::face_user_yaw([0.0, 0.0, 0.0], [x, y, z]);
    gesture::compose_world_rows([x, y, z], yaw).0
}

#[cfg(test)]
mod hmd_matrix_tests {
    use super::gesture::{compose_world_rows, extract_yaw, face_user_yaw};
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
        assert!(!config.hmd_face_user);
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
        // 默认关闭面向用户：旋转部分仍为单位矩阵（v0.2/v0.3 等价）
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
        assert!(!config.hmd_face_user);
        assert_eq!(hmd_local_matrix(&config), v02_hmd_matrix(&config));
    }

    #[test]
    fn hmd_face_user_zero_offset_still_matches_v02() {
        let config = SteamVrConfig {
            hmd_face_user: true,
            ..SteamVrConfig::default()
        };
        assert_eq!(config.horizontal_offset_m, 0.0);
        assert_eq!(hmd_local_matrix(&config), v02_hmd_matrix(&config));
    }

    #[test]
    fn hmd_face_user_yaws_toward_origin_for_side_offset() {
        let config = SteamVrConfig {
            horizontal_offset_m: 0.35,
            height_offset_m: -0.1,
            distance_m: 2.0,
            hmd_face_user: true,
            ..SteamVrConfig::default()
        };
        let m = hmd_local_matrix(&config);
        let pos = [0.35, -0.1, -2.0];
        let expected_yaw = face_user_yaw([0.0, 0.0, 0.0], pos);
        // 文档：yaw = atan2(-x, d)
        assert!((expected_yaw - (-0.35f32).atan2(2.0)).abs() < 1e-5);
        let expected = compose_world_rows(pos, expected_yaw).0;
        assert_eq!(m, expected);
        assert!((extract_yaw(&super::gesture::Mat3x4(m)) - expected_yaw).abs() < 1e-5);
        // Z 列水平指向原点（面板可视面朝用户）
        let z_col = [m[0][2], m[2][2]];
        let to_origin = [-pos[0], -pos[2]];
        let dot = z_col[0] * to_origin[0] + z_col[1] * to_origin[1];
        let zn = (z_col[0] * z_col[0] + z_col[1] * z_col[1]).sqrt();
        let on = (to_origin[0] * to_origin[0] + to_origin[1] * to_origin[1]).sqrt();
        assert!((dot / (zn * on) - 1.0).abs() < 1e-4);
        // 高度偏移不进入俯仰：中间行仍为 [0,1,0,y]
        assert_eq!(m[1][..3], [0.0, 1.0, 0.0]);
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
        // stub/非 Windows：unsupported；Windows Full 真实 sink 初始为 not_probed
        assert!(
            status
                .input_block
                .experimental_message
                .contains("当前构建不支持")
                || status.input_block.experimental_message.contains("尚未检测")
        );
        assert!(sink.take_pose_update().is_none());
        // 保证类型可构造（主线程 clamp 后写回同结构）
        let pose = SteamVrPoseUpdate {
            distance_m: 1.5,
            height_offset_m: -0.1,
            horizontal_offset_m: 0.2,
            world_position: [1.0, 1.5, -2.0],
            world_rotation_deg: [0.0, 15.0, 0.0],
        };
        assert_eq!(pose.distance_m, 1.5);
        let defaults = SteamVrStatus::default();
        assert!(defaults.gesture_locked);
        assert!(!defaults.input_blocked);
        assert!(!defaults.input_block.manifest_ok);
    }

    #[test]
    fn input_block_status_unsupported_and_not_probed() {
        let unsupported = super::InputBlockStatus::unsupported_build();
        assert!(unsupported.experimental_message.contains("当前构建不支持"));
        assert!(!unsupported.manifest_ok);
        assert!(!unsupported.app_registered);
        assert!(!unsupported.bindings_ok);

        let probed = super::InputBlockStatus::not_probed();
        assert!(probed.experimental_message.contains("尚未检测"));
        assert!(probed.manifest_message.contains("尚未检测"));
        assert_ne!(unsupported, probed);
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
