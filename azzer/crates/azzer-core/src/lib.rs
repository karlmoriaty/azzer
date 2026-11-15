//! Azzer 核心纯逻辑与契约层。
//!
//! 这里不依赖音频库、推理 SDK、数据库驱动或网络。放的是全项目共用的
//! 「形状」：指标、DAG 契约、内容寻址、模型声明、引擎协议。
//!
//! 唯一涉及文件系统的模块是 [`asset`] 与 [`model`]，且都**不硬编码路径**——
//! 根目录由上层传入，保证所有产物都能落在工作目录内。
//!
//! 模型不绑定（v3.2 §5.1）：本 crate 不含任何模型权重、不假设模型存在、
//! 不下载、不托管；零模型时 A 入口照常可用。

pub mod asset;
pub mod dag;
pub mod engine;
pub mod humanize;
pub mod model;

pub use asset::{BlobRef, ContentStore, StoreError};
pub use dag::{
    spec, Fallback, IdempotencyKey, NodeId, NodeOutcome, NodeSpec, NodeStatus, NODE_SPECS,
};
pub use engine::{Engine, EngineDescriptor, NoEngine};
pub use humanize::{in_range, HumanizeInput, HumanizeReport, HumanizeScore};
pub use model::{
    Capabilities, ModelManifest, ModelRef, ModelRegistry, ModelRole, ModelStatus, RegistryError,
};

/// 指标计算的统一错误。
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("输入缺失：{0}")]
    Missing(&'static str),
    #[error("数值非法：{field} = {value}")]
    Invalid { field: &'static str, value: f64 },
}
