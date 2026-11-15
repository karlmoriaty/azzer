//! N01 ingest 与 N02 grade 的契约实现。
//!
//! 每个节点都做四件事：算幂等键 → 执行 → 校验契约断言 → 产物进 CAS。
//! 失败按 `NODE_SPECS` 里声明的回退给出「原因 + 下一步」，不出现"未知错误"。

use std::fs;
use std::path::Path;

use azzer_core::{
    spec, BlobRef, IdempotencyKey, NodeId, NodeOutcome, NodeStatus,
};
use azzer_dsp::{
    analyze, decode_wav, decode_wav_bytes, encode_wav_24_bytes, to_internal_rate, AudioBuffer,
    Grade, GradeMetrics, MasterConfig, MasterReport,
};

use crate::{fallback_text, NodeError, PipelineContext};

#[derive(Debug)]
pub struct N01Result {
    pub outcome: NodeOutcome,
    pub buffer: AudioBuffer,
    pub normalized: BlobRef,
    pub src_ref: BlobRef,
    pub cache_hit: bool,
}

/// N01：解码 → 校验时长/峰值 → 归一化到 48k/24bit → 落 `normalized.wav` 与 `src_ref`。
pub fn n01_ingest(ctx: &PipelineContext, source: &Path) -> Result<N01Result, NodeError> {
    let contract = spec(NodeId::N01);
    let raw = fs::read(source).map_err(|e| NodeError::Io(source.to_path_buf(), e))?;
    let key = IdempotencyKey::derive(
        &raw,
        ctx.params(),
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );

    let decoded = decode_wav(source)?;

    // v1 只支持单/双声道。多声道直接拒绝而不是静默处理错：
    // mono_lows 会把所有声道平均、stereo_width 只处理前两路，结果不可预期。
    if decoded.channel_count() > 2 {
        return Err(NodeError::Contract {
            node: NodeId::N01,
            reason: format!(
                "源有 {} 个声道，v1 只支持单声道与双声道",
                decoded.channel_count()
            ),
            next: "先用外部工具把源下混成单声道或双声道，再重新导入。".into(),
        });
    }

    // 契约断言①：时长 >3s
    let duration = decoded.duration_secs();
    if duration <= 3.0 {
        return Err(NodeError::Contract {
            node: NodeId::N01,
            reason: format!("源时长 {duration:.2}s，未超过 3s"),
            next: fallback_text(&contract.fallback),
        });
    }

    // 契约断言②：峰值 >-40dBFS
    // 按声道取最大，不能用单声道平均——立体声只有一侧有信号时，
    // 平均会把峰值压低 6dB，把一条合法源误判成"电平过低"。
    let peak_dbfs = azzer_dsp::peak_dbfs(&decoded);
    if peak_dbfs <= -40.0 {
        return Err(NodeError::Contract {
            node: NodeId::N01,
            reason: format!("源峰值 {peak_dbfs:.1}dBFS，不高于 -40dBFS"),
            next: fallback_text(&contract.fallback),
        });
    }

    // 48k 输入走零损失 clone；非 48k 走 R1 的线性插值占位（R2 换 rubato）
    let normalized = to_internal_rate(&decoded)?;
    let wav_bytes = encode_wav_24_bytes(&normalized)?;
    let (normalized_ref, cache_hit) = ctx.store.put_tracked(&wav_bytes, "wav")?;
    let src_ref = ctx.store.put(&raw, "src")?;

    let outcome = NodeOutcome {
        node: NodeId::N01,
        key,
        status: NodeStatus::Done,
        artifacts: vec![normalized_ref.digest.clone(), src_ref.digest.clone()],
    };
    Ok(N01Result {
        outcome,
        buffer: normalized,
        normalized: normalized_ref,
        src_ref,
        cache_hit,
    })
}

