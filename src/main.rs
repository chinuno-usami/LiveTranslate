#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod asr;
mod config;
mod error;
mod subtitle;
mod tray;
mod translate;
mod ui;

use clap::Parser;
use config::AppConfig;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use subtitle::SubtitleState;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, Window};
use tokio::sync::{mpsc, watch, Mutex};
use tracing_subscriber::EnvFilter;
use ui::{
    OverlayConfigPayload, StatusPayload, SubtitlePayload, CLICK_THROUGH_EVENT, CONFIG_EVENT,
    ERROR_EVENT, NOTICE_EVENT, STATUS_EVENT, SUBTITLE_EVENT,
};

#[derive(Parser, Debug)]
#[command(name = "LiveTranslate")]
#[command(about = "实时语音识别 + 翻译 + 透明浮窗字幕")]
struct Args {
    /// Config file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// List available audio devices
    #[arg(short, long)]
    list_devices: bool,

    /// List models offered by the configured translation / ASR services
    #[arg(long)]
    list_models: bool,

    /// Run in console mode instead of overlay window
    #[arg(long)]
    console: bool,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

struct PipelineControl {
    /// 流水线世代号，用于避免旧任务的清理逻辑覆盖新流水线
    generation: u64,
    stop_tx: Option<watch::Sender<bool>>,
    runner: Option<tauri::async_runtime::JoinHandle<()>>,
    forwarder: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl PipelineControl {
    fn new() -> Self {
        Self {
            generation: 0,
            stop_tx: None,
            runner: None,
            forwarder: None,
        }
    }

    fn is_running(&self) -> bool {
        self.stop_tx.is_some()
    }
}

pub(crate) struct SharedState {
    config: AppConfig,
    /// 配置文件路径（用于把 UI 上的修改写回）
    config_path: Option<PathBuf>,
    current_device: Mutex<String>,
    subtitle_state: Mutex<SubtitleState>,
    pipeline: Mutex<PipelineControl>,
    /// 当前窗口是否处于点击穿透（托盘/快捷键需要读写它）
    pub(crate) click_through: AtomicBool,
    /// 当前字号（面板滑块可调）
    font_size: AtomicU32,
    /// 是否显示原文
    show_source: AtomicBool,
    /// 识别语言（面板改动可实时生效，无需重启流水线）
    language_tx: watch::Sender<String>,
}

impl SharedState {
    fn new(config: AppConfig, config_path: Option<PathBuf>) -> Self {
        let subtitle_state = SubtitleState::new(
            config.subtitle.max_lines,
            config.subtitle.show_source,
        );

        let device = config.audio.device_name.clone();
        let click_through = config.subtitle.click_through;
        let font_size = config.subtitle.font_size;
        let show_source = config.subtitle.show_source;

        // 两个后端的语言字段不同，先取出再按后端归一化
        let configured_language = if asr::languages::is_edge_backend(&config.asr.backend) {
            config.asr.edge.language.clone()
        } else {
            config.asr.language.clone()
        };
        let language =
            asr::languages::normalize_for_backend(&config.asr.backend, &configured_language);
        let (language_tx, _) = watch::channel(language);

        Self {
            config,
            config_path,
            current_device: Mutex::new(device),
            subtitle_state: Mutex::new(subtitle_state),
            pipeline: Mutex::new(PipelineControl::new()),
            click_through: AtomicBool::new(click_through),
            font_size: AtomicU32::new(font_size),
            show_source: AtomicBool::new(show_source),
            language_tx,
        }
    }
}

/// 切换窗口点击穿透，并同步到前端与托盘
///
/// 点击穿透开启后窗口不再接收鼠标事件，因此托盘菜单与全局快捷键
/// 是关闭穿透的唯一入口。
pub(crate) fn toggle_click_through(app: &AppHandle) {
    let state = app.state::<Arc<SharedState>>();
    let new_value = !state.click_through.load(Ordering::SeqCst);
    apply_click_through(app, new_value);
}

/// 设置窗口点击穿透为指定值
pub(crate) fn apply_click_through(app: &AppHandle, enabled: bool) {
    let state = app.state::<Arc<SharedState>>();
    state.click_through.store(enabled, Ordering::SeqCst);

    if let Some(window) = app.get_window("main") {
        if let Err(e) = window.set_ignore_cursor_events(enabled) {
            tracing::warn!("Failed to set ignore cursor events: {}", e);
        }
    }

    let _ = app.emit_all(CLICK_THROUGH_EVENT, enabled);
    tracing::info!("Click-through set to: {}", enabled);
}

/// 把 UI 上的修改写回配置文件（保留注释）
fn persist_setting(config_path: Option<&PathBuf>, section: &str, key: &str, value: &str) {
    let Some(path) = config_path else {
        tracing::debug!("No config file to persist {section}.{key}");
        return;
    };
    match AppConfig::update_value_in_file(path, section, key, value) {
        Ok(()) => tracing::info!("Persisted [{section}] {key} = {value}"),
        Err(e) => tracing::warn!("Failed to persist [{section}] {key}: {e}"),
    }
}

/// 调整字幕字号（面板滑块）
#[tauri::command]
async fn set_font_size(
    state: State<'_, Arc<SharedState>>,
    size: u32,
) -> Result<(), String> {
    let size = size.clamp(10, 120);
    state.font_size.store(size, Ordering::SeqCst);
    persist_setting(
        state.config_path.as_ref(),
        "subtitle",
        "font_size",
        &size.to_string(),
    );
    Ok(())
}

/// 切换是否显示原文，并立即重绘当前字幕
#[tauri::command]
async fn set_show_source(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    enabled: bool,
) -> Result<(), String> {
    state.show_source.store(enabled, Ordering::SeqCst);

    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.set_show_source(enabled);

        let payload = SubtitlePayload {
            source: String::new(),
            translated: String::new(),
            lines: subtitle_state.get_all(),
            text: subtitle_state.get_text(),
        };
        let _ = app.emit_all(SUBTITLE_EVENT, payload);
    }

