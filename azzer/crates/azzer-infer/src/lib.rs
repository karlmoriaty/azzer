//! 推理引擎适配层。
//!
//! 设计目标（见 v3.2 §1）：本地 ONNX 与远程 sidecar 实现同一个 `Engine` trait，
//! 让调度层不区分两者——"上云只改 URL"。
//!
//! 当前状态：
//! - `runtime`：运行时探测与 EP 选择（CPU / CUDA）
//! - `rmvpe`：RMVPE 基频提取，可选模型路径。没声明模型时上层走自研双路。

pub mod rmvpe;
pub mod runtime;

pub use rmvpe::{Rmvpe, RmvpeError, RmvpeTrace, RMVPE_SR};
pub use runtime::{cuda_available, probe_runtime, runtime_ready, EpReport, RuntimeProbe};