#[derive(Debug)]
pub struct N02Result {
    pub outcome: NodeOutcome,
    pub metrics: GradeMetrics,
    pub grade_blob: BlobRef,
    pub cache_hit: bool,
    /// 是否真的执行了五项计算。false 表示命中执行记录、直接复用上次结论。
    pub computed: bool,
}

/// N02：算质检五项 → 校验"五项全算出" → 分级 → 落 `grade.json`。
///
/// RT60 无法估计时序列化为 `null`（显式 not_applicable），**不填 0**——
/// v3 §3.3 的口径。这算"算出"，不是缺项。
///
/// **幂等**：先查执行记录，命中就直接复用上次结论，跳过五项计算。
pub fn n02_grade(ctx: &PipelineContext, n01: &N01Result) -> Result<N02Result, NodeError> {
    let contract = spec(NodeId::N02);
    let key = IdempotencyKey::derive(
        n01.normalized.digest.as_bytes(),
        ctx.params(),
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );
    let cache = crate::cache::RunCache::new(&ctx.root);

    // 快速路径：同样的上游内容 + 同样的参数 → 结论必然相同，不必重算
    if let Some(hit) = cache.get(&key) {
        if let Ok(m) = serde_json::from_value::<GradeMetrics>(hit) {
            let json = serde_json::to_vec_pretty(&m)?;
            let (grade_blob, cas_hit) = ctx.store.put_tracked(&json, "json")?;
            let status = if m.grade == Grade::C {
                NodeStatus::Degraded(format!("C 级源：{}", m.reason))
            } else {
                NodeStatus::Done
            };
            return Ok(N02Result {
                outcome: NodeOutcome {
                    node: NodeId::N02,
                    key,
                    status,
                    artifacts: vec![grade_blob.digest.clone()],
                },
                metrics: m,
                grade_blob,
                cache_hit: cas_hit,
                computed: false,
            });
        }
    }

    let m = analyze(&n01.buffer);

    let checks = [
        ("有效带宽", m.effective_bandwidth_hz.is_finite() && m.effective_bandwidth_hz >= 0.0),
        ("SNR", m.snr_db.is_finite() && m.snr_db >= 0.0),
        ("削顶率", (0.0..=1.0).contains(&m.clip_ratio)),
        ("码率", m.bitrate_bps.is_finite() && m.bitrate_bps > 0.0),
        (
            "估计 RT60",
            match m.rt60_s {
                None => true, // 显式 not_applicable
                Some(v) => v.is_finite() && v > 0.0,
            },
        ),
    ];
    if let Some((name, _)) = checks.iter().find(|(_, ok)| !ok) {
        return Err(NodeError::Contract {
            node: NodeId::N02,
            reason: format!("质检项「{name}」未算出合法值"),
            next: fallback_text(&contract.fallback),
        });
    }

    let json = serde_json::to_vec_pretty(&m)?;
    let (grade_blob, cache_hit) = ctx.store.put_tracked(&json, "json")?;

    // v3 §3.4：C 级源「继续（确认后）」，所以是 Degraded 而不是 Rejected。
    let status = if m.grade == Grade::C {
        NodeStatus::Degraded(format!("C 级源：{}", m.reason))
    } else {
        NodeStatus::Done
    };

    // 记录本次计算结论，下次同条件直接命中
    cache.put(&key, &serde_json::to_value(&m)?);

    let outcome = NodeOutcome {
        node: NodeId::N02,
        key,
        status,
        artifacts: vec![grade_blob.digest.clone()],
    };
    Ok(N02Result {
        outcome,
        metrics: m,
        grade_blob,
        cache_hit,
        computed: true,
    })
}

