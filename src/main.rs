#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod asr;
mod audio;
mod config;
mod error;
mod output;
mod subtitle;
mod translate;
mod tray;
mod ui;

use clap::Parser;
use config::{profiles, schema, writer, AppConfig, AudioSourceConfig};
use output::OutputManager;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use subtitle::SubtitleState;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, Window};
use tokio::sync::{mpsc, watch, Mutex};
use tracing_subscriber::EnvFilter;
use ui::{
    ActiveSourcePayload, CapabilitiesPayload, OverlayConfigPayload, SaveResult, SourceStatusItem,
    SourcesPayload, StatusPayload, SubtitlePayload, CLICK_THROUGH_EVENT, CONFIG_EVENT, ERROR_EVENT,
    NOTICE_EVENT, OUTPUT_STATUS_EVENT, SETTINGS_HIDDEN_EVENT, SETTINGS_SHOWN_EVENT,
    SOURCE_STATUS_EVENT, STATUS_EVENT, SUBTITLE_EVENT,
};

#[derive(Parser, Debug)]
#[command(name = "LiveTranslate")]
#[command(about = "实时语音识别 + 翻译 + 透明浮窗字幕")]
struct Args {
    /// Config file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(short, long)]
    list_devices: bool,

    #[arg(long)]
    list_models: bool,

    #[arg(long)]
    console: bool,

    #[arg(long, default_value = "info")]
    log_level: String,
}

struct PipelineControl {
    generation: u64,
    stop_tx: Option<watch::Sender<bool>>,
    supervisor: Option<tauri::async_runtime::JoinHandle<()>>,
    forwarder: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl PipelineControl {
    fn new() -> Self {
        Self {
            generation: 0,
            stop_tx: None,
            supervisor: None,
            forwarder: None,
        }
    }

    fn is_running(&self) -> bool {
        self.stop_tx.is_some()
    }
}

/// 每个输入源的运行时句柄
struct SourceRuntime {
    id: String,
    label: String,
    device: String,
    enabled: bool,
    running: bool,
    message: String,
    /// 该源的识别语言覆盖；空 = 跟随全局
    language_override: String,
    /// 该源自己的翻译偏好（与全局开关取与）
    translate_pref: bool,
    translate_tx: watch::Sender<bool>,
    /// 保活接收端：没有接收者时 `watch::Sender::send` 会失败
    translate_rx: watch::Receiver<bool>,
    language_tx: watch::Sender<String>,
    _language_rx: watch::Receiver<String>,
}

impl SourceRuntime {
    fn new(source: &AudioSourceConfig, translate_enabled: bool, language: String) -> Self {
        let (translate_tx, translate_rx) = watch::channel(source.translate && translate_enabled);
        let (language_tx, language_rx) = watch::channel(language);
        Self {
            id: source.id.clone(),
            label: source.label.clone(),
            device: source.device.clone(),
            enabled: source.enabled,
            running: false,
            message: if source.enabled {
                "未启动".to_string()
            } else {
                "已停用".to_string()
            },
            language_override: source.language.clone(),
            translate_pref: source.translate,
            translate_tx,
            translate_rx,
            language_tx,
            _language_rx: language_rx,
        }
    }

    fn translate_enabled(&self) -> bool {
        *self.translate_rx.borrow()
    }

    fn context(&self) -> app::SourceContext {
        app::SourceContext {
            id: self.id.clone(),
            label: self.label.clone(),
            device: self.device.clone(),
            language: self.language_tx.subscribe(),
            translate: self.translate_tx.subscribe(),
        }
    }
}

struct SourceRegistry {
    runtimes: Vec<SourceRuntime>,
}

impl SourceRegistry {
    fn from_config(config: &AppConfig) -> Self {
        let global_language = global_language(config);
        let runtimes = config
            .audio
            .effective_sources(config.translate.enabled)
            .iter()
            .map(|source| {
                let language = if source.language.trim().is_empty() {
                    global_language.clone()
                } else {
                    asr::languages::normalize_for_backend(&config.asr.backend, &source.language)
                };
                SourceRuntime::new(source, config.translate.enabled, language)
            })
            .collect();
        Self { runtimes }
    }

    /// 配置热更新：同步源列表、翻译开关、语言，不重启流水线
    fn sync_config(&mut self, config: &AppConfig) {
        let global_language = global_language(config);
        let effective = config.audio.effective_sources(config.translate.enabled);
        let ids: Vec<String> = effective.iter().map(|s| s.id.clone()).collect();

        for source in &effective {
            let language = if source.language.trim().is_empty() {
                global_language.clone()
            } else {
                asr::languages::normalize_for_backend(&config.asr.backend, &source.language)
            };
            if let Some(runtime) = self.runtimes.iter_mut().find(|r| r.id == source.id) {
                runtime.label = source.label.clone();
                runtime.device = source.device.clone();
                runtime.enabled = source.enabled;
                if !runtime.running {
                    runtime.message = if source.enabled {
                        "未启动".to_string()
                    } else {
                        "已停用".to_string()
                    };
                }
                runtime.language_override = source.language.clone();
                runtime.translate_pref = source.translate;
                let _ = runtime
                    .translate_tx
                    .send(source.translate && config.translate.enabled);
                let _ = runtime.language_tx.send(language);
            } else {
                self.runtimes.push(SourceRuntime::new(
                    source,
                    config.translate.enabled,
                    language,
                ));
            }
        }

        self.runtimes.retain(|r| ids.contains(&r.id));
        // 保持配置顺序
        self.runtimes
            .sort_by_key(|r| ids.iter().position(|id| id == &r.id).unwrap_or(usize::MAX));
    }

    fn set_translate(&mut self, id: &str, enabled: bool) -> bool {
        if let Some(runtime) = self.runtimes.iter_mut().find(|r| r.id == id) {
            let _ = runtime.translate_tx.send(enabled);
            return true;
        }
        false
    }

    fn set_translate_all(&mut self, enabled: bool) {
        // 全局开关只在“该源自身打开翻译”时生效，
        // 否则会把用户在浮窗里单独关掉的源重新打开。
        for runtime in &mut self.runtimes {
            let _ = runtime.translate_tx.send(runtime.translate_pref && enabled);
        }
    }

    fn set_language(&mut self, config: &AppConfig, language: &str) {
        let global = asr::languages::normalize_for_backend(&config.asr.backend, language);
        for runtime in &mut self.runtimes {
            let value = if runtime.language_override.trim().is_empty() {
                global.clone()
            } else {
                asr::languages::normalize_for_backend(
                    &config.asr.backend,
                    &runtime.language_override,
                )
            };
            let _ = runtime.language_tx.send(value);
        }
    }

    fn set_running(&mut self, id: &str, running: bool, message: impl Into<String>) {
        if let Some(runtime) = self.runtimes.iter_mut().find(|r| r.id == id) {
            runtime.running = running;
            runtime.message = message.into();
        }
    }

    fn mark_all_stopped(&mut self) {
        for runtime in &mut self.runtimes {
            runtime.running = false;
            if runtime.enabled {
                runtime.message = "已停止".to_string();
            }
        }
    }

    fn active_labels(&self) -> Vec<String> {
        self.runtimes
            .iter()
            .filter(|r| r.running)
            .map(|r| r.label.clone())
            .collect()
    }

    fn active_sources_payload(&self) -> Vec<ActiveSourcePayload> {
        self.runtimes
            .iter()
            .filter(|r| r.running)
            .map(|r| ActiveSourcePayload {
                id: r.id.clone(),
                label: r.label.clone(),
                translate: r.translate_enabled(),
            })
            .collect()
    }

    fn running_count(&self) -> usize {
        self.runtimes.iter().filter(|r| r.running).count()
    }

