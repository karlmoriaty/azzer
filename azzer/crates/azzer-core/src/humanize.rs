//! 拟真度综合分（HumanizeScore）。
//!
//! 依据：v3 §8.1。六个子项各自有一个"真人典型区间"，落在区间内得满分，
//! 偏离按区间宽度线性衰减到 0。这是全套指标里唯一"中间值最好"的一项——
//! 音高偏差太小（<8 cents）说明修音过度，太大（>25 cents）说明跑调，
//! 两端都要扣分。实现时必须对两端都有测试。
//!
//! ## 对 v2.0 原公式的修正
//!
//! v2.0 写的是 `1 - (lo - x) / lo`，用下限本身做分母。这个式子在
//! **负区间**上会溢出：底噪区间是 [-62, -48]，若实测 -70，则
//! `1 - (-62+70)/(-62) = 1.129`，超过 1，归一化失效。
//!
//! 本实现改为**用区间宽度做分母并钳制到 [0,1]**，对正负区间一致：
//! `1 - (lo - x) / (hi - lo)`。两种算法在正区间上的数值会不同，
//! 这是有意为之——宽度归一化才能让六个子项的分数可比。

use crate::EvalError;

/// 单个子项在区间内得 1 分，偏离时按区间宽度线性衰减。
///
/// - `x` 落在 `[lo, hi]` 内 → 1.0
/// - `x < lo` → `1 - (lo - x) / (hi - lo)`，最低 0
/// - `x > hi` → `1 - (x - hi) / (hi - lo)`，最低 0
///
/// `lo >= hi` 视为非法输入，返回 0（不 panic：指标计算不该因脏数据崩溃）。
pub fn in_range(x: f64, lo: f64, hi: f64) -> f64 {
    if !x.is_finite() || !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return 0.0;
    }
    let width = hi - lo;
    let score = if x < lo {
        1.0 - (lo - x) / width
    } else if x > hi {
        1.0 - (x - hi) / width
    } else {
        1.0
    };
    score.clamp(0.0, 1.0)
}

/// HumanizeScore 的六项输入。单位与区间见各字段注释。
#[derive(Debug, Clone, Copy)]
pub struct HumanizeInput {
    /// 音高偏差分布的标准差，单位 cents。真人 8–25。
    pub pitch_std_cents: f64,
    /// 起音相对网格的偏差标准差，单位毫秒。真人 12–35。
    pub onset_std_ms: f64,
    /// 响度范围 LRA，单位 LU。真人 8–12。
    pub lra_lu: f64,
    /// 呼吸事件密度，次/分钟。真人 4–12。
    pub breath_per_min: f64,
    /// 底噪电平，dBFS。真人 -62 ~ -48。
    pub noise_floor_dbfs: f64,
    /// 颤音速率，Hz。真人 4.0–7.0。
    pub vibrato_rate_hz: f64,
    /// 颤音深度，cents。真人 20–60。
    pub vibrato_depth_cents: f64,
}

/// 各项得分与加权总分。
#[derive(Debug, Clone, Copy)]
pub struct HumanizeReport {
    pub h1_pitch: f64,
    pub h2_onset: f64,
    pub h3_dynamics: f64,
    pub h4_breath: f64,
    pub h5_noise: f64,
    pub h6_vibrato: f64,
    pub total: f64,
    /// 触发 0 分的子项名，便于诊断树直接引用。
    pub failed: [bool; 6],
}

/// HumanizeScore 的权重（v3 §8.1）。
const W: [f64; 6] = [0.25, 0.15, 0.15, 0.20, 0.15, 0.10];

impl HumanizeScore {
    /// 计算综合分。任一项非有限值时返回 `Invalid`，由调用方决定降级还是报错。
    pub fn compute(input: &HumanizeInput) -> Result<HumanizeReport, EvalError> {
        let raw = [
            input.pitch_std_cents,
            input.onset_std_ms,
            input.lra_lu,
            input.breath_per_min,
            input.noise_floor_dbfs,
            input.vibrato_rate_hz,
            input.vibrato_depth_cents,
        ];
        let names = [
            "pitch_std_cents",
            "onset_std_ms",
            "lra_lu",
            "breath_per_min",
            "noise_floor_dbfs",
            "vibrato_rate_hz",
            "vibrato_depth_cents",
        ];
        for (v, n) in raw.iter().zip(names.iter()) {
            if !v.is_finite() {
                return Err(EvalError::Invalid { field: n, value: *v });
            }
        }

        let h1 = in_range(input.pitch_std_cents, 8.0, 25.0);
        let h2 = in_range(input.onset_std_ms, 12.0, 35.0);
        let h3 = in_range(input.lra_lu, 8.0, 12.0);
        let h4 = in_range(input.breath_per_min, 4.0, 12.0);
        let h5 = in_range(input.noise_floor_dbfs, -62.0, -48.0);
        // 颤音是二维：速率与深度都要在区间内，取两者较小值（短板决定）。
        let h6 = in_range(input.vibrato_rate_hz, 4.0, 7.0)
            .min(in_range(input.vibrato_depth_cents, 20.0, 60.0));

        let scores = [h1, h2, h3, h4, h5, h6];
        let total: f64 = scores.iter().zip(W.iter()).map(|(s, w)| s * w).sum();

        Ok(HumanizeReport {
            h1_pitch: h1,
            h2_onset: h2,
            h3_dynamics: h3,
            h4_breath: h4,
            h5_noise: h5,
            h6_vibrato: h6,
            total,
            failed: scores.map(|s| s <= 0.0),
        })
    }
}

