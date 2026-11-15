//! 音频 DSP 纯函数层。
//!
//! 不依赖任何模型、引擎或网络。只做这些事：
//! WAV 解码/编码、采样率归一化、输入质检五项、响度测量、真峰值、母带限幅与归一。
//!
//! 模型不绑定约束下，这一层是**无模型路径的主体**：A 入口（同音色直通）
//! 从 N01 到 N15 全部由它承担，不需要任何外部资产。

pub mod biquad;
pub mod buffer;
pub mod chain;
pub mod eval;
pub mod f0;
pub mod loudness;
pub mod master;
pub mod metrics;
pub mod percept;
pub mod repair;
pub mod resample;
pub mod stft;
pub mod truepeak;

pub use biquad::{highpass, lowpass, peaking, Biquad, KWeighting, LinkwitzRiley4};
pub use chain::{apply_front_five, ChainConfig, ChainReport, CompParams};
pub use eval::{
    evaluate, in_range, EvalInput, EvalItem, EvalReport, MetricValue, EXPECTED_METRIC_COUNT,
};
pub use f0::{extract_f0, F0Config, F0Track};
pub use buffer::{
    decode_wav, decode_wav_bytes, encode_wav_24_bytes, write_wav_24, AudioBuffer, DspError,
};
pub use loudness::{measure as measure_loudness, LoudnessReport};
pub use master::{master, MasterConfig, MasterReport};
pub use metrics::{analyze, clip_ratio, peak_amplitude, peak_dbfs, Grade, GradeMetrics};
pub use percept::{
    detect_breaths, detect_onsets, detect_vibrato, BreathInfo, OnsetInfo, PerceptConfig,
    VibratoInfo,
};
pub use repair::{repair, RepairConfig, RepairReport};
pub use resample::{resample_linear, to_internal_rate, INTERNAL_SAMPLE_RATE};
pub use stft::{Stft, StftConfig};
pub use truepeak::{sample_peak_dbfs, true_peak_dbtp};