    fn status_items(&self) -> Vec<SourceStatusItem> {
        self.runtimes
            .iter()
            .map(|r| SourceStatusItem {
                id: r.id.clone(),
                label: r.label.clone(),
                running: r.running,
                enabled: r.enabled,
                translate: r.translate_pref,
                device: r.device.clone(),
                message: r.message.clone(),
            })
            .collect()
    }
}

pub(crate) struct SharedState {
    /// 由只读改为 RwLock：设置页保存后立即生效
    config: RwLock<AppConfig>,
    config_path: RwLock<Option<PathBuf>>,
    subtitle_state: Mutex<SubtitleState>,
    pipeline: Mutex<PipelineControl>,
    /// 串行化启动/停止/切换配置档
    lifecycle: Mutex<()>,
    /// 串行化「读 → 改 → 写回」的配置变更命令
    ///
    /// `config` 本身是 RwLock，但单独读写两步的序列仍会丢失更新（A 读→B 读→A 写→B 写）。
    config_update: Mutex<()>,
    sources: Mutex<SourceRegistry>,
    pub(crate) outputs: Arc<OutputManager>,
    pub(crate) click_through: AtomicBool,
    font_size: AtomicU32,
    show_source: AtomicBool,
    /// 保证关闭浮窗、托盘退出、快捷键退出与 RunEvent::Exit 最多只执行一次停机清理
    shutting_down: AtomicBool,
    /// 浮窗拖动位置持久化的防抖序号
    move_seq: AtomicU64,
    /// 全局语言
    language_tx: watch::Sender<String>,
    _language_rx: watch::Receiver<String>,
    /// 全局翻译开关
    translate_tx: watch::Sender<bool>,
    _translate_rx: watch::Receiver<bool>,
    /// 启动阶段的配置回退/修复提示，在主界面就绪后展示一次
    startup_warning: std::sync::Mutex<Option<String>>,
    /// 磁盘配置加载失败并回退到默认值：显式保存前禁止自动镜像覆盖配置档，并在设置页透出告警
    config_degraded: AtomicBool,
    degraded_warning: std::sync::Mutex<Option<String>>,
}

/// 字幕历史保留上限：桌面浮窗与 SteamVR 的 max_lines 独立，取较大者
fn history_limit(config: &AppConfig) -> usize {
    config
        .subtitle
        .max_lines
        .max(config.output.steamvr.max_lines)
        .max(config.output.osc.max_lines)
        .max(1)
}

fn desktop_entries(
    mut entries: Vec<subtitle::DisplayLine>,
    config: &AppConfig,
) -> Vec<subtitle::DisplayLine> {
    let limit = config.subtitle.max_lines.max(1);
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries
}

fn global_language(config: &AppConfig) -> String {
    let configured = if asr::languages::is_edge_backend(&config.asr.backend) {
        config.asr.edge.language.clone()
    } else {
        config.asr.language.clone()
    };
    asr::languages::normalize_for_backend(&config.asr.backend, &configured)
}

impl SharedState {
    fn new(
        config: AppConfig,
        config_path: Option<PathBuf>,
        startup_warning: Option<String>,
        config_degraded: bool,
    ) -> Self {
        let subtitle_state = SubtitleState::new(
            history_limit(&config),
            config.subtitle.show_source,
            config.subtitle.label_sources,
        );
        let click_through = config.subtitle.click_through;
        let font_size = config.subtitle.font_size;
        let show_source = config.subtitle.show_source;
        let language = global_language(&config);
        let (language_tx, language_rx) = watch::channel(language);
        let (translate_tx, translate_rx) = watch::channel(config.translate.enabled);
        let outputs = OutputManager::new(&config.output);
        let sources = SourceRegistry::from_config(&config);
        let degraded_warning = if config_degraded {
            startup_warning.clone()
        } else {
            None
        };

        Self {
            config: RwLock::new(config),
            config_path: RwLock::new(config_path),
            subtitle_state: Mutex::new(subtitle_state),
            pipeline: Mutex::new(PipelineControl::new()),
            lifecycle: Mutex::new(()),
            config_update: Mutex::new(()),
            sources: Mutex::new(sources),
            outputs,
            click_through: AtomicBool::new(click_through),
            font_size: AtomicU32::new(font_size),
            show_source: AtomicBool::new(show_source),
            shutting_down: AtomicBool::new(false),
            move_seq: AtomicU64::new(0),
            language_tx,
            _language_rx: language_rx,
            translate_tx,
            _translate_rx: translate_rx,
            startup_warning: std::sync::Mutex::new(startup_warning),
            config_degraded: AtomicBool::new(config_degraded),
            degraded_warning: std::sync::Mutex::new(degraded_warning),
        }
    }

    fn take_startup_warning(&self) -> Option<String> {
        self.startup_warning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    fn is_config_degraded(&self) -> bool {
        self.config_degraded.load(Ordering::SeqCst)
    }

    fn clear_config_degraded(&self) {
        self.config_degraded.store(false, Ordering::SeqCst);
        *self
            .degraded_warning
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn degraded_warning(&self) -> Option<String> {
        self.degraded_warning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 设置页表单基准配置：当磁盘文件仅因字段校验未通过而进入降级态时，
    /// 直接从磁盘读取原结构体供设置页展示与编辑，避免表单显示内存默认值导致无法修正。
    fn settings_base_config(&self) -> (AppConfig, Option<String>) {
        if !self.is_config_degraded() {
            return (self.config(), None);
        }
        if let Some(path) = self.config_path() {
            match AppConfig::from_file(&path) {
                Ok(disk_cfg) => {
                    let warning = disk_cfg
                        .validate()
                        .err()
                        .map(|e| format!("配置文件 {} 存在待修正项：{e}", path.display()))
                        .or_else(|| self.degraded_warning());
                    return (disk_cfg, warning);
                }
                Err(e) => {
                    let warning = self.degraded_warning().unwrap_or_else(|| {
                        format!(
                            "配置文件 {} 解析失败（{e}），请切到「高级 / 原始配置」修复后再保存",
                            path.display()
                        )
                    });
                    return (self.config(), Some(warning));
                }
            }
        }
        (self.config(), self.degraded_warning())
    }

    fn config(&self) -> AppConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_config(&self, config: AppConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    fn config_path(&self) -> Option<PathBuf> {
        self.config_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_config_path(&self, path: Option<PathBuf>) {
        *self.config_path.write().unwrap_or_else(|e| e.into_inner()) = path;
    }

    /// 原子地读改写配置，并保证写回时已校验通过
    async fn update_config<F>(&self, mutate: F) -> Result<AppConfig, String>
    where
        F: FnOnce(&mut AppConfig) -> Result<(), String>,
    {
        let _guard = self.config_update.lock().await;
        let mut config = self.config();
        mutate(&mut config)?;
        config.validate()?;
        self.set_config(config.clone());
        Ok(config)
    }
}

/// 切换窗口点击穿透，并同步到前端、持久配置与托盘
pub(crate) fn toggle_click_through(app: &AppHandle) {
    let state = app.state::<Arc<SharedState>>();
    let new_value = !state.click_through.load(Ordering::SeqCst);
    apply_click_through(app, new_value);

    // 必须写回配置：否则下一次 apply_window_config（窗口尺寸变化时）
    // 会用配置里的旧值把穿透改回去，而前端仍然显示“穿透开”。
    let state = state.inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match state
            .update_config(|config| {
                config.subtitle.click_through = new_value;
                Ok(())
            })
            .await
        {
            Ok(config) => {
                persist_value(&app, &state, "subtitle", "click_through", new_value.into());
                emit_config(&app, &config);
            }
            Err(e) => tracing::warn!("切换点击穿透失败: {e}"),
        }
    });
}

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

pub(crate) fn open_settings_window(app: &AppHandle, group: Option<&str>) {
    match app.get_window("settings") {
        Some(window) => {
            let was_visible = window.is_visible().unwrap_or(false);
            // 先显示再通知，避免前端的确认框挂在一个还没显示的窗口上。
            // 仅在由隐藏转为显示（或显式指定了跳转分组）时通知前端。
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
            if !was_visible || group.is_some() {
                let payload = serde_json::json!({
                    "was_visible": was_visible,
                    "group": group,
                });
                let _ = app.emit_all(SETTINGS_SHOWN_EVENT, payload);
            }
        }
        None => tracing::warn!("Settings window not found in tauri.conf.json"),
    }
}

pub(crate) fn start_capture_bg(app: &AppHandle) {
    let app_handle = app.clone();
    let state = app.state::<Arc<SharedState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start_capture_impl(app_handle.clone(), state).await {
            tracing::error!("Failed to start capture: {e}");
            let _ = app_handle.emit_all(ERROR_EVENT, e);
        }
    });
}

pub(crate) fn stop_capture_bg(app: &AppHandle) {
    let app_handle = app.clone();
    let state = app.state::<Arc<SharedState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = stop_capture_impl(app_handle.clone(), state).await {
            tracing::error!("Failed to stop capture: {e}");
            let _ = app_handle.emit_all(ERROR_EVENT, e);
        }
    });
}

async fn shutdown_once(app: &AppHandle) {
    let state = app.state::<Arc<SharedState>>().inner().clone();
    if state.shutting_down.swap(true, Ordering::SeqCst) {
        return;
    }
    let stop = stop_capture_impl(app.clone(), state.clone());
    if tokio::time::timeout(std::time::Duration::from_secs(3), stop)
        .await
        .is_err()
    {
        tracing::warn!("停止采集超时，继续退出");
    }
    state.outputs.shutdown_async().await;
    state.outputs.notify_status();
}

pub(crate) fn quit_app(app: &AppHandle) {
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        shutdown_once(&app_handle).await;
        app_handle.exit(0);
    });
}

/// 确保有可写的配置文件（内置默认配置时首次保存到用户目录）
fn ensure_config_path(state: &SharedState) -> Result<PathBuf, String> {
    if let Some(path) = state.config_path() {
        return Ok(path);
    }
    if state.is_config_degraded() {
        return Err(
            "当前配置文件解析失败并处于回退保护状态，请先修复原配置文件后再保存".to_string(),
        );
    }
    let path = AppConfig::user_config_path().map_err(|e| e.to_string())?;
    AppConfig::write_default_if_absent(&path).map_err(|e| e.to_string())?;
    state.set_config_path(Some(path.clone()));
    Ok(path)
}

/// 把一批配置项一次性写回文件（尽量保留注释）
fn persist_fields(state: &SharedState, fields: &[(&str, serde_json::Value)]) -> Result<(), String> {
    let config_path = ensure_config_path(state)?;
    let payload: Vec<(String, serde_json::Value)> = fields
        .iter()
        .map(|(path, value)| ((*path).to_string(), value.clone()))
        .collect();
    writer::apply_changes(&config_path, &payload)?;
    mirror_profile(state);
    Ok(())
}

