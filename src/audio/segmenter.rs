//! 基于 VAD 的语音分段器
//!
//! 把连续音频流切成**完整句子**，而不是固定时长硬切：
//!
//! ```text
//! 静音 ──┐            ┌──────────────┐
//!        │  语音开始   │   语音持续    │  语音结束
//!        └────────────┘              └──────────
//!          ↑ pre_pad                     ↑ post_pad
//! ```
//!
//! - 语音一旦检出立即确认开始（抖动由 VAD 后端的迟滞处理）
//! - 短于 `min_speech_ms` 的片段丢弃（过滤毛刺噪声）
//! - 静音需持续 `min_silence_ms` 才确认结束（抗句中停顿）
//! - 超过 `max_speech_ms` 强制切分，避免延迟与内存无限增长
//! - 开始前保留 `pre_pad_ms`、结束后保留 `post_pad_ms`，避免吃掉首尾音素

use std::collections::VecDeque;

use crate::audio::detector::{VadEngine, VadStats};
use crate::config::VadConfig;

pub struct SpeechSegmenter {
    sample_rate: u32,
    frame_len: usize,
    vad: VadEngine,

    /// 确认"说完"所需连续静音帧数
    release_frames: u32,
    /// 单个片段最长帧数（超过则强制切分）
    max_frames: u32,
    /// 片段内最短语音帧数（低于则丢弃；仅统计判定为语音的帧）
    min_voiced_frames: u32,
    /// 起始前保留的帧数
    pre_pad_frames: usize,
    /// 结束后保留的帧数
    post_pad_frames: usize,

    /// 不足一帧的剩余样本
    leftover: Vec<f32>,
    /// 语音开始前的滚动缓冲（用于 pre-pad）
    pre_buf: VecDeque<f32>,
    /// 当前累积的语音片段
    utterance: Vec<f32>,
    /// 当前片段内判定为语音的帧数
    voiced_frames: u32,
    unvoiced_run: u32,
    in_speech: bool,
    /// 当前段是强制切段后的续段
    continuing: bool,
    /// 已完成的片段
    ready: VecDeque<Vec<f32>>,
}

impl SpeechSegmenter {
    pub fn new(cfg: &VadConfig, sample_rate: u32, vad: VadEngine) -> Self {
        // 帧长由 VAD 后端决定：能量型跟随 frame_ms，Silero 固定为 512/256 样本
        let frame_len = vad.frame_len();
        let frame_ms = (frame_len as f32 * 1000.0 / sample_rate.max(1) as f32).max(0.1);
        let frames = |ms: u32| ((ms as f32 / frame_ms).ceil() as u32).max(1);

        Self {
            sample_rate,
            frame_len,
            vad,
            release_frames: frames(cfg.min_silence_ms),
            max_frames: frames(cfg.max_speech_ms),
            min_voiced_frames: frames(cfg.min_speech_ms),
            pre_pad_frames: (cfg.pre_pad_ms as f32 / frame_ms).ceil() as usize,
            post_pad_frames: (cfg.post_pad_ms as f32 / frame_ms).ceil() as usize,
            leftover: Vec::new(),
            pre_buf: VecDeque::new(),
            utterance: Vec::new(),
            voiced_frames: 0,
            unvoiced_run: 0,
            in_speech: false,
            continuing: false,
            ready: VecDeque::new(),
        }
    }

    /// 当前噪声底（dBFS），仅能量型后端可用
    pub fn noise_floor_db(&mut self) -> Option<f32> {
        self.vad.noise_floor_db()
    }

    /// VAD 概率统计（仅 Silero 后端可用），用于调参观测
    pub fn stats(&mut self) -> Option<VadStats> {
        self.vad.stats()
    }

    /// 喂入任意长度的音频（单声道 f32）
    pub fn push(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        self.leftover.extend_from_slice(samples);

        while self.leftover.len() >= self.frame_len {
            let frame: Vec<f32> = self.leftover.drain(..self.frame_len).collect();
            self.process_frame(&frame);
        }
    }

    fn process_frame(&mut self, frame: &[f32]) {
        let speech = self.vad.is_speech(frame);

        if self.in_speech {
            self.utterance.extend_from_slice(frame);

            if speech {
                self.voiced_frames += 1;
                self.unvoiced_run = 0;
            } else {
                self.unvoiced_run += 1;
            }

            let ended_by_silence = self.unvoiced_run >= self.release_frames;
            let ended_by_length = self.utterance.len() >= self.max_frames as usize * self.frame_len;

            if ended_by_silence {
                self.finish(false);
            } else if ended_by_length {
                self.finish(true);
            }
        } else {
            if !speech {
                // 维护 pre-roll 缓冲（只存触发帧之前的帧）
                self.pre_buf.extend(frame.iter().copied());
                let cap = self.pre_pad_frames * self.frame_len;
                while self.pre_buf.len() > cap {
                    self.pre_buf.pop_front();
                }
            } else {
                // 立即确认开始：不再要求连续 N 帧（抖动已由 VAD 后端的迟滞处理），
                // 真正的短毛刺靠 min_voiced_frames 过滤
                self.in_speech = true;
                self.voiced_frames = 1;
                self.unvoiced_run = 0;
                self.utterance.clear();
                self.continuing = false;
                self.utterance.extend(self.pre_buf.iter().copied());
                // 触发帧本身也属于语音，不能丢
                self.utterance.extend(frame.iter().copied());
                self.pre_buf.clear();
            }
        }
    }

