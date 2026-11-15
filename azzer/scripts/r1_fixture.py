"""R1 回归夹具的生成与交叉验证。

两个子命令：
    python r1_fixture.py gen
        在工作目录内生成 4 分钟 / 48k / 24bit WAV 夹具与 expected.json。
        Python 独立实现五项指标，不调用 Rust，作为对照基准。

    python r1_fixture.py verify <r1_report.json>
        把 Rust 的 run_r1 输出与 expected.json 逐项比对，打印 PASS/FAIL。

为什么用合成信号：设计师明确要求本机语音模型残缺、不完美，不得绑定，
也不得拿它们的输出当质量基准。合成信号可复现、可精确计算，是唯一可信的
回归金标准。

注意：Python 侧刻意按与 Rust 相同的**算法定义**重新实现一遍（帧长、分位数、
Goertzel 窗、T20 外推），目的是验证实现一致性，不是评价算法优劣。
"""

from __future__ import annotations

import json
import math
import os
import struct
import sys
from pathlib import Path

import numpy as np

WORKSPACE = Path(__file__).resolve().parents[2]
OUT = WORKSPACE / ".cache" / "r1"
SR = 48_000
SECONDS = 240
FIXTURE = OUT / "fixture.wav"
EXPECTED = OUT / "expected.json"

FRAME_MS, HOP_MS = 20.0, 10.0
CLIP_THRESHOLD = 0.999
BW_CUTOFF_POWER = 1e-4
BW_PROBES = [2000.0, 4000.0, 8000.0, 12000.0, 16000.0, 20000.0]


# ---------------------------------------------------------------- WAV 24bit

def write_wav24(path: Path, samples: np.ndarray, rate: int = SR) -> None:
    """手写 RIFF/WAVE 24bit PCM，不依赖任何音频库（与 Rust 的 hound 完全独立）。"""
    s = np.clip(samples.astype(np.float64), -1.0, 1.0 - 1.0 / 8_388_608.0)
    i32 = np.round(s * 8_388_608.0).astype("<i4")
    # 取每个 4 字节小端整数的低 3 字节
    raw = np.frombuffer(i32.tobytes(), dtype=np.uint8).reshape(-1, 4)[:, :3]
    data = raw.tobytes()
    header = (
        b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVE"
        + b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 3, 3, 24)
        + b"data" + struct.pack("<I", len(data))
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        f.write(header)
        f.write(data)


def read_wav24(path: Path, rate: int = SR) -> np.ndarray:
    """读回自己写的 24bit WAV，用于自检。"""
    with open(path, "rb") as f:
        b = f.read()
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE"
    pos = 12
    data = None
    while pos + 8 <= len(b):
        cid, size = b[pos:pos + 4], struct.unpack("<I", b[pos + 4:pos + 8])[0]
        body = b[pos + 8:pos + 8 + size]
        if cid == b"data":
            data = body
            break
        pos += 8 + size + (size & 1)
    assert data is not None
    a = np.frombuffer(data, dtype=np.uint8).reshape(-1, 3)
    i32 = (a[:, 0].astype(np.int32)
           | (a[:, 1].astype(np.int32) << 8)
           | (a[:, 2].astype(np.int32) << 16))
    i32 = np.where(i32 & 0x800000, i32 - 0x1000000, i32)
    return i32.astype(np.float64) / 8_388_608.0


# ---------------------------------------------------------------- 五项指标

def frame_rms(mono: np.ndarray, rate: int) -> np.ndarray:
    frame = max(1, round(FRAME_MS / 1000 * rate))
    hop = max(1, round(HOP_MS / 1000 * rate))
    n = (len(mono) - frame) // hop + 1
    if n <= 0:
        return np.zeros(0)
    c = np.concatenate([[0.0], np.cumsum(mono.astype(np.float64) ** 2)])
    starts = np.arange(n) * hop
    sums = c[starts + frame] - c[starts]
    return np.sqrt(sums / frame)


def db20(x: float) -> float:
    return 20.0 * math.log10(max(float(x), sys.float_info.min))


