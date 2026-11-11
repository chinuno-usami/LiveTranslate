use crate::asr::{languages, AsrEngine};
use crate::audio::chunker::AudioChunker;
use crate::audio::detector::VadEngine;
use crate::audio::resample::LinearResampler;
use crate::audio::segmenter::SpeechSegmenter;
use crate::audio::AudioCapture;
use crate::config::AppConfig;
use crate::error::{AppError, AppResult};
use crate::output::OutputManager;
use crate::subtitle::{SubtitleLine, SubtitleProcessor, SubtitleState};
use crate::translate::OpenAiClient;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};

/// 单个输入源的运行时上下文
#[derive(Clone)]
pub struct SourceContext {
    pub id: String,
    pub label: String,
    pub device: String,
    /// 该源的识别语言（全局语言切换时会遍历所有源 send）
    pub language: watch::Receiver<String>,
    /// 该源的翻译开关（独立热切换）
    pub translate: watch::Receiver<bool>,
}

/// 流水线输出事件
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// 一条可显示的字幕（带来源）
    Subtitle(SubtitleLine),
    /// 供 UI 展示的提示/错误信息
    Notice {
        source_id: Option<String>,
        message: String,
    },
    /// 单源运行状态变化
    SourceState {
        source_id: String,
        running: bool,
        /// 采集流是否真的在收到音频；回环设备没有播放时会一直为 false
        audio: bool,
        message: String,
    },
}

/// 音频分段策略
enum Segmenter {
    Vad(Box<SpeechSegmenter>),
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

    fn flush(&mut self) {
        match self {
            Segmenter::Vad(s) => s.flush(),
            // 固定分片：不足一整片的尾巴不做半片识别，避免噪声误触发
            Segmenter::Fixed(_) => {}
        }
    }

    fn is_silence(&self, chunk: &[f32]) -> bool {
        match self {
            Segmenter::Vad(_) => false,
            Segmenter::Fixed(c) => c.is_silence(chunk),
        }
    }
}

/// 打印服务端可用模型，用于确认是否存在非思考版本
pub async fn list_models(config: &AppConfig) -> AppResult<()> {
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
            Ok(response) if response.status().is_success() => match response.text().await {
                Ok(text) => println!("  {}", text.trim()),
                Err(e) => println!("  读取失败: {e}"),
            },
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
            println!(
                "  [{}] {} ({})",
                device.spec(),
                device.name,
                device.kind_label()
            );
        }
        println!();
        println!("提示: 在 config/default.toml 中设置 device_name 或 [[audio.sources]] 选择设备");
    }
    Ok(())
}

/// 控制台模式：同时跑所有启用中的源
pub async fn start_console(config: AppConfig) -> AppResult<()> {
    let (tx, mut rx) = mpsc::channel(128);
    let (stop_tx, stop_rx) = watch::channel(false);

    let configured_language = if languages::is_edge_backend(&config.asr.backend) {
        config.asr.edge.language.clone()
    } else {
        config.asr.language.clone()
    };
    let global_language =
        languages::normalize_for_backend(&config.asr.backend, &configured_language);

    let mut sources: Vec<_> = config
        .audio
        .effective_sources(config.translate.enabled)
        .into_iter()
        .filter(|s| s.enabled)
        .collect();
    // 按设备去重（与 GUI 的 HashSet 语义一致；dedup_by 只能去掉相邻重复）
    let mut seen_devices = std::collections::HashSet::new();
    sources.retain(|s| s.device.trim().is_empty() || seen_devices.insert(s.device.clone()));
    sources.truncate(config.audio.max_active_sources.max(1));

    let outputs = OutputManager::new(&config.output);
    outputs.set_capture_active(true);
    let history_limit = config
        .subtitle
        .max_lines
        .max(config.output.steamvr.max_lines)
        .max(config.output.osc.max_lines)
        .max(1);
    let mut subtitle_state = SubtitleState::new(
        history_limit,
        config.subtitle.show_source,
        config.subtitle.label_sources,
    );
    let mut active_sources: Vec<(String, String, bool)> = sources
        .iter()
        .map(|s| (s.id.clone(), s.label.clone(), true))
        .collect();
    subtitle_state.set_active_labels(
        active_sources
            .iter()
            .filter(|(_, _, running)| *running)
            .map(|(_, label, _)| label.clone())
            .collect(),
    );

    for source in sources {
        let language = if source.language.trim().is_empty() {
            global_language.clone()
        } else {
            languages::normalize_for_backend(&config.asr.backend, &source.language)
        };
        let (language_tx, language_rx) = watch::channel(language);
        let (translate_tx, translate_rx) =
            watch::channel(source.translate && config.translate.enabled);
        let context = SourceContext {
            id: source.id.clone(),
            label: source.label.clone(),
            device: source.device.clone(),
            language: language_rx,
            translate: translate_rx,
        };
        let source_config = config.clone();
        let source_tx = tx.clone();
        let source_stop = stop_rx.clone();
        tokio::spawn(async move {
            // 保持发送端存活，避免 receiver.changed() 立即报错
            let _language_tx = language_tx;
            let _translate_tx = translate_tx;
            if let Err(e) = run_source(source_config, context, source_tx, source_stop).await {
                tracing::error!("Audio processing error: {}", e);
            }
        });
    }
    drop(tx);

    tracing::info!("Console pipeline running. Press Ctrl+C to stop.");
    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => handle_console_event(&mut subtitle_state, &mut active_sources, &outputs, event),
                    None => break,
                }
            }
            res = tokio::signal::ctrl_c() => {
                if let Err(e) = res {
                    tracing::warn!("Failed to listen for Ctrl+C: {e}");
                } else {
                    tracing::info!("Received Ctrl+C, stopping console pipeline...");
                }
                let _ = stop_tx.send(true);
                let drain = async {
                    while let Some(event) = rx.recv().await {
                        handle_console_event(&mut subtitle_state, &mut active_sources, &outputs, event);
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), drain).await;
                break;
            }
        }
    }

    outputs.set_capture_active(false);
    outputs.shutdown_async().await;
    Ok(())
}

