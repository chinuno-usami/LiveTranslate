use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub mod profiles;
pub mod schema;
pub mod writer;

/// 多源字幕来源标识策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LabelMode {
    /// 同时有 ≥2 个源在跑时自动加前缀（默认）
    #[default]
    Auto,
    /// 始终加
    Always,
    /// 从不加
    Never,
}

impl LabelMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelMode::Auto => "auto",
            LabelMode::Always => "always",
            LabelMode::Never => "never",
        }
    }
}

/// 单个输入源配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSourceConfig {
    /// 稳定 ID（事件 / OSC / 日志使用），仅允许 `[A-Za-z0-9_-]{1,32}`
    pub id: String,
    /// 展示名（多源时作为输出前缀）
    pub label: String,
    /// 设备 spec，语法与旧 `audio.device_name` 相同
    pub device: String,
    /// 是否参与采集（改动需要重启该源）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 该源是否翻译（可热切换）
    #[serde(default = "default_true")]
    pub translate: bool,
    /// 可选：覆盖全局识别语言；空 = 跟随全局
    #[serde(default)]
    pub language: String,
}

impl AudioSourceConfig {
    #[allow(dead_code)]
    pub fn new(id: impl Into<String>, label: impl Into<String>, device: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            device: device.into(),
            enabled: true,
            translate: true,
            language: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// 兼容旧版本的单源字段：`sources` 为空时使用它合成一个默认源
    #[serde(default = "default_device_name")]
    pub device_name: String,
    #[serde(default = "default_chunk_seconds")]
    pub chunk_seconds: f32,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u16,
    #[serde(default = "default_silence_threshold")]
    pub silence_threshold: f32,
    /// 同时采集的输入源上限（防止多路 Silero 把 CPU 打满）
    #[serde(default = "default_max_active_sources")]
    pub max_active_sources: usize,
    /// 多输入源：每个源一条，顺序即 UI 展示顺序
    #[serde(default)]
    pub sources: Vec<AudioSourceConfig>,
}

impl AudioConfig {
    /// 旧配置（只有 `device_name`）会在这里被合成为单源
    pub fn effective_sources(&self, translate_enabled: bool) -> Vec<AudioSourceConfig> {
        if self.sources.is_empty() {
            vec![AudioSourceConfig {
                id: "default".to_string(),
                label: "输入源".to_string(),
                device: self.device_name.clone(),
                enabled: true,
                translate: translate_enabled,
                language: String::new(),
            }]
        } else {
            self.sources.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    /// 识别后端："whisper"（自建/兼容服务）或 "edge"（微软 Edge 在线识别）
    #[serde(default = "default_asr_backend")]
    pub backend: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub timeout_secs: u64,
    pub request_path: String,
    /// 可选的访问令牌
    ///
    /// 设置后会按 `auth_header` 指定的头部发送（默认 `Authorization: Bearer <token>`）。
    /// 留空则不发送任何认证头。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 认证头名称，默认 "Authorization"
    ///
    /// 部分服务使用不同的头部，例如 `api-key`。
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Edge 在线识别后端配置（仅 `backend = "edge"` 时使用）
    #[serde(default)]
    pub edge: EdgeAsrConfig,
    /// 过滤音乐/噪声引起的无意义识别输出（默认开启）
    #[serde(default = "default_true")]
    pub filter_hallucination: bool,
}

/// Edge 在线语音识别后端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAsrConfig {
    /// BCP-47 语言标签，如 `en-US` / `zh-CN`（不属于 `auto`）
    #[serde(default = "default_edge_language")]
    pub language: String,
    /// 单次识别超时（秒）
    #[serde(default = "default_edge_timeout_secs")]
    pub timeout_secs: u64,
    /// 以下三项是服务端可能轮换的身份参数，一般无需修改
    #[serde(default)]
    pub trusted_client_token: Option<String>,
    #[serde(default)]
    pub chromium_full_version: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
}

impl Default for EdgeAsrConfig {
    fn default() -> Self {
        Self {
            language: default_edge_language(),
            timeout_secs: default_edge_timeout_secs(),
            trusted_client_token: None,
            chromium_full_version: None,
            origin: None,
        }
    }
}

fn default_asr_backend() -> String {
    "whisper".to_string()
}
fn default_edge_language() -> String {
    "en-US".to_string()
}
fn default_edge_timeout_secs() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateConfig {
    /// 是否翻译；关闭后为“仅识别”模式，只显示 ASR 原文（面板开关可实时切换）
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub target_language: String,
    pub system_prompt: String,
    pub timeout_secs: u64,
    /// 是否在请求里带上 `temperature`
    ///
    /// 部分推理模型（如 OpenAI o 系列）不接受该字段，
    /// 遇到 400 时可以关掉。
    #[serde(default = "default_true")]
    pub send_temperature: bool,
    /// 采样温度（仅 `send_temperature = true` 时发送）
    #[serde(default = "default_translate_temperature")]
    pub temperature: f32,
    /// 关闭推理模型“思考”的策略，空字符串表示不处理
    #[serde(default)]
    pub disable_thinking: String,
    /// 追加到请求体的自定义字段（最高优先级，会覆盖上面的预设）
    #[serde(default)]
    pub extra_body: Option<toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleConfig {
    pub max_lines: usize,
    pub font_size: u32,
    pub text_color: String,
    pub stroke_color: String,
    /// 字幕面板背景（任意 CSS 颜色，含 alpha）
    #[serde(default = "default_panel_background")]
    pub background: String,
    /// 顶部工具栏背景（任意 CSS 颜色，含 alpha）
    #[serde(default = "default_toolbar_background")]
    pub toolbar_background: String,
    pub show_source: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub position_x: i32,
    pub position_y: i32,
    pub always_on_top: bool,
    pub click_through: bool,
    /// 多源来源标识策略：auto / always / never
    #[serde(default)]
    pub label_sources: LabelMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_vad_backend")]
    pub backend: String,
    #[serde(default = "default_vad_frame_ms")]
    pub frame_ms: u32,
    #[serde(default = "default_vad_margin_db")]
    pub margin_db: f32,
    #[serde(default = "default_vad_noise_percentile")]
    pub noise_percentile: f32,
    #[serde(default)]
    pub silero_model: Option<String>,
    #[serde(default = "default_silero_threshold")]
    pub silero_threshold: f32,
    #[serde(default = "default_vad_min_speech_ms")]
    pub min_speech_ms: u32,
    #[serde(default = "default_vad_min_silence_ms")]
    pub min_silence_ms: u32,
    #[serde(default = "default_vad_max_speech_ms")]
    pub max_speech_ms: u32,
    #[serde(default = "default_vad_pre_pad_ms")]
    pub pre_pad_ms: u32,
    #[serde(default = "default_vad_post_pad_ms")]
    pub post_pad_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: "energy".to_string(),
            frame_ms: 20,
            margin_db: 8.0,
            noise_percentile: 0.1,
            silero_model: None,
            silero_threshold: 0.5,
            min_speech_ms: 200,
            min_silence_ms: 400,
            max_speech_ms: 12_000,
            pre_pad_ms: 200,
            post_pad_ms: 200,
        }
    }
}

/// SteamVR Overlay 输出配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteamVrConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 锚点：hmd = 跟随头部；world = 固定在世界坐标
    #[serde(default = "default_anchor")]
    pub anchor: String,
    #[serde(default = "default_width_m")]
    pub width_m: f32,
    #[serde(default = "default_distance_m")]
    pub distance_m: f32,
    #[serde(default = "default_height_offset_m")]
    pub height_offset_m: f32,
    #[serde(default = "default_world_position")]
    pub world_position: [f32; 3],
    #[serde(default)]
    pub world_rotation_deg: [f32; 3],
    #[serde(default)]
    pub curvature: f32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_max_lines")]
    pub max_lines: usize,
    #[serde(default = "default_true")]
    pub show_source: bool,
    #[serde(default = "default_font_size_px")]
    pub font_size_px: u32,
    #[serde(default)]
    pub font_path: String,
    #[serde(default = "default_vr_background")]
    pub background: String,
    #[serde(default = "default_vr_text_color")]
    pub text_color: String,
    #[serde(default = "default_vr_source_color")]
    pub source_color: String,
    #[serde(default = "default_vr_label_color")]
    pub label_color: String,
    #[serde(default = "default_line_alphas")]
    pub line_alphas: [f32; 3],
    #[serde(default)]
    pub hide_on_silence_secs: u64,
    #[serde(default)]
    pub swap_red_blue: bool,
    #[serde(default = "default_reconnect_interval_secs")]
    pub reconnect_interval_secs: u64,
}