/// N15：母带第 6、7 步（真峰值限幅 + 两遍响度归一）。
///
/// 契约断言三项：真峰值 ≤-1.0 dBTP、LRA ≥8 LU、响度偏差 ≤0.5 LU。
///
/// 处理策略按 v3 §3.4：
/// - 真峰值或响度不达标 → 硬失败（母带没做好，不能产出）
/// - **只有 LRA 不足 → 告警不阻断**（回 N14 降压缩比是上层的事，
///   本轮 R2a 还没有 N14，所以直接登记 Degraded）
pub fn n15_master(
    ctx: &PipelineContext,
    buffer: &AudioBuffer,
    upstream_digest: &str,
    cfg: &MasterConfig,
    chain_cfg: &azzer_dsp::ChainConfig,
) -> Result<N15Result, NodeError> {
    let params = serde_json::to_vec(&serde_json::json!({
        "internal_sample_rate": 48000,
        "wav_bits": 24,
        "target_lufs": cfg.target_lufs,
        "true_peak_limit_dbtp": cfg.true_peak_limit_dbtp,
        "lookahead_ms": cfg.lookahead_ms,
        "release_ms": cfg.release_ms,
        // 前五步的参数也必须进幂等键，否则改 EQ/压缩不会触发重跑
        "low_mono_hz": chain_cfg.low_mono_hz,
        "mud_lo_hz": chain_cfg.mud_lo_hz,
        "mud_hi_hz": chain_cfg.mud_hi_hz,
        "mud_cut_db": chain_cfg.mud_cut_db,
        "split_low_hz": chain_cfg.split_low_hz,
        "split_high_hz": chain_cfg.split_high_hz,
        "sat_drive_db": chain_cfg.sat_drive_db,
        "sat_mix": chain_cfg.sat_mix,
        "width_side_db": chain_cfg.width_side_db,
        "width_hz": chain_cfg.width_hz,
    }))?;
    let key = IdempotencyKey::derive(
        upstream_digest.as_bytes(),
        &params,
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );
    let cache = crate::cache::RunCache::new(&ctx.root);

    // 命中执行记录：连音频都不必重新处理，仍需把产物放回 CAS 供下游取用
    if let Some(hit) = cache.get(&key) {
        let digest = hit["master_digest"].as_str().unwrap_or("").to_string();
        let size = hit["master_size"].as_u64().unwrap_or(0);
        if !digest.is_empty() {
            let blob = BlobRef {
                digest,
                ext: "wav".into(),
                size,
            };
            // 先取产物再反序列化，避免 hit 被移动后在元组里借用
            let wav = ctx.store.get(&blob);
            let chain = hit
                .get("chain")
                .and_then(|v| serde_json::from_value::<azzer_dsp::ChainReport>(v.clone()).ok())
                .unwrap_or_default();
            if let (Ok(report), Ok(wav)) = (serde_json::from_value::<MasterReport>(hit), wav) {
                if let Ok(buf) = decode_wav_bytes(&wav) {
                    return Ok(N15Result {
                        outcome: NodeOutcome {
                            node: NodeId::N15,
                            key,
                            status: NodeStatus::Done,
                            artifacts: vec![blob.digest.clone()],
                        },
                        buffer: buf,
                        master_blob: blob,
                        report,
                        chain,
                        cache_hit: true,
                        computed: false,
                    });
                }
            }
        }
    }

    // 完整七步：先 1–5（分频工艺），再 6–7（限幅 + 两遍归一）
    let (front, chain_report) = azzer_dsp::apply_front_five(buffer, chain_cfg)?;
    let (mastered, report) = azzer_dsp::master(&front, cfg)?;

    if !report.meets_true_peak || !report.meets_lufs {
        return Err(NodeError::Contract {
            node: NodeId::N15,
            reason: format!(
                "真峰值 {:.2}dBTP（{}）、响度 {:?} LUFS（目标 {:.1}，偏差 {}）",
                report.output_dbtp,
                if report.meets_true_peak { "达标" } else { "超限" },
                report.output.integrated_lufs,
                cfg.target_lufs,
                if report.meets_lufs { "达标" } else { "超 0.5LU" }
            ),
            // 按实际失败项给下一步，不用 N15 表里那条 LRA 专用的回退话术
            next: if !report.meets_true_peak {
                "真峰值限幅未达标，检查限幅器的插值余量是否扣足、迭代轮数是否够".into()
            } else {
                "两遍响度归一未收敛，检查目标响度是否与源动态范围冲突".into()
            },
        });
    }

    let wav = azzer_dsp::encode_wav_24_bytes(&mastered)?;
    let (master_blob, cache_hit) = ctx.store.put_tracked(&wav, "wav")?;

    let lra = report.output.lra_lu.unwrap_or(0.0);
    let status = if lra < 8.0 {
        NodeStatus::Degraded(format!(
            "LRA {lra:.1} LU 低于 8 LU（{}），按 v3 §3.4 告警不阻断",
            if report.output.lra_lu.is_none() { "无法估计" } else { "动态偏窄" }
        ))
    } else {
        NodeStatus::Done
    };

    let mut cache_value = serde_json::to_value(&report)?;
    if let Some(obj) = cache_value.as_object_mut() {
        obj.insert("master_digest".into(), serde_json::json!(master_blob.digest));
        obj.insert("master_size".into(), serde_json::json!(master_blob.size));
        obj.insert("chain".into(), serde_json::to_value(&chain_report)?);
    }
    cache.put(&key, &cache_value);

    Ok(N15Result {
        outcome: NodeOutcome {
            node: NodeId::N15,
            key,
            status,
            artifacts: vec![master_blob.digest.clone()],
        },
        buffer: mastered,
        master_blob,
        report,
        chain: chain_report,
        cache_hit,
        computed: true,
    })
}

