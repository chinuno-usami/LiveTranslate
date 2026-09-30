//! Edge 在线语音识别后端
//!
//! 走微软 Edge 浏览器内置语音识别所用的 WebSocket 服务：
//! `wss://speech.platform.bing.com/speech/recognition/edge/interactive/v1`
//!
//! 协议要点（微软 Speech SDK 的帧格式）：
//! - **文本帧**：若干 `Key:Value` 头，空行，再跟 JSON 正文
//! - **二进制帧**：开头是 `u16` 大端表示的头长度，然后是头，再跟负载
//! - 流程：连上后先发 `speech.config` 描述音频格式，再发 `speech.context`
//!   开启一轮识别，随后发 `audio` 帧（先 WAV 头，再 PCM，最后空负载表示结束），
//!   读取 `speech.phrase` 取 `DisplayText`，收到 `turn.end` 结束
//!
//! 连接握手需要一个会轮换的客户端身份（token / Chromium 版本 / Origin），
//! 默认值内置在代码中，也可通过 `[asr.edge]` 配置覆盖。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::config::EdgeAsrConfig;
use crate::error::{AppError, AppResult};

const SPEECH_HOST: &str = "speech.platform.bing.com";
const RECOGNITION_PATH: &str = "/speech/recognition/edge/interactive/v1";

const DEFAULT_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const DEFAULT_CHROMIUM_VERSION: &str = "143.0.3650.75";
const DEFAULT_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// Windows 文件时间纪元（1601-01-01）相对 Unix 纪元的秒数
const WINDOWS_EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
/// `Sec-MS-GEC` 签名使用的时间窗口（秒），服务端按 5 分钟向下取整
const SEC_MS_GEC_WINDOW_SECS: u64 = 300;

const BITS_PER_SAMPLE: u32 = 16;
const CHANNELS: u32 = 1;
/// 音频分块发送的粒度（秒）
const AUDIO_CHUNK_SECS: f64 = 0.1;
/// 结尾补的静音时长（秒），帮助服务端判断句子结束
const TRAILING_SILENCE_SECS: f64 = 0.4;

#[derive(Clone)]
struct EdgeIdentity {
    token: String,
    chromium_version: String,
    origin: String,
}

impl EdgeIdentity {
    fn from_config(cfg: &EdgeAsrConfig) -> Self {
        let pick = |value: &Option<String>, fallback: &str| -> String {
            value
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback)
                .to_string()
        };

        Self {
            token: pick(&cfg.trusted_client_token, DEFAULT_TRUSTED_CLIENT_TOKEN)
                .to_ascii_uppercase(),
            chromium_version: pick(&cfg.chromium_full_version, DEFAULT_CHROMIUM_VERSION),
            origin: pick(&cfg.origin, DEFAULT_ORIGIN),
        }
    }

    fn major_version(&self) -> &str {
        self.chromium_version.split('.').next().unwrap_or("1")
    }

    fn sec_ms_gec_version(&self) -> String {
        format!("1-{}", self.chromium_version)
    }

    fn user_agent(&self) -> String {
        let major = self.major_version();
        format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36 Edg/{major}.0.0.0"
        )
    }
}

#[derive(Clone)]
pub struct EdgeAsrClient {
    identity: EdgeIdentity,
    language: String,
    timeout: Duration,
}

impl EdgeAsrClient {
    pub fn new(config: &EdgeAsrConfig) -> Self {
        Self {
            identity: EdgeIdentity::from_config(config),
            language: config.language.trim().to_string(),
            timeout: Duration::from_secs(config.timeout_secs.max(1)),
        }
    }

    /// 供日志展示的目标描述
    pub fn describe(&self) -> String {
        format!("Edge ASR ({})", self.language)
    }

