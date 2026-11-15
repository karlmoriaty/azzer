//! 独立启动的 HTTP 服务，用于开发期联调（不开 Tauri）。
//!
//! ```text
//! node scripts/cargo-run.cjs run -p azzer-api --bin serve
//! ```
//!
//! 启动后把 `{"port": ..., "token": ...}` 写到运行期文件，便于 curl 与
//! 前端 dev server 取用。文件位置推导：`<工作目录>/.build/runtime.json`。
//!
//! 安全说明：正式版不落盘 token——它由 Tauri 的 initialization script
//! 直接注入 WebView。这里的落盘仅限开发期，且文件在工作目录内。

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

/// 运行期文件路径：`<工作目录>/.build/runtime.json`。
///
/// 从 exe 位置推算：`.build/cargo-target/debug/serve.exe` 上溯三层得到
/// `.build`。这样换机器、换盘符都不用改代码。
fn runtime_file() -> PathBuf {
    if let Ok(p) = std::env::var("AZZER_RUNTIME_FILE") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe;
        // 去掉 serve.exe → debug → cargo-target → .build
        for _ in 0..3 {
            if !d.pop() {
                break;
            }
        }
        if d.is_dir() {
            return d.join("runtime.json");
        }
    }
    PathBuf::from(".build/runtime.json")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let server = azzer_api::serve().await?;

    let out = runtime_file();
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let payload = serde_json::json!({
        "port": server.port,
        "token": server.token,
        "dev_only": true,
        "note": "开发期文件；正式版 token 只注入 WebView，不落盘"
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&payload)?)?;

    println!("AZZER_PORT={}", server.port);
    println!("AZZER_TOKEN={}", server.token);
    println!("AZZER_RUNTIME_FILE={}", out.display());

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    server.handle.abort();
    Ok(())
}
