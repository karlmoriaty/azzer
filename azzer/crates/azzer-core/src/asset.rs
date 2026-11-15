//! 内容寻址存储（CAS）。
//!
//! 产物按 `sha256(bytes)` 命名，同一份内容只存一次。这是幂等 DAG 的物理基础：
//! 参数没变 → 摘要没变 → 直接命中已有产物，不必重跑节点。
//!
//! 路径布局：`<root>/<digest前2位>/<digest>.<ext>`，避免单目录塞几万个文件。
//!
//! **这个模块不硬编码任何根目录**，由调用方传入。R1 里由上层指向工作目录内的
//! `.assets/`，符合「所有产物在工作目录内」的约束。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 一个已存产物的引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub digest: String,
    pub ext: String,
    pub size: u64,
}

impl BlobRef {
    pub fn as_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 写入内容，返回引用。已存在则跳过写入（幂等）。
    pub fn put(&self, bytes: &[u8], ext: &str) -> Result<BlobRef, StoreError> {
        self.put_tracked(bytes, ext).map(|(r, _)| r)
    }

    /// 同 [`Self::put`]，但额外返回 `cache_hit`：产物原本就在则为 true。
    /// R1 用它验证「参数不变就只重跑受影响节点」。
    pub fn put_tracked(&self, bytes: &[u8], ext: &str) -> Result<(BlobRef, bool), StoreError> {
        let digest = hex_digest(bytes);
        let ext = sanitize_ext(ext);
        let path = self.path_for(&digest, &ext);
        let cache_hit = path.is_file();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| StoreError::Io(path.clone(), e))?;
        }
        if !cache_hit {
            fs::write(&path, bytes).map_err(|e| StoreError::Io(path.clone(), e))?;
        }
        Ok((
            BlobRef {
                digest,
                ext,
                size: bytes.len() as u64,
            },
            cache_hit,
        ))
    }

    /// 读取并校验摘要。文件被改动会报错，不会静默返回脏数据。
    pub fn get(&self, r: &BlobRef) -> Result<Vec<u8>, StoreError> {
        let path = self.path_for(&r.digest, &r.ext);
        let bytes = fs::read(&path).map_err(|e| StoreError::Io(path.clone(), e))?;
        if bytes.len() as u64 != r.size {
            return Err(StoreError::SizeMismatch {
                expected: r.size,
                actual: bytes.len() as u64,
            });
        }
        let actual = hex_digest(&bytes);
        if actual != r.digest {
            return Err(StoreError::ChecksumMismatch { expected: r.digest.clone(), actual });
        }
        Ok(bytes)
    }

    pub fn contains(&self, r: &BlobRef) -> bool {
        self.path_for(&r.digest, &r.ext).is_file()
    }

    pub fn path_for(&self, digest: &str, ext: &str) -> PathBuf {
        let shard = digest.get(..2).unwrap_or("00");
        self.root.join(shard).join(format!("{digest}.{ext}"))
    }
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    s
}

/// 扩展名只允许字母数字，防止路径穿越。
fn sanitize_ext(ext: &str) -> String {
    let trimmed = ext.trim_start_matches('.');
    let safe: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    if safe.is_empty() {
        "bin".into()
    } else {
        safe
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("存储 IO 失败：{0}：{1}")]
    Io(PathBuf, std::io::Error),
    #[error("产物大小不符：期望 {expected} 字节，实得 {actual} 字节")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("产物摘要不符：期望 {expected}，实得 {actual}")]
    ChecksumMismatch { expected: String, actual: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        // 测试也在工作目录内落盘，不写系统临时目录
        let p = std::env::current_dir()
            .unwrap_or_default()
            .join(".test-cas")
            .join(name);
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn put_then_get_roundtrips() {
        let store = ContentStore::new(temp_root("roundtrip"));
        let r = store.put(b"hello azzer", "wav").unwrap();
        assert_eq!(r.digest.len(), 64);
        assert_eq!(r.size, 11);
        assert_eq!(store.get(&r).unwrap(), b"hello azzer");
    }

    #[test]
    fn same_content_is_stored_once() {
        let store = ContentStore::new(temp_root("dedupe"));
        let a = store.put(b"same", "json").unwrap();
        let b = store.put(b"same", "json").unwrap();
        assert_eq!(a, b);
        assert!(store.contains(&a));
    }

    #[test]
    fn corrupted_file_is_detected() {
        let store = ContentStore::new(temp_root("corrupt"));
        let r = store.put(b"original", "bin").unwrap();
        fs::write(store.path_for(&r.digest, &r.ext), b"tampered!").unwrap();
        assert!(matches!(
            store.get(&r),
            Err(StoreError::ChecksumMismatch { .. }) | Err(StoreError::SizeMismatch { .. })
        ));
    }

    #[test]
    fn ext_is_sanitized() {
        assert_eq!(sanitize_ext("wav"), "wav");
        assert_eq!(sanitize_ext(".json"), "json");
        assert_eq!(sanitize_ext("../../etc"), "etc", "路径分隔符必须被剔除");
        assert_eq!(sanitize_ext(""), "bin");
    }
}
