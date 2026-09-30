use crate::audio::resample;
use crate::error::{AppError, AppResult};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Microphone,
    Loopback,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// 该设备在其分类内的索引
    pub index: usize,
    /// 设备名称
    pub name: String,
    /// 设备类型
    pub kind: DeviceKind,
}

impl DeviceInfo {
    /// 用于配置与命令行选择的稳定标识符
    pub fn spec(&self) -> String {
        match self.kind {
            DeviceKind::Microphone => format!("mic:{}", self.index),
            DeviceKind::Loopback => format!("loopback:{}", self.index),
        }
    }
}

pub struct AudioCapture {
    _stream: cpal::Stream,
    receiver: mpsc::Receiver<Vec<f32>>,
    is_running: Arc<AtomicBool>,
    /// 设备原生采样率（回调已下混为单声道）
    pub native_sample_rate: u32,
}

impl AudioCapture {
    /// 列举所有可用的采集设备
    ///
    /// - 输入设备 => 麦克风
    /// - 输出设备 => 回环（Windows WASAPI 下作为输入设备打开即为 loopback）
    pub fn list_devices() -> AppResult<Vec<DeviceInfo>> {
        let host = cpal::default_host();
        let mut devices = Vec::new();

        // 输入设备（麦克风）
        let input_devices = host.input_devices().map_err(|e| {
            AppError::CpalDevices(format!("Failed to enumerate input devices: {}", e))
        })?;

        let mut mic_index = 0;
        for device in input_devices {
            if let Ok(desc) = device.description() {
                devices.push(DeviceInfo {
                    index: mic_index,
                    name: desc.name().to_string(),
                    kind: DeviceKind::Microphone,
                });
            } else if let Ok(cfg) = device.default_input_config() {
                devices.push(DeviceInfo {
                    index: mic_index,
                    name: format!("Input Device {} ({:?})", mic_index, cfg.sample_format()),
                    kind: DeviceKind::Microphone,
                });
            } else {
                continue;
            }
            mic_index += 1;
        }

        // 输出设备 => 回环源
        //
        // 仅 Windows WASAPI 支持“把输出设备当输入打开”的透明 loopback。
        //
        // 注意：不能拿 `default_input_config()` 当过滤条件！
        // cpal 的 WASAPI 后端里，该方法只对采集设备（eCapture）返回 Ok，
        // 对渲染设备（eRender）一律返回 UnsupportedOperation，
        // 而回环用的恰恰是渲染设备——之前因此把回环选项全部滤掉了。
        if cfg!(target_os = "windows") {
            if let Ok(output_devices) = host.output_devices() {
                for (loop_index, device) in output_devices.enumerate() {
                    let name = device
                        .description()
                        .map(|d| d.name().to_string())
                        .unwrap_or_else(|_| format!("Output Device {}", loop_index));

                    // 仅作诊断记录，不作为过滤条件
                    if let Err(e) = device.default_output_config() {
                        tracing::debug!(
                            "Loopback 候选 `{}` 输出格式探测失败（仍列出）: {}",
                            name,
                            e
                        );
                    }

                    devices.push(DeviceInfo {
                        index: loop_index,
                        name,
                        kind: DeviceKind::Loopback,
                    });
                }
            }
        }

        if devices.is_empty() {
            tracing::warn!("No audio devices found");
        } else {
            tracing::info!("Found {} audio devices", devices.len());
            for dev in &devices {
                tracing::info!("  [{}] {} ({})", dev.spec(), dev.name, dev.kind_label());
            }
        }

        Ok(devices)
    }