/// N09：修复前三步（谱减去噪、条件去混响、高频重建）。
///
/// 与 v3 §3.4 的回退一致：重建不达标就换谐波激励重试一次，仍不达标则
/// **记录修复方式并继续**（不阻断），因为高频略闷比整首做不出来好。
pub fn n09_repair(
    ctx: &PipelineContext,
    n01: &N01Result,
    rt60_s: Option<f64>,
    cfg: &azzer_dsp::RepairConfig,
) -> Result<N09Result, NodeError> {
    let params = serde_json::to_vec(&serde_json::json!({
        "internal_sample_rate": 48000,
        "denoise_over_subtraction": cfg.denoise_over_subtraction,
        "denoise_floor": cfg.denoise_floor,
        "de_reverb_strength": cfg.de_reverb_strength,
        "de_reverb_rt60_gate_s": cfg.de_reverb_rt60_gate_s,
        "hf_lo_hz": cfg.hf_lo_hz,
        "hf_hi_hz": cfg.hf_hi_hz,
        "hf_gain_db": cfg.hf_gain_db,
    }))?;
    let key = IdempotencyKey::derive(
        n01.normalized.digest.as_bytes(),
        &params,
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );
    let cache = crate::cache::RunCache::new(&ctx.root);

    if let Some(hit) = cache.get(&key) {
        let digest = hit["repaired_digest"].as_str().unwrap_or("").to_string();
        let size = hit["repaired_size"].as_u64().unwrap_or(0);
        let report = hit
            .get("report")
            .and_then(|v| serde_json::from_value::<azzer_dsp::RepairReport>(v.clone()).ok());
        if !digest.is_empty() {
            let blob = BlobRef {
                digest,
                ext: "wav".into(),
                size,
            };
            if let (Some(report), Ok(wav)) = (report, ctx.store.get(&blob)) {
                if let Ok(buf) = decode_wav_bytes(&wav) {
                    let status = if report.meets_hf_ratio {
                        NodeStatus::Done
                    } else {
                        NodeStatus::Degraded(format!(
                            "高频重建后 8–16kHz 能量比 {:.2} 低于 0.6，已记录修复方式继续",
                            report.hf_ratio_after
                        ))
                    };
                    return Ok(N09Result {
                        outcome: NodeOutcome {
                            node: NodeId::N09,
                            key,
                            status,
                            artifacts: vec![blob.digest.clone()],
                        },
                        buffer: buf,
                        repaired_blob: blob,
                        report,
                        cache_hit: true,
                        computed: false,
                    });
                }
            }
        }
    }

    let (repaired, report) = azzer_dsp::repair(&n01.buffer, cfg, rt60_s)?;
    let wav = encode_wav_24_bytes(&repaired)?;
    let (blob, cache_hit) = ctx.store.put_tracked(&wav, "wav")?;

    let status = if report.meets_hf_ratio {
        NodeStatus::Done
    } else {
        NodeStatus::Degraded(format!(
            "高频重建后 8–16kHz 能量比 {:.2} 低于 0.6，已记录修复方式继续",
            report.hf_ratio_after
        ))
    };

    let mut cache_value = serde_json::json!({
        "repaired_digest": blob.digest,
        "repaired_size": blob.size,
        "report": serde_json::to_value(&report)?,
    });
    if let Some(obj) = cache_value.as_object_mut() {
        obj.insert("rt60_s".into(), serde_json::json!(rt60_s));
    }
    cache.put(&key, &cache_value);

    Ok(N09Result {
        outcome: NodeOutcome {
            node: NodeId::N09,
            key,
            status,
            artifacts: vec![blob.digest.clone()],
        },
        buffer: repaired,
        repaired_blob: blob,
        report,
        cache_hit,
        computed: true,
    })
}

