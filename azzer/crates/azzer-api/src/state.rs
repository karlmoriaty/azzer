//! 应用状态与任务表。
//!
//! **2026-11-09 审计修复**：此前的 `AppState` 只有 token 与启动时刻，
//! API 层不持有 pipeline，导致 N01–N16 的能力完全无法通过 HTTP 触达。
//! 现在 `AppState` 持有 `PipelineContext`（克隆成本低：内部是路径与 Vec），
//! 任务表用 `Arc<Mutex<>>` 共享，处理在 tokio 任务里异步跑。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use azzer_pipeline::{AEntryReport, PipelineContext};
use serde::Serialize;

/// 一次处理任务的状态。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Job {
    Running,
    Done {
        /// A 入口完整报告（含 37 项评估）。
        report: Box<AEntryReport>,
        /// 成品文件名，供 `/master.wav` 端点定位。
        master_file: String,
    },
    Failed {
        /// 面向用户的失败原因，必须带下一步。
        message: String,
    },
}

#[derive(Clone)]
pub struct AppState {
    /// 一次性访问令牌。除 `/health` 外所有请求必须携带。
    pub token: String,
    pub started_at: Instant,
    pub version: &'static str,
    /// 流水线上下文（内容寻址存储 + 模型声明 + 引擎描述）。
    pub pipeline: PipelineContext,
    /// 任务表。进程内内存，重启即清空——R1 阶段没有持久化需求。
    pub jobs: Arc<Mutex<HashMap<String, Job>>>,
}

impl AppState {
    /// 生成随机 token。32 字节 → 64 位十六进制。
    ///
    /// 用 `getrandom` 直接取系统 CSPRNG；不引入 `rand` 是为了少一层依赖。
    fn new_token() -> String {
        let mut buf = [0u8; 32];
        if getrandom::fill(&mut buf).is_err() {
            // 理论上不可能失败；退化为时间派生值，并在日志里告警。
            // 不 panic：服务起不来比 token 强度略低更糟。
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            for (i, b) in buf.iter_mut().enumerate() {
                *b = ((nanos >> (i % 16 * 8)) & 0xff) as u8 ^ (i as u8);
            }
            tracing::warn!("getrandom 失败，token 退化为时间派生值");
        }
        buf.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 打开一个运行根目录。产物全部落在这里，不写系统临时目录。
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ServeError> {
        let root = root.into();
        let pipeline = PipelineContext::open(root).map_err(|e| ServeError::Pipeline(e.to_string()))?;
        Ok(Self {
            token: Self::new_token(),
            started_at: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
            pipeline,
            jobs: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 从环境变量或默认位置打开：`AZZER_DATA_DIR` → `<cwd>/.cache/api`。
    pub fn from_env() -> Result<Self, ServeError> {
        let root = std::env::var("AZZER_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(".cache")
                    .join("api")
            });
        Self::open(root)
    }
}

/// 绑定成功后的服务句柄。
pub struct BoundServer {
    pub port: u16,
    pub token: String,
    pub handle: tokio::task::JoinHandle<()>,
}

/// 启动失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("绑定本地端口失败：{0}")]
    Bind(std::io::Error),
    #[error("流水线根目录不可用：{0}")]
    Pipeline(String),
}
