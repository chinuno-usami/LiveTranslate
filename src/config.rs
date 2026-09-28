use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateConfig {
    pub base_url: String,
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
pub struct AppConfig {
    pub audio: AudioConfig,
    pub asr: AsrConfig,
    pub translate: TranslateConfig,
    pub subtitle: SubtitleConfig,
}

impl AppConfig {
    /// 从配置文件加载
    pub fn load(path: Option<PathBuf>) -> AppResult<Self> {
        let config_path = if let Some(p) = path {
            p
        } else {
            Self::default_config_path()?
        };

        if !config_path.exists() {
            return Err(AppError::Config(format!(
                "Config file not found: {}",
                config_path.display()
            )));
        }

        let content = fs::read_to_string(&config_path)?;
        let config: AppConfig = toml::from_str(&content)?;
        
        tracing::info!("Loaded config from: {}", config_path.display());
        Ok(config)
    }

    /// 获取默认配置文件路径
    pub fn default_config_path() -> AppResult<PathBuf> {
        let exe_dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| AppError::Config("Failed to get exe directory".to_string()))?
            .to_path_buf();

        // 首先尝试当前目录
        let local_path = exe_dir.join("config").join("default.toml");
        if local_path.exists() {
            return Ok(local_path);
        }

        // 尝试项目根目录（开发模式）
        let project_path = PathBuf::from("config/default.toml");
        if project_path.exists() {
            return Ok(project_path);
        }

        // 默认返回项目路径
        Ok(project_path)
    }

    /// 保存配置到文件
    pub fn save(&self, path: &Path) -> AppResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| AppError::TomlSerialize(e))?;
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
        }
    }
}