/// OSC（VRChat Chatbox）输出配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_osc_target")]
    pub target: String,
    /// 发送内容："translated" | "source" | "both"
    #[serde(default = "default_osc_content")]
    pub content: String,
    /// 组包模式："rolling" | "latest"
    #[serde(default = "default_osc_mode")]
    pub mode: String,
    #[serde(default = "default_osc_max_lines")]
    pub max_lines: usize,
    #[serde(default = "default_osc_max_chars")]
    pub max_chars: usize,
    #[serde(default = "default_osc_separator")]
    pub separator: String,
    #[serde(default = "default_osc_min_interval_ms")]
    pub min_interval_ms: u64,
    #[serde(default = "default_osc_max_per_window")]
    pub max_per_window: usize,
    #[serde(default = "default_true")]
    pub send_typing: bool,
    #[serde(default)]
    pub custom_address: String,
    #[serde(default)]
    pub debug_log: bool,
}

impl Default for SteamVrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            anchor: default_anchor(),
            width_m: default_width_m(),
            distance_m: default_distance_m(),
            height_offset_m: default_height_offset_m(),
            world_position: default_world_position(),
            world_rotation_deg: [0.0, 0.0, 0.0],
            curvature: 0.0,
            opacity: default_opacity(),
            max_lines: default_max_lines(),
            show_source: true,
            font_size_px: default_font_size_px(),
            font_path: String::new(),
            background: default_vr_background(),
            text_color: default_vr_text_color(),
            source_color: default_vr_source_color(),
            label_color: default_vr_label_color(),
            line_alphas: default_line_alphas(),
            hide_on_silence_secs: 0,
            swap_red_blue: false,
            reconnect_interval_secs: default_reconnect_interval_secs(),
        }
    }
}

impl Default for OscConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: default_osc_target(),
            content: default_osc_content(),
            mode: default_osc_mode(),
            max_lines: default_osc_max_lines(),
            max_chars: default_osc_max_chars(),
            separator: default_osc_separator(),
            min_interval_ms: default_osc_min_interval_ms(),
            max_per_window: default_osc_max_per_window(),
            send_typing: true,
            custom_address: String::new(),
            debug_log: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputConfig {
    #[serde(default)]
    pub steamvr: SteamVrConfig,
    #[serde(default)]
    pub osc: OscConfig,
}

