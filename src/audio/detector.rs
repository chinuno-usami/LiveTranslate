//! VAD 后端抽象
//!
//! - `energy`：内置能量型（自适应噪声底 + 迟滞），无额外依赖
//! - `silero`：Silero VAD 神经网络，能区分音乐/噪声与人声
//!
//! 无论哪种后端，对外都表现为「按固定帧长输入，返回该帧是否语音」，
//! 具体帧长由后端决定（能量型跟随 `frame_ms`，Silero 固定 512/256 样本）。

use crate::audio::vad::EnergyVad;
use crate::config::VadConfig;

#[cfg(feature = "silero-vad")]
use crate::audio::silero::SileroVad;

enum VadImpl {
    Energy(EnergyVad),
    #[cfg(feature = "silero-vad")]
    Silero(SileroVad),
}

pub struct VadEngine {
    inner: VadImpl,
    frame_len: usize,
    backend: &'static str,
}

impl VadEngine {
    /// 依据配置构造 VAD
    ///
    /// 第二个返回值是「需要告知用户的提示」：例如请求了 Silero
    /// 但初始化失败（缺 ONNX Runtime、采样率不支持等）而回退到能量型。
    pub fn from_config(config: &VadConfig, sample_rate: u32) -> (Self, Option<String>) {
        let requested = config.backend.trim().to_ascii_lowercase();

        if requested == "silero" {
            #[cfg(feature = "silero-vad")]
            {
                match SileroVad::new(config, sample_rate) {
                    Ok(vad) => {
                        let frame_len = vad.frame_len();
                        return (
                            Self {
                                inner: VadImpl::Silero(vad),
                                frame_len,
                                backend: "silero",
                            },
                            None,
                        );
                    }
                    Err(e) => {
                        return (
                            Self::energy(config, sample_rate),
                            Some(format!("Silero VAD 初始化失败，已回退到能量 VAD：{e}")),
                        );
                    }
                }
            }

            #[cfg(not(feature = "silero-vad"))]
            {
                return (
                    Self::energy(config, sample_rate),
                    Some(
                        "当前构建未启用 silero-vad feature，已回退到能量 VAD\
                         （需用 --features silero-vad 重新构建）"
                            .to_string(),
                    ),
                );
            }
        }

        (Self::energy(config, sample_rate), None)
    }

    fn energy(config: &VadConfig, sample_rate: u32) -> Self {
        let frame_len =
            ((sample_rate as f32 * config.frame_ms.max(1) as f32 / 1000.0).round() as usize).max(1);

        Self {
            inner: VadImpl::Energy(EnergyVad::new(config.margin_db, config.noise_percentile)),
            frame_len,
            backend: "energy",
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// 期望的帧长（样本数）
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// 输入一帧，返回是否为语音
    pub fn is_speech(&mut self, frame: &[f32]) -> bool {
        match &mut self.inner {
            VadImpl::Energy(vad) => vad.is_speech(frame),
            #[cfg(feature = "silero-vad")]
            VadImpl::Silero(vad) => vad.is_speech(frame),
        }
    }

    /// 当前噪声底（仅能量型后端提供）
    pub fn noise_floor_db(&mut self) -> Option<f32> {
        match &mut self.inner {
            VadImpl::Energy(vad) => Some(vad.current_noise_floor()),
            #[cfg(feature = "silero-vad")]
            VadImpl::Silero(_) => None,
        }
    }
}
