//! 16 节点 DAG 的契约骨架。
//!
//! 每个节点必须声明五件事：**输入 / 输出 / 契约断言 / 失败回退 / 幂等键**。
//! 前四件放进 [`NodeSpec`]，第五件由 [`IdempotencyKey::derive`] 统一计算。
//!
//! 这里只放契约，不放实现——实现在 `azzer-pipeline`。这样 L3（计划层）
//! 不需要知道采样率、模型名、引擎 SDK，符合六层架构的分工。
//!
//! 节点定义来自 `docs/Azzer-design-plan-v3.md` §3.3。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 节点编号。用枚举而不是字符串，避免拼写错误漏到运行期。
macro_rules! node_ids {
    ($($id:ident => $s:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum NodeId { $($id),+ }

        impl NodeId {
            pub const ALL: &'static [NodeId] = &[$(NodeId::$id),+];

            pub fn as_str(self) -> &'static str {
                match self { $(NodeId::$id => $s),+ }
            }

            pub fn from_str_opt(s: &str) -> Option<Self> {
                match s { $($s => Some(NodeId::$id)),+, _ => None }
            }

            /// 1-based 序号，便于报告里显示 N01…N16。
            pub fn ordinal(self) -> usize {
                match self { $(NodeId::$id => stringify!($id)[1..].parse::<usize>().unwrap_or(0)),+ }
            }
        }
    };
}

node_ids! {
    N01 => "N01", N02 => "N02", N03 => "N03", N04 => "N04",
    N05 => "N05", N06 => "N06", N07 => "N07", N08 => "N08",
    N09 => "N09", N10 => "N10", N11 => "N11", N12 => "N12",
    N13 => "N13", N14 => "N14", N15 => "N15", N16 => "N16",
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NodeId {
    type Err = DagError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_opt(s).ok_or_else(|| DagError::UnknownNode(s.to_string()))
    }
}

/// 失败回退。v3 §3.4：「失败不是报错退出，是回到哪一步」。
/// 只序列化（用于报告），静态表不需要反序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Fallback {
    /// 直接拒绝，并给出面向用户的说明（原因 + 下一步）。
    Reject(&'static str),
    /// 继续，但在产物里打标记。
    Continue(&'static str),
    /// 回到指定节点重试，最多 `max_attempts` 次。
    Retry {
        node: NodeId,
        max_attempts: u8,
        hint: &'static str,
    },
    /// 降级到不依赖该能力的路径。
    Degrade(&'static str),
}

/// 节点契约（五要素中的前四件）。静态常量表，只序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeSpec {
    pub id: NodeId,
    pub name: &'static str,
    pub inputs: &'static [&'static str],
    pub outputs: &'static [&'static str],
    pub assertion: &'static str,
    pub fallback: Fallback,
}