#[derive(Debug)]
pub struct N09Result {
    pub outcome: NodeOutcome,
    pub buffer: AudioBuffer,
    pub repaired_blob: BlobRef,
    pub report: azzer_dsp::RepairReport,
    pub cache_hit: bool,
    pub computed: bool,
}

/// N05：基频与清浊音提取（零模型路径，YIN + 自相关双路）。
///
/// 契约断言：双路差异 >600 cents 的帧占比 ≤1%。
/// 不改变音频，只产出 f0 数据供 N11 与后续诊断使用。
///
/// **A 入口不做分离**，所以这里是在未分离的全混音上提取的，伴奏会干扰。
/// 报告里的 `meets_route_contract` 会如实反映可信度。
/// 用 RMVPE 算 f0。输入是 48k 的 buffer，内部降到 16k。
///
/// 降采样不是简单抽取：48k → 16k 是 3:1，直接 `step_by(3)` 会把 8–24kHz 的
/// 成分折回 0–8kHz，而 RMVPE 的 mel 正好覆盖 30–8000Hz，混叠会直接污染 f0。
/// 所以先过两级低通（合起来 4 阶，约 −24dB/oct）再抽取。
fn f0_via_rmvpe(
    buffer: &AudioBuffer,
    model_path: &std::path::Path,
) -> Result<azzer_dsp::F0Track, NodeError> {
    let mono = buffer.mono();
    let fs = buffer.sample_rate;

    let down: Vec<f32> = if fs == azzer_infer::RMVPE_SR {
        mono
    } else {
        let mut lp1 =
            azzer_dsp::biquad::lowpass(fs as f64, 7000.0, std::f64::consts::FRAC_1_SQRT_2);
        let mut lp2 =
            azzer_dsp::biquad::lowpass(fs as f64, 7000.0, std::f64::consts::FRAC_1_SQRT_2);
        let filtered: Vec<f32> = mono
            .iter()
            .map(|&v| lp2.process(lp1.process(v)))
            .collect();
        let ratio = (fs as f64 / azzer_infer::RMVPE_SR as f64).round().max(1.0) as usize;
        filtered.iter().step_by(ratio).copied().collect()
    };

    let mut model = azzer_infer::Rmvpe::load(model_path).map_err(|e| {
        NodeError::Model(format!("RMVPE 加载失败（{}）：{e}", model_path.display()))
    })?;
    let trace = model
        .infer(&down)
        .map_err(|e| NodeError::Model(format!("RMVPE 推理失败：{e}")))?;

    let n = trace.f0_hz.len();
    let voiced = trace.f0_hz.iter().filter(|v| v.is_some()).count();
    Ok(azzer_dsp::F0Track {
        hop_s: trace.hop_s,
        voicing: trace.f0_hz.iter().map(|v| v.is_some()).collect(),
        f0_hz: trace.f0_hz,
        confidence: trace.confidence,
        // RMVPE 是单路，没有「双路分歧」这回事。填 0 并靠 source 字段区分语义，
        // 免得下游把「没有交叉校验」误读成「两路完全一致」。
        route_disagreement_ratio: 0.0,
        meets_route_contract: true,
        voiced_frames: voiced,
        total_frames: n,
        source: "rmvpe".into(),
    })
}

