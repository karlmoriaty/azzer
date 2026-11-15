//! N16 评估：37 项指标（v3 §8.1）。
//!
//! ## 铁律：37 项一个不能少，缺项标 `not_applicable`，不许填 0
//!
//! v3 §3.3 明确要求。填 0 会让「没算」和「算出来是 0」无法区分，
//! 下游的诊断树会据此做出错误判断。所以这里用枚举：
//!
//! ```text
//! MetricValue::Value(f64)             真的算出来了
//! MetricValue::NotApplicable(&str)    算不了，附原因
//! ```
//!
//! `NotApplicable` 必须带原因，且原因要说清「缺什么才能算」，
//! 不能是一句「暂不支持」。
//!
//! ## 本轮能算什么
//!
//! | 组 | 能算 | 不能算及原因 |
//! |---|---|---|
//! | 拟真度 | HNR、底噪、HumanizeScore 的 H1/H3/H5、八度错误率、音域越界率 | H2/H4/H6 需要 onset 与呼吸检测；AI-Detectability 需要判别器模型 |
//! | 母带 | 真峰值、LRA、积分响度 | — |
//! | 延迟 | 端到端耗时、缓存命中 | 交互类指标需要真实用户操作数据 |
//! | 生成质量 / 音色 / 情感 | — | 全部依赖模型（FAD、CLAP、SECS、EDR） |
//! | 未修改部分 | — | A 入口没有「未修改部分」这个概念（源即全部） |
//! | 可控性 / 接缝 | — | 需要多轮交互与人工评审数据 |
//!
//! 这些 not_applicable 是**如实记录**，不是掩盖。等模型接入后自然会填上。

use serde::Serialize;

use crate::buffer::AudioBuffer;
use crate::f0::F0Track;
use crate::loudness::LoudnessReport;
use crate::metrics::GradeMetrics;

/// 指标值。含 `&'static str` 的原因文本，所以**只序列化、不反序列化**——
/// 评估是末端节点，报告只写不读；要读回就重新算一次（它很快）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum MetricValue {
    Value(f64),
    NotApplicable(&'static str),
}

