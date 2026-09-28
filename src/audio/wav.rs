use crate::error::{AppError, AppResult};
use hound::{WavSpec, WavWriter};
use std::io::Cursor;

/// 将 f32 音频样本编码为 WAV 格式
pub fn encode_wav(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> AppResult<Vec<u8>> {
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut cursor, spec)
        .map_err(|e| AppError::Audio(format!("Failed to create WAV writer: {}", e)))?;

    // 将 f32 样本（范围 -1.0 到 1.0）转换为 i16
    for &sample in samples {
        let sample_i16 = (sample * 32767.0) as i16;
        writer
            .write_sample(sample_i16)
            .map_err(|e| AppError::Audio(format!("Failed to write WAV sample: {}", e)))?;
    }

    writer
        .finalize()
        .map_err(|e| AppError::Audio(format!("Failed to finalize WAV: {}", e)))?;

    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_wav() {
        let samples = vec![0.1; 1000];
        let wav = encode_wav(&samples, 16000, 1).unwrap();
        
        // WAV 文件应该有有效的头
        assert!(wav.len() > 44); // 最小 WAV 头大小
        assert_eq!(&wav[0..4], b"RIFF");
    }
}