    persist_setting(
        state.config_path.as_ref(),
        "subtitle",
        "show_source",
        &enabled.to_string(),
    );
    Ok(())
}

/// 面板上的识别语言候选（按当前后端给出不同的语言码格式）
#[tauri::command]
async fn list_languages(
    state: State<'_, Arc<SharedState>>,
) -> Result<ui::LanguagesPayload, String> {
    let backend = state.config.asr.backend.clone();
    let current = state.language_tx.borrow().clone();

    Ok(ui::LanguagesPayload {
        options: asr::languages::options_for_backend(&backend)
            .into_iter()
            .map(|item| ui::LanguageOptionPayload {
                code: item.code,
                label: item.label,
            })
            .collect(),
        current,
    })
}

/// 切换识别语言（立即生效，并写回配置文件）
#[tauri::command]
async fn set_language(
    state: State<'_, Arc<SharedState>>,
    code: String,
) -> Result<(), String> {
    let backend = state.config.asr.backend.clone();
    let normalized = asr::languages::normalize_for_backend(&backend, &code);

    // watch::Sender::send 仅在“无接收者”时失败，不影响设置本身
    let _ = state.language_tx.send(normalized.clone());

    // 两个后端的语言字段位置不同
    let (section, key) = if asr::languages::is_edge_backend(&backend) {
        ("asr.edge", "language")
    } else {
        ("asr", "language")
    };
    persist_setting(
        state.config_path.as_ref(),
        section,
        key,
        &format!("\"{}\"", normalized.replace('"', "")),
    );

    tracing::info!("ASR language set to: {}", normalized);
    Ok(())
}

#[tauri::command]
async fn get_overlay_config(state: State<'_, Arc<SharedState>>) -> Result<OverlayConfigPayload, String> {
    Ok(OverlayConfigPayload::from(&state.config.subtitle))
}

#[tauri::command]
async fn get_status(state: State<'_, Arc<SharedState>>) -> Result<StatusPayload, String> {
    let pipeline = state.pipeline.lock().await;
    Ok(StatusPayload {
        running: pipeline.is_running(),
        message: if pipeline.is_running() {
            "字幕采集中".to_string()
        } else {
            "未启动".to_string()
        },
    })
}

#[tauri::command]
async fn list_devices_command(state: State<'_, Arc<SharedState>>) -> Result<ui::DevicesPayload, String> {
    let devices = audio::capture::AudioCapture::list_devices().map_err(|e| e.to_string())?;
    let current = state.current_device.lock().await.clone();
    Ok(ui::DevicesPayload {
        devices: devices
            .into_iter()
            .map(|d| ui::DevicePayload {
                spec: d.spec(),
                kind: d.kind_label().to_string(),
                name: d.name,
            })
            .collect(),
        current,
    })
}

#[tauri::command]
async fn set_device(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    spec: String,
) -> Result<(), String> {
    let state = state.inner().clone();
    let was_running = {
        let pipeline = state.pipeline.lock().await;
        pipeline.is_running()
    };

    // 运行中先停止，切换设备后再启动
    if was_running {
        stop_capture_impl(app.clone(), state.clone()).await?;
    }

    {
        let mut current = state.current_device.lock().await;
        *current = spec.clone();
    }

    if was_running {
        start_capture_impl(app.clone(), state.clone()).await?;
    }

    emit_status(&app, was_running, format!("设备已切换: {}", spec));
    Ok(())
}

#[tauri::command]
async fn start_capture(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    start_capture_impl(app, state.inner().clone()).await
}

#[tauri::command]
async fn stop_capture(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    stop_capture_impl(app, state.inner().clone()).await
}

#[tauri::command]
fn close_overlay(window: Window) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

/// 开始拖动窗口
///
/// 用自己的命令而不是 `data-tauri-drag-region`，因为后者走 Window 模块，
/// 受 allowlist / cargo feature 门控；而且它在双击时会触发最大化，
/// 不适合本项目的浮窗场景。
#[tauri::command]
fn start_dragging(window: Window) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

/// 读取窗口当前位置与尺寸（逻辑像素），供前端做自定义边缘缩放
#[tauri::command]
fn window_geometry(window: Window) -> Result<(i32, i32, u32, u32), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    Ok((
        (pos.x as f64 / scale).round() as i32,
        (pos.y as f64 / scale).round() as i32,
        (size.width as f64 / scale).round() as u32,
        (size.height as f64 / scale).round() as u32,
    ))
}

