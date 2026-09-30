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

    #[error("TOML deserialize error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

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
}

impl From<cpal::Error> for AppError {
    fn from(err: cpal::Error) -> Self {
        AppError::CpalDevices(format!("CPAL error: {}", err))
    }
}

pub type AppResult<T> = Result<T, AppError>;
