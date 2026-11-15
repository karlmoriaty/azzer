//! 音频缓冲区与 WAV 读写。
//!
//! 内部统一用**平面 f32**（每声道一条 Vec），范围 [-1, 1)。
//! 这是全链路唯一的音频表示，避免每个节点各存一种格式。
//!
//! 解码支持 PCM 16/24/32-bit int 与 32-bit float；编码固定 48k/24bit
//! （N01 的归一化目标）。**输入已是 48k 时不做任何重采样**，零损失直通。

use std::path::Path;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

/// 归一化后的音频。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    /// 平面声道，每个 Vec 长度必须相同。
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    /// 源文件的位深，仅供 N02 分级使用（S 级要求 24bit）。
    pub source_bits: u16,
}

impl AudioBuffer {
    /// 构造时校验声道数一致，避免后面每个函数都重复检查。
    pub fn new(channels: Vec<Vec<f32>>, sample_rate: u32, source_bits: u16) -> Result<Self, DspError> {
        if channels.is_empty() {
            return Err(DspError::Invalid("声道数为 0"));
        }
        let n = channels[0].len();
        if channels.iter().any(|c| c.len() != n) {
            return Err(DspError::Invalid("各声道长度不一致"));
        }
        if sample_rate == 0 {
            return Err(DspError::Invalid("采样率为 0"));
        }
        Ok(Self { channels, sample_rate, source_bits })
    }

    pub fn frame_count(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn duration_secs(&self) -> f64 {
        self.frame_count() as f64 / self.sample_rate as f64
    }

    /// 单声道混合视图（等权平均）。指标计算都基于它。
    pub fn mono(&self) -> Vec<f32> {
        let n = self.frame_count();
        let mut out = vec![0.0f32; n];
        for ch in &self.channels {
            for (i, &s) in ch.iter().enumerate() {
                out[i] += s;
            }
        }
        let k = self.channel_count() as f32;
        if k > 1.0 {
            for v in &mut out {
                *v /= k;
            }
        }
        out
    }
}

/// 解码 WAV 文件。不支持的采样格式会明确报错。
pub fn decode_wav(path: impl AsRef<Path>) -> Result<AudioBuffer, DspError> {
    let path = path.as_ref();
    let reader = WavReader::open(path).map_err(DspError::Wav)?;
    decode_from_reader(reader)
}

/// 从内存字节解码 WAV（内容寻址存储取回的产物走这条路）。
pub fn decode_wav_bytes(bytes: &[u8]) -> Result<AudioBuffer, DspError> {
    let reader = WavReader::new(std::io::Cursor::new(bytes)).map_err(DspError::Wav)?;
    decode_from_reader(reader)
}

fn decode_from_reader<R: std::io::Read>(mut reader: WavReader<R>) -> Result<AudioBuffer, DspError> {
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Int && spec.sample_format != SampleFormat::Float {
        return Err(DspError::Unsupported(format!(
            "采样格式 {:?}",
            spec.sample_format
        )));
    }
    let channels = planarize(&mut reader, &spec)?;
    AudioBuffer::new(channels, spec.sample_rate, spec.bits_per_sample)
}

/// hound 的 `Sample` trait 只保证可读写，不提供数值转换，
/// 所以按位深显式分支，用 `as f32` 转换。宏只是消掉重复循环。
macro_rules! read_as {
    ($reader:expr, $ty:ty, $scale:expr) => {{
        let mut v: Vec<f32> = Vec::new();
        for s in $reader.samples::<$ty>() {
            v.push(s.map_err(DspError::Wav)? as f32 / $scale);
        }
        v
    }};
}

fn planarize<R: std::io::Read>(
    reader: &mut WavReader<R>,
    spec: &WavSpec,
) -> Result<Vec<Vec<f32>>, DspError> {
    let nch = spec.channels as usize;
    if nch == 0 {
        return Err(DspError::Invalid("声道数为 0"));
    }

    let flat: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 8) => read_as!(reader, i8, 128.0f32),
        (SampleFormat::Int, 16) => read_as!(reader, i16, 32_768.0f32),
        (SampleFormat::Int, 24) => read_as!(reader, i32, 8_388_608.0f32),
        (SampleFormat::Int, 32) => read_as!(reader, i32, 2_147_483_648.0f32),
        (SampleFormat::Float, 32) => read_as!(reader, f32, 1.0f32),
        _ => {
            return Err(DspError::Unsupported(format!(
                "{:?}/{}bit",
                spec.sample_format, spec.bits_per_sample
            )))
        }
    };

    let frames = flat.len() / nch;
    let mut out = vec![Vec::with_capacity(frames); nch];
    for (i, &s) in flat.iter().enumerate() {
        out[i % nch].push(s);
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("WAV 读写失败：{0}")]
    Wav(hound::Error),
    #[error("文件 IO 失败：{0}：{1}")]
    Io(std::path::PathBuf, std::io::Error),
    #[error("音频数据非法：{0}")]
    Invalid(&'static str),
    #[error("不支持的音频格式：{0}")]
    Unsupported(String),
}

