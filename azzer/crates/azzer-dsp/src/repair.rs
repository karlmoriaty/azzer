//! N09 修复的前三步（v3 §5.3，顺序不可换）。
//!
//! | 序 | 处理 | 条件 |
//! |---|---|---|
//! | ① | 去伪影（谱减） | 总是执行，强度可配 |
//! | ② | 去混响 | **仅当 RT60 >0.35s**，强度上限 -30% |
//! | ③ | 高频重建（8–16kHz 谐波激励） | 总是执行 |
//!
//! 第 ④ 步（formant 校正）只在移调时需要，A 入口不移调，跳过；
//! 第 ⑤ 步（底噪注入）按 v3 要求在**混音前**，不在这里做。
//!
//! ## 关于契约断言「重建后 8–16kHz 能量比 ≥0.6」
//!
//! v3 原文没有定义分母。这里具体化为：
//!
//! ```text
//! hf_energy_ratio = 8–16kHz 能量 / 4–8kHz 能量
//! ```
//!
//! 理由是这是判断「高频空洞」最直接的量：比值过低说明 8kHz 以上没有内容，
//! 靠重建也补不回来，属于源不可用（v3 §8 的 R8 风险）。
//!
//! **达标与否如实报告，不为了过线硬注入**。硬凑会造出塑料感，这正是 v3
//! 警告的「高频重建编出假谐波」。

use rustfft::num_complex::Complex;

use crate::buffer::{AudioBuffer, DspError};
use crate::stft::{Stft, StftConfig};

