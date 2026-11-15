#!/usr/bin/env node
/**
 * 对任意一条人声源跑一次完整 A 入口（N01→N02→N09→N05→N15→N16），
 * 然后用人类能读的方式回答一个问题：**这条源现在能算出多少东西？**
 *
 * 为什么需要它：R5（f0）和 R7（感知层）两轮都撞在同一堵墙上——合成夹具里
 * 没有连续音符、没有颤音、没有呼吸，所以 HumanizeScore 六子项永远凑不齐，
 * 拟真度评估一直是「代码写好了但没被现实检验」。把真实人声丢进来跑一次，
 * 就能立刻看到那些 not_applicable 里有多少真的变成了数字。
 *
 * 用法：
 *   node scripts/check-source.cjs <源WAV> [运行根目录]
 *
 * 退出码：
 *   0  跑通（不看指标是否达标，那是 N16 的事）
 *   1  失败（源被 N01/N02 拒绝，或编译产物不存在）
 *
 * 注意：源必须是 48kHz / 24bit PCM WAV。其他格式先在外部转好——
 * 解码器只认这一种，这是刻意的（避免引入 ffmpeg 依赖）。
 */
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const AZ = path.resolve(__dirname, "..");
const WORK = path.resolve(AZ, "..");
const EXE = path.join(WORK, ".build", "cargo-target", "debug", "run_a_entry.exe");

const src = process.argv[2];
if (!src) {
  console.error("用法：node scripts/check-source.cjs <源WAV> [运行根目录]");
  console.error("提示：源需为 48kHz / 24bit PCM WAV。");
  process.exit(2);
}
if (!fs.existsSync(src)) {
  console.error(`找不到源文件：${src}`);
  process.exit(2);
}
if (!fs.existsSync(EXE)) {
  console.error(`找不到可执行文件：${EXE}`);
  console.error("先编译：node scripts/cargo-run.cjs build -p azzer-pipeline --bins");
  process.exit(1);
}

const root = process.argv[3] || path.join(WORK, ".cache", "source-check");
const reportPath = path.join(root, "report.json");

fs.mkdirSync(root, { recursive: true });

const t0 = Date.now();
const r = spawnSync(EXE, [src, root, reportPath], {
  cwd: AZ,
  encoding: "buffer",
  timeout: 40 * 60 * 1000,
  maxBuffer: 128 * 1024 * 1024,
});
const elapsed = ((Date.now() - t0) / 1000).toFixed(1);

if (r.status !== 0) {
  const err = (r.stderr || Buffer.alloc(0)).toString("utf8").trim();
  console.error(`\nA 入口执行失败（exit=${r.status}）：\n${err}`);
  if (err.includes("时长") || err.includes("电平")) {
    console.error("\n这是 N01 的输入断言在拦，不是 bug：源不满足可用条件。");
  }
  process.exit(1);
}

const rep = JSON.parse(fs.readFileSync(reportPath, "utf8"));
const ev = rep.eval || {};
const items = ev.items || [];

const valued = [];
const na = [];
const failing = [];
for (const it of items) {
  const v = it.value || {};
  if (Object.prototype.hasOwnProperty.call(v, "Value")) {
    valued.push({ ...it, num: v.Value });
    if (it.meets === false) failing.push(it);
  } else {
    na.push({ ...it, reason: v.NotApplicable || "（未写原因）" });
  }
}

const N = (x, d = 2) => (typeof x === "number" ? x.toFixed(d) : String(x));
const out = [];
out.push("");
out.push("═".repeat(78));
out.push(`源：${src}`);
out.push(`耗时：${elapsed} 秒（含 N01→N02→N09→N05→N15→N16 全链）`);
out.push("═".repeat(78));

out.push("");
out.push("── 输入分级（N02） " + "─".repeat(56));
const g = rep.grade || {};
out.push(`  等级        ${g.grade}    ${g.reason || ""}`);
out.push(`  时长        ${N(g.duration_s, 1)} s`);
out.push(`  峰值        ${N(g.peak_dbfs)} dBFS`);
out.push(`  削顶率      ${N(g.clip_ratio, 6)}`);
out.push(`  SNR         ${N(g.snr_db)} dB`);
out.push(`  有效带宽    ${N(g.effective_bandwidth_hz, 0)} Hz`);
out.push(`  码率        ${N(g.bitrate_bps, 0)} bps${g.bitrate_is_pcm ? "（PCM）" : ""}`);