/// v3 §3.3 的 16 个节点契约，静态表。
///
/// 新增节点必须同时改这里和 `NodeId`，否则 `specs_are_complete` 测试会失败。
pub static NODE_SPECS: &[NodeSpec] = &[
    NodeSpec {
        id: NodeId::N01,
        name: "ingest",
        inputs: &["source"],
        outputs: &["normalized.wav", "src_ref.wav"],
        assertion: "时长 >3s；峰值 >-40dBFS",
        fallback: Fallback::Reject("源太短或电平过低。换一条时长 >3 秒、峰值 >-40dBFS 的录音。"),
    },
    NodeSpec {
        id: NodeId::N02,
        name: "grade",
        inputs: &["normalized.wav"],
        outputs: &["grade.json"],
        assertion: "五项指标全算出，缺一即失败",
        fallback: Fallback::Reject("源质量分级失败。检查文件是否完整、能否正常解码。"),
    },
    NodeSpec {
        id: NodeId::N03,
        name: "separate",
        inputs: &["normalized.wav"],
        outputs: &["vocal_raw.wav", "inst.wav"],
        assertion: "三模型都返回；超时用已返回的",
        // 模型不绑定：缺模型时不是错误，降级到 A 入口（直通）
        fallback: Fallback::Degrade("未声明分离模型，使用 A 入口同音色直通路径。"),
    },
    NodeSpec {
        id: NodeId::N04,
        name: "fuse",
        inputs: &["vocal_raw.wav", "inst.wav"],
        outputs: &["vocal_fused.wav", "sep_qa.json"],
        assertion: "占比 0.25–0.65、残留 <-18dB、空洞率 <20%",
        fallback: Fallback::Retry {
            node: NodeId::N03,
            max_attempts: 2,
            hint: "分离质量不足，已换模型组合重试",
        },
    },
    NodeSpec {
        id: NodeId::N05,
        name: "f0extract",
        inputs: &["vocal_fused.wav"],
        outputs: &["f0.npz", "voicing.npz", "f0_conf.json"],
        assertion: "双路差异 >600 cents 帧占比 ≤1%",
        fallback: Fallback::Degrade("基频提取不可用，跳过依赖 f0 的注入步骤。"),
    },
    NodeSpec {
        id: NodeId::N06,
        name: "precheck_rvc",
        inputs: &["f0.npz", "gate 参数"],
        outputs: &["gate.json"],
        assertion: "四项预检；任一不过给出 shift_key / skip_rvc / reject",
        fallback: Fallback::Continue("预检未全过，按 gate.json 的决定继续。"),
    },
    NodeSpec {
        id: NodeId::N07,
        name: "rvc",
        inputs: &["vocal_fused.wav", "gate.json"],
        outputs: &["vocal_conv_{take}.wav"],
        assertion: "时长差 ≤50ms；无 NaN；峰值 ≤-1dBFS",
        fallback: Fallback::Degrade("未声明转换模型，保留原音色。"),
    },
    NodeSpec {
        id: NodeId::N08,
        name: "coarse_rank",
        inputs: &["vocal_conv_{take}.wav"],
        outputs: &["ranked.json"],
        assertion: "至少 1 个 take 过粗筛",
        fallback: Fallback::Reject("所有候选都未通过粗筛。放宽筛选阈值或重录源。"),
    },
    NodeSpec {
        id: NodeId::N09,
        name: "repair",
        inputs: &["ranked.json"],
        outputs: &["vocal_repaired.wav", "repair_meta.json"],
        assertion: "重建后 8–16kHz 能量比 ≥0.6",
        fallback: Fallback::Continue("高频重建未达标，按原样继续并记入报告。"),
    },
    NodeSpec {
        id: NodeId::N10,
        name: "breath",
        inputs: &["vocal_fused.wav"],
        outputs: &["breath_events.json", "breath_audio.wav"],
        assertion: "事件数 >0；为 0 标 no_breath_detected",
        fallback: Fallback::Continue("未检出呼吸事件，标 no_breath_detected 后继续。"),
    },
    NodeSpec {
        id: NodeId::N11,
        name: "humanize",
        inputs: &["f0.npz", "voicing.npz"],
        outputs: &["perf_humanized.json"],
        assertion: "注入后 σ 在红线内，超限裁剪并记 clamped",
        fallback: Fallback::Continue("注入参数越界，已裁剪并标 clamped。"),
    },
    NodeSpec {
        id: NodeId::N12,
        name: "space",
        inputs: &["vocal_repaired.wav", "inst.wav"],
        outputs: &["vocal_spaced.wav", "space.json"],
        assertion: "人声与伴奏 RT60 差 ≤0.15s",
        fallback: Fallback::Continue("空间匹配未达标，按当前参数继续并记入报告。"),
    },
    NodeSpec {
        id: NodeId::N13,
        name: "layer",
        inputs: &["vocal_spaced.wav", "breath_audio.wav", "perf_humanized.json"],
        outputs: &["layer_stems/"],
        assertion: "各层与 lead 时间偏差 ≤5ms",
        fallback: Fallback::Continue("层对齐偏差超限，记入报告。"),
    },
    NodeSpec {
        id: NodeId::N14,
        name: "mix",
        inputs: &["layer_stems/", "inst.wav"],
        outputs: &["mix.wav"],
        assertion: "mono 折叠后人声衰减 ≤3dB",
        fallback: Fallback::Continue("单声道兼容性未达标，记入报告。"),
    },
    NodeSpec {
        id: NodeId::N15,
        name: "master",
        inputs: &["mix.wav"],
        outputs: &["master.wav"],
        assertion: "真峰值 ≤-1.0 dBTP；LRA ≥8 LU；响度偏差 ≤0.5 LU",
        // v3 §3.4：LRA 不足回 N14 降压缩比重试一次，仍不达标则告警不阻断。
        // 之前误写成 Reject，与文档不符，这里改正。
        fallback: Fallback::Retry {
            node: NodeId::N14,
            max_attempts: 1,
            hint: "母带动态范围不足，回 N14 降低压缩比后重试；仍不达标则告警但不阻断",
        },
    },
    NodeSpec {
        id: NodeId::N16,
        name: "eval",
        inputs: &["master.wav", "src_ref.wav"],
        outputs: &["eval.json", "report.md"],
        assertion: "37 项全算出；缺项标 not_applicable，不许填 0",
        fallback: Fallback::Continue("部分指标不适用，标 not_applicable 而非填 0。"),
    },
];