#[derive(Debug, Clone, Copy)]
pub struct RepairConfig {
    /// 谱减过减因子。1.0 表示刚好减掉估计噪声底。
    pub denoise_over_subtraction: f64,
    /// 谱减后保留的最小幅度比，避免把信号削成金属声。
    pub denoise_floor: f64,
    /// 去混响强度，v3 上限 0.30。
    pub de_reverb_strength: f64,
    /// 触发去混响的 RT60 阈值。
    pub de_reverb_rt60_gate_s: f64,
    pub hf_lo_hz: f64,
    pub hf_hi_hz: f64,
    /// 谐波来源频段。
    pub hf_source_lo_hz: f64,
    pub hf_source_hi_hz: f64,
    /// 重建注入的增益。v3 参数表写 -30dB 起。
    pub hf_gain_db: f64,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            denoise_over_subtraction: 1.2,
            denoise_floor: 0.05,
            de_reverb_strength: 0.30,
            de_reverb_rt60_gate_s: 0.35,
            hf_lo_hz: 8000.0,
            hf_hi_hz: 16000.0,
            hf_source_lo_hz: 4000.0,
            hf_source_hi_hz: 8000.0,
            // -12dB 而不是文档的 -30dB：-30dB 在实测里几乎听不出变化，
            // 而 v3 的 -30dB 更像是"失真上限"而不是"注入量"。
            // 这里按"能补上但不过亮"取值，并如实记录在报告里。
            //
            // 临时二分用：设成 -200 相当于关闭注入，用来确认峰值异常的来源。
            hf_gain_db: -12.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepairReport {
    pub denoise_applied: bool,
    pub de_reverb_applied: bool,
    pub hf_rebuild_applied: bool,
    /// 触发去混响判断用的 RT60。
    pub rt60_s: Option<f64>,
    /// 重建前 8–16kHz / 4–8kHz 能量比。
    pub hf_ratio_before: f64,
    /// 重建后同一比值。
    pub hf_ratio_after: f64,
    /// 重建后比值 ≥ 0.6。
    pub meets_hf_ratio: bool,
    /// 重建让 8–16kHz 能量提升了多少 dB。源本来就没有高频时为 None。
    pub hf_gain_db: Option<f64>,
}

/// 频段能量（所有声道合计）。
fn band_energy(specs: &[Vec<f64>], bin_hz: f64, lo: f64, hi: f64) -> f64 {
    let lo_k = (lo / bin_hz).floor().max(0.0) as usize;
    let hi_k = ((hi / bin_hz).ceil() as usize).min(specs.first().map(|s| s.len()).unwrap_or(0));
    let mut e = 0.0;
    for s in specs {
        for k in lo_k..hi_k.min(s.len()) {
            e += s[k] * s[k];
        }
    }
    e
}

/// 逐声道求幅度谱的逐 bin 最小值，作为噪声底估计。
fn noise_floor(stft: &Stft, x: &[f32]) -> Vec<f64> {
    let specs = stft.magnitude_spectrum(x);
    let bins = stft.bins();
    let mut floor = vec![f64::INFINITY; bins];
    for s in &specs {
        for k in 0..bins.min(s.len()) {
            if s[k] < floor[k] {
                floor[k] = s[k];
            }
        }
    }
    floor
        .into_iter()
        .map(|v| if v.is_finite() { v } else { 0.0 })
        .collect()
}

/// 对一个声道做三步修复。
fn repair_channel(
    stft: &Stft,
    x: &[f32],
    cfg: &RepairConfig,
    do_de_reverb: bool,
) -> (Vec<f32>, f64) {
    let bins = stft.bins();
    let bin_hz = 48_000.0 / stft.cfg.frame as f64;
    let floor = noise_floor(stft, x);
    let alpha = cfg.denoise_over_subtraction.max(0.0);
    let keep = cfg.denoise_floor.clamp(0.0, 1.0);
    let hf_gain = 10f64.powf(cfg.hf_gain_db / 20.0);

    // 去混响用「上一帧幅度」的指数平滑作为混响估计
    let mut prev: Vec<f64> = vec![0.0; bins];
    let mut injected_energy = 0.0f64;

    let out = stft.process(x, |spec, _k| {
        // ① 谱减
        for k in 0..bins {
            let m = spec[k].norm();
            let target = (m - alpha * floor[k]).max(m * keep);
            if m > 1e-12 {
                let s = target / m;
                spec[k] = Complex::new(spec[k].re * s, spec[k].im * s);
            }
        }

        // ② 去混响：按强度比例压掉「上一帧的残留」
        if do_de_reverb {
            let beta = cfg.de_reverb_strength.clamp(0.0, 0.30);
            for k in 0..bins {
                let m = spec[k].norm();
                let est = prev[k] * beta;
                let target = (m - est).max(m * 0.5); // 最多压掉一半，避免过干
                if m > 1e-12 {
                    let s = target / m;
                    spec[k] = Complex::new(spec[k].re * s, spec[k].im * s);
                }
                prev[k] = 0.6 * prev[k] + 0.4 * target;
            }
        }

        // ③ 高频重建：把 4–8kHz 的谱整体倍频搬到 8–16kHz
        let src_lo = (cfg.hf_source_lo_hz / bin_hz).ceil() as usize;
        let src_hi = ((cfg.hf_source_hi_hz / bin_hz).floor() as usize).min(bins);
        let mut add: Vec<(usize, Complex<f64>)> = Vec::new();
        for k in src_lo..src_hi {
            let f = k as f64 * bin_hz;
            let target_hz = f * 2.0;
            if target_hz < cfg.hf_hi_hz {
                let tk = (target_hz / bin_hz).round() as usize;
                if tk < bins && bin_hz * tk as f64 >= cfg.hf_lo_hz {
                    add.push((tk, spec[k] * hf_gain));
                }
            }
        }
        for (tk, v) in add {
            injected_energy += v.norm_sqr();
            spec[tk] = spec[tk] + v;
        }
    });

    (out, injected_energy)
}

/// 三步修复。`rt60` 由上层提供（N02 或响度测量结果）。
pub fn repair(
    buf: &AudioBuffer,
    cfg: &RepairConfig,
    rt60: Option<f64>,
) -> Result<(AudioBuffer, RepairReport), DspError> {
    if buf.sample_rate != 48_000 {
        return Err(DspError::Invalid("N09 修复只支持 48kHz；请先用 N01 归一"));
    }
    let stft = Stft::new(StftConfig::default());
    let bin_hz = 48_000.0 / stft.cfg.frame as f64;

    let (hf_ratio_before, hf_energy_before) = {
        let specs: Vec<Vec<f64>> = buf
            .channels
            .iter()
            .flat_map(|ch| stft.magnitude_spectrum(ch))
            .collect();
        let hf = band_energy(&specs, bin_hz, cfg.hf_lo_hz, cfg.hf_hi_hz);
        let mid = band_energy(&specs, bin_hz, cfg.hf_source_lo_hz, cfg.hf_source_hi_hz);
        (if mid > 0.0 { hf / mid } else { 0.0 }, hf)
    };

    let do_de_reverb = rt60.map(|t| t > cfg.de_reverb_rt60_gate_s).unwrap_or(false);

    let mut channels = Vec::with_capacity(buf.channel_count());
    let mut total_injected = 0.0f64;
    for ch in &buf.channels {
        let (y, inj) = repair_channel(&stft, ch, cfg, do_de_reverb);
        total_injected += inj;
        channels.push(y);
    }
    let repaired = AudioBuffer::new(channels, buf.sample_rate, buf.source_bits)?;

    let (hf_ratio_after, hf_energy_after) = {
        let specs: Vec<Vec<f64>> = repaired
            .channels
            .iter()
            .flat_map(|ch| stft.magnitude_spectrum(ch))
            .collect();
        let hf = band_energy(&specs, bin_hz, cfg.hf_lo_hz, cfg.hf_hi_hz);
        let mid = band_energy(&specs, bin_hz, cfg.hf_source_lo_hz, cfg.hf_source_hi_hz);
        (if mid > 0.0 { hf / mid } else { 0.0 }, hf)
    };

    let hf_gain_db = if hf_energy_before > 0.0 && hf_energy_after > 0.0 {
        Some(10.0 * (hf_energy_after / hf_energy_before).log10())
    } else {
        None
    };
    let _ = total_injected;

    let report = RepairReport {
        denoise_applied: true,
        de_reverb_applied: do_de_reverb,
        hf_rebuild_applied: true,
        rt60_s: rt60,
        hf_ratio_before,
        hf_ratio_after,
        meets_hf_ratio: hf_ratio_after >= 0.6,
        hf_gain_db,
    };
    Ok((repaired, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone(amp: f64, freq: f64, secs: f64) -> AudioBuffer {
        let sr = 48_000.0f64;
        let n = (sr * secs) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * freq * i as f64 / sr).sin() * amp) as f32)
            .collect();
        AudioBuffer::new(vec![v], 48_000, 24).unwrap()
    }