/// 设置窗口位置与尺寸（逻辑像素），供前端自定义边缘缩放
///
/// Tauri v1 没有 `start_resize_dragging`，只能在 JS 里跟踪鼠标位移后反复
/// 调用本命令；尺寸和位置放在同一个命令里设置，避免两次调用间出现撕裂。
#[tauri::command]
fn set_window_bounds(window: Window, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    let width = width.max(1.0);
    let height = height.max(1.0);
    window
        .set_size(tauri::Size::Logical(LogicalSize::new(width, height)))
        .map_err(|e| e.to_string())?;
    window
        .set_position(tauri::Position::Logical(LogicalPosition::new(x, y)))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 手动缩放结束后，把当前窗口尺寸与位置写回配置文件（保留注释）
///
/// 直接读窗口实际几何而非相信前端传值，避免与最后一次 `set_window_bounds`
/// 的异步执行产生竞争。
#[tauri::command]
fn persist_window_geometry(
    state: State<'_, Arc<SharedState>>,
    window: Window,
) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;

    let x = (pos.x as f64 / scale).round() as i32;
    let y = (pos.y as f64 / scale).round() as i32;
    let width = (size.width as f64 / scale).round() as u32;
    let height = (size.height as f64 / scale).round() as u32;

    let path = state.config_path.as_ref();
    persist_setting(path, "subtitle", "window_width", &width.to_string());
    persist_setting(path, "subtitle", "window_height", &height.to_string());
    persist_setting(path, "subtitle", "position_x", &x.to_string());
    persist_setting(path, "subtitle", "position_y", &y.to_string());
    Ok(())
}

