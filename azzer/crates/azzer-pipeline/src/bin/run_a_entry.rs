//! A 入口闭环（同音色直通）的命令行入口。
//!
//! ```text
//! node scripts/cargo-run.cjs run -p azzer-pipeline --bin run_a_entry -- <源WAV> [运行根] [输出JSON] [目标LUFS]
//! ```
//!
//! 零模型可运行：不需要任何语音模型，走 N01 → N02 → N15。
//! 所有产物写在运行根目录内。

use std::path::PathBuf;

use azzer_dsp::MasterConfig;
use azzer_pipeline::{run_a_entry, PipelineContext};

fn main() {
    if let Err(e) = real_main() {
        eprintln!("A 入口执行失败：{e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法：run_a_entry <源WAV> [运行根目录] [输出JSON] [目标LUFS]");
        eprintln!("说明：零模型可运行；models.json 不存在是正常状态。");
        std::process::exit(2);
    }
    let source = PathBuf::from(&args[1]);
    let root = args
        .get(2)
        .map(PathBuf::from)
        .or_else(|| std::env::var("AZZER_RUN_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .join(".cache/a-entry")
        });

    let mut cfg = MasterConfig::default();
    if let Some(v) = args.get(4) {
        cfg.target_lufs = v.parse().unwrap_or(cfg.target_lufs);
    }

    let ctx = PipelineContext::open(&root)?;
    let report = run_a_entry(&ctx, &source, &cfg)?;

    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");

    // 由 Rust 直接写 UTF-8 文件，避开 PowerShell 的 GBK 解码
    if let Some(out) = args.get(3) {
        let out = std::path::Path::new(out);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(out, &json)?;
    }

    let runs = root.join("runs");
    std::fs::create_dir_all(&runs)?;
    let name = format!("a-{}.json", &report.n15_key[..12]);
    std::fs::write(runs.join(name), json)?;

    // 把成品从内容寻址存储复制到固定路径，便于试听与外部工具验证
    let src = ctx.store.path_for(&report.master_blob, "wav");
    if src.is_file() {
        let out_dir = root.join("out");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::copy(&src, out_dir.join("master.wav"))?;
        println!("master.wav -> {}", out_dir.join("master.wav").display());
    }
    Ok(())
}
