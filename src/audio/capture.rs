use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub is_input: bool,
    pub is_loopback: bool,
}

pub struct AudioCapture {
    _stream: Stream,
    receiver: mpsc::Receiver<Vec<f32>>,
    is_running: Arc<AtomicBool>,
}

impl AudioCapture {
    /// 列举所有可用的输入设备
    pub fn list_devices() -> AppResult<Vec<DeviceInfo>> {
        let host = cpal::default_host();
        let mut devices = Vec::new();
        let mut idx = 0;

        let input_devices = host
            .input_devices()
            .map_err(|e| AppError::CpalDevices(format!("Failed to enumerate devices: {}", e)))?;

        for device in input_devices {
            if let Ok(_config) = device.default_input_config() {
                // 获取设备 ID 作为标识
                let device_id = device.id();
                let name = format!("Device[{}] {:?}", idx, device_id);
                let is_loopback = Self::is_loopback_device(&name);
                
                devices.push(DeviceInfo {
                    name,
                    is_input: true,
                    is_loopback,
                });
                idx += 1;
            }
        }

        if devices.is_empty() {
            tracing::warn!("No input devices found");
        } else {
            tracing::info!("Found {} input devices", devices.len());
            for dev in &devices {
                let type_str = if dev.is_loopback {
                    "🔁 Loopback"
                } else {
                    "🎤 Microphone"
                };
                tracing::info!("  {} ({})", dev.name, type_str);
            }
        }

        Ok(devices)
    }

    /// 检查设备是否是回环设备 (Windows WASAPI Loopback pattern)
    fn is_loopback_device(name: &str) -> bool {
        let name_lower = name.to_lowercase();
        // Windows Stereo Mix / WASAPI loopback patterns
        name_lower.contains("stereo")
            || name_lower.contains("loopback")
            || name_lower.contains("what u hear")
            || name_lower.contains("mix")
            || name_lower.contains("virtual")
    }

    /// 根据名称或索引选择设备
    fn select_device(device_spec: &str) -> AppResult<cpal::Device> {
        let host = cpal::default_host();

        // 特殊情况: "default"
        if device_spec == "default" {
            return host
                .default_input_device()
                .ok_or(AppError::Audio("No default input device found".to_string()));
        }

        // 尝试按索引选择
        if let Ok(index) = device_spec.parse::<usize>() {
            let mut idx = 0;
            for device in host.input_devices()? {
                if let Ok(_config) = device.default_input_config() {
                    if idx == index {
                        return Ok(device);
                    }
                    idx += 1;
                }
            }
            return Err(AppError::Audio(format!(
                "Device index out of range: {}",
                index
            )));
        }

        // 特殊情况: "microphone" - 选择第一个非 loopback 设备
        if device_spec == "microphone" {
            for device in host.input_devices()? {
                if let Ok(_config) = device.default_input_config() {
                    let device_id_str = format!("{:?}", device.id());
                    let is_loop = Self::is_loopback_device(&device_id_str);
                    if !is_loop {
                        return Ok(device);
                    }
                }
            }
            return Err(AppError::Audio("No microphone device found".to_string()));
        }

        // 特殊情况: "loopback" - 选择第一个 loopback 设备
        if device_spec == "loopback" {
            for device in host.input_devices()? {
                if let Ok(_config) = device.default_input_config() {
                    let device_id_str = format!("{:?}", device.id());
                    let is_loop = Self::is_loopback_device(&device_id_str);
                    if is_loop {
                        return Ok(device);
                    }
                }
            }
            return Err(AppError::Audio("No loopback device found".to_string()));
        }

        // 默认返回第一个输入设备
        tracing::warn!(
            "Device '{}' not found, using default",
            device_spec
        );
        host.default_input_device()
            .ok_or(AppError::Audio("No default input device found".to_string()))
    }

    /// 创建音频采集流
    pub async fn new(
        device_name: &str,
        sample_rate: u32,
        channels: u16,
    ) -> AppResult<Self> {
        // 选择设备
        let device = Self::select_device(device_name)?;
        let device_id_str = format!("{:?}", device.id());

        tracing::info!("Opening audio device: {}", device_id_str);

        // 尝试使用指定的配置，失败则使用设备默认配置
        let config = match device.default_input_config() {
            Ok(supported) => {
                let desired_config = StreamConfig {
                    channels: channels.min(supported.channels()),
                    sample_rate: sample_rate.into(),
                    buffer_size: cpal::BufferSize::Default,
                };
                tracing::info!(
                    "Desired config: channels={}, sample_rate={}",
                    channels, sample_rate
                );
                desired_config
            }
            Err(e) => {
                return Err(AppError::CpalBuildStream(format!(
                    "Failed to get default config: {}",
                    e
                )))
            }
        };

        tracing::info!(
            "Audio config: channels={}",
            config.channels
        );

        // 创建通道用于传输音频数据
        let (tx, rx) = mpsc::channel(128);

        // 创建流回调
        let err_fn = |err| {
            tracing::error!("Stream error: {}", err);
        };

        // 根据采样格式选择合适的回调
        let stream = match device.default_input_config() {
            Ok(supported_config) => {
                let sample_format = supported_config.sample_format();

                match sample_format {
                    cpal::SampleFormat::F32 => {
                        let data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            let samples = data.to_vec();
                            let _ = tx.try_send(samples);
                        };
                        device
                            .build_input_stream(config.clone(), data_fn, err_fn, None)
                            .map_err(|e| {
                                AppError::CpalBuildStream(format!("Failed to build F32 stream: {}", e))
                            })?
                    }
                    cpal::SampleFormat::I16 => {
                        let data_fn = move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            let samples: Vec<f32> =
                                data.iter().map(|&s| s as f32 / 32768.0).collect();
                            let _ = tx.try_send(samples);
                        };
                        device
                            .build_input_stream(config.clone(), data_fn, err_fn, None)
                            .map_err(|e| {
                                AppError::CpalBuildStream(format!("Failed to build I16 stream: {}", e))
                            })?
                    }
                    cpal::SampleFormat::U16 => {
                        let data_fn = move |data: &[u16], _: &cpal::InputCallbackInfo| {
                            let samples: Vec<f32> = data
                                .iter()
                                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                .collect();
                            let _ = tx.try_send(samples);
                        };
                        device
                            .build_input_stream(config.clone(), data_fn, err_fn, None)
                            .map_err(|e| {
                                AppError::CpalBuildStream(format!("Failed to build U16 stream: {}", e))
                            })?
                    }
                    _ => {
                        return Err(AppError::Audio(
                            "Unsupported sample format".to_string(),
                        ));
                    }
                }
            }
            Err(e) => {
                return Err(AppError::CpalBuildStream(format!(
                    "Failed to get default config: {}",
                    e
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
