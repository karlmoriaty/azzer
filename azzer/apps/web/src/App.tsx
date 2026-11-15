import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  fetchMasterBlob,
  getCover,
  uploadCover,
  ApiError,
  type AEntryReport,
  type EvalItem,
  type SystemInfo,
} from "./api/client";
import { Waveform } from "./features/waveform/Waveform";
import { BrandMark, BareLineIcon, ContourIcon, SourceTargetIcon } from "./components/Brand";

type Conn =
  | { kind: "loading" }
  | { kind: "ok"; info: SystemInfo }
  | { kind: "error"; message: string };

type Phase =
  | { kind: "idle" }
  | { kind: "uploading"; name: string; size: number }
  | { kind: "running"; jobId: string; startedAt: number; name: string }
  | { kind: "done"; report: AEntryReport; name: string }
  | { kind: "failed"; message: string };

/** 把后端错误统一转成「原因 + 下一步」，不出现「未知错误」。 */
function describeError(e: unknown): string {
  if (e instanceof ApiError) return `${e.code}：${e.message}`;
  if (e instanceof Error) {
    if (e.message.includes("Failed to fetch")) {
      return "连不上本地服务。确认 Azzer 主进程还在运行，然后刷新页面。";
    }
    return e.message;
  }
  return "发生了未预期的错误。刷新页面重试，若反复出现请保留截图反馈。";
}

/** 解码音频并按像素列取峰值，供 Canvas 波形使用。 */
async function computePeaks(blob: Blob, cols = 1200): Promise<Float32Array> {
  const buf = await blob.arrayBuffer();
  const ctx = new AudioContext();
  try {
    const audio = await ctx.decodeAudioData(buf);
    const data = audio.getChannelData(0);
    const per = Math.max(1, Math.floor(data.length / cols));
    const peaks = new Float32Array(cols);
    for (let c = 0; c < cols; c++) {
      let max = 0;
      const start = c * per;
      const end = Math.min(data.length, start + per);
      for (let i = start; i < end; i++) {
        const v = Math.abs(data[i] ?? 0);
        if (v > max) max = v;
      }
      peaks[c] = max;
    }
    return peaks;
  } finally {
    void ctx.close();
  }
}

function fmtDuration(ms: number): string {
  // 命中缓存时可能只有几十毫秒，用 "0.0s" 显示会让人以为没跑
  if (!Number.isFinite(ms)) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function fmt(v: number | null | undefined, digits = 2, unit = ""): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  return `${v.toFixed(digits)}${unit}`;
}

