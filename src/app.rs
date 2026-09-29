use crate::asr::{languages, AsrEngine};
use crate::audio::chunker::AudioChunker;
use crate::audio::detector::VadEngine;
use crate::audio::resample::LinearResampler;
use crate::audio::segmenter::SpeechSegmenter;
use crate::audio::AudioCapture;
use crate::config::AppConfig;
use crate::error::AppResult;
use crate::subtitle::{Subtitle, SubtitleProcessor};
use crate::translate::OpenAiClient;
use tokio::sync::mpsc::error::TrySendError;
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

/// 打印服务端可用模型，用于确认是否存在非思考版本
pub async fn list_models(config: &AppConfig) -> AppResult<()> {
    // 翻译服务
    println!("翻译服务: {}", config.translate.base_url);
    let client = OpenAiClient::new(config.translate.clone());
    match client.list_models().await {
        Ok(models) if !models.is_empty() => {
            println!("可用模型 ({}):", models.len());
            for model in &models {
                println!("  {model}");
            }
            println!();
            println!(
                "提示: 若其中有非思考版本（常见命名 *-chat / *-instruct / *-non-thinking），\n      \
                 把 [translate] model 换成它即可直接关闭思考，比调参数可靠"
            );
        }
        Ok(_) => println!("  服务未返回模型列表（部分兼容服务不支持 GET /models）"),
        Err(e) => println!("  获取失败: {e}"),
    }

    // 识别服务（可选，失败不影响退出码）
    if !config.asr.base_url.trim().is_empty() {
        println!();
        println!("识别服务: {}", config.asr.base_url);
        let asr_url = format!("{}/models", config.asr.base_url.trim_end_matches('/'));
        match reqwest::Client::new()
            .get(&asr_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.text().await {
                    Ok(text) => println!("  {}", text.trim()),
                    Err(e) => println!("  读取失败: {e}"),
                }
            }
            Ok(response) => println!("  返回 {}", response.status()),
            Err(e) => println!("  获取失败: {e}"),
        }
    }

    Ok(())
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

    // 控制台模式没有面板，语言直接取自配置（两个后端字段不同）
    let configured_language = if languages::is_edge_backend(&config.asr.backend) {
        config.asr.edge.language.clone()
    } else {
        config.asr.language.clone()
    };
    let initial_language =
        languages::normalize_for_backend(&config.asr.backend, &configured_language);
    let (_language_tx, language_rx) = watch::channel(initial_language);

    tokio::spawn(async move {
        if let Err(e) = run_pipeline(config, tx, stop_rx, language_rx).await {
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
    engine: &AsrEngine,
    translator: &OpenAiClient,
    processor: &mut SubtitleProcessor,
    tx: &mpsc::Sender<PipelineEvent>,
    last_notice: &mut String,
    filter_hallucination: bool,
) -> AppResult<bool> {
    match engine.transcribe(chunk, sample_rate).await {
        Ok(text) if !text.trim().is_empty() => {
            // 音乐/噪声引起的幻听输出直接丢弃，既不显示也不翻译
            if filter_hallucination && !crate::asr::cleaner::is_meaningful_speech(&text) {
                tracing::debug!("Dropped non-speech ASR output: {:?}", text);
                return Ok(true);
            }

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
                format!("ASR 失败（{} @ {}）: {e}", engine.name(), engine.endpoint()),
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
    language_rx: watch::Receiver<String>,
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
        let (engine, warning) = VadEngine::from_config(&config.vad, target_rate);
        tracing::info!(
            "VAD enabled: backend={} frame_len={} min_speech={}ms min_silence={}ms max={}ms",
            engine.backend(),
            engine.frame_len(),
            config.vad.min_speech_ms,
            config.vad.min_silence_ms,
            config.vad.max_speech_ms
        );

        // 例如请求了 silero 但缺 ONNX Runtime，这里会把原因告知用户
        if let Some(message) = warning {
            send_notice(&tx, &mut last_notice, message).await;
        }

        Segmenter::Vad(Box::new(SpeechSegmenter::new(
            &config.vad,
            target_rate,
            engine,
        )))
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

    let asr_engine = AsrEngine::from_config(&config.asr, language_rx)?;
    tracing::info!(
        "ASR backend: {} ({}) language={}",
        asr_engine.name(),
        asr_engine.endpoint(),
        asr_engine.language()
    );

    // 把识别/翻译放到独立任务里：
    // 网络请求可能耗时数秒，若直接在采集循环里 await，采集会被阻塞，
    // 音频通道填满后 `try_send` 会静默丢样本——结果是片段残缺、请求延迟。
    let (utterance_tx, mut utterance_rx) = mpsc::channel::<Vec<f32>>(4);
    let translate_client = OpenAiClient::new(config.translate.clone());
    let worker_tx = tx.clone();
    let filter_hallucination = config.asr.filter_hallucination;

    let worker = tokio::spawn(async move {
        let mut subtitle_processor = SubtitleProcessor::new();
        let mut last_notice = String::new();

        while let Some(chunk) = utterance_rx.recv().await {
            let keep_going = process_segment(
                &chunk,
                target_rate,
                &asr_engine,
                &translate_client,
                &mut subtitle_processor,
                &worker_tx,
                &mut last_notice,
                filter_hallucination,
            )
            .await
            .unwrap_or(true);

            if !keep_going {
                tracing::warn!("Subtitle receiver dropped, stopping worker");
                break;
            }
        }
    });

    // 周期性打印噪声底，便于用户按环境调整 vad.margin_db
    let mut pushed_chunks: u64 = 0;
    let mut dropped_segments: u64 = 0;

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
                        if let Some(floor) = vad.noise_floor_db() {
                            tracing::debug!(
                                "VAD 噪声底 ≈ {:.1} dBFS (speech 阈值 ≈ {:.1} dBFS)",
                                floor,
                                floor + config.vad.margin_db
                            );
                        }
                    }
                }

                while let Some(chunk) = segmenter.next_chunk() {
                    if segmenter.is_silence(&chunk) {
                        tracing::debug!("Silence detected, skipping");
                        continue;
                    }

                    let ms = chunk.len() as u64 * 1000 / target_rate.max(1) as u64;
                    tracing::info!("VAD segment ready: {} ms", ms);

                    match utterance_tx.try_send(chunk) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            dropped_segments += 1;
                            tracing::warn!(
                                "识别速度跟不上，丢弃一个片段（累计 {} 个）",
                                dropped_segments
                            );
                        }
                        Err(TrySendError::Closed(_)) => break,
                    }
                }
            }
        }
    }

    // 采集结束：关闭发送端，等处理任务收尾
    drop(utterance_tx);
    let _ = worker.await;

    audio_capture.stop();
    tracing::info!("Audio pipeline stopped");
    Ok(())
}
