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
use std::sync::Arc;
use subtitle::SubtitleState;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, Window};
use tokio::sync::{mpsc, watch, Mutex};
use tracing_subscriber::EnvFilter;
use ui::{
    OverlayConfigPayload, StatusPayload, SubtitlePayload, CONFIG_EVENT, ERROR_EVENT, STATUS_EVENT,
    SUBTITLE_EVENT,
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

struct SharedState {
    config: AppConfig,
    current_device: Mutex<String>,
    subtitle_state: Mutex<SubtitleState>,
    pipeline: Mutex<PipelineControl>,
}

impl SharedState {
    fn new(config: AppConfig) -> Self {
        let subtitle_state = SubtitleState::new(
            config.subtitle.max_lines,
            config.subtitle.show_source,
        );

        let device = config.audio.device_name.clone();

        Self {
            config,
            current_device: Mutex::new(device),
            subtitle_state: Mutex::new(subtitle_state),
            pipeline: Mutex::new(PipelineControl::new()),
        }
    }
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

#[tauri::command]
fn set_click_through(window: Window, enabled: bool) -> Result<(), String> {
    window
        .set_ignore_cursor_events(enabled)
        .map_err(|e| e.to_string())
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

    let (subtitle_tx, mut subtitle_rx): (mpsc::Sender<subtitle::Subtitle>, mpsc::Receiver<subtitle::Subtitle>) = mpsc::channel(128);
    let (stop_tx, stop_rx) = watch::channel(false);

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
        if let Err(e) = app::run_pipeline(config, subtitle_tx, stop_rx).await {
            tracing::error!("Audio processing error: {}", e);
            let _ = app_for_runner.emit_all(ERROR_EVENT, e.to_string());
            emit_status(&app_for_runner, false, format!("运行失败: {}", e));
        }
    });

    let app_for_forwarder = app.clone();
    let state_for_forwarder = state.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(subtitle) = subtitle_rx.recv().await {
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

fn emit_status(app: &AppHandle, running: bool, message: impl Into<String>) {
    let _ = app.emit_all(
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

fn init_logging(level: &str) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(true)
        .init();
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_logging(&args.log_level);

    tracing::info!("Starting LiveTranslate Application");

    let config = match AppConfig::load(args.config.clone()) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!("Failed to load config: {}", e);
            tracing::info!("Using default configuration");
            AppConfig::default()
        }
    };

    if args.list_devices {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::list_devices())?;
        return Ok(());
    }

    if args.console {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::start_console(config))?;
        return Ok(());
    }

    let shared = Arc::new(SharedState::new(config.clone()));

    tauri::Builder::default()
        .system_tray(tray::create_system_tray())
        .on_system_tray_event(tray::handle_system_tray_event)
        .manage(shared)
        .setup(move |app| {
            let window = app.get_window("main").ok_or_else(|| anyhow::anyhow!("main window not found"))?;
            apply_window_config(&window, &config).map_err(anyhow::Error::msg)?;
            window.emit(CONFIG_EVENT, OverlayConfigPayload::from(&config.subtitle)).ok();
            tray::register_global_shortcuts(&app.handle()).map_err(|e| anyhow::anyhow!("Failed to register shortcuts: {}", e))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_overlay_config,
            get_status,
            list_devices_command,
            set_device,
            start_capture,
            stop_capture,
            close_overlay,
            set_click_through,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    Ok(())
}