/// 取某个节点的契约。查不到说明表与枚举不同步，属于编程错误。
pub fn spec(id: NodeId) -> &'static NodeSpec {
    NODE_SPECS
        .iter()
        .find(|s| s.id == id)
        .expect("NODE_SPECS 必须覆盖所有 NodeId")
}

/// 幂等键：`sha256(上游产物 + 参数 + 引擎版本 + adapter_hash)`。
///
/// 用长度前缀分隔四段，避免 "ab"+"c" 与 "a"+"bc" 拼出同样的字节串。
/// 这直接决定调参效率：改 humanize 只重跑最后 5 个节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyKey(pub [u8; 32]);

impl IdempotencyKey {
    pub fn derive(
        upstream: &[u8],
        params: &[u8],
        engine_version: &str,
        adapter_hash: &str,
    ) -> Self {
        let mut h = Sha256::new();
        for part in [upstream, params, engine_version.as_bytes(), adapter_hash.as_bytes()] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part);
        }
        let digest: [u8; 32] = h.finalize().into();
        Self(digest)
    }

    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
            s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
        }
        s
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

/// 节点执行结果的状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Done,
    Degraded(String),
    Rejected(String),
}

/// 一次节点执行的记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub node: NodeId,
    pub key: IdempotencyKey,
    pub status: NodeStatus,
    /// 产物的内容寻址引用（十六进制摘要）。
    pub artifacts: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("未知节点：{0}")]
    UnknownNode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_16_nodes_have_specs() {
        assert_eq!(NodeId::ALL.len(), 16);
        assert_eq!(NODE_SPECS.len(), 16);
        for id in NodeId::ALL {
            let s = spec(*id);
            assert!(!s.name.is_empty());
            assert!(!s.inputs.is_empty(), "{id} 缺输入");
            assert!(!s.outputs.is_empty(), "{id} 缺输出");
            assert!(!s.assertion.is_empty(), "{id} 缺契约断言");
        }
    }

    #[test]
    fn node_id_roundtrip() {
        for id in NodeId::ALL {
            assert_eq!(NodeId::from_str(id.as_str()).unwrap(), *id);
        }
        assert!(NodeId::from_str("N99").is_err());
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        let a = IdempotencyKey::derive(b"upstream", b"{}", "1.0", "abc");
        let b = IdempotencyKey::derive(b"upstream", b"{}", "1.0", "abc");
        assert_eq!(a, b);
        assert_eq!(a.hex().len(), 64);
    }

    #[test]
    fn changing_any_component_changes_key() {
        let base = IdempotencyKey::derive(b"u", b"p", "v", "a");
        assert_ne!(base, IdempotencyKey::derive(b"U", b"p", "v", "a"));
        assert_ne!(base, IdempotencyKey::derive(b"u", b"P", "v", "a"));
        assert_ne!(base, IdempotencyKey::derive(b"u", b"p", "V", "a"));
        assert_ne!(base, IdempotencyKey::derive(b"u", b"p", "v", "A"));
    }

    /// 长度前缀的意义：四段拼接不能有歧义。
    #[test]
    fn no_concatenation_ambiguity() {
        let a = IdempotencyKey::derive(b"ab", b"c", "", "");
        let b = IdempotencyKey::derive(b"a", b"bc", "", "");
        assert_ne!(a, b, "分段边界必须参与摘要");
    }

    /// 模型不绑定：N03/N07 缺模型时是降级，不是拒绝。
    #[test]
    fn missing_model_degrades_instead_of_rejecting() {
        assert!(matches!(spec(NodeId::N03).fallback, Fallback::Degrade(_)));
        assert!(matches!(spec(NodeId::N07).fallback, Fallback::Degrade(_)));
    }
}
