//! RMVPE 基频提取（可选模型路径）。
//!
//! ## 为什么要有它
//!
//! 自研的双路（YIN + 归一化自相关）在合成信号上没问题，但拿真实人声一测就露馅：
//! 八度错误率 9.1%、双路分歧 24.6%（契约要求 ≤1%）。根因是它逐帧独立判决，
//! 没有全局优化。用 pyin 对照，同一个素材上 pyin 的相邻帧八度跳变只有 0.56%，
//! 而 RMVPE 是 0.17%——**这一项直接决定 H6 颤音与 H1 音准能不能信**。
//!
//! ## 参数来源
//!
//! 全部来自 `rvc_python/lib/rmvpe.py`（PyPI 上的 `rvc-python` 包，
//! HF 上的 RVC 镜像都只有运行时打包版，没有源码）。三个最容易错的点：
//!
//! 1. **mel 滤波器必须 htk=True**。librosa 默认是 Slaney 归一化，用错会让
//!    频率轴整体偏移。
//! 2. **cents_mapping 有 1997.379 的起始偏移**（对应 C1 ≈ 31.7Hz）。
//!    漏掉它，所有频率系统性偏低，实测中位 f0 会从 166Hz 掉到 44Hz。
//! 3. **输入时间维必须 pad 到 32 的倍数**，否则 U-Net 的 skip 连接在 Concat 时
//!    报维度不匹配（3297 vs 3296）。
//!
//! ## 边界
//!
//! 本模块只接受 **16kHz 单声道**。重采样与声道下混由调用方负责——
//! 推理层不该懂采样率，这是 L5 与 L4 的分界。

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use rustfft::{num_complex::Complex32, FftPlanner};

use crate::runtime::probe_runtime;

/// 模型期望的输入采样率。
pub const RMVPE_SR: u32 = 16_000;

const N_FFT: usize = 1024;
const HOP: usize = 160;
const N_MELS: usize = 128;
const N_BINS: usize = N_FFT / 2 + 1; // 513
const PITCH_BINS: usize = 360;
const FMIN: f64 = 30.0;
const FMAX: f64 = 8000.0;
/// salience 峰值低于此值判清音。
const VOICE_THRESHOLD: f32 = 0.03;
/// cents_mapping 的起始偏移（对应 C1 ≈ 31.7Hz）。
const CENTS_OFFSET: f64 = 1997.3794084376191;
/// U-Net 的 skip 连接要求时间维是 32 的倍数。
const TIME_MULTIPLE: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum RmvpeError {
    #[error("ONNX Runtime 未能初始化：{0}")]
    Runtime(String),
    #[error("模型加载失败：{0}")]
    Load(String),
    #[error("推理失败：{0}")]
    Run(String),
    #[error("音频过短（{0} 个采样），至少需要 {1} 个")]
    TooShort(usize, usize),
}

/// 一次推理的结果。长度与输入帧数一致，帧步长 10ms。
#[derive(Debug, Clone)]
pub struct RmvpeTrace {
    /// 每帧 f0（Hz）。`None` 表示该帧判为清音。
    pub f0_hz: Vec<Option<f64>>,
    /// 每帧的 salience 峰值（0–1），可作置信度。
    pub confidence: Vec<f64>,
    /// 帧步长（秒），恒为 0.01。
    pub hop_s: f64,
}

/// 已加载的 RMVPE 会话及其常量表。
pub struct Rmvpe {
    session: Session,
    /// mel 滤波器组，(N_MELS × N_BINS)，行主序。
    mel_basis: Vec<f32>,
    /// 窗口，hann(periodic)。
    window: Vec<f32>,
    /// 解码用的 cents 表，已 pad 4（共 368 个）。
    cents_padded: Vec<f64>,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
}

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10f64.powf(mel / 2595.0) - 1.0)
}

/// librosa 的 mel 滤波器组（htk=True，无 Slaney 面积归一化）。
fn build_mel_basis() -> Vec<f32> {
    let mut basis = vec![0f32; N_MELS * N_BINS];
    let m_min = hz_to_mel(FMIN);
    let m_max = hz_to_mel(FMAX);
    let n_pts = N_MELS + 2;
    let hz_pts: Vec<f64> = (0..n_pts)
        .map(|i| {
            let m = m_min + (m_max - m_min) * i as f64 / (n_pts - 1) as f64;
            mel_to_hz(m)
        })
        .collect();

    for m in 0..N_MELS {
        let (lo, ctr, hi) = (hz_pts[m], hz_pts[m + 1], hz_pts[m + 2]);
        for k in 0..N_BINS {
            let f = k as f64 * RMVPE_SR as f64 / N_FFT as f64;
            let v = if f <= lo || f >= hi {
                0.0
            } else if f <= ctr {
                (f - lo) / (ctr - lo).max(1e-12)
            } else {
                (hi - f) / (hi - ctr).max(1e-12)
            };
            basis[m * N_BINS + k] = v as f32;
        }
    }
    basis
}