#[tauri::command]
async fn set_click_through(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    enabled: bool,
) -> Result<(), String> {
    state.click_through.store(enabled, Ordering::SeqCst);
    apply_click_through(&app, enabled);
    Ok(())
}

async fn start_capture_impl(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    {
        let pipeline = state.pipeline.lock().await;
        if pipeline.is_running() {
            emit_status(&app, true, "字幕采集中");
            return Ok(());
        }
    }

    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.clear();
    }
    emit_subtitle_reset(&app);

    let (event_tx, mut event_rx): (
        mpsc::Sender<app::PipelineEvent>,
        mpsc::Receiver<app::PipelineEvent>,
    ) = mpsc::channel(128);
    let (stop_tx, stop_rx) = watch::channel(false);
    // 语言用独立通道：面板切换可实时生效，不需要重启流水线
    let language_rx = state.language_tx.subscribe();

    let mut config = state.config.clone();
    {
        let current_device = state.current_device.lock().await;
        config.audio.device_name = current_device.clone();
    }

    // 分配新的世代号
    let generation = {
        let mut pipeline = state.pipeline.lock().await;
        pipeline.generation = pipeline.generation.wrapping_add(1);
        pipeline.generation
    };

    let app_for_runner = app.clone();
    let runner = tauri::async_runtime::spawn(async move {
        if let Err(e) = app::run_pipeline(config, event_tx, stop_rx, language_rx).await {
            tracing::error!("Audio processing error: {}", e);
            let _ = app_for_runner.emit_all(ERROR_EVENT, e.to_string());
            emit_status(&app_for_runner, false, format!("运行失败: {}", e));
        }
    });

    let app_for_forwarder = app.clone();
    let state_for_forwarder = state.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                app::PipelineEvent::Subtitle(subtitle) => {
                    let payload = {
                        let mut subtitle_state = state_for_forwarder.subtitle_state.lock().await;
                        subtitle_state.push(subtitle.clone());
                        SubtitlePayload {
                            source: subtitle.source.clone(),
                            translated: subtitle.translated.clone(),
                            lines: subtitle_state.get_all(),
                            text: subtitle_state.get_text(),
                        }
                    };

                    let _ = app_for_forwarder.emit_all(SUBTITLE_EVENT, payload);
                }
                app::PipelineEvent::Notice(message) => {
                    // 把 ASR/翻译/配置问题直接显示到面板，避免“一直等待”却无提示
                    let _ = app_for_forwarder.emit_all(NOTICE_EVENT, message);
                }
            }
        }

        // 仅当仍是当前世代时才清理，避免覆盖刚启动的新流水线
        let mut pipeline = state_for_forwarder.pipeline.lock().await;
        if pipeline.generation == generation {
            emit_status(&app_for_forwarder, false, "字幕流已停止");
            pipeline.stop_tx = None;
            pipeline.runner = None;
            pipeline.forwarder = None;
        }
    });

    {
        let mut pipeline = state.pipeline.lock().await;
        pipeline.stop_tx = Some(stop_tx);
        pipeline.runner = Some(runner);
        pipeline.forwarder = Some(forwarder);
    }

    emit_status(&app, true, "字幕采集中");
    Ok(())
}

async fn stop_capture_impl(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    let (stop_tx, runner, forwarder) = {
        let mut pipeline = state.pipeline.lock().await;
        (
            pipeline.stop_tx.take(),
            pipeline.runner.take(),
            pipeline.forwarder.take(),
        )
    };

    if let Some(stop_tx) = stop_tx {
        let _ = stop_tx.send(true);
    }

    if let Some(runner) = runner {
        let _ = runner.await;
    }

    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }

    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.clear();
    }

    emit_subtitle_reset(&app);
    emit_status(&app, false, "已停止");
    Ok(())
}

