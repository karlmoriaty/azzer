//! 短时傅里叶变换（STFT）与重叠相加（OLA）重建。
//!
//! 母带第 1–5 步全是时域滤波，用不着它；N09 修复（谱减去噪、去混响、
//! 高频重建）必须在频域做，所以这里补上基础件。
//!
//! ## 参数与重建精度
//!
//! | 项 | 值 | 理由 |
//! |---|---|---|
//! | 帧长 | 2048（42.7ms @48k） | 频率分辨率 23.4Hz，够分辨基频与低次谐波 |
//! | 跳距 | 512（75% 重叠） | Hann 窗在 75% 重叠下满足 COLA |
//! | 窗 | Hann | 旁瓣衰减快，重建误差小 |
//!
//! **不做任何处理时，重建必须与原信号几乎一致**——这条是硬指标，
//! 有测试守着（误差 <1e-5）。频域改写一旦不可逆，后面所有修复都失去意义。

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

#[derive(Debug, Clone, Copy)]
pub struct StftConfig {
    pub frame: usize,
    pub hop: usize,
}

impl Default for StftConfig {
    fn default() -> Self {
        Self {
            frame: 2048,
            hop: 512,
        }
    }
}

pub struct Stft {
    fft: Arc<dyn Fft<f64>>,
    ifft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    pub cfg: StftConfig,
}

impl Stft {
    pub fn new(cfg: StftConfig) -> Self {
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(cfg.frame);
        let ifft = planner.plan_fft_inverse(cfg.frame);
        let window: Vec<f64> = (0..cfg.frame)
            .map(|i| {
                let x = std::f64::consts::PI * i as f64 / cfg.frame as f64;
                x.sin().powi(2)
            })
            .collect();
        Self {
            fft,
            ifft,
            window,
            cfg,
        }
    }

    pub fn bins(&self) -> usize {
        self.cfg.frame / 2 + 1
    }