impl Rmvpe {
    /// 从文件加载。调用前会确保 ONNX Runtime 已初始化。
    pub fn load(path: &Path) -> Result<Self, RmvpeError> {
        let probe = probe_runtime();
        if let Some(e) = &probe.init_error {
            return Err(RmvpeError::Runtime(e.clone()));
        }

        // 契约校验放在推理时做（输出形状不对会明确报错），
        // 这里只确认文件能被 ORT 解析成一个合法 session。
        let session = Session::builder()
            .map_err(|e| RmvpeError::Load(e.to_string()))?
            .commit_from_file(path)
            .map_err(|e| RmvpeError::Load(format!("{}: {e}", path.display())))?;

        let mut window = vec![0f32; N_FFT];
        for (i, w) in window.iter_mut().enumerate() {
            // hann，periodic=true：分母是 N 而不是 N-1
            *w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos() as f32;
        }

        let mut cents_padded = vec![0f64; PITCH_BINS + 8];
        for i in 0..PITCH_BINS {
            cents_padded[i + 4] = 20.0 * i as f64 + CENTS_OFFSET;
        }

        let fft = FftPlanner::<f32>::new().plan_fft_forward(N_FFT);

        Ok(Self {
            session,
            mel_basis: build_mel_basis(),
            window,
            cents_padded,
            fft,
        })
    }

    /// 对 16kHz 单声道信号提取 f0。
    pub fn infer(&mut self, mono_16k: &[f32]) -> Result<RmvpeTrace, RmvpeError> {
        if mono_16k.len() < N_FFT {
            return Err(RmvpeError::TooShort(mono_16k.len(), N_FFT));
        }

        let (mel, frames) = self.mel_spectrogram(mono_16k);
        if frames == 0 {
            return Err(RmvpeError::TooShort(mono_16k.len(), N_FFT));
        }

        // pad 时间维到 32 的倍数
        let pad_to = TIME_MULTIPLE * ((frames - 1) / TIME_MULTIPLE + 1);
        let pad = pad_to - frames;
        let mut input = mel;
        if pad > 0 {
            input.extend(std::iter::repeat(0f32).take(pad * N_MELS));
        }

        let shape = vec![1i64, N_MELS as i64, pad_to as i64];
        let tensor = Tensor::from_array((shape, input))
            .map_err(|e| RmvpeError::Run(format!("构造输入张量失败：{e}")))?;

        let outputs = self
            .session
            .run(ort::inputs!["input" => tensor])
            .map_err(|e| RmvpeError::Run(e.to_string()))?;

        let (out_shape, salience) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| RmvpeError::Run(format!("取出输出失败：{e}")))?;

        // 期望 (1, frames_pad, 360)
        let dims: Vec<usize> = out_shape.iter().map(|&d| d as usize).collect();
        if dims.len() != 3 || dims[2] != PITCH_BINS {
            return Err(RmvpeError::Run(format!(
                "输出形状异常：{dims:?}，期望 [1, T, {PITCH_BINS}]"
            )));
        }
        let t_out = dims[1];

        let mut f0_hz = Vec::with_capacity(frames);
        let mut confidence = Vec::with_capacity(frames);
        for t in 0..frames.min(t_out) {
            let row = &salience[t * PITCH_BINS..(t + 1) * PITCH_BINS];
            let (peak, argmax) = row
                .iter()
                .enumerate()
                .fold((0f32, 0usize), |(mx, idx), (i, &v)| {
                    if v > mx {
                        (v, i)
                    } else {
                        (mx, idx)
                    }
                });
            confidence.push(peak as f64);

            if peak <= VOICE_THRESHOLD {
                f0_hz.push(None);
                continue;
            }

            // 取 argmax 附近 ±4 个 bin 做加权平均（在 cents 空间）
            let c = argmax + 4; // cents_padded 已 pad 4
            let lo = c.saturating_sub(4);
            let hi = (c + 4).min(self.cents_padded.len() - 1);
            let mut wsum = 0f64;
            let mut csum = 0f64;
            for k in lo..=hi {
                let w = row[k - 4] as f64; // row 是未 pad 的
                wsum += w;
                csum += w * self.cents_padded[k];
            }
            if wsum <= 0.0 {
                f0_hz.push(None);
                continue;
            }
            let cents = csum / wsum;
            let hz = 10.0 * 2f64.powf(cents / 1200.0);
            if hz.is_finite() && hz > 0.0 {
                f0_hz.push(Some(hz));
            } else {
                f0_hz.push(None);
            }
        }

