//! 引擎协议（六层架构的 L4 边界）。
//!
//! L2 编排层与 L3 计划层只见到这个 trait，不 import 任何推理 SDK，
//! 也不知道模型名、模型路径、采样率。这样本地 ONNX 与远端 sidecar
//! 可以互换——"上云只改 URL"。
//!
//! 模型不绑定约束下，还要求：**没有引擎也能构造出一个合法的引擎对象**，
//! 所以提供 [`NoEngine`]。避免出现 `Option<Box<dyn Engine>>` 散落到每一层。

use crate::model::ModelRole;

pub trait Engine: Send + Sync {
    /// 引擎标识，例如 `onnx-local` / `sidecar-remote`。
    fn id(&self) -> &str;
    /// 引擎版本，参与幂等键。
    fn version(&self) -> &str;
    /// 适配器实现的摘要，参与幂等键。改适配器应让下游节点全部失效重跑。
    fn adapter_hash(&self) -> &str;
    /// 该引擎能否承担某个角色。
    fn supports(&self, role: ModelRole) -> bool;
}

/// 零模型/零引擎时的空实现。A 入口（同音色直通）走的就是它。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEngine;

impl Engine for NoEngine {
    fn id(&self) -> &str {
        "none"
    }
    fn version(&self) -> &str {
        "0"
    }
    fn adapter_hash(&self) -> &str {
        "no-adapter"
    }
    fn supports(&self, _role: ModelRole) -> bool {
        false
    }
}

/// 把引擎的版本信息抽成值对象，供幂等键使用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineDescriptor {
    pub id: String,
    pub version: String,
    pub adapter_hash: String,
}

impl EngineDescriptor {
    pub fn of(e: &dyn Engine) -> Self {
        Self {
            id: e.id().to_string(),
            version: e.version().to_string(),
            adapter_hash: e.adapter_hash().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_engine_is_a_valid_engine() {
        let e = NoEngine;
        assert_eq!(e.id(), "none");
        assert!(!e.supports(ModelRole::Separation));
        let d = EngineDescriptor::of(&e);
        assert_eq!(d.adapter_hash, "no-adapter");
    }

    #[test]
    fn descriptor_captures_version_and_adapter() {
        struct Fake;
        impl Engine for Fake {
            fn id(&self) -> &str { "fake" }
            fn version(&self) -> &str { "1.2.3" }
            fn adapter_hash(&self) -> &str { "deadbeef" }
            fn supports(&self, _: ModelRole) -> bool { true }
        }
        let d = EngineDescriptor::of(&Fake);
        assert_eq!(d.version, "1.2.3");
        assert_eq!(d.adapter_hash, "deadbeef");
    }
}
