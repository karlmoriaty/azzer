import { useCallback, useEffect, useRef } from "react";

/**
 * 波形视图（单轨）。
 *
 * 三条设计约束（v3.2 §5「性能三条硬规矩」）：
 * 1. 波形用 Canvas 画，不生成 DOM 节点——10 分钟音频约 3×10⁷ 采样点，SVG 会崩。
 * 2. 播放游标是独立的绝对定位元素，用 `transform` 推进，不重绘 Canvas。
 * 3. 游标由外部 ref 驱动，不经过 React state，避免每帧重渲染整棵树。
 *
 * 现在喂的是合成峰值（M0 占位）。接真音频时只换 `peaks` 的来源，
 * 组件本身不改——解码后按像素列取 min/max 得到 Float32Array 即可。
 */

export interface WaveformProps {
  label: string;
  peaks: Float32Array;
  accent: "warm" | "cool";
  /** 0..1，外部每帧更新，本组件只读 */
  progressRef: React.RefObject<number>;
  /** 点击定位，参数为 0..1 */
  onSeek?: (ratio: number) => void;
  height?: number;
}

export function Waveform({
  label,
  peaks,
  accent,
  progressRef,
  onSeek,
  height = 72,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const cursorRef = useRef<HTMLDivElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  // 绘制逻辑抽成回调，供首次渲染与 ResizeObserver 复用。
  // 用 ResizeObserver 而不是只在 mount 时画一次：首次渲染时容器宽度
  // 可能还不是最终值（布局未稳定 / 滚动条未出现），会画出偏窄的波形。
  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;

    const cssW = wrap.clientWidth;
    const cssH = height;
    if (cssW === 0) return;

    const dpr = window.devicePixelRatio || 1;
    const targetW = Math.round(cssW * dpr);
    const targetH = Math.round(cssH * dpr);
    if (canvas.width !== targetW || canvas.height !== targetH) {
      canvas.width = targetW;
      canvas.height = targetH;
    }
    canvas.style.width = `${cssW}px`;
    canvas.style.height = `${cssH}px`;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    const mid = cssH / 2;
    const colW = 2;
    const gap = 1;
    const step = colW + gap;
    const cols = Math.max(1, Math.floor(cssW / step));

    // 颜色：暖石给原声，青绿给结果
    ctx.fillStyle =
      accent === "warm" ? "rgba(232, 178, 125, 0.62)" : "rgba(94, 200, 176, 0.62)";

    for (let i = 0; i < cols; i++) {
      // peaks 长度与列数解耦：按比例采样，兼容不同分辨率
      const idx = Math.floor((i / cols) * peaks.length);
      const v = peaks[idx] ?? 0;
      // 留 10px 上下边距，避免波形顶到卡片边缘（视觉上会"糊成一块"）
      const h = Math.max(1, Math.abs(v) * (mid - 10));
      ctx.fillRect(i * step, mid - h, colW, h * 2);
    }

    // 中线
    ctx.fillStyle = "rgba(255,255,255,0.10)";
    ctx.fillRect(0, mid - 0.5, cssW, 1);

    // 首帧定位游标
    const p = progressRef.current ?? 0;
    if (cursorRef.current) {
      cursorRef.current.style.transform = `translateX(${p * cssW}px)`;
    }
  }, [peaks, accent, height, progressRef]);

  useEffect(() => {
    draw();
    const wrap = wrapRef.current;
    if (!wrap) return;
    const ro = new ResizeObserver(() => draw());
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [draw]);

  // 游标：rAF 读 ref，直接改 transform，不触发重渲染
  useEffect(() => {
    let raf = 0;
    let lastX = -1;
    const tick = () => {
      const wrap = wrapRef.current;
      const cursor = cursorRef.current;
      if (wrap && cursor) {
        const p = Math.min(1, Math.max(0, progressRef.current ?? 0));
        const x = p * wrap.clientWidth;
        if (Math.abs(x - lastX) > 0.4) {
          cursor.style.transform = `translateX(${x}px)`;
          lastX = x;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [progressRef]);

  return (
    <div>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          marginBottom: 6,
          fontSize: 11,
          color: "var(--text-tertiary)",
        }}
      >
        <span>{label}</span>
      </div>
      <div
        ref={wrapRef}
        style={{ position: "relative", width: "100%", height }}
        onClick={(e) => {
          if (!onSeek) return;
          const rect = e.currentTarget.getBoundingClientRect();
          onSeek((e.clientX - rect.left) / rect.width);
        }}
      >
        <canvas ref={canvasRef} style={{ display: "block" }} />
        <div
          ref={cursorRef}
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            width: 1,
            height,
            background: "var(--text-primary)",
            opacity: 0.9,
            pointerEvents: "none",
            willChange: "transform",
          }}
        />
      </div>
    </div>
  );
}

/**
 * 生成合成峰值，M0 占位用。
 *
 * 刻意做成"像一首歌"的形状：前奏弱、主歌中等、副歌强、尾奏衰减，
 * 这样布局与视觉密度在真音频接入前就能被真实评估。
 */
export function synthPeaks(seed: number, cols = 1200): Float32Array {
  const out = new Float32Array(cols);
  let s = seed >>> 0;
  const rand = () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0xffffffff;
  };
  for (let i = 0; i < cols; i++) {
    const t = i / cols;
    // 段落包络：0.15 → 0.55 → 0.9 → 0.5 → 0.12
    const env =
      t < 0.12
        ? 0.15 + t * 2
        : t < 0.38
          ? 0.55
          : t < 0.62
            ? 0.9
            : t < 0.86
              ? 0.5
              : 0.4 - (t - 0.86) * 2;
    const jitter = 0.75 + rand() * 0.5;
    const transient = i % 24 < 2 ? 1.25 : 1;
    out[i] = Math.min(1, Math.max(0.02, env * jitter * transient));
  }
  return out;
}
