//! 逐帧语音活动检测（纯 Rust，无外部依赖）
//!
//! 策略：**自适应噪声底 + 迟滞（hysteresis）**
//!
//! - 用最近若干帧能量的低分位数估计当前噪声底
//! - 帧能量高于 `噪声底 + margin_db` 判为语音（开启阈值）
//! - 已处于语音态时，需低于 `噪声底 + margin_db * release_ratio` 才回落
//!   （迟滞可避免句子中间的短暂停顿被误判为结束）
//! - 噪声底**只在非语音段更新**，避免长句把自己的噪声底抬高而中途截断
//!
//! 注意：本模块只做"这一帧是不是语音"，句子的起止由 `segmenter` 决定。

use std::collections::VecDeque;

pub struct EnergyVad {
    /// 最近的非语音帧能量（dBFS）
    window: VecDeque<f32>,
    window_size: usize,
    /// 用于估计噪声底的分位数（0.0 - 1.0）
    percentile: f32,
    /// 判定为语音所需的余量（dB）
    margin_db: f32,
    /// 回落阈值相对开启阈值的比例（迟滞）
    release_ratio: f32,
    /// 当前是否处于语音态
    active: bool,
    /// 计算分位数时的临时缓冲
    scratch: Vec<f32>,
}

impl EnergyVad {
    /// 冷启动时使用的噪声底估计（dBFS）
    const COLD_START_DB: f32 = -60.0;
    /// 冷启动阶段强制采集的帧数
    const BOOTSTRAP_FRAMES: usize = 25;

    pub fn new(margin_db: f32, percentile: f32) -> Self {
        let window_size = 150; // 约 3s @20ms
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            percentile: percentile.clamp(0.0, 0.9),
            margin_db,
            release_ratio: 0.6,
            active: false,
            scratch: Vec::with_capacity(window_size),
        }
    }

    /// 计算一帧的 RMS 电平（dBFS，满量程为 0）
    pub fn frame_db(frame: &[f32]) -> f32 {
        if frame.is_empty() {
            return -100.0;
        }
        let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
        let rms = (sum_sq / frame.len() as f32).sqrt();
        20.0 * rms.max(1e-7).log10()
    }

    /// 估计当前噪声底
    fn noise_floor(&mut self) -> f32 {
        if self.window.len() < 10 {
            return Self::COLD_START_DB;
        }
        self.scratch.clear();
        self.scratch.extend(self.window.iter().copied());
        let idx = ((self.scratch.len() as f32 - 1.0) * self.percentile) as usize;
        let (_, &mut value, _) =
            self.scratch
                .select_nth_unstable_by(idx, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        value
    }

    /// 输入一帧，返回该帧是否属于语音
    pub fn is_speech(&mut self, frame: &[f32]) -> bool {
        let db = Self::frame_db(frame);

        // 仅在非语音态更新噪声底（冷启动阶段强制采集足够样本）
        if !self.active || self.window.len() < Self::BOOTSTRAP_FRAMES {
            self.window.push_back(db);
            while self.window.len() > self.window_size {
                self.window.pop_front();
            }
        }

        let floor = self.noise_floor();
        let threshold_on = floor + self.margin_db;
        let threshold_off = floor + self.margin_db * self.release_ratio;

        if self.active {
            if db < threshold_off {
                self.active = false;
            }
        } else if db > threshold_on {
            self.active = true;
        }

        self.active
    }

    /// 当前噪声底（供日志/调试）
    pub fn current_noise_floor(&mut self) -> f32 {
        self.noise_floor()
    }

    pub fn reset(&mut self) {
        self.window.clear();
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize, amplitude: f32, freq: f32, sample_rate: f32) -> Vec<f32> {
        (0..len)
            .map(|i| {
                amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin()
            })
            .collect()
    }

    #[test]
    fn frame_db_reflects_level() {
        let quiet = tone(320, 0.001, 440.0, 16000.0);
        let loud = tone(320, 0.3, 440.0, 16000.0);
        assert!(EnergyVad::frame_db(&loud) > EnergyVad::frame_db(&quiet) + 30.0);
    }

    #[test]
    fn detects_speech_after_noise_floor_settles() {
        let mut vad = EnergyVad::new(8.0, 0.1);
        let noisy = tone(320, 0.002, 300.0, 16000.0); // 视为背景噪声
        let speech = tone(320, 0.3, 440.0, 16000.0);

        // 先喂一段噪声让噪声底收敛
        for _ in 0..40 {
            vad.is_speech(&noisy);
        }
        assert!(!vad.is_speech(&noisy), "纯噪声不应被判为语音");
        assert!(vad.is_speech(&speech), "明显高于噪声底应判为语音");
    }
}