export function App() {
  const [conn, setConn] = useState<Conn>({ kind: "loading" });
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [peaks, setPeaks] = useState<Float32Array | null>(null);
  const [audioUrl, setAudioUrl] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [elapsed, setElapsed] = useState(0);

  const audioRef = useRef<HTMLAudioElement>(null);
  const progressRef = useRef<number>(0);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  // 拉系统信息
  useEffect(() => {
    let alive = true;
    api
      .systemInfo()
      .then((info) => alive && setConn({ kind: "ok", info }))
      .catch((e: unknown) => alive && setConn({ kind: "error", message: describeError(e) }));
    return () => {
      alive = false;
    };
  }, []);

  // 支持 `?job=<id>` 直接恢复某个任务的结果。
  // 两个用途：刷新页面不丢结果；验证脚本能截到「处理完成」的界面而不是空态。
  useEffect(() => {
    const id = new URLSearchParams(window.location.search).get("job");
    if (id) {
      setPhase({ kind: "running", jobId: id, startedAt: Date.now(), name: "" });
    }
  }, []);

  // 轮询任务状态
  useEffect(() => {
    if (phase.kind !== "running") return;
    let alive = true;
    const jobId = phase.jobId;
    const name = phase.name;

    const tick = async () => {
      try {
        const job = await getCover(jobId);
        if (!alive) return;
        if (job.state === "done") {
          const blob = await fetchMasterBlob(jobId);
          if (!alive) return;
          const url = URL.createObjectURL(blob);
          const p = await computePeaks(blob);
          if (!alive) {
            URL.revokeObjectURL(url);
            return;
          }
          setAudioUrl(url);
          setPeaks(p);
          // 用 ?job= 恢复时没有文件名，从报告里的源路径取末段
          const displayName =
            name || job.report.source.split(/[\\/]/).pop() || "成品";
          setPhase({ kind: "done", report: job.report, name: displayName });
        } else if (job.state === "failed") {
          setPhase({ kind: "failed", message: job.message });
        }
      } catch (e) {
        if (alive) setPhase({ kind: "failed", message: describeError(e) });
      }
    };

    void tick();
    const timer = window.setInterval(tick, 1200);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
    // phase.name 在 running 期间不变，其余字段用作触发条件
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase.kind, phase.kind === "running" ? phase.jobId : ""]);

  // 处理计时：只更新秒数，不触发波形重绘
  useEffect(() => {
    if (phase.kind !== "running") return;
    const t0 = phase.startedAt;
    const timer = window.setInterval(() => setElapsed((Date.now() - t0) / 1000), 200);
    return () => window.clearInterval(timer);
  }, [phase]);

  // 播放进度驱动波形游标（只写 ref，不 setState）
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    let raf = 0;
    const tick = () => {
      if (audio.duration > 0) progressRef.current = audio.currentTime / audio.duration;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [audioUrl]);

  // 换成品时释放旧的 object URL，避免整段音频一直占内存
  useEffect(() => {
    return () => {
      if (audioUrl) URL.revokeObjectURL(audioUrl);
    };
  }, [audioUrl]);

  const startUpload = useCallback(async (file: File) => {
    if (!/\.(wav|wave)$/i.test(file.name)) {
      setPhase({
        kind: "failed",
        message: `只接受 WAV，当前是「${file.name}」。先用工具转成 WAV（48kHz 或 44.1kHz、单或双声道）再上传。`,
      });
      return;
    }
    setPhase({ kind: "uploading", name: file.name, size: file.size });
    setElapsed(0);
    try {
      const { job_id } = await uploadCover(file);
      setPhase({ kind: "running", jobId: job_id, startedAt: Date.now(), name: file.name });
    } catch (e) {
      setPhase({ kind: "failed", message: describeError(e) });
    }
  }, []);

  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragging(false);
      const f = e.dataTransfer.files?.[0];
      if (f) void startUpload(f);
    },
    [startUpload]
  );

  const reset = () => {
    if (audioUrl) URL.revokeObjectURL(audioUrl);
    setAudioUrl(null);
    setPeaks(null);
    setPhase({ kind: "idle" });
    progressRef.current = 0;
  };

  return (
    <div style={S.page}>
      <Header conn={conn} />

      <main style={S.main}>
        {(phase.kind === "idle" || phase.kind === "failed" || phase.kind === "uploading") && (
          <DropZone
            dragging={dragging}
            phase={phase}
            onDrop={onDrop}
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onPick={() => fileInputRef.current?.click()}
          />
        )}

        {phase.kind === "running" && <Running name={phase.name} elapsed={elapsed} />}

        {phase.kind === "done" && (
          <Result
            report={phase.report}
            name={phase.name}
            audioUrl={audioUrl}
            peaks={peaks}
            progressRef={progressRef}
            audioRef={audioRef}
            onReset={reset}
          />
        )}

        <input
          ref={fileInputRef}
          type="file"
          accept=".wav,audio/wav"
          style={{ display: "none" }}
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void startUpload(f);
            e.target.value = "";
          }}
        />
      </main>
    </div>
  );
}