/// 写入失败时同时在日志与界面错误条上告知用户
///
/// 只 `warn!` 的话，内存里已经生效的改动会在重启后静默消失，
/// 用户以为保存成功了。
fn persist_or_report(app: &AppHandle, state: &SharedState, fields: &[(&str, serde_json::Value)]) {
    if let Err(e) = persist_fields(state, fields) {
        tracing::warn!("Failed to persist config: {e}");
        let _ = app.emit_all(ERROR_EVENT, format!("配置未能写入磁盘: {e}"));
    }
}

fn persist_value(
    app: &AppHandle,
    state: &SharedState,
    section: &str,
    key: &str,
    value: serde_json::Value,
) {
    persist_or_report(app, state, &[(&format!("{section}.{key}"), value)]);
}

fn persist_sources(state: &SharedState, config: &AppConfig) -> Result<(), String> {
    let sources = serde_json::to_value(&config.audio.sources)
        .map_err(|e| format!("序列化输入源失败: {e}"))?;
    persist_fields(state, &[("audio.sources", sources)])
}

fn mirror_profile(state: &SharedState) {
    if state.is_config_degraded() {
        return;
    }
    let Some(config_path) = state.config_path() else {
        return;
    };
    if let Err(e) = profiles::mirror_active(&config_path) {
        tracing::warn!("Failed to mirror active profile: {e}");
    }
}

fn emit_config(app: &AppHandle, config: &AppConfig) {
    let payload = OverlayConfigPayload::from_config(config);
    let _ = app.emit_all(CONFIG_EVENT, payload);
}

fn emit_output_status(app: &AppHandle, state: &SharedState) {
    let _ = app.emit_all(OUTPUT_STATUS_EVENT, state.outputs.status());
}

async fn emit_source_status(app: &AppHandle, state: &SharedState) {
    let registry = state.sources.lock().await;
    let _ = app.emit_all(
        SOURCE_STATUS_EVENT,
        SourcesPayload {
            sources: registry.status_items(),
            max_active_sources: state.config().audio.max_active_sources,
        },
    );
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
    let _ = app.emit_all(SUBTITLE_EVENT, SubtitlePayload::empty());
}

async fn emit_subtitle_with_config(app: &AppHandle, state: &SharedState, config: &AppConfig) {
    let registry = state.sources.lock().await;
    let active = registry.active_sources_payload();
    drop(registry);
    let subtitle_state = state.subtitle_state.lock().await;
    // 历史长度取各输出的最大值，但浮窗只能按 subtitle.max_lines 显示，
    // 否则切一下「显示来源」就会把 SteamVR/OSC 的多余行泄到浮窗上。
    let entries = desktop_entries(subtitle_state.display_lines(), config);
    let _ = app.emit_all(SUBTITLE_EVENT, SubtitlePayload::new(&entries, active));
}

async fn emit_subtitle(app: &AppHandle, state: &SharedState) {
    let config = state.config();
    emit_subtitle_with_config(app, state, &config).await;
}

/// 将重新格式化后的当前字幕快照同步给外部输出（SteamVR / OSC）
async fn publish_subtitle_snapshot(state: &SharedState) {
    let lines = state.subtitle_state.lock().await.display_lines();
    state.outputs.publish(&lines);
}

/// 应用热更新字段（样式 / 输出 / 翻译开关 / 语言 / 窗口属性）
async fn apply_hot_config(app: &AppHandle, state: &SharedState, old: &AppConfig, new: &AppConfig) {
    // 字幕状态
    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.set_max_lines(history_limit(new));
        subtitle_state.set_show_source(new.subtitle.show_source);
        subtitle_state.set_label_mode(new.subtitle.label_sources);
    }
    state
        .font_size
        .store(new.subtitle.font_size, Ordering::SeqCst);
    state
        .show_source
        .store(new.subtitle.show_source, Ordering::SeqCst);

    // 输出
    state.outputs.apply_config(&new.output).await;
    publish_subtitle_snapshot(state).await;

    // 翻译开关 / 语言
    let _ = state.translate_tx.send(new.translate.enabled);
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(new);
        if !new.translate.enabled {
            registry.set_translate_all(false);
        }
    }
    if old.asr.backend != new.asr.backend
        || old.asr.language != new.asr.language
        || old.asr.edge.language != new.asr.edge.language
    {
        let language = global_language(new);
        let _ = state.language_tx.send(language.clone());
        let mut registry = state.sources.lock().await;
        registry.set_language(new, &language);
    }

    // 窗口属性
    if let Some(window) = app.get_window("main") {
        if old.subtitle.always_on_top != new.subtitle.always_on_top {
            let _ = window.set_always_on_top(new.subtitle.always_on_top);
        }
        if old.subtitle.click_through != new.subtitle.click_through {
            apply_click_through(app, new.subtitle.click_through);
        }
        if old.subtitle.window_width != new.subtitle.window_width
            || old.subtitle.window_height != new.subtitle.window_height
            || old.subtitle.position_x != new.subtitle.position_x
            || old.subtitle.position_y != new.subtitle.position_y
        {
            if let Err(e) = apply_window_config(&window, new) {
                tracing::warn!("Failed to apply window config: {e}");
            }
        }
    }

    emit_config(app, new);
    emit_subtitle(app, state).await;
    emit_source_status(app, state).await;
    emit_output_status(app, state);
}

// ---------------- 配置档辅助 ----------------

fn profiles_status(state: &SharedState) -> profiles::ProfilesStatus {
    profiles::list_status(state.config_path().as_deref())
}

// ---------------- 生命周期 ----------------

async fn start_capture_impl(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    let _lifecycle = state.lifecycle.lock().await;
    start_capture_locked(app, state.clone()).await
}