fn emit_status(app: &AppHandle, running: bool, message: impl Into<String>) {    let _ = app.emit_all(
        STATUS_EVENT,
        StatusPayload {
            running,
            message: message.into(),
        },
    );
}

fn emit_subtitle_reset(app: &AppHandle) {
    let _ = app.emit_all(
        SUBTITLE_EVENT,
        SubtitlePayload {
            source: String::new(),
            translated: String::new(),
            lines: Vec::new(),
            text: String::new(),
        },
    );
}

/// 在系统文件管理器中打开用户配置目录
///
/// 同时在目录不存在时先创建它，方便用户首次直接编辑。
pub(crate) fn open_config_dir() -> Result<(), String> {
    let dir = AppConfig::user_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";

    std::process::Command::new(opener)
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("Failed to open {}: {}", dir.display(), e))?;

    tracing::info!("Opened config directory: {}", dir.display());
    Ok(())
}

fn apply_window_config(window: &Window, config: &AppConfig) -> Result<(), String> {
    window
        .set_size(tauri::Size::Logical(LogicalSize::new(
            config.subtitle.window_width as f64,
            config.subtitle.window_height as f64,
        )))
        .map_err(|e| e.to_string())?;

    window
        .set_position(tauri::Position::Logical(LogicalPosition::new(
            config.subtitle.position_x as f64,
            config.subtitle.position_y as f64,
        )))
        .map_err(|e| e.to_string())?;

    window
        .set_always_on_top(config.subtitle.always_on_top)
        .map_err(|e| e.to_string())?;

    let _ = window.set_ignore_cursor_events(config.subtitle.click_through);
    Ok(())
}

/// 选择第一个可写的日志文件位置
///
/// 依次尝试：用户日志目录 -> 可执行文件同级 -> 系统临时目录。
/// 返回 `(日志文件路径, 目录, 文件名)`。
fn resolve_log_target() -> Option<(PathBuf, PathBuf, String)> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(dir) = AppConfig::log_dir() {
        candidates.push(dir.join("livetranslate.log"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("livetranslate.log"));
        }
    }
    candidates.push(std::env::temp_dir().join("livetranslate.log"));

    for path in candidates {
        let parent = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        if std::fs::create_dir_all(&parent).is_err() {
            continue;
        }
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .is_ok()
        {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "livetranslate.log".to_string());
            return Some((path, parent, file_name));
        }
    }
    None
}

fn init_logging(level: &str) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    // 控制台输出（从终端启动或重定向时可见）
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    // 文件输出：双击运行（GUI 子系统、无控制台）时唯一的可诊断手段
    if let Some((log_path, log_dir, file_name)) = resolve_log_target() {
        let appender = tracing_appender::rolling::never(&log_dir, file_name);
        let (writer, guard) = tracing_appender::non_blocking(appender);

        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(true);

        let subscriber = tracing_subscriber::registry()
            .with(env_filter)
            .with(console_layer)
            .with(file_layer);

        if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
            eprintln!("failed to set tracing subscriber: {e}");
        }
        eprintln!("log file: {}", log_path.display());
        return Some(guard);
    }

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer);
    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("failed to set tracing subscriber: {e}");
    }
    None
}

/// Windows: `windows_subsystem = "windows"` 不分配控制台，
/// 但从 cmd/PowerShell 启动时，附着到父进程控制台就能直接看到日志。
#[cfg(target_os = "windows")]
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_parent_console() {}

/// 捕获 panic 并记录到日志；Windows 下额外弹窗提示
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        default_hook(info);

        #[cfg(target_os = "windows")]
        {
            static SHOWN: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
                show_error_dialog("LiveTranslate 崩溃", &format!("{info}"));
            }
        }
    }));
}

