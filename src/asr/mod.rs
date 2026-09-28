use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use crate::config::AsrConfig;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperResponse {
    pub text: String,
}

#[derive(Clone)]
pub struct WhisperClient {
    client: Client,
    pub config: AsrConfig,
}

impl WhisperClient {
    pub fn new(config: AsrConfig) -> Self {
        let client = Client::new();
        Self { client, config }
    }

    /// 识别音频
    pub async fn transcribe(&self, audio_data: Vec<u8>) -> AppResult<String> {
        let url = format!(
            "{}{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.request_path
        );

        tracing::info!("Sending audio to ASR: {} (size: {} bytes)", url, audio_data.len());

        // 使用 multipart form data
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(audio_data).file_name("audio.wav"))
            .text("model", self.config.model.clone())
            .text("language", self.config.language.clone());

        let response = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(AppError::Asr(format!(
                "ASR request failed with status {}: {}",
                status, error_text
            )));
        }

        let whisper_response: WhisperResponse = response.json().await?;

        let text = whisper_response.text.trim().to_string();
        if text.is_empty() {
            tracing::debug!("Empty ASR response");
        } else {
            tracing::info!("ASR result: {}", text);
        }

        Ok(text)
    }
}
