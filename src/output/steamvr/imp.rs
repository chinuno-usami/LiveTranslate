//! Windows + steamvr feature 的真实实现。
//!
//! 线程模型：tokio 侧只更新共享快照并递增 revision；VR 线程默认各 ~33ms 检查一次，
//! 内容变化才重新渲染/上传，因此字幕事件不会跨线程拷贝两次纹理。
//! 可选：Dragging 期把轮询提到 ~11ms（见 [`FASTER_INTERVAL_WHILE_DRAGGING`]），纹理节奏不变。
//!
//! 3C 手势：legacy `GetControllerStateWithPose` + `gesture::GestureState`；
//! `config_epoch` 与内容 `revision` 分离；松手经 `pending_pose` 通知主线程持久化。
//!
//! 3A 输入屏蔽：仅当 `gesture.block_game_input` 且进入拖动时懒初始化
//! [`super::input::InputBlocker`]，以 overlay-global `nPriority` 提交；
//! 松手 / cancel / Dashboard / 断开 / 异常路径显式归零；失败降级 3C。
//!
//! free 拖动期 pointer 隐藏（面板 absolute，与基于 live_config 的 plane_hit 不同步）；
//! free + both_grips 拖动中不再要求真实 pointer 命中（见 [`gesture::chord_pointer_for_space`]）；
//! plane 仍跟命中点并换色。clamp 触边界每拖动会话至多一次 haptic。

