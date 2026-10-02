use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub device_name: String,
    pub chunk_seconds: f32,
    pub sample_rate: u32,
    pub channels: u16,
    pub silence_threshold: f32,
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
    ///
    /// 能量型 VAD 无法区分音乐与人声，音乐片段仍会被送去识别，
    /// 典型结果是 `[Music]`、`♪♪♪`、"感谢观看" 之类的固定幻听文本。
    /// 开启后会丢弃这些输出，同时避免无谓的翻译请求。
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
    ///
    /// - `"reasoning_effort"`：发送 `reasoning_effort = "minimal"`
    ///   （OpenAI o 系列 / gpt-5 等）
    /// - `"enable_thinking"`：发送 `enable_thinking = false`（Qwen3 等）
    /// - `"chat_template"`：发送
    ///   `chat_template_kwargs = { enable_thinking = false }`
    ///   （vLLM / SGLang 部署的 Qwen3 等）
    ///
    /// 思考会让翻译首字延迟大幅上升，实时字幕场景通常应当关闭。
    #[serde(default)]
    pub disable_thinking: String,
    /// 追加到请求体的自定义字段（最高优先级，会覆盖上面的预设）
    ///
    /// 各家关闭思考的字段名不统一，这里留一个通用口子。例如：
    /// ```toml
    /// [translate.extra_body]
    /// reasoning_effort = "none"
    /// ```
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
    /// - `"transparent"`：完全透明，只显示文字
    /// - `"rgba(8, 10, 14, 0.6)"`：半透明黑，数值越大越不透明
    #[serde(default = "default_panel_background")]
    pub background: String,
    /// 顶部工具栏背景（任意 CSS 颜色，含 alpha）
    /// - `"transparent"`：完全透明
    /// - `"rgba(10, 12, 18, 0.42)"`：半透明黑
    #[serde(default = "default_toolbar_background")]
    pub toolbar_background: String,
    pub show_source: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub position_x: i32,
    pub position_y: i32,
    pub always_on_top: bool,
    pub click_through: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// 是否启用 VAD 分段（关闭则回到固定时长切片）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// VAD 算法后端：
    /// - `energy`：内置能量型（无额外依赖，默认）
    /// - `silero`：Silero VAD 神经网络（需 ONNX Runtime，能区分音乐与人声）
    #[serde(default = "default_vad_backend")]
    pub backend: String,
    /// VAD 帧长（毫秒），推荐 20（仅 energy 后端使用）
    #[serde(default = "default_vad_frame_ms")]
    pub frame_ms: u32,
    /// 判为语音所需的、高于自适应噪声底的余量（dB，仅 energy 后端）
    #[serde(default = "default_vad_margin_db")]
    pub margin_db: f32,
    /// 噪声底估计所用的分位数（0.0-1.0，仅 energy 后端）
    #[serde(default = "default_vad_noise_percentile")]
    pub noise_percentile: f32,
    /// Silero 模型路径；留空则使用内嵌模型
    #[serde(default)]
    pub silero_model: Option<String>,
    /// Silero 判为语音的概率阈值
    #[serde(default = "default_silero_threshold")]
    pub silero_threshold: f32,
    /// 最短语音时长（短于此的片段被丢弃；起始不再要求连续这么多帧）
    #[serde(default = "default_vad_min_speech_ms")]
    pub min_speech_ms: u32,
    /// 静音需持续多久才确认说完
    #[serde(default = "default_vad_min_silence_ms")]
    pub min_silence_ms: u32,
    /// 单个片段最长时长，超过则强制切分（控制延迟）
    #[serde(default = "default_vad_max_speech_ms")]
    pub max_speech_ms: u32,
    /// 开始前保留的音频，避免吃掉首音素
    #[serde(default = "default_vad_pre_pad_ms")]
    pub pre_pad_ms: u32,
    /// 结束后保留的音频，避免吃掉尾音素
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

