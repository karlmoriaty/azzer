//! 二阶 IIR 滤波器（transposed direct form II）。
//!
//! 内部用 f64 递推、输入输出 f32，兼顾精度与内存。
//!
//! 用 transposed direct form II 而不是 direct form I：状态量更小、在 f32
//! 极端增益下更不容易溢出，这是音频处理的常规选择。
//!
//! 目前只提供 BS.1770 所需的固定系数（48kHz），**不做任意采样率重设计**。
//! 原因是 N01 已把全链路归一到 48k；若要支持其他采样率，必须按
//! BS.1770 的模拟原型重新设计系数，不能直接搬 48k 的数。

/// 单个二阶段。
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(b: [f64; 3], a: [f64; 3]) -> Self {
        debug_assert!(a[0] != 0.0, "a0 不能为 0");
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            // transposed DF-II 里 a1/a2 取负号参与递推
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y as f32
    }
}

// ---------------------------------------------------------------------------
// RBJ Audio EQ Cookbook 的系数设计。
// 母带链的分频、动态 EQ 都从这里生成；BS.1770 那两级固定系数是特例，单独放上面。
// ---------------------------------------------------------------------------

/// 二阶低通。
pub fn lowpass(sample_rate: f64, f0: f64, q: f64) -> Biquad {
    let w0 = 2.0 * std::f64::consts::PI * f0 / sample_rate;
    let c = w0.cos();
    let alpha = w0.sin() / (2.0 * q);
    Biquad::new(
        [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
        [1.0 + alpha, -2.0 * c, 1.0 - alpha],
    )
}

/// 二阶高通。
pub fn highpass(sample_rate: f64, f0: f64, q: f64) -> Biquad {
    let w0 = 2.0 * std::f64::consts::PI * f0 / sample_rate;
    let c = w0.cos();
    let alpha = w0.sin() / (2.0 * q);
    Biquad::new(
        [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
        [1.0 + alpha, -2.0 * c, 1.0 - alpha],
    )
}

/// 峰值 EQ（peaking），用于去浑浊。
pub fn peaking(sample_rate: f64, f0: f64, q: f64, gain_db: f64) -> Biquad {
    let w0 = 2.0 * std::f64::consts::PI * f0 / sample_rate;
    let c = w0.cos();
    let alpha = w0.sin() / (2.0 * q);
    let a = 10f64.powf(gain_db / 40.0);
    Biquad::new(
        [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a],
        [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a],
    )
}

/// Linkwitz-Riley 4 阶分频（**独立双路径**）。
///
/// 低通与高通各由两个 Q=0.707 的 Butterworth 2 阶级联（-24dB/oct），
/// 在分频点各 -6dB。两者相加**幅度全通但有相位翻转**（实测 440Hz 处约 45°），
/// 所以适合「替换其中一段」的处理：低频单声道化、多段压缩。
///
/// 若需要「精确重建剩余部分」，用 [`ComplementarySplit`]。
#[derive(Debug, Clone, Copy)]
pub struct LinkwitzRiley4 {
    lp1: Biquad,
    lp2: Biquad,
    hp1: Biquad,
    hp2: Biquad,
}

impl LinkwitzRiley4 {
    pub fn new(sample_rate: f64, crossover_hz: f64) -> Self {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        Self {
            lp1: lowpass(sample_rate, crossover_hz, q),
            lp2: lowpass(sample_rate, crossover_hz, q),
            hp1: highpass(sample_rate, crossover_hz, q),
            hp2: highpass(sample_rate, crossover_hz, q),
        }
    }

    pub fn reset(&mut self) {
        self.lp1.reset();
        self.lp2.reset();
        self.hp1.reset();
        self.hp2.reset();
    }

    /// 返回 (低频分量, 高频分量)，两条路径独立。
    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        let lo = self.lp2.process(self.lp1.process(x));
        let hi = self.hp2.process(self.hp1.process(x));
        (lo, hi)
    }
}

/// 互补式分频：低通 = 4 阶 Butterworth，高频 = `x - 低通`。
///
/// 相加**精确等于输入**（无相位失真），适合「只给高频加增益、低频必须原样」的
/// 立体声加宽，以及从信号里精确扣掉一个频带。
///
/// 代价：`x - LP` 在分频点以下不是 0（LP 有相位），所以**不能**拿它做
/// 低频单声道化——低频会泄漏到高频路径里（实测残留能量 0.154）。
#[derive(Debug, Clone, Copy)]
pub struct ComplementarySplit {
    lp1: Biquad,
    lp2: Biquad,
}

impl ComplementarySplit {
    pub fn new(sample_rate: f64, crossover_hz: f64) -> Self {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        Self {
            lp1: lowpass(sample_rate, crossover_hz, q),
            lp2: lowpass(sample_rate, crossover_hz, q),
        }
    }

    pub fn reset(&mut self) {
        self.lp1.reset();
        self.lp2.reset();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        let lo = self.lp2.process(self.lp1.process(x));
        (lo, x - lo)
    }
}

/// BS.1770-4 的第一级：高架滤波（约 +4dB @ 1.5kHz）。
/// 系数来自标准给出的 48kHz 数值，改采样率必须重新设计。
pub fn k_weighting_shelf_48k() -> Biquad {
    Biquad::new(
        [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
        [1.0, -1.690_659_293_182_41, 0.732_480_774_215_85],
    )
}

/// BS.1770-4 的第二级：38Hz 高通。
pub fn k_weighting_highpass_48k() -> Biquad {
    Biquad::new(
        [1.0, -2.0, 1.0],
        [1.0, -1.990_047_454_833_98, 0.990_072_250_366_21],
    )
}

/// 完整 K 加权：两级串联。
#[derive(Debug, Clone, Copy)]
pub struct KWeighting {
    shelf: Biquad,
    highpass: Biquad,
}

impl Default for KWeighting {
    fn default() -> Self {
        Self::new()
    }
}

impl KWeighting {
    pub fn new() -> Self {
        Self {
            shelf: k_weighting_shelf_48k(),
            highpass: k_weighting_highpass_48k(),
        }
    }

    pub fn reset(&mut self) {
        self.shelf.reset();
        self.highpass.reset();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.highpass.process(self.shelf.process(x))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_1_SQRT_2, PI};

    /// 直流增益：两级都应为 0dB（shelf 在 DC 处增益 0dB，高通在 DC 处衰减到 0）。
    /// 高通在 DC 处是 0，所以整体 DC 增益应为 0。
    #[test]
    fn highpass_blocks_dc() {
        let mut hp = k_weighting_highpass_48k();
        let mut last = 0.0f32;
        for _ in 0..20_000 {
            last = hp.process(1.0);
        }
        assert!(last.abs() < 1e-4, "直流应被高通滤掉，实得 {last}");
    }

    /// BS.1770 的 K 加权在 1kHz 处约 **+0.69dB 而不是 0dB**。
    /// 这是标准的特性：shelving 级在 1kHz 已经抬起来了，不是平坦的。
    /// 之前把期望写成 0dB 是错的，实测 0.698dB 才是对的。
    #[test]
    fn k_weighting_gain_at_1khz_matches_standard() {
        let sr = 48_000.0;
        let freq = 1000.0;
        let n = 48_000usize; // 1 秒
        let mut kw = KWeighting::new();
        let mut in_energy = 0.0f64;
        let mut out_energy = 0.0f64;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f64 / sr).sin() as f32;
            let y = kw.process(x);
            in_energy += (x as f64).powi(2);
            out_energy += (y as f64).powi(2);
        }
        let gain_db = 10.0 * (out_energy / in_energy).log10();
        assert!(
            (gain_db - 0.69).abs() < 0.15,
            "1kHz 处 K 加权增益应约 +0.69dB，实得 {gain_db:.3}dB"
        );
    }

    /// K 加权在 60Hz 应明显衰减（高通在 38Hz，60Hz 已进入过渡带）。
    #[test]
    fn k_weighting_attenuates_low_frequency() {
        let sr = 48_000.0;
        let freq = 60.0;
        let n = 48_000usize;
        let mut kw = KWeighting::new();
        let mut in_energy = 0.0f64;
        let mut out_energy = 0.0f64;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f64 / sr).sin() as f32;
            let y = kw.process(x);
            in_energy += (x as f64).powi(2);
            out_energy += (y as f64).powi(2);
        }
        let gain_db = 10.0 * (out_energy / in_energy).log10();
        assert!(
            (-4.0..-2.0).contains(&gain_db),
            "60Hz 应衰减约 -2.9dB，实得 {gain_db:.2}dB"
        );
    }

    /// 高频端应抬升（shelving 的设计目标），10kHz 约 +4dB。
    #[test]
    fn k_weighting_boosts_high_frequency() {
        let sr = 48_000.0;
        let freq = 10_000.0;
        let n = 48_000usize;
        let mut kw = KWeighting::new();
        let mut in_energy = 0.0f64;
        let mut out_energy = 0.0f64;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f64 / sr).sin() as f32;
            let y = kw.process(x);
            in_energy += (x as f64).powi(2);
            out_energy += (y as f64).powi(2);
        }
        let gain_db = 10.0 * (out_energy / in_energy).log10();
        assert!(
            (gain_db - 4.0).abs() < 1.0,
            "10kHz 应抬升约 +4dB，实得 {gain_db:.2}dB"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut b = k_weighting_highpass_48k();
        for _ in 0..100 {
            b.process(1.0);
        }
        b.reset();
        // reset 后对 0 输入的输出必须立刻是 0
        assert_eq!(b.process(0.0), 0.0);
    }

    /// RBJ 低通/高通在远离分频点处应分别接近 0dB 与明显衰减。
    #[test]
    fn rbj_lowpass_and_highpass_are_complementary() {
        let sr = 48_000.0;
        let f0 = 1000.0;
        let measure = |mut b: Biquad, freq: f64| -> f64 {
            let n = 48_000usize;
            let mut ein = 0.0f64;
            let mut eout = 0.0f64;
            for i in 0..n {
                let x = (2.0 * PI * freq * i as f64 / sr).sin() as f32;
                let y = b.process(x);
                ein += (x as f64).powi(2);
                eout += (y as f64).powi(2);
            }
            10.0 * (eout / ein).log10()
        };
        assert!((measure(lowpass(sr, f0, FRAC_1_SQRT_2), 100.0)).abs() < 0.5);
        assert!(measure(lowpass(sr, f0, FRAC_1_SQRT_2), 8000.0) < -20.0);
        assert!(measure(highpass(sr, f0, FRAC_1_SQRT_2), 8000.0).abs() < 0.5);
        assert!(measure(highpass(sr, f0, FRAC_1_SQRT_2), 100.0) < -20.0);
    }

    /// 独立双路径：相加幅度全通（相位不保），所以验证能量而不是瞬时值。
    #[test]
    fn lr4_is_magnitude_allpass() {
        let sr = 48_000.0;
        let freq = 440.0;
        let mut lr = LinkwitzRiley4::new(sr, 120.0);
        let mut ein = 0.0f64;
        let mut esum = 0.0f64;
        for i in 0..48_000 {
            let x = ((2.0 * PI * freq * i as f64 / sr).sin() * 0.7) as f32;
            let (lo, hi) = lr.process(x);
            ein += (x as f64).powi(2);
            esum += ((lo + hi) as f64).powi(2);
        }
        let db = 10.0 * (esum / ein).log10();
        assert!(db.abs() < 0.3, "LR4 相加应幅度全通，实得 {db:.3}dB");
    }

    /// 互补式：相加必须**精确**等于输入。
    #[test]
    fn complementary_split_reconstructs_input() {
        let sr = 48_000.0;
        let mut cs = ComplementarySplit::new(sr, 120.0);
        let mut max_err = 0.0f64;
        for i in 0..48_000 {
            let x = ((2.0 * PI * 440.0 * i as f64 / sr).sin() * 0.7) as f32;
            let (lo, hi) = cs.process(x);
            max_err = max_err.max((x - (lo + hi)) as f64);
        }
        assert!(max_err < 1e-6, "互补分频应精确重建，实得误差 {max_err:.3e}");
    }

    /// 分频点处两条路径各约 -6dB，这是 -24dB/oct 的来源。
    #[test]
    fn lr4_crossover_is_minus_6db() {
        let sr = 48_000.0;
        let f0 = 1000.0;
        let mut lr = LinkwitzRiley4::new(sr, f0);
        let mut elo = 0.0f64;
        let mut ehi = 0.0f64;
        let mut ein = 0.0f64;
        let n = 48_000usize;
        for i in 0..n {
            let x = (2.0 * PI * f0 * i as f64 / sr).sin() as f32;
            let (lo, hi) = lr.process(x);
            ein += (x as f64).powi(2);
            elo += (lo as f64).powi(2);
            ehi += (hi as f64).powi(2);
        }
        let lo_db = 10.0 * (elo / ein).log10();
        let hi_db = 10.0 * (ehi / ein).log10();
        assert!((lo_db + 6.0).abs() < 0.3, "分频点低通应约 -6dB，实得 {lo_db:.2}");
        assert!((hi_db + 6.0).abs() < 0.3, "分频点高通应约 -6dB，实得 {hi_db:.2}");
    }

    /// peaking EQ 的增益应准确落在设定值上。
    #[test]
    fn peaking_gain_matches_setting() {
        let sr = 48_000.0;
        let f0 = 1000.0;
        let mut b = peaking(sr, f0, 2.0, -3.0);
        let mut ein = 0.0f64;
        let mut eout = 0.0f64;
        for i in 0..48_000 {
            let x = (2.0 * PI * f0 * i as f64 / sr).sin() as f32;
            let y = b.process(x);
            ein += (x as f64).powi(2);
            eout += (y as f64).powi(2);
        }
        let db = 10.0 * (eout / ein).log10();
        assert!((db + 3.0).abs() < 0.2, "peaking -3dB 实得 {db:.2}dB");
    }
}