use super::gesture::{
    self, apply_hmd_delta, apply_world_delta, chord_pointer_for_space, clamp_hmd_offsets_checked,
    clamp_world_position_checked, compose_world_rows, controller_forward, drag_world_delta,
    extract_yaw, hit_in_panel_rect, hmd_offsets_from_world, normalize_deg, ray_nearly_parallel,
    ray_plane_intersection, FreeDragSession, GestureFrameInput, GestureState, GestureTransition,
    GrabChord, HmdOffsets, Mat3x4, UnlockGestureState, MAX_FRAME_DELTA_M, PANEL_HIT_MARGIN_FRAC,
};
use super::input::InputBlocker;
use super::render;
use super::{InputBlockStatus, SteamVrPoseUpdate, SteamVrStatus};
use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use openvr::overlay::{OverlayHandle, VROverlayError};
use openvr::pose::Matrix3x4;
use openvr::system::Event;
use openvr::tracked_device_index;
use openvr::{
    button_id, ApplicationType, Context, ControllerState, InitError, TrackedControllerRole,
    TrackingUniverseOrigin,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// openvr 0.9 直接把 &str 裸指针传给 C 的 CreateOverlay，末尾必须自带 '\0'
const OVERLAY_KEY: &str = "com.chinuno.livetranslate.subtitle\0";
const OVERLAY_NAME: &str = "LiveTranslate Subtitle\0";
const POINTER_OVERLAY_KEY: &str = "com.chinuno.livetranslate.subtitle.pointer\0";
const POINTER_OVERLAY_NAME: &str = "LiveTranslate Pointer\0";
const POINTER_SORT_ORDER: u32 = 32;
const POINTER_WIDTH_M: f32 = 0.018;
/// 光标沿面板法线朝用户前推的距离，避免与面板平面共面导致 z-fighting
const POINTER_FORWARD_OFFSET_M: f32 = 0.02;
const POINTER_TEX_SIZE: usize = 32;
const HAPTIC_PULSE_US: u16 = 1200;
/// 非拖动期 / 纹理友好默认轮询间隔。
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// Dragging 期位姿轮询间隔（仅当 [`FASTER_INTERVAL_WHILE_DRAGGING`] 为 true）。
/// 纹理上传仍跟 content `revision`，不因本开关提速——换跟手度、略增 CPU。
const DRAGGING_FRAME_INTERVAL: Duration = Duration::from_millis(11);
/// 拖动时是否改用 [`DRAGGING_FRAME_INTERVAL`]。关=始终 ~33ms（更省）。
const FASTER_INTERVAL_WHILE_DRAGGING: bool = true;
/// OpenVR `ButtonMaskFromId`：`1 << EVRButtonId`
const BTN_GRIP: u64 = 1u64 << (button_id::GRIP as u32);
const BTN_TRIGGER: u64 = 1u64 << (button_id::STEAM_VR_TRIGGER as u32);

struct Shared {
    lines: Mutex<Vec<DisplayLine>>,
    config: Mutex<SteamVrConfig>,
    status: Mutex<SteamVrStatus>,
    /// 活动配置文件路径（便携=exe 旁 config；默认=OS 用户配置）；3A 资源解压用
    active_config_path: Mutex<Option<PathBuf>>,
    /// 内容/显示属性 revision：`publish` 与非手势 `apply_config` 递增；拖动不靠它驱动 transform
    revision: AtomicU64,
    /// 配置世代：仅 `apply_config` 递增；拖动中途变化则取消且不写回
    config_epoch: AtomicU64,
    /// 松手后待主线程持久化的位姿；与 content `revision` 分离
    pending_pose: Mutex<Option<SteamVrPoseUpdate>>,
    /// VR 内解锁手势已触发，待主线程写回 `gesture.locked = false`
    pending_unlock: AtomicBool,
    last_activity: Mutex<Instant>,
    /// 同一时刻最多允许一个线程持有 OpenVR Context（openvr 全局 INITIALIZED 重入会 panic）
    worker_alive: AtomicBool,
    notify: Arc<tokio::sync::Notify>,
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

pub struct SteamVrSink {
    shared: Arc<Shared>,
    worker: Option<Worker>,
}

impl SteamVrSink {
    pub fn new(config: &SteamVrConfig, notify: Arc<tokio::sync::Notify>) -> Self {
        let shared = Arc::new(Shared {
            lines: Mutex::new(Vec::new()),
            config: Mutex::new(config.clone()),
            status: Mutex::new(SteamVrStatus {
                available: true,
                enabled: config.enabled,
                connected: false,
                message: if config.enabled {
                    "等待 SteamVR".to_string()
                } else {
                    "未启用".to_string()
                },
                gesture_locked: config.gesture.locked,
                dragging: false,
                // 默认不屏蔽；仅 block_game_input + 拖动中成功提交高优时置 true
                input_blocked: false,
                input_block_message: String::new(),
                input_block: InputBlockStatus::not_probed(),
            }),
            active_config_path: Mutex::new(None),
            revision: AtomicU64::new(1),
            config_epoch: AtomicU64::new(1),
            pending_pose: Mutex::new(None),
            pending_unlock: AtomicBool::new(false),
            last_activity: Mutex::new(Instant::now()),
            worker_alive: AtomicBool::new(false),
            notify,
        });

        let mut sink = Self {
            shared,
            worker: None,
        };
        if config.enabled {
            sink.start();
        }
        sink
    }

    fn start(&mut self) {
        if let Some(existing) = &self.worker {
            if !existing.handle.is_finished() {
                return;
            }
        }
        if let Some(finished) = self.worker.take() {
            let _ = finished.handle.join();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let shared = self.shared.clone();
        match std::thread::Builder::new()
            .name("steamvr-overlay".to_string())
            .spawn(move || run(shared, worker_stop))
        {
            Ok(handle) => {
                self.worker = Some(Worker { stop, handle });
            }
            Err(e) => {
                set_status(&self.shared, false, format!("无法启动 SteamVR 线程: {e}"));
            }
        }
    }

    /// 请求当前 VR 线程退出并取走句柄。
    ///
    /// 每个线程持有独立的 `stop` 标志：即便旧线程因 `openvr::init()` 卡住被超时 detach，
    /// 新线程也不会把旧线程的 `stop` 刷回 `false`。
    fn take_thread(&mut self) -> Option<JoinHandle<()>> {
        let worker = self.worker.take()?;
        worker.stop.store(true, Ordering::SeqCst);
        Some(worker.handle)
    }

    pub fn apply_config(&mut self, config: &SteamVrConfig) -> Option<JoinHandle<()>> {
        *lock(&self.shared.config) = config.clone();
        {
            let mut status = lock(&self.shared.status);
            status.enabled = config.enabled;
            status.gesture_locked = config.gesture.locked;
            // 关闭 3A 时清屏蔽声明；开启时由 VR 线程在拖动中更新
            if !config.gesture.block_game_input {
                status.input_blocked = false;
                status.input_block_message.clear();
                status.input_block = InputBlockStatus::not_probed();
            }
            if !config.enabled {
                status.connected = false;
                status.dragging = false;
                status.input_blocked = false;
                status.input_block_message.clear();
                status.input_block = InputBlockStatus::not_probed();
                status.message = "未启用".to_string();
            } else if !status.connected && status.message == "未启用" {
                status.message = "正在连接 SteamVR".to_string();
            }
        }
        if config.enabled {
            // 调整设置或实时预览时刷新活动时间，让已因静音淡出的面板重新显现
            *lock(&self.shared.last_activity) = Instant::now();
        }
        // epoch 与 revision 分离：设置页改参取消拖动；publish 不得冒充 epoch
        self.shared.config_epoch.fetch_add(1, Ordering::SeqCst);
        self.shared.revision.fetch_add(1, Ordering::SeqCst);
        self.shared.notify.notify_one();

        if config.enabled {
            self.start();
            None
        } else {
            self.take_thread()
        }
    }

    pub fn publish(&self, lines: &[DisplayLine]) {
        // VR 线程只在克隆 Vec 时短暂持锁（微秒级），直接 lock 以免丢失收尾字幕或清屏快照
        *lock(&self.shared.lines) = lines.to_vec();
        if !lines.is_empty() {
            *lock(&self.shared.last_activity) = Instant::now();
        }
        // 仅内容 revision；不碰 config_epoch
        self.shared.revision.fetch_add(1, Ordering::SeqCst);
    }

    pub fn status(&self) -> SteamVrStatus {
        lock(&self.shared.status).clone()
    }

    pub fn take_pose_update(&self) -> Option<SteamVrPoseUpdate> {
        lock(&self.shared.pending_pose).take()
    }

    /// VR 内解锁手势触发后取走一次性请求（主线程负责写回配置）。
    pub fn take_unlock_request(&self) -> bool {
        self.shared.pending_unlock.swap(false, Ordering::SeqCst)
    }

    pub fn set_gesture_locked(&self, locked: bool) {
        {
            let mut config = lock(&self.shared.config);
            config.gesture.locked = locked;
        }
        let changed = {
            let mut status = lock(&self.shared.status);
            let changed = status.gesture_locked != locked || status.dragging;
            status.gesture_locked = locked;
            // 锁定变化时取消拖动态（VR 线程状态机下帧也会看到 locked）
            if locked {
                status.dragging = false;
            }
            changed
        };
        if changed {
            self.shared.notify.notify_one();
        }
    }

    /// 同步活动配置路径，供 3A 资源解压（便携 zip = exe 旁 config）。
    pub fn set_active_config_path(&self, path: Option<PathBuf>) {
        *lock(&self.shared.active_config_path) = path;
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.take_thread()
    }
}

impl Drop for SteamVrSink {
    fn drop(&mut self) {
        if let Some(handle) = self.take_thread() {
            super::join_bounded(handle, super::STOP_TIMEOUT);
        }
    }
}

/// 锁中毒时取回内部值：VR 线程里一次 panic 不该让输出永久失效
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_status(shared: &Shared, connected: bool, message: impl Into<String>) {
    let changed = {
        let mut status = lock(&shared.status);
        // 若外部已将输出停用，正在退出途中的工作线程不得再写回“已连接”状态
        if !status.enabled && connected {
            return;
        }
        let message = message.into();
        let changed = status.connected != connected || status.message != message;
        status.connected = connected;
        status.message = message;
        changed
    };
    if changed {
        shared.notify.notify_one();
    }
}

fn set_dragging(shared: &Shared, dragging: bool) {
    let changed = {
        let mut status = lock(&shared.status);
        if status.dragging == dragging {
            false
        } else {
            status.dragging = dragging;
            true
        }
    };
    if changed {
        shared.notify.notify_one();
    }
}

fn set_input_block_status(
    shared: &Shared,
    blocked: bool,
    message: impl Into<String>,
    detail: Option<&InputBlockStatus>,
) {
    let changed = {
        let mut status = lock(&shared.status);
        let message = message.into();
        let mut changed = status.input_blocked != blocked || status.input_block_message != message;
        status.input_blocked = blocked;
        status.input_block_message = message;
        if let Some(detail) = detail {
            if status.input_block != *detail {
                status.input_block = detail.clone();
                changed = true;
            }
        }
        changed
    };
    if changed {
        shared.notify.notify_one();
    }
}

fn diagnostics_to_status(diag: &super::input::InputBlockDiagnostics) -> InputBlockStatus {
    InputBlockStatus {
        experimental_enabled: diag.experimental_enabled,
        experimental_message: diag.experimental_message.clone(),
        manifest_ok: diag.manifest_ok,
        manifest_message: diag.manifest_message.clone(),
        app_registered: diag.app_registered,
        app_register_message: diag.app_register_message.clone(),
        bindings_ok: diag.bindings_ok,
        bindings_message: diag.bindings_message.clone(),
    }
}

/// 会话内 3A 屏蔽控制器：懒初始化、拖动高优、退出归零（Drop 保底）。
struct InputBlockController<'a> {
    ctx: &'a Context,
    shared: &'a Shared,
    blocker: Option<InputBlocker>,
    init_error: Option<String>,
    last_init_attempt: Option<Instant>,
    last_epoch: u64,
    currently_blocked: bool,
}

/// 3A 初始化失败后的重试间隔：用户在 SteamVR 里打开实验开关后无需保存配置也能重试。
const INPUT_BLOCK_INIT_RETRY: Duration = Duration::from_secs(5);

/// 松手后等待主线程落盘 + apply_config 的上限；超时则回到共享配置避免永久卡住。
const POSE_APPLY_TIMEOUT: Duration = Duration::from_secs(3);

impl<'a> InputBlockController<'a> {
    fn new(ctx: &'a Context, shared: &'a Shared) -> Self {
        Self {
            ctx,
            shared,
            blocker: None,
            init_error: None,
            last_init_attempt: None,
            last_epoch: 0,
            currently_blocked: false,
        }
    }

    fn note_epoch(&mut self, epoch: u64) {
        if self.last_epoch != epoch {
            self.last_epoch = epoch;
            // 配置变更后允许重新 try_init（路径 / 实验设置可能已变）
            if self.blocker.is_none() {
                self.init_error = None;
            }
        }
    }

    fn ensure_blocker(&mut self) -> Result<&mut InputBlocker, String> {
        if self.blocker.is_none() {
            if let Some(err) = &self.init_error {
                let retry_due = self
                    .last_init_attempt
                    .map(|at| at.elapsed() >= INPUT_BLOCK_INIT_RETRY)
                    .unwrap_or(true);
                if !retry_due {
                    return Err(err.clone());
                }
                self.init_error = None;
            }
            self.last_init_attempt = Some(Instant::now());
            let path = lock(&self.shared.active_config_path).clone();
            match InputBlocker::try_init(self.ctx, path.as_deref()) {
                Ok(blocker) => self.blocker = Some(blocker),
                Err(err) => {
                    self.init_error = Some(err.clone());
                    return Err(err);
                }
            }
        }
        self.blocker
            .as_mut()
            .ok_or_else(|| "InputBlocker 未初始化".to_string())
    }

    /// 拖动中：提交 overlay-global priority；失败则 `input_blocked=false` 并写可读原因。
    fn apply_blocking(&mut self) {
        if let Err(err) = self.ensure_blocker().map(|_| ()) {
            self.currently_blocked = false;
            let detail = failure_detail(&err);
            set_input_block_status(self.shared, false, err, Some(&detail));
            return;
        }
        // 未屏蔽成功时刷新实验开关（用户可能中途打开）；成功后不再反复读 IVRSettings
        if !self.currently_blocked {
            if let Some(blocker) = self.blocker.as_mut() {
                let _ = blocker.refresh_settings(self.ctx);
            }
        }
        let mut input = match self.ctx.input() {
            Ok(input) => input,
            Err(e) => {
                self.currently_blocked = false;
                let msg = format!("获取 IVRInput 失败: {e:?}");
                let detail = failure_detail(&msg);
                set_input_block_status(self.shared, false, msg, Some(&detail));
                return;
            }
        };
        let Some(blocker) = self.blocker.as_mut() else {
            return;
        };
        match blocker.update_blocking(&mut input) {
            Ok(()) => {
                let detail = diagnostics_to_status(blocker.diagnostics());
                let summary = blocker.diagnostics().summary();
                let msg = if summary.is_empty() {
                    "移动中已屏蔽游戏 grip/trigger".to_string()
                } else {
                    format!("移动中已屏蔽游戏 grip/trigger；{summary}")
                };
                self.currently_blocked = true;
                set_input_block_status(self.shared, true, msg, Some(&detail));
            }
            Err(err) => {
                self.currently_blocked = false;
                let detail = if let Some(blocker) = self.blocker.as_ref() {
                    diagnostics_to_status(blocker.diagnostics())
                } else {
                    failure_detail(&err)
                };
                let lower = err.to_ascii_lowercase();
                let message = if lower.contains("priority") || lower.contains("invalid") {
                    format!(
                        "{err}（可尝试 output.steamvr.application_type = \"overlay\" 后重启应用）"
                    )
                } else {
                    err
                };
                set_input_block_status(self.shared, false, message, Some(&detail));
            }
        }
    }

    /// 退出移动：显式 nPriority 归零；不再声称已屏蔽。
    fn clear_blocking(&mut self) {
        let needs_clear = self
            .blocker
            .as_ref()
            .map(|b| b.priority_active())
            .unwrap_or(false)
            || self.currently_blocked
            || lock(&self.shared.status).input_blocked;
        if !needs_clear {
            return;
        }
        if let Some(blocker) = self.blocker.as_mut() {
            match self.ctx.input() {
                Ok(mut input) => {
                    if let Err(e) = blocker.clear_priority(&mut input) {
                        tracing::warn!("SteamVR clear_priority 失败: {e}");
                    }
                }
                Err(e) => tracing::warn!("SteamVR clear_priority 时获取 IVRInput 失败: {e:?}"),
            }
        }
        let detail = self
            .blocker
            .as_ref()
            .map(|b| diagnostics_to_status(b.diagnostics()))
            .or_else(|| self.init_error.as_ref().map(|e| failure_detail(e)))
            .unwrap_or_else(InputBlockStatus::not_probed);
        let msg = self
            .init_error
            .clone()
            .or_else(|| {
                self.blocker
                    .as_ref()
                    .map(|b| b.diagnostics().summary())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();
        self.currently_blocked = false;
        set_input_block_status(self.shared, false, msg, Some(&detail));
    }
}

/// 初始化失败时把可读错误填进明细字段，便于设置页展示。
/// 只填 manifest 一行，其余保持「未检测」，避免同一段错误在 UI 里出现四次。
fn failure_detail(err: &str) -> InputBlockStatus {
    let mut detail = InputBlockStatus::not_probed();
    detail.manifest_ok = false;
    detail.manifest_message = err.to_string();
    detail.experimental_message.clear();
    detail.app_register_message.clear();
    detail.bindings_message.clear();
    detail
}

impl Drop for InputBlockController<'_> {
    fn drop(&mut self) {
        self.clear_blocking();
    }
}

struct WorkerGuard<'a>(&'a AtomicBool);

impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

enum SessionError {
    WaitingForRuntime,
    Message(String),
}

fn run(shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    // 若上一个被 detach 的线程仍在退出途中，等待它释放 OpenVR Context，防止重复 init 触发 panic
    while shared
        .worker_alive
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _guard = WorkerGuard(&shared.worker_alive);
    // 字体数据库在线程生命周期内只加载一次，避免重连时重复全盘扫描系统字体
    let mut renderer = render::Renderer::new();

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let interval = lock(&shared.config).reconnect_interval_secs.max(1);

        let result = catch_unwind(AssertUnwindSafe(|| session(&shared, &stop, &mut renderer)));
        if stop.load(Ordering::SeqCst) {
            // 主动停止：不要再写“已断开”，否则会覆盖刚设置的“未启用”
            break;
        }
        match result {
            Ok(Ok(())) => set_status(&shared, false, "SteamVR 已退出，等待重连"),
            Ok(Err(SessionError::WaitingForRuntime)) => {
                set_status(&shared, false, "等待 SteamVR 启动");
            }
            Ok(Err(SessionError::Message(e))) => {
                set_status(&shared, false, format!("SteamVR 不可用: {e}"));
            }
            Err(_) => set_status(&shared, false, "SteamVR 初始化异常，等待重连"),
        }

        // 分片睡眠，保证退出时能快速 join
        let mut slept = Duration::ZERO;
        while slept < Duration::from_secs(interval) && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }
}

/// 配置里的应用类型字符串 → OpenVR 枚举。默认 Background：
/// 只附着到已运行的 SteamVR，不在桌面场景下主动拉起 vrserver。
fn application_type(config: &SteamVrConfig) -> ApplicationType {
    match config.application_type.as_str() {
        "overlay" => ApplicationType::Overlay,
        _ => ApplicationType::Background,
    }
}

fn init_context(config: &SteamVrConfig) -> Result<openvr::Context, SessionError> {
    if !openvr::is_runtime_installed() {
        return Err(SessionError::Message("未检测到 SteamVR 运行时".to_string()));
    }
    unsafe { openvr::init(application_type(config)) }.map_err(|err| match err {
        InitError::Init_NoServerForBackgroundApp | InitError::Init_ShuttingDown => {
            SessionError::WaitingForRuntime
        }
        other => SessionError::Message(format!("{other:?}")),
    })
}

fn is_fatal_overlay_error(err: VROverlayError) -> bool {
    // RequestFailed 常见于单帧纹理尺寸或瞬时 IPC 繁忙，不应销毁整个 OpenVR 会话触发 5s 重连
    matches!(
        err,
        VROverlayError::UnknownOverlay | VROverlayError::InvalidHandle
    )
}

fn button_down(state: &ControllerState, mask: u64) -> bool {
    state.button_pressed & mask != 0
}

fn mat_from_pose(rows: &[[f32; 4]; 3]) -> Mat3x4 {
    Mat3x4(*rows)
}

fn length3(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn panel_half_extents(config: &SteamVrConfig, tex_w: usize, tex_h: usize) -> (f32, f32) {
    let width = config.width_m.clamp(0.3, 5.0);
    let aspect = if tex_w > 0 {
        tex_h as f32 / tex_w as f32
    } else {
        0.5
    };
    let height = width * aspect.max(0.05);
    (width * 0.5, height * 0.5)
}

fn panel_world_matrix(config: &SteamVrConfig, hmd_pose: &Mat3x4) -> Mat3x4 {
    if config.anchor == "world" {
        Mat3x4(world_matrix_rows(config))
    } else {
        hmd_pose.multiply(&Mat3x4(super::hmd_local_matrix(config)))
    }
}

fn make_pointer_rgba(dragging: bool) -> Vec<u8> {
    let size = POINTER_TEX_SIZE;
    let mut rgba = vec![0u8; size * size * 4];
    let (r, g, b, a) = if dragging {
        (255u8, 168, 64, 230)
    } else {
        (255u8, 255, 255, 210)
    };
    let cx = (size as f32 - 1.0) * 0.5;
    let cy = cx;
    let rad2 = 10.0f32 * 10.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx + dy * dy <= rad2 {
                let i = (y * size + x) * 4;
                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = a;
            }
        }
    }
    rgba
}

