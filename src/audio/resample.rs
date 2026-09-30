/// 简单的线性插值重采样器
///
/// 用于把设备原生采样率（如 48000Hz）转换为 Whisper 期望的采样率（通常 16000Hz）。
/// 语音识别对重采样质量要求不算极端，线性插值已经足够。
pub struct LinearResampler {
    /// 输入采样率 / 输出采样率
    ratio: f64,
    /// 当前在输入样本中的浮点位置
    pos: f64,
    /// 上一次内插用到的最后一个输入样本
    last: Option<f32>,
    input_rate: u32,
    output_rate: u32,
}

impl LinearResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            ratio: input_rate as f64 / output_rate as f64,
            pos: 0.0,
            last: None,
            input_rate,
            output_rate,
        }
    }

    /// 是否需要重采样
    pub fn is_needed(&self) -> bool {
        self.input_rate != self.output_rate
    }

    /// 处理一段输入样本，将结果追加到 output
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        // 采样率相同，直接透传
        if !self.is_needed() {
            output.extend_from_slice(input);
            return;
        }

        // 为保持连续性，把上次最后一个样本拼到前面
        let mut samples: Vec<f32> = Vec::with_capacity(input.len() + 1);
        if let Some(last) = self.last {
            samples.push(last);
        }
        samples.extend_from_slice(input);

        // 位置从 0 开始；若上次有残留，pos 会小于 1
        while self.pos + 1.0 < samples.len() as f64 {
            let idx = self.pos.floor() as usize;
            let frac = (self.pos - idx as f64) as f32;
            let a = samples[idx];
            let b = samples[idx + 1];
            output.push(a + (b - a) * frac);
            self.pos += self.ratio;
        }

        // 保存状态：循环退出时 pos >= len-1。下一轮会把本轮最后一个样本
        // （索引 len-1）放到索引 0，因此位置整体平移 len-1。
        self.pos -= (samples.len() - 1) as f64;
        self.last = samples.last().copied();
    }
}

/// 将交错多声道样本下混为单声道
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    if channels == 1 {
        return interleaved.to_vec();
    }

    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let start = frame * channels;
        let mut sum = 0.0f32;
        for ch in 0..channels {
            sum += interleaved[start + ch];
        }
        mono.push(sum / channels as f32);
    }
    mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_stereo() {
        let input = vec![1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mono = downmix_to_mono(&input, 2);
        assert_eq!(mono, vec![0.5, 0.5, 1.0]);
    }

    #[test]
    fn test_downmix_mono_passthrough() {
        let input = vec![0.1, 0.2, 0.3];
        let mono = downmix_to_mono(&input, 1);
        assert_eq!(mono, input);
    }

    #[test]
    fn test_resample_downsample() {
        let mut resampler = LinearResampler::new(48000, 16000);
        // 48000 个样本应大致产生 16000 个输出样本
        let input: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut output = Vec::new();
        resampler.process(&input, &mut output);
        assert!(output.len() >= 15000 && output.len() <= 16500, "got {}", output.len());
    }

    #[test]
    fn test_resample_passthrough() {
        let mut resampler = LinearResampler::new(16000, 16000);
        let input = vec![0.1, 0.2, 0.3];
        let mut output = Vec::new();
        resampler.process(&input, &mut output);
        assert_eq!(output, input);
    }

    #[test]
    fn test_resample_streaming_blocks_do_not_stall_or_drift() {
        for &(rate, block) in &[(48000u32, 512usize), (48000, 1024), (44100, 512), (44100, 1024)] {
            let mut resampler = LinearResampler::new(rate, 16000);
            let total = rate as usize * 5;
            let input: Vec<f32> = (0..total).map(|i| (i as f32 * 0.01).sin()).collect();
            let mut output = Vec::new();
            for chunk in input.chunks(block) {
                resampler.process(chunk, &mut output);
            }
            let expected = total as f64 * 16000.0 / rate as f64;
            assert!(
                (output.len() as f64 - expected).abs() <= 2.0,
                "rate={rate} block={block}: got {} expected ~{expected}",
                output.len()
            );
        }
    }

    #[test]
    fn test_resample_streaming_matches_one_shot() {
        let input: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.013).sin()).collect();
        let mut one = LinearResampler::new(48000, 16000);
        let mut expected = Vec::new();
        one.process(&input, &mut expected);
        let mut streamed = LinearResampler::new(48000, 16000);
        let mut got = Vec::new();
        for chunk in input.chunks(1024) {
            streamed.process(chunk, &mut got);
        }
        assert_eq!(got.len(), expected.len());
        for (a, b) in got.iter().zip(&expected) {
            assert!((a - b).abs() < 1e-4);
        }
    }
}