    /// 逐帧处理：回调拿到一帧的**完整复数谱**（长度 = frame），可以就地改写。
    ///
    /// 逐帧而不是先存所有帧，是为了不让 4 分钟音频的频谱把内存撑爆
    /// （5630 帧 × 2048 bin × 16 字节 ≈ 184MB）。
    pub fn process<F>(&self, x: &[f32], mut f: F) -> Vec<f32>
    where
        F: FnMut(&mut Vec<Complex<f64>>, usize),
    {
        let n = x.len();
        if n == 0 {
            return Vec::new();
        }
        let frame = self.cfg.frame;
        let hop = self.cfg.hop;

        // 两端各补一个帧长的零。
        //
        // 不补零的话首尾只有 1 帧覆盖，窗平方和从 0 开始增长：Hann 窗第一个
        // 样本 w=0、第二个 w²≈5.5e-12，用它做归一化除数会把浮点误差放大成
        // 几百倍的尖峰（实测修复后峰值 700，正常应 <1）。
        let pad = frame;
        let total = n + 2 * pad;
        let mut padded = vec![0.0f32; total];
        padded[pad..pad + n].copy_from_slice(x);

        let frames = if total <= frame {
            1
        } else {
            (total - frame).div_ceil(hop) + 1
        };
        let ftotal = (frames - 1) * hop + frame;
        let mut out = vec![0.0f64; ftotal];
        let mut wsum = vec![0.0f64; ftotal];
        let mut buf = vec![Complex::new(0.0, 0.0); frame];

        for k in 0..frames {
            let start = k * hop;
            for i in 0..frame {
                let s = padded.get(start + i).copied().unwrap_or(0.0) as f64;
                buf[i] = Complex::new(s * self.window[i], 0.0);
            }
            self.fft.process(&mut buf);
            f(&mut buf, k);

            // 强制恢复共轭对称：实信号的频谱必须满足 X[N-k] = conj(X[k])，
            // 否则逆变换出来的不是实信号。调用方通常只改前 bins 个（正频率），
            // 负频率那一半会与正频率脱钩。
            for i in 1..frame / 2 {
                buf[frame - i] = buf[i].conj();
            }
            buf[0] = Complex::new(buf[0].re, 0.0);
            buf[frame / 2] = Complex::new(buf[frame / 2].re, 0.0);

            self.ifft.process(&mut buf);
            // rustfft 的逆变换不归一化，这里补 1/N
            let scale = 1.0 / frame as f64;
            for i in 0..frame {
                let w = self.window[i];
                out[start + i] += buf[i].re * scale * w;
                wsum[start + i] += w * w;
            }
        }

        (pad..pad + n)
            .map(|i| {
                let w = wsum.get(i).copied().unwrap_or(0.0);
                if w > 1e-6 {
                    (out[i] / w) as f32
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// 只做正变换、不做逆变换，用于分析（例如测某频段能量）。
    pub fn magnitude_spectrum(&self, x: &[f32]) -> Vec<Vec<f64>> {
        let frame = self.cfg.frame;
        let hop = self.cfg.hop;
        let n = x.len();
        let frames = if n <= frame {
            1
        } else {
            (n - frame).div_ceil(hop) + 1
        };
        let mut out = Vec::with_capacity(frames);
        let mut buf = vec![Complex::new(0.0, 0.0); frame];
        for k in 0..frames {
            let start = k * hop;
            for i in 0..frame {
                let s = if start + i < n { x[start + i] as f64 } else { 0.0 };
                buf[i] = Complex::new(s * self.window[i], 0.0);
            }
            self.fft.process(&mut buf);
            out.push(buf.iter().take(self.bins()).map(|c| c.norm()).collect());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(freq: f64, amp: f64, secs: f64) -> Vec<f32> {
        let sr = 48_000.0;
        let n = (sr * secs) as usize;
        (0..n)
            .map(|i| ((2.0 * PI * freq * i as f64 / sr).sin() * amp) as f32)
            .collect()
    }

    /// 不改写任何频谱时，STFT→ISTFT 必须几乎完美重建。
    /// 这是所有频域修复的前提：链路本身不能有损。
    #[test]
    fn perfect_reconstruction_without_processing() {
        let x = sine(440.0, 0.6, 0.5);
        let stft = Stft::new(StftConfig::default());
        let y = stft.process(&x, |_spec, _k| {});
        assert_eq!(y.len(), x.len());
        let max_err = x
            .iter()
            .zip(y.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-5, "重建误差应 <1e-5，实得 {max_err:.3e}");
    }

    /// 频域整体乘以 0 应得到静音（验证改写路径确实生效）。
    #[test]
    fn zeroing_spectrum_yields_silence() {
        let x = sine(440.0, 0.6, 0.2);
        let stft = Stft::new(StftConfig::default());
        let y = stft.process(&x, |spec, _| {
            for c in spec.iter_mut() {
                *c = Complex::new(0.0, 0.0);
            }
        });
        let peak = y.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak < 1e-6, "应得到静音，实得峰值 {peak}");
    }

    /// 幅度谱的峰值 bin 应对应输入频率。
    #[test]
    fn magnitude_spectrum_peak_matches_frequency() {
        let sr = 48_000.0;
        let freq = 1000.0;
        let x = sine(freq, 1.0, 0.2);
        let stft = Stft::new(StftConfig::default());
        let spec = stft.magnitude_spectrum(&x);
        // 取中间一帧，避开边界
        let frame = &spec[spec.len() / 2];
        let peak_bin = frame
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        let bin_hz = sr / StftConfig::default().frame as f64;
        let peak_hz = peak_bin as f64 * bin_hz;
        assert!(
            (peak_hz - freq).abs() < bin_hz * 2.0,
            "峰值应在 {freq}Hz 附近，实得 {peak_hz:.1}Hz"
        );
    }

    /// 只改**半个**频谱也必须能正确重建。
    ///
    /// 这条测试专门守住共轭对称：早期版本只处理正频率 bins，负频率没同步，
    /// 逆变换结果不是实信号，端到端峰值从 0.6 涨到 350。
    #[test]
    fn half_spectrum_edit_stays_real() {
        let x = sine(1000.0, 0.5, 0.3);
        let stft = Stft::new(StftConfig::default());
        // 把 1kHz 附近的 bin 幅度减半，其余不动
        let y = stft.process(&x, |spec, _| {
            for i in 40..48 {
                spec[i] = spec[i] * 0.5;
            }
        });
        let peak = y.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            peak < 1.0,
            "只改半个频谱后峰值不应爆炸，实得 {peak}"
        );
        assert!(y.iter().all(|v| v.is_finite()));
    }

    /// 长信号也必须完美重建。单测原来只测 0.5 秒，
    /// 而端到端是 240 秒，差别可能就在长度上。
    #[test]
    fn long_signal_reconstructs() {
        let x = sine(1000.0, 0.5, 10.0);
        let stft = Stft::new(StftConfig::default());
        let y = stft.process(&x, |_, _| {});
        assert_eq!(y.len(), x.len(), "长度应一致");
        let peak = y.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak < 1.0, "10 秒信号重建峰值不应爆炸，实得 {peak}");
        let max_err = x
            .iter()
            .zip(y.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-4, "10 秒重建误差应 <1e-4，实得 {max_err:.3e}");
    }

    #[test]
    fn short_input_does_not_panic() {
        let stft = Stft::new(StftConfig::default());
        assert!(stft.process(&[], |_, _| {}).is_empty());
        let y = stft.process(&[0.5; 100], |_, _| {});
        assert_eq!(y.len(), 100);
    }
}
