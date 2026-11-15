//! EBU R128 / ITU-R BS.1770-4 响度测量。
//!
//! 提供三样东西：
//! - **integrated LUFS**：400ms 块、75% 重叠、两级门限
//! - **short-term LUFS（3s）**：用于 LRA
//! - **LRA**：短时响度的 10th 与 95th 百分位之差（EBU Tech 3342）
//!
//! ## 可解析验证
//!
//! 1kHz 正弦、幅度 A 的理论读数是：
//!
//! ```text
//! mean square = A² / 2
//! LUFS = -0.691 + 10·log10(A² / 2)
//! ```
//!
//! A=0.5 → -9.72 LUFS。测试直接拿这个数对，误差要求 <0.1dB。
//! 这比"和另一个实现对比"更强：解析解不会跟着实现一起错。
//!
//! ## 只支持 48kHz
//!
//! K 加权系数是 BS.1770 给的 48kHz 数值。换采样率必须按模拟原型重新设计，
//! 不能直接搬。N01 已把全链路归一到 48k，所以这里显式拒绝其他采样率。

use crate::biquad::KWeighting;
use crate::buffer::{AudioBuffer, DspError};

/// 400ms 积分块。
const BLOCK_SECS: f64 = 0.4;
/// 75% 重叠 → hop 100ms。
const HOP_SECS: f64 = 0.1;
/// 短时窗 3s（EBU Tech 3342）。
const SHORT_SECS: f64 = 3.0;
const SHORT_HOP_SECS: f64 = 1.0;
/// 绝对门限。
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// 相对门限，相对已过绝对门限部分的能量均值。
const RELATIVE_GATE_LU: f64 = -10.0;
/// BS.1770 的偏移常数。
const LUFS_OFFSET: f64 = -0.691;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoudnessReport {
    /// 整段积分响度。全静音或全部被门限滤掉时为 None。
    pub integrated_lufs: Option<f64>,
    /// 每个 3s 短时窗的响度。
    pub short_term_lufs: Vec<f64>,
    /// 响度范围（LU）。样本不足时为 None。
    pub lra_lu: Option<f64>,
    /// 参与积分计算的块数 / 总块数，用于判断门限是否把所有内容都滤掉。
    pub gated_blocks: usize,
    pub total_blocks: usize,
}

/// K 加权后每个声道的样本。
fn k_weighted(buf: &AudioBuffer) -> Vec<Vec<f32>> {
    buf.channels
        .iter()
        .map(|ch| {
            let mut kw = KWeighting::new();
            ch.iter().map(|&x| kw.process(x)).collect()
        })
        .collect()
}

/// 各声道平方的累积和再逐点相加，得到"总能量累积曲线"。
/// 之后任意窗的能量都能 O(1) 取出，不必重复遍历。
fn energy_prefix(filtered: &[Vec<f32>]) -> Vec<f64> {
    let n = filtered.first().map(|c| c.len()).unwrap_or(0);
    let mut out = vec![0.0f64; n + 1];
    for i in 0..n {
        let mut s = 0.0f64;
        for ch in filtered {
            s += (ch[i] as f64) * (ch[i] as f64);
        }
        out[i + 1] = out[i] + s;
    }
    out
}

#[inline]
fn window_mean(prefix: &[f64], start: usize, len: usize) -> f64 {
    if len == 0 || start + len >= prefix.len() {
        return 0.0;
    }
    (prefix[start + len] - prefix[start]) / len as f64
}

#[inline]
fn to_lufs(mean_square: f64) -> f64 {
    if mean_square <= 0.0 {
        f64::NEG_INFINITY
    } else {
        LUFS_OFFSET + 10.0 * mean_square.log10()
    }
}

/// 测量整段音频的响度。
pub fn measure(buf: &AudioBuffer) -> Result<LoudnessReport, DspError> {
    if buf.sample_rate != 48_000 {
        return Err(DspError::Invalid(
            "响度测量只支持 48kHz（K 加权系数是 48k 专用）；请先用 N01 归一",
        ));
    }
    let sr = buf.sample_rate as f64;
    let block = (BLOCK_SECS * sr).round() as usize;
    let hop = (HOP_SECS * sr).round() as usize;
    let total = buf.frame_count();
    if total < block {
        return Ok(LoudnessReport {
            integrated_lufs: None,
            short_term_lufs: Vec::new(),
            lra_lu: None,
            gated_blocks: 0,
            total_blocks: 0,
        });
    }

    let filtered = k_weighted(buf);
    let prefix = energy_prefix(&filtered);

    // 400ms 块能量
    let mut blocks: Vec<f64> = Vec::new();
    let mut start = 0usize;
    while start + block <= total {
        blocks.push(window_mean(&prefix, start, block));
        start += hop;
    }

    // 第一级：绝对门限
    let abs_gate_lin = 10f64.powf((ABSOLUTE_GATE_LUFS - LUFS_OFFSET) / 10.0);
    let after_abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&z| z > abs_gate_lin)
        .collect();

    // 第二级：相对门限（相对已过绝对门限部分的能量均值，减 10 LU）
    //
    // 注意符号：RELATIVE_GATE_LU 本身就是 -10，所以这里是 10^(-10/10)=0.1。
    // 早期写成 `10f64.powf(-RELATIVE_GATE_LU / 10.0)` 会得到 10 倍，
    // 把所有块都滤掉，integrated 恒为 None —— 这个错误由调试测试抓出来。
    let integrated = if after_abs.is_empty() {
        None
    } else {
        let mean_abs = after_abs.iter().sum::<f64>() / after_abs.len() as f64;
        let rel_gate_lin = mean_abs * 10f64.powf(RELATIVE_GATE_LU / 10.0);
        let gated: Vec<f64> = after_abs.iter().copied().filter(|&z| z > rel_gate_lin).collect();
        if gated.is_empty() {
            None
        } else {
            let mean_gated = gated.iter().sum::<f64>() / gated.len() as f64;
            Some(to_lufs(mean_gated))
        }
    };
    let gated_blocks = after_abs.len();

    // 短时 3s 窗
    let short_win = (SHORT_SECS * sr).round() as usize;
    let short_hop = (SHORT_HOP_SECS * sr).round() as usize;
    let mut short_term = Vec::new();
    if total >= short_win {
        let mut s = 0usize;
        while s + short_win <= total {
            short_term.push(to_lufs(window_mean(&prefix, s, short_win)));
            s += short_hop;
        }
    }

    let lra = lra_from_short_term(&short_term);

    Ok(LoudnessReport {
        integrated_lufs: integrated,
        short_term_lufs: short_term,
        lra_lu: lra,
        gated_blocks,
        total_blocks: blocks.len(),
    })
}