fn pose_update_from_config(config: &SteamVrConfig) -> SteamVrPoseUpdate {
    SteamVrPoseUpdate {
        distance_m: config.distance_m,
        height_offset_m: config.height_offset_m,
        horizontal_offset_m: config.horizontal_offset_m,
        world_position: config.world_position,
        world_rotation_deg: config.world_rotation_deg,
    }
}

fn haptic(system: &openvr::System, device: Option<openvr::TrackedDeviceIndex>, enabled: bool) {
    if !enabled {
        return;
    }
    if let Some(idx) = device {
        system.trigger_haptic_pulse(idx, 0, HAPTIC_PULSE_US);
    }
}

struct ControllerSample {
    left_grip: bool,
    left_trigger: bool,
    right_grip: bool,
    right_trigger: bool,
    right_pose: Option<Mat3x4>,
    right_index: Option<openvr::TrackedDeviceIndex>,
    left_index: Option<openvr::TrackedDeviceIndex>,
    tracking_ok: bool,
}

/// `require_left` 为 false 时（单手抓取组合）左手 tracking 失效不再视为整体失败，
/// 否则左手柄充电/待机时 `right_grip` / `right_trigger` 也会拖不动。
fn poll_controllers(system: &openvr::System, require_left: bool) -> ControllerSample {
    let left_index =
        system.tracked_device_index_for_controller_role(TrackedControllerRole::LeftHand);
    let right_index =
        system.tracked_device_index_for_controller_role(TrackedControllerRole::RightHand);

    let mut sample = ControllerSample {
        left_grip: false,
        left_trigger: false,
        right_grip: false,
        right_trigger: false,
        right_pose: None,
        right_index,
        left_index,
        tracking_ok: true,
    };

    if let Some(idx) = left_index {
        if let Some((state, pose)) =
            system.controller_state_with_pose(TrackingUniverseOrigin::Standing, idx)
        {
            sample.left_grip = button_down(&state, BTN_GRIP);
            sample.left_trigger = button_down(&state, BTN_TRIGGER);
            if !pose.pose_is_valid() && require_left {
                sample.tracking_ok = false;
            }
        } else if require_left {
            // Dashboard / 系统菜单时常返回 None：仅在需要左手时视为输入不可用
            sample.tracking_ok = false;
        }
    }

    if let Some(idx) = right_index {
        match system.controller_state_with_pose(TrackingUniverseOrigin::Standing, idx) {
            Some((state, pose)) => {
                sample.right_grip = button_down(&state, BTN_GRIP);
                sample.right_trigger = button_down(&state, BTN_TRIGGER);
                if pose.pose_is_valid() {
                    sample.right_pose = Some(mat_from_pose(pose.device_to_absolute_tracking()));
                } else {
                    sample.tracking_ok = false;
                }
            }
            None => {
                sample.tracking_ok = false;
            }
        }
    } else {
        // 右手缺失时无法射线拖动
        sample.tracking_ok = false;
    }

    sample
}

