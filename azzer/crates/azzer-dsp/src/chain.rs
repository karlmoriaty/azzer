//! 母带链第 1–5 步（v3 §5.6，顺序不可换）。
//!
//! | 序 | 处理 | 参数 |
//! |---|---|---|
//! | 1 | 低频单声道化 | <120Hz 合并，24dB/oct |
//! | 2 | 动态 EQ 去浑浊 | 200–400Hz，最多 -2dB |
//! | 3 | 多段压缩 | 低 30/300ms 2:1；中 10/120ms 2.5:1；高 3/80ms 2:1 |
//! | 4 | 谐波饱和 | 3dB，混入 30% |
//! | 5 | 立体声增强 | side +1.5dB，<200Hz 不加宽 |
//!
//! 第 6（真峰值限幅）、7（两遍归一）步在 `master.rs`，由上层串起来。
//!
//! ## 明确的实现取舍
//!
//! - **压缩器用峰值包络而不是 RMS**：标准实现会先平方、低通、再开方。
//!   这里用峰值检测 + attack/release 平滑，行为相近但阈值手感不同，
//!   不影响"不把动态压平"这条红线（v3 §8 把 LRA 设为硬门禁）。
//! - **动态 EQ 用分频 + 动态增益**，不是逐样本改 biquad 系数：
//!   后者需要每帧重算系数，容易引入系数跳变导致的咔嗒声。
//! - **所有处理都要求 48kHz**：LR4 系数按 48k 设计。

use crate::biquad::{ComplementarySplit, LinkwitzRiley4};
use crate::buffer::{AudioBuffer, DspError};

#[derive(Debug, Clone, Copy)]
pub struct CompParams {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ChainConfig {
    pub low_mono_hz: f64,
    pub mud_lo_hz: f64,
    pub mud_hi_hz: f64,
    pub mud_cut_db: f64,
    pub comp_low: CompParams,
    pub comp_mid: CompParams,
    pub comp_high: CompParams,
    /// 低/中、中/高的分频点。v3 写的是低 60–120Hz、中 1–4k、高 8k+，
    /// 这里取 150Hz / 6500Hz 两个边界覆盖三段。
    pub split_low_hz: f64,
    pub split_high_hz: f64,
    pub sat_drive_db: f64,
    pub sat_mix: f64,
    pub width_side_db: f64,
    pub width_hz: f64,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            low_mono_hz: 120.0,
            mud_lo_hz: 200.0,
            mud_hi_hz: 400.0,
            mud_cut_db: -2.0,
            comp_low: CompParams { threshold_db: -18.0, ratio: 2.0, attack_ms: 30.0, release_ms: 300.0 },
            comp_mid: CompParams { threshold_db: -20.0, ratio: 2.5, attack_ms: 10.0, release_ms: 120.0 },
            comp_high: CompParams { threshold_db: -22.0, ratio: 2.0, attack_ms: 3.0, release_ms: 80.0 },
            split_low_hz: 150.0,
            split_high_hz: 6500.0,
            sat_drive_db: 3.0,
            sat_mix: 0.30,
            width_side_db: 1.5,
            width_hz: 200.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainReport {
    pub mono_lows_applied: bool,
    pub dynamic_eq_applied: bool,
    pub multiband_applied: bool,
    pub saturation_applied: bool,
    pub width_applied: bool,
    /// 各步之后的最大绝对幅度，用于发现某一步意外放大。
    pub peak_after_each: Vec<f64>,
}

// ---------------------------------------------------------------- 分频工具

fn require_48k(buf: &AudioBuffer) -> Result<f64, DspError> {
    if buf.sample_rate != 48_000 {
        return Err(DspError::Invalid("母带前五步只支持 48kHz；请先用 N01 归一"));
    }
    Ok(buf.sample_rate as f64)
}

/// 独立双路径 LR4 分频：适合「替换其中一段」。
fn split(buf: &AudioBuffer, hz: f64) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let sr = buf.sample_rate as f64;
    buf.channels
        .iter()
        .map(|ch| {
            let mut lr = LinkwitzRiley4::new(sr, hz);
            let mut lo = Vec::with_capacity(ch.len());
            let mut hi = Vec::with_capacity(ch.len());
            for &x in ch {
                let (l, h) = lr.process(x);
                lo.push(l);
                hi.push(h);
            }
            (lo, hi)
        })
        .unzip()
}

/// 互补式分割：低通 + (x - 低通)，相加精确。
/// 适合「只改其中一段、另一段必须原样」。
fn split_complementary(buf: &AudioBuffer, hz: f64) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let sr = buf.sample_rate as f64;
    buf.channels
        .iter()
        .map(|ch| {
            let mut cs = ComplementarySplit::new(sr, hz);
            let mut lo = Vec::with_capacity(ch.len());
            let mut hi = Vec::with_capacity(ch.len());
            for &x in ch {
                let (l, h) = cs.process(x);
                lo.push(l);
                hi.push(h);
            }
            (lo, hi)
        })
        .unzip()
}