pub fn n05_f0extract(
    ctx: &PipelineContext,
    upstream_digest: &str,
    buffer: &AudioBuffer,
    cfg: &azzer_dsp::F0Config,
) -> Result<N05Result, NodeError> {
    // 有声明且文件存在的 f0 模型就用 RMVPE；否则回落自研双路。
    // 这是「模型可选」的具体落点：两条路都合法，缺模型不是错误。
    let f0_model = ctx
        .models
        .declared_for(azzer_core::ModelRole::F0)
        .filter(|m| ctx.models.presence(m).is_usable())
        .map(|m| m.path.clone());

    let params = serde_json::to_vec(&serde_json::json!({
        "internal_sample_rate": 48000,
        "hop_ms": cfg.hop_ms,
        "window_ms": cfg.window_ms,
        "f0_min_hz": cfg.f0_min_hz,
        "f0_max_hz": cfg.f0_max_hz,
        "yin_threshold": cfg.yin_threshold,
        "aperiodicity_max": cfg.aperiodicity_max,
        "route_disagree_cents": cfg.route_disagree_cents,
        // 取峰策略与后处理同样改变结果，必须进幂等键。
        // 漏掉它们的话，改完实现再跑会命中旧缓存，看到的是旧数字——
        // 这个坑在 2026-11-15 调 f0 时差点踩上。
        "peak_rel_threshold": cfg.peak_rel_threshold,
        "median_window": cfg.median_window,
        // 换模型（或模型文件被替换）必须让旧结果失效。
        // 只放路径不够——同一个路径换了权重是常见操作，所以带上修改时间。
        "f0_model": f0_model.as_ref().map(|p| {
            let mtime = std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}@{}", p.display(), mtime)
        }),
    }))?;
    let key = IdempotencyKey::derive(
        upstream_digest.as_bytes(),
        &params,
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );
    let cache = crate::cache::RunCache::new(&ctx.root);

    if let Some(hit) = cache.get(&key) {
        let digest = hit["f0_digest"].as_str().unwrap_or("").to_string();
        let size = hit["f0_size"].as_u64().unwrap_or(0);
        if !digest.is_empty() {
            let blob = BlobRef {
                digest,
                ext: "json".into(),
                size,
            };
            if let Ok(bytes) = ctx.store.get(&blob) {
                if let Ok(track) = serde_json::from_slice::<azzer_dsp::F0Track>(&bytes) {
                    let status = if track.meets_route_contract {
                        NodeStatus::Done
                    } else {
                        NodeStatus::Degraded(format!(
                            "双路不一致率 {:.2}% 超过 1%",
                            track.route_disagreement_ratio * 100.0
                        ))
                    };
                    return Ok(N05Result {
                        outcome: NodeOutcome {
                            node: NodeId::N05,
                            key,
                            status,
                            artifacts: vec![blob.digest.clone()],
                        },
                        track,
                        f0_blob: blob,
                        cache_hit: true,
                        computed: false,
                    });
                }
            }
        }
    }

    let track = match &f0_model {
        Some(path) => f0_via_rmvpe(buffer, path)?,
        None => azzer_dsp::extract_f0(buffer, cfg)?,
    };
    let json = serde_json::to_vec(&track)?;
    let (blob, cache_hit) = ctx.store.put_tracked(&json, "json")?;

    let status = if track.meets_route_contract {
        NodeStatus::Done
    } else {
        NodeStatus::Degraded(format!(
            "双路不一致率 {:.2}% 超过 1%，f0 可信度受限",
            track.route_disagreement_ratio * 100.0
        ))
    };

    cache.put(
        &key,
        &serde_json::json!({
            "f0_digest": blob.digest,
            "f0_size": blob.size,
        }),
    );

    Ok(N05Result {
        outcome: NodeOutcome {
            node: NodeId::N05,
            key,
            status,
            artifacts: vec![blob.digest.clone()],
        },
        track,
        f0_blob: blob,
        cache_hit,
        computed: true,
    })
}

