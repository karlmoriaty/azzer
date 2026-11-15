//! N05 基频提取（零模型路径）。
//!
//! v3 的契约是「**双路差异 >600 cents 帧占比 ≤1%**」，意思是用两种独立算法
//! 交叉校验：两路都同意的帧才可信。这里用 YIN 与归一化自相关两路。
//!
//! ## 为什么不做在全带宽上
//!
//! f0 检测只关心 65–1200Hz 的周期性，48kHz 采样率是浪费。先低通到 3.5kHz
//! 再抽取到 8kHz，每帧的计算量降到 1/36：4 分钟音频从约 300 亿次乘加
//! 降到约 10 亿次，单声道能在 1 秒内跑完。
//!
//! ## 精度取舍（与 rmvpe 的差距，如实记录）
//!
//! | 项 | YIN + 自相关 | rmvpe（模型） |
//! |---|---|---|
//! | 干净人声 | 好（误差 <1%） | 好 |
//! | 带伴奏/噪声 | **明显变差** | 好很多 |
//! | 气声段 voicing 判定 | 一般 | 好 |
//! | 依赖 | 零模型 | 需要模型 |
//!
//! A 入口不做分离，f0 是在**未分离的全混音**上提取的，置信度会低。
//! 这一点必须在报告里体现，不能让下游误以为 f0 可信。

use crate::biquad::lowpass;
use crate::buffer::{AudioBuffer, DspError};

#[derive(Debug, Clone, Copy)]
pub struct F0Config {
    pub hop_ms: f64,
    pub window_ms: f64,
    pub f0_min_hz: f64,
    pub f0_max_hz: f64,
    /// YIN 的绝对阈值，越小越严格。
    pub yin_threshold: f64,
    /// 低于此周期度的帧判为清音/静音。
    pub aperiodicity_max: f64,
    /// 判定两路"不一致"的 cents 阈值。
    pub route_disagree_cents: f64,
    /// 自相关取峰的相对阈值：取第一个超过「全局最大 × 此值」的局部峰。
    ///
    /// 原本是"第一个超过 0.5 的局部峰"，在人声上会系统性判高一个八度——
    /// 人声谐波丰富时，二次谐波（tau 减半）的相关峰常先过 0.5，而基频峰更高。
    /// 用相对阈值既能排除弱峰，又不会像"取全局最大"那样被次谐波骗。
    pub peak_rel_threshold: f64,
    /// 后处理的中值窗（帧数，奇数）。1 表示不做。
    ///
    /// 只用来摘掉孤立的八度错误，不做整体平滑——颤音周期 125–250ms，
    /// 而窗宽 3 帧（30ms）远小于它，不会把颤音抹平。
    pub median_window: usize,
}

