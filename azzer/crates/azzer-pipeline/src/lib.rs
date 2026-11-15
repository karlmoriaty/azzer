//! DAG 节点实现层（R1：N01 ingest + N02 grade）。
//!
//! 这一层把 `azzer-dsp` 的纯函数包成符合五要素契约的节点：
//! 输入 / 输出 / 契约断言 / 失败回退 / 幂等键。
//!
//! **零模型运行**：`PipelineContext` 加载用户可选的 `models.json`，
//! 文件不存在就是空注册表；R1 全程使用 `NoEngine`。这直接实测了
//! v3.2 §5.1 的「模型不绑定」——没有模型，A 入口照常跑通。

pub mod cache;
pub mod nodes;

use std::path::{Path, PathBuf};

use azzer_core::{
    Capabilities, ContentStore, EngineDescriptor, ModelRegistry, NoEngine, RegistryError,
    StoreError,
};
use serde::{Deserialize, Serialize};

pub use nodes::{
    n01_ingest, n02_grade, n05_f0extract, n09_repair, n15_master, n16_eval, N01Result,
    N02Result, N05Result, N09Result, N15Result, N16Result,
};

/// f0 的摘要（完整逐帧数据太大，只在报告里放摘要，全量存内容寻址存储）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct F0Summary {
    pub total_frames: usize,
    pub voiced_frames: usize,
    pub voiced_ratio: f64,
    pub route_disagreement_ratio: f64,
    pub meets_route_contract: bool,
    pub median_f0_hz: Option<f64>,
    pub min_f0_hz: Option<f64>,
    pub max_f0_hz: Option<f64>,
}

impl F0Summary {
    pub fn from_track(t: &azzer_dsp::F0Track) -> Self {
        let mut v: Vec<f64> = t.f0_hz.iter().flatten().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = if v.is_empty() { None } else { Some(v[v.len() / 2]) };
        Self {
            total_frames: t.total_frames,
            voiced_frames: t.voiced_frames,
            voiced_ratio: if t.total_frames > 0 {
                t.voiced_frames as f64 / t.total_frames as f64
            } else {
                0.0
            },
            route_disagreement_ratio: t.route_disagreement_ratio,
            meets_route_contract: t.meets_route_contract,
            median_f0_hz: median,
            min_f0_hz: v.first().copied(),
            max_f0_hz: v.last().copied(),
        }
    }
}

/// A 入口（同音色直通）的完整报告。
///
/// A 入口的定义（v3 §5）：用户唱自己的歌，源音色已接近目标，
/// **跳过分离与音色转换**，只做修复 + 拟真化 + 母带。
/// 当前链上是 N01 → N02 → N09 → N05 → N15 → N16。
///
/// 只做序列化不反序列化：N16 的评估项带 `&'static str` 原因文本，
/// 且报告是末端产物，只写不读。
#[derive(Debug, Clone, Serialize)]
pub struct AEntryReport {
    pub source: String,
    pub root: String,
    pub n01_key: String,
    pub n02_key: String,
    pub n09_key: String,
    pub n05_key: String,
    pub n15_key: String,
    pub n16_key: String,
    pub n01_cache_hit: bool,
    pub n02_computed: bool,
    pub n09_computed: bool,
    pub n05_computed: bool,
    pub n15_computed: bool,
    pub grade: azzer_dsp::GradeMetrics,
    /// N09 修复的执行记录（前三步）。
    pub repair: azzer_dsp::RepairReport,
    /// N05 基频提取摘要。
    pub f0: F0Summary,
    /// N16 的 37 项评估（一项不少，缺项标 not_applicable）。
    pub eval: azzer_dsp::EvalReport,
    /// 原始源（进入前五步之前）的测量值，供外部交叉验证比对。
    pub source_lufs: Option<f64>,
    pub source_dbtp: f64,
    pub master: azzer_dsp::MasterReport,
    /// 母带前五步的执行记录（v3 §5.6 的完整七步链）。
    pub chain: azzer_dsp::ChainReport,
    pub master_blob: String,
    pub capabilities: Capabilities,
    pub models_declared: usize,
    pub path_note: String,
    /// 端到端耗时（毫秒）。M1 验收线是 4 分钟源 ≤12 分钟。
    pub elapsed_ms: u128,
}

