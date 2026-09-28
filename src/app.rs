use crate::audio::chunker::AudioChunker;
use crate::audio::resample::LinearResampler;
use crate::audio::segmenter::SpeechSegmenter;
use crate::audio::{wav, AudioCapture};
use crate::asr::WhisperClient;
use crate::config::AppConfig;
use crate::error::AppResult;
use crate::subtitle::{Subtitle, SubtitleProcessor};
use crate::translate::OpenAiClient;
use tokio::sync::{mpsc, watch};

/// 流水线输出事件
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// 一条可显示的字幕
    Subtitle(Subtitle),
    /// 供 UI 展示的提示/错误信息（用于避免“界面一片空白但不说原因”）
    Notice(String),
}

/// 音频分段策略
enum Segmenter {
    /// 基于 VAD 按语音边界切句（推荐）
    Vad(Box<SpeechSegmenter>),
    /// 固定时长切片（回退方案）
    Fixed(Box<AudioChunker>),
}

impl Segmenter {
    fn push(&mut self, samples: &[f32]) {
        match self {
            Segmenter::Vad(s) => s.push(samples),
            Segmenter::Fixed(c) => c.push_samples(samples),
        }
    }

    fn next_chunk(&mut self) -> Option<Vec<f32>> {
        match self {
            Segmenter::Vad(s) => s.pop(),
            Segmenter::Fixed(c) => c.next_chunk(),
        }
    }

    /// 固定切片模式下需要跳过纯静音
    fn is_silence(&self, chunk: &[f32]) -> bool {
        match self {
            Segmenter::Vad(_) => false, // VAD 已保证片段内有语音
            Segmenter::Fixed(c) => c.is_silence(chunk),
        }
    }
}

pub async fn list_devices() -> AppResult<()> {
    let devices = AudioCapture::list_devices()?;
    if devices.is_empty() {
        println!("No audio devices found");
    } else {
        println!("Available audio devices:");
        for device in devices.iter() {
            println!("  [{}] {} ({})", device.spec(), device.name, device.kind_label());
        }
        println!();
        println!("提示: 在 config/default.toml 中设置 device_name 为 [方括号] 内的值即可选择设备");
    }
    Ok(())
}

pub async fn start_console(config: AppConfig) -> AppResult<()> {
    let (tx, mut rx) = mpsc::channel(128);
    let (_stop_tx, stop_rx) = watch::channel(false);

    tokio::spawn(async move {
        if let Err(e) = run_pipeline(config, tx, stop_rx).await {
            tracing::error!("Audio processing error: {}", e);
        }
    });

    tracing::info!("Console pipeline running. Press Ctrl+C to stop.");
    while let Some(event) = rx.recv().await {
        match event {
            PipelineEvent::Subtitle(s) => {
                tracing::info!("Subtitle: {} => {}", s.source, s.translated)
            }
            PipelineEvent::Notice(n) => tracing::warn!("Notice: {}", n),
        }
    }

    Ok(())
}

/// 去重地发送 Notice，避免同一个错误每几秒刷屏
async fn send_notice(tx: &mpsc::Sender<PipelineEvent>, last: &mut String, msg: String) {
    if *last == msg {
        return;
    }
    *last = msg.clone();
    let _ = tx.send(PipelineEvent::Notice(msg)).await;
}