impl Default for F0Config {
    fn default() -> Self {
        Self {
            hop_ms: 10.0,
            // 40ms → 50ms：8kHz 下 tau 只有 6–123，窗口越长 τ 的估计越稳，
            // 但太宽会糊掉快速滑音。50ms 是两个约束的折中。
            window_ms: 50.0,
            // 人声常见范围；v1 不假定具体音色，取宽一点
            f0_min_hz: 65.0,
            f0_max_hz: 1200.0,
            yin_threshold: 0.15,
            aperiodicity_max: 0.30,
            route_disagree_cents: 600.0,
            peak_rel_threshold: 0.90,
            median_window: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct F0Track {
    pub hop_s: f64,
    pub f0_hz: Vec<Option<f64>>,
    pub voicing: Vec<bool>,
    /// YIN 的周期度（1 - 归一化差函数最小值），越大越可信。
    pub confidence: Vec<f64>,
    /// 双路差异超过阈值的帧占比。
    pub route_disagreement_ratio: f64,
    /// 契约判定：不一致占比 ≤1%。
    pub meets_route_contract: bool,
    pub voiced_frames: usize,
    pub total_frames: usize,
    /// 实际使用的算法：`yin+autocorr`（零模型路径）或 `rmvpe`（声明了模型）。
    ///
    /// 加这个字段是因为两条路径的 `route_disagreement_ratio` 语义不同：
    /// RMVPE 是单路，不存在"双路分歧"，那一刻的 0 不代表"两路一致"。
    /// 旧缓存没有这个字段，反序列化时补默认值。
    #[serde(default)]
    pub source: String,
}

/// 低通后抽取到 8kHz，专供 f0 检测使用。
fn downsample_for_f0(x: &[f32], sr: u32) -> (Vec<f32>, f64) {
    const TARGET: f64 = 8000.0;
    if (sr as f64) <= TARGET {
        return (x.to_vec(), sr as f64);
    }
    let factor = (sr as f64 / TARGET).round().max(1.0) as usize;
    let fs_new = sr as f64 / factor as f64;
    // 抗混叠：截止取新采样率的 0.44 倍
    let mut lpf = lowpass(sr as f64, fs_new * 0.44, std::f64::consts::FRAC_1_SQRT_2);
    let filtered: Vec<f32> = x.iter().map(|&v| lpf.process(v)).collect();
    (
        filtered.iter().step_by(factor).copied().collect(),
        fs_new,
    )
}

/// YIN：返回 (f0_hz, 周期度)。
fn yin(frame: &[f32], fs: f64, cfg: &F0Config) -> (Option<f64>, f64) {
    let w = frame.len();
    let tau_min = (fs / cfg.f0_max_hz).floor().max(2.0) as usize;
    let tau_max = ((fs / cfg.f0_min_hz).ceil() as usize).min(w / 2);
    if tau_max <= tau_min + 2 {
        return (None, 0.0);
    }

    let mut d = vec![0.0f64; tau_max + 1];
    for tau in tau_min..=tau_max {
        let mut sum = 0.0f64;
        for j in 0..(w - tau) {
            let diff = frame[j] as f64 - frame[j + tau] as f64;
            sum += diff * diff;
        }
        d[tau] = sum;
    }

    // 累积均值归一化
    let mut cmnd = vec![1.0f64; tau_max + 1];
    let mut running = 0.0f64;
    for tau in tau_min..=tau_max {
        running += d[tau];
        if running > 0.0 {
            cmnd[tau] = d[tau] * (tau - tau_min + 1) as f64 / running;
        }
    }

    // 找第一个低于阈值的局部谷
    let mut best: Option<usize> = None;
    let mut tau = tau_min + 1;
    while tau < tau_max {
        if cmnd[tau] < cfg.yin_threshold {
            while tau + 1 < tau_max && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            best = Some(tau);
            break;
        }
        tau += 1;
    }
    // 没找到就用全局最小（可能是不稳定帧）
    let (tau_best, ap) = match best {
        Some(t) => (t, cmnd[t]),
        None => {
            let mut t = tau_min;
            for i in tau_min..=tau_max {
                if cmnd[i] < cmnd[t] {
                    t = i;
                }
            }
            (t, cmnd[t])
        }
    };

    if ap > cfg.aperiodicity_max {
        return (None, (1.0 - ap).max(0.0));
    }

    // 抛物线插值精化 τ
    let refined = if tau_best > tau_min && tau_best < tau_max {
        let a = cmnd[tau_best - 1];
        let b = cmnd[tau_best];
        let c = cmnd[tau_best + 1];
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            tau_best as f64 + 0.5 * (a - c) / denom
        } else {
            tau_best as f64
        }
    } else {
        tau_best as f64
    };

    let f0 = fs / refined;
    if f0.is_finite() && f0 >= cfg.f0_min_hz && f0 <= cfg.f0_max_hz {
        (Some(f0), (1.0 - ap).clamp(0.0, 1.0))
    } else {
        (None, (1.0 - ap).max(0.0).clamp(0.0, 1.0))
    }
}

/// 归一化自相关：取第一个显著峰作为周期。
fn autocorr_f0(frame: &[f32], fs: f64, cfg: &F0Config) -> Option<f64> {
    let w = frame.len();
    let tau_min = (fs / cfg.f0_max_hz).floor().max(2.0) as usize;
    let tau_max = ((fs / cfg.f0_min_hz).ceil() as usize).min(w / 2);
    if tau_max <= tau_min + 2 {
        return None;
    }
    let energy: f64 = frame.iter().map(|&v| (v as f64) * (v as f64)).sum();
    if energy < 1e-9 {
        return None;
    }

    let mut r = vec![0.0f64; tau_max + 1];
    for tau in tau_min..=tau_max {
        let mut sum = 0.0f64;
        let mut e2 = 0.0f64;
        for j in 0..(w - tau) {
            sum += frame[j] as f64 * frame[j + tau] as f64;
            e2 += (frame[j + tau] as f64) * (frame[j + tau] as f64);
        }
        r[tau] = if e2 > 0.0 { sum / (energy * e2).sqrt() } else { 0.0 };
    }

    // 取第一个超过「全局最大 × peak_rel_threshold」的局部峰。
    //
    // 为什么不是"第一个超过 0.5 的峰"：人声谐波丰富，二次谐波在 tau/2 处
    // 的相关峰常常先越过 0.5，而基频峰虽然更高却排在后面。按绝对阈值取，
    // 就会系统性地判高一个八度（实测某条真实人声上八度错误率 9.1%）。
    // 相对阈值把"弱峰"挡掉，又不至于像"直接取全局最大"那样被次谐波骗。
    //
    // 但相对阈值单独用会丢掉绝对质量把关：白噪声的 r_max 只有 0.1–0.2，
    // 「0.9 × r_max」照样能过，于是每个噪声帧都被判成浊音（实测 100%）。
    // 所以再加一个绝对下限。
    const MIN_AUTOCORR_PEAK: f64 = 0.40;
    let mut r_max = 0.0f64;
    for tau in tau_min..=tau_max {
        if r[tau] > r_max {
            r_max = r[tau];
        }
    }
    if r_max < MIN_AUTOCORR_PEAK {
        // 连最强的那一档周期性都不够，直接判清音
        return None;
    }
    let thresh = (r_max * cfg.peak_rel_threshold).max(MIN_AUTOCORR_PEAK);

    let mut best: Option<usize> = None;
    for tau in (tau_min + 1)..tau_max {
        if r[tau] >= thresh && r[tau] >= r[tau - 1] && r[tau] >= r[tau + 1] {
            best = Some(tau);
            break;
        }
    }
    // 兜底：全局最大若正好落在区间端点（不构成局部峰），直接用它。
    let t = match best {
        Some(t) => t,
        None => {
            let mut tt = tau_min;
            for i in tau_min..=tau_max {
                if r[i] > r[tt] {
                    tt = i;
                }
            }
            tt
        }
    };
    let refined = if t > tau_min && t < tau_max {
        let a = r[t - 1];
        let b = r[t];
        let c = r[t + 1];
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-12 {
            t as f64 + 0.5 * (a - c) / denom
        } else {
            t as f64
        }
    } else {
        t as f64
    };
    let f0 = fs / refined;
    if f0.is_finite() && f0 >= cfg.f0_min_hz && f0 <= cfg.f0_max_hz {
        Some(f0)
    } else {
        None
    }
}

#[inline]
fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2().abs()
}

/// 把 `f` 折叠到离 `reference` 最近的八度（2 的整数次幂倍）。
///
/// f0 检测最常见的失败模式就是八度错误（判成本身的两倍或一半）。
/// 比较两路结果前先做这一步，否则一个 2 倍关系会被当成 1200 cents 的"真分歧"。
fn fold_octave(f: f64, reference: f64) -> f64 {
    if f <= 0.0 || reference <= 0.0 {
        return f;
    }
    let n = (f / reference).log2().round();
    f / 2f64.powf(n)
}

/// 摘掉孤立的八度错误，不做整体平滑。
///
/// 判据：某帧与前后邻域的中值相差超过 600 cents，且折叠到该中值后差小于
/// 100 cents —— 这就是"跳一个八度又跳回来"的孤立错误。
///
/// **真实演唱里的八度跳进不会被改**：那种跳变会连着好几帧，邻域中值自己就
/// 跟着走了。窗宽只取 3 帧（30ms），远小于颤音周期（125–250ms），所以颤音
/// 也不会被抹掉——这一点很关键，H6 颤音检测就靠 f0 上的 4–8Hz 起伏。
fn fix_isolated_octave_jumps(f0: &mut [Option<f64>], win: usize, f0_min: f64, f0_max: f64) {
    if win < 3 || f0.len() < 3 {
        return;
    }
    let half = win / 2;
    let orig = f0.to_vec();
    for i in half..f0.len().saturating_sub(half) {
        let Some(cur) = orig[i] else { continue };

        // 邻域中值（不含自身），这样单个异常值不会污染参考基准
        let mut nb: Vec<f64> = Vec::with_capacity(win - 1);
        for k in (i - half)..=(i + half) {
            if k != i {
                if let Some(v) = orig[k] {
                    nb.push(v);
                }
            }
        }
        if nb.len() < 2 {
            continue;
        }
        nb.sort_by(|a, b| a.partial_cmp(b).expect("f0 不应为 NaN"));
        let med = nb[nb.len() / 2];

        if cents(cur, med) > 600.0 {
            let folded = fold_octave(cur, med);
            // 折叠是双向的：既能往下（÷2ⁿ）也能往上（×2ⁿ）。
            // 往上折会越出合法音域——实测 306Hz 遇到 1200Hz 的中值会被折成
            // 1224Hz，超过 f0_max 却仍满足「离中值 33 cents」，于是被写了回去。
            // 所以必须再校一次范围。
            if folded >= f0_min && folded <= f0_max && cents(folded, med) < 100.0 {
                f0[i] = Some(folded);
            }
        }
    }
}

/// 双路提取。
pub fn extract_f0(buf: &AudioBuffer, cfg: &F0Config) -> Result<F0Track, DspError> {
    let mono = buf.mono();
    if mono.is_empty() {
        return Err(DspError::Invalid("空音频无法提取基频"));
    }
    let (x, fs) = downsample_for_f0(&mono, buf.sample_rate);
    let hop = ((cfg.hop_ms / 1000.0) * fs).round().max(1.0) as usize;
    let win = ((cfg.window_ms / 1000.0) * fs).round().max(1.0) as usize;
    if x.len() < win {
        return Ok(F0Track {
            hop_s: hop as f64 / fs,
            f0_hz: Vec::new(),
            voicing: Vec::new(),
            confidence: Vec::new(),
            route_disagreement_ratio: 0.0,
            meets_route_contract: true,
            voiced_frames: 0,
            total_frames: 0,
            source: "yin+autocorr".into(),
        });
    }

    let frames = (x.len() - win) / hop + 1;
    let mut f0_hz = Vec::with_capacity(frames);
    let mut voicing = Vec::with_capacity(frames);
    let mut confidence = Vec::with_capacity(frames);
    let mut disagree = 0usize;
    let mut voiced = 0usize;

    for k in 0..frames {
        let start = k * hop;
        let frame = &x[start..start + win];
        let (y, conf) = yin(frame, fs, cfg);
        let a = autocorr_f0(frame, fs, cfg);

        // 双路交叉校验。
        //
        // 先做八度折叠再比较：两路之一判成 2 倍或 1/2 是最常见的分歧形态，
        // 直接比较会把它算成 1200 cents 的真分歧，虚高不一致率。
        let (out, is_voiced) = match (y, a) {
            (Some(f1), Some(f2)) => {
                let f2c = fold_octave(f2, f1);
                if cents(f1, f2c) > cfg.route_disagree_cents {
                    disagree += 1;
                    // 真分歧（不是八度关系）：取 YIN 那一路，但记入不一致计数
                    (Some(f1), true)
                } else {
                    // 频率的中点要用几何平均。算术平均会系统性偏高：
                    // 跨 1200 cents 时，算术中点比几何中点高约 100 cents。
                    //
                    // 但几何平均会把值拉到两路之间，可能越出 [f0_min, f0_max]：
                    // 两路分别贴着上下边界时，中点就跑到边界外去了（实测出现过
                    // 55.1Hz 与 1224Hz，而配置是 65–1200）。越界时退回 YIN 那一路，
                    // 它本身已经保证在范围内。
                    let merged = (f1 * f2c).sqrt();
                    if merged >= cfg.f0_min_hz && merged <= cfg.f0_max_hz {
                        (Some(merged), true)
                    } else {
                        (Some(f1), true)
                    }
                }
            }
            (Some(f1), None) => (Some(f1), true),
            (None, Some(f2)) => (Some(f2), true),
            (None, None) => (None, false),
        };

        if is_voiced {
            voiced += 1;
        }
        f0_hz.push(out);
        voicing.push(is_voiced);
        confidence.push(conf);
    }

    let ratio = if frames > 0 {
        disagree as f64 / frames as f64
    } else {
        0.0
    };

    // 后处理：摘掉孤立的八度错误。放在统计之前不影响 voiced 计数
    // （折叠不改变"这一帧有没有值"），但会修正 f0 数值本身。
    if cfg.median_window >= 3 {
        fix_isolated_octave_jumps(&mut f0_hz, cfg.median_window, cfg.f0_min_hz, cfg.f0_max_hz);
    }

    Ok(F0Track {
        hop_s: hop as f64 / fs,
        f0_hz,
        voicing,
        confidence,
        route_disagreement_ratio: ratio,
        meets_route_contract: ratio <= 0.01,
        voiced_frames: voiced,
        total_frames: frames,
        source: "yin+autocorr".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn signal(secs: f64, mut gen: impl FnMut(f64) -> f64) -> AudioBuffer {
        let sr = 48_000u32;
        let n = (sr as f64 * secs) as usize;
        let v: Vec<f32> = (0..n).map(|i| gen(i as f64 / sr as f64) as f32).collect();
        AudioBuffer::new(vec![v], sr, 24).unwrap()
    }

    fn median_f0(t: &F0Track) -> f64 {
        let mut v: Vec<f64> = t.f0_hz.iter().flatten().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    }

    #[test]
    fn detects_pure_sine() {
        let b = signal(0.5, |t| (2.0 * PI * 220.0 * t).sin() * 0.5);
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let f = median_f0(&t);
        assert!((f - 220.0).abs() < 5.0, "应检出 220Hz，实得 {f:.1}");
        assert!(t.voiced_frames > 0);
    }

    /// 锯齿波谐波丰富，容易出倍频/半频错误——这是 f0 检测最典型的失效模式。
    #[test]
    fn sawtooth_does_not_double_the_pitch() {
        let b = signal(0.5, |t| {
            let phase = (t * 200.0) % 1.0;
            (2.0 * phase - 1.0) * 0.4
        });
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let f = median_f0(&t);
        assert!((f - 200.0).abs() < 8.0, "锯齿波应检出 200Hz 而非倍频，实得 {f:.1}");
    }

    #[test]
    fn silence_is_unvoiced() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 48_000]], 48_000, 24).unwrap();
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        assert_eq!(t.voiced_frames, 0, "静音不该有浊音帧");
        assert!(t.f0_hz.iter().all(|v| v.is_none()));
    }

    #[test]
    fn tracks_a_glide() {
        // 200Hz 线性滑到 400Hz
        let secs = 2.0;
        let b = signal(secs, |t| {
            let f = 200.0 + 100.0 * t; // 200 → 400
            (2.0 * PI * f * t).sin() * 0.5
        });
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let first = t.f0_hz.iter().flatten().next().copied().unwrap_or(0.0);
        let last = t.f0_hz.iter().flatten().last().copied().unwrap_or(0.0);
        assert!(first < 240.0, "起始应接近 200Hz，实得 {first:.1}");
        assert!(last > 360.0, "末尾应接近 400Hz，实得 {last:.1}");
    }

    /// 干净信号上两路应当高度一致，契约必然达标。
    #[test]
    fn clean_signal_passes_route_contract() {
        let b = signal(1.0, |t| (2.0 * PI * 330.0 * t).sin() * 0.5);
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        assert!(
            t.meets_route_contract,
            "干净信号的不一致率应 ≤1%，实得 {:.3}",
            t.route_disagreement_ratio
        );
    }

    /// 白噪声没有基频，应基本不产生浊音帧。
    #[test]
    fn white_noise_is_mostly_unvoiced() {
        let mut seed = 12345u64;
        let b = signal(0.5, |_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.6
        });
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let ratio = t.voiced_frames as f64 / t.total_frames.max(1) as f64;
        assert!(ratio < 0.2, "白噪声浊音帧占比应 <20%，实得 {ratio:.2}");
    }

    /// 类人声信号：基频 + 谐波 + 缓慢音量起伏。
    /// 这是 f0 检测真正的目标输入；纯正弦太简单，噪声太乱，都代表不了人声。
    #[test]
    fn harmonic_voice_like_signal_passes_contract() {
        let b = signal(1.5, |t| {
            let f0 = 220.0;
            let env = 0.55 + 0.45 * (2.0 * PI * 3.0 * t).sin();
            let mut s = 0.0;
            for h in 1..=6 {
                s += (2.0 * PI * f0 * h as f64 * t).sin() / h as f64;
            }
            s * env * 0.3
        });
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let f = median_f0(&t);
        assert!((f - 220.0).abs() < 10.0, "类人声应检出 220Hz，实得 {f:.1}");
        assert!(
            t.meets_route_contract,
            "类人声的两路不一致率应 ≤1%，实得 {:.3}%",
            t.route_disagreement_ratio * 100.0
        );
    }

    /// 带轻微噪声的类人声（模拟真实录音的底噪）。
    #[test]
    fn voice_with_noise_still_mostly_agrees() {
        let mut seed = 999u64;
        let b = signal(1.5, |t| {
            let f0 = 180.0;
            let mut s = 0.0;
            for h in 1..=5 {
                s += (2.0 * PI * f0 * h as f64 * t).sin() / h as f64;
            }
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.02;
            s * 0.3 + n
        });
        let t = extract_f0(&b, &F0Config::default()).unwrap();
        let f = median_f0(&t);
        assert!((f - 180.0).abs() < 10.0, "带噪也应检出 180Hz，实得 {f:.1}");
        assert!(
            t.route_disagreement_ratio < 0.05,
            "轻微底噪下不一致率应 <5%，实得 {:.2}%",
            t.route_disagreement_ratio * 100.0
        );
    }

    #[test]
    fn cents_distance_is_symmetric() {
        assert!((cents(440.0, 880.0) - 1200.0).abs() < 1e-9);
        assert!((cents(880.0, 440.0) - 1200.0).abs() < 1e-9);
    }

    #[test]
    fn fold_octave_brings_ratio_near_one() {
        // 200 相对 100 折叠后应回到 100（少一个八度）
        assert!((fold_octave(200.0, 100.0) - 100.0).abs() < 1e-9);
        // 50 相对 100 折叠后应回到 100（多一个八度）
        assert!((fold_octave(50.0, 100.0) - 100.0).abs() < 1e-9);
        // 本来就是同一八度：不动
        assert!((fold_octave(110.0, 100.0) - 110.0).abs() < 1e-9);
        // 已经是 1200 cents 的正中间（sqrt(2) 倍）时，round 会取到某一侧，仍是有限值
        let mid = fold_octave(100.0 * 2f64.sqrt(), 100.0);
        assert!(mid.is_finite() && mid > 0.0);
    }

    #[test]
    fn isolated_octave_jump_is_fixed() {
        // 中间那一帧跳了一个八度又跳回来，应被折回
        let mut v: Vec<Option<f64>> = vec![
            Some(100.0),
            Some(100.0),
            Some(200.0),
            Some(100.0),
            Some(100.0),
        ];
        fix_isolated_octave_jumps(&mut v, 3, 65.0, 1200.0);
        assert!(
            (v[2].unwrap() - 100.0).abs() < 1.0,
            "孤立八度错误应被折回 100Hz，实得 {:?}",
            v[2]
        );
        // 正常帧不受影响
        assert!((v[0].unwrap() - 100.0).abs() < 1e-9);
        assert!((v[4].unwrap() - 100.0).abs() < 1e-9);
    }

    /// 真实演唱里的八度跳进（连唱好几帧）必须保住——这是表现力，不是错误。
    #[test]
    fn sustained_octave_jump_is_kept() {
        let mut v: Vec<Option<f64>> = vec![
            Some(100.0),
            Some(100.0),
            Some(100.0),
            Some(200.0),
            Some(200.0),
            Some(200.0),
        ];
        fix_isolated_octave_jumps(&mut v, 3, 65.0, 1200.0);
        assert!(
            (v[3].unwrap() - 200.0).abs() < 1.0,
            "持续的八度跳进不该被抹平，实得 {:?}",
            v[3]
        );
        assert!((v[5].unwrap() - 200.0).abs() < 1.0);
    }

    /// 折叠是双向的，向上折可能越出音域，必须挡住。
    /// 实测触发过：306Hz 遇到 1200Hz 的邻域中值会折成 1224Hz。
    #[test]
    fn fold_never_escapes_range() {
        let mut v: Vec<Option<f64>> = vec![Some(1200.0), Some(306.0), Some(1200.0)];
        fix_isolated_octave_jumps(&mut v, 3, 65.0, 1200.0);
        let got = v[1].unwrap();
        assert!(
            (65.0..=1200.0).contains(&got),
            "折叠结果必须落在 [65, 1200] 内，实得 {got:.1}"
        );
    }

    /// 颤音必须被保住：±50 cents、5Hz，在 10ms hop 下周期约 20 帧。
    #[test]
    fn vibrato_survives_postprocess() {        let mut v: Vec<Option<f64>> = Vec::new();
        for i in 0..200 {
            let t = i as f64 * 0.01; // 10ms hop
            let cents_off = 50.0 * (2.0 * PI * 5.0 * t).sin();
            v.push(Some(200.0 * 2f64.powf(cents_off / 1200.0)));
        }
        let before = v.clone();
        fix_isolated_octave_jumps(&mut v, 3, 65.0, 1200.0);
        let max_dev = v
            .iter()
            .zip(before.iter())
            .filter_map(|(a, b)| match (a, b) {
                (Some(x), Some(y)) => Some(cents(*x, *y)),
                _ => None,
            })
            .fold(0.0f64, f64::max);
        assert!(max_dev < 5.0, "后处理不该改动颤音，最大偏差 {max_dev:.1} cents");
    }
}
