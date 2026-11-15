//! 母带第 6、7 步：真峰值限幅 + 两遍响度归一。
//!
//! v3 §5.6 的母带链共七步，顺序不可换。本轮（R2a）只做第 6、7 步，
//! 因为**测量是判断其余五步的前提**：没有准确的 LUFS / dBTP / LRA，
//! 前面五步做得好不好无从判断，而 N15 的契约断言本身就全是测量值。
//!
//! | 步 | 内容 | 本轮 |
//! |---|---|---|
//! | 1–5 | 低频单声道化 / 动态 EQ / 多段压缩 / 谐波饱和 / 立体声增强 | R2b |
//! | 6 | 真峰值 limiter（-1.0 dBTP，lookahead 5ms） | ✅ |
//! | 7 | 两遍响度归一（先测再线性增益） | ✅ |
//!
//! ## 为什么必须两遍
//!
//! 限幅会改变响度：先归一再加限制，实际响度会低于目标；先限幅再归一，
//! 增益又会把峰值顶回去。所以是「测 → 增益 → 限幅 → 复测 → 微调」的闭环，
//! 最多 3 轮，每轮只做线性增益，**绝不用压缩代替归一**（那会吃掉动态）。

use crate::buffer::{AudioBuffer, DspError};
use crate::loudness::{measure, LoudnessReport};
use crate::truepeak::true_peak_dbtp;