fn default_true() -> bool {
    true
}
fn default_device_name() -> String {
    "default".to_string()
}
fn default_chunk_seconds() -> f32 {
    2.0
}
fn default_sample_rate() -> u32 {
    16000
}
fn default_channels() -> u16 {
    1
}
fn default_silence_threshold() -> f32 {
    0.01
}
fn default_max_active_sources() -> usize {
    4
}
fn default_panel_background() -> String {
    "rgba(8, 10, 14, 0.28)".to_string()
}
fn default_toolbar_background() -> String {
    "rgba(10, 12, 18, 0.42)".to_string()
}
fn default_vad_frame_ms() -> u32 {
    VadConfig::default().frame_ms
}
fn default_vad_backend() -> String {
    VadConfig::default().backend
}
fn default_silero_threshold() -> f32 {
    VadConfig::default().silero_threshold
}
fn default_translate_temperature() -> f32 {
    0.2
}
fn default_vad_margin_db() -> f32 {
    VadConfig::default().margin_db
}
fn default_vad_noise_percentile() -> f32 {
    VadConfig::default().noise_percentile
}
fn default_vad_min_speech_ms() -> u32 {
    VadConfig::default().min_speech_ms
}
fn default_vad_min_silence_ms() -> u32 {
    VadConfig::default().min_silence_ms
}
fn default_vad_max_speech_ms() -> u32 {
    VadConfig::default().max_speech_ms
}
fn default_vad_pre_pad_ms() -> u32 {
    VadConfig::default().pre_pad_ms
}
fn default_vad_post_pad_ms() -> u32 {
    VadConfig::default().post_pad_ms
}
fn default_anchor() -> String {
    "hmd".to_string()
}
fn default_width_m() -> f32 {
    1.6
}
fn default_distance_m() -> f32 {
    1.6
}
fn default_height_offset_m() -> f32 {
    -0.28
}
fn default_opacity() -> f32 {
    0.92
}
fn default_max_lines() -> usize {
    3
}
fn default_font_size_px() -> u32 {
    34
}
fn default_vr_background() -> String {
    "#0B0F14CC".to_string()
}
fn default_vr_text_color() -> String {
    "#FFFFFF".to_string()
}
fn default_vr_source_color() -> String {
    "#AEB8C4".to_string()
}
fn default_vr_label_color() -> String {
    "#7DD3FC".to_string()
}
fn default_line_alphas() -> [f32; 3] {
    [1.0, 0.72, 0.48]
}
fn default_world_position() -> [f32; 3] {
    [0.0, 1.3, -1.6]
}
fn default_reconnect_interval_secs() -> u64 {
    5
}
fn default_osc_target() -> String {
    "127.0.0.1:9000".to_string()
}
fn default_osc_content() -> String {
    "translated".to_string()
}
fn default_osc_mode() -> String {
    "rolling".to_string()
}
fn default_osc_max_lines() -> usize {
    4
}
fn default_osc_max_chars() -> usize {
    144
}
fn default_osc_separator() -> String {
    "\n".to_string()
}
fn default_osc_min_interval_ms() -> u64 {
    2000
}
fn default_osc_max_per_window() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub audio: AudioConfig,
    pub asr: AsrConfig,
    pub translate: TranslateConfig,
    pub subtitle: SubtitleConfig,
    /// VAD 分段配置（旧配置文件没有该段时使用默认值）
    #[serde(default)]
    pub vad: VadConfig,
    /// 额外输出通道（旧配置文件没有该段时全部关闭）
    #[serde(default)]
    pub output: OutputConfig,
}

/// 配置解析结果（附带可恢复的告警信息，供 GUI 在启动后提示用户）
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub config: AppConfig,
    pub path: Option<PathBuf>,
    pub warning: Option<String>,
    /// 磁盘配置加载失败并回退到默认值：运行期禁止自动镜像覆盖已有配置档，
    /// 直至用户显式保存设置或切换配置档。
    pub degraded: bool,
}

impl AppConfig {
    /// 用户级配置目录（跨平台）
    ///
    /// - macOS: `~/Library/Application Support/com.chinuno.LiveTranslate`
    /// - Windows: `%APPDATA%\chinuno\LiveTranslate\config`
    /// - Linux: `~/.config/livetranslate`
    pub fn user_config_dir() -> AppResult<PathBuf> {
        ProjectDirs::from("com", "chinuno", "LiveTranslate")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .ok_or_else(|| {
                AppError::Config("Failed to determine user config directory".to_string())
            })
    }

    /// 用户级配置文件路径（打包后应用的主要配置入口）
    pub fn user_config_path() -> AppResult<PathBuf> {
        Ok(Self::user_config_dir()?.join("config.toml"))
    }

    /// 用户级日志目录
    pub fn log_dir() -> AppResult<PathBuf> {
        ProjectDirs::from("com", "chinuno", "LiveTranslate")
            .map(|dirs| dirs.data_local_dir().join("logs"))
            .ok_or_else(|| AppError::Config("Failed to determine log directory".to_string()))
    }

