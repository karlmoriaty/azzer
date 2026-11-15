//! 模型注册：**外部资产声明**，不是软件的一部分。
//!
//! 设计约束（2026-11-08 设计师明确要求，见 v3.2 §5.1）：
//! 本机所有语音模型都不绑定在 Azzer 内。不内置、不假设、不下载、不托管。
//! 用户自行放置模型、自行在 `models.json` 里声明路径，Azzer 只读引用。
//!
//! 由此产生三条行为准则，都有测试守住：
//! 1. **声明文件不存在是正常状态**，返回空注册表，不报错、不创建、不下载。
//! 2. **模型文件不存在是正常状态**，状态为 `Missing`，不 panic、不阻断启动。
//! 3. **零模型时 A 入口（同音色直通）必须可用**——见 [`Capabilities::direct`]。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 模型在流水线里扮演的角色。这只是分类，不绑定任何具体模型名或格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Separation,
    F0,
    Content,
    Conversion,
    VocalRepair,
    Other,
}

/// 用户对一个模型的声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    /// 用户自定义 ID，Azzer 不解释其内容。
    pub id: String,
    pub role: ModelRole,
    /// 用户自己放置的路径。Azzer 只读，不写入/升级/删除/移动。
    pub path: PathBuf,
    /// 可选的整体校验和，用于发现文件被换掉。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 可选的引擎标识，只作标签，不触发任何 SDK 依赖。
    #[serde(default)]
    pub engine: Option<String>,
    /// 用户自己的备注（例如"这版残缺，只用于试验"）。
    #[serde(default)]
    pub note: Option<String>,
}

/// 声明文件的顶层结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelManifest {
    #[serde(default)]
    pub models: Vec<ModelRef>,
}

/// 一个已声明模型的实际状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelStatus {
    /// 文件存在且未声明校验和（或校验通过）。
    Available,
    /// 文件不存在。**不是错误**，只是这档能力暂不可用。
    Missing,
    /// 声明了校验和但不匹配。
    ChecksumMismatch,
}

impl ModelStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, ModelStatus::Available)
    }
}

/// 零模型也必须能跑，所以 `direct` 恒为 true。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// A 入口：同音色直通，不依赖任何模型。
    pub direct: bool,
    /// B 入口：需要用户声明且可用的分离模型。
    pub separate: bool,
    /// C 入口：需要用户声明且可用的转换模型。
    pub convert: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            direct: true,
            separate: false,
            convert: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    manifest: ModelManifest,
    /// 声明文件来自哪里，仅用于报告展示。
    source: Option<PathBuf>,
}