    #[test]
    fn repair_preserves_length_and_finiteness() {
        let b = tone(0.5, 1000.0, 1.0);
        let (out, rep) = repair(&b, &RepairConfig::default(), Some(0.2)).unwrap();
        assert_eq!(out.frame_count(), b.frame_count());
        assert!(out.channels[0].iter().all(|x| x.is_finite()));
        assert!(rep.denoise_applied && rep.hf_rebuild_applied);
        assert!(!rep.de_reverb_applied, "RT60 0.2s 低于 0.35 阈值，不该去混响");
    }

    #[test]
    fn de_reverb_requires_high_rt60() {
        let b = tone(0.5, 1000.0, 0.5);
        let (_, rep) = repair(&b, &RepairConfig::default(), Some(0.5)).unwrap();
        assert!(rep.de_reverb_applied, "RT60 0.5s 应触发去混响");
        let (_, rep2) = repair(&b, &RepairConfig::default(), None).unwrap();
        assert!(!rep2.de_reverb_applied, "测不出 RT60 时不该去混响");
    }

    /// 高频重建必须真的往 8–16kHz 注入能量。
    #[test]
    fn hf_rebuild_adds_energy_above_8k() {
        // 只有 6kHz 的信号：8kHz 以上本该是空的
        let b = tone(0.5, 6000.0, 1.0);
        let (out, rep) = repair(&b, &RepairConfig::default(), Some(0.2)).unwrap();
        let sr = 48_000.0;
        let stft = Stft::new(StftConfig::default());
        let bin_hz = sr / stft.cfg.frame as f64;
        let e_before = band_energy(&stft.magnitude_spectrum(&b.channels[0]), bin_hz, 8000.0, 16000.0);
        let e_after = band_energy(&stft.magnitude_spectrum(&out.channels[0]), bin_hz, 8000.0, 16000.0);
        assert!(e_after > e_before * 2.0, "8–16kHz 能量应显著增加：{e_before:.3e} → {e_after:.3e}");
        assert!(rep.hf_ratio_after > rep.hf_ratio_before, "重建后比值应上升");
        assert!(rep.hf_gain_db.unwrap_or(0.0) > 3.0, "提升应 >3dB，实得 {:?}", rep.hf_gain_db);
    }

    /// 谱减不应把有内容的频段削没。
    #[test]
    fn denoise_keeps_a_loud_tone() {
        let b = tone(0.5, 1000.0, 0.5);
        let (out, _) = repair(&b, &RepairConfig::default(), Some(0.1)).unwrap();
        let peak = out.channels[0].iter().fold(0.0f32, |a, &c| a.max(c.abs()));
        assert!(peak > 0.2, "1kHz 主体应保留，实得峰值 {peak}");
    }

    /// 长信号上修复不能把幅度放大。端到端 240 秒曾出现峰值 700（正常应 <1）。
    #[test]
    fn repair_on_long_signal_stays_bounded() {
        let b = tone(0.5, 1000.0, 10.0);
        let (out, _) = repair(&b, &RepairConfig::default(), Some(0.1)).unwrap();
        let peak = out.channels[0].iter().fold(0.0f32, |a, &c| a.max(c.abs()));
        assert!(peak < 2.0, "10 秒修复后峰值应 <2，实得 {peak}");
        assert!(out.channels[0].iter().all(|v| v.is_finite()));
    }

    #[test]
    fn non_48k_is_rejected() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 4800]], 44_100, 24).unwrap();
        assert!(repair(&b, &RepairConfig::default(), None).is_err());
    }

    /// 契约判定的口径要能被测试固定：静音信号比值算 0，不谎报达标。
    #[test]
    fn silent_input_fails_hf_contract_honestly() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 48_000]], 48_000, 24).unwrap();
        let (_, rep) = repair(&b, &RepairConfig::default(), None).unwrap();
        assert_eq!(rep.hf_ratio_before, 0.0);
        assert!(!rep.meets_hf_ratio, "静音不该被判为达标");
    }
}
