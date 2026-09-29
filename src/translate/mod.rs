use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::config::TranslateConfig;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
        Self {
            client: Client::new(),
            config,
        }
    }

    /// 组装请求体
    ///
    /// 优先级（后者覆盖前者）：
    /// 1. 基础字段（model / messages / temperature）
    /// 2. 关闭思考的预设字段
    /// 3. 用户自定义的 `extra_body`
    pub fn build_body(&self, text: &str) -> Value {
        let mut body = Map::new();

        body.insert("model".into(), json!(self.config.model));
        body.insert(
            "messages".into(),
            json!([
                { "role": "system", "content": &self.config.system_prompt },
                {
                    "role": "user",
                    "content": format!(
                        "Translate to {}: {}",
                        self.config.target_language, text
                    ),
                },
            ]),
        );

        if self.config.send_temperature {
            body.insert("temperature".into(), json!(self.config.temperature));
        }

        // 关闭推理模型的“思考”：各家字段名不同，按配置选一种
        match self
            .config
            .disable_thinking
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "reasoning_effort" => {
                body.insert("reasoning_effort".into(), json!("minimal"));
            }
            "enable_thinking" => {
                body.insert("enable_thinking".into(), json!(false));
            }
            "chat_template" => {
                body.insert(
                    "chat_template_kwargs".into(),
                    json!({ "enable_thinking": false }),
                );
            }
            "" | "none" => {}
            other => {
                tracing::warn!(
                    "未知的 translate.disable_thinking 取值: {other}（可选: reasoning_effort / enable_thinking / chat_template）"
                );
            }
        }

        // 用户自定义字段，优先级最高
        if let Some(extra) = &self.config.extra_body {
            match serde_json::to_value(extra) {
                Ok(Value::Object(map)) => {
                    for (key, value) in map {
                        body.insert(key, value);
                    }
                }
                Ok(_) => tracing::warn!("translate.extra_body 必须是表（table）形式，已忽略"),
                Err(e) => tracing::warn!("translate.extra_body 解析失败，已忽略: {e}"),
            }
        }

        Value::Object(body)
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

        let body = self.build_body(text);
        tracing::debug!("Sending translation request to: {} body={}", url, body);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(AppError::Translation(format!(
                "Translation request failed with status {}: {}",
                status, error_text
            )));
        }

        let chat_response: ChatResponse = response.json().await?;

        let raw = chat_response
            .choices
            .first()
            .ok_or_else(|| {
                AppError::Translation("No response choices from translation API".to_string())
            })?
            .message
            .content
            .clone();

        // 即使没能通过请求参数关掉思考，也把思考内容从结果里剥掉
        let translated = strip_reasoning(&raw);
        if translated.is_empty() {
            return Err(AppError::Translation(format!(
                "翻译结果为空（去思考后）。原始响应: {}",
                raw.trim()
            )));
        }

        tracing::info!("Translated: {} -> {}", text, translated);

        Ok(translated)
    }
}

/// 去掉模型输出里的思考块
///
/// 兼容两种常见形态：
/// - 成对标签：` thinking...` + 闭标签 + `答案`
/// - 只剩闭标签（服务端已剥离开始标签）：`思考内容` + 闭标签 + `答案`
///
/// 闭标签用 Unicode 转义书写，避免源文件里出现全角字符导致被工具链丢失。
pub fn strip_reasoning(text: &str) -> String {
    // "<｜end▁of▁thinking｜>"，其中 ｜ = U+FF5C，▁ = U+2581
    const CLOSE: &str = "<\u{FF5C}end\u{2581}of\u{2581}thinking\u{FF5C}>";
    const OPEN: &str = " thinking";

    let mut output = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(end) = rest.find(CLOSE) {
        match rest[..end].rfind(OPEN) {
            // 有成对标签：丢掉标签之间的内容，保留标签之前的部分
            Some(start) => output.push_str(&rest[..start]),
            // 没有开始标签：闭标签之前的内容整段按思考处理
            None => {}
        }
        rest = &rest[end + CLOSE.len()..];
    }

    output.push_str(rest);

    // 兜底：遇到未闭合的 ` thinking` 时，丢掉它之后的内容
    if let Some(start) = output.find(OPEN) {
        output.truncate(start);
    }

    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(thinking: &str) -> TranslateConfig {
        TranslateConfig {
            base_url: "http://localhost/v1".to_string(),
            api_key: "k".to_string(),
            model: "m".to_string(),
            target_language: "zh-CN".to_string(),
            system_prompt: "sys".to_string(),
            timeout_secs: 5,
            send_temperature: true,
            temperature: 0.2,
            disable_thinking: thinking.to_string(),
            extra_body: None,
        }
    }

    const CLOSE: &str = "<\u{FF5C}end\u{2581}of\u{2581}thinking\u{FF5C}>";

    /// 拼一个“思考块 + 答案”的样本
    fn with_think(inner: &str, answer: &str) -> String {
        format!(" thinking{inner}{CLOSE}{answer}")
    }

    #[test]
    fn body_contains_basics_and_temperature() {
        let client = OpenAiClient::new(config(""));
        let body = client.build_body("hello");
        assert_eq!(body["model"], "m");

        let temperature = body["temperature"].as_f64().expect("应包含 temperature");
        assert!((temperature - 0.2).abs() < 1e-6, "温度不对: {temperature}");

        assert!(body.get("reasoning_effort").is_none());
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("sys"));
    }

    #[test]
    fn temperature_can_be_omitted() {
        let mut cfg = config("");
        cfg.send_temperature = false;
        let body = OpenAiClient::new(cfg).build_body("hi");
        assert!(body.get("temperature").is_none(), "关闭后不应发送 temperature");
    }

    #[test]
    fn thinking_presets_emit_expected_fields() {
        let body = OpenAiClient::new(config("reasoning_effort")).build_body("x");
        assert_eq!(body["reasoning_effort"], "minimal");

        let body = OpenAiClient::new(config("enable_thinking")).build_body("x");
        assert_eq!(body["enable_thinking"], false);

        let body = OpenAiClient::new(config("chat_template")).build_body("x");
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn extra_body_overrides_presets() {
        let mut cfg = config("reasoning_effort");
        cfg.extra_body = Some(
            toml::from_str::<toml::Value>("reasoning_effort = \"none\"").unwrap(),
        );

        let body = OpenAiClient::new(cfg).build_body("x");
        assert_eq!(body["reasoning_effort"], "none", "extra_body 应覆盖预设");
    }

    #[test]
    fn strips_paired_think_block() {
        assert_eq!(strip_reasoning(&with_think("让我想想", "你好")), "你好");
        assert_eq!(
            strip_reasoning(&with_think("推理过程", "答案在这里")),
            "答案在这里"
        );
    }

    #[test]
    fn strips_orphan_closing_tag() {
        let text = format!("推理过程{CLOSE}答案");
        assert_eq!(strip_reasoning(&text), "答案");
    }

    #[test]
    fn drops_unterminated_think_block() {
        // 只有开始标签没有结束标签时，之后的内容按思考处理
        assert_eq!(strip_reasoning(" thinking还在思考中"), "");
    }

    #[test]
    fn keeps_plain_text_intact() {
        assert_eq!(strip_reasoning("你好，世界"), "你好，世界");
        assert_eq!(strip_reasoning("  前后空白  "), "前后空白");
    }
}