    /// 识别一段单声道音频
    ///
    /// `language` 为 BCP-47 标签（如 `en-US`）；面板切换后立即生效。
    /// 传入空值时回退到配置中的语言。
    pub async fn transcribe(
        &self,
        samples: &[f32],
        sample_rate: u32,
        language: &str,
    ) -> AppResult<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }

        let language = if language.trim().is_empty() {
            self.language.clone()
        } else {
            language.trim().to_string()
        };

        let started = Instant::now();
        match tokio::time::timeout(
            self.timeout,
            self.transcribe_inner(samples, sample_rate, &language),
        )
        .await
        {
            Ok(result) => {
                tracing::debug!("Edge ASR turn took {:?}", started.elapsed());
                result
            }
            Err(_) => Err(AppError::Asr(format!(
                "Edge ASR 超时（{} 秒）",
                self.timeout.as_secs()
            ))),
        }
    }

    async fn transcribe_inner(
        &self,
        samples: &[f32],
        sample_rate: u32,
        language: &str,
    ) -> AppResult<String> {
        let url = self.build_url(language);
        tracing::debug!("Connecting Edge ASR: {}", url);

        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| AppError::Asr(format!("Edge ASR 请求构造失败: {e}")))?;

        {
            let headers = request.headers_mut();
            let mut insert = |name: &'static str, value: String| -> AppResult<()> {
                let parsed = HeaderValue::from_str(&value)
                    .map_err(|e| AppError::Asr(format!("Edge ASR 头部 {name} 非法: {e}")))?;
                headers.insert(name, parsed);
                Ok(())
            };

            insert("Origin", self.identity.origin.clone())?;
            insert("User-Agent", self.identity.user_agent())?;
            insert("Accept-Language", "en-US,en;q=0.9".to_string())?;
            insert("Pragma", "no-cache".to_string())?;
            insert("Cache-Control", "no-cache".to_string())?;
        }

        let (socket, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| AppError::Asr(format!("Edge ASR 连接失败: {e}")))?;

        let (mut sink, mut stream) = socket.split();

        // 1) 描述音频格式
        send_text(
            &mut sink,
            "speech.config",
            &speech_config_body(sample_rate),
            None,
            Some("application/json"),
        )
        .await?;

        let request_id = uuid::Uuid::new_v4().simple().to_string();
        let stream_id = "1";

        // 2) 开启一轮识别
        send_text(
            &mut sink,
            "speech.context",
            &speech_context_body(stream_id),
            Some(&request_id),
            Some("application/json"),
        )
        .await?;

        // 3) 发送与接收并发进行：服务端可能在上传途中就结束本轮或关闭连接，
        //    若先发完再读，发送失败会丢掉已识别出的结果
        let send_audio = async {
            // 3) 音频：先发一个零长度 WAV 头，再发 PCM16
            let pcm = f32_to_pcm16(samples);
            sink.send(binary_message(
                "audio",
                &request_id,
                &wav_header(sample_rate),
                Some(stream_id),
                Some("audio/x-wav"),
            ))
            .await
            .map_err(|e| AppError::Asr(format!("Edge ASR 发送失败: {e}")))?;

            let chunk_bytes = ((sample_rate as f64 * AUDIO_CHUNK_SECS) as usize).max(1) * 2;
            for block in pcm.chunks(chunk_bytes) {
                sink.send(binary_message(
                    "audio",
                    &request_id,
                    block,
                    Some(stream_id),
                    None,
                ))
                .await
                .map_err(|e| AppError::Asr(format!("Edge ASR 发送失败: {e}")))?;
            }

            // 结尾静音，随后空负载表示流结束（让服务端尽快收敛结果）
            let silence_len = (sample_rate as f64 * TRAILING_SILENCE_SECS) as usize * 2;
            if silence_len > 0 {
                sink.send(binary_message(
                    "audio",
                    &request_id,
                    &vec![0u8; silence_len],
                    Some(stream_id),
                    None,
                ))
                .await
                .map_err(|e| AppError::Asr(format!("Edge ASR 发送失败: {e}")))?;
            }

            sink.send(binary_message(
                "audio",
                &request_id,
                &[],
                Some(stream_id),
                None,
            ))
            .await
            .map_err(|e| AppError::Asr(format!("Edge ASR 发送失败: {e}")))?;
            Ok::<(), AppError>(())
        };

        let read_result = async {
            // 4) 读取直到 turn.end
            let mut text = String::new();
            loop {
                let Some(message) = stream.next().await else {
                    break;
                };

                let message =
                    message.map_err(|e| AppError::Asr(format!("Edge ASR 读取失败: {e}")))?;

                let Some((path, body)) = parse_frame(&message) else {
                    continue;
                };

                match path.as_str() {
                    "speech.phrase" => {
                        let payload: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                        let status = payload
                            .get("RecognitionStatus")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if status == "Success" {
                            // 一个 turn 可能产出多个 phrase（长片段中间有停顿），需要拼接
                            let phrase = payload
                                .get("DisplayText")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .trim();
                            if !phrase.is_empty() {
                                if !text.is_empty() {
                                    text.push(' ');
                                }
                                text.push_str(phrase);
                            }
                        } else {
                            // NoMatch / InitialSilenceTimeout 等都属于正常结果，不清空已识别内容
                            tracing::debug!("Edge ASR status: {}", status);
                        }
                    }
                    "turn.end" => break,
                    _ => {}
                }
            }
            Ok::<String, AppError>(text)
        };

        let (send_res, read_res) = tokio::join!(send_audio, read_result);
        let text = match (send_res, read_res) {
            (_, Ok(text)) if !text.is_empty() => text,
            (Err(e), _) => return Err(e),
            (Ok(()), res) => res?,
        };

        let _ = sink.close().await;

        if text.is_empty() {
            tracing::debug!("Empty Edge ASR result");
        } else {
            tracing::debug!("ASR result (edge): {}", text);
        }

        Ok(text)
    }

    fn build_url(&self, language: &str) -> String {
        let gec = generate_sec_ms_gec(&self.identity.token);
        format!(
            "wss://{SPEECH_HOST}{RECOGNITION_PATH}\
             ?TrustedClientToken={token}&Sec-MS-GEC={gec}\
             &Sec-MS-GEC-Version={version}&language={language}&profanity=raw",
            token = self.identity.token,
            version = self.identity.sec_ms_gec_version(),
        )
    }
}