function Header({ conn }: { conn: Conn }) {
  const caps = conn.kind === "ok" ? conn.info : null;
  return (
    <header style={S.header}>
      <div style={{ display: "flex", alignItems: "center", gap: "var(--s3)" }}>
        <span className="brand-mark" aria-hidden="true">
          <BrandMark size={26} />
        </span>
        <span style={S.brand}>Azzer</span>
        <span style={S.subtle}>同音色直通</span>
      </div>
      <div style={{ display: "flex", gap: "var(--s4)", alignItems: "center" }}>
        {conn.kind === "loading" && <span style={S.subtle}>连接中…</span>}
        {conn.kind === "error" && (
          <span style={{ ...S.subtle, color: "var(--accent-danger)" }}>{conn.message}</span>
        )}
        {caps && (
          <>
            <Chip
              label={caps.gpu_accelerated ? "GPU 加速" : "仅 CPU"}
              tone={caps.gpu_accelerated ? "cool" : "muted"}
            />
            <Chip
              label={caps.capabilities.direct ? "A 入口可用" : "A 入口不可用"}
              tone={caps.capabilities.direct ? "warm" : "muted"}
            />
            <Chip
              label={
                caps.models_declared === 0 ? "未声明模型" : `已声明 ${caps.models_declared} 个模型`
              }
              tone="muted"
            />
          </>
        )}
      </div>
    </header>
  );
}