fn hmd_pose_mat(system: &openvr::System) -> Option<Mat3x4> {
    let poses = system.device_to_absolute_tracking_pose(TrackingUniverseOrigin::Standing, 0.0);
    let pose = &poses[tracked_device_index::HMD.0 as usize];
    if pose.pose_is_valid() {
        Some(mat_from_pose(pose.device_to_absolute_tracking()))
    } else {
        None
    }
}

/// 射线或近场命中：返回 (平面命中点, 是否在面板矩形内, 用于拖动的命中点)
fn compute_hit(
    config: &SteamVrConfig,
    panel: &Mat3x4,
    right_pose: &Mat3x4,
    tex_w: usize,
    tex_h: usize,
) -> (Option<[f32; 3]>, bool, Option<[f32; 3]>) {
    let (half_w, half_h) = panel_half_extents(config, tex_w, tex_h);
    let center = panel.translation();
    let normal = panel.panel_normal();
    let right = panel.panel_right();
    let up = panel.panel_up();
    let origin = right_pose.translation();

    if config.gesture.mode == "near" {
        // 近场：把手柄位置投影到面板平面作为命中
        let plane_hit = project_to_plane(origin, center, normal);
        // 半径按「手柄到投影点」算，而不是到面板中心：面板 1.6m 宽时半宽 0.8m，
        // 用中心距离会让边缘永远够不到（默认半径 0.35m）。
        let dist = plane_hit
            .map(|h| length3(sub3(origin, h)))
            .unwrap_or(f32::INFINITY);
        let on = dist <= config.gesture.near_radius_m.max(0.05);
        let on_panel = on
            && plane_hit
                .map(|h| {
                    hit_in_panel_rect(h, center, right, up, half_w, half_h, PANEL_HIT_MARGIN_FRAC)
                })
                .unwrap_or(false);
        let move_hit = if on_panel { plane_hit } else { None };
        return (plane_hit, on_panel, move_hit);
    }

    // ray 模式
    let dir = controller_forward(right_pose);
    if ray_nearly_parallel(dir, normal) {
        return (None, false, None);
    }
    let plane_hit = ray_plane_intersection(origin, dir, center, normal);
    let on_panel = plane_hit
        .map(|h| hit_in_panel_rect(h, center, right, up, half_w, half_h, PANEL_HIT_MARGIN_FRAC))
        .unwrap_or(false);
    let move_hit = if on_panel { plane_hit } else { None };
    (plane_hit, on_panel, move_hit)
}

