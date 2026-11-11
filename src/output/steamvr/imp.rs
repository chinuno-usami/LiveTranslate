//! Windows + steamvr feature 的真实实现。
//!
//! 线程模型：tokio 侧只更新共享快照并递增 revision；VR 线程各 33ms 检查一次，
//! 内容变化才重新渲染/上传，因此字幕事件不会跨线程拷贝两次纹理。

use super::render;
use super::SteamVrStatus;
use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use openvr::overlay::{OverlayHandle, VROverlayError};
use openvr::pose::Matrix3x4;
use openvr::system::Event;
use openvr::tracked_device_index;
use openvr::{ApplicationType, InitError, TrackingUniverseOrigin};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// openvr 0.9 直接把 &str 裸指针传给 C 的 CreateOverlay，末尾必须自带 '\0'
const OVERLAY_KEY: &str = "com.chinuno.livetranslate.subtitle\0";
const OVERLAY_NAME: &str = "LiveTranslate Subtitle\0";
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

struct Shared {
    lines: Mutex<Vec<DisplayLine>>,
    config: Mutex<SteamVrConfig>,
    status: Mutex<SteamVrStatus>,
    revision: AtomicU64,
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
            }),
            revision: AtomicU64::new(1),
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
            if !config.enabled {
                status.connected = false;
                status.message = "未启用".to_string();
            }
        }
        if config.enabled {
            // 调整设置或实时预览时刷新活动时间，让已因静音淡出的面板重新显现
            *lock(&self.shared.last_activity) = Instant::now();
        }
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
        self.shared.revision.fetch_add(1, Ordering::SeqCst);
    }

    pub fn status(&self) -> SteamVrStatus {
        lock(&self.shared.status).clone()
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

fn init_context() -> Result<openvr::Context, SessionError> {
    if !openvr::is_runtime_installed() {
        return Err(SessionError::Message("未检测到 SteamVR 运行时".to_string()));
    }
    // 使用 Background 模式：只附着到已运行的 SteamVR，不在桌面场景下主动拉起 vrserver
    unsafe { openvr::init(ApplicationType::Background) }.map_err(|err| match err {
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

fn session(
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    renderer: &mut render::Renderer,
) -> Result<(), SessionError> {
    let context = init_context()?;
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
    set_status(shared, true, "已连接 SteamVR");

    let mut last_revision = u64::MAX;
    let mut current_opacity = 0.0f32;
    let mut target_opacity = 0.0f32;
    let mut last_animate = Instant::now();
    let mut had_upload_error = false;

    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = overlay.set_visibility(handle, false);
            return Ok(());
        }

        while let Some(info) = system.poll_next_event() {
            if matches!(info.event, Event::Quit(_) | Event::DriverRequestedQuit) {
                system.acknowledge_quit_exiting();
                return Ok(());
            }
        }

        let config = lock(&shared.config).clone();
        let revision = shared.revision.load(Ordering::SeqCst);

        if revision != last_revision {
            last_revision = revision;
            apply_transform(&mut overlay, handle, &config).map_err(SessionError::Message)?;
            if let Err(e) = overlay.set_width(handle, config.width_m.clamp(0.3, 5.0)) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }
            if let Err(e) = overlay.set_curvature(handle, config.curvature.clamp(0.0, 1.0)) {
                if is_fatal_overlay_error(e) {
                    return Err(SessionError::Message(format!("Overlay 会话失效: {e:?}")));
                }
            }

            let lines = lock(&shared.lines).clone();

            if lines.is_empty() {
                // 淡出后隐藏（由下面的动画循环在 opacity 接近 0 时隐藏）
                target_opacity = 0.0;
            } else {
                let rendered = catch_unwind(AssertUnwindSafe(|| renderer.render(&lines, &config)))
                    .map_err(|_| SessionError::Message("渲染字幕时发生异常".to_string()))?;
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
                        target_opacity = config.opacity.clamp(0.0, 1.0);
                    }
                    Err(e) if is_fatal_overlay_error(e) => {
                        return Err(SessionError::Message(format!("上传纹理失败: {e:?}")));
                    }
                    Err(e) => {
                        // 单帧上传失败（尺寸超限 / SteamVR 瞬时忙）跳过本帧、保留上一帧纹理
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
        let now = Instant::now();
        let dt = now.duration_since(last_animate).as_secs_f32();
        last_animate = now;
        // 静音一段时间后淡出（新字幕会重新点亮）
        if config.hide_on_silence_secs > 0 {
            let silent =
                lock(&shared.last_activity).elapsed().as_secs() >= config.hide_on_silence_secs;
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

        std::thread::sleep(FRAME_INTERVAL);
    }
}

fn apply_transform(
    overlay: &mut openvr::Overlay,
    handle: OverlayHandle,
    config: &SteamVrConfig,
) -> Result<(), String> {
    if config.anchor == "world" {
        let matrix = world_matrix(config);
        overlay
            .set_transform_absolute(handle, TrackingUniverseOrigin::Standing, &matrix)
            .map_err(|e| format!("设置世界坐标失败: {e:?}"))
    } else {
        let matrix = hmd_matrix(config);
        overlay
            .set_transform_tracked_device_relative(handle, tracked_device_index::HMD, &matrix)
            .map_err(|e| format!("设置头部相对坐标失败: {e:?}"))
    }
}

/// HMD 局部坐标：-Z 为前方
fn hmd_matrix(config: &SteamVrConfig) -> Matrix3x4 {
    Matrix3x4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, config.height_offset_m],
        [0.0, 0.0, 1.0, -config.distance_m],
    ])
}

/// 站立坐标系：只应用绕 Y 轴的偏航（符合字幕面板的常见用法）
fn world_matrix(config: &SteamVrConfig) -> Matrix3x4 {
    let yaw = config.world_rotation_deg[1].to_radians();
    let (sin, cos) = yaw.sin_cos();
    let [x, y, z] = config.world_position;
    Matrix3x4([[cos, 0.0, sin, x], [0.0, 1.0, 0.0, y], [-sin, 0.0, cos, z]])
}
