//! R1 闭环的命令行入口。
//!
//! ```text
//! node scripts/cargo-run.cjs run -p azzer-pipeline --bin run_r1 -- <源WAV> [运行根目录]
//! ```
//!
//! 不传运行根目录时用 `AZZER_RUN_ROOT` 或当前目录下的 `.cache/r1`。
//! 所有产物写在该根目录内；根目录由调用方决定，保证能落在工作目录内。

use std::path::PathBuf;

use azzer_pipeline::{run_r1, PipelineContext};

fn main() {
    if let Err(e) = real_main() {
        eprintln!("R1 执行失败：{e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法：run_r1 <源WAV> [运行根目录] [输出JSON路径]");
        eprintln!("说明：零模型可运行；models.json 不存在是正常状态。");
        std::process::exit(2);
    }
    let source = PathBuf::from(&args[1]);
    let root = args
        .get(2)
        .map(PathBuf::from)
        .or_else(|| std::env::var("AZZER_RUN_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(".cache/r1"));

    let ctx = PipelineContext::open(&root)?;
    let report = run_r1(&ctx, &source)?;

    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");

    // 由 Rust 直接写 UTF-8 文件，不经过 PowerShell 的 GBK 解码。
    // 显式传第 3 个参数时，这个文件就是可机读的干净报告。
    if let Some(out) = args.get(3) {
        let out = std::path::Path::new(out);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(out, &json)?;
    }

    // 报告也在运行根目录里留一份，便于回归对比
    let runs = root.join("runs");
    std::fs::create_dir_all(&runs)?;
    let name = format!("r1-{}.json", &report.n01_key[..12]);
    std::fs::write(runs.join(name), json)?;
    Ok(())
}