/// 处理一个音频片段：ASR -> 翻译 -> 去重 -> 发送
///
/// 返回 `Ok(false)` 表示接收端已关闭，流水线应结束。
async fn process_segment(
    chunk: &[f32],
    sample_rate: u32,
    whisper: &WhisperClient,
    translator: &OpenAiClient,
    processor: &mut SubtitleProcessor,
    tx: &mpsc::Sender<PipelineEvent>,
    last_notice: &mut String,
) -> AppResult<bool> {
    let wav_data = wav::encode_wav(chunk, sample_rate, 1)?;

    match whisper.transcribe(wav_data).await {
        Ok(text) if !text.trim().is_empty() => {
            tracing::info!("Recognized text: {}", text);

            match translator.translate(&text).await {
                Ok(translated) => {
                    if processor.is_duplicate(&text, &translated) {
                        tracing::debug!("Duplicate subtitle detected, skipping");
                    } else {
                        let (clean_text, clean_trans) =
                            processor.deduplicate_overlap(&text, &translated);

                        if clean_text.is_empty() {
                            tracing::debug!("Subtitle empty after overlap removal, skipping");
                        } else {
                            processor.record(&text, &translated);

                            if tx
                                .send(PipelineEvent::Subtitle(Subtitle::new(clean_text, clean_trans)))
                                .await
                                .is_err()
                            {
                                tracing::warn!("Subtitle receiver dropped, stopping pipeline");
                                return Ok(false);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Translation failed: {}", e);
                    send_notice(tx, last_notice, format!("翻译失败: {e}")).await;
                }
            }
        }
        Ok(_) => {
            tracing::debug!("Empty ASR result");
        }
        Err(e) => {
            tracing::error!("ASR failed: {}", e);
            send_notice(
                tx,
                last_notice,
                format!(
                    "ASR 失败（请确认 Whisper 服务在 {} 上运行）: {e}",
                    whisper.config.base_url
                ),
            )
            .await;
        }
    }

    Ok(true)
}

pub async fn run_pipeline(
    config: AppConfig,
    tx: mpsc::Sender<PipelineEvent>,
    mut stop_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    tracing::info!("Starting audio pipeline");

    let mut last_notice = String::new();

    // 配置健全性检查：这些是最常见的“看着在跑但其实没结果”的原因
    let api_key = config.translate.api_key.trim();
    if api_key.is_empty() || api_key == "YOUR_API_KEY" {
        send_notice(
            &tx,
            &mut last_notice,
            "翻译 API Key 未配置：请在配置文件里设置 translate.api_key".to_string(),
        )
        .await;
    }

    let mut audio_capture = AudioCapture::new(
        &config.audio.device_name,
        config.audio.sample_rate,
        config.audio.channels,
    )
    .await?;

    let native_rate = audio_capture.native_sample_rate;
    let target_rate = config.audio.sample_rate;
    let mut resampler = LinearResampler::new(native_rate, target_rate);
    tracing::info!(
        "Resampling: {} Hz -> {} Hz (needed: {})",
        native_rate,
        target_rate,
        resampler.is_needed()
    );

    let mut segmenter = if config.vad.enabled {
        tracing::info!(
            "VAD segmentation enabled: frame={}ms margin={}dB min_speech={}ms min_silence={}ms max={}ms",
            config.vad.frame_ms,
            config.vad.margin_db,
            config.vad.min_speech_ms,
            config.vad.min_silence_ms,
            config.vad.max_speech_ms
        );
        Segmenter::Vad(Box::new(SpeechSegmenter::new(&config.vad, target_rate)))
    } else {
        tracing::info!(
            "VAD disabled: fixed {}s chunks",
            config.audio.chunk_seconds
        );
        Segmenter::Fixed(Box::new(AudioChunker::new(
            target_rate,
            config.audio.chunk_seconds,
            0.25,
            config.audio.silence_threshold,
        )?))
    };

    let whisper_client = WhisperClient::new(config.asr.clone());
    let translate_client = OpenAiClient::new(config.translate.clone());
    let mut subtitle_processor = SubtitleProcessor::new();

    // 周期性打印噪声底，便于用户按环境调整 vad.margin_db
    let mut pushed_chunks: u64 = 0;
    let mut noise_floor_tick: u64 = 0;

    loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_ok() && *stop_rx.borrow() {
                    tracing::info!("Received stop signal for audio pipeline");
                    break;
                }
            }
            samples = audio_capture.next_chunk() => {
                let Some(samples) = samples else {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                };

                // 下混已是单声道，这里做重采样到目标采样率
                let mut resampled = Vec::new();
                resampler.process(&samples, &mut resampled);
                segmenter.push(&resampled);

                pushed_chunks += 1;
                if pushed_chunks % 500 == 0 {
                    if let Segmenter::Vad(vad) = &mut segmenter {
                        noise_floor_tick += 1;
                        tracing::debug!(
                            "VAD 噪声底 ≈ {:.1} dBFS (speech 阈值 ≈ {:.1} dBFS)",
                            vad.noise_floor_db(),
                            vad.noise_floor_db() + config.vad.margin_db
                        );
                        let _ = noise_floor_tick;
                    }
                }

                while let Some(chunk) = segmenter.next_chunk() {
                    if segmenter.is_silence(&chunk) {
                        tracing::debug!("Silence detected, skipping");
                        continue;
                    }

                    if !process_segment(
                        &chunk,
                        target_rate,
                        &whisper_client,
                        &translate_client,
                        &mut subtitle_processor,
                        &tx,
                        &mut last_notice,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                }
            }
        }
    }

    audio_capture.stop();
    tracing::info!("Audio pipeline stopped");
    Ok(())
}
