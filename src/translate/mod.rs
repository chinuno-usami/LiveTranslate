use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use crate::config::TranslateConfig;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub message: ChatMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
}

#[derive(Clone)]
pub struct OpenAiClient {
    client: Client,
    pub config: TranslateConfig,
}

impl OpenAiClient {
    pub fn new(config: TranslateConfig) -> Self {
        let client = Client::new();
        Self { client, config }
    }

    /// 翻译文本
    pub async fn translate(&self, text: &str) -> AppResult<String> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: self.config.system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: format!("Translate to {}: {}", self.config.target_language, text),
                },
            ],
            temperature: Some(0.2),
        };

        tracing::debug!("Sending translation request to: {}", url);

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.api_key),
            )
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(AppError::Translation(format!(
                "Translation request failed with status {}: {}",
                status, error_text
            )));
        }

        let chat_response: ChatResponse = response.json().await?;

        let translated = chat_response
            .choices
            .first()
            .ok_or_else(|| {
                AppError::Translation("No response choices from translation API".to_string())
            })?
            .message
            .content
            .trim()
            .to_string();

        tracing::info!("Translated: {} -> {}", text, translated);

        Ok(translated)
    }
}