#[derive(Debug)]
pub struct N05Result {
    pub outcome: NodeOutcome,
    pub track: azzer_dsp::F0Track,
    pub f0_blob: BlobRef,
    pub cache_hit: bool,
    pub computed: bool,
}

/// N16：37 项评估。
///
/// **不做执行记录缓存**：它依赖端到端耗时这类运行属性，同一份音频两次跑
/// 结论本就不同；而且评估本身很便宜（几项扫描），省它的收益小于缓存带来的
/// 语义混乱。这与 N01–N15 的处理不同，是刻意的。
pub fn n16_eval(
    ctx: &PipelineContext,
    before: &AudioBuffer,
    after: &AudioBuffer,
    grade: &GradeMetrics,
    out_loudness: &azzer_dsp::LoudnessReport,
    out_dbtp: f64,
    f0: &azzer_dsp::F0Track,
    elapsed_ms: u128,
    cache_hit: bool,
    upstream_digest: &str,
) -> Result<N16Result, NodeError> {
    let report = azzer_dsp::evaluate(&azzer_dsp::EvalInput {
        before,
        after,
        grade,
        out_loudness,
        out_dbtp,
        f0,
        elapsed_ms,
        cache_hit,
    });
    let json = serde_json::to_vec(&report)?;
    let (blob, cache_hit_cas) = ctx.store.put_tracked(&json, "json")?;
    let key = IdempotencyKey::derive(
        upstream_digest.as_bytes(),
        br#"{"internal_sample_rate":48000,"metrics":37}"#,
        &ctx.engine.version,
        &ctx.engine.adapter_hash,
    );

    let status = if report.failing.is_empty() {
        NodeStatus::Done
    } else {
        NodeStatus::Degraded(format!(
            "{} 项未达标：{}",
            report.failing.len(),
            report.failing.join("、")
        ))
    };

    Ok(N16Result {
        outcome: NodeOutcome {
            node: NodeId::N16,
            key,
            status,
            artifacts: vec![blob.digest.clone()],
        },
        report,
        eval_blob: blob,
        cache_hit: cache_hit_cas,
    })
}

#[derive(Debug)]
pub struct N16Result {
    pub outcome: NodeOutcome,
    pub report: azzer_dsp::EvalReport,
    pub eval_blob: BlobRef,
    pub cache_hit: bool,
}

#[derive(Debug)]
pub struct N15Result {
    pub outcome: NodeOutcome,
    pub buffer: AudioBuffer,
    pub master_blob: BlobRef,
    pub report: MasterReport,
    pub chain: azzer_dsp::ChainReport,
    pub cache_hit: bool,
    pub computed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use azzer_dsp::write_wav_24;