/// 命名空间，仅为让 `HumanizeScore::compute` 读起来像类型方法。
pub struct HumanizeScore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_range_inside_returns_one() {
        assert!((in_range(15.0, 8.0, 25.0) - 1.0).abs() < 1e-9);
        assert!((in_range(-55.0, -62.0, -48.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn in_range_below_decays() {
        // 8 - 4 = 4，宽度 17 → 1 - 4/17
        let got = in_range(4.0, 8.0, 25.0);
        assert!((got - (1.0 - 4.0 / 17.0)).abs() < 1e-9);
    }

    #[test]
    fn in_range_above_decays() {
        // 30 - 25 = 5，宽度 17 → 1 - 5/17
        let got = in_range(30.0, 8.0, 25.0);
        assert!((got - (1.0 - 5.0 / 17.0)).abs() < 1e-9);
    }

    #[test]
    fn in_range_negative_interval_does_not_overflow() {
        // v2.0 的原公式在此会得到 1.129，本实现必须钳到 [0,1]
        let got = in_range(-70.0, -62.0, -48.0);
        assert!((got - (1.0 - 8.0 / 14.0)).abs() < 1e-9);
        assert!(got <= 1.0 && got >= 0.0);
        // 极端偏离必须归零，不能是负数
        assert_eq!(in_range(-100.0, -62.0, -48.0), 0.0);
    }

    #[test]
    fn in_range_invalid_interval_returns_zero() {
        assert_eq!(in_range(5.0, 10.0, 10.0), 0.0);
        assert_eq!(in_range(5.0, 10.0, 2.0), 0.0);
        assert_eq!(in_range(f64::NAN, 0.0, 1.0), 0.0);
    }

    fn ideal() -> HumanizeInput {
        HumanizeInput {
            pitch_std_cents: 15.0,
            onset_std_ms: 20.0,
            lra_lu: 10.0,
            breath_per_min: 8.0,
            noise_floor_dbfs: -55.0,
            vibrato_rate_hz: 5.5,
            vibrato_depth_cents: 35.0,
        }
    }

    #[test]
    fn ideal_input_scores_one() {
        let r = HumanizeScore::compute(&ideal()).unwrap();
        assert!((r.total - 1.0).abs() < 1e-9);
        assert!(r.failed.iter().all(|f| !f));
    }

    #[test]
    fn over_corrected_pitch_loses_points() {
        // 修音过度：偏差只剩 2 cents，比下限 8 少 6，宽度 17
        let mut i = ideal();
        i.pitch_std_cents = 2.0;
        let r = HumanizeScore::compute(&i).unwrap();
        assert!((r.h1_pitch - (1.0 - 6.0 / 17.0)).abs() < 1e-9);
        assert!(r.total < 1.0);
    }

    #[test]
    fn dead_noise_floor_loses_points() {
        // 数字静音：-90 dBFS，远离 -62 下限
        let mut i = ideal();
        i.noise_floor_dbfs = -90.0;
        let r = HumanizeScore::compute(&i).unwrap();
        assert_eq!(r.h5_noise, 0.0);
        assert!(r.failed[4]);
    }

    #[test]
    fn vibrato_takes_the_weaker_axis() {
        let mut i = ideal();
        i.vibrato_rate_hz = 9.0; // 超出 4.0–7.0
        let r = HumanizeScore::compute(&i).unwrap();
        assert!(r.h6_vibrato < 1.0);
        assert!(r.h6_vibrato <= r.h1_pitch.max(0.0) + 1.0); // 只是确保有值，不溢出
    }

    #[test]
    fn nan_input_is_rejected() {
        let mut i = ideal();
        i.lra_lu = f64::NAN;
        assert!(HumanizeScore::compute(&i).is_err());
    }
}