fn handle_console_event(
    subtitle_state: &mut SubtitleState,
    active_sources: &mut [(String, String, bool)],
    outputs: &OutputManager,
    event: PipelineEvent,
) {
    match event {
        PipelineEvent::Subtitle(line) if line.translated.is_empty() => {
            subtitle_state.push(line.clone());
            outputs.publish(&subtitle_state.display_lines());
            tracing::info!("[{}] {}", line.label, line.source);
        }
        PipelineEvent::Subtitle(line) => {
            subtitle_state.push(line.clone());
            outputs.publish(&subtitle_state.display_lines());
            tracing::info!("[{}] {} => {}", line.label, line.source, line.translated);
        }
        PipelineEvent::Notice { source_id, message } => {
            tracing::warn!("Notice ({}): {}", source_id.unwrap_or_default(), message);
        }
        PipelineEvent::SourceState {
            source_id,
            running,
            audio,
            message,
        } => {
            if let Some((_, _, slot_running)) = active_sources
                .iter_mut()
                .find(|(id, _, _)| *id == source_id)
            {
                *slot_running = running;
                let labels = active_sources
                    .iter()
                    .filter(|(_, _, active)| *active)
                    .map(|(_, label, _)| label.clone())
                    .collect();
                subtitle_state.set_active_labels(labels);
                outputs.publish(&subtitle_state.display_lines());
            }
            tracing::info!(
                "Source {} {} (audio={}): {}",
                source_id,
                if running { "started" } else { "stopped" },
                audio,
                message
            );
        }
    }
}

/// 去重地发送 Notice，避免同一个错误每几秒刷屏
async fn send_notice(
    tx: &mpsc::Sender<PipelineEvent>,
    last: &mut String,
    source_id: Option<&str>,
    msg: String,
) {
    if *last == msg {
        return;
    }
    *last = msg.clone();
    let _ = tx
        .send(PipelineEvent::Notice {
            source_id: source_id.map(str::to_string),
            message: msg,
        })
        .await;
}

