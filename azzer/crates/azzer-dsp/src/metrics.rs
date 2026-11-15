//! 输入质检五项（v3 §5.1）：有效带宽 / SNR / 削顶率 / 估计 RT60 / 码率。
//!
//! 全部是**确定性纯函数**：同样的输入永远得到同样的输出，可以脱机单测，
//! 也可以用 Python 独立实现交叉比对。
//!
//! ## 诚实标注：哪些是估计
//!
//! | 指标 | 方法 | 精度 |
//! |---|---|---|
//! | 有效带宽 | 6 个频点的 Goertzel 功率，-40dB 截止 | **粗估**；M1-R2 换 rustfft 做谱分析 |
//! | SNR | 最低 10% 帧作噪声底、最高 20% 帧作信号 | 常用近似，对稳态噪声较准 |
//! | 削顶率 | `abs(x) >= 0.999` 的采样占比 | 精确 |
//! | 估计 RT60 | T20 外推（能量下降 20dB 的时长 ×3） | **粗估**；稳态信号无法估计时为 `None` |
//! | 码率 | `采样率 × 位深 × 声道数` | 对 PCM WAV 精确；**压缩源的真实码率不可知** |
//!
//! RT60 算不出时返回 `None`（序列化为 null），**不填 0**——v3 §3.3 明确
//! 「缺项标 not_applicable，不许填 0」。

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::buffer::AudioBuffer;

/// 20ms 一帧，10ms 步长。
const FRAME_MS: f64 = 20.0;
const HOP_MS: f64 = 10.0;
/// 判定削顶的阈值（线性幅度）。
const CLIP_THRESHOLD: f32 = 0.999;
/// 有效带宽的截止：相对最强频点 -40dB（功率比 1e-4）。
const BW_CUTOFF_POWER: f64 = 1e-4;
const BW_PROBE_HZ: &[f64] = &[2_000.0, 4_000.0, 8_000.0, 12_000.0, 16_000.0, 20_000.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Grade {
    S,
    A,
    B,
    C,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeMetrics {
    /// 有效带宽（Hz，粗估）。
    pub effective_bandwidth_hz: f64,
    /// 信噪比（dB，近似）。
    pub snr_db: f64,
    /// 削顶采样占比，0..1。
    pub clip_ratio: f64,
    /// 估计混响时间（秒）。无法估计为 null，绝不填 0。
    pub rt60_s: Option<f64>,
    /// PCM 码率（bit/s）。压缩源的真实码率不可知，见 `bitrate_is_pcm`。
    pub bitrate_bps: f64,
    pub bitrate_is_pcm: bool,
    /// 时长（秒），也用于 N01 的契约断言。
    pub duration_s: f64,
    /// 峰值（dBFS），也用于 N01 的契约断言。
    pub peak_dbfs: f64,
    /// 分级结果。
    pub grade: Grade,
    /// 分级依据的可读说明，直接进前端提示。
    pub reason: String,
}

/// 帧 RMS 序列。
fn frame_rms(mono: &[f32], sample_rate: u32) -> Vec<f64> {
    let frame = ((FRAME_MS / 1000.0 * sample_rate as f64).round() as usize).max(1);
    let hop = ((HOP_MS / 1000.0 * sample_rate as f64).round() as usize).max(1);
    let mut out = Vec::new();
    let mut start = 0usize;
    while start + frame <= mono.len() {
        let mut sum = 0.0f64;
        for &x in &mono[start..start + frame] {
            sum += (x as f64) * (x as f64);
        }
        out.push((sum / frame as f64).sqrt());
        start += hop;
    }
    out
}

fn db20(x: f64) -> f64 {
    20.0 * x.max(f64::MIN_POSITIVE).log10()
}

/// 单频点的 Goertzel 功率（幅度平方，已按窗长归一化）。
fn goertzel_power(samples: &[f32], sample_rate: u32, freq: f64) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let omega = 2.0 * PI * freq / sample_rate as f64;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n as f64 * n as f64);
    power.max(0.0)
}

fn effective_bandwidth(mono: &[f32], sample_rate: u32) -> f64 {
    let win = 8192usize.min(mono.len());
    if win == 0 {
        return 0.0;
    }
    let start = (mono.len() / 4).min(mono.len() - win);
    let window = &mono[start..start + win];
    let nyquist = sample_rate as f64 / 2.0;
    let probes: Vec<f64> = BW_PROBE_HZ
        .iter()
        .copied()
        .filter(|&f| f < nyquist * 0.95)
        .collect();
    if probes.is_empty() {
        return 0.0;
    }
    let powers: Vec<f64> = probes
        .iter()
        .map(|&f| goertzel_power(window, sample_rate, f))
        .collect();
    let peak = powers.iter().copied().fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return 0.0;
    }
    let mut cutoff = probes[0];
    for (&f, &p) in probes.iter().zip(powers.iter()) {
        if p >= peak * BW_CUTOFF_POWER {
            cutoff = f;
        }
    }
    cutoff
}

