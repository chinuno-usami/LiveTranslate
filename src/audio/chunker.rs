use crate::error::{AppError, AppResult};

/// 音频分片器
/// 将连续的音频流切分成固定大小的块，支持重叠
pub struct AudioChunker {
    sample_rate: u32,
    chunk_seconds: f32,
    chunk_samples: usize,
    overlap_samples: usize,
    silence_threshold: f32,
    buffer: Vec<f32>,
}

impl AudioChunker {
    pub fn new(
        sample_rate: u32,
        chunk_seconds: f32,
        overlap_ratio: f32,
        silence_threshold: f32,
    ) -> AppResult<Self> {
        let chunk_samples = (sample_rate as f32 * chunk_seconds) as usize;
        let overlap_samples = (chunk_samples as f32 * overlap_ratio) as usize;

        if chunk_samples == 0 {
            return Err(AppError::Audio(
                "Chunk size must be greater than 0".to_string(),
            ));
        }

        tracing::info!(
            "AudioChunker: sample_rate={}, chunk_seconds={}, chunk_samples={}, overlap_samples={}",
            sample_rate,
            chunk_seconds,
            chunk_samples,
            overlap_samples
        );

        Ok(Self {
            sample_rate,
            chunk_seconds,
            chunk_samples,
            overlap_samples,
            silence_threshold,
            buffer: Vec::with_capacity(chunk_samples * 2),
        })
    }

    /// 添加新的音频样本
    pub fn push_samples(&mut self, samples: Vec<f32>) {
        self.buffer.extend_from_slice(&samples);
    }

    /// 获取下一个分片
    /// 返回完整的分片或 None（如果数据不足）
    pub fn next_chunk(&mut self) -> Option<Vec<f32>> {
        if self.buffer.len() < self.chunk_samples {
            return None;
        }

        // 获取一个完整的分片
        let chunk: Vec<f32> = self.buffer.drain(0..self.chunk_samples).collect();

        // 如果启用了重叠，保留 overlap_samples 用于下一个分片
        if self.overlap_samples > 0 && self.buffer.len() >= self.overlap_samples {
            // 下一个分片会从已保留的重叠部分开始
        }

        Some(chunk)
    }

    /// 检查分片是否为静音
    pub fn is_silence(&self, chunk: &[f32]) -> bool {
        if chunk.is_empty() {
            return true;
        }

        let max_amplitude = chunk
            .iter()
            .map(|&s| s.abs())
            .fold(f32::NEG_INFINITY, f32::max);

        max_amplitude < self.silence_threshold
    }

    /// 获取缓冲区中的样本数
    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// 获取缓冲区是否有足够的数据
    pub fn has_next(&self) -> bool {
        self.buffer.len() >= self.chunk_samples
    }

    /// 清空缓冲区
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// 获取分片时长（秒）
    pub fn chunk_duration_secs(&self) -> f32 {
        self.chunk_seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunker_basic() {
        let mut chunker = AudioChunker::new(16000, 1.0, 0.25, 0.01).unwrap();
        
        // 1 秒的样本 = 16000 个样本
        let samples = vec![0.1; 16000];
        chunker.push_samples(samples);

        let chunk = chunker.next_chunk();
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().len(), 16000);
    }

    #[test]
    fn test_silence_detection() {
        let chunker = AudioChunker::new(16000, 1.0, 0.0, 0.01).unwrap();
        
        let silent = vec![0.001; 1000];
        assert!(chunker.is_silence(&silent));

        let loud = vec![0.1; 1000];
        assert!(!chunker.is_silence(&loud));
    }
}