    /// 从指定文件读取配置
    pub fn from_file(path: &Path) -> AppResult<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| AppError::Config(format!("读取 {} 失败: {e}", path.display())))?;
        toml::from_str(&content).map_err(|e| {
            AppError::Config(format!(
                "解析 {} 失败: {}",
                path.display(),
                toml_error_text(&e)
            ))
        })
    }

    /// 从 TOML 文本解析配置
    pub fn from_str(content: &str) -> AppResult<Self> {
        toml::from_str(content).map_err(|e| AppError::TomlDeserialize(toml_error_text(&e)))
    }

    /// 序列化为 TOML 文本
    pub fn to_toml_string(&self) -> AppResult<String> {
        toml::to_string_pretty(self).map_err(AppError::TomlSerialize)
    }

    /// 随包发布的默认配置（带完整中文注释，作为设置页 help 的来源）
    pub const SHIPPED_DEFAULT: &'static str = include_str!("../../config/default.toml");

    /// 若目标文件不存在，则写入一份默认配置（并创建父目录）
    ///
    /// 优先使用随包的 `config/default.toml`（保留注释）；若内容不可解析，
    /// 回退到 `AppConfig::default()` 的结构化序列化。
    pub fn write_default_if_absent(path: &Path) -> AppResult<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = match toml::from_str::<AppConfig>(Self::SHIPPED_DEFAULT) {
            Ok(_) => Self::SHIPPED_DEFAULT.to_string(),
            Err(_) => AppConfig::default().to_toml_string()?,
        };
        // 文件里有 api_key，尽量不留其他用户可读的窗口；
        // 用 create_new 消除「检查存在 → 截断写入」之间的竞态（另一个实例刚写好的配置会被清空）。
        match write_private_if_absent(path, &content) {
            Ok(true) => Ok(true),
            Ok(false) => Ok(false),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    /// 用户配置损坏时的收尾：把坏文件改名保留，返回备份路径
    ///
    /// 不直接删除：用户手工编辑错一个引号是可恢复的，不能当成“配置不存在”。
    fn quarantine(path: &Path) -> Option<PathBuf> {
        let mut target = path.as_os_str().to_os_string();
        target.push(format!(".bad-{}", std::process::id()));
        let target = PathBuf::from(target);
        match fs::rename(path, &target) {
            Ok(()) => Some(target),
            Err(e) => {
                tracing::error!("无法隔离损坏的配置文件 {}: {e}", path.display());
                None
            }
        }
    }

    /// 从指定文件读取并校验配置
    pub fn load_validated(path: &Path) -> AppResult<Self> {
        let cfg = Self::from_file(path)?;
        cfg.validate()
            .map_err(|e| AppError::Config(format!("校验 {} 失败: {e}", path.display())))?;
        Ok(cfg)
    }

    /// 按优先级解析配置，返回 `(配置, 来源路径)`
    #[cfg(test)]
    pub fn resolve(explicit: Option<PathBuf>) -> AppResult<(Self, Option<PathBuf>)> {
        let resolved = Self::resolve_detailed(explicit)?;
        Ok((resolved.config, resolved.path))
    }

    /// 按优先级解析配置，并在非显式配置文件损坏时保留原路径与告警信息
    pub fn resolve_detailed(explicit: Option<PathBuf>) -> AppResult<ResolvedConfig> {
        Self::resolve_inner(explicit, false)
    }

    /// 只读解析配置（用于 `--list-models` 等诊断命令，不创建或隔离改写用户配置文件）
    pub fn resolve_read_only(explicit: Option<PathBuf>) -> AppResult<ResolvedConfig> {
        Self::resolve_inner(explicit, true)
    }

    fn resolve_inner(explicit: Option<PathBuf>, read_only: bool) -> AppResult<ResolvedConfig> {
        if let Some(path) = explicit {
            if !path.exists() {
                return Err(AppError::Config(format!(
                    "Config file not found: {}",
                    path.display()
                )));
            }
            let cfg = Self::load_validated(&path)?;
            tracing::info!("Loaded config from: {}", path.display());
            return Ok(ResolvedConfig {
                config: cfg,
                path: Some(path),
                warning: None,
                degraded: false,
            });
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let p = dir.join("config").join("default.toml");
                if p.exists() {
                    return Ok(Self::load_non_explicit_or_warn(p));
                }
            }
        }

        let cwd_path = PathBuf::from("config/default.toml");
        if cwd_path.exists() {
            return Ok(Self::load_non_explicit_or_warn(cwd_path));
        }

        if let Ok(user_path) = Self::user_config_path() {
            if user_path.exists() {
                match Self::load_validated(&user_path) {
                    Ok(cfg) => {
                        tracing::info!("Loaded config from: {}", user_path.display());
                        return Ok(ResolvedConfig {
                            config: cfg,
                            path: Some(user_path),
                            warning: None,
                            degraded: false,
                        });
                    }
                    Err(e) if read_only => {
                        tracing::warn!(
                            "Failed to load config from {}: {}; falling back to defaults",
                            user_path.display(),
                            e
                        );
                        return Ok(ResolvedConfig {
                            config: AppConfig::default(),
                            path: None,
                            warning: Some(format!(
                                "配置文件 {} 加载失败（{e}），本次将使用内置默认配置",
                                user_path.display()
                            )),
                            degraded: true,
                        });
                    }
                    Err(e) => {
                        return Ok(Self::recover_user_config(&user_path, &e.to_string()));
                    }
                }
            }

            if !read_only {
                match Self::write_default_if_absent(&user_path) {
                    Ok(true) => {
                        tracing::info!(
                            "Created default config at: {} (请填入翻译 API Key)",
                            user_path.display()
                        );
                        let cfg = Self::load_validated(&user_path).unwrap_or_default();
                        return Ok(ResolvedConfig {
                            config: cfg,
                            path: Some(user_path),
                            warning: None,
                            degraded: false,
                        });
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(
                            "Failed to create default config at {}: {}",
                            user_path.display(),
                            e
                        );
                    }
                }
            }
        }

        let hint = Self::user_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        tracing::warn!(
            "No config file found; using built-in defaults. \
             Create one at: {}",
            hint
        );
        Ok(ResolvedConfig {
            config: AppConfig::default(),
            path: None,
            warning: None,
            degraded: false,
        })
    }

    /// 用户配置目录 `config.toml` 加载失败时的恢复逻辑：
    /// 1. 先将损坏原文件备份为 `.bad-*`；
    /// 2. 若当前激活配置档（`profiles/<active>.toml`）存在且校验合法，优先从该快照恢复，避免丢失 `api_key` 等设置；
    /// 3. 否则回退到内置默认配置，并标记 `degraded = true` 以阻止后续运行期自动镜像覆盖完好的配置档文件。
    pub(crate) fn recover_user_config(user_path: &Path, err_msg: &str) -> ResolvedConfig {
        // 保留坏文件（用户可手动修复），并继续用配置路径运行，
        // 否则配置档功能会静默消失、下次保存会覆盖原文件。
        let quarantined = Self::quarantine(user_path);

        if let Some(active) = profiles::read_active(user_path) {
            if profiles::validate_name(&active).is_ok() {
                let snap = profiles::profiles_dir(user_path).join(format!("{active}.toml"));
                if let Ok(text) = fs::read_to_string(&snap) {
                    if let Ok(cfg) = Self::from_str(&text).and_then(|cfg| {
                        cfg.validate().map_err(AppError::Config)?;
                        Ok(cfg)
                    }) {
                        let restored =
                            quarantined.is_some() && writer::write_atomic(user_path, &text).is_ok();
                        let warning = match (&quarantined, restored) {
                            (Some(bad), true) => format!(
                                "配置 {} 无法加载（{err_msg}），已备份为 {} 并从配置档「{active}」恢复",
                                user_path.display(),
                                bad.display()
                            ),
                            (Some(bad), false) => format!(
                                "配置 {} 无法加载（{err_msg}），已备份为 {}，本次使用配置档「{active}」运行",
                                user_path.display(),
                                bad.display()
                            ),
                            (None, _) => format!(
                                "配置 {} 无法加载（{err_msg}），本次使用配置档「{active}」运行",
                                user_path.display()
                            ),
                        };
                        tracing::error!("{warning}");
                        return ResolvedConfig {
                            config: cfg,
                            path: Some(user_path.to_path_buf()),
                            warning: Some(warning),
                            degraded: !restored,
                        };
                    }
                }
            }
        }

        let warning = match quarantined {
            Some(bad) => format!(
                "配置 {} 无法加载（{err_msg}），已备份为 {} 并恢复默认配置",
                user_path.display(),
                bad.display()
            ),
            None => format!(
                "配置 {} 无法加载（{err_msg}），本次使用内置默认配置",
                user_path.display()
            ),
        };
        tracing::error!("{warning}");
        let cfg = match Self::write_default_if_absent(user_path) {
            Ok(true) => Self::load_validated(user_path).unwrap_or_default(),
            _ => AppConfig::default(),
        };
        ResolvedConfig {
            config: cfg,
            path: Some(user_path.to_path_buf()),
            warning: Some(warning),
            degraded: true,
        }
    }

    /// 便携模式 / 工作目录配置损坏时保留原路径（避免运行期误写进系统用户配置目录）
    fn load_non_explicit_or_warn(path: PathBuf) -> ResolvedConfig {
        match Self::load_validated(&path) {
            Ok(cfg) => {
                tracing::info!("Loaded config from: {}", path.display());
                ResolvedConfig {
                    config: cfg,
                    path: Some(path),
                    warning: None,
                    degraded: false,
                }
            }
            Err(e) => {
                let warning = format!(
                    "配置文件 {} 加载失败（{e}），本次使用默认配置",
                    path.display()
                );
                tracing::error!("{warning}");
                ResolvedConfig {
                    config: AppConfig::default(),
                    path: Some(path),
                    warning: Some(warning),
                    degraded: true,
                }
            }
        }
    }

    /// 返回一份抹掉密钥的副本，仅用于日志输出
    pub fn redacted(&self) -> Self {
        fn mask(s: &str) -> String {
            if s.is_empty() {
                String::new()
            } else {
                "***".to_string()
            }
        }
        let mut c = self.clone();
        c.translate.api_key = mask(&c.translate.api_key);
        c.asr.api_key = c.asr.api_key.as_deref().map(mask);
        c.asr.edge.trusted_client_token = c.asr.edge.trusted_client_token.as_deref().map(mask);
        // extra_body 是任意请求体字段（网关场景常把 token 放这里），整体扱掉
        if c.translate.extra_body.is_some() {
            c.translate.extra_body = None;
        }
        c
    }

    /// 校验配置（保存设置前调用）
    pub fn validate(&self) -> Result<(), String> {
        let mut errors: Vec<String> = Vec::new();
        let sources = self.audio.effective_sources(self.translate.enabled);

        let mut ids = std::collections::HashSet::new();
        let mut devices = std::collections::HashSet::new();

        for (index, source) in sources.iter().enumerate() {
            let label = format!("输入源 #{}", index + 1);
            if source.id.is_empty() || source.id.len() > 32 {
                errors.push(format!("{label}: id 必须为 1–32 个字符"));
            } else if !source
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                errors.push(format!(
                    "{label}: id「{}」只能包含字母、数字、下划线和短横线",
                    source.id
                ));
            }
            if !ids.insert(source.id.clone()) {
                errors.push(format!("{label}: id「{}」重复", source.id));
            }
            if source.label.trim().is_empty() {
                errors.push(format!("{label}: 展示名不能为空"));
            }
            if source.enabled && source.device.trim().is_empty() {
                errors.push(format!("{label}: 启用中的输入源必须指定设备"));
            }
            if !self.audio.sources.is_empty()
                && source.enabled
                && !source.device.trim().is_empty()
                && !devices.insert(source.device.clone())
            {
                errors.push(format!(
                    "{label}: 设备「{}」已被其它启用中的输入源占用",
                    source.device
                ));
            }
        }

        if self.audio.max_active_sources == 0 || self.audio.max_active_sources > 8 {
            errors.push("audio.max_active_sources 必须在 1–8 之间".to_string());
        }
        if self.audio.sample_rate < 8000 || self.audio.sample_rate > 48000 {
            errors.push("audio.sample_rate 必须在 8000–48000 之间".to_string());
        }
        if !(1..=2).contains(&self.audio.channels) {
            errors.push("audio.channels 必须在 1–2 之间".to_string());
        }
        check_range(
            &mut errors,
            "audio.chunk_seconds",
            self.audio.chunk_seconds,
            0.5,
            10.0,
        );
        check_range(
            &mut errors,
            "audio.silence_threshold",
            self.audio.silence_threshold,
            0.0,
            1.0,
        );

        // 字幕（桌面浮窗）
        if !(1..=10).contains(&self.subtitle.max_lines) {
            errors.push("subtitle.max_lines 必须在 1–10 之间".to_string());
        }
        if !(12..=72).contains(&self.subtitle.font_size) {
            errors.push("subtitle.font_size 必须在 12–72 之间".to_string());
        }
        if self.subtitle.window_width < 160 || self.subtitle.window_width > 7680 {
            errors.push("subtitle.window_width 必须在 160–7680 之间".to_string());
        }
        if self.subtitle.window_height < 80 || self.subtitle.window_height > 4320 {
            errors.push("subtitle.window_height 必须在 80–4320 之间".to_string());
        }

        // VAD
        if !matches!(self.vad.backend.as_str(), "energy" | "silero") {
            errors.push("vad.backend 只能是 energy 或 silero".to_string());
        }
        if !(10..=100).contains(&self.vad.frame_ms) {
            errors.push("vad.frame_ms 必须在 10–100 之间".to_string());
        }
        check_range(&mut errors, "vad.margin_db", self.vad.margin_db, 0.0, 40.0);
        check_range(
            &mut errors,
            "vad.noise_percentile",
            self.vad.noise_percentile,
            0.0,
            1.0,
        );
        check_range(
            &mut errors,
            "vad.silero_threshold",
            self.vad.silero_threshold,
            0.0,
            1.0,
        );
        if self.vad.min_speech_ms > self.vad.max_speech_ms {
            errors.push("vad.min_speech_ms 不能大于 vad.max_speech_ms".to_string());
        }
        if self.vad.max_speech_ms < 500 || self.vad.max_speech_ms > 30_000 {
            errors.push("vad.max_speech_ms 必须在 500–30000 之间".to_string());
        }
        if self.vad.min_silence_ms < 50 || self.vad.min_silence_ms > 5_000 {
            errors.push("vad.min_silence_ms 必须在 50–5000 之间".to_string());
        }

        // 翻译
        if self.translate.timeout_secs < 3 || self.translate.timeout_secs > 300 {
            errors.push("translate.timeout_secs 必须在 3–300 之间".to_string());
        }
        check_range(
            &mut errors,
            "translate.temperature",
            self.translate.temperature,
            0.0,
            2.0,
        );

        // 识别（原始配置编辑器只有 from_str + validate 两道关，这里不能留空）
        if !matches!(self.asr.backend.as_str(), "whisper" | "edge") {
            errors.push("asr.backend 只能是 whisper 或 edge".to_string());
        }
        if !(3..=300).contains(&self.asr.timeout_secs) {
            errors.push("asr.timeout_secs 必须在 3–300 之间".to_string());
        }
        if self.asr.backend == "whisper" {
            if self.asr.base_url.trim().is_empty() {
                errors.push("asr.base_url 不能为空（whisper 后端）".to_string());
            }
            if self.asr.model.trim().is_empty() {
                errors.push("asr.model 不能为空（whisper 后端）".to_string());
            }
            if !self.asr.request_path.starts_with('/') {
                errors.push("asr.request_path 必须以 / 开头".to_string());
            }
        } else {
            let edge_lang = self.asr.edge.language.trim();
            if edge_lang.is_empty() {
                errors.push("asr.edge.language 不能为空（edge 后端）".to_string());
            } else if edge_lang.eq_ignore_ascii_case("auto") {
                errors.push(
                    "asr.edge.language 在 edge 后端下不支持 auto，请指定具体语言（如 en-US）"
                        .to_string(),
                );
            }
            for (index, source) in sources.iter().enumerate() {
                if source.language.trim().eq_ignore_ascii_case("auto") {
                    errors.push(format!(
                        "输入源 #{}: edge 后端不支持单源 language = \"auto\"，请指定具体语言或留空跟随全局",
                        index + 1
                    ));
                }
            }
        }
        if !(3..=120).contains(&self.asr.edge.timeout_secs) {
            errors.push("asr.edge.timeout_secs 必须在 3–120 之间".to_string());
        }
        check_range(
            &mut errors,
            "vad.pre_pad_ms",
            self.vad.pre_pad_ms as f32,
            0.0,
            2000.0,
        );
        check_range(
            &mut errors,
            "vad.post_pad_ms",
            self.vad.post_pad_ms as f32,
            0.0,
            2000.0,
        );

        let vr = &self.output.steamvr;
        if !matches!(vr.anchor.as_str(), "hmd" | "world") {
            errors.push("output.steamvr.anchor 只能是 hmd 或 world".to_string());
        }
        check_range(&mut errors, "output.steamvr.opacity", vr.opacity, 0.0, 1.0);
        check_range(
            &mut errors,
            "output.steamvr.curvature",
            vr.curvature,
            0.0,
            1.0,
        );
        check_range(&mut errors, "output.steamvr.width_m", vr.width_m, 0.3, 5.0);
        check_range(
            &mut errors,
            "output.steamvr.distance_m",
            vr.distance_m,
            0.2,
            10.0,
        );
        check_range(
            &mut errors,
            "output.steamvr.height_offset_m",
            vr.height_offset_m,
            -2.0,
            2.0,
        );
        if !(1..=8).contains(&vr.max_lines) {
            errors.push("output.steamvr.max_lines 必须在 1–8 之间".to_string());
        }
        if !(12..=96).contains(&vr.font_size_px) {
            errors.push("output.steamvr.font_size_px 必须在 12–96 之间".to_string());
        }
        for (index, value) in vr.line_alphas.iter().enumerate() {
            check_range(
                &mut errors,
                &format!("output.steamvr.line_alphas[{index}]"),
                *value,
                0.0,
                1.0,
            );
        }
        for (index, value) in vr.world_position.iter().enumerate() {
            if !value.is_finite() {
                errors.push(format!(
                    "output.steamvr.world_position[{index}] 必须是有限值"
                ));
            }
        }
        for (index, value) in vr.world_rotation_deg.iter().enumerate() {
            if !value.is_finite() {
                errors.push(format!(
                    "output.steamvr.world_rotation_deg[{index}] 必须是有限值"
                ));
            }
        }
        for (key, color) in [
            ("background", &vr.background),
            ("text_color", &vr.text_color),
            ("source_color", &vr.source_color),
            ("label_color", &vr.label_color),
        ] {
            if !is_hex_color(color) {
                errors.push(format!(
                    "output.steamvr.{key} 必须是 #RGB / #RRGGBB / #RRGGBBAA"
                ));
            }
        }

        let osc = &self.output.osc;
        if !matches!(osc.mode.as_str(), "rolling" | "latest") {
            errors.push("output.osc.mode 只能是 rolling 或 latest".to_string());
        }
        if !matches!(osc.content.as_str(), "translated" | "source" | "both") {
            errors.push("output.osc.content 只能是 translated / source / both".to_string());
        }
        if !(1..=9).contains(&osc.max_lines) {
            errors.push("output.osc.max_lines 必须在 1–9 之间".to_string());
        }
        if osc.max_chars < 8 || osc.max_chars > 144 {
            errors.push("output.osc.max_chars 必须在 8–144 之间".to_string());
        }
        if osc.min_interval_ms < 200 || osc.min_interval_ms > 10_000 {
            errors.push("output.osc.min_interval_ms 必须在 200–10000 之间".to_string());
        }
        if !(1..=10).contains(&osc.max_per_window) {
            errors.push("output.osc.max_per_window 必须在 1–10 之间".to_string());
        }
        let sep_len = osc.separator.chars().count();
        if sep_len == 0 || sep_len > 4 {
            errors.push("output.osc.separator 必须是 1–4 个字符".to_string());
        }
        if parse_target(&osc.target).is_none() {
            errors.push("output.osc.target 必须是合法的 host:port".to_string());
        }
        if !osc.custom_address.trim().is_empty() && parse_target(&osc.custom_address).is_none() {
            errors.push("output.osc.custom_address 留空或填合法的 host:port".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            audio: AudioConfig {
                device_name: default_device_name(),
                chunk_seconds: default_chunk_seconds(),
                sample_rate: default_sample_rate(),
                channels: default_channels(),
                silence_threshold: default_silence_threshold(),
                max_active_sources: default_max_active_sources(),
                sources: Vec::new(),
            },
            asr: AsrConfig {
                backend: "whisper".to_string(),
                base_url: "http://127.0.0.1:8765".to_string(),
                model: "whisper-1".to_string(),
                language: "auto".to_string(),
                timeout_secs: 20,
                request_path: "/v1/audio/transcriptions".to_string(),
                api_key: None,
                auth_header: None,
                edge: EdgeAsrConfig::default(),
                filter_hallucination: true,
            },
            translate: TranslateConfig {
                enabled: true,
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: "YOUR_API_KEY".to_string(),
                model: "gpt-4o-mini".to_string(),
                target_language: "zh-CN".to_string(),
                system_prompt:
                    "You are a real-time subtitle translator. Translate naturally and concisely."
                        .to_string(),
                timeout_secs: 20,
                send_temperature: true,
                temperature: 0.2,
                disable_thinking: String::new(),
                extra_body: None,
            },
            subtitle: SubtitleConfig {
                max_lines: 3,
                font_size: 28,
                text_color: "#FFFFFF".to_string(),
                stroke_color: "#000000".to_string(),
                background: default_panel_background(),
                toolbar_background: default_toolbar_background(),
                show_source: false,
                window_width: 1200,
                window_height: 260,
                position_x: 200,
                position_y: 760,
                always_on_top: true,
                click_through: false,
                label_sources: LabelMode::Auto,
            },
            vad: VadConfig::default(),
            output: OutputConfig::default(),
        }
    }
}