out.push("");
out.push("── 修复（N09） " + "─".repeat(60));
const rp = rep.repair || {};
out.push(`  降噪        ${rp.denoise_applied ? "执行" : "跳过"}`);
out.push(`  去混响      ${rp.de_reverb_applied ? "执行" : "跳过（RT60 未达门限或无法估计）"}`);
out.push(`  高频重建    ${rp.hf_rebuild_applied ? "执行" : "跳过"}   增益 ${N(rp.hf_gain_db)} dB`);
out.push(`  高频占比    ${N(rp.hf_ratio_before, 3)} → ${N(rp.hf_ratio_after, 3)}`);

out.push("");
out.push("── 基频（N05） " + "─".repeat(60));
const f0 = rep.f0 || {};
out.push(`  浊音帧占比  ${N((f0.voiced_ratio || 0) * 100)} %（${f0.voiced_frames}/${f0.total_frames} 帧）`);
out.push(`  中位 f0     ${N(f0.median_f0_hz, 1)} Hz`);
out.push(`  f0 范围     ${N(f0.min_f0_hz, 1)} – ${N(f0.max_f0_hz, 1)} Hz`);
out.push(`  双路分歧    ${N((f0.route_disagreement_ratio || 0) * 100, 3)} %  契约 ${f0.meets_route_contract ? "满足" : "未满足"}`);

out.push("");
out.push("── 母带（N15） " + "─".repeat(60));
const m = rep.master || {};
const mi = m.input || {};
const mo = m.output || {};
out.push(`  源响度      ${N(rep.source_lufs)} LUFS / ${N(rep.source_dbtp)} dBTP`);
out.push(`  母带前      ${N(mi.integrated_lufs)} LUFS / ${N(m.input_dbtp)} dBTP`);
out.push(`  母带后      ${N(mo.integrated_lufs)} LUFS / ${N(m.output_dbtp)} dBTP`);
out.push(`  施加增益    ${N(m.gain_db)} dB    限幅削减 ${N(m.limiter_reduction_db)} dB    轮次 ${m.passes}`);
out.push(`  门禁        响度 ${m.meets_lufs ? "达标" : "未达标"} / 真峰值 ${m.meets_true_peak ? "达标" : "未达标"}`);

out.push("");
out.push("── 37 项评估（N16） " + "─".repeat(54));
out.push(`  有实测值    ${valued.length} 项`);
out.push(`  不适用      ${na.length} 项`);
out.push(`  不达标      ${failing.length} 项${failing.length ? "：" + failing.map((f) => f.name).join("、") : ""}`);

const hs = items.find((i) => i.name === "HumanizeScore");
if (hs) {
  const v = hs.value || {};
  if (Object.prototype.hasOwnProperty.call(v, "Value")) {
    out.push(`  HumanizeScore  ${N(v.Value, 3)}（目标 ≥0.75，${hs.meets ? "达标" : "未达标"}）`);
  } else {
    out.push(`  HumanizeScore  算不出：${v.NotApplicable}`);
  }
}

if (rep.eval && Array.isArray(rep.eval.humanize_parts)) {
  out.push("");
  out.push("── HumanizeScore 六子项（已算出的部分） " + "─".repeat(35));
  const got = new Set();
  for (const [name, val] of rep.eval.humanize_parts) {
    got.add(String(name).slice(0, 2));
    out.push(`  ${String(name).padEnd(24)}${N(val, 3)}`);
  }
  // 六个位置里缺了哪个，直接点名——这决定了 HumanizeScore 能不能算
  const missing = ["H1", "H2", "H3", "H4", "H5", "H6"].filter((k) => !got.has(k));
  if (missing.length) {
    out.push(`  ↑ 缺：${missing.join("、")}（六项齐了才有 HumanizeScore）`);
  } else {
    out.push("  ↑ 六项已齐");
  }
}

out.push("");
out.push("── 有值的项 " + "─".repeat(62));
for (const it of valued) {
  const mark = it.meets === true ? "达标" : it.meets === false ? "未达标" : "  —  ";
  out.push(`  [${mark}] ${(it.group + " · " + it.name).padEnd(30)}${N(it.num, 3).padStart(10)}   目标 ${it.target}`);
}

out.push("");
out.push("── 不适用的项（判断「哪些能算」时看这里） " + "─".repeat(32));
for (const it of na) {
  out.push(`  ${(it.group + " · " + it.name).padEnd(30)}${it.reason}`);
}

out.push("");
out.push(`完整报告：${reportPath}`);
out.push(`成品：${path.join(root, "out", "master.wav")}`);
out.push("");

console.log(out.join("\n"));

// 本机 PowerShell 的 stdout 会用 GBK 解码 Node 写出的 UTF-8 字节，中文必乱码。
// 需要落盘看时设 AZZER_CHECK_OUT=<文件路径>，由 Node 自己写，全程不经过 shell。
if (process.env.AZZER_CHECK_OUT) {
  fs.writeFileSync(process.env.AZZER_CHECK_OUT, out.join("\n"), "utf8");
}