fn project_to_plane(point: [f32; 3], plane_point: [f32; 3], normal: [f32; 3]) -> Option<[f32; 3]> {
    let len = length3(normal);
    if len < 1e-8 {
        return None;
    }
    let n = [normal[0] / len, normal[1] / len, normal[2] / len];
    let v = sub3(point, plane_point);
    let d = v[0] * n[0] + v[1] * n[1] + v[2] * n[2];
    Some([
        point[0] - n[0] * d,
        point[1] - n[1] * d,
        point[2] - n[2] * d,
    ])
}

fn absolute_matrix_at(hit: [f32; 3], normal: [f32; 3]) -> Matrix3x4 {
    // 面向 -Z 的单位朝向小方块，仅用于 pointer 定位；沿法线朝用户推开一点避免共面闪烁
    let len = length3(normal);
    let offset = if len < 1e-8 {
        [0.0, 0.0, 0.0]
    } else {
        [
            normal[0] / len * POINTER_FORWARD_OFFSET_M,
            normal[1] / len * POINTER_FORWARD_OFFSET_M,
            normal[2] / len * POINTER_FORWARD_OFFSET_M,
        ]
    };
    Matrix3x4([
        [1.0, 0.0, 0.0, hit[0] + offset[0]],
        [0.0, 1.0, 0.0, hit[1] + offset[1]],
        [0.0, 0.0, 1.0, hit[2] + offset[2]],
    ])
}