impl MetricValue {
    pub fn as_value(&self) -> Option<f64> {
        match self {
            MetricValue::Value(v) => Some(*v),
            MetricValue::NotApplicable(_) => None,
        }
    }
    pub fn is_value(&self) -> bool {
        matches!(self, MetricValue::Value(_))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalItem {
    pub group: &'static str,
    pub name: &'static str,
    pub target: &'static str,
    pub value: MetricValue,
    /// 是否达标。NotApplicable 时为 None（不是 false，不冤枉也不放过）。
    pub meets: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalReport {
    pub items: Vec<EvalItem>,
    /// 有实测值的项数。
    pub measured: usize,
    /// 标 not_applicable 的项数。
    pub not_applicable: usize,
    /// 有值且不达标的项名，直接给用户看。
    pub failing: Vec<String>,
    /// HumanizeScore 六子项里已经算出来的部分（不占 37 项名额，供诊断）。
    pub humanize_parts: Vec<(String, f64)>,
}

impl EvalReport {
    pub fn get(&self, name: &str) -> Option<&EvalItem> {
        self.items.iter().find(|i| i.name == name)
    }
}

/// 底噪（dBFS）：取帧 RMS 的最低 5% 均值。
fn noise_floor_dbfs(mono: &[f32], sr: u32) -> f64 {
    let frame = (0.02 * sr as f64).round().max(1.0) as usize;
    let hop = frame / 2;
    if mono.len() < frame {
        return -120.0;
    }
    let mut rms: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + frame <= mono.len() {
        let mut s = 0.0f64;
        for &x in &mono[i..i + frame] {
            s += (x as f64) * (x as f64);
        }
        rms.push((s / frame as f64).sqrt());
        i += hop;
    }
    if rms.is_empty() {
        return -120.0;
    }
    rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = (rms.len() / 20).max(1);
    let mean: f64 = rms[..n].iter().sum::<f64>() / n as f64;
    20.0 * mean.max(1e-12).log10()
}

/// 八度错误率：相邻浊音帧的 f0 跳变超过 600 cents 的比例。
/// 真人不会一帧之内跳 5 个半音以上，出现就是八度误判。
fn octave_error_rate(track: &F0Track) -> f64 {
    let mut jumps = 0usize;
    let mut pairs = 0usize;
    let mut prev: Option<f64> = None;
    for f in &track.f0_hz {
        match (prev, f) {
            (Some(a), Some(b)) if a > 0.0 && *b > 0.0 => {
                pairs += 1;
                if (1200.0 * (b / a).log2()).abs() > 600.0 {
                    jumps += 1;
                }
            }
            _ => {}
        }
        prev = *f;
    }
    if pairs == 0 {
        0.0
    } else {
        jumps as f64 / pairs as f64
    }
}

/// 音域越界率：f0 超出通用人声范围（65–1100Hz）的浊音帧比例。
fn range_overflow_rate(track: &F0Track) -> f64 {
    let voiced: Vec<f64> = track.f0_hz.iter().flatten().copied().collect();
    if voiced.is_empty() {
        return 0.0;
    }
    let out = voiced.iter().filter(|&&f| f < 65.0 || f > 1100.0).count();
    out as f64 / voiced.len() as f64
}

/// 音准偏差的标准差（cents）——HumanizeScore 的 H1 输入。
///
/// 定义：每个浊音帧的 f0 相对最近十二平均律半音的偏差，取标准差。
/// 真人约 8–25 cents，AI 生成约 0–3。
fn cents_deviation_std(track: &F0Track) -> Option<f64> {
    let mut dev: Vec<f64> = Vec::new();
    for &f in track.f0_hz.iter().flatten() {
        if f <= 0.0 {
            continue;
        }
        // 相对 A4=440 的半音数
        let semis = 12.0 * (f / 440.0).log2();
        let nearest = semis.round();
        dev.push((semis - nearest) * 100.0);
    }
    if dev.len() < 8 {
        return None;
    }
    let mean = dev.iter().sum::<f64>() / dev.len() as f64;
    let var = dev.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / dev.len() as f64;
    Some(var.sqrt())
}

/// `in_range`：落在区间内得 1，落到 0（两端线性衰减到 1/4 区间宽度处）。
/// 这是"中间值最好"类指标的通用形状，v3 的六个 H 子项都用它。
pub fn in_range(v: f64, lo: f64, hi: f64) -> f64 {
    if !v.is_finite() || hi <= lo {
        return 0.0;
    }
    let span = hi - lo;
    if v >= lo && v <= hi {
        return 1.0;
    }
    let d = if v < lo { lo - v } else { v - hi };
    let falloff = span / 4.0;
    (1.0 - d / falloff).clamp(0.0, 1.0)
}

/// 谐噪比（HNR，dB）：用归一化自相关峰值估计周期性。
///
/// `HNR = 10·log10(r / (1 - r))`，r 是自相关峰值。r→1 表示纯周期信号。
fn hnr_db(mono: &[f32], sr: u32) -> Option<f64> {
    let frame = (0.04 * sr as f64).round() as usize;
    if mono.len() < frame * 2 {
        return None;
    }
    // 取中间帧，避免首尾静音
    let start = (mono.len() / 2).saturating_sub(frame / 2);
    let seg = &mono[start..start + frame];
    let energy: f64 = seg.iter().map(|&v| (v as f64) * (v as f64)).sum();
    if energy < 1e-9 {
        return None;
    }
    let tau_min = (sr as f64 / 1100.0) as usize;
    let tau_max = ((sr as f64 / 65.0) as usize).min(frame / 2);
    let mut best = 0.0f64;
    for tau in tau_min..tau_max {
        let mut num = 0.0f64;
        let mut e2 = 0.0f64;
        for j in 0..(frame - tau) {
            num += seg[j] as f64 * seg[j + tau] as f64;
            e2 += (seg[j + tau] as f64) * (seg[j + tau] as f64);
        }
        if e2 > 0.0 {
            best = best.max(num / (energy * e2).sqrt());
        }
    }
    if best <= 0.0 || best >= 1.0 {
        return None;
    }
    Some(10.0 * (best / (1.0 - best)).log10())
}

/// 评估的输入。
pub struct EvalInput<'a> {
    /// 母带前的信号（用于对比底噪是否被改变）。
    pub before: &'a AudioBuffer,
    /// 最终输出。
    pub after: &'a AudioBuffer,
    pub grade: &'a GradeMetrics,
    pub out_loudness: &'a LoudnessReport,
    pub out_dbtp: f64,
    pub f0: &'a F0Track,
    pub elapsed_ms: u128,
    pub cache_hit: bool,
}

/// 组装 37 项。**顺序与 v3 §8.1 一致，一项不少。**
pub fn evaluate(input: &EvalInput) -> EvalReport {
    let mut items: Vec<EvalItem> = Vec::with_capacity(37);
    let sr = input.after.sample_rate;

    let mono_after = input.after.mono();
    let mono_before = input.before.mono();
    let nf_after = noise_floor_dbfs(&mono_after, sr);
    let nf_before = noise_floor_dbfs(&mono_before, sr);
    let hnr = hnr_db(&mono_after, sr);
    let octave = octave_error_rate(input.f0);
    let range_over = range_overflow_rate(input.f0);
    let cents_std = cents_deviation_std(input.f0);
    let lra = input.out_loudness.lra_lu;

    let mut push = |group: &'static str,
                    name: &'static str,
                    target: &'static str,
                    value: MetricValue,
                    meets: Option<bool>| {
        items.push(EvalItem {
            group,
            name,
            target,
            value,
            meets,
        });
    };

    // ---- 生成质量（需要模型）
    push("生成质量", "FAD", "≤base×1.2", MetricValue::NotApplicable("需要 FAD 参考分布，零模型路径没有"), None);
    push("生成质量", "CLAP score", "≥0.30", MetricValue::NotApplicable("需要 CLAP 模型"), None);
    push("生成质量", "结构合规率", "≥90%", MetricValue::NotApplicable("A 入口不做结构生成"), None);
    push("生成质量", "MOS", "≥3.6", MetricValue::NotApplicable("需要主观评测或 MOS 模型"), None);

    // ---- 符号编辑（需要参考轨）
    push("符号编辑", "TPA", "≥0.90", MetricValue::NotApplicable("需要参考 MIDI/乐谱"), None);
    push("符号编辑", "TPA_hard", "≥0.75", MetricValue::NotApplicable("需要参考 MIDI/乐谱"), None);
    push("符号编辑", "OnsetErr", "≤30ms", MetricValue::NotApplicable("需要参考 onset 网格"), None);

    // ---- 未修改部分（A 入口没有这个概念）
    push("未修改部分", "UPS_emb", "≥0.97", MetricValue::NotApplicable("A 入口源即全部，无未修改区段"), None);
    push("未修改部分", "UPS_lsd", "≤1.5dB", MetricValue::NotApplicable("A 入口源即全部，无未修改区段"), None);
    push("未修改部分", "UPS_wave", "=1.0", MetricValue::NotApplicable("仅 graft 模式适用"), None);

    // ---- 翻唱音色（需要模型）
    push("翻唱音色", "SECS", "≥0.82", MetricValue::NotApplicable("需要说话人编码器模型"), None);
    push("翻唱音色", "ΔSECS", "≥0.25", MetricValue::NotApplicable("需要说话人编码器模型"), None);
    push("翻唱音色", "WER", "≤5%", MetricValue::NotApplicable("需要 ASR 模型"), None);

    // ---- 情感细节
    push("情感细节", "EDR 综合", "≥0.80", MetricValue::NotApplicable("需要情感嵌入模型"), None);

    // ---- 翻唱保真
    push("翻唱保真", "SFR", "pitch ≥0.95/time ≤25ms", MetricValue::NotApplicable("仅 replace 模式适用"), None);

    // ---- 拟真度（本轮能算的部分）
    //
    // 注意：v3 §8.1 的 37 项里 HumanizeScore **只算一项**，六子项是它的
    // 内部分解。早先把它拆成 H1–H6 六个 push，总数变成 42，与契约对不上。
    // 这里的处理：HumanizeScore 一项；六子项齐全才算加权值，缺项 → not_applicable。
    let onsets = crate::percept::detect_onsets(
        input.after,
        &crate::percept::PerceptConfig::default(),
    );
    let breaths = crate::percept::detect_breaths(
        input.after,
        input.f0,
        &crate::percept::PerceptConfig::default(),
    );
    let vibrato = crate::percept::detect_vibrato(input.f0, &crate::percept::PerceptConfig::default());

    // 六子项。每个都可能为 None（数据不足），齐全才加权。
    let h1 = cents_std.map(|c| in_range(c, 8.0, 25.0));
    // H2 用「相邻起音间隔的变异系数」而不是绝对标准差。
    //
    // v3 原文写的是 `std(onset_measured - onset_grid)`，需要节拍网格；
    // A 入口拿不到乐谱，R7 改成了「相邻间隔标准差」，却沿用了 12–35ms 的阈值。
    // 那是给「相对网格的偏差」定的量纲，用在「音符时值的离散度」上必然不达标：
    // 实测一条真实演唱是 164ms，而这不是缺陷——真人时值本就长短不一。
    // 归一化后才有可比性：真人 CV 约 0.2–0.8，机械节拍接近 0。
    let h2 = match (onsets.jitter_ms, onsets.mean_ioi_ms) {
        (Some(j), Some(m)) if m > 0.0 => Some(in_range(j / m, 0.15, 0.80)),
        _ => None,
    };
    let h3 = lra.map(|l| in_range(l, 8.0, 12.0));
    // H4 不返回 Option：数不出呼吸就是 0 次，而 0 次本身就是「不像真人」的证据
    let h4 = Some(in_range(breaths.per_minute, 4.0, 12.0));
    let h5 = Some(in_range(nf_after, -62.0, -48.0));
    let h6 = vibrato.map(|v| {
        // 速率与深度都要在范围内；两项各占一半，任一越界都会拉低
        (in_range(v.rate_hz, 4.0, 7.0) + in_range(v.depth_cents, 20.0, 60.0)) * 0.5
    });

    let humanize = match (h1, h2, h3, h4, h5, h6) {
        (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) => {
            Some(0.25 * a + 0.15 * b + 0.15 * c + 0.20 * d + 0.15 * e + 0.10 * f)
        }
        _ => None,
    };

    let mut humanize_parts: Vec<(&'static str, f64)> = Vec::new();
    if let Some(c) = cents_std {
        humanize_parts.push(("H1_音准偏差std_cents", c));
    }
    if let Some(j) = onsets.jitter_ms {
        humanize_parts.push(("H2_起音间隔jitter_ms", j));
        // 同时给出归一化后的 CV，H2 的打分用的是它，保留原始 jitter 便于核对
        if let Some(m) = onsets.mean_ioi_ms {
            if m > 0.0 {
                humanize_parts.push(("H2b_起音间隔CV", j / m));
            }
        }
    }
    if let Some(l) = lra {
        humanize_parts.push(("H3_LRA_LU", l));
    }
    humanize_parts.push(("H4_呼吸次数每分", breaths.per_minute));
    humanize_parts.push(("H5_底噪_dBFS", nf_after));
    if let Some(v) = vibrato {
        humanize_parts.push(("H6a_颤音速率Hz", v.rate_hz));
        humanize_parts.push(("H6b_颤音深度cents", v.depth_cents));
    }

    push(
        "拟真度",
        "HumanizeScore",
        "≥0.75",
        match humanize {
            Some(v) => MetricValue::Value(v),
            None => MetricValue::NotApplicable(
                "六子项未齐：H1 需≥8 个浊音帧、H2 需≥3 个起音、H3 需≥3 秒、H6 需≥0.3 秒且有周期性的浊音段；\
                 逐项实测值见 humanize_parts",
            ),
        },
        humanize.map(|v| v >= 0.75),
    );
    push(
        "拟真度",
        "HNR",
        "与源偏离 ≤3dB",
        match hnr {
            Some(h) => MetricValue::Value(h),
            None => MetricValue::NotApplicable("信号能量过低或无周期性，算不出谐噪比"),
        },
        // 没有源的 HNR（同一段信号前后不可比），先只报数值不判达标
        None,
    );
    push(
        "拟真度",
        "NoiseFloorMatch",
        "≤6dB（相对源）",
        MetricValue::Value(nf_after - nf_before),
        Some((nf_after - nf_before).abs() <= 6.0),
    );
    // 呼吸保留率：母带前后的呼吸事件数之比。
    // 注意这不是「整条链路的呼吸保留率」——before 已经是 N09 修复后的信号，
    // 分离与转换阶段的呼吸损失不在这个数里。A 入口不过 RVC，所以够用。
    let before_breaths = crate::percept::detect_breaths(
        input.before,
        input.f0,
        &crate::percept::PerceptConfig::default(),
    );
    let breath_retention = if before_breaths.starts_s.is_empty() {
        None
    } else {
        Some(breaths.starts_s.len() as f64 / before_breaths.starts_s.len() as f64)
    };
    push(
        "拟真度",
        "BreathRetention",
        "≥0.90",
        match breath_retention {
            Some(v) => MetricValue::Value(v),
            None => MetricValue::NotApplicable("母带前的信号里没有检到呼吸事件，无从计算保留率"),
        },
        breath_retention.map(|v| v >= 0.90),
    );
    push("拟真度", "AI-Detectability", "≤0.35", MetricValue::NotApplicable("需要判别器模型"), None);

    // ---- RVC 质量（A 入口不过 RVC，但八度/音域可算）
    push(
        "RVC 质量",
        "OctaveErrRate",
        "≤1%",
        MetricValue::Value(octave),
        Some(octave <= 0.01),
    );
    push(
        "RVC 质量",
        "RangeOverflowRate",
        "≤3%",
        MetricValue::Value(range_over),
        Some(range_over <= 0.03),
    );
    push("RVC 质量", "ConsonantClarity", "≥0.6", MetricValue::NotApplicable("需要辅音检测"), None);
    push("RVC 质量", "TransientRatio", "≥0.75", MetricValue::NotApplicable("需要瞬态检测"), None);

    // ---- 延迟
    push(
        "延迟",
        "L_full",
        "≤20min",
        MetricValue::Value(input.elapsed_ms as f64 / 1000.0),
        Some(input.elapsed_ms <= 20 * 60 * 1000),
    );
    push("延迟", "L_edit P50", "≤8s", MetricValue::NotApplicable("需要交互编辑会话数据"), None);
    push("延迟", "L_edit P95", "≤20s", MetricValue::NotApplicable("需要交互编辑会话数据"), None);
    push("延迟", "TTFA", "≤3s", MetricValue::NotApplicable("需要交互会话数据"), None);
    push(
        "延迟",
        "cache_hit",
        "≥40%",
        MetricValue::Value(if input.cache_hit { 1.0 } else { 0.0 }),
        Some(input.cache_hit),
    );

    // ---- 可控性 / 接缝（需要人工评审）
    push("可控性", "EditAcceptRate", "≥60%", MetricValue::NotApplicable("需要多轮交互数据"), None);
    push("可控性", "RoundsToAccept", "≤4", MetricValue::NotApplicable("需要多轮交互数据"), None);
    push("可控性", "BlastRadius", "≤1.5", MetricValue::NotApplicable("需要多轮交互数据"), None);
    push("可控性", "PlanEditRatio", "≥0.5", MetricValue::NotApplicable("需要多轮交互数据"), None);
    push("可控性", "SUS", "≥75", MetricValue::NotApplicable("需要用户问卷"), None);
    push("接缝", "SeamBlindRate", "≤10%", MetricValue::NotApplicable("需要人工盲测"), None);

    // ---- 母带
    push(
        "母带",
        "真峰值",
        "≤-1.0 dBTP",
        MetricValue::Value(input.out_dbtp),
        Some(input.out_dbtp <= -1.0 + 0.05),
    );
    push(
        "母带",
        "LRA",
        "≥8 LU",
        match lra {
            Some(l) => MetricValue::Value(l),
            None => MetricValue::NotApplicable("无法估计 LRA"),
        },
        lra.map(|l| l >= 8.0),
    );

    let measured = items.iter().filter(|i| i.value.is_value()).count();
    let not_applicable = items.iter().filter(|i| !i.value.is_value()).count();
    let failing: Vec<String> = items
        .iter()
        .filter(|i| i.meets == Some(false))
        .map(|i| format!("{}/{}", i.group, i.name))
        .collect();

    EvalReport {
        items,
        measured,
        not_applicable,
        failing,
        humanize_parts: humanize_parts
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}

/// 37 项的名称清单，供测试与报表使用。
pub const EXPECTED_METRIC_COUNT: usize = 37;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_range_is_one_inside_and_decays_outside() {
        assert_eq!(in_range(10.0, 8.0, 12.0), 1.0);
        assert_eq!(in_range(8.0, 8.0, 12.0), 1.0);
        assert_eq!(in_range(12.0, 8.0, 12.0), 1.0);
        assert!(in_range(7.0, 8.0, 12.0) < 1.0);
        assert!(in_range(13.0, 8.0, 12.0) < 1.0);
        assert_eq!(in_range(0.0, 8.0, 12.0), 0.0);
        assert_eq!(in_range(f64::NAN, 8.0, 12.0), 0.0);
    }

    #[test]
    fn in_range_rejects_invalid_interval() {
        assert_eq!(in_range(5.0, 12.0, 8.0), 0.0);
        assert_eq!(in_range(5.0, 8.0, 8.0), 0.0);
    }

    #[test]
    fn noise_floor_of_silence_is_very_low() {
        let s = vec![0.0f32; 48_000];
        assert!(noise_floor_dbfs(&s, 48_000) < -200.0);
    }

    #[test]
    fn noise_floor_reflects_low_level_content() {
        // 幅度 0.001 的白噪约 -63dBFS
        let mut seed = 7u64;
        let s: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5) as f32 * 0.002
            })
            .collect();
        let nf = noise_floor_dbfs(&s, 48_000);
        assert!((-75.0..-55.0).contains(&nf), "应落在 -75..-55dBFS，实得 {nf:.1}");
    }

