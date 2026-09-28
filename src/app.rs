use crate::audio::{wav, AudioCapture, AudioChunker};
use crate::asr::WhisperClient;
use crate::config::AppConfig;
use crate::error::AppResult;
use crate::subtitle::Subtitle;
use crate::translate::OpenAiClient;
use tokio::sync::{mpsc, watch};

pub async fn list_devices() -> AppResult<()> {
    let devices = AudioCapture::list_devices()?;
    if devices.is_empty() {
        println!("No audio devices found");
    } else {
        println!("Available audio devices:");
        for (i, device) in devices.iter().enumerate() {
            let type_str = if device.is_loopback {
                "[Loopback]"
            } else {
                "[Microphone]"
            };
            println!("  {}. {} {}", i, device.name, type_str);
        }
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
    while let Some(subtitle) = rx.recv().await {
        tracing::info!("Subtitle: {} => {}", subtitle.source, subtitle.translated);
    }

    Ok(())
}

pub async fn run_pipeline(
    config: AppConfig,
    tx: mpsc::Sender<Subtitle>,
    mut stop_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    tracing::info!("Starting audio pipeline");

    let mut audio_capture = AudioCapture::new(
        &config.audio.device_name,
        config.audio.sample_rate,
        config.audio.channels,
    )
    .await?;

    let mut audio_chunker = AudioChunker::new(
        config.audio.sample_rate,
        config.audio.chunk_seconds,
        0.25,
        config.audio.silence_threshold,
    )?;

    let whisper_client = WhisperClient::new(config.asr.clone());
    let translate_client = OpenAiClient::new(config.translate.clone());

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
                        audio_chunker.push_samples(samples);

                        while let Some(chunk) = audio_chunker.next_chunk() {
                            if audio_chunker.is_silence(&chunk) {
                                tracing::debug!("Silence detected, skipping");
                                continue;
                            }

                            let wav_data = wav::encode_wav(
                                &chunk,
                                config.audio.sample_rate,
                                config.audio.channels,
                            )?;

                            match whisper_client.transcribe(wav_data).await {
                                Ok(text) if !text.is_empty() => {
                                    tracing::info!("Recognized text: {}", text);
                                    match translate_client.translate(&text).await {
                                        Ok(translated) => {
                                            if tx.send(Subtitle::new(text, translated)).await.is_err() {
                                                tracing::warn!("Subtitle receiver dropped, stopping pipeline");
                                                return Ok(());
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!("Translation failed: {}", e);
                                        }
                                    }
                                }
                                Ok(_) => {
                                    tracing::debug!("Empty ASR result");
                                }
                                Err(e) => {
                                    tracing::error!("ASR failed: {}", e);
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