def goertzel_power(samples: np.ndarray, rate: int, freq: float) -> float:
    n = len(samples)
    if n == 0:
        return 0.0
    omega = 2 * math.pi * freq / rate
    coeff = 2 * math.cos(omega)
    s1 = s2 = 0.0
    for x in samples:
        s0 = float(x) + coeff * s1 - s2
        s2, s1 = s1, s0
    return max(0.0, (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n * n))


def effective_bandwidth(mono: np.ndarray, rate: int) -> float:
    win = min(8192, len(mono))
    if win == 0:
        return 0.0
    start = min(len(mono) // 4, len(mono) - win)
    window = mono[start:start + win]
    probes = [f for f in BW_PROBES if f < rate / 2 * 0.95]
    if not probes:
        return 0.0
    powers = [goertzel_power(window, rate, f) for f in probes]
    peak = max(powers)
    if peak <= 0:
        return 0.0
    cutoff = probes[0]
    for f, p in zip(probes, powers):
        if p >= peak * BW_CUTOFF_POWER:
            cutoff = f
    return cutoff


def snr_db(rms: np.ndarray) -> float:
    if len(rms) == 0:
        return 0.0
    s = np.sort(rms)
    n = len(s)
    nn, sn = max(1, n // 10), max(1, n // 5)
    noise = float(s[:nn].mean())
    signal = float(s[n - sn:].mean())
    if noise <= 1e-12:
        return 120.0
    return float(np.clip(db20(signal / noise), 0.0, 120.0))


RT60_MAX_SEARCH_S = 3.0  # T20 只在峰值后 3s 内找衰减，跨乐句会算出荒谬值


def rt60_t20(rms: np.ndarray) -> float | None:
    if len(rms) < 4:
        return None
    peak_idx = int(np.argmax(rms))
    peak = float(rms[peak_idx])
    if peak <= 1e-12:
        return None
    target = peak * 0.1
    max_lag = math.ceil(RT60_MAX_SEARCH_S / (HOP_MS / 1000))
    end = min(peak_idx + 1 + max_lag, len(rms))
    for i in range(peak_idx + 1, end):
        if rms[i] <= target:
            t20 = (i - peak_idx) * HOP_MS / 1000
            return max(t20 * 3.0, HOP_MS / 1000)
    return None


def compute_metrics(mono: np.ndarray, rate: int, bits: int, channels: int) -> dict:
    """与 Rust 侧逐项对齐的独立实现。

    注意：Rust 的峰值与削顶率按**声道独立**统计（见 azzer-dsp/src/metrics.rs），
    本夹具是单声道，两种口径等价。**若将来换成立体声夹具，这里必须改成
    按声道取最大 / 按声道统计**，否则交叉验证会失去意义。
    """
    n = len(mono)
    rms = frame_rms(mono, rate)
    peak = float(np.max(np.abs(mono))) if n else 0.0
    clipped = int(np.count_nonzero(np.abs(mono) >= CLIP_THRESHOLD))
    return {
        "effective_bandwidth_hz": effective_bandwidth(mono, rate),
        "snr_db": snr_db(rms),
        "clip_ratio": clipped / n if n else 0.0,
        "rt60_s": rt60_t20(rms),
        "bitrate_bps": float(rate * bits * channels),
        "duration_s": n / rate,
        "peak_dbfs": db20(peak),
    }


# ---------------------------------------------------------------- 命令

def make_signal() -> np.ndarray:
    n = SR * SECONDS
    t = np.arange(n, dtype=np.float64) / SR
    rng = np.random.default_rng(20261108)
    # 全程低幅噪声底，让 SNR 落在可判定的区间而不是数字静音
    x = (rng.standard_normal(n) * 0.001).astype(np.float32)

    seg = (t < 30)
    x[seg] += (0.35 * np.sin(2 * np.pi * 440 * t[seg])).astype(np.float32)

    # 30–60s：只有噪声底的准静音段
    seg = (t >= 60) & (t < 180)
    tt = t[seg]
    mix = (0.30 * np.sin(2 * np.pi * 220 * tt)
           + 0.20 * np.sin(2 * np.pi * 1000 * tt)
           + 0.12 * np.sin(2 * np.pi * 4000 * tt)
           + 0.08 * np.sin(2 * np.pi * 8000 * tt)
           + 0.05 * np.sin(2 * np.pi * 12000 * tt))
    x[seg] += mix.astype(np.float32)

    # 180–200s：带指数衰减的尾音，供 RT60 的 T20 外推
    seg = (t >= 180) & (t < 200)
    tt = t[seg] - 180
    x[seg] += (0.40 * np.exp(-tt * 1.2)
               * np.sin(2 * np.pi * 660 * tt)).astype(np.float32)

    # 200–240s：安静尾段
    return np.clip(x, -1.0, 1.0).astype(np.float32)


def cmd_gen() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    mono = make_signal()
    write_wav24(FIXTURE, mono)
    back = read_wav24(FIXTURE)
    max_err = float(np.max(np.abs(back - mono.astype(np.float64))))
    print(f"夹具：{FIXTURE}  ({FIXTURE.stat().st_size / 1e6:.1f} MB, {SECONDS}s, {SR}Hz, 24bit)")
    print(f"写读自检最大误差：{max_err:.3e}（应为 24bit 量化级）")

    m = compute_metrics(mono.astype(np.float64), SR, 24, 1)
    EXPECTED.write_text(json.dumps(m, indent=2, ensure_ascii=False), encoding="utf-8")
    print("Python 独立基准：")
    for k, v in m.items():
        print(f"  {k:26s} = {v}")
    return 0 if max_err < 1e-6 else 1


TOLERANCE = {
    "duration_s": 1e-6,
    "peak_dbfs": 2e-3,
    "clip_ratio": 1e-9,
    "bitrate_bps": 1e-6,
    "snr_db": 5e-2,
    "effective_bandwidth_hz": 1.0,
    "rt60_s": 2e-2,
}


def cmd_verify(report_path: str) -> int:
    if not EXPECTED.exists():
        print("缺少 expected.json，请先运行 gen")
        return 2
    expected = json.loads(EXPECTED.read_text(encoding="utf-8"))
    report = json.loads(Path(report_path).read_text(encoding="utf-8"))
    got = report["grade"]

    print(f"{'指标':<28}{'Python 独立值':>18}{'Rust 值':>18}{'容差':>12}  判定")
    ok = True
    for k, tol in TOLERANCE.items():
        e, g = expected.get(k), got.get(k)
        if e is None or g is None:
            passed = e is None and g is None
            ok &= passed
            note = "双方一致标 not_applicable" if passed else "一方为 null 另一方有值"
            print(f"{k:<28}{'null' if e is None else f'{e:.6f}':>18}"
                  f"{'null' if g is None else f'{g:.6f}':>18}{'-':>12}"
                  f"  {'PASS' if passed else 'FAIL'}  ({note})")
            continue
        diff = abs(float(e) - float(g))
        passed = diff <= tol
        ok &= passed
        print(f"{k:<28}{e:>18.6f}{g:>18.6f}{tol:>12.2g}  {'PASS' if passed else 'FAIL'}"
              f"  (diff={diff:.3g})")

    print("\n零模型与幂等：")
    checks = [
        ("models_declared == 0", report.get("models_declared") == 0),
        ("capabilities.direct == true", report.get("capabilities", {}).get("direct") is True),
        ("capabilities.separate == false", report.get("capabilities", {}).get("separate") is False),
        ("path_note 明确未下载", "未触发任何模型下载" in report.get("path_note", "")),
    ]
    for label, good in checks:
        print(f"  {'PASS' if good else 'FAIL'}  {label}")
        ok &= good

    print("\nRESULT:", "PASS - R1 与独立基准一致" if ok else "FAIL - 存在偏差")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    if sys.argv[1] == "gen":
        sys.exit(cmd_gen())
    if sys.argv[1] == "verify" and len(sys.argv) >= 3:
        sys.exit(cmd_verify(sys.argv[2]))
    print("用法：r1_fixture.py gen | verify <r1_report.json>")
    sys.exit(2)
