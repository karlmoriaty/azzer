//! 感知层检测：起音、呼吸、颤音。
//!
//! 这三样补的是 HumanizeScore 的 H2 / H4 / H6。补之前六子项只有三项能算，
//! 加权无从谈起；补完才能给出真正的拟真度评分。
//!
//! ## 三个检测的口径与已知局限
//!
//! | 检测 | 方法 | 局限 |
//! |---|---|---|
//! | 起音 | 谱通量峰值 + 最小间隔 | 复调/强伴奏下会过检；A 入口未分离，这条影响真实存在 |
//! | 呼吸 | voicing=0 + 能量高于底噪 6dB + 频谱重心 2–6kHz | 清辅音（s/sh/f）与呼吸的频谱特征接近，会混判 |
//! | 颤音 | f0 去趋势后的 4–8Hz 周期性 | f0 提取本身不可靠时（未分离的全混音）结果不可信 |
//!
//! **H2 的口径说明**：v3 写的是 `in_range(std(onset_measured - onset_grid), 12, 35)`，
//! 需要一张节拍网格。A 入口拿不到乐谱/节拍，所以这里改用**相邻起音间隔的标准差**
//! （self-referential jitter）：真人演唱的起音间隔本就有 10–40ms 的弹性，
//! 节拍器般的精确反而说明是机器。这个替代口径写在返回值里，不藏在实现里。

use crate::buffer::AudioBuffer;
use crate::f0::F0Track;
use crate::stft::{Stft, StftConfig};

#[derive(Debug, Clone, Copy)]
pub struct PerceptConfig {
    /// 起音检测：谱通量超过「局部均值 + k×标准差」才算峰。
    pub onset_k: f64,
    /// 两个起音之间的最小间隔（秒），防止同一个起音被切成多个。
    pub onset_min_gap_s: f64,
    /// 起音检测的局部统计窗（秒）。
    pub onset_local_win_s: f64,
    /// 呼吸：能量至少高于底噪多少 dB。
    pub breath_min_above_floor_db: f64,
    /// 呼吸：相对邻域峰值的能量凹陷深度（dB）。
    ///
    /// 为什么需要这个：原先只用「非浊音帧」当候选，这在自研 f0 上够用
    /// （浊音率 87%），换成 RMVPE 后浊音率涨到 96.8%，几乎不再有非浊音帧，
    /// 呼吸数从 17.97 次/分直接掉到 1.37。真人换气发生在音符之间——
    /// 能量比邻域低十几 dB，但仍明显高于底噪，靠能量凹陷识别才不依赖 voicing。
    pub breath_dip_db: f64,
    /// 凹陷判据的邻域半宽（帧数）。15 帧 × 10ms = 前后各 150ms。
    pub breath_dip_half_win: usize,
    /// 呼吸：频谱重心下限 / 上限（Hz）。
    pub breath_centroid_lo_hz: f64,
    pub breath_centroid_hi_hz: f64,
    /// 呼吸最短持续（秒）。
    pub breath_min_dur_s: f64,
    /// 颤音搜索范围（Hz）与最短持续（秒）。
    pub vibrato_lo_hz: f64,
    pub vibrato_hi_hz: f64,
    pub vibrato_min_dur_s: f64,
}