/// 处理一个音频片段：ASR -> 翻译（可关闭）-> 去重 -> 发送
#[allow(clippy::too_many_arguments)]
async fn process_segment(
    chunk: &[f32],
    sample_rate: u32,
    source_id: &str,
    label: &str,
    engine: &AsrEngine,
    translator: &OpenAiClient,
    translate_enabled: bool,
    processor: &mut SubtitleProcessor,
    tx: &mpsc::Sender<PipelineEvent>,
    last_notice: &mut String,
    filter_hallucination: bool,
) -> AppResult<bool> {
    match engine.transcribe(chunk, sample_rate).await {
        Ok(text) if !text.trim().is_empty() => {
            if filter_hallucination && !crate::asr::cleaner::is_meaningful_speech(&text) {
                tracing::debug!("Dropped non-speech ASR output: {:?}", text);
                return Ok(true);
            }

            tracing::debug!("[{}] Recognized text: {}", label, text);

            // 先按原文去重/去重叠，再翻译：固定分片有 overlap 时若先翻译会得到重复译文碎片。
            // 注意：这里只比对原文（传空译文），译文重复不再吞字幕——
            // 不同原文译出同一句（如都是「是的」）是正常现象，不应被当成重复丢掉。
            if processor.is_duplicate(&text, "") {
                tracing::debug!("Duplicate subtitle detected, skipping");
                return Ok(true);
            }
            let (clean_text, _) = processor.deduplicate_overlap(&text, "");
            if clean_text.trim().is_empty() {
                tracing::debug!("Subtitle empty after overlap removal, skipping");
                return Ok(true);
            }

            let translated = if translate_enabled {
                match translator.translate(&clean_text).await {
                    Ok(translated) => translated,
                    Err(e) => {
                        tracing::error!("Translation failed: {}", e);
                        send_notice(tx, last_notice, Some(source_id), format!("翻译失败: {e}"))
                            .await;
                        // 翻译失败仍保留原文上屏，避免整句识别结果被吞掉
                        String::new()
                    }
                }
            } else {
                String::new()
            };

            processor.record(&clean_text, &translated);

            if tx
                .send(PipelineEvent::Subtitle(SubtitleLine::new(
                    source_id, label, clean_text, translated,
                )))
                .await
                .is_err()
            {
                tracing::warn!("Subtitle receiver dropped, stopping pipeline");
                return Ok(false);
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
                Some(source_id),
                format!("ASR 失败（{} @ {}）: {e}", engine.name(), engine.endpoint()),
            )
            .await;
        }
    }

    Ok(true)
}

/// 单个输入源的完整流水线
///
/// 外层包一层：不管内部是正常结束、提前 `?` 返回还是错误，
/// 都会补发一条 `running: false`，否则 UI 会永远停在“采集中”。
pub async fn run_source(
    base: AppConfig,
    source: SourceContext,
    tx: mpsc::Sender<PipelineEvent>,
    stop_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    let source_id = source.id.clone();
    let result = run_source_inner(base, source, tx.clone(), stop_rx).await;
    let message = match &result {
        Ok(()) => "已停止".to_string(),
        Err(e) => format!("已停止: {e}"),
    };
    let _ = tx
        .send(PipelineEvent::SourceState {
            source_id,
            running: false,
            audio: false,
            message,
        })
        .await;
    result
}