    fn fixture(root: &Path, secs: f64, amp: f32) -> std::path::PathBuf {
        let sr = 48_000u32;
        let n = (sr as f64 * secs) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (2.0 * std::f64::consts::PI * 220.0 * t).sin() as f32 * amp
            })
            .collect();
        let b = AudioBuffer::new(vec![v], sr, 24).unwrap();
        fs::create_dir_all(root).unwrap();
        let p = root.join("source.wav");
        write_wav_24(&p, &b).unwrap();
        p
    }

    fn test_root(name: &str) -> std::path::PathBuf {
        std::env::current_dir().unwrap().join(".test-pipeline").join(name)
    }

    #[test]
    fn short_source_is_rejected_with_actionable_message() {
        let root = test_root("short");
        let src = fixture(&root, 0.5, 0.5);
        let ctx = PipelineContext::open(&root).unwrap();
        let e = n01_ingest(&ctx, &src).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("N01 契约断言未通过"), "{msg}");
        assert!(msg.contains("下一步"), "{msg}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn too_quiet_source_is_rejected() {
        let root = test_root("quiet");
        let src = fixture(&root, 4.0, 0.00001); // << -40dBFS
        let ctx = PipelineContext::open(&root).unwrap();
        let e = n01_ingest(&ctx, &src).unwrap_err();
        assert!(e.to_string().contains("峰值"), "{}", e);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn r1_runs_with_zero_models_and_hits_cache_on_second_run() {
        let root = test_root("full");
        let src = fixture(&root, 4.0, 0.4);
        let ctx = PipelineContext::open(&root).unwrap();

        let first = crate::run_r1(&ctx, &src).unwrap();
        assert!(!first.n01_cache_hit);
        assert!(!first.n02_cache_hit);
        assert!(first.n02_computed, "首次必须真的算一遍");
        assert_eq!(first.models_declared, 0, "零模型是正常状态");
        assert!(first.capabilities.direct, "A 入口必须可用");
        assert!(!first.capabilities.separate);
        assert!(first.path_note.contains("未触发任何模型下载"));

        let second = crate::run_r1(&ctx, &src).unwrap();
        assert!(second.n01_cache_hit, "同样输入应命中 CAS");
        assert!(second.n02_cache_hit, "同样参数应命中 CAS");
        assert!(!second.n02_computed, "命中执行记录就该跳过五项计算");
        assert_eq!(first.n01_key, second.n01_key, "幂等键必须稳定");
        assert_eq!(first.n02_key, second.n02_key);
        // 跳过计算不能改变结论
        assert_eq!(first.grade, second.grade, "缓存命中后指标必须完全一致");

        assert!(!ctx.models_json_path().exists(), "软件不能替用户创建模型声明");
        let _ = fs::remove_dir_all(&root);
    }

    /// 单侧有信号的立体声不该被 N01 拒绝。
    /// 旧实现用单声道平均算峰值，会把 -34.4dBFS 算成 -40.4dBFS 而误拒。
    #[test]
    fn stereo_with_one_silent_channel_is_accepted() {
        let root = test_root("stereo-one-silent");
        fs::create_dir_all(&root).unwrap();
        let sr = 48_000u32;
        let n = sr as usize * 4;
        let left: Vec<f32> = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * 220.0 * i as f64 / sr as f64).sin() as f32 * 0.019
            })
            .collect();
        let right = vec![0.0f32; n];
        let b = AudioBuffer::new(vec![left, right], sr, 24).unwrap();
        let p = root.join("stereo.wav");
        write_wav_24(&p, &b).unwrap();
        let ctx = PipelineContext::open(&root).unwrap();
        let r = n01_ingest(&ctx, &p);
        assert!(r.is_ok(), "单侧有信号的立体声不应被拒：{:?}", r.err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn changing_source_changes_n01_key() {
        let root = test_root("keys");
        let src = fixture(&root, 4.0, 0.4);
        let ctx = PipelineContext::open(&root).unwrap();
        let a = n01_ingest(&ctx, &src).unwrap();

        let src2 = fixture(&root, 4.0, 0.45);
        let b = n01_ingest(&ctx, &src2).unwrap();
        assert_ne!(a.outcome.key, b.outcome.key);
        let _ = fs::remove_dir_all(&root);
    }
}