/// 跑 A 入口闭环：N01 → N02 → N15。
pub fn run_a_entry(
    ctx: &PipelineContext,
    source: impl AsRef<Path>,
    cfg: &azzer_dsp::MasterConfig,
) -> Result<AEntryReport, NodeError> {
    use std::time::Instant;
    let t0 = Instant::now();
    let source = source.as_ref();

    let n01 = n01_ingest(ctx, source)?;
    let n02 = n02_grade(ctx, &n01)?;
    let source_loudness = azzer_dsp::measure_loudness(&n01.buffer)?;
    let source_dbtp = azzer_dsp::true_peak_dbtp(&n01.buffer);
    // 去混响是否触发由 N02 测出的 RT60 决定（v3 §5.3 的硬要求）
    let n09 = n09_repair(
        ctx,
        &n01,
        n02.metrics.rt60_s,
        &azzer_dsp::RepairConfig::default(),
    )?;
    let n15 = n15_master(
        ctx,
        &n09.buffer,
        &n09.repaired_blob.digest,
        cfg,
        &azzer_dsp::ChainConfig::default(),
    )?;
    // N05 从修复后的人声取 f0（A 入口未分离，伴奏会干扰，报告里如实标注）
    let n05 = n05_f0extract(
        ctx,
        &n09.repaired_blob.digest,
        &n09.buffer,
        &azzer_dsp::F0Config::default(),
    )?;

    let elapsed_ms = t0.elapsed().as_millis();
    let n16 = n16_eval(
        ctx,
        &n09.buffer,
        &n15.buffer,
        &n02.metrics,
        &n15.report.output,
        n15.report.output_dbtp,
        &n05.track,
        elapsed_ms,
        n01.cache_hit && n02.cache_hit && n09.cache_hit,
        &n15.master_blob.digest,
    )?;

    let caps = ctx.capabilities();
    let mut note = if caps.separate {
        "已声明可用的分离模型，但 A 入口按定义仍跳过分离与转换。".to_string()
    } else {
        "未声明可用的分离模型（正常状态），A 入口同音色直通：跳过分离与音色转换，未触发任何模型下载。".to_string()
    };
    note.push_str(&format!(
        "修复链为谱减去噪 → {} → 高频重建；基频提取 {} 帧（浊音 {:.0}%，双路不一致率 {:.2}%{}）。拟真化（N11）尚未接入。",
        if n09.report.de_reverb_applied {
            "去混响"
        } else {
            "去混响已跳过（RT60 未超阈值）"
        },
        n05.track.total_frames,
        F0Summary::from_track(&n05.track).voiced_ratio * 100.0,
        n05.track.route_disagreement_ratio * 100.0,
        if n05.track.meets_route_contract {
            ""
        } else {
            "，超过 1% 契约"
        }
    ));

    Ok(AEntryReport {
        source: source.display().to_string(),
        root: ctx.root.display().to_string(),
        n01_key: n01.outcome.key.hex(),
        n02_key: n02.outcome.key.hex(),
        n09_key: n09.outcome.key.hex(),
        n05_key: n05.outcome.key.hex(),
        n15_key: n15.outcome.key.hex(),
        n16_key: n16.outcome.key.hex(),
        n01_cache_hit: n01.cache_hit,
        n02_computed: n02.computed,
        n09_computed: n09.computed,
        n05_computed: n05.computed,
        n15_computed: n15.computed,
        grade: n02.metrics,
        repair: n09.report,
        f0: F0Summary::from_track(&n05.track),
        eval: n16.report,
        source_lufs: source_loudness.integrated_lufs,
        source_dbtp,
        master: n15.report,
        chain: n15.chain,
        master_blob: n15.master_blob.digest,
        capabilities: caps,
        models_declared: ctx.models.models().len(),
        path_note: note,
        elapsed_ms,
    })
}

/// 流水线运行的根上下文。根目录由调用方给定，本层不硬编码任何绝对路径。
#[derive(Debug, Clone)]
pub struct PipelineContext {
    pub root: PathBuf,
    pub store: ContentStore,
    pub models: ModelRegistry,
    pub engine: EngineDescriptor,
}