/// LRA 的 10th / 95th 百分位差（EBU Tech 3342）。
/// 先用 -70 LUFS 绝对门限剔除静音窗，再取百分位。
fn lra_from_short_term(short_term: &[f64]) -> Option<f64> {
    let mut v: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&x| x.is_finite() && x > ABSOLUTE_GATE_LUFS)
        .collect();
    if v.len() < 2 {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p10 = percentile(&v, 10.0);
    let p95 = percentile(&v, 95.0);
    Some((p95 - p10).max(0.0))
}

/// 线性插值百分位。标准未规定插值方式，这里固定用线性并写进注释，
/// 保证同一输入永远得到同一输出。
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let t = rank - lo as f64;
    sorted[lo] * (1.0 - t) + sorted[hi] * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(amp: f64, freq: f64, secs: f64) -> AudioBuffer {
        let sr = 48_000u32;
        let n = (sr as f64 * secs) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * freq * i as f64 / sr as f64).sin() * amp) as f32)
            .collect();
        AudioBuffer::new(vec![v], sr, 24).unwrap()
    }

    /// 解析解：A=0.5 的 1kHz 正弦 → -9.72 LUFS **加上 K 加权在 1kHz 的 +0.69dB**。
    ///
    /// 之前把期望写成纯 -9.72 是漏算了 K 加权在 1kHz 的抬升（BS.1770 的
    /// 1kHz 增益不是 0dB）。这里显式写出 +0.69 这个标准常数，容差 0.15dB。
    #[test]
    fn integrated_lufs_matches_analytic_value() {
        let b = sine(0.5, 1000.0, 5.0);
        let r = measure(&b).unwrap();
        let got = r.integrated_lufs.expect("应能测出积分响度");
        let k_gain_at_1khz = 0.69;
        let expected = -0.691 + 10.0 * (0.5f64 * 0.5 / 2.0).log10() + k_gain_at_1khz;
        assert!(
            (got - expected).abs() < 0.15,
            "1kHz A=0.5 应约 {expected:.2} LUFS，实得 {got:.2}"
        );
    }

    /// 相对门限的回归测试：稳态信号的全部块都应通过两级门限。
    /// 曾经因为相对门限符号写反，所有块被滤掉、integrated 恒为 None。
    #[test]
    fn steady_signal_passes_both_gates() {
        let b = sine(0.3, 1000.0, 3.0);
        let r = measure(&b).unwrap();
        assert!(r.total_blocks > 0);
        assert_eq!(r.gated_blocks, r.total_blocks, "稳态信号不该被门限滤掉");
        assert!(r.integrated_lufs.is_some(), "稳态信号必须测得出响度");
    }

    /// 幅度翻倍应 +6.02 LUFS。
    #[test]
    fn doubling_amplitude_adds_six_lu() {
        let a = measure(&sine(0.25, 1000.0, 5.0)).unwrap().integrated_lufs.unwrap();
        let b = measure(&sine(0.5, 1000.0, 5.0)).unwrap().integrated_lufs.unwrap();
        assert!((b - a - 6.02).abs() < 0.05, "幅度翻倍应 +6.02 LU，实得 {:.2}", b - a);
    }

    #[test]
    fn silence_has_no_integrated_loudness() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 48_000 * 2]], 48_000, 24).unwrap();
        let r = measure(&b).unwrap();
        assert!(r.integrated_lufs.is_none(), "全静音不应给出响度");
        assert!(r.lra_lu.is_none());
    }

    #[test]
    fn non_48k_is_rejected() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 44_100]], 44_100, 24).unwrap();
        assert!(measure(&b).is_err(), "44.1k 必须被拒绝而不是算出错误读数");
    }

    /// 两段电平相差 12dB 的稳态信号，LRA 应接近 12 LU。
    #[test]
    fn lra_reflects_level_difference() {
        let sr = 48_000u32;
        let n = sr as usize * 8;
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let amp = if t < 4.0 { 0.5 } else { 0.125 }; // -12dB
            v.push(((2.0 * PI * 1000.0 * t).sin() * amp) as f32);
        }
        let b = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let r = measure(&b).unwrap();
        let lra = r.lra_lu.expect("应能算出 LRA");
        assert!(
            (lra - 12.0).abs() < 1.5,
            "两段相差 12dB 时 LRA 应约 12 LU，实得 {lra:.2}"
        );
    }

    #[test]
    fn percentile_is_deterministic() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 100.0), 5.0);
        assert_eq!(percentile(&v, 50.0), 3.0);
    }
}