    fn finish(&mut self, forced: bool) {
        let mut utt = std::mem::take(&mut self.utterance);

        if !forced {
            // 去掉多余的尾部静音，只保留 post_pad
            let trailing = self.unvoiced_run as usize * self.frame_len;
            let keep = self.post_pad_frames * self.frame_len;
            if trailing > keep {
                let cut = utt.len().saturating_sub(trailing - keep);
                utt.truncate(cut);
            }
        }

        // 强制切段后的续段即使很短也属于同一句话，不做最短语音过滤
        if self.voiced_frames >= self.min_voiced_frames
            || (self.continuing && self.voiced_frames > 0)
        {
            let ms = utt.len() as u32 * 1000 / self.sample_rate.max(1);
            tracing::debug!(
                "VAD segment: {} ms ({} voiced frames){}",
                ms,
                self.voiced_frames,
                if forced { " (forced cut)" } else { "" }
            );
            self.ready.push_back(utt);
        }

        if forced {
            // 仍在说话：立刻开启下一段，避免丢掉这段时间的音频
            self.continuing = true;
            self.in_speech = true;
            self.voiced_frames = 0;
            self.unvoiced_run = 0;
        } else {
            self.continuing = false;
            self.in_speech = false;
            self.voiced_frames = 0;
            self.unvoiced_run = 0;
            self.pre_buf.clear();
        }
    }

    /// 取出一个已完成的语音片段
    pub fn pop(&mut self) -> Option<Vec<f32>> {
        self.ready.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16000;

    fn segmenter(cfg: &VadConfig) -> SpeechSegmenter {
        let (engine, _) = VadEngine::from_config(cfg, SR);
        SpeechSegmenter::new(cfg, SR, engine)
    }

    fn cfg() -> VadConfig {
        VadConfig {
            enabled: true,
            frame_ms: 20,
            margin_db: 8.0,
            noise_percentile: 0.1,
            min_speech_ms: 200,
            min_silence_ms: 300,
            max_speech_ms: 10_000,
            pre_pad_ms: 100,
            post_pad_ms: 100,
            // 其余字段（backend / silero_*）保持默认，确保分段器测试
            // 使用能量后端，不依赖 ONNX Runtime
            ..Default::default()
        }
    }

    fn tone(ms: u32, amplitude: f32) -> Vec<f32> {
        let len = (SR as f32 * ms as f32 / 1000.0) as usize;
        (0..len)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SR as f32).sin())
            .collect()
    }

    #[test]
    fn no_segment_on_pure_silence() {
        let mut seg = segmenter(&cfg());
        seg.push(&tone(2000, 0.001));
        assert!(seg.pop().is_none(), "纯静音不应产生片段");
    }

    #[test]
    fn drops_short_blip() {
        let mut seg = segmenter(&cfg());

        // 静音 -> 20ms 毛刺（1 帧，远短于 min_speech_ms=200ms）-> 静音
        seg.push(&tone(500, 0.001));
        seg.push(&tone(20, 0.3));
        seg.push(&tone(800, 0.001));

        let segments: Vec<_> = std::iter::from_fn(|| seg.pop()).collect();
        assert!(
            segments.is_empty(),
            "短毛刺不应产生片段, got {}",
            segments.len()
        );
    }

    #[test]
    fn emits_one_segment_for_a_sentence() {
        let mut seg = segmenter(&cfg());

        // 静音 -> 说话 -> 静音
        seg.push(&tone(600, 0.001));
        seg.push(&tone(1200, 0.3));
        seg.push(&tone(800, 0.001));

        let segments: Vec<_> = std::iter::from_fn(|| seg.pop()).collect();
        assert_eq!(segments.len(), 1, "应恰好产生一个完整语句片段");
        // 片段应明显长于纯语音部分（因为带了前后留白）
        let ms = segments[0].len() as f32 * 1000.0 / SR as f32;
        assert!(ms >= 1200.0, "片段时长应覆盖整句, got {ms} ms");
    }

    #[test]
    fn does_not_split_on_short_pause_inside_sentence() {
        let mut seg = segmenter(&cfg());

        seg.push(&tone(500, 0.001));
        seg.push(&tone(800, 0.3)); // 说
        seg.push(&tone(150, 0.001)); // 句中短暂停顿（< min_silence 300ms）
        seg.push(&tone(800, 0.3)); // 继续
        seg.push(&tone(800, 0.001));

        let segments: Vec<_> = std::iter::from_fn(|| seg.pop()).collect();
        assert_eq!(segments.len(), 1, "句中停顿不应切分");
    }

    #[test]
    fn force_cuts_long_speech() {
        let mut c = cfg();
        c.max_speech_ms = 1000;
        let mut seg = segmenter(&c);

        seg.push(&tone(500, 0.001));
        seg.push(&tone(3500, 0.3)); // 连续说话 3.5s，远超 max 1s

        let segments: Vec<_> = std::iter::from_fn(|| seg.pop()).collect();
        assert!(
            segments.len() >= 2,
            "超长语音应被强制切分, got {}",
            segments.len()
        );
    }
}