fn session(
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    renderer: &mut render::Renderer,
) -> Result<(), SessionError> {
    let context = init_context(&lock(&shared.config).clone())?;
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let system = context
        .system()
        .map_err(|e| SessionError::Message(format!("{e:?}")))?;
    let mut overlay = context
        .overlay()
        .map_err(|e| SessionError::Message(format!("{e:?}")))?;
    let handle: OverlayHandle = overlay
        .create_overlay(OVERLAY_KEY, OVERLAY_NAME)
        .map_err(|e| SessionError::Message(format!("创建 Overlay 失败: {e:?}")))?;
    // OpenVR 新建 Overlay 默认不透明度为 1.0，先置 0 再走淡入动画，避免首帧闪烁
    let _ = overlay.set_opacity(handle, 0.0);

    // pointer：独立 key/name + 更高 sort_order；失败不阻断字幕 overlay
    let pointer_handle = match overlay.create_overlay(POINTER_OVERLAY_KEY, POINTER_OVERLAY_NAME) {
        Ok(h) => {
            let _ = overlay.set_sort_order(h, POINTER_SORT_ORDER);
            let _ = overlay.set_width(h, POINTER_WIDTH_M);
            let _ = overlay.set_opacity(h, 1.0);
            let _ = overlay.set_visibility(h, false);
            let rgba = make_pointer_rgba(false);
            let _ = overlay.set_raw_data(h, &rgba, POINTER_TEX_SIZE, POINTER_TEX_SIZE, 4);
            Some(h)
        }
        Err(e) => {
            tracing::warn!("创建 pointer Overlay 失败（手势仍可用）: {e:?}");
            None
        }
    };

    set_status(shared, true, "已连接 SteamVR");

    let mut last_revision = u64::MAX;
    let mut current_opacity = 0.0f32;
    let mut target_opacity = 0.0f32;
    let mut last_animate = Instant::now();
    let mut had_upload_error = false;
    let mut consecutive_upload_errors = 0u32;
    let mut last_upload_retry: Option<Instant> = None;

    let mut live_config = lock(&shared.config).clone();
    let mut gesture = GestureState::new();
    let session_start = Instant::now();
    let mut last_tex_w: usize = 1024;
    let mut last_tex_h: usize = 512;
    let mut pointer_dragging_tex = false;
    let mut panel_visible = false;
    let mut unlock_gesture = UnlockGestureState::new();
    // 松手后等主线程落盘 + apply_config：在此之前不要用旧配置回弹面板
    let mut pose_apply_epoch: Option<(u64, Instant)> = None;
    // 3A：Drop 时归零 priority，覆盖异常 / 提前 return
    let mut input_block = InputBlockController::new(&context, shared);
    // v0.4 free 空间拖动会话（DragStarted 当帧初始化；plane 路径为 None）
    let mut free_session: Option<FreeDragSession> = None;
    // 取消 / 松手后需立即 apply_transform（尤其 hmd 锚点曾用 absolute）
    let mut force_apply_transform = false;
    // clamp 触边界 haptic：每个拖动会话至多一次（DragStarted 清零）
    let mut clamp_haptic_sent = false;

    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = overlay.set_visibility(handle, false);
            if let Some(ph) = pointer_handle {
                let _ = overlay.set_visibility(ph, false);
            }
            // 断开：取消拖动并归零 3A priority，不写回
            if gesture.is_dragging() {
                let _ = gesture.cancel();
                set_dragging(shared, false);
            }
            input_block.clear_blocking();
            return Ok(());
        }

        while let Some(info) = system.poll_next_event() {
            if matches!(
                info.event,
                Event::Quit(_) | Event::DriverRequestedQuit | Event::RestartRequested
            ) {
                system.acknowledge_quit_exiting();
                if gesture.is_dragging() {
                    let _ = gesture.cancel();
                    set_dragging(shared, false);
                }
                input_block.clear_blocking();
                return Ok(());
            }
        }

        // 必须先读 revision 再锁 config/lines，否则并发 apply_config/publish 夹在中间时
        // last_revision 会记录新版本号但应用旧快照，导致最近一次配置更新永久漏掉。
        let revision = shared.revision.load(Ordering::SeqCst);
        let config_epoch = shared.config_epoch.load(Ordering::SeqCst);
        let shared_config = lock(&shared.config).clone();
        input_block.note_epoch(config_epoch);

        // 非拖动时同步主线程配置；拖动中保留 live，仅靠 epoch 取消。
        // 刚松手时位姿还在异步落盘，先用 live 渲染，等 `apply_config` 推进 epoch 再同步，
        // 否则会看到面板先跳回旧位置再跳回来（或落盘失败时永久卡在旧位置）。
        let holding_pose = match pose_apply_epoch {
            Some((epoch, at)) if epoch == config_epoch && at.elapsed() < POSE_APPLY_TIMEOUT => true,
            Some(_) => {
                pose_apply_epoch = None;
                false
            }
            None => false,
        };
        if !gesture.is_dragging() && !holding_pose {
            live_config = shared_config.clone();
            // 关闭 3A 时确保不残留高优（例如设置页关掉 block_game_input）
            if !live_config.gesture.block_game_input {
                input_block.clear_blocking();
            }
        }

        let dashboard_open = overlay.is_dashboard_visible();
        let grab = GrabChord::parse(&live_config.gesture.grab).unwrap_or(GrabChord::BothGrips);
        let want_unlock = live_config.gesture.enabled
            && live_config.gesture.locked
            && live_config.gesture.unlock_via_vr_gesture;
        let controllers = if live_config.gesture.enabled && !live_config.gesture.locked {
            poll_controllers(&system, grab.needs_left_hand())
        } else if want_unlock {
            // 锁定时唯一允许的手柄读取：仅用于识别解锁组合
            poll_controllers(&system, false)
        } else {
            ControllerSample {
                left_grip: false,
                left_trigger: false,
                right_grip: false,
                right_trigger: false,
                right_pose: None,
                right_index: None,
                left_index: None,
                tracking_ok: true,
            }
        };

        let hmd = hmd_pose_mat(&system);
        let mut plane_hit: Option<[f32; 3]> = None;
        let mut pointer_normal: Option<[f32; 3]> = None;
        let mut pointer_on_panel = false;
        let mut move_hit: Option<[f32; 3]> = None;

        if let (Some(right_pose), Some(hmd_pose)) = (controllers.right_pose.as_ref(), hmd.as_ref())
        {
            let panel = panel_world_matrix(&live_config, hmd_pose);
            let (ph, on, mh) =
                compute_hit(&live_config, &panel, right_pose, last_tex_w, last_tex_h);
            plane_hit = ph;
            pointer_normal = Some(panel.panel_normal());
            pointer_on_panel = on && panel_visible;
            move_hit = if panel_visible { mh } else { None };
        }

        let now = session_start.elapsed();
        if want_unlock {
            if unlock_gesture.update(
                controllers.left_trigger,
                controllers.right_trigger,
                pointer_on_panel,
                now,
            ) {
                shared.pending_unlock.store(true, Ordering::SeqCst);
                haptic(
                    &system,
                    controllers.right_index.or(controllers.left_index),
                    live_config.gesture.haptics,
                );
                shared.notify.notify_one();
            }
        } else {
            unlock_gesture.reset();
        }

        // Dashboard / 追踪失效 / 面板隐藏：取消进行中的拖动
        let force_cancel = dashboard_open
            || !controllers.tracking_ok
            || (gesture.is_dragging() && !panel_visible)
            || hmd.is_none();

        if force_cancel
            && (gesture.is_dragging() || matches!(gesture.phase(), gesture::GesturePhase::Arming))
        {
            let tr = gesture.cancel();
            if tr == GestureTransition::DragCancelled {
                set_dragging(shared, false);
            }
            // 取消不写回：丢弃拖动中的 live 偏移；3A 归零；立即恢复原锚点变换
            pose_apply_epoch = None;
            live_config = shared_config.clone();
            free_session = None;
            clamp_haptic_sent = false;
            force_apply_transform = true;
            input_block.clear_blocking();
        }

        // free 拖动：absolute 面板与 live_config 命中不同步；both_grips 若仍要求真实
        // pointer 会在左右挥/near 跟手时误松手。arming / plane 仍用真实命中。
        let pointer_for_chord = chord_pointer_for_space(
            &live_config.gesture.space,
            gesture.is_dragging(),
            pointer_on_panel,
        );
        let frame_input = GestureFrameInput {
            locked: live_config.gesture.locked || !live_config.gesture.enabled,
            left_grip: controllers.left_grip,
            right_grip: controllers.right_grip,
            right_trigger: controllers.right_trigger,
            pointer_on_panel: pointer_for_chord,
            config_epoch,
            now,
        };
        let start_hmd = HmdOffsets::from_config_fields(
            live_config.horizontal_offset_m,
            live_config.height_offset_m,
            live_config.distance_m,
        );
        let start_world = live_config.world_position;
        let transition = if force_cancel {
            GestureTransition::None
        } else {
            gesture.update(&frame_input, grab, start_hmd, start_world)
        };

        match transition {
            GestureTransition::DragStarted => {
                set_dragging(shared, true);
                free_session = None;
                clamp_haptic_sent = false;
                if live_config.gesture.space == "free" {
                    if let (Some(hmd_pose), Some(right_pose)) =
                        (hmd.as_ref(), controllers.right_pose.as_ref())
                    {
                        let panel = panel_world_matrix(&live_config, hmd_pose);
                        let ray_mode = live_config.gesture.mode != "near";
                        free_session = Some(FreeDragSession::start(
                            ray_mode, hmd_pose, right_pose, panel,
                        ));
                    }
                }
                if live_config.gesture.block_game_input {
                    input_block.apply_blocking();
                }
                haptic(
                    &system,
                    controllers.right_index.or(controllers.left_index),
                    live_config.gesture.haptics,
                );
                *lock(&shared.last_activity) = Instant::now();
            }
            GestureTransition::DragReleased => {
                let mut release_clamped = false;
                if let Some(session) = free_session.take() {
                    release_clamped =
                        apply_free_session_to_config(&mut live_config, &session, hmd.as_ref());
                    force_apply_transform = true;
                }
                let pose = pose_update_from_config(&live_config);
                *lock(&shared.pending_pose) = Some(pose);
                pose_apply_epoch = Some((config_epoch, Instant::now()));
                let _ = gesture.take_drag_origins();
                set_dragging(shared, false);
                input_block.clear_blocking();
                // 松手必震一次；若本帧才触 clamp 且本会话未发过触边震，不额外叠加
                // （松手震已覆盖提示）。拖动中已发过触边震则 flag 已 true。
                if release_clamped {
                    clamp_haptic_sent = true;
                }
                haptic(
                    &system,
                    controllers.right_index.or(controllers.left_index),
                    live_config.gesture.haptics,
                );
                shared.notify.notify_one();
                *lock(&shared.last_activity) = Instant::now();
            }
            GestureTransition::DragCancelled => {
                set_dragging(shared, false);
                input_block.clear_blocking();
                pose_apply_epoch = None;
                live_config = shared_config.clone();
                free_session = None;
                clamp_haptic_sent = false;
                force_apply_transform = true;
            }
            GestureTransition::None => {}
        }

        // 拖动中每帧刷新高优 action set（OpenVR 需持续 UpdateActionState）
        if gesture.is_dragging() && live_config.gesture.block_game_input {
            input_block.apply_blocking();
        }

        // 拖动中：free → absolute 映射；plane → v0.3 平面增量（不写 Shared.config）
        let mut free_absolute: Option<Mat3x4> = None;
        let mut clamp_hit_this_frame = false;
        if gesture.is_dragging() {
            if live_config.gesture.space == "free" {
                if let Some(session) = free_session.as_mut() {
                    let world_anchor = live_config.anchor == "world";
                    if let (Some(hmd_pose), Some(right_pose)) =
                        (hmd.as_ref(), controllers.right_pose.as_ref())
                    {
                        // tick==false（JUMP 丢帧）时 panel 保持上一有效位姿；仍须 absolute。
                        let _ = session.tick(
                            right_pose,
                            hmd_pose,
                            live_config.gesture.sensitivity,
                            &live_config.gesture.orientation,
                            world_anchor,
                        );
                    }
                    // 拖动中钳 validate，复用每会话一次触边 haptic；减少松手才 clamp 的跳变。
                    clamp_hit_this_frame |= session.clamp_to_validate(world_anchor, hmd.as_ref());
                    // 会话存活期间始终 absolute（含毛刺丢帧）：禁止回落
                    // apply_transform(live_config)（抓取前配置 → 瞬移 / hmd 锚点切换）。
                    free_absolute = Some(session.panel);
                }
            } else if let Some(hmd_pose) = hmd.as_ref() {
                if let Some(delta) = drag_world_delta(
                    move_hit,
                    gesture.prev_hit_mut(),
                    live_config.gesture.sensitivity,
                    MAX_FRAME_DELTA_M,
                ) {
                    if live_config.anchor == "world" {
                        let (next, hit) = clamp_world_position_checked(apply_world_delta(
                            live_config.world_position,
                            delta,
                        ));
                        live_config.world_position = next;
                        clamp_hit_this_frame |= hit;
                    } else {
                        let cur = HmdOffsets::from_config_fields(
                            live_config.horizontal_offset_m,
                            live_config.height_offset_m,
                            live_config.distance_m,
                        );
                        let (next, hit) =
                            clamp_hmd_offsets_checked(apply_hmd_delta(cur, hmd_pose, delta));
                        live_config.horizontal_offset_m = next.horizontal_offset_m;
                        live_config.height_offset_m = next.height_offset_m;
                        live_config.distance_m = next.distance_m;
                        clamp_hit_this_frame |= hit;
                    }
                }
            }
            *lock(&shared.last_activity) = Instant::now();
        }
        if clamp_hit_this_frame && !clamp_haptic_sent {
            haptic(
                &system,
                controllers.right_index.or(controllers.left_index),
                live_config.gesture.haptics,
            );
            clamp_haptic_sent = true;
        }

        // pointer：plane 跟命中点、拖动换色；free 拖动中隐藏（absolute 面板与 plane_hit 不同步）
        if let Some(ph) = pointer_handle {
            let free_dragging = gesture.is_dragging() && live_config.gesture.space == "free";
            let show = live_config.gesture.enabled
                && live_config.gesture.pointer
                && !live_config.gesture.locked
                && !dashboard_open
                && panel_visible
                && plane_hit.is_some()
                && !free_dragging;
            if show {
                if let Some(hit) = plane_hit {
                    let dragging = gesture.is_dragging();
                    if dragging != pointer_dragging_tex {
                        let rgba = make_pointer_rgba(dragging);
                        let _ =
                            overlay.set_raw_data(ph, &rgba, POINTER_TEX_SIZE, POINTER_TEX_SIZE, 4);
                        pointer_dragging_tex = dragging;
                    }
                    let _ = overlay.set_transform_absolute(
                        ph,
                        TrackingUniverseOrigin::Standing,
                        &absolute_matrix_at(hit, pointer_normal.unwrap_or([0.0, 0.0, 1.0])),
                    );
                    let _ = overlay.set_visibility(ph, true);
                }
            } else {
                let _ = overlay.set_visibility(ph, false);
            }
        }

        let dragging_now = gesture.is_dragging();
        let retry_due = revision != last_revision
            && last_upload_retry
                .map(|t| t.elapsed() >= Duration::from_millis(500))
                .unwrap_or(true);

        // free 拖动：绝对变换（含 JUMP 丢帧）；plane / 非拖动：原 apply_transform。
        // free 会话存活时绝不可用 live_config 走 apply_transform（不变量 6 / A12）。
        if let Some(panel) = free_absolute {
            if let Err(e) = overlay.set_transform_absolute(
                handle,
                TrackingUniverseOrigin::Standing,
                &Matrix3x4(panel.0),
            ) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!(
                        "设置 Overlay 坐标失败: {e:?}"
                    )));
                }
                tracing::warn!("设置 Overlay 坐标失败: {e:?}");
            }
        } else if free_session.is_none() && (dragging_now || retry_due || force_apply_transform) {
            // free_session.is_some() 时即使缺 pose / JUMP 丢帧也不进此支，避免回落配置变换。
            force_apply_transform = false;
            if let Err(e) = apply_transform(&mut overlay, handle, &live_config) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!(
                        "设置 Overlay 坐标失败: {e:?}"
                    )));
                }
                tracing::warn!("设置 Overlay 坐标失败: {e:?}");
            }
        }

        if retry_due {
            if let Err(e) = overlay.set_width(handle, live_config.width_m.clamp(0.3, 5.0)) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }
            if let Err(e) = overlay.set_curvature(handle, live_config.curvature.clamp(0.0, 1.0)) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }

            let lines = lock(&shared.lines).clone();
            let has_visible_text = lines
                .iter()
                .any(|l| !l.source.trim().is_empty() || !l.translated.trim().is_empty());

            if !has_visible_text {
                last_revision = revision;
                last_upload_retry = None;
                consecutive_upload_errors = 0;
                if had_upload_error {
                    had_upload_error = false;
                    set_status(shared, true, "已连接 SteamVR");
                }
                // 淡出后隐藏（由下面的动画循环在 opacity 接近 0 时隐藏）
                target_opacity = 0.0;
            } else {
                let rendered =
                    catch_unwind(AssertUnwindSafe(|| renderer.render(&lines, &live_config)))
                        .map_err(|_| SessionError::Message("渲染字幕时发生异常".to_string()))?;
                last_tex_w = rendered.width.max(1);
                last_tex_h = rendered.height.max(1);
                // texel aspect 保持默认 1.0（方形 texel），面板高度由纹理宽高自动推导；
                // 传整幅纹理的宽高比会让面板被压扁且随行数抖动。
                match overlay.set_raw_data(
                    handle,
                    &rendered.rgba,
                    rendered.width,
                    rendered.height,
                    4,
                ) {
                    Ok(()) => {
                        last_revision = revision;
                        last_upload_retry = None;
                        consecutive_upload_errors = 0;
                        if had_upload_error {
                            had_upload_error = false;
                            set_status(shared, true, "已连接 SteamVR");
                        }
                        if let Err(e) = overlay.set_visibility(handle, true) {
                            if is_fatal_overlay_error(e) {
                                return Err(SessionError::Message(format!(
                                    "显示 Overlay 失败: {e:?}"
                                )));
                            }
                            tracing::warn!("显示 Overlay 失败: {e:?}");
                        }
                        target_opacity = live_config.opacity.clamp(0.0, 1.0);
                    }
                    Err(e) if is_fatal_overlay_error(e) => {
                        return Err(SessionError::Message(format!("上传纹理失败: {e:?}")));
                    }
                    Err(e) => {
                        consecutive_upload_errors += 1;
                        if consecutive_upload_errors >= 10 {
                            return Err(SessionError::Message(format!(
                                "连续上传纹理失败（SteamVR 会话可能已断开）: {e:?}"
                            )));
                        }
                        // 单帧上传失败（SteamVR 瞬时忙）保留未提交的 revision，500ms 后自动重试
                        last_upload_retry = Some(Instant::now());
                        tracing::warn!(
                            "上传纹理失败（{}x{}）: {e:?}",
                            rendered.width,
                            rendered.height
                        );
                        had_upload_error = true;
                        set_status(shared, true, format!("上传纹理失败: {e:?}"));
                    }
                }
            }
        }

        // 120ms 平滑过渡
        let now_anim = Instant::now();
        let dt = now_anim.duration_since(last_animate).as_secs_f32();
        last_animate = now_anim;
        // 静音一段时间后淡出（新字幕会重新点亮）；拖动中保持可见
        if live_config.hide_on_silence_secs > 0 && !dragging_now {
            let silent =
                lock(&shared.last_activity).elapsed().as_secs() >= live_config.hide_on_silence_secs;
            if silent {
                target_opacity = 0.0;
            }
        }

        if (target_opacity - current_opacity).abs() > 0.005 {
            let step = (dt / 0.12).clamp(0.0, 1.0);
            current_opacity += (target_opacity - current_opacity) * step;
            let safe_opacity = if current_opacity.is_finite() {
                current_opacity.clamp(0.0, 1.0)
            } else {
                current_opacity = 0.0;
                0.0
            };
            if let Err(e) = overlay.set_opacity(handle, safe_opacity) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }
        } else if target_opacity <= 0.001 && current_opacity > 0.0 {
            current_opacity = 0.0;
            let _ = overlay.set_opacity(handle, 0.0);
            if let Err(e) = overlay.set_visibility(handle, false) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }
        }

        panel_visible = current_opacity > 0.05 && target_opacity > 0.0;

        let sleep_for = if FASTER_INTERVAL_WHILE_DRAGGING && gesture.is_dragging() {
            DRAGGING_FRAME_INTERVAL
        } else {
            FRAME_INTERVAL
        };
        std::thread::sleep(sleep_for);
    }
}