    /// 根据 spec 选择设备
    ///
    /// 支持:
    /// - `default`          : 默认输入设备
    /// - `microphone` / `mic`: 第一个麦克风
    /// - `loopback`         : 第一个回环设备（Windows 输出设备）
    /// - `mic:<n>`          : 第 n 个麦克风
    /// - `loopback:<n>`     : 第 n 个回环设备
    /// - `<n>`              : 合并列表中的第 n 个设备
    /// - 其它字符串         : 按设备名模糊匹配
    pub fn resolve_device(spec: &str) -> AppResult<(cpal::Device, DeviceKind)> {
        let host = cpal::default_host();
        let spec = spec.trim();
        let spec_lower = spec.to_lowercase();

        if spec.is_empty() || spec_lower == "default" {
            return host
                .default_input_device()
                .map(|device| (device, DeviceKind::Microphone))
                .ok_or(AppError::Audio("No default input device found".to_string()));
        }

        // mic:<n> / microphone / mic
        if spec_lower == "microphone" || spec_lower == "mic" || spec_lower.starts_with("mic:") {
            let want_index = parse_suffix_index(spec).unwrap_or(0);
            return Self::nth_input_device(&host, want_index);
        }

        // loopback / loopback:<n>
        if spec_lower == "loopback" || spec_lower.starts_with("loopback:") {
            if !cfg!(target_os = "windows") {
                return Err(AppError::Audio(
                    "Loopback capture is only supported on Windows".to_string(),
                ));
            }
            let want_index = parse_suffix_index(spec).unwrap_or(0);
            return Self::nth_output_device(&host, want_index);
        }

        // 纯数字 => 合并列表索引
        if let Ok(index) = spec.parse::<usize>() {
            let devices = Self::list_devices()?;
            let target = devices
                .get(index)
                .ok_or_else(|| AppError::Audio(format!("Device index out of range: {}", index)))?;
            return match target.kind {
                DeviceKind::Microphone => Self::nth_input_device(&host, target.index),
                DeviceKind::Loopback => Self::nth_output_device(&host, target.index),
            };
        }

        // 按名称模糊匹配（先输入设备，再输出设备）
        if let Ok(iter) = host.input_devices() {
            for device in iter {
                if let Ok(desc) = device.description() {
                    if desc.name().to_lowercase().contains(&spec_lower) {
                        return Ok((device, DeviceKind::Microphone));
                    }
                }
            }
        }
        if cfg!(target_os = "windows") {
            if let Ok(iter) = host.output_devices() {
                for device in iter {
                    if let Ok(desc) = device.description() {
                        if desc.name().to_lowercase().contains(&spec_lower) {
                            return Ok((device, DeviceKind::Loopback));
                        }
                    }
                }
            }
        }

        Err(AppError::DeviceNotFound(spec.to_string()))
    }

    fn nth_input_device(
        host: &cpal::Host,
        want_index: usize,
    ) -> AppResult<(cpal::Device, DeviceKind)> {
        let mut idx = 0;
        for device in host.input_devices().map_err(|e| {
            AppError::CpalDevices(format!("Failed to enumerate input devices: {}", e))
        })? {
            // 必须与 list_devices 的计数规则一致，否则 mic:N 会打开错误的设备
            if device.description().is_ok() || device.default_input_config().is_ok() {
                if idx == want_index {
                    return Ok((device, DeviceKind::Microphone));
                }
                idx += 1;
            }
        }
        Err(AppError::Audio(format!(
            "Microphone index out of range: {}",
            want_index
        )))
    }

    /// 取第 N 个输出设备作为回环源
    ///
    /// 这里不能像输入设备那样用 `default_input_config()` 筛选：
    /// WASAPI 下渲染设备的该调用恒定失败，会把所有回环设备排除掉。
    fn nth_output_device(
        host: &cpal::Host,
        want_index: usize,
    ) -> AppResult<(cpal::Device, DeviceKind)> {
        for (idx, device) in host
            .output_devices()
            .map_err(|e| {
                AppError::CpalDevices(format!("Failed to enumerate output devices: {}", e))
            })?
            .enumerate()
        {
            if idx == want_index {
                return Ok((device, DeviceKind::Loopback));
            }
        }
        Err(AppError::Audio(format!(
            "Loopback device index out of range: {}",
            want_index
        )))
    }

    /// 创建音频采集流
    ///
    /// 使用设备原生配置打开，回调中下混为单声道。
    /// 后续在消费端按需重采样到 Whisper 期望的采样率。
    pub async fn new(device_name: &str, _sample_rate: u32, _channels: u16) -> AppResult<Self> {
        let (device, kind) = Self::resolve_device(device_name)?;

        let device_label = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "Unknown Device".to_string());