function Chip({ label, tone }: { label: string; tone: "warm" | "cool" | "muted" }) {
  const color =
    tone === "warm"
      ? "var(--accent-warm)"
      : tone === "cool"
        ? "var(--accent-cool)"
        : "var(--text-tertiary)";
  return (
    <span
      style={{
        fontSize: 11,
        letterSpacing: "0.02em",
        color,
        border: `1px solid ${color}33`,
        borderRadius: "var(--r-pill)",
        padding: "2px 8px",
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </span>
  );
}

function DropZone(props: {
  dragging: boolean;
  phase: Phase;
  onDrop: (e: React.DragEvent) => void;
  onDragOver: (e: React.DragEvent) => void;
  onDragLeave: () => void;
  onPick: () => void;
}) {
  const { dragging, phase, onDrop, onDragOver, onDragLeave, onPick } = props;
  const failed = phase.kind === "failed" ? phase.message : null;
  const uploading = phase.kind === "uploading";

  return (
    <div style={{ display: "grid", gap: "var(--s4)", maxWidth: 720, width: "100%" }}>
      <div
        onDrop={onDrop}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onClick={uploading ? undefined : onPick}
        style={{
          ...S.drop,
          borderColor: dragging ? "var(--accent-warm)" : "var(--border-strong)",
          background: dragging ? "var(--bg-hover)" : "var(--bg-surface)",
          cursor: uploading ? "progress" : "pointer",
        }}
      >
        <div style={{ fontSize: 15, color: "var(--text-primary)" }}>
          {uploading ? "正在上传…" : "把 WAV 拖到这里，或点击选择"}
        </div>
        <div style={{ ...S.subtle, marginTop: "var(--s2)" }}>
          {uploading
            ? `${phase.name}（${(phase.size / 1_048_576).toFixed(1)}MB）`
            : "48kHz 或 44.1kHz｜单声道或双声道｜≤200MB｜≥3 秒"}
        </div>
        <button
          type="button"
          className="icon-btn primary"
          disabled={uploading}
          style={{ marginTop: "var(--s4)" }}
          onClick={(e) => {
            // 外层 drop 区本身也可点击，不拦住会连开两次文件选择器
            e.stopPropagation();
            onPick();
          }}
        >
          <span className="ico">
            <BareLineIcon size={18} />
          </span>
          选择 WAV 文件
        </button>
      </div>

      {failed && (
        <div style={S.errorBox}>
          <strong style={{ color: "var(--accent-danger)" }}>处理没有开始</strong>
          <div style={{ marginTop: "var(--s2)", lineHeight: 1.6 }}>{failed}</div>
        </div>
      )}

      <div style={{ ...S.subtle, lineHeight: 1.8 }}>
        A 入口做三件事：修复（谱减去噪 / 条件去混响 / 高频重建）、母带七步、37 项评估。
        它不改音色、不修音高——源就是你自己的声音。
      </div>
    </div>
  );
}

function Running({ name, elapsed }: { name: string; elapsed: number }) {
  return (
    <div style={{ display: "grid", gap: "var(--s3)", maxWidth: 720, width: "100%" }}>
      <div style={{ fontSize: 15, color: "var(--text-primary)" }}>正在处理 {name}</div>
      <div style={{ ...S.subtle, fontFamily: "var(--font-mono)" }}>
        已用 {elapsed.toFixed(1)}s
      </div>
      <div style={S.progressTrack}>
        <div style={{ ...S.progressBar, width: `${Math.min(96, (elapsed / 45) * 100)}%` }} />
      </div>
      <div style={{ ...S.subtle, lineHeight: 1.8 }}>
        4 分钟源约 40 秒。链上是 N01 归一 → N02 质检 → N09 修复 → N05 基频 → N15 母带 →
        N16 评估。
      </div>
    </div>
  );
}

function Result(props: {
  report: AEntryReport;
  name: string;
  audioUrl: string | null;
  peaks: Float32Array | null;
  progressRef: React.RefObject<number>;
  audioRef: React.RefObject<HTMLAudioElement>;
  onReset: () => void;
}) {
  const { report, name, audioUrl, peaks, progressRef, audioRef, onReset } = props;
  const m = report.master;

  return (
    <div style={{ display: "grid", gap: "var(--s6)", maxWidth: 960, width: "100%" }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline" }}>
        <div>
          <div style={{ fontSize: 15, color: "var(--text-primary)" }}>{name}</div>
          <div style={{ ...S.subtle, marginTop: 2 }}>
            端到端 {fmtDuration(report.elapsed_ms)}｜质检 {report.grade.grade} 级｜
            {report.repair.de_reverb_applied ? "已去混响" : "跳过去混响"}
          </div>
        </div>
        <button type="button" className="icon-btn" onClick={onReset}>
          <span className="ico">
            <SourceTargetIcon size={18} />
          </span>
          换一条
        </button>
      </div>

      {audioUrl && (
        <div style={{ display: "grid", gap: "var(--s3)" }}>
          {peaks && (
            <Waveform
              label="成品"
              peaks={peaks}
              accent="cool"
              progressRef={progressRef}
              onSeek={(r) => {
                const a = audioRef.current;
                if (a && a.duration > 0) a.currentTime = r * a.duration;
              }}
              height={88}
            />
          )}
          <audio ref={audioRef} src={audioUrl} controls style={{ width: "100%" }} />
        </div>
      )}

      <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: "var(--s3)" }}>
        <Stat label="积分响度" value={fmt(m.output.integrated_lufs, 2, " LUFS")} ok={m.meets_lufs} />
        <Stat label="真峰值" value={fmt(m.output_dbtp, 2, " dBTP")} ok={m.meets_true_peak} />
        <Stat
          label="LRA"
          value={fmt(m.output.lra_lu, 1, " LU")}
          ok={(m.output.lra_lu ?? 0) >= 8}
        />
        <Stat label="增益" value={fmt(m.gain_db, 2, " dB")} ok={true} />
      </div>

      <ChainSummary report={report} />
      <EvalTable evalReport={report.eval} />
    </div>
  );
}

function Stat({ label, value, ok }: { label: string; value: string; ok: boolean }) {
  return (
    <div style={S.card}>
      <div style={{ ...S.subtle, fontSize: 11 }}>{label}</div>
      <div
        style={{
          fontFamily: "var(--font-mono)",
          fontSize: 18,
          marginTop: "var(--s2)",
          color: ok ? "var(--text-primary)" : "var(--accent-danger)",
        }}
      >
        {value}
      </div>
    </div>
  );
}

function ChainSummary({ report }: { report: AEntryReport }) {
  const c = report.chain;
  const r = report.repair;
  const f = report.f0;
  const step = (on: boolean, label: string) => (
    <span style={{ color: on ? "var(--text-secondary)" : "var(--text-tertiary)" }}>
      {on ? "✓" : "·"} {label}
    </span>
  );
  return (
    <div style={S.card}>
      <div style={{ ...S.subtle, fontSize: 11, marginBottom: "var(--s3)" }}>链路</div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: "var(--s4)", fontSize: 12 }}>
        {step(c.mono_lows_applied, "低频单声道化")}
        {step(c.dynamic_eq_applied, "动态 EQ")}
        {step(c.multiband_applied, "多段压缩")}
        {step(c.saturation_applied, "谐波饱和")}
        {step(c.width_applied, "立体声增强")}
        {step(true, "真峰值限幅")}
        {step(true, "响度归一")}
      </div>
      <div style={{ ...S.subtle, marginTop: "var(--s3)", lineHeight: 1.8 }}>
        修复：谱减 {r.denoise_applied ? "✓" : "·"}｜去混响{" "}
        {r.de_reverb_applied
          ? `✓（RT60 ${fmt(r.rt60_s, 2, "s")}）`
          : "跳过（RT60 未超 0.35s）"}
        ｜高频重建 {r.hf_rebuild_applied ? "✓" : "·"}（8–16kHz 比 {fmt(r.hf_ratio_before, 3)} →{" "}
        {fmt(r.hf_ratio_after, 3)}
        {r.meets_hf_ratio ? "，达标" : "，未达 0.6"}）
      </div>
      <div style={{ ...S.subtle, marginTop: "var(--s2)", lineHeight: 1.8 }}>
        基频：{f.total_frames} 帧，浊音 {(f.voiced_ratio * 100).toFixed(0)}%，中位{" "}
        {fmt(f.median_f0_hz, 1, "Hz")}，双路不一致率{" "}
        {(f.route_disagreement_ratio * 100).toFixed(2)}%
        {f.meets_route_contract ? "（达标）" : "（超 1% 契约）"}
      </div>
      <div style={{ ...S.subtle, marginTop: "var(--s2)", lineHeight: 1.8 }}>{report.path_note}</div>
    </div>
  );
}