impl ModelRegistry {
    /// 空注册表 = 零模型 = 完全正常的运行状态。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从声明文件加载。**文件不存在时返回空注册表，不报错、不创建文件。**
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(Self::empty());
        }
        let text = fs::read_to_string(path).map_err(|e| RegistryError::Io(path.to_path_buf(), e))?;
        let mut registry = Self::from_json_str(&text)?;
        registry.source = Some(path.to_path_buf());
        Ok(registry)
    }

    pub fn from_json_str(text: &str) -> Result<Self, RegistryError> {
        if text.trim().is_empty() {
            return Ok(Self::empty());
        }
        let manifest: ModelManifest =
            serde_json::from_str(text).map_err(RegistryError::Parse)?;
        Ok(Self {
            manifest,
            source: None,
        })
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn models(&self) -> &[ModelRef] {
        &self.manifest.models
    }

    pub fn is_empty(&self) -> bool {
        self.manifest.models.is_empty()
    }

    /// 按角色找一个声明。不保证可用，调用方需再看 [`Self::status`]。
    pub fn declared_for(&self, role: ModelRole) -> Option<&ModelRef> {
        self.manifest.models.iter().find(|m| m.role == role)
    }

    /// 只判文件在不在，不读内容（开销 O(1)），适合每次轮询调用。
    ///
    /// **注意**：这不算校验和。声音文件被换成同名错误文件时这里会返回
    /// `Available`，要发现它必须显式调 [`Self::verify`]。
    pub fn presence(&self, m: &ModelRef) -> ModelStatus {
        if m.path.is_file() {
            ModelStatus::Available
        } else {
            ModelStatus::Missing
        }
    }

    /// 完整校验：文件存在 + 可选 sha256 比对。
    ///
    /// 会读整个文件。300MB 的模型约几百毫秒，所以只应在"开始处理前"
    /// 或用户点"验证"时调用一次，**不要放进每次状态轮询**。
    pub fn verify(&self, m: &ModelRef) -> ModelStatus {
        if !m.path.is_file() {
            return ModelStatus::Missing;
        }
        if let Some(expected) = &m.sha256 {
            match fs::read(&m.path) {
                Ok(bytes) => {
                    let actual = crate::asset::hex_digest(&bytes);
                    if actual.eq_ignore_ascii_case(expected) {
                        ModelStatus::Available
                    } else {
                        ModelStatus::ChecksumMismatch
                    }
                }
                Err(_) => ModelStatus::Missing,
            }
        } else {
            ModelStatus::Available
        }
    }

    /// 当前实际可用的能力档。缺模型只影响对应档位，不影响 `direct`。
    ///
    /// 用 `presence`（不读文件）判可用性，避免每次查询都算一遍校验和。
    /// 需要强校验时先对目标模型调 [`Self::verify`]。
    pub fn capabilities(&self) -> Capabilities {
        let usable = |role| {
            self.declared_for(role)
                .map(|m| self.presence(m).is_usable())
                .unwrap_or(false)
        };
        Capabilities {
            direct: true,
            separate: usable(ModelRole::Separation),
            convert: usable(ModelRole::Conversion),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("读取模型声明失败：{0}：{1}")]
    Io(PathBuf, std::io::Error),
    #[error("模型声明不是合法 JSON：{0}")]
    Parse(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_manifest_is_an_empty_registry_not_an_error() {
        let p = std::env::current_dir().unwrap().join(".test-does-not-exist-models.json");
        let r = ModelRegistry::load(&p).unwrap();
        assert!(r.is_empty());
        assert!(r.capabilities().direct, "零模型时 A 入口必须可用");
        assert!(!r.capabilities().separate);
        assert!(!r.capabilities().convert);
        assert!(!p.exists(), "加载不能创建声明文件");
    }

    #[test]
    fn declared_but_missing_file_is_not_an_error() {
        let r = ModelRegistry::from_json_str(
            r#"{"models":[{"id":"broken","role":"separation","path":"G:/no/such/model.onnx"}]}"#,
        )
        .unwrap();
        let m = r.declared_for(ModelRole::Separation).unwrap();
        assert_eq!(r.presence(m), ModelStatus::Missing);
        assert_eq!(r.verify(m), ModelStatus::Missing);
        assert!(!r.capabilities().separate, "残缺模型不该启用 B 入口");
        assert!(r.capabilities().direct, "但仍必须保留 A 入口");
    }

    #[test]
    fn empty_or_blank_manifest_is_valid() {
        assert!(ModelRegistry::from_json_str("").unwrap().is_empty());
        assert!(ModelRegistry::from_json_str("  \n ").unwrap().is_empty());
        assert!(ModelRegistry::from_json_str("{}").unwrap().is_empty());
    }

    #[test]
    fn checksum_mismatch_is_reported() {
        let dir = std::env::current_dir().unwrap().join(".test-model");
        let file = dir.join("fake.onnx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&file, b"not-a-real-model").unwrap();
        let json = format!(
            r#"{{"models":[{{"id":"x","role":"conversion","path":{},"sha256":"deadbeef"}}]}}"#,
            serde_json::to_string(&file.to_string_lossy()).unwrap()
        );
        let r = ModelRegistry::from_json_str(&json).unwrap();
        let m = r.declared_for(ModelRole::Conversion).unwrap();
        assert_eq!(r.verify(m), ModelStatus::ChecksumMismatch);
        // presence 只看文件在不在，所以它认为可用；校验和必须显式 verify 才发现。
        assert_eq!(r.presence(m), ModelStatus::Available);
        assert!(r.capabilities().convert, "capabilities 走 presence，这是刻意的性能取舍");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_is_a_readable_error() {
        let e = ModelRegistry::from_json_str("{not json").unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)));
        assert!(e.to_string().contains("模型声明不是合法 JSON"));
    }
}