#[derive(Debug, Clone, Copy)]
pub struct MasterConfig {
    /// 目标积分响度。母带交付默认 -14 LUFS。
    pub target_lufs: f64,
    /// 真峰值上限。
    pub true_peak_limit_dbtp: f64,
    /// 限幅器前视时间。
    pub lookahead_ms: f64,
    /// 限幅器释放时间。
    pub release_ms: f64,
    /// 最多迭代轮数。
    pub max_passes: u8,
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            target_lufs: -14.0,
            true_peak_limit_dbtp: -1.0,
            lookahead_ms: 5.0,
            release_ms: 100.0,
            max_passes: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MasterReport {
    pub input: LoudnessReport,
    pub input_dbtp: f64,
    pub output: LoudnessReport,
    pub output_dbtp: f64,
    /// 累计施加的线性增益（dB）。
    pub gain_db: f64,
    /// 限幅器削掉的峰值（dB，正数表示削减量）。
    pub limiter_reduction_db: f64,
    pub passes: u8,
    /// 是否达成三项门禁。
    pub meets_lufs: bool,
    pub meets_true_peak: bool,
}

/// 执行第 6、7 步。
pub fn master(buf: &AudioBuffer, cfg: &MasterConfig) -> Result<(AudioBuffer, MasterReport), DspError> {
    if buf.sample_rate != 48_000 {
        return Err(DspError::Invalid("母带链只支持 48kHz；请先用 N01 归一"));
    }
    let input_report = measure(buf)?;
    let input_dbtp = true_peak_dbtp(buf);

    let sr = buf.sample_rate as f64;
    let lookahead = (cfg.lookahead_ms / 1000.0 * sr).round().max(1.0) as usize;
    let release_coeff = (-1.0 / (cfg.release_ms / 1000.0 * sr)).exp();

    let mut current = buf.clone();
    let mut total_gain_db = 0.0;
    let mut total_reduction_db = 0.0;
    let mut passes = 0u8;

    for _ in 0..cfg.max_passes {
        passes += 1;
        let m = measure(&current)?;

        // ① 线性增益逼近目标响度（第一遍用完整差值，之后用残差）
        if let Some(lufs) = m.integrated_lufs {
            let delta = cfg.target_lufs - lufs;
            if delta.abs() > 0.05 {
                current = apply_gain(&current, delta);
                total_gain_db += delta;
            }
        }

        // ② 真峰值超限则限幅
        //
        // 限幅器作用在采样点上，但门禁量的是过采样后的真峰值。
        // 两者之差（插值余量）必须预先扣掉，否则限幅到 -1.0dBFS 后
        // 真峰值仍会高出 0.1~0.5dB（实测夹具 -0.85dBTP，超限 0.15dB）。
        let tp = true_peak_dbtp(&current);
        if tp > cfg.true_peak_limit_dbtp + 0.01 {
            let sp = crate::truepeak::sample_peak_dbfs(&current);
            let interp_excess = (tp - sp).max(0.0);
            let safe_db = cfg.true_peak_limit_dbtp - interp_excess - 0.1;
            let (limited, reduction) = limit(
                &current,
                db_to_lin(safe_db),
                lookahead,
                release_coeff,
            );
            current = limited;
            total_reduction_db += reduction;
        }

        // ③ 复测判定
        let after = measure(&current)?;
        let after_tp = true_peak_dbtp(&current);
        let lufs_ok = after
            .integrated_lufs
            .map(|l| (l - cfg.target_lufs).abs() <= 0.5)
            .unwrap_or(false);
        let tp_ok = after_tp <= cfg.true_peak_limit_dbtp + 0.05;
        if lufs_ok && tp_ok {
            return Ok((
                current,
                MasterReport {
                    input: input_report,
                    input_dbtp,
                    output: after,
                    output_dbtp: after_tp,
                    gain_db: total_gain_db,
                    limiter_reduction_db: total_reduction_db,
                    passes,
                    meets_lufs: true,
                    meets_true_peak: true,
                },
            ));
        }
    }

    // 达到轮数上限仍未完全达标：如实报告，不伪造
    let output = measure(&current)?;
    let output_dbtp = true_peak_dbtp(&current);
    let meets_lufs = output
        .integrated_lufs
        .map(|l| (l - cfg.target_lufs).abs() <= 0.5)
        .unwrap_or(false);
    let meets_true_peak = output_dbtp <= cfg.true_peak_limit_dbtp + 0.05;
    Ok((
        current,
        MasterReport {
            input: input_report,
            input_dbtp,
            output,
            output_dbtp,
            gain_db: total_gain_db,
            limiter_reduction_db: total_reduction_db,
            passes,
            meets_lufs,
            meets_true_peak,
        },
    ))
}

fn apply_gain(buf: &AudioBuffer, gain_db: f64) -> AudioBuffer {
    let g = db_to_lin(gain_db);
    let channels = buf
        .channels
        .iter()
        .map(|ch| ch.iter().map(|&x| (x as f64 * g) as f32).collect())
        .collect();
    AudioBuffer {
        channels,
        sample_rate: buf.sample_rate,
        source_bits: buf.source_bits,
    }
}

/// lookahead 峰值限幅。
///
/// 先算每个采样点"不超天花板所需的增益"，在 [n, n+L] 内取最小值作为
/// 前视增益（保证提前压下来），再做瞬时 attack / 指数 release 平滑，
/// 避免增益曲线本身产生咔嗒声。
///
/// 返回 `(处理后音频, 最大削减量 dB)`。
fn limit(
    buf: &AudioBuffer,
    ceiling_lin: f64,
    lookahead: usize,
    release_coeff: f64,
) -> (AudioBuffer, f64) {
    // 所有声道共用一条增益曲线，否则立体声像会被拉歪
    let n = buf.frame_count();
    let mut required = vec![1.0f64; n];
    for ch in &buf.channels {
        for i in 0..n {
            let a = (ch[i] as f64).abs();
            if a > ceiling_lin {
                required[i] = required[i].min(ceiling_lin / a);
            }
        }
    }

    // 前视：向后看 L 个采样，提前把增益压下去
    let lookahead_gain = min_over_future_window(&required, lookahead);

    // 平滑：下降立即，回升缓慢
    let mut smooth = vec![1.0f64; n];
    let mut cur = 1.0f64;
    for i in 0..n {
        let target = lookahead_gain[i];
        cur = if target < cur {
            target
        } else {
            target + (cur - target) * release_coeff
        };
        smooth[i] = cur;
    }

    let min_gain = smooth.iter().fold(1.0f64, |a, &b| a.min(b));
    let reduction_db = if min_gain <= 0.0 || min_gain >= 1.0 {
        0.0
    } else {
        -20.0 * min_gain.log10()
    };

    let channels = buf
        .channels
        .iter()
        .map(|ch| {
            ch.iter()
                .enumerate()
                .map(|(i, &x)| (x as f64 * smooth[i]) as f32)
                .collect()
        })
        .collect();

    (
        AudioBuffer {
            channels,
            sample_rate: buf.sample_rate,
            source_bits: buf.source_bits,
        },
        reduction_db,
    )
}

#[inline]
fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// 对每个位置取「当前及之后 L 个采样」窗口内的最小值。
///
/// 用单调队列，O(n)。朴素双重循环是 O(n×L)：4 分钟 48k 音频 n=1150 万、
/// L=240，需要 27 亿次比较，10 分钟源直接翻倍。
fn min_over_future_window(values: &[f64], lookahead: usize) -> Vec<f64> {
    use std::collections::VecDeque;
    let n = values.len();
    let mut out = vec![1.0f64; n];
    if n == 0 {
        return out;
    }
    let win = lookahead.max(1);
    let mut dq: VecDeque<usize> = VecDeque::new();
    for j in 0..win.min(n) {
        while let Some(&back) = dq.back() {
            if values[back] >= values[j] {
                dq.pop_back();
            } else {
                break;
            }
        }
        dq.push_back(j);
    }
    for i in 0..n {
        while let Some(&front) = dq.front() {
            if front < i {
                dq.pop_front();
            } else {
                break;
            }
        }
        out[i] = dq.front().map(|&j| values[j]).unwrap_or(1.0);
        let add = i + win;
        if add < n {
            while let Some(&back) = dq.back() {
                if values[back] >= values[add] {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(add);
        }
    }
    out
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

    #[test]
    fn quiet_signal_is_raised_to_target() {
        // A=0.1 的 1kHz 正弦：-0.691 + 10log10(0.01/2) + 0.69 ≈ -23.0 LUFS
        // 归一到 -14 需要约 +9.0dB
        let b = sine(0.1, 1000.0, 5.0);
        let (out, report) = master(&b, &MasterConfig::default()).unwrap();
        let lufs = report.output.integrated_lufs.unwrap();
        assert!((lufs - (-14.0)).abs() <= 0.5, "应归一到 -14 LUFS，实得 {lufs:.2}");
        assert!(
            (report.gain_db - 9.0).abs() < 1.5,
            "应施加约 +9.0dB，实得 {:.2}",
            report.gain_db
        );
        assert!(report.meets_true_peak, "峰值 {} 未达标", report.output_dbtp);
        assert_eq!(out.sample_rate, 48_000);
    }

    #[test]
    fn loud_signal_is_limited_below_ceiling() {
        // 幅度 0.99 的正弦约 -3.7 LUFS，归一到 -14 会大幅衰减；
        // 反过来构造一个已经很响、需要限幅的信号
        let b = sine(1.0, 997.0, 3.0);
        let cfg = MasterConfig {
            target_lufs: -6.0, // 目标比源还响，逼限幅器介入
            ..MasterConfig::default()
        };
        let (_out, report) = master(&b, &cfg).unwrap();
        assert!(
            report.output_dbtp <= cfg.true_peak_limit_dbtp + 0.05,
            "真峰值 {} 超过上限",
            report.output_dbtp
        );
        assert!(report.meets_true_peak);
    }

    /// 回归测试：目标响度远高于源，必然触发限幅。
    /// 曾经因为只按采样峰值限幅、没扣插值余量，真峰值停在 -0.85dBTP 超限。
    #[test]
    fn limiter_leaves_true_peak_headroom_when_gain_is_large() {
        let b = sine(0.2, 997.0, 3.0);
        let cfg = MasterConfig {
            target_lufs: -1.0, // 逼系统大幅增益后必须限幅
            ..MasterConfig::default()
        };
        let (out, report) = master(&b, &cfg).unwrap();
        let measured_tp = crate::truepeak::true_peak_dbtp(&out);
        assert!(
            measured_tp <= cfg.true_peak_limit_dbtp + 0.1,
            "限幅后真峰值 {measured_tp:.3} 应 ≤ {:.1}dBTP",
            cfg.true_peak_limit_dbtp
        );
        assert!((measured_tp - report.output_dbtp).abs() < 1e-9);
    }

    #[test]
    fn already_on_target_is_left_alone() {
        // 归一后的 -14 LUFS 信号：再跑一次应几乎不动
        let b = sine(0.1, 1000.0, 5.0);
        let (first, _) = master(&b, &MasterConfig::default()).unwrap();
        let (_, second) = master(&first, &MasterConfig::default()).unwrap();
        assert!(
            second.gain_db.abs() < 0.2,
            "已在目标响度上，第二次增益应接近 0，实得 {:.3}dB",
            second.gain_db
        );
    }

    #[test]
    fn limiter_does_not_change_gain_when_under_ceiling() {
        let b = sine(0.2, 440.0, 2.0);
        let cfg = MasterConfig {
            target_lufs: -20.0,
            ..MasterConfig::default()
        };
        let (_, report) = master(&b, &cfg).unwrap();
        assert_eq!(report.limiter_reduction_db, 0.0, "未超限时不该削峰");
    }

    #[test]
    fn non_48k_is_rejected() {
        let b = AudioBuffer::new(vec![vec![0.0f32; 44_100]], 44_100, 24).unwrap();
        assert!(master(&b, &MasterConfig::default()).is_err());
    }

    /// 单调队列必须与朴素窗口最小值逐点一致。
    #[test]
    fn min_window_matches_naive_implementation() {
        let values: Vec<f64> = (0..500)
            .map(|i| ((i as f64 * 0.37).sin() * 0.5 + 0.6).abs())
            .collect();
        for lookahead in [1usize, 2, 5, 17, 200, 600] {
            let fast = min_over_future_window(&values, lookahead);
            let naive: Vec<f64> = (0..values.len())
                .map(|i| {
                    let end = (i + lookahead.max(1)).min(values.len());
                    values[i..end]
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min)
                })
                .collect();
            for i in 0..values.len() {
                assert!(
                    (fast[i] - naive[i]).abs() < 1e-12,
                    "lookahead={lookahead} i={i}: fast={} naive={}",
                    fast[i],
                    naive[i]
                );
            }
        }
    }

    #[test]
    fn min_window_edge_cases() {
        // 空输入
        assert!(min_over_future_window(&[], 5).is_empty());
        // 单元素
        assert_eq!(min_over_future_window(&[0.5], 3), vec![0.5]);
        // lookahead=0 按 1 处理（只看自己）
        assert_eq!(min_over_future_window(&[0.9, 0.1], 0), vec![0.9, 0.1]);
        // 单调递增：每个位置的最小值就是自己
        assert_eq!(min_over_future_window(&[0.1, 0.2, 0.3], 3), vec![0.1, 0.2, 0.3]);
        // 单调递减：每个位置的最小值都是窗口末尾
        assert_eq!(min_over_future_window(&[0.3, 0.2, 0.1], 3), vec![0.1, 0.1, 0.1]);
    }

    #[test]
    fn report_is_honest_when_impossible() {
        // 极短且极静的信号：测不出积分响度，不能假装达标
        let b = sine(0.0001, 1000.0, 0.1);
        let (_, report) = master(&b, &MasterConfig::default()).unwrap();
        if report.output.integrated_lufs.is_none() {
            assert!(!report.meets_lufs, "测不出响度就不能声称达标");
        }
    }
}
