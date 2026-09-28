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

        let mut request = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .multipart(form);

        // 可选认证：默认 Authorization: Bearer <token>
        if let Some(key) = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            let header_name = self
                .config
                .auth_header
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .unwrap_or("Authorization");

            let header_value = if header_name.eq_ignore_ascii_case("authorization") {
                format!("Bearer {key}")
            } else {
                key.to_string()
            };

            request = request.header(header_name, header_value);
            tracing::debug!("ASR request using auth header: {}", header_name);
        }

        let response = request.send().await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            if status == reqwest::StatusCode::UNAUTHORIZED {
                let hint = if self
                    .config
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .map_or(true, str::is_empty)
                {
                    "未配置 [asr] api_key"
                } else {
                    "[asr] api_key 可能不正确"
                };
                return Err(AppError::Asr(format!(
                    "ASR 认证失败 (401)：{hint}。响应: {error_text}"
                )));
            }

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