function EvalTable({ evalReport }: { evalReport: AEntryReport["eval"] }) {
  const groups = useMemo(() => {
    const map = new Map<string, EvalItem[]>();
    for (const it of evalReport.items) {
      const arr = map.get(it.group) ?? [];
      arr.push(it);
      map.set(it.group, arr);
    }
    return [...map.entries()];
  }, [evalReport.items]);

  const [open, setOpen] = useState<Record<string, boolean>>({ 母带: true, 拟真度: true });
  const allOpen = groups.every(([group]) => open[group] ?? false);
  const toggleAll = () => {
    const next: Record<string, boolean> = {};
    for (const [group] of groups) next[group] = !allOpen;
    setOpen(next);
  };

  return (
    <div style={{ display: "grid", gap: "var(--s3)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: "var(--s3)", flexWrap: "wrap" }}>
        <span style={{ fontSize: 13, color: "var(--text-primary)" }}>评估</span>
        <span style={S.subtle}>
          {evalReport.items.length} 项｜{evalReport.measured} 项实测｜
          {evalReport.not_applicable} 项不适用
          {evalReport.failing.length > 0 && `｜${evalReport.failing.length} 项未达标`}
        </span>
        <button
          type="button"
          className="icon-btn sm"
          style={{ marginLeft: "auto" }}
          onClick={toggleAll}
        >
          <span className="ico">
            <ContourIcon size={20} />
          </span>
          {allOpen ? "全部收起" : "全部展开"}
        </button>
      </div>

      {groups.map(([group, items]) => {
        const isOpen = open[group] ?? false;
        const measured = items.filter((i) => "Value" in i.value).length;
        const failing = items.filter((i) => i.meets === false).length;
        return (
          <div key={group} style={S.card}>
            <button
              onClick={() => setOpen((o) => ({ ...o, [group]: !isOpen }))}
              style={S.groupHead}
            >
              <span style={{ color: "var(--text-secondary)" }}>
                {isOpen ? "▾" : "▸"} {group}
              </span>
              <span style={S.subtle}>
                {measured}/{items.length} 实测
                {failing > 0 && (
                  <span style={{ color: "var(--accent-danger)" }}>｜{failing} 未达标</span>
                )}
              </span>
            </button>
            {isOpen && (
              <div style={{ display: "grid", gap: "var(--s2)", marginTop: "var(--s3)" }}>
                {items.map((it) => (
                  <EvalRow key={it.name} item={it} />
                ))}
              </div>
            )}
          </div>
        );
      })}

      {evalReport.humanize_parts.length > 0 && (
        <div style={{ ...S.subtle, fontSize: 11, lineHeight: 1.8 }}>
          HumanizeScore 已算子项：
          {evalReport.humanize_parts.map(([k, v]) => `${k} ${v.toFixed(2)}`).join("｜")}
        </div>
      )}
    </div>
  );
}

function EvalRow({ item }: { item: EvalItem }) {
  const hasValue = "Value" in item.value;
  const value = hasValue ? (item.value as { Value: number }).Value : null;
  const reason = hasValue ? null : (item.value as { NotApplicable: string }).NotApplicable;
  const bad = item.meets === false;

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "160px 1fr 120px",
        gap: "var(--s3)",
        alignItems: "baseline",
        fontSize: 12,
        paddingLeft: "var(--s3)",
      }}
    >
      <span style={{ color: hasValue ? "var(--text-secondary)" : "var(--text-tertiary)" }}>
        {item.name}
      </span>
      <span style={{ color: "var(--text-tertiary)", lineHeight: 1.6 }}>
        {hasValue ? item.target : reason}
      </span>
      <span
        style={{
          fontFamily: "var(--font-mono)",
          textAlign: "right",
          color: bad
            ? "var(--accent-danger)"
            : hasValue
              ? "var(--text-primary)"
              : "var(--text-tertiary)",
        }}
      >
        {hasValue ? value!.toFixed(3) : "不适用"}
        {bad ? " ✗" : ""}
      </span>
    </div>
  );
}

