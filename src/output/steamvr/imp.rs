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

    fn stop_thread(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    pub fn apply_config(&mut self, config: &SteamVrConfig) {
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
        } else {
            self.stop_thread();
        }
    }

    pub fn publish(&self, lines: &[DisplayLine]) {
        if let Ok(mut current) = self.shared.lines.lock() {
            *current = lines.to_vec();
        }
        if !lines.is_empty() {
            if let Ok(mut last) = self.shared.last_activity.lock() {
                *last = Instant::now();
            }
        }
        self.shared.revision.fetch_add(1, Ordering::SeqCst);
    }

    pub fn status(&self) -> SteamVrStatus {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn shutdown(&mut self) {
        self.stop_thread();
    }
}

impl Drop for SteamVrSink {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

fn set_status(shared: &Shared, connected: bool, message: impl Into<String>) {
    if let Ok(mut status) = shared.status.lock() {
        status.connected = connected;
        status.message = message.into();
    }
}

fn run(shared: Arc<Shared>) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let interval = shared
            .config
            .lock()
            .map(|c| c.reconnect_interval_secs.max(1))
            .unwrap_or(5);

        let result = catch_unwind(AssertUnwindSafe(|| session(&shared)));
        if shared.stop.load(Ordering::SeqCst) {
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

        let config = shared
            .config
            .lock()
            .map(|c| c.clone())
            .map_err(|_| "配置锁失效".to_string())?;
        let revision = shared.revision.load(Ordering::SeqCst);

        if revision != last_revision {
            last_revision = revision;
            apply_transform(&mut overlay, handle, &config)?;
            let _ = overlay.set_width(handle, config.width_m.clamp(0.3, 5.0));
            let _ = overlay.set_curvature(handle, config.curvature.clamp(0.0, 1.0));

            let lines = shared
                .lines
                .lock()
                .map(|l| l.clone())
                .map_err(|_| "字幕锁失效".to_string())?;

            if lines.is_empty() {
                // 淡出后隐藏（由下面的动画循环在 opacity 接近 0 时隐藏）
                target_opacity = 0.0;
            } else {
                let rendered = catch_unwind(AssertUnwindSafe(|| renderer.render(&lines, &config)))
                    .map_err(|_| "渲染字幕时发生异常".to_string())?;
                let aspect = rendered.width as f32 / rendered.height.max(1) as f32;
                let _ = overlay.set_texel_aspect(handle, aspect);
                overlay
                    .set_raw_data(handle, &rendered.rgba, rendered.width, rendered.height, 4)
                    .map_err(|e| format!("上传纹理失败: {e:?}"))?;
                if let Err(e) = overlay.set_visibility(handle, true) {
                    return Err(format!("显示 Overlay 失败: {e:?}"));
                }
                target_opacity = config.opacity.clamp(0.0, 1.0);
            }
        }

        // 120ms 平滑过渡
        let now = Instant::now();
        let dt = now.duration_since(last_animate).as_secs_f32();
        last_animate = now;
        // 静音一段时间后淡出（新字幕会重新点亮）
        if config.hide_on_silence_secs > 0 {
            let silent = shared
                .last_activity
                .lock()
                .map(|t| t.elapsed().as_secs() >= config.hide_on_silence_secs)
                .unwrap_or(false);
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