/// 只取低通分量（互补式分割的低半部分）。
fn low_pass_only(buf: &AudioBuffer, hz: f64) -> Vec<Vec<f32>> {
    split_complementary(buf, hz).0
}

fn sub(a: &[Vec<f32>], b: &[Vec<f32>]) -> Vec<Vec<f32>> {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| p - q).collect())
        .collect()
}

fn from_channels(channels: Vec<Vec<f32>>, template: &AudioBuffer) -> AudioBuffer {
    AudioBuffer {
        channels,
        sample_rate: template.sample_rate,
        source_bits: template.source_bits,
    }
}

fn add(a: &[Vec<f32>], b: &[Vec<f32>]) -> Vec<Vec<f32>> {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| p + q).collect())
        .collect()
}

// ---------------------------------------------------------------- 第 1 步

/// 低频单声道化：把 <120Hz 的左右声道合并，高频保持原样。
/// 单声道输入原样返回（零损失）。
fn mono_lows(buf: &AudioBuffer, hz: f64) -> AudioBuffer {
    if buf.channel_count() < 2 {
        return buf.clone();
    }
    let (lo, hi) = split(buf, hz);
    let n = lo[0].len();
    let mut merged = vec![0.0f32; n];
    for i in 0..n {
        let mut s = 0.0f32;
        for ch in &lo {
            s += ch[i];
        }
        merged[i] = s / lo.len() as f32;
    }
    let lows: Vec<Vec<f32>> = (0..buf.channel_count()).map(|_| merged.clone()).collect();
    from_channels(add(&lows, &hi), buf)
}

// ---------------------------------------------------------------- 第 2 步

/// 动态 EQ 去浑浊：提取 200–400Hz 带，按该带的包络动态衰减，最多 -2dB。
///
/// 不是「静态 EQ 一刀切」（v3 明确列为常见错误）：浑浊不突出时几乎不动，
/// 该带能量高时才逐步加大衰减。
fn dynamic_eq_mud(buf: &AudioBuffer, lo_hz: f64, hi_hz: f64, max_cut_db: f64) -> AudioBuffer {
    // 带通 = LP@hi - LP@lo。两个低通同相，差是干净的 200–400Hz 带；
    // 其余部分用 x - band 精确扣除，避免分频相位失真污染非目标频段。
    let lp_hi = low_pass_only(buf, hi_hz);
    let lp_lo = low_pass_only(buf, lo_hz);
    let band = sub(&lp_hi, &lp_lo);
    let rest = subtract(buf, &band);

    let sr = buf.sample_rate as f64;
    let win = (0.010 * sr).round().max(1.0) as usize; // 10ms 检测窗
    let attack = (-1.0 / (0.010 * sr)).exp();
    let release = (-1.0 / (0.100 * sr)).exp();

    let n = buf.frame_count();
    let mut env = 0.0f64;
    let mut gains = vec![1.0f64; n];
    // 用带通信号的绝对值和做包络（简化检测，见模块注释）
    let mut band_abs = vec![0.0f64; n];
    for ch in &band {
        for i in 0..n {
            band_abs[i] += (ch[i] as f64).abs();
        }
    }
    let mut running = 0.0f64;
    let mut ring = vec![0.0f64; win];
    let mut idx = 0usize;
    let mut filled = 0usize;
    for i in 0..n {
        let old = ring[idx];
        ring[idx] = band_abs[i];
        running += band_abs[i] - old;
        idx = (idx + 1) % win;
        filled = (filled + 1).min(win);
        let mean = running / filled.max(1) as f64;
        let coeff = if mean > env { attack } else { release };
        env = coeff * env + (1.0 - coeff) * mean;
        // 阈值 0.02（线性），超出部分线性映射到最多 -2dB
        let amount = ((env - 0.02) / 0.08).clamp(0.0, 1.0);
        gains[i] = 10f64.powf(max_cut_db * amount / 20.0);
    }

    let channels = rest
        .channels
        .iter()
        .zip(band.iter())
        .map(|(r, b)| {
            r.iter()
                .enumerate()
                .zip(b.iter())
                .map(|((i, &x), &y)| (x as f64 + y as f64 * gains[i]) as f32)
                .collect()
        })
        .collect();
    from_channels(channels, buf)
}

