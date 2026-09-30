//! Whisper 兼容 HTTP API 客户端
//!
//! 对接 `POST {base_url}{request_path}` 的 multipart 接口
//! （OpenAI Whisper / whisper.cpp server / faster-whisper-server 等）。

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::audio::wav;
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
        Self {
            client: Client::new(),
            config,
        }
    }

    /// 供日志展示的目标地址
    pub fn endpoint(&self) -> String {
        format!(
            "{}{}",
            self.config.base_url.trim_end_matches('/'),
            self.config.request_path
        )
    }

    /// 识别一段单声道音频
    ///
    /// `language` 为 ISO-639-1 语言码（如 `zh`）或 `auto`；
    /// 面板上切换语言后会立即用新值发起请求。
    pub async fn transcribe(
        &self,
        samples: &[f32],
        sample_rate: u32,
        language: &str,
    ) -> AppResult<String> {
        let url = self.endpoint();
        let audio_data = wav::encode_wav(samples, sample_rate, 1)?;
        let language = if language.trim().is_empty() {
            "auto"
        } else {
            language.trim()
        };

        tracing::debug!(
            "Sending audio to ASR: {} ({} bytes, {} samples)",
            url,
            audio_data.len(),
            samples.len()
        );

        let mut form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(audio_data).file_name("audio.wav"),
            )
            .text("model", self.config.model.clone());
        // "auto" 不是 ISO-639-1 代码，OpenAI 等会返回 400；省略字段即为自动检测
        if !language.eq_ignore_ascii_case("auto") {
            form = form.text("language", language.to_string());
        }

        let mut request = self
            .client
            .post(&url)
            .timeout(Duration::from_secs(self.config.timeout_secs.max(1)))
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
            tracing::debug!("ASR result: {}", text);
        }

        Ok(text)
    }
}