impl From<hound::Error> for DspError {
    fn from(e: hound::Error) -> Self {
        DspError::Wav(e)
    }
}

/// 写 24bit PCM WAV（N01 的 `normalized.wav`）。
pub fn write_wav_24(path: impl AsRef<Path>, buf: &AudioBuffer) -> Result<(), DspError> {
    let path = path.as_ref().to_path_buf();
    let bytes = encode_wav_24_bytes(buf)?;
    std::fs::write(&path, bytes).map_err(|e| DspError::Io(path, e))
}

/// 编码成内存里的 24bit PCM WAV，供内容寻址存储直接落盘。
/// 与 [`write_wav_24`] 共用同一套数值转换，避免两条路径出现偏差。
pub fn encode_wav_24_bytes(buf: &AudioBuffer) -> Result<Vec<u8>, DspError> {
    let spec = WavSpec {
        channels: buf.channel_count() as u16,
        sample_rate: buf.sample_rate,
        bits_per_sample: 24,
        sample_format: SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut cursor, spec).map_err(DspError::Wav)?;
        for i in 0..buf.frame_count() {
            for ch in &buf.channels {
                let v = (ch[i] as f64 * 8_388_608.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0) as i32;
                writer.write_sample(v).map_err(DspError::Wav)?;
            }
        }
        writer.finalize().map_err(DspError::Wav)?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_and_duration() {
        let b = AudioBuffer::new(vec![vec![0.0; 48_000], vec![0.0; 48_000]], 48_000, 24).unwrap();
        assert_eq!(b.frame_count(), 48_000);
        assert_eq!(b.channel_count(), 2);
        assert!((b.duration_secs() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mismatched_channel_lengths_are_rejected() {
        assert!(matches!(
            AudioBuffer::new(vec![vec![0.0; 10], vec![0.0; 9]], 48_000, 24),
            Err(DspError::Invalid(_))
        ));
    }

    #[test]
    fn mono_is_channel_average() {
        let b = AudioBuffer::new(vec![vec![1.0, -1.0], vec![0.0, 0.0]], 48_000, 24).unwrap();
        assert_eq!(b.mono(), vec![0.5, -0.5]);
    }

    #[test]
    fn wav_24_roundtrip_preserves_samples_within_quantization() {
        let dir = std::env::current_dir().unwrap().join(".test-wav");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("roundtrip.wav");
        let original: Vec<f32> = (0..1000)
            .map(|i| (i as f32 * 0.01).sin() * 0.5)
            .collect();
        let b = AudioBuffer::new(vec![original.clone()], 48_000, 24).unwrap();
        write_wav_24(&p, &b).unwrap();
        let back = decode_wav(&p).unwrap();
        assert_eq!(back.sample_rate, 48_000);
        assert_eq!(back.source_bits, 24);
        assert_eq!(back.frame_count(), 1000);
        let max_err = original
            .iter()
            .zip(back.channels[0].iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-6, "24bit 量化误差应小于 1e-6，实得 {max_err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