async fn start_capture_locked(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    {
        let pipeline = state.pipeline.lock().await;
        if pipeline.is_running() {
            emit_status(&app, true, "字幕采集中");
            return Ok(());
        }
    }

    let config = state.config();
    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.clear();
    }
    emit_subtitle_reset(&app);

    // 计算本次要启动的源（并发上限 + 设备去重）
    let mut contexts = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(&config);
        let max = config.audio.max_active_sources.max(1);
        let mut used_devices = std::collections::HashSet::new();
        for runtime in &mut registry.runtimes {
            if !runtime.enabled {
                runtime.running = false;
                runtime.message = "已停用".to_string();
                continue;
            }
            if contexts.len() >= max {
                runtime.running = false;
                runtime.message = format!("超出并发上限（{max}），未启动");
                skipped.push((runtime.id.clone(), runtime.message.clone()));
                continue;
            }
            if !runtime.device.trim().is_empty() && !used_devices.insert(runtime.device.clone()) {
                runtime.running = false;
                runtime.message = "设备重复，已跳过".to_string();
                skipped.push((runtime.id.clone(), runtime.message.clone()));
                continue;
            }
            runtime.running = true;
            runtime.message = "启动中".to_string();
            contexts.push(runtime.context());
        }
        let labels = registry.active_labels();
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.set_active_labels(labels);
    }

    if contexts.is_empty() {
        emit_status(&app, false, "没有可用的输入源");
        if let Some(warning) = state.take_startup_warning() {
            let _ = app.emit_all(ERROR_EVENT, warning);
        }
        emit_source_status(&app, &state).await;
        return Ok(());
    }

    state.outputs.set_capture_active(true);
    let active_count = contexts.len();

    let (event_tx, mut event_rx): (
        mpsc::Sender<app::PipelineEvent>,
        mpsc::Receiver<app::PipelineEvent>,
    ) = mpsc::channel(128);
    let (stop_tx, stop_rx) = watch::channel(false);

    emit_status(&app, true, format!("{active_count} 源采集中"));
    // 提示放在 emit_status 之后发送，避免刚发出的告警被状态文案立刻覆盖
    if !skipped.is_empty() {
        let message = skipped
            .iter()
            .map(|(id, reason)| format!("{id}: {reason}"))
            .collect::<Vec<_>>()
            .join("；");
        let _ = app.emit_all(NOTICE_EVENT, format!("部分输入源未启动 — {message}"));
    }
    if config.vad.enabled && config.vad.backend == "silero" && contexts.len() > 2 {
        let _ = app.emit_all(
            NOTICE_EVENT,
            format!(
                "已启用 {} 个 Silero VAD 输入源，CPU 占用可能较高；可降低 max_active_sources",
                contexts.len()
            ),
        );
    }
    if let Some(warning) = state.take_startup_warning() {
        let _ = app.emit_all(ERROR_EVENT, warning);
    }
    emit_source_status(&app, &state).await;

    let mut pipeline = state.pipeline.lock().await;
    pipeline.generation = pipeline.generation.wrapping_add(1);
    let generation = pipeline.generation;
    // 停止时要能取消各源任务，否则超时后它们会脱管并继续占用音频设备
    let app_for_runner = app.clone();
    let supervisor = tauri::async_runtime::spawn(async move {
        let mut set = tokio::task::JoinSet::new();
        for context in contexts {
            let source_config = config.clone();
            let source_tx = event_tx.clone();
            let source_stop = stop_rx.clone();
            let app = app_for_runner.clone();
            set.spawn(async move {
                if let Err(e) =
                    app::run_source(source_config, context, source_tx, source_stop).await
                {
                    tracing::error!("Audio processing error: {}", e);
                    let _ = app.emit_all(ERROR_EVENT, e.to_string());
                }
            });
        }
        drop(event_tx);
        // 子任务可能在同步收尾里卡住（设备/驱动），给个上限：
        // 超时就 abort 全部子任务并让用户知道“设备可能仍被占用”，
        // 而不是让 stop_capture_locked 报成功、紧接着重开采集时设备冲突。
        let drained = tokio::time::timeout(std::time::Duration::from_secs(6), async {
            while let Some(joined) = set.join_next().await {
                if let Err(e) = joined {
                    tracing::error!("Source task ended abnormally: {e}");
                }
            }
        })
        .await;
        if drained.is_err() {
            tracing::warn!("Some source tasks did not stop in time; aborting them");
            set.abort_all();
            let _ = app_for_runner.emit_all(
                NOTICE_EVENT,
                "部分输入源未能在 6 秒内停止，已强制中断（设备可能仍被占用）",
            );
        }
    });

    let app_for_forwarder = app.clone();
    let state_for_forwarder = state.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                app::PipelineEvent::Subtitle(line) => {
                    // 锁顺序统一为 sources -> subtitle，避免与 SourceState 分支互相等待
                    let (labels, active) = {
                        let registry = state_for_forwarder.sources.lock().await;
                        (registry.active_labels(), registry.active_sources_payload())
                    };
                    let display = {
                        let mut subtitle_state = state_for_forwarder.subtitle_state.lock().await;
                        subtitle_state.set_active_labels(labels);
                        subtitle_state.push(line);
                        subtitle_state.display_lines()
                    };
                    let config = state_for_forwarder.config();
                    let desktop = desktop_entries(display.clone(), &config);
                    let payload = SubtitlePayload::new(&desktop, active);
                    state_for_forwarder.outputs.publish(&display);
                    let _ = app_for_forwarder.emit_all(SUBTITLE_EVENT, payload);
                }
                app::PipelineEvent::Notice { source_id, message } => {
                    let label = {
                        let registry = state_for_forwarder.sources.lock().await;
                        source_id
                            .as_ref()
                            .and_then(|id| registry.runtimes.iter().find(|r| &r.id == id))
                            .map(|r| r.label.clone())
                    };
                    let text = match label {
                        Some(label) => format!("[{label}] {message}"),
                        None => message,
                    };
                    let _ = app_for_forwarder.emit_all(NOTICE_EVENT, text);
                }
                app::PipelineEvent::SourceState {
                    source_id,
                    running,
                    message,
                } => {
                    {
                        let mut registry = state_for_forwarder.sources.lock().await;
                        registry.set_running(&source_id, running, message.clone());
                        let labels = registry.active_labels();
                        let mut subtitle_state = state_for_forwarder.subtitle_state.lock().await;
                        subtitle_state.set_active_labels(labels);
                    }
                    emit_source_status(&app_for_forwarder, &state_for_forwarder).await;
                }
            }
        }

        // 仅当仍是当前世代时才清理
        let mut pipeline = state_for_forwarder.pipeline.lock().await;
        if pipeline.generation == generation {
            let mut registry = state_for_forwarder.sources.lock().await;
            registry.mark_all_stopped();
            drop(registry);
            state_for_forwarder.outputs.set_capture_active(false);
            emit_status(&app_for_forwarder, false, "字幕流已停止");
            pipeline.stop_tx = None;
            pipeline.supervisor = None;
            pipeline.forwarder = None;
            drop(pipeline);
            emit_source_status(&app_for_forwarder, &state_for_forwarder).await;
            emit_output_status(&app_for_forwarder, &state_for_forwarder);
        }
    });

    pipeline.stop_tx = Some(stop_tx);
    pipeline.supervisor = Some(supervisor);
    pipeline.forwarder = Some(forwarder);
    emit_output_status(&app, &state);
    Ok(())
}

async fn stop_capture_impl(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    let _lifecycle = state.lifecycle.lock().await;
    stop_capture_locked(app, state.clone()).await
}

async fn stop_capture_locked(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    let (stop_tx, supervisor, forwarder) = {
        let mut pipeline = state.pipeline.lock().await;
        (
            pipeline.stop_tx.take(),
            pipeline.supervisor.take(),
            pipeline.forwarder.take(),
        )
    };

    if let Some(stop_tx) = stop_tx {
        let _ = stop_tx.send(true);
    }
    if let Some(mut supervisor) = supervisor {
        match tokio::time::timeout(std::time::Duration::from_secs(8), &mut supervisor).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("Source supervisor joined with error: {e}"),
            Err(_) => {
                // 超时就取消：supervisor 持有 JoinSet，abort 会连带取消所有源任务，
                // 否则它们会脱管并继续占用音频设备（重开采集时设备冲突）。
                tracing::warn!("Source supervisor did not stop in time; aborting source tasks");
                supervisor.abort();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), supervisor).await;
            }
        }
    }
    if let Some(mut forwarder) = forwarder {
        // supervisor 已 join，发送端全关，但通道里可能还排着已识别完的字幕。
        // 直接 abort 会让最后一两句既不上屏也不进 OSC/SteamVR。
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut forwarder)
            .await
            .is_err()
        {
            tracing::warn!("Subtitle forwarder did not finish in time; aborting");
            forwarder.abort();
        }
    }

    {
        let mut registry = state.sources.lock().await;
        registry.mark_all_stopped();
    }
    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.clear();
    }
    state.outputs.set_capture_active(false);

    emit_subtitle_reset(&app);
    emit_status(&app, false, "已停止");
    emit_source_status(&app, &state).await;
    emit_output_status(&app, &state);
    Ok(())
}

// ---------------- 前端命令：查询 ----------------

#[tauri::command]
async fn get_overlay_config(
    state: State<'_, Arc<SharedState>>,
) -> Result<OverlayConfigPayload, String> {
    let config = state.config();
    Ok(OverlayConfigPayload::from_config(&config))
}

#[tauri::command]
async fn get_status(state: State<'_, Arc<SharedState>>) -> Result<StatusPayload, String> {
    let pipeline = state.pipeline.lock().await;
    let running = pipeline.is_running();
    drop(pipeline);
    if !running {
        return Ok(StatusPayload {
            running: false,
            message: "未启动".to_string(),
        });
    }
    let registry = state.sources.lock().await;
    let count = registry.running_count();
    Ok(StatusPayload {
        running: true,
        message: match count {
            0 => "正在启动".to_string(),
            1 => "字幕采集中".to_string(),
            n => format!("{n} 源采集中"),
        },
    })
}

#[tauri::command]
async fn list_languages(
    state: State<'_, Arc<SharedState>>,
) -> Result<ui::LanguagesPayload, String> {
    let config = state.config();
    let current = state.language_tx.borrow().clone();
    Ok(ui::LanguagesPayload {
        options: asr::languages::options_for_backend(&config.asr.backend)
            .into_iter()
            .map(|item| ui::LanguageOptionPayload {
                code: item.code,
                label: item.label,
            })
            .collect(),
        current,
    })
}

#[tauri::command]
async fn list_devices_command(
    state: State<'_, Arc<SharedState>>,
) -> Result<ui::DevicesPayload, String> {
    let devices = audio::capture::AudioCapture::list_devices().map_err(|e| e.to_string())?;
    let config = state.config();
    let current = config
        .audio
        .effective_sources(config.translate.enabled)
        .first()
        .map(|s| s.device.clone())
        .unwrap_or_default();
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
async fn list_sources(state: State<'_, Arc<SharedState>>) -> Result<SourcesPayload, String> {
    let registry = state.sources.lock().await;
    Ok(SourcesPayload {
        sources: registry.status_items(),
        max_active_sources: state.config().audio.max_active_sources,
    })
}

#[tauri::command]
async fn get_sources_config(
    state: State<'_, Arc<SharedState>>,
) -> Result<serde_json::Value, String> {
    let (config, _) = state.settings_base_config();
    Ok(serde_json::json!({
        "sources": config.audio.effective_sources(config.translate.enabled),
        "max_active_sources": config.audio.max_active_sources,
    }))
}

#[tauri::command]
async fn get_output_status(
    state: State<'_, Arc<SharedState>>,
) -> Result<output::OutputStatusPayload, String> {
    Ok(state.outputs.status())
}

#[tauri::command]
async fn get_capabilities() -> Result<CapabilitiesPayload, String> {
    let (steamvr, osc) = output::capabilities();
    Ok(CapabilitiesPayload { steamvr, osc })
}

#[tauri::command]
async fn get_settings_schema(
    state: State<'_, Arc<SharedState>>,
) -> Result<schema::SettingsSchema, String> {
    let (config, warning) = state.settings_base_config();
    Ok(schema::build_with_warning(&config, warning))
}

#[tauri::command]
async fn get_config_raw(state: State<'_, Arc<SharedState>>) -> Result<serde_json::Value, String> {
    let path = state.config_path();
    let text = match &path {
        Some(path) => writer::read_text(path)?,
        None => String::new(),
    };
    Ok(serde_json::json!({
        "path": path.map(|p| p.display().to_string()),
        "text": text,
    }))
}

#[tauri::command]
async fn get_profiles(
    state: State<'_, Arc<SharedState>>,
) -> Result<profiles::ProfilesStatus, String> {
    Ok(profiles_status(&state))
}

// ---------------- 前端命令：面板热更新 ----------------

#[tauri::command]
async fn set_font_size(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    size: u32,
) -> Result<(), String> {
    let size = size.clamp(12, 72);
    state.font_size.store(size, Ordering::SeqCst);
    let config = state
        .update_config(|config| {
            config.subtitle.font_size = size;
            Ok(())
        })
        .await?;
    persist_value(&app, &state, "subtitle", "font_size", size.into());
    emit_config(&app, &config);
    Ok(())
}

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
    }
    let config = state
        .update_config(|config| {
            config.subtitle.show_source = enabled;
            Ok(())
        })
        .await?;
    persist_value(&app, &state, "subtitle", "show_source", enabled.into());
    publish_subtitle_snapshot(&state).await;
    emit_config(&app, &config);
    emit_subtitle(&app, &state).await;
    Ok(())
}