fn subtract(buf: &AudioBuffer, b: &[Vec<f32>]) -> AudioBuffer {
    let channels = buf
        .channels
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| p - q).collect())
        .collect();
    from_channels(channels, buf)
}

// ---------------------------------------------------------------- 第 3 步

/// 峰值检波压缩器。
///
/// 结构分三层，缺一层就会出问题（三层都实测踩过）：
/// 1. **峰值检波**：`|x|` 超过包络就立即抓住，否则按 `release_env` 指数下降。
///    不能对电平做一阶平滑——正弦每个周期只有峰值附近几个样本高于包络，
///    平滑上升时 attack 根本爬不上去，等于不压缩。
/// 2. **目标增益**：按阈值与比率算。
/// 3. **增益平滑**：attack 决定增益下降多快、release 决定回升多快。
///    平滑作用在**增益**上而不是电平上，这样 attack_ms 才有实际意义。
struct Compressor {
    threshold_db: f64,
    ratio: f64,
    /// 增益下降的平滑系数（越小越慢）。
    attack: f64,
    /// 增益回升的平滑系数。
    release: f64,
    /// 峰值包络的释放系数。
    release_env: f64,
    env_lin: f64,
    gain_db: f64,
}

impl Compressor {
    fn new(p: &CompParams, sr: f64) -> Self {
        let thresh_lin = 10f64.powf(p.threshold_db / 20.0);
        Self {
            threshold_db: p.threshold_db,
            ratio: p.ratio.max(1.0),
            attack: (-1.0 / (p.attack_ms / 1000.0 * sr)).exp(),
            release: (-1.0 / (p.release_ms / 1000.0 * sr)).exp(),
            // 包络释放比增益释放再慢一点，避免增益抖动
            release_env: (-1.0 / (p.release_ms / 1000.0 * sr * 1.5)).exp(),
            env_lin: thresh_lin,
            gain_db: 0.0,
        }
    }

    /// 只算增益，不直接改样本。
    ///
    /// 拆出 `gain_for` 是为了**声道联动**：立体声必须用同一帧所有声道里
    /// 最大的那个电平去算增益，左右声道同乘一个增益，声像才不会漂移。
    /// 早期版本每个声道各建一个压缩器，左声道被压 3dB、右声道没压，
    /// 声音就会往右跑——这是立体声压缩的典型错误。
    #[inline]
    fn gain_for(&mut self, level_lin: f32) -> f32 {
        let x = (level_lin as f64).abs();

        // ① 峰值检波：上升立即，下降指数
        if x > self.env_lin {
            self.env_lin = x;
        } else {
            self.env_lin *= self.release_env;
        }

        // ② 目标增益
        let level_db = 20.0 * self.env_lin.max(f64::MIN_POSITIVE).log10();
        let over = (level_db - self.threshold_db).max(0.0);
        let target_db = -(1.0 - 1.0 / self.ratio) * over;

        // ③ 增益平滑：压下去要快，放开要慢
        let coeff = if target_db < self.gain_db { self.attack } else { self.release };
        self.gain_db = coeff * self.gain_db + (1.0 - coeff) * target_db;

        10f64.powf(self.gain_db / 20.0) as f32
    }
}

/// 对一个频段做声道联动压缩。
fn compress_band(band: &[Vec<f32>], p: &CompParams, sr: f64) -> Vec<Vec<f32>> {
    let n = band.first().map(|c| c.len()).unwrap_or(0);
    let mut comp = Compressor::new(p, sr);

    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(n); band.len()];
    for i in 0..n {
        let mut peak = 0.0f32;
        for ch in band {
            peak = peak.max(ch[i].abs());
        }
        let g = comp.gain_for(peak);
        for (c, ch) in band.iter().enumerate() {
            out[c].push(ch[i] * g);
        }
    }
    out
}