    /// 契约测试：必须正好 37 项，一项不少也一项不多。
    /// v3 §3.3 要求「37 项全算出；缺项标 not_applicable，不许填 0」。
    #[test]
    fn evaluate_always_returns_exactly_37_items() {
        use crate::f0::{extract_f0, F0Config};
        use crate::loudness::measure;
        use crate::metrics::analyze;

        let sr = 48_000u32;
        let n = sr as usize * 3;
        let v: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let mut s = 0.0;
                for h in 1..=4 {
                    s += (2.0 * std::f64::consts::PI * 220.0 * h as f64 * t).sin() / h as f64;
                }
                (s * 0.15) as f32
            })
            .collect();
        let buf = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let grade = analyze(&buf);
        let loud = measure(&buf).unwrap();
        let track = extract_f0(&buf, &F0Config::default()).unwrap();
        let dbtp = crate::truepeak::true_peak_dbtp(&buf);

        let report = evaluate(&EvalInput {
            before: &buf,
            after: &buf,
            grade: &grade,
            out_loudness: &loud,
            out_dbtp: dbtp,
            f0: &track,
            elapsed_ms: 1234,
            cache_hit: false,
        });

        assert_eq!(
            report.items.len(),
            EXPECTED_METRIC_COUNT,
            "必须正好 {EXPECTED_METRIC_COUNT} 项，实得 {}：{:?}",
            report.items.len(),
            report.items.iter().map(|i| i.name).collect::<Vec<_>>()
        );
        assert_eq!(report.measured + report.not_applicable, EXPECTED_METRIC_COUNT);
        // 不许把算不出的项填成 0
        for item in &report.items {
            if let MetricValue::NotApplicable(reason) = &item.value {
                assert!(!reason.is_empty(), "{} 的 not_applicable 必须带原因", item.name);
            }
        }
    }

    /// 有值的项里，母带两项必须算出来（这是 M1 的硬门禁）。
    #[test]
    fn mastering_metrics_are_always_measured() {
        use crate::f0::{extract_f0, F0Config};
        use crate::loudness::measure;
        use crate::metrics::analyze;

        let sr = 48_000u32;
        // 必须长于 LRA 的 3 秒短时窗，否则 LRA 本来就该是 not_applicable
        let v: Vec<f32> = (0..sr as usize * 5)
            .map(|i| ((2.0 * std::f64::consts::PI * 300.0 * i as f64 / sr as f64).sin() * 0.3) as f32)
            .collect();
        let buf = AudioBuffer::new(vec![v], sr, 24).unwrap();
        let grade = analyze(&buf);
        let loud = measure(&buf).unwrap();
        let track = extract_f0(&buf, &F0Config::default()).unwrap();
        let dbtp = crate::truepeak::true_peak_dbtp(&buf);
        let r = evaluate(&EvalInput {
            before: &buf,
            after: &buf,
            grade: &grade,
            out_loudness: &loud,
            out_dbtp: dbtp,
            f0: &track,
            elapsed_ms: 100,
            cache_hit: false,
        });
        assert!(r.get("真峰值").unwrap().value.is_value());
        assert!(r.get("LRA").unwrap().value.is_value());
        assert!(r.get("OctaveErrRate").unwrap().value.is_value());
    }
}
