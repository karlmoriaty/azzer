//! 重采样。
//!
//! **R1 占位实现**：线性插值，只用于让 N01 在非 48k 输入时也能跑通链路，
//! 并验证「48k 输入零损失直通」这条关键性质。
//!
//! ⚠️ 线性插值会劣化高频，**不能用于成品音质链路**。M1-R2 按 v3.2 定案
//! 换成 `rubato` 的 Sinc 固定窗重采样。届时本文件替换为适配器，接口不变。
//!
//! 「源才是资产」这条判断下，48k 输入走的是纯 clone，不经过任何插值。

use crate::buffer::{AudioBuffer, DspError};

pub const INTERNAL_SAMPLE_RATE: u32 = 48_000;

/// 归一化到内部采样率。已是目标采样率时返回同一个缓冲区（零损失）。
pub fn to_internal_rate(buf: &AudioBuffer) -> Result<AudioBuffer, DspError> {
    resample_linear(buf, INTERNAL_SAMPLE_RATE)
}

pub fn resample_linear(buf: &AudioBuffer, target_rate: u32) -> Result<AudioBuffer, DspError> {
    if target_rate == 0 {
        return Err(DspError::Invalid("目标采样率为 0"));
    }
    if buf.sample_rate == target_rate {
        return Ok(buf.clone());
    }

    let ratio = target_rate as f64 / buf.sample_rate as f64;
    let src_frames = buf.frame_count();
    if src_frames == 0 {
        return AudioBuffer::new(vec![Vec::new(); buf.channel_count()], target_rate, buf.source_bits);
    }
    let dst_frames = ((src_frames as f64 * ratio).floor() as usize).max(1);

    let mut channels = Vec::with_capacity(buf.channel_count());
    for ch in &buf.channels {
        let mut out = Vec::with_capacity(dst_frames);
        for i in 0..dst_frames {
            let pos = i as f64 / ratio;
            let i0 = (pos.floor() as usize).min(src_frames - 1);
            let i1 = (i0 + 1).min(src_frames - 1);
            let t = (pos - i0 as f64) as f32;
            out.push(ch[i0] * (1.0 - t) + ch[i1] * t);
        }
        channels.push(out);
    }
    AudioBuffer::new(channels, target_rate, buf.source_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_bit_identical_clone() {
        let b = AudioBuffer::new(vec![vec![0.1, -0.2, 0.3]], 48_000, 24).unwrap();
        let r = to_internal_rate(&b).unwrap();
        assert_eq!(r, b, "48k 输入必须零损失直通");
    }

    #[test]
    fn output_frame_count_matches_ratio() {
        let b = AudioBuffer::new(vec![vec![0.0; 44_100]], 44_100, 16).unwrap();
        let r = to_internal_rate(&b).unwrap();
        assert_eq!(r.frame_count(), 48_000);
        assert_eq!(r.sample_rate, 48_000);
        assert_eq!(r.source_bits, 16, "源位深信息要保留给 N02 分级");
    }

    #[test]
    fn constant_signal_survives_resampling_exactly() {
        let b = AudioBuffer::new(vec![vec![0.25; 1000]], 44_100, 16).unwrap();
        let r = resample_linear(&b, 48_000).unwrap();
        assert!(r.channels[0].iter().all(|&x| (x - 0.25).abs() < 1e-6));
    }

    #[test]
    fn empty_input_stays_empty() {
        let b = AudioBuffer::new(vec![Vec::new()], 44_100, 16).unwrap();
        let r = to_internal_rate(&b).unwrap();
        assert_eq!(r.frame_count(), 0);
    }

    #[test]
    fn zero_target_rate_is_rejected() {
        let b = AudioBuffer::new(vec![vec![0.0; 10]], 44_100, 16).unwrap();
        assert!(resample_linear(&b, 0).is_err());
    }
}
