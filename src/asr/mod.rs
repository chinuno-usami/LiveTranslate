//! 语音识别后端
//!
//! 通过 `[asr] backend` 在多个后端之间切换：
//! - `whisper`：自建 / 兼容的 Whisper HTTP 服务（默认）
//! - `edge`：微软 Edge 内置语音识别的在线服务

pub mod cleaner;
pub mod edge;
pub mod whisper;

use crate::config::AsrConfig;
use crate::error::{AppError, AppResult};

pub use edge::EdgeAsrClient;
pub use whisper::WhisperClient;

/// 可选的识别后端
pub enum AsrEngine {
    Whisper(WhisperClient),
    Edge(EdgeAsrClient),
}

impl AsrEngine {
    /// 依据配置构造后端
    pub fn from_config(config: &AsrConfig) -> AppResult<Self> {
        match config.backend.trim().to_ascii_lowercase().as_str() {
            "edge" => Ok(AsrEngine::Edge(EdgeAsrClient::new(&config.edge))),
            "" | "whisper" => Ok(AsrEngine::Whisper(WhisperClient::new(config.clone()))),
            other => Err(AppError::Config(format!(
                "未知的 asr.backend: {other}（可选: whisper | edge）"
            ))),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            AsrEngine::Whisper(_) => "whisper",
            AsrEngine::Edge(_) => "edge",
        }
    }

    /// 用于日志与错误提示的目标描述
    pub fn endpoint(&self) -> String {
        match self {
            AsrEngine::Whisper(client) => client.endpoint(),
            AsrEngine::Edge(client) => client.describe(),
        }
    }

    /// 识别一段单声道音频
    pub async fn transcribe(&self, samples: &[f32], sample_rate: u32) -> AppResult<String> {
        match self {
            AsrEngine::Whisper(client) => client.transcribe(samples, sample_rate).await,
            AsrEngine::Edge(client) => client.transcribe(samples, sample_rate).await,
        }
    }
}