/// Windows: 弹原生错误对话框，避免 GUI 子系统下错误被静默吞掉
#[cfg(target_os = "windows")]
fn show_error_dialog(title: &str, message: &str) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let text = HSTRING::from(message);
    let caption = HSTRING::from(title);
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_error_dialog(_title: &str, _message: &str) {}

fn main() {
    // 先附着父控制台（Windows），否则 GUI 子系统下看不到任何输出
    attach_parent_console();

    let args = Args::parse();
    let _log_guard = init_logging(&args.log_level);
    install_panic_hook();

    tracing::info!("==== LiveTranslate starting ====");
    tracing::info!("version: {}", env!("CARGO_PKG_VERSION"));

    if let Err(e) = run(args) {
        let message = format!("{e:#}");
        tracing::error!("Fatal error: {message}");
        show_error_dialog("LiveTranslate 启动失败", &message);
        std::process::exit(1);
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    tracing::info!("Starting LiveTranslate Application");

    // ONNX Runtime 动态库探测必须在创建任何线程/异步运行时之前完成：
    // 它会写入进程级环境变量 ORT_DYLIB_PATH，多线程下与 env 读取并发存在 UB 风险。
    audio::detector::prepare_ort_library();

    let (config, config_path) = match AppConfig::resolve(args.config.clone()) {
        Ok(resolved) => resolved,
        Err(e) => {
            tracing::error!("Failed to load config: {}", e);
            tracing::warn!("Falling back to built-in default configuration");
            (AppConfig::default(), None)
        }
    };

    match &config_path {
        Some(path) => tracing::info!("Config file: {}", path.display()),
        None => {
            let hint = AppConfig::user_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string());
            tracing::warn!("Using built-in defaults. 建议在该位置创建配置文件: {}", hint);
        }
    }
    tracing::debug!("Config: {:?}", config);

    if args.list_devices {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::list_devices())?;
        return Ok(());
    }

    if args.list_models {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::list_models(&config))?;
        return Ok(());
    }

    if args.console {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::start_console(config))?;
        return Ok(());
    }

    let shared = Arc::new(SharedState::new(config.clone(), config_path.clone()));

    tauri::Builder::default()
        .system_tray(tray::create_system_tray())
        .on_system_tray_event(tray::handle_system_tray_event)
        .manage(shared)
        .setup(move |app| {
            // macOS: 未打包为 .app 直接运行二进制时，应用不会被自动“激活”，
            // 会导致窗口收不到鼠标/键盘事件（并伴随 IMKCFRunLoopWakeUpReliable 报错）。
            // 显式设为 Regular 激活策略，让它作为前台应用运行。
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Regular);
            }

            let window = app.get_window("main").ok_or_else(|| anyhow::anyhow!("main window not found"))?;

            // 以下步骤均“尽力而为”，失败不应阻止应用启动
            if let Err(e) = apply_window_config(&window, &config) {
                tracing::warn!("Failed to apply window config: {e}");
            }
            if let Err(e) = window.emit(CONFIG_EVENT, OverlayConfigPayload::from(&config.subtitle)) {
                tracing::warn!("Failed to emit config event: {e}");
            }
            if let Err(e) = tray::register_global_shortcuts(&app.handle()) {
                tracing::warn!("Failed to register global shortcuts (可能被其他程序占用): {e}");
            }

            // 确保窗口可见并获取焦点
            let _ = window.show();
            let _ = window.set_focus();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_overlay_config,
            set_font_size,
            set_show_source,
            list_languages,
            set_language,
            get_status,
            list_devices_command,
            set_device,
            start_capture,
            stop_capture,
            close_overlay,
            start_dragging,
            window_geometry,
            set_window_bounds,
            persist_window_geometry,
            set_click_through,
        ])
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri runtime error: {e}"))?;

    Ok(())
}