/// 多段压缩：低/中/高三段分别压缩后再相加。每段内所有声道共用一条增益曲线。
fn multiband_compress(buf: &AudioBuffer, cfg: &ChainConfig) -> AudioBuffer {
    let sr = buf.sample_rate as f64;
    let (low, rest) = split_complementary(buf, cfg.split_low_hz);
    let rest_buf = from_channels(rest.clone(), buf);
    let (mid, high) = split_complementary(&rest_buf, cfg.split_high_hz);

    let c_low = compress_band(&low, &cfg.comp_low, sr);
    let c_mid = compress_band(&mid, &cfg.comp_mid, sr);
    let c_high = compress_band(&high, &cfg.comp_high, sr);
    let mut out = add(&c_low, &c_mid);
    out = add(&out, &c_high);
    from_channels(out, buf)
}

// ---------------------------------------------------------------- 第 4 步

/// 谐波饱和：tanh 软削波，按 mix 混入原信号。
/// `tanh(drive*x)/drive` 保证小信号增益为 1（不会出现意外放大）。
fn saturate(buf: &AudioBuffer, drive_db: f64, mix: f64) -> AudioBuffer {
    let drive = 10f64.powf(drive_db / 20.0);
    let m = mix.clamp(0.0, 0.30); // v3 硬约束：混入 ≤30%
    let channels = buf
        .channels
        .iter()
        .map(|ch| {
            ch.iter()
                .map(|&x| {
                    let shaped = (drive * x as f64).tanh() / drive;
                    ((1.0 - m) * x as f64 + m * shaped) as f32
                })
                .collect()
        })
        .collect();
    from_channels(channels, buf)
}

// ---------------------------------------------------------------- 第 5 步

/// 立体声增强：M/S 变换后只给 side 的 >200Hz 部分 +1.5dB，低频不加宽。
/// 单声道输入原样返回。
fn stereo_width(buf: &AudioBuffer, hz: f64, side_db: f64) -> AudioBuffer {
    if buf.channel_count() < 2 {
        return buf.clone();
    }
    let n = buf.frame_count();
    let l = &buf.channels[0];
    let r = &buf.channels[1];
    let mut mid = Vec::with_capacity(n);
    let mut side = Vec::with_capacity(n);
    for i in 0..n {
        mid.push((l[i] + r[i]) * 0.5);
        side.push((l[i] - r[i]) * 0.5);
    }

    // 用互补式分割，保证 side_lo + side_hi 精确等于 side
    let mut lr = ComplementarySplit::new(buf.sample_rate as f64, hz);
    let mut side_lo = Vec::with_capacity(n);
    let mut side_hi = Vec::with_capacity(n);
    for &s in &side {
        let (a, b) = lr.process(s);
        side_lo.push(a);
        side_hi.push(b);
    }
    let g = 10f64.powf(side_db / 20.0);
    let side_new: Vec<f32> = (0..n)
        .map(|i| side_lo[i] + (side_hi[i] as f64 * g) as f32)
        .collect();

    let mut out_l = Vec::with_capacity(n);
    let mut out_r = Vec::with_capacity(n);
    for i in 0..n {
        out_l.push(mid[i] + side_new[i]);
        out_r.push(mid[i] - side_new[i]);
    }
    let mut channels = vec![out_l, out_r];
    channels.extend(buf.channels.iter().skip(2).cloned());
    from_channels(channels, buf)
}

// ---------------------------------------------------------------- 串联

