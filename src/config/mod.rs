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
    #[serde(default)]
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
            world_position: [0.0, 1.3, -1.6],
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
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// 从 TOML 文本解析配置
    pub fn from_str(content: &str) -> AppResult<Self> {
        Ok(toml::from_str(content)?)
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
        fs::write(path, content)?;
        Ok(true)
    }

    /// 按优先级解析配置，返回 `(配置, 来源路径)`
    pub fn resolve(explicit: Option<PathBuf>) -> AppResult<(Self, Option<PathBuf>)> {
        if let Some(path) = explicit {
            if !path.exists() {
                return Err(AppError::Config(format!(
                    "Config file not found: {}",
                    path.display()
                )));
            }
            let cfg = Self::from_file(&path)?;
            tracing::info!("Loaded config from: {}", path.display());
            return Ok((cfg, Some(path)));
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let p = dir.join("config").join("default.toml");
                if p.exists() {
                    let cfg = Self::from_file(&p)?;
                    tracing::info!("Loaded config from: {}", p.display());
                    return Ok((cfg, Some(p)));
                }
            }
        }

        let cwd_path = PathBuf::from("config/default.toml");
        if cwd_path.exists() {
            let cfg = Self::from_file(&cwd_path)?;
            tracing::info!("Loaded config from: {}", cwd_path.display());
            return Ok((cfg, Some(cwd_path)));
        }

        if let Ok(user_path) = Self::user_config_path() {
            if user_path.exists() {
                let cfg = Self::from_file(&user_path)?;
                tracing::info!("Loaded config from: {}", user_path.display());
                return Ok((cfg, Some(user_path)));
            }

            match Self::write_default_if_absent(&user_path) {
                Ok(true) => {
                    tracing::info!(
                        "Created default config at: {} (请填入翻译 API Key)",
                        user_path.display()
                    );
                    let cfg = Self::from_file(&user_path).unwrap_or_default();
                    return Ok((cfg, Some(user_path)));
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

        let hint = Self::user_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        tracing::warn!(
            "No config file found; using built-in defaults. \
             Create one at: {}",
            hint
        );
        Ok((AppConfig::default(), None))
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

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    /// 就地修改配置文件里的单个键值（**保留注释与原有排版**）
    #[allow(dead_code)]
    pub fn update_value_in_file(
        path: &Path,
        section: &str,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let original = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let updated = patch_toml(&original, section, key, value);
        if updated != original {
            writer::write_atomic(path, &updated).map_err(AppError::Config)?;
        }
        Ok(())
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
    let (host, port) = value.rsplit_once(':')?;
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_string(), port))
}

/// 在 TOML 文本中就地修改 `[section]` 下的 `key = value`
///
/// 保留其余内容（包括注释与空行）。结构化写入请优先使用 [`writer::apply_changes`]。
#[allow(dead_code)]
fn patch_toml(content: &str, section: &str, key: &str, value: &str) -> String {
    let section_header = format!("[{section}]");
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    let mut section_start: Option<usize> = None;
    let mut section_end = lines.len();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if trimmed == section_header {
                section_start = Some(i);
            } else if section_start.is_some() {
                section_end = i;
                break;
            }
        }
    }

    let trailing_newline = content.ends_with('\n');
    let join = |lines: Vec<String>| {
        let mut s = lines.join("\n");
        if trailing_newline {
            s.push('\n');
        }
        s
    };

    if let Some(start) = section_start {
        for (i, line) in lines.iter().enumerate().take(section_end).skip(start + 1) {
            let indent_len = line.len() - line.trim_start().len();
            let trimmed = line.trim_start();
            let matches_key = trimmed
                .strip_prefix(key)
                .map(|rest| rest.trim_start().starts_with('='))
                .unwrap_or(false);

            if matches_key {
                let indent = &line[..indent_len];
                lines[i] = format!("{indent}{key} = {value}");
                return join(lines);
            }
        }

        lines.insert(section_end, format!("{key} = {value}"));
        return join(lines);
    }

    let mut out = content.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("[{section}]\n{key} = {value}\n"));
    out
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
    fn patch_toml_replaces_existing_key_and_keeps_comments() {
        let src = "# 顶部注释\n\n[subtitle]\n# 字号注释\nfont_size = 28\nmax_lines = 3\n";
        let out = patch_toml(src, "subtitle", "font_size", "40");
        assert!(out.contains("# 顶部注释"), "注释应保留");
        assert!(out.contains("# 字号注释"), "行内注释应保留");
        assert!(out.contains("font_size = 40"), "值应被替换");
        assert!(out.contains("max_lines = 3"), "其他键不应变动");
        assert!(!out.contains("font_size = 28"));
    }

    #[test]
    fn patch_toml_inserts_missing_key_inside_section() {
        let src = "[subtitle]\nfont_size = 28\n\n[other]\nkey = 1\n";
        let out = patch_toml(src, "subtitle", "show_source", "true");
        let subtitle_part = out.split("[other]").next().unwrap();
        assert!(
            subtitle_part.contains("show_source = true"),
            "应插入到 subtitle 段内"
        );
        assert!(out.contains("[other]\nkey = 1"), "其他段不应受影响");
    }

    #[test]
    fn patch_toml_appends_missing_section() {
        let out = patch_toml("[audio]\nchunk_seconds = 2.0\n", "vad", "enabled", "true");
        assert!(out.contains("[vad]"));
        assert!(out.contains("enabled = true"));
        assert!(out.contains("[audio]"));
    }

    #[test]
    fn patch_toml_does_not_confuse_prefix_keys() {
        let src = "[subtitle]\nfont_size_extra = 1\n";
        let out = patch_toml(src, "subtitle", "font_size", "40");
        assert!(out.contains("font_size_extra = 1"), "不应误改前缀相同的键");
        assert!(out.contains("font_size = 40"));
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
            parse_target("127.0.0.1:9000"),
            Some(("127.0.0.1".to_string(), 9000))
        );
        assert!(parse_target("127.0.0.1").is_none());
        assert!(parse_target("host:notaport").is_none());
    }
}