        tracing::info!(
            "Opening audio device: '{}' ({})",
            device_label,
            match kind {
                DeviceKind::Microphone => "microphone",
                DeviceKind::Loopback => "loopback",
            }
        );

        // 回环设备的配置必须取自「输出」格式：
        // WASAPI 下渲染设备的 default_input_config() 恒定返回 UnsupportedOperation。
        // 取到输出混音格式后交给 build_input_stream，
        // cpal 会自动为渲染设备加上 AUDCLNT_STREAMFLAGS_LOOPBACK。
        let supported = match kind {
            DeviceKind::Microphone => device.default_input_config(),
            DeviceKind::Loopback => device.default_output_config(),
        }
        .map_err(|e| {
            AppError::CpalBuildStream(format!(
                "Failed to get default config for {device_label}: {e}"
            ))
        })?;

        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let native_channels = config.channels;
        let native_sample_rate = config.sample_rate;

        tracing::info!(
            "Audio native config: channels={}, sample_rate={}, format={:?}",
            native_channels,
            native_sample_rate,
            sample_format
        );

        let (tx, rx) = mpsc::channel(128);
        // 消费端跟不上时的丢块计数（实时回调里不能阻塞，只能丢弃并记录）
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let err_fn = |err| {
            tracing::error!("Audio stream error: {}", err);
        };

        // 根据采样格式选择回调，统一转换为单声道 f32
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono = resample::downmix_to_mono(data, native_channels);
                    send_or_count(&tx, mono, &dropped);
                };
                device
                    .build_input_stream(config, data_fn, err_fn, None)
                    .map_err(|e| {
                        AppError::CpalBuildStream(format!("Failed to build F32 stream: {}", e))
                    })?
            }
            cpal::SampleFormat::I16 => {
                let data_fn = move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let converted: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    let mono = resample::downmix_to_mono(&converted, native_channels);
                    send_or_count(&tx, mono, &dropped);
                };
                device
                    .build_input_stream(config, data_fn, err_fn, None)
                    .map_err(|e| {
                        AppError::CpalBuildStream(format!("Failed to build I16 stream: {}", e))
                    })?
            }
            cpal::SampleFormat::U16 => {
                let data_fn = move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect();
                    let mono = resample::downmix_to_mono(&converted, native_channels);
                    send_or_count(&tx, mono, &dropped);
                };
                device
                    .build_input_stream(config, data_fn, err_fn, None)
                    .map_err(|e| {
                        AppError::CpalBuildStream(format!("Failed to build U16 stream: {}", e))
                    })?
            }
            other => {
                return Err(AppError::Audio(format!(
                    "Unsupported sample format: {:?}",
                    other
                )));
            }
        };

        stream
            .play()
            .map_err(|e| AppError::CpalPlayStream(format!("Failed to play stream: {}", e)))?;

        tracing::info!("Audio stream started successfully");

        Ok(Self {
            _stream: stream,
            receiver: rx,
            is_running: Arc::new(AtomicBool::new(true)),
            native_sample_rate,
        })
    }

    /// 获取下一个音频块
    pub async fn next_chunk(&mut self) -> Option<Vec<f32>> {
        self.receiver.recv().await
    }

    /// 停止采集
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    /// 是否正在运行
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

impl DeviceInfo {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            DeviceKind::Microphone => "🎤 Microphone",
            DeviceKind::Loopback => "🔁 Loopback",
        }
    }
}

/// 解析 `prefix:<n>` 形式的索引；仅 `prefix` 时返回 None
fn parse_suffix_index(spec: &str) -> Option<usize> {
    spec.split_once(':').and_then(|(_, idx)| idx.parse().ok())
}

/// 在音频回调中投递数据；通道满时计数并低频告警
fn send_or_count(
    tx: &mpsc::Sender<Vec<f32>>,
    data: Vec<f32>,
    dropped: &std::sync::atomic::AtomicU64,
) {
    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(data) {
        let n = dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n == 1 || n % 100 == 0 {
            tracing::warn!(
                "Audio consumer is lagging, dropped {} audio blocks so far",
                n
            );
        }
    }
}
