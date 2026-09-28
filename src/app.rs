use crate::audio::resample::LinearResampler;
use crate::audio::{wav, AudioCapture, AudioChunker};
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
    /// 供 UI 展示的提示/错误信息（用于避免"界面一片空白但不说原因"）
    Notice(String),
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

/// 去重地发送 Notice，避免同一个错误每 2 秒刷屏
async fn send_notice(tx: &mpsc::Sender<PipelineEvent>, last: &mut String, msg: String) {
    if *last == msg {
        return;
    }
    *last = msg.clone();
    let _ = tx.send(PipelineEvent::Notice(msg)).await;
}

pub async fn run_pipeline(
    config: AppConfig,
    tx: mpsc::Sender<PipelineEvent>,
    mut stop_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    tracing::info!("Starting audio pipeline");

    let mut last_notice = String::new();

    // 配置健全性检查：这些是最常见的"看着在跑但其实没结果"的原因
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

    let mut audio_chunker = AudioChunker::new(
        target_rate,
        config.audio.chunk_seconds,
        0.25,
        config.audio.silence_threshold,
    )?;

    let whisper_client = WhisperClient::new(config.asr.clone());
    let translate_client = OpenAiClient::new(config.translate.clone());
    let mut subtitle_processor = SubtitleProcessor::new();

    loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_ok() && *stop_rx.borrow() {
                    tracing::info!("Received stop signal for audio pipeline");
                    break;
                }
            }
            samples = audio_capture.next_chunk() => {
                match samples {
                    Some(samples) => {
                        // 下混已是单声道，这里做重采样到目标采样率
                        let mut resampled = Vec::new();
                        resampler.process(&samples, &mut resampled);
                        audio_chunker.push_samples(resampled);

                        while let Some(chunk) = audio_chunker.next_chunk() {
                            if audio_chunker.is_silence(&chunk) {
                                tracing::debug!("Silence detected, skipping");
                                continue;
                            }

                            let wav_data = wav::encode_wav(&chunk, target_rate, 1)?;

                            match whisper_client.transcribe(wav_data).await {
                                Ok(text) if !text.is_empty() => {
                                    tracing::info!("Recognized text: {}", text);

                                    match translate_client.translate(&text).await {
                                        Ok(translated) => {
                                            // 检查是否重复
                                            if subtitle_processor.is_duplicate(&text, &translated) {
                                                tracing::debug!("Duplicate subtitle detected, skipping");
                                            } else {
                                                // 做前缀/后缀去重
                                                let (clean_text, clean_trans) =
                                                    subtitle_processor.deduplicate_overlap(&text, &translated);

                                                if clean_text.is_empty() {
                                                    tracing::debug!("Subtitle is empty after overlap removal, skipping");
                                                } else {
                                                    subtitle_processor.record(&text, &translated);

                                                    if tx
                                                        .send(PipelineEvent::Subtitle(Subtitle::new(
                                                            clean_text,
                                                            clean_trans,
                                                        )))
                                                        .await
                                                        .is_err()
                                                    {
                                                        tracing::warn!("Subtitle receiver dropped, stopping pipeline");
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!("Translation failed: {}", e);
                                            send_notice(
                                                &tx,
                                                &mut last_notice,
                                                format!("翻译失败: {e}"),
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Ok(_) => {
                                    tracing::debug!("Empty ASR result");
                                }
                                Err(e) => {
                                    tracing::error!("ASR failed: {}", e);
                                    send_notice(
                                        &tx,
                                        &mut last_notice,
                                        format!("ASR 失败（请确认 Whisper 服务在 {} 上运行）: {e}",
                                            config.asr.base_url),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    None => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }

    audio_capture.stop();
    tracing::info!("Audio pipeline stopped");
    Ok(())
}