#[tauri::command]
async fn set_translate_enabled(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    enabled: bool,
) -> Result<(), String> {
    let config = state
        .update_config(|config| {
            config.translate.enabled = enabled;
            Ok(())
        })
        .await?;
    let _ = state.translate_tx.send(enabled);
    {
        let mut registry = state.sources.lock().await;
        registry.set_translate_all(enabled);
    }
    persist_value(&app, &state, "translate", "enabled", enabled.into());
    emit_config(&app, &config);
    emit_source_status(&app, &state).await;
    tracing::info!(
        "Mode set to: {}",
        if enabled {
            "ASR + translate"
        } else {
            "ASR only"
        }
    );
    Ok(())
}

#[tauri::command]
async fn set_language(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    code: String,
) -> Result<(), String> {
    let mut resolved_lang = String::new();
    let mut resolved_section = "asr";
    let config = state
        .update_config(|config| {
            let normalized = asr::languages::normalize_for_backend(&config.asr.backend, &code);
            if normalized.is_empty()
                || normalized.len() > 32
                || !normalized
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(format!("非法语言代码: {code}"));
            }
            if asr::languages::is_edge_backend(&config.asr.backend) {
                resolved_section = "asr.edge";
                config.asr.edge.language = normalized.clone();
            } else {
                resolved_section = "asr";
                config.asr.language = normalized.clone();
            }
            resolved_lang = normalized;
            Ok(())
        })
        .await?;

    let _ = state.language_tx.send(resolved_lang.clone());
    {
        let mut registry = state.sources.lock().await;
        registry.set_language(&config, &resolved_lang);
    }
    persist_value(
        &app,
        &state,
        resolved_section,
        "language",
        resolved_lang.clone().into(),
    );
    emit_config(&app, &config);
    tracing::info!("ASR language set to: {}", resolved_lang);
    Ok(())
}

#[tauri::command]
async fn set_device(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    spec: String,
) -> Result<(), String> {
    let spec = spec.trim().to_string();
    if spec.is_empty() {
        return Err("设备名不能为空".to_string());
    }
    let inner = state.inner().clone();
    let _lifecycle = inner.lifecycle.lock().await;
    let was_running = inner.pipeline.lock().await.is_running();
    let config = inner
        .update_config(|config| {
            if config.audio.sources.is_empty() {
                config.audio.sources = config.audio.effective_sources(config.translate.enabled);
            }
            if let Some(first) = config.audio.sources.first_mut() {
                first.device = spec.clone();
            }
            // 兼容旧版本的单源字段（sources 非空时 effective_sources 会忽略它）
            config.audio.device_name = spec.clone();
            Ok(())
        })
        .await?;
    let sources = serde_json::to_value(&config.audio.sources)
        .map_err(|e| format!("序列化输入源失败: {e}"))?;
    persist_or_report(
        &app,
        &inner,
        &[
            ("audio.device_name", spec.into()),
            ("audio.sources", sources),
        ],
    );
    {
        let mut registry = inner.sources.lock().await;
        registry.sync_config(&config);
    }
    // 只在本就在采集时重启：用户只是换了下设备就自己开麦会让人意外
    if was_running {
        stop_capture_locked(app.clone(), inner.clone()).await?;
        start_capture_locked(app, inner.clone()).await
    } else {
        emit_source_status(&app, &inner).await;
        Ok(())
    }
}

#[tauri::command]
async fn set_source_translate(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let config = {
        let _guard = state.config_update.lock().await;
        let mut next = state.config();
        if next.audio.sources.is_empty() {
            next.audio.sources = next.audio.effective_sources(next.translate.enabled);
        }
        let Some(source) = next.audio.sources.iter_mut().find(|s| s.id == id) else {
            return Err(format!("输入源不存在: {id}"));
        };
        source.translate = enabled;
        next.validate()?;
        // 先落盘校验成功再更新内存与运行态，避免磁盘失败时内存与 SourceRegistry 失步
        persist_sources(&state, &next)?;
        state.set_config(next.clone());
        next
    };
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(&config);
        registry.set_translate(&id, enabled && config.translate.enabled);
    }
    emit_source_status(&app, &state).await;
    Ok(())
}

