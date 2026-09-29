//! 语音识别后端
//!
//! 通过 `[asr] backend` 在多个后端之间切换：
//! - `whisper`：自建 / 兼容的 Whisper HTTP 服务（默认）
//! - `edge`：微软 Edge 内置语音识别的在线服务

pub mod cleaner;
pub mod edge;
pub mod languages;
pub mod whisper;

use tokio::sync::watch;

use crate::config::AsrConfig;
use crate::error::{AppError, AppResult};

pub use edge::EdgeAsrClient;
pub use whisper::WhisperClient;

enum AsrImpl {
    Whisper(WhisperClient),
    Edge(EdgeAsrClient),
}

/// 识别引擎
///
/// 持有的识别语言来自 `watch` 通道，因此面板上切换语言会**立即生效**，
/// 不需要重启采集流水线。
pub struct AsrEngine {
    inner: AsrImpl,
    language: watch::Receiver<String>,
}

impl AsrEngine {
    pub fn from_config(
        config: &AsrConfig,
        language: watch::Receiver<String>,
    ) -> AppResult<Self> {
        let backend = config.backend.trim().to_ascii_lowercase();

        let inner = match backend.as_str() {
            "edge" => AsrImpl::Edge(EdgeAsrClient::new(&config.edge)),
            "" | "whisper" => AsrImpl::Whisper(WhisperClient::new(config.clone())),
            other => {
                return Err(AppError::Config(format!(
                    "未知的 asr.backend: {other}（可选: whisper | edge）"
                )))
            }
        };

        Ok(Self { inner, language })
    }

    pub fn name(&self) -> &'static str {
        match self.inner {
            AsrImpl::Whisper(_) => "whisper",
            AsrImpl::Edge(_) => "edge",
        }
    }

    /// 用于日志与错误提示的目标描述
    pub fn endpoint(&self) -> String {
        match &self.inner {
            AsrImpl::Whisper(client) => client.endpoint(),
            AsrImpl::Edge(client) => client.describe(),
        }
    }

    /// 当前识别语言（面板改动后会立即反映）
    pub fn language(&self) -> String {
        self.language.borrow().clone()
    }

    /// 识别一段单声道音频
    pub async fn transcribe(&self, samples: &[f32], sample_rate: u32) -> AppResult<String> {
        // 取一次快照，避免在 await 期间持有 watch 的读锁
        let language = self.language();

        match &self.inner {
            AsrImpl::Whisper(client) => client.transcribe(samples, sample_rate, &language).await,
            AsrImpl::Edge(client) => client.transcribe(samples, sample_rate, &language).await,
        }
    }
}
