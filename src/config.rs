use serde::{Deserialize, Serialize};
use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use crate::error::{AppError, AppResult};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateConfig {    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub target_language: String,
    pub system_prompt: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleConfig {
    pub max_lines: usize,
    pub font_size: u32,
    pub text_color: String,
    pub stroke_color: String,
    pub background: String,
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
    /// VAD 帧长（毫秒），推荐 20
    #[serde(default = "default_vad_frame_ms")]
    pub frame_ms: u32,
    /// 判为语音所需的、高于自适应噪声底的余量（dB）
    #[serde(default = "default_vad_margin_db")]
    pub margin_db: f32,
    /// 噪声底估计所用的分位数（0.0-1.0）
    #[serde(default = "default_vad_noise_percentile")]
    pub noise_percentile: f32,
    /// 语音需持续多久才确认开始（也是最短片段长度）
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
            frame_ms: 20,
            margin_db: 8.0,
            noise_percentile: 0.1,
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
fn default_vad_frame_ms() -> u32 {
    VadConfig::default().frame_ms
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
        let content = toml::to_string_pretty(&AppConfig::default())
            .map_err(AppError::TomlSerialize)?;
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

    /// 保存配置到文件
    pub fn save(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(AppError::TomlSerialize)?;
        fs::write(path, content)?;
        tracing::info!("Config saved to: {}", path.display());
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
                base_url: "http://127.0.0.1:8765".to_string(),
                model: "whisper-1".to_string(),
                language: "auto".to_string(),
                timeout_secs: 20,
                request_path: "/v1/audio/transcriptions".to_string(),
                api_key: None,
                auth_header: None,
            },
            translate: TranslateConfig {
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: "YOUR_API_KEY".to_string(),
                model: "gpt-4o-mini".to_string(),
                target_language: "zh-CN".to_string(),
                system_prompt: "You are a real-time subtitle translator. Translate naturally and concisely.".to_string(),
                timeout_secs: 20,
            },
            subtitle: SubtitleConfig {
                max_lines: 3,
                font_size: 28,
                text_color: "#FFFFFF".to_string(),
                stroke_color: "#000000".to_string(),
                background: "transparent".to_string(),
                show_source: false,
                window_width: 1200,
                window_height: 220,
                position_x: 200,
                position_y: 760,
                always_on_top: true,
                click_through: false,
            },
            vad: VadConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "livetranslate-test-{}-{}",
            std::process::id(),
            tag
        ));
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