const S: Record<string, React.CSSProperties> = {
  page: {
    minHeight: "100%",
    background: "var(--bg-base)",
    color: "var(--text-primary)",
    fontFamily: "var(--font-sans)",
    fontSize: 13,
  },
  header: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    padding: "var(--s4) var(--s6)",
    borderBottom: "1px solid var(--border-subtle)",
  },
  brand: { fontSize: 16, letterSpacing: "0.01em", color: "var(--text-primary)" },
  subtle: { fontSize: 12, color: "var(--text-tertiary)" },
  main: {
    padding: "var(--s8) var(--s6)",
    display: "flex",
    justifyContent: "center",
  },
  drop: {
    border: "1px dashed var(--border-strong)",
    borderRadius: "var(--r-lg)",
    padding: "var(--s10) var(--s6)",
    textAlign: "center",
    transition: "background 120ms, border-color 120ms",
  },
  errorBox: {
    background: "var(--bg-surface)",
    border: "1px solid rgba(224,108,90,0.25)",
    borderRadius: "var(--r-md)",
    padding: "var(--s4)",
    fontSize: 12,
    color: "var(--text-secondary)",
  },
  progressTrack: {
    height: 3,
    background: "var(--bg-elevated)",
    borderRadius: "var(--r-pill)",
    overflow: "hidden",
  },
  progressBar: {
    height: "100%",
    background: "var(--accent-warm)",
    transition: "width 200ms linear",
  },
  card: {
    background: "var(--bg-surface)",
    border: "1px solid var(--border-subtle)",
    borderRadius: "var(--r-md)",
    padding: "var(--s4)",
  },
  groupHead: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "baseline",
    width: "100%",
    background: "transparent",
    border: "none",
    padding: 0,
    cursor: "pointer",
    fontFamily: "inherit",
    fontSize: 13,
    textAlign: "left",
  },
};