impl Default for PerceptConfig {
    fn default() -> Self {
        Self {
            onset_k: 1.2,
            onset_min_gap_s: 0.05,
            onset_local_win_s: 0.30,
            breath_min_above_floor_db: 6.0,
            breath_dip_db: 12.0,
            breath_dip_half_win: 15,
            breath_centroid_lo_hz: 2000.0,
            breath_centroid_hi_hz: 6000.0,
            breath_min_dur_s: 0.08,
            vibrato_lo_hz: 4.0,
            vibrato_hi_hz: 8.0,
            vibrato_min_dur_s: 0.30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OnsetInfo {
    /// 起音时刻（秒）。
    pub onsets_s: Vec<f64>,
    /// 相邻间隔的标准差（毫秒）。样本不足时为 None。
    pub jitter_ms: Option<f64>,
    /// 相邻间隔的均值（毫秒）。
    pub mean_ioi_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BreathInfo {
    /// 每次呼吸的起始时刻（秒）。
    pub starts_s: Vec<f64>,
    /// 每分钟呼吸次数。
    pub per_minute: f64,
    /// 判为呼吸的总时长占全曲比例。
    pub coverage: f64,
}

/// 颤音结果只有三个标量，标记 Copy 以便调用方多次读取而不必借用。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VibratoInfo {
    /// 调制速率（Hz）。
    pub rate_hz: f64,
    /// 调制深度（cents，峰峰值的一半）。
    pub depth_cents: f64,
    /// 参与计算的浊音时长（秒）。
    pub analysed_s: f64,
}

fn db(v: f64) -> f64 {
    20.0 * v.max(1e-12).log10()
}

/// 逐帧谱通量：相邻两帧幅度谱的正向增量之和。
fn spectral_flux(mono: &[f32], sr: u32) -> (Vec<f64>, f64) {
    let stft = Stft::new(StftConfig::default());
    let specs = stft.magnitude_spectrum(mono);
    let hop_s = stft.cfg.hop as f64 / sr as f64;
    let mut flux = Vec::with_capacity(specs.len());
    for i in 1..specs.len() {
        let mut s = 0.0;
        for k in 0..specs[i].len().min(specs[i - 1].len()) {
            let d = specs[i][k] - specs[i - 1][k];
            if d > 0.0 {
                s += d;
            }
        }
        flux.push(s);
    }
    (flux, hop_s)
}

/// 起音检测。
pub fn detect_onsets(buf: &AudioBuffer, cfg: &PerceptConfig) -> OnsetInfo {
    let mono = buf.mono();
    if mono.len() < 4096 {
        return OnsetInfo {
            onsets_s: Vec::new(),
            jitter_ms: None,
            mean_ioi_ms: None,
        };
    }
    let (flux, hop_s) = spectral_flux(&mono, buf.sample_rate);
    if flux.len() < 4 {
        return OnsetInfo {
            onsets_s: Vec::new(),
            jitter_ms: None,
            mean_ioi_ms: None,
        };
    }

    let win = ((cfg.onset_local_win_s / hop_s).round() as usize).max(3);
    // 绝对阈值：谱通量的全局最大值的固定比例。
    //
    // 只用「局部均值 + k×标准差」在稳态信号上会过检——稳态音的谱通量本来
    // 就在 0 附近抖动，微小的数值波动也能超过均值加 1.2σ，于是一段长音被判成
    // 十几个起音（实测稳态正弦检出 3 个以上）。
    let global_max = flux.iter().copied().fold(0.0f64, f64::max);
    let abs_floor = global_max * 0.08;

    let mut peaks: Vec<usize> = Vec::new();
    for i in 0..flux.len() {
        let lo = i.saturating_sub(win);
        let hi = (i + win).min(flux.len());
        let slice = &flux[lo..hi];
        let mean = slice.iter().sum::<f64>() / slice.len() as f64;
        let var = slice.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / slice.len() as f64;
        let sd = var.sqrt();
        let is_local_max =
            (i == 0 || flux[i] >= flux[i - 1]) && (i + 1 >= flux.len() || flux[i] >= flux[i + 1]);
        if is_local_max
            && flux[i] > mean + cfg.onset_k * sd
            && flux[i] > abs_floor
            && flux[i] > 0.0
        {
            peaks.push(i);
        }
    }

    // 合并过近的峰，保留通量更大的那个
    let min_gap = (cfg.onset_min_gap_s / hop_s).round().max(1.0) as usize;
    let mut kept: Vec<usize> = Vec::new();
    for p in peaks {
        match kept.last() {
            Some(&last) if p - last < min_gap => {
                if flux[p] > flux[last] {
                    kept.pop();
                    kept.push(p);
                }
            }
            _ => kept.push(p),
        }
    }

    let onsets_s: Vec<f64> = kept.iter().map(|&i| (i + 1) as f64 * hop_s).collect();
    // 只把「连续音符之间」的间隔算进节奏抖动。
    //
    // 超过 2 秒的间隔是段落停顿或换气，不是节奏弹性；把它们算进来会得到
    // 荒谬的结果（4 分钟的分段夹具上实测 44997ms）。
    const MAX_IOI_MS: f64 = 2000.0;
    let (jitter_ms, mean_ioi_ms) = {
        let iois: Vec<f64> = onsets_s
            .windows(2)
            .map(|w| (w[1] - w[0]) * 1000.0)
            .filter(|&v| v <= MAX_IOI_MS)
            .collect();
        if iois.len() >= 2 {
            let mean = iois.iter().sum::<f64>() / iois.len() as f64;
            let var = iois.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / iois.len() as f64;
            (Some(var.sqrt()), Some(mean))
        } else {
            (None, None)
        }
    };

    OnsetInfo {
        onsets_s,
        jitter_ms,
        mean_ioi_ms,
    }
}

/// 频谱重心（Hz），用幅度谱加权。
fn spectral_centroid(mono: &[f32], sr: u32, start: usize, len: usize) -> f64 {
    let end = (start + len).min(mono.len());
    if end <= start {
        return 0.0;
    }
    let stft = Stft::new(StftConfig::default());
    let specs = stft.magnitude_spectrum(&mono[start..end]);
    let bin_hz = sr as f64 / stft.cfg.frame as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for s in &specs {
        for (k, &m) in s.iter().enumerate() {
            num += m * k as f64 * bin_hz;
            den += m;
        }
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// 底噪（dBFS）：帧 RMS 的最低 5%。
fn noise_floor(mono: &[f32], sr: u32) -> f64 {
    let frame = (0.02 * sr as f64) as usize;
    if mono.len() < frame || frame == 0 {
        return -120.0;
    }
    let mut rms = Vec::new();
    let mut i = 0;
    while i + frame <= mono.len() {
        let s: f64 = mono[i..i + frame]
            .iter()
            .map(|&v| (v as f64) * (v as f64))
            .sum();
        rms.push((s / frame as f64).sqrt());
        i += frame / 2;
    }
    if rms.is_empty() {
        return -120.0;
    }
    rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = (rms.len() / 20).max(1);
    db(rms[..n].iter().sum::<f64>() / n as f64)
}

/// 呼吸检测：voicing=0 + 能量高于底噪 6dB + 频谱重心 2–6kHz。
pub fn detect_breaths(buf: &AudioBuffer, track: &F0Track, cfg: &PerceptConfig) -> BreathInfo {
    let mono = buf.mono();
    let sr = buf.sample_rate;
    if mono.is_empty() || track.total_frames == 0 {
        return BreathInfo {
            starts_s: Vec::new(),
            per_minute: 0.0,
            coverage: 0.0,
        };
    }
    let floor_db = noise_floor(&mono, sr);
    let hop_s = track.hop_s.max(1e-6);
    let duration_s = mono.len() as f64 / sr as f64;

    // 逐帧能量
    let frame_len = (hop_s * sr as f64).round().max(1.0) as usize;
    let energy_db = |k: usize| -> f64 {
        let start = (k as f64 * hop_s * sr as f64).round() as usize;
        let end = (start + frame_len).min(mono.len());
        if end <= start {
            return -120.0;
        }
        let s: f64 = mono[start..end].iter().map(|&v| (v as f64) * (v as f64)).sum();
        db((s / (end - start) as f64).sqrt())
    };

    let mut starts = Vec::new();
    let mut voiced_or_not: Vec<bool> = Vec::with_capacity(track.total_frames);
    let mut run_start: Option<usize> = None;
    let mut considered = 0usize;

    // 先把逐帧能量算出来——凹陷判据要看邻域，逐帧算两遍没必要。
    let n_frames = track.total_frames;
    let frame_energy: Vec<f64> = (0..n_frames).map(energy_db).collect();
    // 邻域峰值：前后各 half 帧的最大能量。呼吸夹在音符之间，
    // 邻域里一定有更响的音符，所以「比邻域峰值低 12dB」是个稳定的判据。
    let half = cfg.breath_dip_half_win.max(1);
    let local_max: Vec<f64> = (0..n_frames)
        .map(|k| {
            let lo = k.saturating_sub(half);
            let hi = (k + half + 1).min(n_frames);
            frame_energy[lo..hi]
                .iter()
                .fold(f64::NEG_INFINITY, |a, &b| if b > a { b } else { a })
        })
        .collect();

    for k in 0..n_frames {
        let is_unvoiced = !track.voicing.get(k).copied().unwrap_or(false);
        let loud_enough = frame_energy[k] > floor_db + cfg.breath_min_above_floor_db;
        // 两条路径任一成立即可：
        //   1) 非浊音帧——自研 f0 的常规情况（浊音率 87%）
        //   2) 能量凹陷 —— RMVPE 浊音率 96.8% 时唯一可行的信号
        let dip = frame_energy[k] < local_max[k] - cfg.breath_dip_db;
        let candidate = loud_enough && (is_unvoiced || dip);
        voiced_or_not.push(candidate);
        if candidate {
            considered += 1;
            if run_start.is_none() {
                run_start = Some(k);
            }
        } else if let Some(s) = run_start.take() {
            // 段结束：检查时长与频谱重心
            let dur = (k - s) as f64 * hop_s;
            if dur >= cfg.breath_min_dur_s {
                let start_sample = (s as f64 * hop_s * sr as f64) as usize;
                let len = (dur * sr as f64) as usize;
                let c = spectral_centroid(&mono, sr, start_sample, len);
                if c >= cfg.breath_centroid_lo_hz && c <= cfg.breath_centroid_hi_hz {
                    starts.push(s as f64 * hop_s);
                }
            }
        }
    }
    // 收尾段
    if let Some(s) = run_start {
        let dur = (track.total_frames - s) as f64 * hop_s;
        if dur >= cfg.breath_min_dur_s {
            let start_sample = (s as f64 * hop_s * sr as f64) as usize;
            let len = (dur * sr as f64) as usize;
            let c = spectral_centroid(&mono, sr, start_sample, len);
            if c >= cfg.breath_centroid_lo_hz && c <= cfg.breath_centroid_hi_hz {
                starts.push(s as f64 * hop_s);
            }
        }
    }

    let minutes = (duration_s / 60.0).max(1e-6);
    BreathInfo {
        per_minute: starts.len() as f64 / minutes,
        coverage: if track.total_frames > 0 {
            considered as f64 / track.total_frames as f64
        } else {
            0.0
        },
        starts_s: starts,
    }
}

/// 颤音检测：f0 去趋势后找 4–8Hz 的周期性。
pub fn detect_vibrato(track: &F0Track, cfg: &PerceptConfig) -> Option<VibratoInfo> {
    // 收集连续浊音段，逐段分析后取中位数，避免被长段主导
    let hop_s = track.hop_s;
    if hop_s <= 0.0 {
        return None;
    }
    let mut segs: Vec<Vec<f64>> = Vec::new();
    let mut cur: Vec<f64> = Vec::new();
    for (k, f) in track.f0_hz.iter().enumerate() {
        let voiced = track.voicing.get(k).copied().unwrap_or(false);
        match f {
            Some(v) if voiced => cur.push(*v),
            _ => {
                if cur.len() >= (cfg.vibrato_min_dur_s / hop_s) as usize {
                    segs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
        }
    }
    if cur.len() >= (cfg.vibrato_min_dur_s / hop_s) as usize {
        segs.push(cur);
    }
    if segs.is_empty() {
        return None;
    }

    let mut rates = Vec::new();
    let mut depths = Vec::new();
    let mut analysed = 0.0;
    for seg in &segs {
        let n = seg.len();
        // 转 cents（相对段内中位数），消除绝对音高的影响
        let mut sorted = seg.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = sorted[n / 2];
        if med <= 0.0 {
            continue;
        }
        let cents: Vec<f64> = seg.iter().map(|f| 1200.0 * (f / med).log2()).collect();

        // 去趋势：滑动平均（约 250ms）
        let trend_win = ((0.25 / hop_s) as usize).max(3).min(n / 2 + 1);
        if trend_win == 0 || n < trend_win * 2 {
            continue;
        }
        let mut resid = vec![0.0; n];
        for i in 0..n {
            let lo = i.saturating_sub(trend_win);
            let hi = (i + trend_win).min(n);
            let mean = cents[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
            resid[i] = cents[i] - mean;
        }

        let rms = (resid.iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt();
        if rms <= 0.0 {
            continue;
        }

        // 在 4–8Hz 上扫频，找归一化自相关最强的周期
        let fs = 1.0 / hop_s;
        let mut best = (0.0f64, 0.0f64); // (rate, score)
        let mut f = cfg.vibrato_lo_hz;
        while f <= cfg.vibrato_hi_hz {
            let period = fs / f;
            if period < 2.0 || period > n as f64 / 2.0 {
                f += 0.25;
                continue;
            }
            let lag = period.round() as usize;
            if lag < 2 || lag >= n {
                f += 0.25;
                continue;
            }
            let mut num = 0.0;
            let mut d1 = 0.0;
            let mut d2 = 0.0;
            for i in 0..(n - lag) {
                num += resid[i] * resid[i + lag];
                d1 += resid[i] * resid[i];
                d2 += resid[i + lag] * resid[i + lag];
            }
            let denom = (d1 * d2).sqrt();
            let score = if denom > 0.0 { num / denom } else { 0.0 };
            if score > best.1 {
                best = (f, score);
            }
            f += 0.25;
        }

        if best.1 < 0.3 {
            continue; // 周期性太弱，不算颤音
        }
        // 深度：残差的峰峰值一半 ≈ rms × √2
        depths.push(rms * std::f64::consts::SQRT_2);
        rates.push(best.0);
        analysed += n as f64 * hop_s;
    }

    if rates.is_empty() {
        return None;
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    depths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(VibratoInfo {
        rate_hz: rates[rates.len() / 2],
        depth_cents: depths[depths.len() / 2],
        analysed_s: analysed,
    })
}

/// 合成一个带颤音的 f0 轨迹，供测试与自检使用。
#[cfg(test)]
fn synth_track(f0_seq: Vec<Option<f64>>, hop_s: f64) -> F0Track {
    let voicing = f0_seq.iter().map(|v| v.is_some()).collect();
    let confidence = f0_seq.iter().map(|v| if v.is_some() { 0.9 } else { 0.1 }).collect();
    F0Track {
        hop_s,
        total_frames: f0_seq.len(),
        voiced_frames: f0_seq.iter().filter(|v| v.is_some()).count(),
        f0_hz: f0_seq,
        voicing,
        confidence,
        route_disagreement_ratio: 0.0,
        meets_route_contract: true,
        source: "synthetic".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::AudioBuffer;
    use std::f64::consts::PI;

    fn noise_burst_signal() -> AudioBuffer {
        // 0.5s 静音 → 0.3s 噪声（呼吸）→ 0.5s 220Hz 音 → 0.4s 静音 → 0.3s 噪声
        let sr = 48_000u32;
        let n = sr as usize * 2;
        let mut v = vec![0.0f32; n];
        let mut seed = 42u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) as f32
        };
        // 第一段噪声：带高频成分
        for i in (sr as usize * 5 / 10)..(sr as usize * 8 / 10) {
            let t = i as f64 / sr as f64;
            let hf = ((2.0 * PI * 4000.0 * t).sin() * 0.12) as f32;
            v[i] = rnd() * 0.06 + hf;
        }
        // 中间的有声音
        for i in (sr as usize * 8 / 10)..(sr as usize * 13 / 10) {
            let t = i as f64 / sr as f64;
            v[i] = ((2.0 * PI * 220.0 * t).sin() * 0.4) as f32;
        }
        // 第二段噪声
        for i in (sr as usize * 17 / 10)..(sr as usize * 20 / 10) {
            let t = i as f64 / sr as f64;
            let hf = ((2.0 * PI * 4500.0 * t).sin() * 0.12) as f32;
            v[i] = rnd() * 0.06 + hf;
        }
        AudioBuffer::new(vec![v], sr, 24).unwrap()
    }

    #[test]
    fn onsets_are_detected_at_energy_jumps() {
        let b = noise_burst_signal();
        let info = detect_onsets(&b, &PerceptConfig::default());
        assert!(
            info.onsets_s.len() >= 2,
            "至少应检出两次起始，实得 {}",
            info.onsets_s.len()
        );
        // 第一个起音应接近 0.5s
        let first = info.onsets_s[0];
        assert!((first - 0.5).abs() < 0.15, "第一个起音应在 0.5s 附近，实得 {first:.2}");
    }

    /// 段落停顿不能算进节奏抖动。
    ///
    /// 构造三个音符，前两个间隔 300ms（连续），第三个隔 5 秒（段落停顿）。
    /// 正确的 jitter 只由连续的那对决定，约为 0；若把 5 秒也算进去会到几千毫秒。
    #[test]
    fn long_silence_is_not_counted_as_rhythm_jitter() {
        let sr = 48_000u32;
        let total = sr as usize * 8;
        let mut v = vec![0.0f32; total];
        let mut put_note = |at_s: f64, dur_s: f64| {
            let start = (at_s * sr as f64) as usize;
            let len = (dur_s * sr as f64) as usize;
            for i in 0..len {
                if start + i >= total {
                    break;
                }
                let t = i as f64 / sr as f64;
                // 带包络，确保有明显的起始
                let env = (-t * 8.0).exp();
                v[start + i] = ((2.0 * PI * 330.0 * t).sin() * 0.5 * env) as f32;
            }
        };
        put_note(0.5, 0.25);
        put_note(0.8, 0.25);
        put_note(6.0, 0.25);
        let b = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let info = detect_onsets(&b, &PerceptConfig::default());
        if let Some(j) = info.jitter_ms {
            assert!(
                j < 200.0,
                "5 秒停顿不该被算成节奏抖动，实得 {j:.0}ms（起音：{:?}）",
                info.onsets_s
            );
        }
    }

    /// 稳态长音不该被判出多个起音（谱通量的数值波动不能当起音）。
    #[test]
    fn steady_tone_has_at_most_one_onset() {
        let sr = 48_000u32;
        let v: Vec<f32> = (0..sr as usize)
            .map(|i| ((2.0 * PI * 220.0 * i as f64 / sr as f64).sin() * 0.3) as f32)
            .collect();
        let b = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let info = detect_onsets(&b, &PerceptConfig::default());
        assert!(
            info.onsets_s.len() <= 1,
            "稳态音最多一个起音，实得 {} 个：{:?}",
            info.onsets_s.len(),
            info.onsets_s
        );
        // 起音样本不足 3 个时不给 jitter，避免用两个点算出误导性的标准差
        assert!(info.jitter_ms.is_none());
    }

    /// 呼吸：两段高重心噪声必须被识别。
    #[test]
    fn breaths_are_detected() {
        let b = noise_burst_signal();
        // 用一个全清音的 f0 轨迹表示「没有浊音」
        let track = synth_track(vec![None; 200], 0.01);
        let info = detect_breaths(&b, &track, &PerceptConfig::default());
        assert!(
            info.per_minute >= 4.0,
            "2 秒内两次呼吸约合 60 次/分，实得 {:.1}",
            info.per_minute
        );
    }

    /// 纯浊音信号不该被判成呼吸。
    #[test]
    fn sustained_tone_is_not_breath() {
        let sr = 48_000u32;
        let v: Vec<f32> = (0..sr as usize * 2)
            .map(|i| ((2.0 * PI * 220.0 * i as f64 / sr as f64).sin() * 0.4) as f32)
            .collect();
        let b = AudioBuffer::new(vec![v], sr, 24).unwrap();
        // 全部标为浊音
        let track = synth_track(vec![Some(220.0); 200], 0.01);
        let info = detect_breaths(&b, &track, &PerceptConfig::default());
        assert_eq!(info.starts_s.len(), 0, "浊音不该被判成呼吸");
    }

    /// 5Hz / 30 cents 的颤音必须被检出。
    #[test]
    fn vibrato_rate_and_depth_are_detected() {
        let sr = 48_000.0f64;
        let hop = 0.01;
        let secs = 2.0;
        let n = (secs / hop) as usize;
        let seq: Vec<Option<f64>> = (0..n)
            .map(|i| {
                let t = i as f64 * hop;
                let cents = 30.0 * (2.0 * PI * 5.0 * t).sin();
                Some(220.0 * 2f64.powf(cents / 1200.0))
            })
            .collect();
        let track = synth_track(seq, hop);
        let v = detect_vibrato(&track, &PerceptConfig::default()).expect("应检出颤音");
        assert!(
            (v.rate_hz - 5.0).abs() < 1.0,
            "速率应接近 5Hz，实得 {:.2}",
            v.rate_hz
        );
        assert!(
            (v.depth_cents - 30.0).abs() < 12.0,
            "深度应接近 30 cents，实得 {:.1}",
            v.depth_cents
        );
        let _ = sr;
    }

    /// 平稳的长音没有颤音。
    #[test]
    fn steady_tone_has_no_vibrato() {
        let hop = 0.01;
        let seq: Vec<Option<f64>> = (0..200).map(|_| Some(220.0)).collect();
        let track = synth_track(seq, hop);
        assert!(detect_vibrato(&track, &PerceptConfig::default()).is_none());
    }
}