// ---------------------------------------------------------------- 协议构造

/// 用可信客户端令牌对当前 5 分钟窗口签名
fn generate_sec_ms_gec(token: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut ticks = now + WINDOWS_EPOCH_OFFSET_SECS;
    ticks -= ticks % SEC_MS_GEC_WINDOW_SECS;

    // Windows 文件时间以 100 纳秒为单位
    let ticks_100ns = (ticks as u128) * 10_000_000;

    let mut hasher = Sha256::new();
    hasher.update(format!("{ticks_100ns}{token}").as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>()
}

async fn send_text<S>(
    sink: &mut S,
    path: &str,
    body: &str,
    request_id: Option<&str>,
    content_type: Option<&str>,
) -> AppResult<()>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let mut headers = vec![format!("X-Timestamp:{}", timestamp()), format!("Path:{path}")];
    if let Some(id) = request_id {
        headers.push(format!("X-RequestId:{id}"));
    }
    if let Some(ct) = content_type {
        headers.push(format!("Content-Type:{ct}"));
    }

    let frame = format!("{}\r\n\r\n{}", headers.join("\r\n"), body);
    sink.send(Message::Text(frame))
        .await
        .map_err(|e| AppError::Asr(format!("Edge ASR 发送失败: {e}")))
}

fn binary_message(
    path: &str,
    request_id: &str,
    payload: &[u8],
    stream_id: Option<&str>,
    content_type: Option<&str>,
) -> Message {
    let mut headers = vec![
        format!("X-Timestamp:{}", timestamp()),
        format!("Path:{path}"),
        format!("X-RequestId:{request_id}"),
    ];
    if let Some(ct) = content_type {
        headers.push(format!("Content-Type:{ct}"));
    }
    if let Some(sid) = stream_id {
        headers.push(format!("X-StreamId:{sid}"));
    }

    let head = headers.join("\r\n");
    let mut out = Vec::with_capacity(2 + head.len() + payload.len());
    out.extend_from_slice(&(head.len() as u16).to_be_bytes());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(payload);

    Message::Binary(out)
}

fn speech_config_body(sample_rate: u32) -> String {
    serde_json::json!({
        "context": {
            "audio": {
                "source": {
                    "bitspersample": BITS_PER_SAMPLE.to_string(),
                    "channelcount": CHANNELS.to_string(),
                    "model": "",
                    "samplerate": sample_rate.to_string(),
                    "type": "Stream"
                }
            },
            "os": { "name": "Client", "platform": "Windows", "version": "10" },
            "system": { "build": "Windows-x64", "name": "SpeechSDK", "version": "1.15.0" }
        }
    })
    .to_string()
}

fn speech_context_body(stream_id: &str) -> String {
    serde_json::json!({
        "audio": { "streams": { stream_id: serde_json::Value::Null } }
    })
    .to_string()
}

