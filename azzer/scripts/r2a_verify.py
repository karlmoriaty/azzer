"""R2a 交叉验证：Python 独立实现 EBU R128 与真峰值，与 Rust 输出比对。

为什么需要它：Rust 侧的单测只能证明"实现没崩"，不能证明"数值对"。
这里用 scipy/NumPy 按 BS.1770 的定义**重新实现一遍**，两边独立算，
差得太多说明至少有一边错了。

用法：
    python r2a_verify.py <原始 WAV> <母带 WAV> <Rust 报告 JSON>

比对项：输入/输出的积分响度、真峰值、LRA。
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

import numpy as np
from scipy.signal import lfilter

# BS.1770-4 的 48kHz K 加权系数（与 Rust 侧同一组标准数值）
B1 = [1.53512485958697, -2.69169618940638, 1.19839281085285]
A1 = [1.0, -1.69065929318241, 0.73248077421585]
B2 = [1.0, -2.0, 1.0]
A2 = [1.0, -1.99004745483398, 0.99007225036621]

BLOCK_S, HOP_S = 0.4, 0.1
SHORT_S, SHORT_HOP_S = 3.0, 1.0
ABS_GATE_LUFS, REL_GATE_LU = -70.0, -10.0
LUFS_OFFSET = -0.691
RATIO = 4


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    """读 PCM 16/24/32-bit WAV，返回 (samples[N,C] float64, sample_rate)。"""
    b = path.read_bytes()
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE", "不是 RIFF/WAVE"
    pos, fmt, data = 12, None, None
    while pos + 8 <= len(b):
        cid = b[pos:pos + 4]
        size = struct.unpack("<I", b[pos + 4:pos + 8])[0]
        body = b[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            audio_fmt, ch, sr, _, _, bits = struct.unpack("<HHIIHH", body[:16])
            # 24bit 常被写成 WAVE_FORMAT_EXTENSIBLE(0xFFFE)，真实格式在
            # SubFormat GUID 的前两个字节（偏移 24），不认这个会误判成非 PCM
            if audio_fmt == 0xFFFE and len(body) >= 26:
                audio_fmt = struct.unpack("<H", body[24:26])[0]
            fmt = (audio_fmt, ch, sr, bits)
        elif cid == b"data":
            data = body
            break
        pos += 8 + size + (size & 1)
    assert fmt and data is not None, "缺少 fmt 或 data chunk"
    audio_fmt, ch, sr, bits = fmt
    assert audio_fmt == 1, f"只支持 PCM，实得 format={audio_fmt}"

    if bits == 16:
        x = np.frombuffer(data, dtype="<i2").astype(np.float64) / 32768.0
    elif bits == 24:
        a = np.frombuffer(data, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
        v = a[:, 0] | (a[:, 1] << 8) | (a[:, 2] << 16)
        v = np.where(v & 0x800000, v - 0x1000000, v)
        x = v.astype(np.float64) / 8388608.0
    elif bits == 32:
        x = np.frombuffer(data, dtype="<i4").astype(np.float64) / 2147483648.0
    else:
        raise ValueError(f"不支持的位深 {bits}")
    return x.reshape(-1, ch), sr


def k_weight(x: np.ndarray) -> np.ndarray:
    y = lfilter(B1, A1, x, axis=0)
    return lfilter(B2, A2, y, axis=0)


def to_lufs(mean_sq: float) -> float:
    if mean_sq <= 0:
        return float("-inf")
    return LUFS_OFFSET + 10.0 * np.log10(mean_sq)


def window_means(x: np.ndarray, win: int, hop: int) -> np.ndarray:
    """各声道能量求和后按窗取均值。x 为 [N, C]。"""
    n = x.shape[0]
    if n < win:
        return np.zeros(0)
    total = np.sum(x.astype(np.float64) ** 2, axis=1)
    c = np.concatenate([[0.0], np.cumsum(total)])
    starts = np.arange(0, n - win + 1, hop)
    return (c[starts + win] - c[starts]) / win


def integrated_lufs(x: np.ndarray, sr: int) -> float | None:
    y = k_weight(x)
    blocks = window_means(y, int(round(BLOCK_S * sr)), int(round(HOP_S * sr)))
    abs_gate = 10 ** ((ABS_GATE_LUFS - LUFS_OFFSET) / 10.0)
    after_abs = blocks[blocks > abs_gate]
    if after_abs.size == 0:
        return None
    rel_gate = after_abs.mean() * 10 ** (REL_GATE_LU / 10.0)
    gated = after_abs[after_abs > rel_gate]
    if gated.size == 0:
        return None
    return to_lufs(gated.mean())


def lra(x: np.ndarray, sr: int) -> float | None:
    y = k_weight(x)
    st = window_means(y, int(round(SHORT_S * sr)), int(round(SHORT_HOP_S * sr)))
    st = np.array([to_lufs(v) for v in st])
    st = st[np.isfinite(st) & (st > ABS_GATE_LUFS)]
    if st.size < 2:
        return None
    st.sort()
    p10 = float(np.percentile(st, 10))
    p95 = float(np.percentile(st, 95))
    return max(0.0, p95 - p10)


def hermite(p0, p1, p2, p3, t):
    a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3
    b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3
    c = -0.5 * p0 + 0.5 * p2
    d = p1
    return ((a * t + b) * t + c) * t + d


def true_peak(x: np.ndarray) -> float:
    """与 Rust 同算法：4× Catmull-Rom 插值后取最大绝对值。"""
    peak = 0.0
    for ch in range(x.shape[1]):
        s = x[:, ch]
        if s.size < 2:
            peak = max(peak, float(np.max(np.abs(s))) if s.size else 0.0)
            continue
        peak = max(peak, float(np.max(np.abs(s))))
        p0 = np.concatenate([[s[0]], s[:-1]])
        p2 = np.concatenate([s[1:], [s[-1]]])
        p3 = np.concatenate([s[2:], [s[-1]], [s[-1]]])
        p1 = s
        for k in range(1, RATIO):
            t = k / RATIO
            v = hermite(p0, p1, p2, p3, t)
            peak = max(peak, float(np.max(np.abs(v))))
    return 20.0 * np.log10(max(peak, np.finfo(float).tiny))


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__)
        return 2
    src_path, master_path, report_path = (Path(p) for p in sys.argv[1:4])
    src, sr = read_wav(src_path)
    master, msr = read_wav(master_path)
    report = json.loads(report_path.read_text(encoding="utf-8"))
    m = report["master"]

    print(f"原始：{src.shape} @ {sr}Hz")
    print(f"母带：{master.shape} @ {msr}Hz\n")

    checks = [
        # 注意：master.input 是「已过前五步、进入第 6 步」的信号，
        # 不是原始源。原始源的独立值在 report.source_lufs / source_dbtp。
        ("原始源积分响度 (LUFS)", integrated_lufs(src, sr), report.get("source_lufs"), 0.1),
        ("原始源真峰值 (dBTP)", true_peak(src), report.get("source_dbtp"), 0.3),
        ("输出积分响度 (LUFS)", integrated_lufs(master, msr), m["output"]["integrated_lufs"], 0.1),
        ("输出真峰值 (dBTP)", true_peak(master), m["output_dbtp"], 0.3),
        ("输出 LRA (LU)", lra(master, msr), m["output"]["lra_lu"], 0.5),
    ]

    print(f"{'指标':<26}{'Python':>14}{'Rust':>14}{'容差':>8}  判定")
    ok = True
    for name, py_val, rs_val, tol in checks:
        if py_val is None or rs_val is None:
            both_none = py_val is None and rs_val is None
            ok &= both_none
            print(f"{name:<26}{'null':>14}{'null':>14}{'-':>8}  {'PASS' if both_none else 'FAIL'}")
            continue
        diff = abs(py_val - rs_val)
        passed = diff <= tol
        ok &= passed
        print(f"{name:<26}{py_val:>14.4f}{rs_val:>14.4f}{tol:>8.2f}  "
              f"{'PASS' if passed else 'FAIL'} (diff={diff:.4f})")

    print("\n门禁：")
    gate = [
        ("真峰值 ≤ -1.0 dBTP", m["output_dbtp"] <= -1.0 + 0.05),
        ("响度偏差 ≤ 0.5 LU", abs(m["output"]["integrated_lufs"] - (-14.0)) <= 0.5),
        ("报告 meets_true_peak", m["meets_true_peak"] is True),
        ("报告 meets_lufs", m["meets_lufs"] is True),
    ]
    for label, good in gate:
        print(f"  {'PASS' if good else 'FAIL'}  {label}")
        ok &= good

    # 单声道兼容性（N14 契约：折叠后衰减 ≤3dB）。仅双声道有意义。
    if master.shape[1] >= 2:
        folded = (master[:, 0] + master[:, 1]) * 0.5
        e_folded = float(np.mean(folded.astype(np.float64) ** 2))
        e_avg = float(np.mean(
            (master[:, 0].astype(np.float64) ** 2 + master[:, 1].astype(np.float64) ** 2) * 0.5
        ))
        atten = 10.0 * np.log10(max(e_folded, 1e-20) / max(e_avg, 1e-20))
        good = atten >= -3.0
        ok &= good
        print(f"\n单声道兼容性：折叠后 {atten:+.2f}dB "
              f"（红线 ≥-3dB）  {'PASS' if good else 'FAIL'}")

    print("\nRESULT:", "PASS - Rust 与 Python 独立实现一致" if ok else "FAIL - 存在偏差")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