fn snr_db(rms: &[f64]) -> f64 {
    if rms.is_empty() {
        return 0.0;
    }
    let mut sorted = rms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let noise_n = (n / 10).max(1);
    let signal_n = (n / 5).max(1);
    let noise: f64 = sorted[..noise_n].iter().sum::<f64>() / noise_n as f64;
    let signal: f64 = sorted[n - signal_n..].iter().sum::<f64>() / signal_n as f64;
    if noise <= 1e-12 {
        120.0 // 数字静音底，给一个明确上限而不是 +inf
    } else {
        db20(signal / noise).clamp(0.0, 120.0)
    }
}

/// T20 外推 RT60。找不到 20dB 衰减点返回 None。
/// 帧间隔由 `HOP_MS` 决定，不需要采样率参数。
///
/// **搜索窗口限制在峰值后 3 秒内**：T20 测的是局部衰减，不是"从峰值到
/// 整首歌结束"。不设限会跨过后续独立乐句，算出几十甚至几百秒的荒谬值
/// （实测夹具曾得 217.86s）。找不到就返回 None，明确标 not_applicable。
const RT60_MAX_SEARCH_S: f64 = 3.0;

fn rt60_t20(rms: &[f64]) -> Option<f64> {
    if rms.len() < 4 {
        return None;
    }
    let (peak_idx, &peak) = rms
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    if peak <= 1e-12 {
        return None;
    }
    let target = peak * 0.1; // -20dB
    let max_lag = (RT60_MAX_SEARCH_S / (HOP_MS / 1000.0)).ceil() as usize;
    let end = (peak_idx + 1 + max_lag).min(rms.len());
    for i in (peak_idx + 1)..end {
        if rms[i] <= target {
            let hop_s = HOP_MS / 1000.0;
            let t20 = (i - peak_idx) as f64 * hop_s;
            return Some((t20 * 3.0).max(hop_s));
        }
    }
    None
}

/// 峰值（线性），取**所有声道**的最大绝对值。
///
/// 不能用单声道平均：立体声里只有一侧有信号时，平均会把峰值压低 6dB，
/// 一条 -34dBFS 的合法源会被算成 -40dBFS 而被 N01 误拒。
pub fn peak_amplitude(buf: &AudioBuffer) -> f32 {
    buf.channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |a, &b| a.max(b.abs()))
}

/// 峰值（dBFS），按所有声道取最大。
pub fn peak_dbfs(buf: &AudioBuffer) -> f64 {
    db20(peak_amplitude(buf) as f64)
}

/// 削顶率：按声道独立统计，分母是全部声道的样本总数。
///
/// 不能用单声道平均：左右反相时平均后接近 0，真实的削顶会被完全抵消掉。
pub fn clip_ratio(buf: &AudioBuffer) -> f64 {
    let mut clipped = 0usize;
    let mut total = 0usize;
    for ch in &buf.channels {
        total += ch.len();
        clipped += ch.iter().filter(|&&x| x.abs() >= CLIP_THRESHOLD).count();
    }
    if total == 0 {
        0.0
    } else {
        clipped as f64 / total as f64
    }
}

/// 计算五项 + 分级。纯函数。
///
/// 峰值与削顶率按声道独立算；有效带宽、SNR、RT60 用单声道混合
/// （这三项描述的是整体频谱与能量包络，单声道混合是常规做法）。
pub fn analyze(buf: &AudioBuffer) -> GradeMetrics {
    let mono = buf.mono();
    let rms = frame_rms(&mono, buf.sample_rate);

    let peak_db = peak_dbfs(buf);
    let clip = clip_ratio(buf);

    let bandwidth = effective_bandwidth(&mono, buf.sample_rate);
    let snr = snr_db(&rms);
    let rt60 = rt60_t20(&rms);
    let bitrate = buf.sample_rate as f64 * buf.source_bits as f64 * buf.channel_count() as f64;
    let duration = buf.duration_secs();

    let (grade, reason) = classify(snr, clip, buf, rt60);

    GradeMetrics {
        effective_bandwidth_hz: bandwidth,
        snr_db: snr,
        clip_ratio: clip,
        rt60_s: rt60,
        bitrate_bps: bitrate,
        bitrate_is_pcm: true,
        duration_s: duration,
        peak_dbfs: peak_db,
        grade,
        reason,
    }
}