pub fn apply_front_five(
    buf: &AudioBuffer,
    cfg: &ChainConfig,
) -> Result<(AudioBuffer, ChainReport), DspError> {
    require_48k(buf)?;
    let mut x = buf.clone();
    let mut peaks = Vec::with_capacity(5);

    x = mono_lows(&x, cfg.low_mono_hz);
    peaks.push(peak(&x));
    x = dynamic_eq_mud(&x, cfg.mud_lo_hz, cfg.mud_hi_hz, cfg.mud_cut_db);
    peaks.push(peak(&x));
    x = multiband_compress(&x, cfg);
    peaks.push(peak(&x));
    x = saturate(&x, cfg.sat_drive_db, cfg.sat_mix);
    peaks.push(peak(&x));
    x = stereo_width(&x, cfg.width_hz, cfg.width_side_db);
    peaks.push(peak(&x));

    Ok((
        x,
        ChainReport {
            mono_lows_applied: buf.channel_count() >= 2,
            dynamic_eq_applied: true,
            multiband_applied: true,
            saturation_applied: true,
            width_applied: buf.channel_count() >= 2,
            peak_after_each: peaks,
        },
    ))
}

fn peak(buf: &AudioBuffer) -> f64 {
    buf.channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |a, &b| a.max(b.abs())) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(ch: usize, amp: f64, freq: f64, secs: f64) -> AudioBuffer {
        let sr = 48_000u32;
        let n = (sr as f64 * secs) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * freq * i as f64 / sr as f64).sin() * amp) as f32)
            .collect();
        AudioBuffer::new(vec![v; ch], sr, 24).unwrap()
    }

    #[test]
    fn mono_lows_merges_only_low_band() {
        let sr = 48_000u32;
        let n = 48_000usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        for i in 0..n {
            let t = i as f64 / sr as f64;
            // 低频：左右反相；高频：左右同相
            l[i] = ((2.0 * PI * 60.0 * t).sin() * 0.4
                + (2.0 * PI * 4000.0 * t).sin() * 0.2) as f32;
            r[i] = (-(2.0 * PI * 60.0 * t).sin() * 0.4
                + (2.0 * PI * 4000.0 * t).sin() * 0.2) as f32;
        }
        let b = AudioBuffer::new(vec![l, r], sr, 24).unwrap();
        let out = mono_lows(&b, 120.0);
        // 只能测低频：4kHz 同相分量本来就该原样保留（能量约 0.02），
        // 把整段都算进去会误判成"低频没合并掉"。
        let out_low = low_pass_only(&out, 120.0);
        let low_energy: f64 = (0..4800)
            .map(|i| (out_low[0][i] as f64).powi(2))
            .sum::<f64>()
            / 4800.0;
        assert!(low_energy < 0.005, "低频反相分量应被合并掉，实得能量 {low_energy:.5}");
        // 高频必须基本保留
        let out_high = sub(
            &out.channels.iter().map(|c| c.to_vec()).collect::<Vec<_>>(),
            &out_low,
        );
        let high_energy: f64 = (0..4800)
            .map(|i| (out_high[0][i] as f64).powi(2))
            .sum::<f64>()
            / 4800.0;
        assert!(high_energy > 0.01, "高频同相分量应保留，实得能量 {high_energy:.5}");
    }

    #[test]
    fn mono_input_is_untouched_by_width_and_lows() {
        let b = sine(1, 0.5, 440.0, 0.5);
        assert_eq!(mono_lows(&b, 120.0), b);
        assert_eq!(stereo_width(&b, 200.0, 1.5), b);
    }

    #[test]
    fn saturation_does_not_amplify_small_signals() {
        let b = sine(1, 0.01, 440.0, 0.2);
        let out = saturate(&b, 3.0, 0.30);
        let p = peak(&out);
        assert!(p <= 0.0101, "小信号不应被放大，实得峰值 {p}");
    }

    #[test]
    fn saturation_mix_is_capped_at_30_percent() {
        let b = sine(1, 0.9, 440.0, 0.2);
        let a = saturate(&b, 3.0, 0.90);
        let c = saturate(&b, 3.0, 0.30);
        assert_eq!(a, c, "混入比例必须被钳制到 30%");
    }

    #[test]
    fn front_five_preserves_length_and_finite_values() {
        let b = sine(2, 0.4, 440.0, 1.0);
        let (out, report) = apply_front_five(&b, &ChainConfig::default()).unwrap();
        assert_eq!(out.frame_count(), b.frame_count());
        assert_eq!(out.channel_count(), 2);
        assert!(out.channels.iter().flat_map(|c| c.iter()).all(|x| x.is_finite()));
        assert_eq!(report.peak_after_each.len(), 5);
        assert!(report.peak_after_each.iter().all(|p| p.is_finite() && *p < 2.0));
    }

    #[test]
    fn front_five_rejects_non_48k() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 1000]], 44_100, 24).unwrap();
        assert!(apply_front_five(&b, &ChainConfig::default()).is_err());
    }

    /// 立体声压缩必须声道联动：左右用同一个增益，电平比例保持不变。
    /// 早期版本每声道各建一个压缩器，左被压右没压，声像会右移。
    #[test]
    fn band_compressor_is_channel_linked() {
        let sr = 48_000.0;
        let n = 48_000usize;
        let l: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * 440.0 * i as f64 / sr).sin() * 0.6) as f32)
            .collect();
        let r: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * 440.0 * i as f64 / sr).sin() * 0.15) as f32)
            .collect();
        let p = CompParams {
            threshold_db: -30.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 50.0,
        };
        let out = compress_band(&[l.clone(), r.clone()], &p, sr);
        let mut checked = 0usize;
        for i in 10_000..n {
            if l[i].abs() > 0.2 {
                let ratio_out = out[1][i] as f64 / out[0][i] as f64;
                let ratio_in = r[i] as f64 / l[i] as f64;
                assert!(
                    (ratio_out - ratio_in).abs() < 1e-4,
                    "i={i} 声道比例被改变：in={ratio_in} out={ratio_out}"
                );
                checked += 1;
            }
        }
        assert!(checked > 1000, "样本数不足，测试没真正生效");
    }

    /// 压缩确实降低电平。用 0.1ms 的快速 attack：
    /// 1ms attack 对 440Hz（周期 2.3ms）压不住第一个峰值，那是**正常行为**，
    /// 不是缺陷——见 `slow_attack_lets_initial_transient_through`。
    #[test]
    fn band_compressor_actually_reduces_level() {
        let sr = 48_000.0;
        let n = 48_000usize;
        let x: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * 440.0 * i as f64 / sr).sin() * 0.8) as f32)
            .collect();
        let p = CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 0.1,
            release_ms: 20.0,
        };
        let out = compress_band(&[x.clone()], &p, sr);
        let peak_in = x.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        let peak_out = out[0].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        // 阈值 -20dB、ratio 4:1、输入峰值 -1.9dB：
        // over = 18.1dB，理论增益 -(1-1/4)*18.1 = -13.6dB → 输出约 0.17
        assert!(
            peak_out < peak_in * 0.5,
            "峰值应从 {peak_in} 压到约 0.17，实得 {peak_out}"
        );
    }

    /// 慢 attack 放过初始瞬态是压缩器的固有行为，把它写成测试固定下来，
    /// 避免以后有人看到"峰值没降"就误以为压缩坏了。
    #[test]
    fn slow_attack_lets_initial_transient_through() {
        let sr = 48_000.0;
        let n = 48_000usize;
        let x: Vec<f32> = (0..n)
            .map(|i| ((2.0 * PI * 440.0 * i as f64 / sr).sin() * 0.8) as f32)
            .collect();
        let p = CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 20.0, // 远比周期慢
            release_ms: 100.0,
        };
        let out = compress_band(&[x.clone()], &p, sr);
        let peak_in = x.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        let peak_out = out[0].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            peak_out > peak_in * 0.5,
            "慢 attack 本就该放过首个瞬态，实得 {peak_out}"
        );
        // 但稳态部分仍应被压下去
        let tail_in: f32 = x[n / 2..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        let tail_out: f32 = out[0][n / 2..].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(tail_out < tail_in * 0.5, "稳态应从 {tail_in} 压到一半以下，实得 {tail_out}");
    }

    /// N14 契约「mono 折叠后人声衰减 ≤3dB」针对的是**混音阶段的空间处理**，
    /// 不是整条母带链前后的电平差（压缩本来就会改变电平）。
    /// 所以这里单独验证第 5 步：同相立体声的 side=0，加宽不该动到它。
    #[test]
    fn width_preserves_mono_compatibility() {
        let b = sine(2, 0.4, 1000.0, 1.0);
        let out = stereo_width(&b, 200.0, 1.5);
        let max_diff = out.channels[0]
            .iter()
            .zip(b.channels[0].iter())
            .map(|(a, c)| (a - c).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1e-6, "同相输入的 side 为 0，加宽不应改变它，实得 {max_diff}");
    }
}
