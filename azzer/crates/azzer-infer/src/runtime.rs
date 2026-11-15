//! ONNX Runtime 运行时探测与准备。
//!
//! ## 版本约束（2026-11-08 实测 + 官方文档核对）
//!
//! | 组件 | 版本 | 说明 |
//! |---|---|---|
//! | `ort` crate | 2.0.0-rc.9 | 匹配 ORT 1.26 的 C API |
//! | `onnxruntime-gpu` | 1.26.0 | 官方文档：1.27 起 PyPI 的 GPU 包默认 CUDA 13，只有 1.26 及更早是 CUDA 12.8 |
//! | CUDA 运行库 | 12.9（cu12 的 pip 包） | `cudart64_12` / `cublas64_12` / `cublasLt64_12` / `cudnn64_9` / `cufft64_11` |
//!
//! 为什么不用最新的 rc.15：它要求 ORT ≥1.31，而 1.31 的 CUDA EP 只认 CUDA 13，
//! NVIDIA 又还没在 pip 发布 cu13 的 cublas（只有 0.0.1 占位包）。链条断在最后一环，
//! 只能往上退到 1.26。
//!
//! ## 三个必须遵守的约束
//!
//! **一、每个进程只能有一个 Environment**，重复初始化会报错，所以结果要缓存。
//!
//! **二、rc.9 的 load-dynamic 只认 `ORT_DYLIB_PATH`**，没有 `init_from`。
//! 所以加载前要先设环境变量——**只改当前进程**，不动系统。
//!
//! **三、CUDA 的依赖 DLL 必须能被找到**，启动时把 `native/cuda/bin` 前置到进程 PATH。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

/// 单个执行后端的可用性。
#[derive(Debug, Clone, Serialize)]
pub struct EpReport {
    /// EP 名称，如 `CPUExecutionProvider`、`CUDAExecutionProvider`。
    pub name: String,
    /// 是否在编译期启用。
    pub compiled_in: bool,
    /// 本机推荐优先级；0 表示不可用。
    pub priority: u8,
}

/// 运行时探测结果，作为 `/v1/system/info` 的一部分返回给前端。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProbe {
    pub ort_version: String,
    pub dylib_path: Option<String>,
    pub cuda_bin: Option<String>,
    /// CUDA 关键 DLL 是否齐全。
    pub cuda_dlls: Vec<String>,
    pub providers: Vec<EpReport>,
    pub recommended: String,
    pub init_error: Option<String>,
}

static PROBE: OnceLock<RuntimeProbe> = OnceLock::new();