/// v3 §5.1 的分级判据，写成显式规则。
fn classify(
    snr: f64,
    clip: f64,
    buf: &AudioBuffer,
    rt60: Option<f64>,
) -> (Grade, String) {
    if clip > 0.001 {
        return (Grade::C, format!("削顶率 {:.4} 超过 0.001，波形已被切平", clip));
    }
    if snr < 18.0 {
        return (Grade::C, format!("SNR {:.1}dB 低于 18dB，底噪过重", snr));
    }
    if snr < 25.0 {
        return (Grade::B, format!("SNR {:.1}dB 落在 18–25dB 区间", snr));
    }
    if snr < 35.0 {
        return (Grade::A, format!("SNR {:.1}dB 落在 25–35dB 区间", snr));
    }
    let hires = buf.sample_rate >= 48_000 && buf.source_bits >= 24;
    let dry = rt60.map(|t| t < 0.25).unwrap_or(false);
    if hires && dry {
        (Grade::S, "48k/24bit 干声，SNR >35dB 且 RT60 <0.25s".into())
    } else {
        let mut why = Vec::new();
        if !hires {
            why.push(format!("不是 48k/24bit（当前 {}k/{}bit）", buf.sample_rate / 1000, buf.source_bits));
        }
        if !dry {
            why.push("RT60 未达到 <0.25s 或无法估计".to_string());
        }
        (Grade::A, format!("SNR >35dB，但 {}，降为 A 级", why.join("、")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::AudioBuffer;

    fn tone(freq: f64, secs: f64, amp: f32) -> AudioBuffer {
        let sr = 48_000u32;
        let n = (sr as f64 * secs) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * freq * i as f64 / sr as f64).sin() * amp as f64) as f32)
            .collect();
        AudioBuffer::new(vec![v], sr, 24).unwrap()
    }

    #[test]
    fn clip_ratio_is_exact() {
        let mut b = tone(440.0, 0.1, 0.5);
        b.channels[0][100] = 1.0;
        b.channels[0][101] = -1.0;
        let m = analyze(&b);
        assert!((m.clip_ratio - 2.0 / 4800.0).abs() < 1e-12);
    }

    #[test]
    fn heavy_clipping_forces_grade_c() {
        let mut b = tone(440.0, 0.2, 0.5);
        for x in b.channels[0].iter_mut() {
            *x = 1.0;
        }
        let m = analyze(&b);
        assert_eq!(m.grade, Grade::C);
        assert!(m.reason.contains("削顶"));
    }

    #[test]
    fn bitrate_is_pcm_formula() {
        let b = tone(440.0, 0.1, 0.5);
        let m = analyze(&b);
        assert_eq!(m.bitrate_bps, 48_000.0 * 24.0);
        assert!(m.bitrate_is_pcm);
    }

    #[test]
    fn pure_digital_silence_has_finite_snr() {
        let b = AudioBuffer::new(vec![vec![0.0; 4800]], 48_000, 24).unwrap();
        let m = analyze(&b);
        assert_eq!(m.snr_db, 120.0);
        assert!(m.peak_dbfs.is_finite() || m.peak_dbfs < -300.0);
    }

    #[test]
    fn goertzel_finds_the_tone_frequency() {
        let sr = 48_000u32;
        let v: Vec<f32> = (0..8192)
            .map(|i| (2.0 * PI * 1000.0 * i as f64 / sr as f64).sin() as f32)
            .collect();
        let p1k = goertzel_power(&v, sr, 1000.0);
        let p4k = goertzel_power(&v, sr, 4000.0);
        assert!(p1k > p4k * 100.0, "Goertzel 应在 1kHz 处能量远高于 4kHz");
    }

    #[test]
    fn metrics_are_deterministic() {
        let b = tone(440.0, 0.3, 0.4);
        assert_eq!(analyze(&b), analyze(&b));
    }

    /// 立体声只有一侧有信号时，峰值不能被单声道平均压低。
    #[test]
    fn peak_is_per_channel_not_mono_average() {
        let left = vec![0.019f32; 4800]; // -34.4 dBFS
        let right = vec![0.0f32; 4800];
        let b = AudioBuffer::new(vec![left, right], 48_000, 24).unwrap();
        let db = peak_dbfs(&b);
        assert!(db > -35.0 && db < -34.0, "单侧信号的峰值应约 -34.4dBFS，实得 {db}");
        // 单声道平均会得到 -40.4dBFS，那是错的
        let mono_peak = b.mono().iter().fold(0.0f32, |a, &c| a.max(c.abs()));
        assert!(db20(mono_peak as f64) < -40.0);
    }

    /// 左右反相的削顶不能被单声道平均抵消掉。
    #[test]
    fn clipping_is_detected_even_when_channels_cancel_out() {
        let left = vec![1.0f32; 4800];
        let right = vec![-1.0f32; 4800];
        let b = AudioBuffer::new(vec![left, right], 48_000, 24).unwrap();
        assert!((clip_ratio(&b) - 1.0).abs() < 1e-9, "全部样本削顶，比例应为 1");
        let mono_peak = b.mono().iter().fold(0.0f32, |a, &c| a.max(c.abs()));
        assert!(mono_peak < 1e-6, "单声道混合后应当抵消为 0，证明必须按声道统计");
        assert_eq!(analyze(&b).grade, Grade::C);
    }
}