impl PipelineContext {
    /// 打开一个运行根目录。**不创建 `models.json`**——声明文件由用户自己维护。
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, NodeError> {
        let root = root.into();
        let store = ContentStore::new(root.join(".assets"));
        let models = ModelRegistry::load(root.join("models.json"))?;
        let engine = EngineDescriptor::of(&NoEngine);
        std::fs::create_dir_all(&root)
            .map_err(|e| NodeError::Io(root.clone(), e))?;
        Ok(Self { root, store, models, engine })
    }

    /// 参数快照，参与幂等键。改这里的任何值都应让下游节点失效。
    pub fn params(&self) -> &'static [u8] {
        br#"{"internal_sample_rate":48000,"wav_bits":24,"clip_threshold":0.999}"#
    }

    pub fn capabilities(&self) -> Capabilities {
        self.models.capabilities()
    }

    pub fn models_json_path(&self) -> PathBuf {
        self.root.join("models.json")
    }
}

/// R1 的可执行报告，直接序列化成 JSON 交给前端或 CLI。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R1Report {
    pub source: String,
    pub root: String,
    /// n01 产物是否命中已有 CAS 内容（幂等验证点）。
    pub n01_cache_hit: bool,
    pub n02_cache_hit: bool,
    /// N02 是否真的执行了五项计算。false = 命中执行记录，直接复用上次结论。
    pub n02_computed: bool,
    pub n01_key: String,
    pub n02_key: String,
    pub normalized_blob: String,
    pub grade_blob: String,
    pub grade: azzer_dsp::GradeMetrics,
    pub capabilities: Capabilities,
    pub models_declared: usize,
    /// 无模型时的可读说明，明确告诉用户当前走的是哪条路径。
    pub path_note: String,
}

/// 串起 N01 → N02，产出 R1 报告。
pub fn run_r1(ctx: &PipelineContext, source: impl AsRef<Path>) -> Result<R1Report, NodeError> {
    let source = source.as_ref();
    let n01 = n01_ingest(ctx, source)?;
    let n02 = n02_grade(ctx, &n01)?;

    let caps = ctx.capabilities();
    let path_note = if caps.separate {
        "已声明可用的分离模型，可进入 B 入口；R1 仍只跑 A 入口链路。".into()
    } else {
        "未声明可用的分离模型（正常状态），使用 A 入口同音色直通路径；未触发任何模型下载。".into()
    };

    Ok(R1Report {
        source: source.display().to_string(),
        root: ctx.root.display().to_string(),
        n01_cache_hit: n01.cache_hit,
        n02_cache_hit: n02.cache_hit,
        n02_computed: n02.computed,
        n01_key: n01.outcome.key.hex(),
        n02_key: n02.outcome.key.hex(),
        normalized_blob: n01.normalized.digest,
        grade_blob: n02.grade_blob.digest,
        grade: n02.metrics,
        capabilities: caps,
        models_declared: ctx.models.models().len(),
        path_note,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("文件 IO 失败：{0}：{1}")]
    Io(PathBuf, std::io::Error),
    #[error("音频解码失败：{0}")]
    Dsp(#[from] azzer_dsp::DspError),
    #[error("内容寻址存储失败：{0}")]
    Store(#[from] StoreError),
    #[error("模型声明读取失败：{0}")]
    Registry(#[from] RegistryError),
    #[error("模型推理失败：{0}")]
    Model(String),
    #[error("JSON 序列化失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("{node} 契约断言未通过：{reason}。下一步：{next}")]
    Contract {
        node: azzer_core::NodeId,
        reason: String,
        next: String,
    },
}

/// 从契约的失败回退里取出面向用户的下一步说明。
pub fn fallback_text(f: &azzer_core::Fallback) -> String {
    use azzer_core::Fallback;
    match f {
        Fallback::Reject(s)
        | Fallback::Continue(s)
        | Fallback::Degrade(s) => (*s).to_string(),
        Fallback::Retry { hint, .. } => (*hint).to_string(),
    }
}