/// 零长度的 RIFF 头（真正的长度由流式音频提供）
fn wav_header(sample_rate: u32) -> Vec<u8> {
    let byte_rate = sample_rate * CHANNELS * BITS_PER_SAMPLE / 8;
    let block_align = (CHANNELS * BITS_PER_SAMPLE / 8) as u16;

    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&(BITS_PER_SAMPLE as u16).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

fn f32_to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 把一帧拆成 `(Path 头, 正文)`
fn parse_frame(message: &Message) -> Option<(String, String)> {
    match message {
        Message::Text(text) => {
            let (head, body) = match text.split_once("\r\n\r\n") {
                Some((head, body)) => (head, body),
                None => (text.as_str(), ""),
            };
            Some((extract_path(head), body.to_string()))
        }
        Message::Binary(data) => {
            if data.len() < 2 {
                return None;
            }
            let head_len = u16::from_be_bytes([data[0], data[1]]) as usize;
            if data.len() < 2 + head_len {
                return None;
            }
            let head = String::from_utf8_lossy(&data[2..2 + head_len]);
            let body = String::from_utf8_lossy(&data[2 + head_len..]);
            Some((extract_path(&head), body.to_string()))
        }
        _ => None,
    }
}

fn extract_path(head: &str) -> String {
    for line in head.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Path:") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// RFC3339 UTC 时间戳（`YYYY-MM-DDTHH:MM:SSZ`）
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 自 Unix 纪元起的天数 -> (年, 月, 日)
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gec_is_uppercase_sha256_hex() {
        let gec = generate_sec_ms_gec(DEFAULT_TRUSTED_CLIENT_TOKEN);
        assert_eq!(gec.len(), 64, "SHA256 十六进制应为 64 字符");
        assert!(gec.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    }

    #[test]
    fn gec_is_stable_within_the_same_window() {
        let a = generate_sec_ms_gec(DEFAULT_TRUSTED_CLIENT_TOKEN);
        let b = generate_sec_ms_gec(DEFAULT_TRUSTED_CLIENT_TOKEN);
        assert_eq!(a, b, "同一时间窗口内签名应一致");
    }

    #[test]
    fn wav_header_shape() {
        let header = wav_header(16_000);
        assert_eq!(header.len(), 44);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[36..40], b"data");
        assert_eq!(u32::from_le_bytes([header[24], header[25], header[26], header[27]]), 16_000);
    }

    #[test]
    fn pcm16_roundtrip_bounds() {
        let pcm = f32_to_pcm16(&[0.0, 1.0, -1.0, 2.0, -2.0]);
        assert_eq!(pcm.len(), 10);
        let first = i16::from_le_bytes([pcm[0], pcm[1]]);
        assert_eq!(first, 0);
        // 超范围应被夹紧，不出现回绕
        let clamped_hi = i16::from_le_bytes([pcm[2], pcm[3]]);
        let clamped_lo = i16::from_le_bytes([pcm[4], pcm[5]]);
        assert_eq!(clamped_hi, i16::MAX);
        assert_eq!(clamped_lo, -i16::MAX);
    }

    #[test]
    fn parse_text_frame_extracts_path() {
        let frame = "X-Timestamp:2024-01-01T00:00:00Z\r\nPath:speech.phrase\r\n\r\n{\"a\":1}";
        let (path, body) = parse_frame(&Message::Text(frame.to_string())).unwrap();
        assert_eq!(path, "speech.phrase");
        assert_eq!(body, "{\"a\":1}");
    }

    #[test]
    fn parse_binary_frame_uses_big_endian_length() {
        let head = "Path:turn.end";
        let mut data = (head.len() as u16).to_be_bytes().to_vec();
        data.extend_from_slice(head.as_bytes());
        data.extend_from_slice(b"{}");

        let (path, body) = parse_frame(&Message::Binary(data)).unwrap();
        assert_eq!(path, "turn.end");
        assert_eq!(body, "{}");
    }

    #[test]
    fn binary_message_prefixes_header_length() {
        let message = binary_message("audio", "abc", &[1, 2, 3], Some("1"), None);
        let Message::Binary(data) = message else {
            panic!("应为二进制帧");
        };
        let head_len = u16::from_be_bytes([data[0], data[1]]) as usize;
        let head = std::str::from_utf8(&data[2..2 + head_len]).unwrap();
        assert!(head.contains("Path:audio"));
        assert!(head.contains("X-RequestId:abc"));
        assert!(head.contains("X-StreamId:1"));
        assert_eq!(&data[2 + head_len..], &[1, 2, 3]);
    }

    #[test]
    fn timestamps_and_dates_are_sane() {
        // 1970-01-01 为第 0 天
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2024-01-01 为第 19723 天
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // 闰日
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));

        let ts = timestamp();
        assert!(ts.ends_with('Z'), "时间戳应以 Z 结尾: {ts}");
        assert_eq!(ts.len(), 20, "RFC3339 长度应为 20: {ts}");
    }
}
