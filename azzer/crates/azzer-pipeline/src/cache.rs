//! 节点执行记录（按幂等键索引）。
//!
//! 幂等键的意义不只是"不重复写同一份产物"，而是**命中就不重算**。
//! 没有这层，M2 的分离节点每次都要重跑 90 秒，调参效率会崩。
//!
//! 布局：`<运行根>/outcomes/<key前2位>/<key>.json`，key 是 64 位十六进制的幂等键。
//!
//! 与 CAS 的区别：CAS 按**内容**寻址（存产物），这里按**计算条件**寻址
//! （存这次计算的结论）。两者互补。

use std::fs;
use std::path::{Path, PathBuf};

use azzer_core::IdempotencyKey;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct RunCache {
    root: PathBuf,
}

impl RunCache {
    pub fn new(run_root: impl AsRef<Path>) -> Self {
        Self {
            root: run_root.as_ref().join("outcomes"),
        }
    }

    fn path_for(&self, key: &IdempotencyKey) -> PathBuf {
        let hex = key.hex();
        let shard = hex.get(..2).unwrap_or("00");
        self.root.join(shard).join(format!("{hex}.json"))
    }

    /// 命中返回缓存的 JSON。读不出来（损坏/半截）就当没命中，不阻断执行。
    pub fn get(&self, key: &IdempotencyKey) -> Option<Value> {
        let path = self.path_for(key);
        let text = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 写入。失败不阻断主流程——缓存丢了最多重算一次，不该让处理失败。
    pub fn put(&self, key: &IdempotencyKey, value: &Value) {
        let path = self.path_for(key);
        if let Some(dir) = path.parent() {
            if fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(value) {
            let _ = fs::write(path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_root(name: &str) -> PathBuf {
        std::env::current_dir()
            .unwrap_or_default()
            .join(".test-runcache")
            .join(name)
    }

    #[test]
    fn miss_then_hit() {
        let root = test_root("basic");
        let _ = fs::remove_dir_all(&root);
        let cache = RunCache::new(&root);
        let key = IdempotencyKey::derive(b"a", b"b", "1", "x");

        assert!(cache.get(&key).is_none(), "首次必须未命中");
        cache.put(&key, &json!({"v": 42}));
        assert_eq!(cache.get(&key).unwrap()["v"], 42, "写入后必须命中");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_entry_is_treated_as_miss() {
        let root = test_root("corrupt");
        let _ = fs::remove_dir_all(&root);
        let cache = RunCache::new(&root);
        let key = IdempotencyKey::derive(b"c", b"d", "1", "x");
        cache.put(&key, &json!({"v": 1}));
        fs::write(cache.path_for(&key), b"{not json").unwrap();
        assert!(cache.get(&key).is_none(), "损坏的缓存当作未命中，不阻断");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn different_keys_do_not_collide() {
        let root = test_root("distinct");
        let _ = fs::remove_dir_all(&root);
        let cache = RunCache::new(&root);
        let a = IdempotencyKey::derive(b"a", b"b", "1", "x");
        let b = IdempotencyKey::derive(b"a", b"b", "2", "x");
        cache.put(&a, &json!({"v": "A"}));
        assert!(cache.get(&b).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