/// 写入默认配置，但只在文件不存在时创建。
///
/// 返回 `Ok(false)` 表示文件已存在（说明别的实例/用户抢先创建了），不覆盖。
fn write_private_if_absent(path: &Path, content: &str) -> std::io::Result<bool> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    };
    use std::io::Write;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(true)
}

/// TOML 解析错误的可展示文本。
///
/// `toml` / `toml_edit` 的 `Display` 会把出错那一行的**原文**拼进去，
/// 而配置文件里有 `api_key`（少写一个引号就会把密钥打进日志、
/// 设置页的错误条和 IPC 返回值）。这里只保留错误消息本身。
pub fn toml_error_text(e: &toml::de::Error) -> String {
    e.message().to_string()
}

fn check_range(errors: &mut Vec<String>, name: &str, value: f32, min: f32, max: f32) {
    if !value.is_finite() || value < min || value > max {
        errors.push(format!("{name} 必须在 {min}–{max} 之间"));
    }
}

/// `#RGB` / `#RRGGBB` / `#RRGGBBAA`
pub fn is_hex_color(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('#') else {
        return false;
    };
    matches!(rest.len(), 3 | 6 | 8) && rest.chars().all(|c| c.is_ascii_hexdigit())
}

/// 解析 `host:port`，返回 `(host, port)`
pub fn parse_target(value: &str) -> Option<(String, u16)> {
    let trimmed = value.trim();
    let (host, port) = trimmed.rsplit_once(':')?;
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    if host.starts_with('[') || host.ends_with(']') {
        let inner = host.strip_prefix('[')?.strip_suffix(']')?;
        inner.parse::<std::net::Ipv6Addr>().ok()?;
    } else if host.contains(':') {
        // 未加方括号的 IPv6（如 "::1:9000"）无法被 ToSocketAddrs 解析
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp_config_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("livetranslate-test-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir.join("config.toml")
    }

    #[test]
    fn write_default_creates_file_once() {
        let path = temp_config_path("write-default");

        assert!(AppConfig::write_default_if_absent(&path).unwrap());
        assert!(path.exists());
        assert!(!AppConfig::write_default_if_absent(&path).unwrap());

        let cfg = AppConfig::from_file(&path).unwrap();
        assert_eq!(cfg.translate.target_language, "zh-CN");
        assert_eq!(cfg.audio.sample_rate, 16000);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn user_config_path_is_toml_file() {
        let path = AppConfig::user_config_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "config.toml");
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let missing = PathBuf::from("/nonexistent/definitely/not/here.toml");
        assert!(AppConfig::resolve(Some(missing)).is_err());
    }

    #[test]
    fn asr_api_key_is_optional_and_backward_compatible() {
        let old = r#"
            base_url = "http://127.0.0.1:8765"
            model = "whisper-1"
            language = "auto"
            timeout_secs = 20
            request_path = "/v1/audio/transcriptions"
        "#;
        let cfg: AsrConfig = toml::from_str(old).unwrap();
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.auth_header, None);

        let with_key = r#"
            base_url = "https://api.example.com"
            model = "whisper-1"
            language = "auto"
            timeout_secs = 20
            request_path = "/v1/audio/transcriptions"
            api_key = "sk-test"
            auth_header = "api-key"
        "#;
        let cfg: AsrConfig = toml::from_str(with_key).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("sk-test"));
        assert_eq!(cfg.auth_header.as_deref(), Some("api-key"));
    }

    #[test]
    fn old_config_synthesizes_single_source() {
        let toml = AppConfig::default().to_toml_string().unwrap();
        let mut value: toml::Value = toml::from_str(&toml).unwrap();
        // 模拟 v0.1.6 配置：没有 sources / output / label_sources / max_active_sources
        let audio = value.get_mut("audio").unwrap().as_table_mut().unwrap();
        audio.remove("sources");
        audio.remove("max_active_sources");
        audio.insert(
            "device_name".into(),
            toml::Value::String("mic:1".to_string()),
        );
        value.as_table_mut().unwrap().remove("output");
        let subtitle = value.get_mut("subtitle").unwrap().as_table_mut().unwrap();
        subtitle.remove("label_sources");

        let cfg: AppConfig = toml::from_str(&toml::to_string(&value).unwrap()).unwrap();
        let sources = cfg.audio.effective_sources(cfg.translate.enabled);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "default");
        assert_eq!(sources[0].device, "mic:1");
        assert!(sources[0].translate);
        assert!(!cfg.output.osc.enabled);
        assert!(!cfg.output.steamvr.enabled);
        assert_eq!(cfg.subtitle.label_sources, LabelMode::Auto);
    }

    #[test]
    fn validation_rejects_duplicate_ids_and_devices() {
        let mut cfg = AppConfig::default();
        cfg.audio.sources = vec![
            AudioSourceConfig {
                id: "mic".into(),
                label: "麦".into(),
                device: "mic:0".into(),
                ..AudioSourceConfig::new("mic", "麦", "mic:0")
            },
            AudioSourceConfig {
                id: "mic".into(),
                label: "麦2".into(),
                device: "mic:0".into(),
                ..AudioSourceConfig::new("mic", "麦2", "mic:0")
            },
        ];
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("重复"));
        assert!(err.contains("设备"));
    }

    #[test]
    fn validation_accepts_defaults() {
        AppConfig::default().validate().unwrap();
    }

    #[test]
    fn shipped_default_toml_parses_and_validates() {
        let cfg: AppConfig = toml::from_str(include_str!("../../config/default.toml")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.audio.sources.len(), 2);
        assert!(!cfg.audio.sources[1].enabled);
        assert_eq!(cfg.subtitle.label_sources, LabelMode::Auto);
        assert_eq!(cfg.output.osc.mode, "rolling");
        assert_eq!(cfg.output.osc.max_lines, 4);
    }

    #[test]
    fn color_and_target_parsing() {
        assert!(is_hex_color("#FFF"));
        assert!(is_hex_color("#FFFFFF"));
        assert!(is_hex_color("#FFFFFF00"));
        assert!(!is_hex_color("rgba(0,0,0,0.5)"));
        assert_eq!(
            parse_target(" 127.0.0.1:9000 "),
            Some(("127.0.0.1".to_string(), 9000))
        );
        assert_eq!(
            parse_target("[::1]:9000"),
            Some(("[::1]".to_string(), 9000))
        );
        assert!(parse_target("::1:9000").is_none());
        assert!(parse_target("[not-ipv6]:9000").is_none());
        assert!(parse_target("127.0.0.1").is_none());
        assert!(parse_target("host:notaport").is_none());
    }

    #[test]
    fn validation_rejects_nan_rotation_and_bad_custom_address() {
        let mut cfg = AppConfig::default();
        cfg.output.steamvr.world_rotation_deg[1] = f32::NAN;
        cfg.output.osc.custom_address = "/chatbox/custom".to_string();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("world_rotation_deg[1]"), "{err}");
        assert!(err.contains("custom_address"), "{err}");

        cfg.output.steamvr.world_rotation_deg[1] = 15.0;
        cfg.output.osc.custom_address = "127.0.0.1:9001".to_string();
        cfg.validate().unwrap();
    }

    #[test]
    fn non_explicit_corrupt_config_preserves_path_and_warns() {
        let path = temp_config_path("corrupt-portable");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not valid toml [[[").unwrap();

        let resolved = AppConfig::load_non_explicit_or_warn(path.clone());
        assert_eq!(resolved.path.as_deref(), Some(path.as_path()));
        assert!(resolved.warning.is_some());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "not valid toml [[[",
            "便携/开发配置损坏时不应被覆盖或移动"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validation_rejects_edge_auto_language_on_global_and_source() {
        let mut cfg = AppConfig::default();
        cfg.asr.backend = "edge".to_string();
        cfg.asr.edge.language = "auto".to_string();
        assert!(cfg.validate().is_err());

        cfg.asr.edge.language = "en-US".to_string();
        let mut src = AudioSourceConfig::new("mic", "麦克风", "mic:0");
        src.language = "auto".to_string();
        cfg.audio.sources = vec![src];
        assert!(cfg.validate().is_err());

        cfg.audio.sources[0].language = "ja-JP".to_string();
        cfg.validate().unwrap();
    }

    #[test]
    fn recover_user_config_restores_from_valid_active_profile() {
        let path = temp_config_path("recover-profile");
        let mut work = AppConfig::default();
        work.translate.api_key = "sk-keep-me".to_string();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, work.to_toml_string().unwrap()).unwrap();
        profiles::ensure_initialized(&path).unwrap();
        profiles::create(&path, "work", Some(&path)).unwrap();
        profiles::switch_to(&path, "work").unwrap();

        // 模拟权威 config.toml 损坏，但激活配置档 work.toml 完好
        fs::write(&path, "broken toml [[[").unwrap();
        let resolved = AppConfig::recover_user_config(&path, "parse error");
        assert!(!resolved.degraded);
        assert_eq!(resolved.config.translate.api_key, "sk-keep-me");
        assert_eq!(
            AppConfig::from_file(&path).unwrap().translate.api_key,
            "sk-keep-me"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn recover_user_config_marks_degraded_without_valid_profile() {
        let path = temp_config_path("recover-degraded");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "broken toml [[[").unwrap();

        let resolved = AppConfig::recover_user_config(&path, "parse error");
        assert!(resolved.degraded);
        assert!(resolved.warning.is_some());
        assert!(path.exists());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