async fn run_source_inner(
    base: AppConfig,
    source: SourceContext,
    tx: mpsc::Sender<PipelineEvent>,
    mut stop_rx: watch::Receiver<bool>,
) -> AppResult<()> {
    tracing::info!(
        "Starting audio pipeline for source {} ({}) device={}",
        source.id,
        source.label,
        source.device
    );

    let _ = tx
        .send(PipelineEvent::SourceState {
            source_id: source.id.clone(),
            running: true,
            audio: false,
            message: "采集中".to_string(),
        })
        .await;

    let mut last_notice = String::new();

    let api_key = base.translate.api_key.trim();
    let translate_enabled = *source.translate.borrow();
    if translate_enabled && (api_key.is_empty() || api_key == "YOUR_API_KEY") {
        send_notice(
            &tx,
            &mut last_notice,
            Some(&source.id),
            "翻译 API Key 未配置：请在设置页填写 translate.api_key".to_string(),
        )
        .await;
    }

    let mut audio_capture = match AudioCapture::new(
        &source.device,
        base.audio.sample_rate,
        base.audio.channels,
    )
    .await
    {
        Ok(capture) => capture,
        Err(e) => {
            // 结束状态由 `run_source` 统一补发（带真实原因）
            return Err(e);
        }
    };

    let native_rate = audio_capture.native_sample_rate;
    let target_rate = base.audio.sample_rate;
    let mut resampler = LinearResampler::new(native_rate, target_rate);
    tracing::info!(
        "[{}] Resampling: {} Hz -> {} Hz (needed: {})",
        source.label,
        native_rate,
        target_rate,
        resampler.is_needed()
    );

    let overlap_removal = !base.vad.enabled;
    let mut segmenter = if base.vad.enabled {
        let (engine, warning) = VadEngine::from_config(&base.vad, target_rate);
        tracing::info!(
            "[{}] VAD enabled: backend={} frame_len={} min_speech={}ms min_silence={}ms max={}ms",
            source.label,
            engine.backend(),
            engine.frame_len(),
            base.vad.min_speech_ms,
            base.vad.min_silence_ms,
            base.vad.max_speech_ms
        );
        if let Some(message) = warning {
            send_notice(&tx, &mut last_notice, Some(&source.id), message).await;
        }
        Segmenter::Vad(Box::new(SpeechSegmenter::new(
            &base.vad,
            target_rate,
            engine,
        )))
    } else {
        tracing::info!(
            "[{}] VAD disabled: fixed {}s chunks",
            source.label,
            base.audio.chunk_seconds
        );
        Segmenter::Fixed(Box::new(AudioChunker::new(
            target_rate,
            base.audio.chunk_seconds,
            0.25,
            base.audio.silence_threshold,
        )?))
    };

    let asr_engine = AsrEngine::from_config(&base.asr, source.language.clone())?;
    tracing::info!(
        "[{}] ASR backend: {} ({}) language={}",
        source.label,
        asr_engine.name(),
        asr_engine.endpoint(),
        asr_engine.language()
    );

    let (utterance_tx, mut utterance_rx) = mpsc::channel::<Vec<f32>>(4);
    let translate_client = OpenAiClient::new(base.translate.clone());
    let worker_tx = tx.clone();
    let filter_hallucination = base.asr.filter_hallucination;
    let worker_source_id = source.id.clone();
    let worker_label = source.label.clone();
    let translate_rx = source.translate.clone();

    let worker = tokio::spawn(async move {
        let mut subtitle_processor = SubtitleProcessor::new().with_overlap_removal(overlap_removal);
        let mut last_notice = String::new();

        while let Some(chunk) = utterance_rx.recv().await {
            let translate_enabled = *translate_rx.borrow();
            let keep_going = match process_segment(
                &chunk,
                target_rate,
                &worker_source_id,
                &worker_label,
                &asr_engine,
                &translate_client,
                translate_enabled,
                &mut subtitle_processor,
                &worker_tx,
                &mut last_notice,
                filter_hallucination,
            )
            .await
            {
                Ok(keep) => keep,
                Err(e) => {
                    tracing::error!("[{worker_label}] 片段处理失败: {e}");
                    let _ = worker_tx
                        .send(PipelineEvent::Notice {
                            source_id: Some(worker_source_id.clone()),
                            message: format!("处理失败: {e}"),
                        })
                        .await;
                    true
                }
            };

            if !keep_going {
                break;
            }
        }
    });
    // 若外层任务被 supervisor 超时强行 abort，连带取消 worker，防止飞行中的识别/翻译请求泄漏
    struct AbortWorkerOnDrop(Option<tokio::task::JoinHandle<()>>);
    impl Drop for AbortWorkerOnDrop {
        fn drop(&mut self) {
            if let Some(handle) = self.0.take() {
                handle.abort();
            }
        }
    }
    let mut worker_guard = AbortWorkerOnDrop(Some(worker));

    let mut pushed_chunks: u64 = 0;
    let mut dropped_segments: u64 = 0;
    // 非停止信号导致的流中断要带出去：只有当它变成 Err 时，
    // SourceState 与采集状态才会显示「已停止: 音频输入已中断（设备断开？）」而不是裸的「已停止」
    let mut stream_ended: Option<AppError> = None;
    // 采样是否在真的流动：WASAPI 回环只在对应端点有播放时才推数据，
    // 所以「连上了但没声音」必须是可观测状态，否则面板上只有绿灯 + 无字幕，无法排查
    let mut has_audio = false;
    // 本轮 tick 窗口内是否收到过音频块
    let mut audio_in_window = false;
    // 「已连接但没数据」是否已经上报过，避免每 2s 刷一条同样的状态
    let mut reported_silent = false;
    let mut audio_tick = {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // interval 的第一次 tick 立即就绪，先消费掉，避免刚启动就报「暂无音频」
        tick.tick().await;
        tick
    };

    'capture: loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_err() || *stop_rx.borrow() {
                    tracing::info!("[{}] Received stop signal", source.label);
                    break;
                }
            }
            _ = audio_tick.tick() => {
                if audio_in_window {
                    audio_in_window = false;
                } else if !reported_silent {
                    // 启动后一直没有数据，或上一个窗口还有数据、这个窗口一个块都没有：端点停播了
                    reported_silent = true;
                    let was_audible = has_audio;
                    has_audio = false;
                    tracing::warn!(
                        "[{}] 已连接但未收到音频数据（was_audible={}）",
                        source.label,
                        was_audible
                    );
                    let _ = tx
                        .send(PipelineEvent::SourceState {
                            source_id: source.id.clone(),
                            running: true,
                            audio: false,
                            message: "已连接，但没有音频数据（回环设备需有播放才会推送声音）"
                                .to_string(),
                        })
                        .await;
                }
            }
            samples = audio_capture.next_chunk() => {
                let Some(samples) = samples else {
                    // 通道只会在 cpal 回调报错时提前关闭；若是我们自己置位的停止信号
                    // 与回调竞争到了这一轮，按正常停止处理，不误报成设备断开。
                    if *stop_rx.borrow() {
                        tracing::info!("[{}] Audio stream closed during stop", source.label);
                        break;
                    }
                    tracing::warn!("[{}] Audio stream ended unexpectedly", source.label);
                    // 回调里存下的致命错误原因（设备被拔出 / 被独占 / 后端错误）
                    let detail = audio_capture.take_error();
                    let (notice, err) = match detail {
                        Some(detail) => (
                            format!("音频输入已中断（{detail}），该源已停止"),
                            AppError::AudioStreamFailed(detail),
                        ),
                        None => (
                            "音频输入已中断（设备断开？），该源已停止".to_string(),
                            AppError::AudioStreamEnded,
                        ),
                    };
                    send_notice(&tx, &mut last_notice, Some(&source.id), notice).await;
                    stream_ended = Some(err);
                    break 'capture;
                };

                let mut resampled = Vec::new();
                resampler.process(&samples, &mut resampled);
                segmenter.push(&resampled);

                if !has_audio {
                    has_audio = true;
                    reported_silent = false;
                    let _ = tx
                        .send(PipelineEvent::SourceState {
                            source_id: source.id.clone(),
                            running: true,
                            audio: true,
                            message: "采集中".to_string(),
                        })
                        .await;
                }
                audio_in_window = true;

                pushed_chunks += 1;
                if pushed_chunks.is_multiple_of(500) {
                    if let Segmenter::Vad(vad) = &mut segmenter {
                        if let Some(floor) = vad.noise_floor_db() {
                            tracing::debug!(
                                "[{}] VAD 噪声底 ≈ {:.1} dBFS (speech 阈值 ≈ {:.1} dBFS)",
                                source.label,
                                floor,
                                floor + base.vad.margin_db
                            );
                        }
                        if let Some(stats) = vad.stats() {
                            tracing::debug!(
                                "[{}] Silero 最近概率: max={:.3} avg={:.3} 阈值(进/出)={:.2}/{:.2} active={}",
                                source.label,
                                stats.max_prob,
                                stats.avg_prob,
                                stats.threshold,
                                stats.neg_threshold,
                                stats.active
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
                    tracing::debug!("[{}] VAD segment ready: {} ms", source.label, ms);

                    match utterance_tx.try_send(chunk) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            dropped_segments += 1;
                            tracing::warn!(
                                "[{}] 识别速度跟不上，丢弃一个片段（累计 {} 个）",
                                source.label,
                                dropped_segments
                            );
                        }
                        Err(TrySendError::Closed(_)) => break 'capture,
                    }
                }
            }
        }
    }

    // 先显式析构 AudioCapture 以关闭底层 cpal::Stream 硬件句柄，再排空已在队列中的识别/翻译请求
    audio_capture.stop();
    drop(audio_capture);

    // 停止时若仍有未达 min_silence_ms 的尾部语音，flush 出来交给识别 worker
    segmenter.flush();
    while let Some(chunk) = segmenter.next_chunk() {
        if segmenter.is_silence(&chunk) {
            continue;
        }
        match utterance_tx.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                dropped_segments += 1;
                tracing::warn!(
                    "[{}] 停止排空时识别队列已满，丢弃一个尾部片段（累计 {} 个）",
                    source.label,
                    dropped_segments
                );
            }
            Err(TrySendError::Closed(_)) => break,
        }
    }

    drop(utterance_tx);
    if let Some(mut worker) = worker_guard.0.take() {
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut worker)
            .await
            .is_err()
        {
            tracing::info!("[{}] Worker did not finish in time, aborting", source.label);
            worker.abort();
        }
    }

    tracing::info!("[{}] Audio pipeline stopped", source.label);
    match stream_ended {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