async fn mutate_sources<F>(app: AppHandle, state: Arc<SharedState>, mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut AppConfig) -> Result<(), String>,
{
    let _lifecycle = state.lifecycle.lock().await;
    let was_running = state.pipeline.lock().await.is_running();

    let config = {
        let _guard = state.config_update.lock().await;
        let mut next = state.config();
        if next.audio.sources.is_empty() {
            next.audio.sources = next.audio.effective_sources(next.translate.enabled);
        }
        mutate(&mut next)?;
        next.validate()?;
        persist_sources(&state, &next)?;
        state.set_config(next.clone());
        next
    };
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(&config);
    }

    if was_running {
        stop_capture_locked(app.clone(), state.clone()).await?;
        start_capture_locked(app.clone(), state.clone()).await?;
    } else {
        emit_source_status(&app, &state).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_source_enabled(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        let source = config
            .audio
            .sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("输入源不存在: {id}"))?;
        source.enabled = enabled;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_source_device(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    id: String,
    spec: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        let source = config
            .audio
            .sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("输入源不存在: {id}"))?;
        source.device = spec;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn add_source(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    source: AudioSourceConfig,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        if config.audio.sources.iter().any(|s| s.id == source.id) {
            return Err(format!("输入源 id 重复: {}", source.id));
        }
        config.audio.sources.push(source);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn update_source(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    source: AudioSourceConfig,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        let item = config
            .audio
            .sources
            .iter_mut()
            .find(|s| s.id == source.id)
            .ok_or_else(|| format!("输入源不存在: {}", source.id))?;
        *item = source;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn remove_source(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        let before = config.audio.sources.len();
        config.audio.sources.retain(|s| s.id != id);
        if config.audio.sources.len() == before {
            return Err(format!("输入源不存在: {id}"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn reorder_sources(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    ids: Vec<String>,
) -> Result<(), String> {
    let inner = state.inner().clone();
    mutate_sources(app, inner, |config| {
        let mut ordered = Vec::with_capacity(config.audio.sources.len());
        for id in &ids {
            if let Some(pos) = config.audio.sources.iter().position(|s| &s.id == id) {
                ordered.push(config.audio.sources.remove(pos));
            }
        }
        ordered.append(&mut config.audio.sources);
        config.audio.sources = ordered;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_output_enabled(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    kind: String,
    enabled: bool,
) -> Result<(), String> {
    let (cap_steamvr, cap_osc) = output::capabilities();
    if enabled && ((kind == "steamvr" && !cap_steamvr) || (kind == "osc" && !cap_osc)) {
        return Err("当前构建未包含该输出，请下载 Full 版".to_string());
    }

    let section = match kind.as_str() {
        "steamvr" => "output.steamvr",
        "osc" => "output.osc",
        other => return Err(format!("未知输出类型: {other}")),
    };
    let config = state
        .update_config(|config| {
            match kind.as_str() {
                "steamvr" => config.output.steamvr.enabled = enabled,
                _ => config.output.osc.enabled = enabled,
            }
            Ok(())
        })
        .await?;
    persist_value(&app, &state, section, "enabled", enabled.into());
    state.outputs.apply_config(&config.output).await;
    if enabled {
        publish_subtitle_snapshot(&state).await;
    }
    emit_output_status(&app, &state);
    Ok(())
}

// ---------------- 设置页保存 ----------------

#[derive(Debug, Deserialize)]
struct SettingChange {
    path: String,
    value: serde_json::Value,
}

fn apply_settings_changes(
    config: &mut AppConfig,
    changes: &[SettingChange],
) -> Result<bool, String> {
    let (cap_steamvr, cap_osc) = output::capabilities();
    // 白名单：只接受 schema 里声明过的点分路径。
    // 否则一个粗粒度的路径（如 "output"）就能整表替换，绕过上面的风味门控。
    let allowed: std::collections::HashSet<String> = schema::build(config)
        .groups
        .iter()
        .flat_map(|group| group.fields.iter())
        .map(|field| field.path.trim_end_matches("[]").to_string())
        .collect();
    let mut restart = false;
    for change in changes {
        let normalized = change.path.trim_end_matches("[]");
        if normalized != "audio.sources" && !allowed.contains(normalized) {
            return Err(format!("未知配置项: {}", change.path));
        }
        if !cap_steamvr && change.path.starts_with("output.steamvr.") {
            return Err("当前构建为标准版，未包含 SteamVR 支持，请下载 Full 版".to_string());
        }
        if !cap_osc && change.path.starts_with("output.osc.") {
            return Err("当前构建为标准版，未包含 OSC 支持，请下载 Full 版".to_string());
        }
        if change.path == "audio.sources" {
            config.audio.sources = serde_json::from_value(change.value.clone())
                .map_err(|e| format!("解析输入源失败: {e}"))?;
            restart = true;
            continue;
        }
        restart |= schema::apply_change(config, &change.path, &change.value)?;
    }
    Ok(restart)
}

async fn write_config_changes(
    state: &SharedState,
    changes: &[SettingChange],
) -> Result<AppConfig, String> {
    let path = ensure_config_path(state)?;

    let payload: Vec<(String, serde_json::Value)> = changes
        .iter()
        .map(|c| (c.path.trim_end_matches("[]").to_string(), c.value.clone()))
        .collect();
    // writer 先校验再落盘，并回读确认每个配置项真的生效了
    let cfg = writer::apply_changes(&path, &payload)?;
    state.clear_config_degraded();
    mirror_profile(state);
    Ok(cfg)
}

#[tauri::command]
async fn save_settings(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    changes: Vec<SettingChange>,
) -> Result<SaveResult, String> {
    // 与 mutate_sources / switch_profile 用同一把锁，避免与浮窗热更新命令互相覆盖。
    // 锁顺序固定为 lifecycle -> config_update，不存在反向嵌套。
    let _lifecycle = state.lifecycle.lock().await;
    let _config_guard = state.config_update.lock().await;
    let old = state.config();
    let (mut new, _) = state.settings_base_config();
    let restart = apply_settings_changes(&mut new, &changes)?;
    new.validate()?;

    // 以落盘后重新解析的结果为准（writer 可能做过归一化）
    let saved = write_config_changes(&state, &changes).await?;
    state.set_config(saved.clone());
    apply_hot_config(&app, &state, &old, &saved).await;

    let running = state.pipeline.lock().await.is_running();
    if restart && running {
        // 已经持着 lifecycle 锁，不能再走 restart_capture_impl（会重复加锁死锁）
        stop_capture_locked(app.clone(), state.inner().clone()).await?;
        start_capture_locked(app.clone(), state.inner().clone()).await?;
    }

    tracing::info!(
        "Settings saved ({} changes, restart={restart}, running={running})",
        changes.len()
    );
    Ok(SaveResult {
        applied: changes.len(),
        restart_required: false,
        message: match (restart, running) {
            (true, true) => "已保存并自动重启采集".to_string(),
            (true, false) => "已保存（下次开始采集时生效）".to_string(),
            (false, _) => "已保存并立即生效".to_string(),
        },
    })
}

/// 实时预览（不写文件、不改变持久配置）
#[tauri::command]
async fn preview_settings(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    changes: Vec<SettingChange>,
) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let saved = state.config();
    let mut preview = saved.clone();
    apply_settings_changes(&mut preview, &changes)?;
    preview.validate()?;
    state.outputs.apply_config(&preview.output).await;
    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.set_max_lines(history_limit(&preview));
        subtitle_state.set_show_source(preview.subtitle.show_source);
        subtitle_state.set_label_mode(preview.subtitle.label_sources);
    }
    publish_subtitle_snapshot(&state).await;
    // 仅覆盖纯视觉预览字段，不伪造点击穿透、翻译开关、识别语言等运行态按钮状态
    let mut payload = OverlayConfigPayload::from_config(&saved);
    payload.font_size = preview.subtitle.font_size;
    payload.text_color = preview.subtitle.text_color.clone();
    payload.stroke_color = preview.subtitle.stroke_color.clone();
    payload.background = preview.subtitle.background.clone();
    payload.toolbar_background = preview.subtitle.toolbar_background.clone();
    payload.show_source = preview.subtitle.show_source;
    payload.max_lines = preview.subtitle.max_lines;
    payload.label_sources = preview.subtitle.label_sources.as_str().to_string();
    let _ = app.emit_all(CONFIG_EVENT, payload);
    emit_subtitle_with_config(&app, &state, &preview).await;
    emit_output_status(&app, &state);
    Ok(())
}

#[tauri::command]
async fn cancel_preview(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    cancel_preview_impl(app, state.inner().clone()).await
}

/// 撤销实时预览（也用于设置窗口关闭时，避免运行态与配置长期不一致）
async fn cancel_preview_impl(app: AppHandle, state: Arc<SharedState>) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let config = state.config();
    state.outputs.apply_config(&config.output).await;
    {
        let mut subtitle_state = state.subtitle_state.lock().await;
        subtitle_state.set_max_lines(history_limit(&config));
        subtitle_state.set_show_source(config.subtitle.show_source);
        subtitle_state.set_label_mode(config.subtitle.label_sources);
    }
    publish_subtitle_snapshot(&state).await;
    emit_config(&app, &config);
    emit_subtitle_with_config(&app, &state, &config).await;
    emit_output_status(&app, &state);
    Ok(())
}

#[tauri::command]
async fn save_config_raw(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    text: String,
) -> Result<SaveResult, String> {
    let _lifecycle = state.lifecycle.lock().await;
    let _config_guard = state.config_update.lock().await;
    let old = state.config();
    let path = ensure_config_path(state.inner())?;
    // 先解析/校验能力，避免非法或不可用的配置落盘
    let parsed = AppConfig::from_str(&text).map_err(|e| e.to_string())?;
    parsed.validate()?;
    let (cap_steamvr, cap_osc) = output::capabilities();
    if !cap_steamvr && parsed.output.steamvr.enabled && !old.output.steamvr.enabled {
        return Err("当前构建为标准版，未包含 SteamVR 支持，请下载 Full 版".to_string());
    }
    if !cap_osc && parsed.output.osc.enabled && !old.output.osc.enabled {
        return Err("当前构建为标准版，未包含 OSC 支持，请下载 Full 版".to_string());
    }
    let new = writer::save_raw(&path, &text)?;
    state.clear_config_degraded();
    state.set_config(new.clone());
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(&new);
    }
    mirror_profile(&state);
    apply_hot_config(&app, &state, &old, &new).await;

    let running = state.pipeline.lock().await.is_running();
    if running {
        stop_capture_locked(app.clone(), state.inner().clone()).await?;
        start_capture_locked(app.clone(), state.inner().clone()).await?;
    }
    // 未知键会被 serde 静默丢弃：提示出来，否则用户会以为改了但没生效
    let unknown = writer::unknown_keys(&text);
    let message = if unknown.is_empty() {
        if running {
            "原始配置已保存并自动重启采集".to_string()
        } else {
            "原始配置已保存并校验通过".to_string()
        }
    } else {
        format!(
            "原始配置已保存，但以下配置项未被识别（不会生效）: {}",
            unknown.join("、")
        )
    };
    Ok(SaveResult {
        applied: 1,
        restart_required: false,
        message,
    })
}

#[tauri::command]
async fn reload_config(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
) -> Result<SaveResult, String> {
    let _lifecycle = state.lifecycle.lock().await;
    let _config_guard = state.config_update.lock().await;
    let path = state
        .config_path()
        .ok_or_else(|| "当前没有配置文件".to_string())?;
    let new = AppConfig::from_file(&path).map_err(|e| e.to_string())?;
    new.validate()?;
    let old = state.config();
    state.clear_config_degraded();
    state.set_config(new.clone());
    {
        let mut registry = state.sources.lock().await;
        registry.sync_config(&new);
    }
    apply_hot_config(&app, &state, &old, &new).await;
    let running = state.pipeline.lock().await.is_running();
    if running {
        stop_capture_locked(app.clone(), state.inner().clone()).await?;
        start_capture_locked(app.clone(), state.inner().clone()).await?;
    }
    Ok(SaveResult {
        applied: 1,
        restart_required: false,
        message: if running {
            "配置已重新加载并自动重启采集".to_string()
        } else {
            "配置已重新加载".to_string()
        },
    })
}

#[tauri::command]
async fn open_config_file(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    let path = ensure_config_path(state.inner())?;
    open_path(&path)
}

#[tauri::command]
async fn open_config_folder(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    let path = ensure_config_path(state.inner())?;
    match path.parent() {
        Some(dir) => open_path(dir),
        None => Err("无法定位配置目录".to_string()),
    }
}

#[tauri::command]
async fn open_settings(app: AppHandle, group: Option<String>) -> Result<(), String> {
    open_settings_window(&app, group.as_deref());
    Ok(())
}

#[tauri::command]
fn is_window_visible(window: Window) -> bool {
    window.is_visible().unwrap_or(true)
}

// ---------------- 配置档命令 ----------------

fn require_profile_path(state: &SharedState) -> Result<PathBuf, String> {
    let path = state
        .config_path()
        .ok_or_else(|| "当前使用内置默认配置，无法使用配置档".to_string())?;
    let (enabled, reason) = profiles::profiles_enabled(Some(&path));
    if !enabled {
        return Err(reason.unwrap_or_else(|| "当前配置路径不支持配置档".to_string()));
    }
    Ok(path)
}

#[tauri::command]
async fn create_profile(
    state: State<'_, Arc<SharedState>>,
    name: String,
    copy_current: bool,
) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let path = require_profile_path(&state)?;
    let source = if copy_current {
        Some(path.as_path())
    } else {
        None
    };
    profiles::create(&path, &name, source)
}

#[tauri::command]
async fn duplicate_profile(
    state: State<'_, Arc<SharedState>>,
    from: String,
    to: String,
) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let path = require_profile_path(&state)?;
    profiles::duplicate(&path, &from, &to)
}

#[tauri::command]
async fn rename_profile(
    state: State<'_, Arc<SharedState>>,
    old: String,
    new: String,
) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let path = require_profile_path(&state)?;
    profiles::rename(&path, &old, &new)
}

#[tauri::command]
async fn delete_profile(state: State<'_, Arc<SharedState>>, name: String) -> Result<(), String> {
    let _config_guard = state.config_update.lock().await;
    let path = require_profile_path(&state)?;
    profiles::delete(&path, &name)
}

#[tauri::command]
async fn switch_profile(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    name: String,
) -> Result<SaveResult, String> {
    let inner = state.inner().clone();
    let _lifecycle = inner.lifecycle.lock().await;
    let _config_guard = inner.config_update.lock().await;
    let config_path = require_profile_path(&inner)?;
    // 先校验目标配置档，再停采集，避免校验失败白白中断字幕
    let text = profiles::switch(&config_path, &name)?;
    let new = AppConfig::from_str(&text).map_err(|e| e.to_string())?;
    new.validate()?;

    let was_running = inner.pipeline.lock().await.is_running();
    let old = inner.config();

    if was_running {
        stop_capture_locked(app.clone(), inner.clone()).await?;
    }
    // switch_to 内部保证 profiles.toml 与 config.toml 不会长期不一致
    if let Err(e) = profiles::switch_to(&config_path, &name) {
        if was_running {
            let _ = start_capture_locked(app.clone(), inner.clone()).await;
        }
        return Err(e);
    }

    inner.clear_config_degraded();
    inner.set_config(new.clone());
    {
        let mut registry = inner.sources.lock().await;
        registry.sync_config(&new);
    }
    apply_hot_config(&app, &inner, &old, &new).await;

    if was_running {
        start_capture_locked(app.clone(), inner.clone()).await?;
    }
    let now_running = inner.pipeline.lock().await.is_running();
    let status_message = if was_running && !now_running {
        format!("已切换到配置档「{name}」（无可用输入源，采集未启动）")
    } else {
        format!("已切换到配置档「{name}」")
    };
    emit_status(&app, now_running, status_message.clone());
    Ok(SaveResult {
        applied: 1,
        restart_required: false,
        message: status_message,
    })
}

#[tauri::command]
async fn open_profiles_dir(state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    let path = require_profile_path(&state)?;
    let dir = profiles::profiles_dir(&path);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    open_path(&dir)
}

#[tauri::command]
async fn restart_pipeline(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
) -> Result<(), String> {
    let _lifecycle = state.lifecycle.lock().await;
    if !state.pipeline.lock().await.is_running() {
        return Ok(());
    }
    stop_capture_locked(app.clone(), state.inner().clone()).await?;
    start_capture_locked(app, state.inner().clone()).await
}

// ---------------- 原有窗口命令 ----------------

#[tauri::command]
async fn start_capture(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    start_capture_impl(app, state.inner().clone()).await
}

#[tauri::command]
async fn stop_capture(app: AppHandle, state: State<'_, Arc<SharedState>>) -> Result<(), String> {
    stop_capture_impl(app, state.inner().clone()).await
}

#[tauri::command]
fn close_overlay(app: AppHandle) -> Result<(), String> {
    quit_app(&app);
    Ok(())
}

#[tauri::command]
fn start_dragging(window: Window) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

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

#[tauri::command]
fn set_window_bounds(
    window: Window,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
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

#[tauri::command]
async fn persist_window_geometry(
    app: AppHandle,
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

    // 走 config_update 锁：否则拖动窗口的回写会覆盖其它并发命令刚改的字段
    state
        .update_config(|config| {
            config.subtitle.window_width = width;
            config.subtitle.window_height = height;
            config.subtitle.position_x = x;
            config.subtitle.position_y = y;
            Ok(())
        })
        .await?;

    // 四个字段一次落盘：每次 resize 读改写四遍整份配置太重
    persist_or_report(
        &app,
        &state,
        &[
            ("subtitle.window_width", width.into()),
            ("subtitle.window_height", height.into()),
            ("subtitle.position_x", x.into()),
            ("subtitle.position_y", y.into()),
        ],
    );
    Ok(())
}

#[tauri::command]
async fn set_click_through(
    app: AppHandle,
    state: State<'_, Arc<SharedState>>,
    enabled: bool,
) -> Result<(), String> {
    let config = state
        .update_config(|config| {
            config.subtitle.click_through = enabled;
            Ok(())
        })
        .await?;
    apply_click_through(&app, enabled);
    persist_value(&app, &state, "subtitle", "click_through", enabled.into());
    emit_config(&app, &config);
    Ok(())
}

// ---------------- 系统集成 ----------------

pub(crate) fn open_config_dir(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<SharedState>>();
    let path = ensure_config_path(&state)?;
    match path.parent() {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            open_path(dir)
        }
        None => Err("无法定位配置目录".to_string()),
    }
}

fn schedule_persist_main_position(window: &Window) {
    let app = window.app_handle();
    let state = app.state::<Arc<SharedState>>().inner().clone();
    if state.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let seq = state.move_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if state.move_seq.load(Ordering::SeqCst) != seq
            || state.shutting_down.load(Ordering::SeqCst)
        {
            return;
        }
        let (Ok(scale), Ok(pos)) = (window.scale_factor(), window.outer_position()) else {
            return;
        };
        let x = (pos.x as f64 / scale).round() as i32;
        let y = (pos.y as f64 / scale).round() as i32;
        let current = state.config();
        if (x - current.subtitle.position_x).abs() <= 1
            && (y - current.subtitle.position_y).abs() <= 1
        {
            return;
        }
        if state
            .update_config(|config| {
                config.subtitle.position_x = x;
                config.subtitle.position_y = y;
                Ok(())
            })
            .await
            .is_ok()
        {
            persist_or_report(
                &app,
                &state,
                &[
                    ("subtitle.position_x", x.into()),
                    ("subtitle.position_y", y.into()),
                ],
            );
        }
    });
}

fn open_path(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";

    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    tracing::info!("Opened: {}", path.display());
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

    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    if let Some((log_path, log_dir, file_name)) = resolve_log_target() {
        let appender = match tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(file_name.clone())
            .max_log_files(7)
            .build(&log_dir)
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("failed to create rolling log ({e}), falling back to single file");
                tracing_appender::rolling::RollingFileAppender::new(
                    tracing_appender::rolling::Rotation::NEVER,
                    &log_dir,
                    file_name,
                )
            }
        };
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
        eprintln!("log file: {}.<date>", log_path.display());
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

#[cfg(target_os = "windows")]
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_parent_console() {}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        default_hook(info);

        #[cfg(target_os = "windows")]
        {
            static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
                show_error_dialog("LiveTranslate 崩溃", &format!("{info}"));
            }
        }
    }));
}

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

    // ONNX Runtime 动态库探测必须在创建任何线程/异步运行时之前完成
    audio::detector::prepare_ort_library();

    // --list-devices 纯查询命令不应创建/修改/隔离任何配置文件
    if args.list_devices {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::list_devices())?;
        return Ok(());
    }

    let resolved = if args.list_models {
        AppConfig::resolve_read_only(args.config.clone())
    } else {
        AppConfig::resolve_detailed(args.config.clone())
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(e) if args.config.is_some() => {
            tracing::error!("Failed to load config: {}", e);
            return Err(e.into());
        }
        Err(e) => {
            tracing::error!("Failed to load config: {}", e);
            tracing::warn!("Falling back to built-in default configuration");
            config::ResolvedConfig {
                config: AppConfig::default(),
                path: None,
                warning: Some(format!("配置文件加载失败（{e}），已回退到默认配置")),
                degraded: true,
            }
        }
    };
    let config = resolved.config;
    let config_path = resolved.path;
    let startup_warning = resolved.warning;
    let config_degraded = resolved.degraded;

    match &config_path {
        Some(path) => tracing::info!("Config file: {}", path.display()),
        None => {
            let hint = AppConfig::user_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string());
            tracing::warn!(
                "Using built-in defaults. 建议在该位置创建配置文件: {}",
                hint
            );
        }
    }
    tracing::debug!("Config: {:?}", config.redacted());

    if args.list_models {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::list_models(&config))?;
        return Ok(());
    }

    // 配置档首次启用时做非破坏性迁移（仅用户配置目录，且不覆盖损坏恢复态）
    if !config_degraded {
        if let Some(path) = &config_path {
            if profiles::profiles_enabled(Some(path)).0 {
                if let Err(e) = profiles::ensure_initialized(path) {
                    tracing::warn!("Failed to initialize profiles: {e}");
                }
            }
        }
    }

    if args.console {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(app::start_console(config))?;
        return Ok(());
    }

    let shared = Arc::new(SharedState::new(
        config.clone(),
        config_path.clone(),
        startup_warning,
        config_degraded,
    ));

    let app = tauri::Builder::default()
        .system_tray(tray::create_system_tray())
        .on_system_tray_event(tray::handle_system_tray_event)
        .manage(shared)
        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::CloseRequested { api, .. } => match event.window().label() {
                "settings" => {
                    // 关闭设置页时只隐藏，保留窗口与前端状态；
                    // 通知前端清掉防抖预览定时器，并把运行态回滚到持久配置。
                    api.prevent_close();
                    let _ = event.window().hide();
                    let app = event.window().app_handle();
                    let _ = app.emit_all(SETTINGS_HIDDEN_EVENT, ());
                    let state = app.state::<Arc<SharedState>>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = cancel_preview_impl(app, state).await {
                            tracing::warn!("撤销设置预览失败: {e}");
                        }
                    });
                }
                "main" => {
                    // 隐藏的 settings 窗口会阻止 Tauri 自动触发退出，主窗口关闭时统一走完整退场
                    api.prevent_close();
                    quit_app(&event.window().app_handle());
                }
                _ => {}
            },
            tauri::WindowEvent::Moved(_) if event.window().label() == "main" => {
                schedule_persist_main_position(event.window());
            }
            _ => {}
        })
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Regular);
            }

            let window = app
                .get_window("main")
                .ok_or_else(|| anyhow::anyhow!("main window not found"))?;

            if let Err(e) = apply_window_config(&window, &config) {
                tracing::warn!("Failed to apply window config: {e}");
            }
            let payload = OverlayConfigPayload::from_config(&config);
            if let Err(e) = window.emit(CONFIG_EVENT, payload) {
                tracing::warn!("Failed to emit config event: {e}");
            }
            if let Err(e) = tray::register_global_shortcuts(&app.handle()) {
                tracing::warn!("Failed to register global shortcuts (可能被其他程序占用): {e}");
            }

            let handle = app.handle();
            let state = handle.state::<Arc<SharedState>>();
            if let Some(window) = app.get_window("main") {
                if let Ok(registry) = state.sources.try_lock() {
                    let _ = window.emit(
                        SOURCE_STATUS_EVENT,
                        SourcesPayload {
                            sources: registry.status_items(),
                            max_active_sources: state.config().audio.max_active_sources,
                        },
                    );
                }
                let _ = window.emit(OUTPUT_STATUS_EVENT, state.outputs.status());
            }

            // 后台输出线程（SteamVR 连接重连 / OSC 发送与报错）状态变化时实时推送给前端
            let app_for_output = app.handle();
            let state_for_output = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    state_for_output.outputs.status_notified().await;
                    if state_for_output.shutting_down.load(Ordering::SeqCst) {
                        break;
                    }
                    emit_output_status(&app_for_output, &state_for_output);
                }
            });

            let _ = window.show();
            let _ = window.set_focus();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_overlay_config,
            set_font_size,
            set_show_source,
            set_translate_enabled,
            list_languages,
            set_language,
            get_status,
            list_devices_command,
            set_device,
            list_sources,
            get_sources_config,
            set_source_translate,
            set_source_enabled,
            set_source_device,
            add_source,
            update_source,
            remove_source,
            reorder_sources,
            get_output_status,
            set_output_enabled,
            get_capabilities,
            open_settings,
            is_window_visible,
            get_settings_schema,
            save_settings,
            preview_settings,
            cancel_preview,
            get_config_raw,
            save_config_raw,
            reload_config,
            open_config_file,
            open_config_folder,
            get_profiles,
            create_profile,
            duplicate_profile,
            rename_profile,
            delete_profile,
            switch_profile,
            open_profiles_dir,
            restart_pipeline,
            start_capture,
            stop_capture,
            close_overlay,
            start_dragging,
            window_geometry,
            set_window_bounds,
            persist_window_geometry,
            set_click_through,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri runtime error: {e}"))?;

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            // 退出前先停采集并回收外部输出（与 quit_app 共用单次门闩）
            let handle = app_handle.clone();
            tauri::async_runtime::block_on(async move {
                shutdown_once(&handle).await;
            });
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_changes_apply_scalars_and_sources() {
        let mut config = AppConfig::default();
        let changes = vec![
            SettingChange {
                path: "subtitle.font_size".to_string(),
                value: json!(40),
            },
            SettingChange {
                path: "translate.enabled".to_string(),
                value: json!(false),
            },
            SettingChange {
                path: "audio.sources".to_string(),
                value: json!([{
                    "id": "mic",
                    "label": "麦克风",
                    "device": "mic:0",
                    "enabled": true,
                    "translate": true,
                    "language": ""
                }]),
            },
        ];
        let restart = apply_settings_changes(&mut config, &changes).unwrap();
        assert!(restart, "输入源变更需要重启采集");
        assert_eq!(config.subtitle.font_size, 40);
        assert!(!config.translate.enabled);
        assert_eq!(config.audio.sources.len(), 1);
        assert_eq!(config.audio.sources[0].device, "mic:0");
    }

    #[test]
    fn settings_changes_reject_unknown_and_coarse_paths() {
        let mut config = AppConfig::default();
        for path in ["subtitle.fontsize", "output", "output.steamvr"] {
            let changes = vec![SettingChange {
                path: path.to_string(),
                value: json!({ "enabled": true }),
            }];
            let err = apply_settings_changes(&mut config, &changes).unwrap_err();
            assert!(err.contains("未知配置项"), "{path}: {err}");
        }
    }

    #[test]
    fn settings_changes_reject_unavailable_outputs() {
        if cfg!(feature = "osc") {
            return;
        }
        let mut config = AppConfig::default();
        let changes = vec![SettingChange {
            path: "output.osc.enabled".to_string(),
            value: json!(true),
        }];
        assert!(apply_settings_changes(&mut config, &changes).is_err());
    }

    #[test]
    fn history_limit_covers_all_outputs() {
        let mut config = AppConfig::default();
        config.subtitle.max_lines = 2;
        config.output.steamvr.max_lines = 5;
        config.output.osc.max_lines = 7;
        assert_eq!(history_limit(&config), 7);
    }

    #[test]
    fn desktop_entries_keep_only_configured_lines() {
        let config = AppConfig {
            subtitle: config::SubtitleConfig {
                max_lines: 2,
                ..AppConfig::default().subtitle
            },
            ..AppConfig::default()
        };
        let entries: Vec<_> = (0..4)
            .map(|i| subtitle::DisplayLine {
                source_id: "mic".into(),
                label: None,
                source: format!("s{i}"),
                translated: format!("t{i}"),
                lines: vec![format!("t{i}")],
                text: format!("t{i}"),
            })
            .collect();
        let kept = desktop_entries(entries, &config);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].text, "t2");
        assert_eq!(kept[1].text, "t3");
    }

    #[test]
    fn registry_sync_and_per_source_translate() {
        let mut config = AppConfig::default();
        config.audio.sources = vec![
            AudioSourceConfig::new("mic", "麦克风", "mic:0"),
            AudioSourceConfig::new("game", "游戏", "loopback:0"),
        ];
        let mut registry = SourceRegistry::from_config(&config);
        assert_eq!(registry.runtimes.len(), 2);
        assert!(registry.set_translate("mic", false));
        assert!(!registry
            .runtimes
            .iter()
            .find(|r| r.id == "mic")
            .unwrap()
            .translate_enabled());
        assert!(registry
            .runtimes
            .iter()
            .find(|r| r.id == "game")
            .unwrap()
            .translate_enabled());

        config.audio.sources.remove(1);
        registry.sync_config(&config);
        assert_eq!(registry.runtimes.len(), 1);

        // 全局翻译关闭时，status_items 仍保留单源自身的翻译偏好供浮窗开关展示
        registry.set_translate_all(false);
        assert!(registry.status_items()[0].translate);
        assert!(!registry.runtimes[0].translate_enabled());
    }

    #[test]
    fn preview_validation_catches_invalid_values() {
        let mut preview = AppConfig::default();
        let changes = vec![SettingChange {
            path: "subtitle.font_size".to_string(),
            value: json!(999),
        }];
        apply_settings_changes(&mut preview, &changes).unwrap();
        assert!(preview.validate().is_err());
    }

    #[test]
    fn degraded_shared_state_blocks_unconfigured_ensure_and_clears_cleanly() {
        let state = SharedState::new(
            AppConfig::default(),
            None,
            Some("便携配置损坏".to_string()),
            true,
        );
        assert!(state.is_config_degraded());
        assert_eq!(state.degraded_warning().as_deref(), Some("便携配置损坏"));
        assert!(ensure_config_path(&state).is_err());

        state.clear_config_degraded();
        assert!(!state.is_config_degraded());
        assert!(state.degraded_warning().is_none());
    }
}
