use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Audio error: {0}")]
    Audio(String),

    #[error("ASR error: {0}")]
    Asr(String),

    #[error("Translation error: {0}")]
    Translation(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("TOML 解析失败: {0}")]
    TomlDeserialize(String),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("CPAL error: {0}")]
    CpalDevices(String),

    #[error("CPAL build stream error: {0}")]
    CpalBuildStream(String),

    #[error("CPAL play stream error: {0}")]
    CpalPlayStream(String),

    /// 采集流在收到停止信号之前就断了（USB 拔出 / 设备被抢占）
    #[error("音频输入已中断（设备断开？）")]
    AudioStreamEnded,
}

impl From<cpal::Error> for AppError {
    fn from(err: cpal::Error) -> Self {
        AppError::CpalDevices(format!("CPAL error: {}", err))
    }
}

pub type AppResult<T> = Result<T, AppError>;