/// 项目自带的 native 目录：`<workspace>/native`。
fn native_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AZZER_NATIVE_DIR") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for up in 0usize..=5 {
                let mut d = dir.to_path_buf();
                for _ in 0..up {
                    if !d.pop() {
                        break;
                    }
                }
                // 两种布局都要认：
                //   <workspace>/native            （打包后的安装目录）
                //   <workspace>/azzer/native     （开发期仓库布局）
                for p in [d.join("native"), d.join("azzer").join("native")] {
                    if p.is_dir() {
                        return Some(p);
                    }
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for p in [cwd.join("native"), cwd.join("azzer").join("native")] {
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

fn locate_dylib(native: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let p = native?.join("onnxruntime").join("onnxruntime.dll");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// CUDA 运行库的五个关键 DLL。缺任何一个，CUDA EP 都加载不起来。
const CUDA_REQUIRED: &[&str] = &[
    "cudart64_12.dll",
    "cublas64_12.dll",
    "cublasLt64_12.dll",
    "cudnn64_9.dll",
    "cufft64_11.dll",
];

/// 把 `native/cuda/bin` 前置到进程 PATH（仅当前进程，退出即失效）。
fn prepend_cuda_path(cuda_bin: &Path) -> bool {
    if !cuda_bin.is_dir() {
        return false;
    }
    let old = std::env::var("PATH").unwrap_or_default();
    let bin = cuda_bin.display().to_string();
    if old.split(';').any(|p| p.eq_ignore_ascii_case(&bin)) {
        return true;
    }
    std::env::set_var("PATH", format!("{bin};{old}"));
    true
}

fn do_probe() -> RuntimeProbe {
    let native = native_dir();

    let cuda_bin = native.as_ref().map(|n| n.join("cuda").join("bin"));
    let cuda_ready = match &cuda_bin {
        Some(p) => prepend_cuda_path(p),
        None => false,
    };
    let cuda_dlls: Vec<String> = match &cuda_bin {
        Some(p) => CUDA_REQUIRED
            .iter()
            .filter(|f| p.join(f).is_file())
            .map(|f| (*f).to_string())
            .collect(),
        None => Vec::new(),
    };
    let cuda_complete = cuda_dlls.len() == CUDA_REQUIRED.len();

    // ort rc.10 移除了 `available_providers()`（只剩
    // ExecutionProvider::is_available() 逐个查询）。这里按编译期 feature
    // 报告，并用 CUDA DLL 齐全与否判定实际可用性。
    let mut providers = Vec::<EpReport>::new();

    if cfg!(feature = "cuda") {
        providers.push(EpReport {
            name: "CUDAExecutionProvider".into(),
            compiled_in: true,
            priority: if cuda_complete && cuda_ready { 3 } else { 0 },
        });
    }
    if cfg!(feature = "directml") {
        providers.push(EpReport {
            name: "DmlExecutionProvider".into(),
            compiled_in: true,
            priority: 2,
        });
    }
    providers.push(EpReport {
        name: "CPUExecutionProvider".into(),
        compiled_in: true,
        priority: 1,
    });
    providers.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.name.cmp(&b.name)));

    let recommended = providers
        .iter()
        .max_by_key(|p| p.priority)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "none".into());

    let dylib = locate_dylib(native.as_deref());
    let mut init_error = None;

    match &dylib {
        None => {
            init_error = Some(
                "找不到 onnxruntime.dll。请运行 scripts/fetch-cuda.ps1 部署原生库。".into(),
            );
        }
        Some(path) => {
            // rc.9 的 load-dynamic 通过环境变量定位 dll（无 init_from）。
            // 只影响当前进程。
            std::env::set_var("ORT_DYLIB_PATH", path);
            if let Err(e) = ort::init().commit() {
                init_error = Some(format!("初始化环境失败：{e}"));
            }
        }
    }

    RuntimeProbe {
        ort_version: "1.26.0".into(),
        dylib_path: dylib.map(|p| p.display().to_string()),
        cuda_bin: cuda_bin
            .filter(|p| p.is_dir())
            .map(|p| p.display().to_string()),
        cuda_dlls,
        providers,
        recommended,
        init_error,
    }
}

/// 探测运行时。结果缓存，进程内只初始化一次。
pub fn probe_runtime() -> RuntimeProbe {
    PROBE.get_or_init(do_probe).clone()
}

/// 是否已成功初始化。
pub fn runtime_ready() -> bool {
    probe_runtime().init_error.is_none()
}

/// 是否拿到了可用的 CUDA 路径（五个关键 DLL 齐全且已加入 PATH）。
pub fn cuda_available() -> bool {
    let p = probe_runtime();
    p.providers
        .iter()
        .any(|e| e.name.contains("CUDA") && e.priority > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_cpu_provider() {
        let r = probe_runtime();
        assert!(
            r.providers.iter().any(|p| p.name.contains("CPU")),
            "CPU EP 必须存在"
        );
        assert_ne!(r.recommended, "none");
    }

    #[test]
    fn probe_is_cached() {
        let a = probe_runtime();
        let b = probe_runtime();
        assert_eq!(a.init_error, b.init_error);
    }

    #[test]
    fn cuda_dll_list_is_consistent() {
        let r = probe_runtime();
        assert!(
            r.cuda_dlls.is_empty() || r.cuda_dlls.len() == CUDA_REQUIRED.len(),
            "CUDA DLL 部分缺失：{:?}",
            r.cuda_dlls
        );
    }
}
