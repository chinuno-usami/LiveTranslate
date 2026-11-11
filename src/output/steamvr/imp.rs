//! Windows + steamvr feature 的真实实现。
//!
//! 线程模型：tokio 侧只更新共享快照并递增 revision；VR 线程各 33ms 检查一次，
//! 内容变化才重新渲染/上传，因此字幕事件不会跨线程拷贝两次纹理。

use super::render;
use super::SteamVrStatus;
use crate::config::SteamVrConfig;
use crate::subtitle::DisplayLine;
use openvr::overlay::OverlayHandle;
use openvr::pose::Matrix3x4;
use openvr::tracked_device_index;
use openvr::{ApplicationType, TrackingUniverseOrigin};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const OVERLAY_KEY: &str = "com.chinuno.livetranslate.subtitle";
const OVERLAY_NAME: &str = "LiveTranslate Subtitle";
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

struct Shared {
    lines: Mutex<Vec<DisplayLine>>,
    config: Mutex<SteamVrConfig>,
    status: Mutex<SteamVrStatus>,
    revision: AtomicU64,
    stop: AtomicBool,
    last_activity: Mutex<Instant>,
}

pub struct SteamVrSink {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl SteamVrSink {
    pub fn new(config: &SteamVrConfig) -> Self {
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
            stop: AtomicBool::new(false),
            last_activity: Mutex::new(Instant::now()),
        });

        let mut sink = Self {
            shared,
            handle: None,
        };
        if config.enabled {
            sink.start();
        }
        sink
    }

    fn start(&mut self) {
        if self.handle.is_some() {
            return;
        }
        self.shared.stop.store(false, Ordering::SeqCst);
        let shared = self.shared.clone();
        self.handle = std::thread::Builder::new()
            .name("steamvr-overlay".to_string())
            .spawn(move || run(shared))
            .ok();
    }

    /// 请求 VR 线程退出并取走句柄。
    ///
    /// **不在调用处 join**：`join()` 可能卡在 `openvr::init()` 里任意长时间，
    /// 而调用方（`mod.rs` 的外层 Mutex）被持有时，热路径的 `publish` 会一起阻塞。
    fn take_thread(&mut self) -> Option<JoinHandle<()>> {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.handle.take()
    }

    pub fn apply_config(&mut self, config: &SteamVrConfig) -> Option<JoinHandle<()>> {
        if let Ok(mut current) = self.shared.config.lock() {
            *current = config.clone();
        }
        if let Ok(mut status) = self.shared.status.lock() {
            status.enabled = config.enabled;
            if !config.enabled {
                status.connected = false;
                status.message = "未启用".to_string();
            }
        }
        self.shared.revision.fetch_add(1, Ordering::SeqCst);

        if config.enabled {
            self.start();
            None
        } else {
            self.take_thread()
        }
    }

    pub fn publish(&self, lines: &[DisplayLine]) {
        // 33ms 的 VR 线程可能正拿着这把锁，这里不能无限等待热路径
        let stored = match self.shared.lines.try_lock() {
            Ok(mut current) => {
                *current = lines.to_vec();
                true
            }
            Err(_) => false,
        };
        if !lines.is_empty() {
            *lock(&self.shared.last_activity) = Instant::now();
        }
        // 快照被丢掉时就别递增 revision：VR 线程会把「上一份内容」当成已渲染，
        // 导致最新一条字幕再也不出现，直到下一次 ASR 事件。
        if stored {
            self.shared.revision.fetch_add(1, Ordering::SeqCst);
        }
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
    let mut status = lock(&shared.status);
    status.connected = connected;
    status.message = message.into();
}

fn run(shared: Arc<Shared>) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let interval = lock(&shared.config).reconnect_interval_secs.max(1);

        let result = catch_unwind(AssertUnwindSafe(|| session(&shared)));
        if shared.stop.load(Ordering::SeqCst) {
            // 主动停止：不要再写“已断开”，否则会覆盖刚设置的“未启用”
            break;
        }
        match result {
            Ok(Ok(())) => set_status(&shared, false, "SteamVR 已断开，等待重连"),
            Ok(Err(e)) => set_status(&shared, false, format!("SteamVR 不可用: {e}")),
            Err(_) => set_status(&shared, false, "SteamVR 初始化异常，等待重连"),
        }

        // 分片睡眠，保证退出时能快速 join
        let mut slept = Duration::ZERO;
        while slept < Duration::from_secs(interval) && !shared.stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            slept += Duration::from_millis(100);
        }
    }
}

fn init_context() -> Result<openvr::Context, String> {
    if !openvr::is_runtime_installed() {
        return Err("未检测到 SteamVR 运行时".to_string());
    }
    unsafe { openvr::init(ApplicationType::Overlay) }.map_err(|e| format!("{e:?}"))
}

fn session(shared: &Arc<Shared>) -> Result<(), String> {
    let context = init_context()?;
    let mut overlay = context.overlay().map_err(|e| format!("{e:?}"))?;
    let handle: OverlayHandle = overlay
        .create_overlay(OVERLAY_KEY, OVERLAY_NAME)
        .map_err(|e| format!("创建 Overlay 失败: {e:?}"))?;
    set_status(shared, true, "已连接 SteamVR");

    let mut last_revision = u64::MAX;
    let mut renderer = render::Renderer::new();
    let mut current_opacity = 0.0f32;
    let mut target_opacity = 0.0f32;
    let mut last_animate = Instant::now();

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            let _ = overlay.set_visibility(handle, false);
            return Ok(());
        }

        let config = lock(&shared.config).clone();
        let revision = shared.revision.load(Ordering::SeqCst);

        if revision != last_revision {
            last_revision = revision;
            apply_transform(&mut overlay, handle, &config)?;
            let _ = overlay.set_width(handle, config.width_m.clamp(0.3, 5.0));
            let _ = overlay.set_curvature(handle, config.curvature.clamp(0.0, 1.0));

            let lines = lock(&shared.lines).clone();

            if lines.is_empty() {
                // 淡出后隐藏（由下面的动画循环在 opacity 接近 0 时隐藏）
                target_opacity = 0.0;
            } else {
                let rendered = catch_unwind(AssertUnwindSafe(|| renderer.render(&lines, &config)))
                    .map_err(|_| "渲染字幕时发生异常".to_string())?;
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
                        // 单帧上传失败（尺寸超限 / SteamVR 瞬时忙）不该拆掉整个会话，
                        // 跳过这一帧、保留上一帧纹理即可
                        if let Err(e) = overlay.set_visibility(handle, true) {
                            tracing::warn!("显示 Overlay 失败: {e:?}");
                        }
                        target_opacity = config.opacity.clamp(0.0, 1.0);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "上传纹理失败（{}x{}）: {e:?}",
                            rendered.width,
                            rendered.height
                        );
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
            let _ = overlay.set_opacity(handle, current_opacity.clamp(0.0, 1.0));
        } else if target_opacity <= 0.001 && current_opacity > 0.0 {
            current_opacity = 0.0;
            let _ = overlay.set_opacity(handle, 0.0);
            let _ = overlay.set_visibility(handle, false);
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