/// 松手分解 free 会话到位姿字段并 clamp。返回是否触 validate 边界。
fn apply_free_session_to_config(
    config: &mut SteamVrConfig,
    session: &FreeDragSession,
    hmd: Option<&Mat3x4>,
) -> bool {
    let p = session.panel.translation();
    if config.anchor == "world" {
        let (next, hit) = clamp_world_position_checked(p);
        config.world_position = next;
        if config.gesture.orientation != "keep" {
            let yaw_deg = normalize_deg(extract_yaw(&session.panel).to_degrees());
            config.world_rotation_deg[1] = yaw_deg;
        }
        hit
    } else if let Some(hmd_pose) = hmd {
        let (next, hit) = clamp_hmd_offsets_checked(hmd_offsets_from_world(hmd_pose, p));
        config.horizontal_offset_m = next.horizontal_offset_m;
        config.height_offset_m = next.height_offset_m;
        config.distance_m = next.distance_m;
        hit
    } else {
        false
    }
}

fn apply_transform(
    overlay: &mut openvr::Overlay,
    handle: OverlayHandle,
    config: &SteamVrConfig,
) -> Result<(), VROverlayError> {
    if config.anchor == "world" {
        let matrix = world_matrix(config);
        overlay.set_transform_absolute(handle, TrackingUniverseOrigin::Standing, &matrix)
    } else {
        let matrix = hmd_matrix(config);
        overlay.set_transform_tracked_device_relative(handle, tracked_device_index::HMD, &matrix)
    }
}

/// HMD 局部坐标：+X 右、+Y 上、-Z 前；`horizontal_offset_m` 右正。
/// `hmd_face_user` 开启时旋转由 [`super::hmd_local_matrix`] 按偏移偏航面向原点。
fn hmd_matrix(config: &SteamVrConfig) -> Matrix3x4 {
    Matrix3x4(super::hmd_local_matrix(config))
}

fn world_matrix_rows(config: &SteamVrConfig) -> [[f32; 4]; 3] {
    compose_world_rows(
        config.world_position,
        config.world_rotation_deg[1].to_radians(),
    )
    .0
}

/// 站立坐标系：只应用绕 Y 轴的偏航（符合字幕面板的常见用法）
fn world_matrix(config: &SteamVrConfig) -> Matrix3x4 {
    Matrix3x4(world_matrix_rows(config))
}