fn default_true() -> bool {
    true
}
fn default_panel_background() -> String {
    // 与 tauri-ui/styles.css 里的 --panel-bg 保持一致
    "rgba(8, 10, 14, 0.28)".to_string()
}
fn default_toolbar_background() -> String {
    // 与 tauri-ui/styles.css 里的 --toolbar-bg 保持一致
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub audio: AudioConfig,
    pub asr: AsrConfig,
    pub translate: TranslateConfig,
    pub subtitle: SubtitleConfig,
    /// VAD 分段配置（旧配置文件没有该段时使用默认值）
    #[serde(default)]
    pub vad: VadConfig,
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
    ///
    /// - macOS: `~/Library/Application Support/com.chinuno.LiveTranslate/logs`
    /// - Windows: `%LOCALAPPDATA%\chinuno\LiveTranslate\logs`
    /// - Linux: `~/.local/share/livetranslate/logs`
    pub fn log_dir() -> AppResult<PathBuf> {
        ProjectDirs::from("com", "chinuno", "LiveTranslate")
            .map(|dirs| dirs.data_local_dir().join("logs"))
            .ok_or_else(|| AppError::Config("Failed to determine log directory".to_string()))
    }

    /// 从指定文件读取配置
    fn from_file(path: &Path) -> AppResult<Self> {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// 若目标文件不存在，则写入一份默认配置（并创建父目录）
    ///
    /// 返回是否实际写入了文件。
    pub fn write_default_if_absent(path: &Path) -> AppResult<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content =
            toml::to_string_pretty(&AppConfig::default()).map_err(AppError::TomlSerialize)?;
        fs::write(path, content)?;
        Ok(true)
    }

    /// 按优先级解析配置，返回 `(配置, 来源路径)`
    ///
    /// 查找顺序：
    /// 1. `--config` 显式指定
    /// 2. 可执行文件同级 `config/default.toml`（便携版）
    /// 3. 当前工作目录 `config/default.toml`（开发/源码运行）
    /// 4. 用户配置目录（首次运行自动生成默认文件）
    /// 5. 内置默认值
    pub fn resolve(explicit: Option<PathBuf>) -> AppResult<(Self, Option<PathBuf>)> {
        // 1. 显式指定：找不到就直接报错，避免静默用错配置
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

        // 2. 可执行文件同级（Windows 便携包 / 解压即用）
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

        // 3. 当前工作目录（开发模式）
        let cwd_path = PathBuf::from("config/default.toml");
        if cwd_path.exists() {
            let cfg = Self::from_file(&cwd_path)?;
            tracing::info!("Loaded config from: {}", cwd_path.display());
            return Ok((cfg, Some(cwd_path)));
        }

        // 4. 用户配置目录（打包后的 .app / exe 走这里）
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
                    return Ok((AppConfig::default(), Some(user_path)));
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

        // 5. 内置默认
        let hint = Self::user_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        tracing::warn!(
            "No config file found; using built-in defaults (translation api_key is a placeholder). \
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

    /// 就地修改配置文件里的单个键值（**保留注释与原有排版**）
    ///
    /// 不直接重写整个文件，因为那会丢失用户在配置里写的注释。
    pub fn update_value_in_file(
        path: &Path,
        section: &str,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        // 多个命令（异步线程 + 主线程）可能同时写配置，串行化读改写
        static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // 读取失败（非 UTF-8、被占用等）必须报错：若当成空文件，
        // 写回时只剩一个键值，会抹掉用户整个配置（包括 API Key）
        let original = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let updated = patch_toml(&original, section, key, value);
        if updated != original {
            // 先写临时文件再 rename，避免中途崩溃/并发读看到截断的文件
            let tmp = path.with_extension("toml.tmp");
            fs::write(&tmp, updated)?;
            fs::rename(&tmp, path)?;
        }
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            audio: AudioConfig {
                device_name: "default".to_string(),
                chunk_seconds: 2.0,
                sample_rate: 16000,
                channels: 1,
                silence_threshold: 0.01,
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
            },
            vad: VadConfig::default(),
        }
    }
}

/// 在 TOML 文本中就地修改 `[section]` 下的 `key = value`
///
/// 保留其余内容（包括注释与空行）。
/// - 找到 section 且找到 key：替换该行
/// - 找到 section 但没找到 key：插入到 section 末尾
/// - 整个 section 不存在：在文件末尾追加
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

        // 首次应写入
        assert!(AppConfig::write_default_if_absent(&path).unwrap());
        assert!(path.exists());

        // 已存在则不覆盖
        assert!(!AppConfig::write_default_if_absent(&path).unwrap());

        // 写出的文件必须能解析回 AppConfig
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
        // 旧配置文件（没有 api_key / auth_header）必须仍能解析
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

        // 带 token 的配置
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
}