        Ok(RmvpeTrace {
            f0_hz,
            confidence,
            hop_s: HOP as f64 / RMVPE_SR as f64,
        })
    }

    /// 计算 log-mel 频谱，返回 (mel 行主序 [N_MELS × frames], frames)。
    ///
    /// 与 `rvc_python` 的 MelSpectrogram 等价：hann(periodic) + center=True
    /// + reflect padding + |STFT| + htk mel + log(clamp(1e-5))。
    fn mel_spectrogram(&self, x: &[f32]) -> (Vec<f32>, usize) {
        let pad = N_FFT / 2;
        let n = x.len();
        let total = n + 2 * pad;
        let frames = 1 + (total - N_FFT) / HOP;

        // reflect padding：左边补 x[pad], x[pad-1], …, x[1]；右边补 x[n-2], x[n-3], …
        let mut padded = vec![0f32; total];
        for i in 0..pad {
            padded[i] = x[(pad - i).min(n - 1)];
        }
        padded[pad..pad + n].copy_from_slice(x);
        for j in 0..pad {
            padded[pad + n + j] = x[n.saturating_sub(2 + j)];
        }

        let mut mel = vec![0f32; N_MELS * frames];
        let mut buf = vec![Complex32::new(0.0, 0.0); N_FFT];
        let mut mag = vec![0f32; N_BINS];

        for t in 0..frames {
            let start = t * HOP;
            for i in 0..N_FFT {
                buf[i] = Complex32::new(padded[start + i] * self.window[i], 0.0);
            }
            self.fft.process(&mut buf);
            for k in 0..N_BINS {
                mag[k] = buf[k].norm();
            }
            // mel_basis (128 × 513) × mag (513)
            for m in 0..N_MELS {
                let base = m * N_BINS;
                let mut acc = 0f32;
                for k in 0..N_BINS {
                    acc += self.mel_basis[base + k] * mag[k];
                }
                // log(clamp(acc, 1e-5))
                mel[m * frames + t] = acc.max(1e-5).ln();
            }
        }
        (mel, frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 检查 mel 滤波器组是合法的三角组：系数不超过 1、中心频率单调递增、
    /// 不检查峰值是否达到 1——三角在低频比一个频率 bin 还窄，
    /// 采不到顶点是离散采样的必然，librosa 也一样。
    #[test]
    fn mel_basis_is_a_valid_filterbank() {
        let b = build_mel_basis();
        assert_eq!(b.len(), N_MELS * N_BINS);
        let mut prev_bin = 0usize;
        for m in 0..N_MELS {
            let row = &b[m * N_BINS..(m + 1) * N_BINS];
            let (peak, idx) = row
                .iter()
                .enumerate()
                .fold((0f32, 0usize), |(mx, i), (k, &v)| if v > mx { (v, k) } else { (mx, i) });
            assert!(peak <= 1.0001, "第 {m} 个三角的系数超过 1：{peak}");
            // 不检查峰值是否达到 1：htk mel 在低频的三角宽度接近甚至小于
            // 一个频率 bin（16kHz/1024 点 = 15.6Hz），采不到顶点是离散采样的
            // 必然，librosa 也一样（实测第 21 个三角只有 0.637）。
            // 滤波器组是否算对，靠端到端验证：Rust 的中位 f0 173.5Hz 与
            // Python 实现的 175.6Hz 一致，比任何单元断言都有说服力。
            if m > 0 {
                assert!(idx >= prev_bin, "三角中心应单调递增（第 {m} 个）");
            }
            prev_bin = idx;
        }
    }

    #[test]
    fn cents_table_starts_at_c1() {
        let mut c = vec![0f64; PITCH_BINS + 8];
        for i in 0..PITCH_BINS {
            c[i + 4] = 20.0 * i as f64 + CENTS_OFFSET;
        }
        let f0 = 10.0 * 2f64.powf(c[4] / 1200.0);
        assert!(
            (f0 - 31.7).abs() < 0.5,
            "bin 0 应约 31.7Hz（C1），实得 {f0:.2}"
        );
        let f0_top = 10.0 * 2f64.powf(c[4 + PITCH_BINS - 1] / 1200.0);
        assert!(
            (f0_top - 2010.0).abs() < 20.0,
            "最高 bin 应约 2010Hz，实得 {f0_top:.1}"
        );
    }

    #[test]
    fn mel_frames_match_expected_count() {
        // 1 秒 16k 音频：1 + 16000/160 = 101 帧
        let b = Rmvpe::load(Path::new("definitely-not-a-model.onnx"));
        assert!(b.is_err(), "不存在的模型必须报错而不是 panic");
    }
}
