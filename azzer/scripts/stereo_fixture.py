"""立体声回归夹具：验证母带第 1 步（低频单声道化）与第 5 步（立体声增强）。

为什么要单独的立体声夹具：之前 4 分钟夹具是单声道，母带第 1、5 步在端到端里
按设计被跳过（`mono=false`、`width=false`），等于没在真实双声道上验证过。

夹具刻意构造四种情况：

| 区段 | 内容 | 检验什么 |
|---|---|---|
| 0–30s | 左右同相的低频 + 高频 | 同相时不该被改变 |
| 30–60s | 60Hz 左右反相，4kHz 同相 | 低频反相必须被合并掉（第 1 步） |
| 60–180s | 左偏低频、右偏高频 | side 能量真实存在，加宽有效（第 5 步） |
| 180–240s | 指数衰减尾音 + 安静段 | 让 LRA 有意义 |

用法：
    python stereo_fixture.py
输出：
    <工作目录>/.cache/r2b/stereo.wav
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

import numpy as np

WORKSPACE = Path(__file__).resolve().parents[2]
OUT = WORKSPACE / ".cache" / "r2b"
SR = 48_000
SECONDS = 240
FIXTURE = OUT / "stereo.wav"


def write_wav24(path: Path, data: np.ndarray, rate: int = SR) -> None:
    """data 形状 [N, C]，手写 RIFF/WAVE 24bit PCM。"""
    n, ch = data.shape
    s = np.clip(data.astype(np.float64), -1.0, 1.0 - 1.0 / 8_388_608.0)
    i32 = np.round(s * 8_388_608.0).astype("<i4")
    raw = np.frombuffer(i32.tobytes(), dtype=np.uint8).reshape(-1, 4)[:, :3]
    payload = raw.tobytes()
    header = (
        b"RIFF" + struct.pack("<I", 36 + len(payload)) + b"WAVE"
        + b"fmt " + struct.pack("<IHHIIHH", 16, 1, ch, rate, rate * ch * 3, ch * 3, 24)
        + b"data" + struct.pack("<I", len(payload))
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        f.write(header)
        f.write(payload)
    print(f"夹具：{path}  {n / rate:.0f}s  {ch}ch  {rate}Hz  24bit  "
          f"{path.stat().st_size / 1e6:.1f}MB")


def make_stereo() -> np.ndarray:
    n = SR * SECONDS
    t = np.arange(n, dtype=np.float64) / SR
    rng = np.random.default_rng(20261109)
    # 全程低幅噪声底，避免数字静音导致门限行为异常
    left = rng.standard_normal(n) * 0.001
    right = rng.standard_normal(n) * 0.001

    def seg(lo: float, hi: float) -> np.ndarray:
        return (t >= lo) & (t < hi)

    # 0–30s：左右同相
    m = seg(0, 30)
    tt = t[m]
    common = (0.30 * np.sin(2 * np.pi * 220 * tt)
              + 0.10 * np.sin(2 * np.pi * 4000 * tt))
    left[m] += common
    right[m] += common

    # 30–60s：60Hz 反相（低频单声道化必须把这段压掉），4kHz 同相
    m = seg(30, 60)
    tt = t[m]
    low = 0.40 * np.sin(2 * np.pi * 60 * tt)
    high = 0.10 * np.sin(2 * np.pi * 4000 * tt)
    left[m] += low + high
    right[m] += -low + high

    # 60–180s：左偏低频、右偏高频（制造真实的 side 能量）
    m = seg(60, 180)
    tt = t[m]
    left[m] += (0.28 * np.sin(2 * np.pi * 120 * tt)
                + 0.06 * np.sin(2 * np.pi * 1000 * tt))
    right[m] += (0.06 * np.sin(2 * np.pi * 120 * tt)
                 + 0.24 * np.sin(2 * np.pi * 6000 * tt))

    # 180–200s：指数衰减尾音
    m = seg(180, 200)
    tt = t[m] - 180
    env = 0.40 * np.exp(-tt * 1.2)
    left[m] += env * np.sin(2 * np.pi * 660 * tt)
    right[m] += env * np.sin(2 * np.pi * 660 * tt + 0.5)

    data = np.stack([left, right], axis=1)
    return np.clip(data, -1.0, 1.0).astype(np.float32)


if __name__ == "__main__":
    if not FIXTURE.parent.exists():
        FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    data = make_stereo()
    write_wav24(FIXTURE, data)
    # 自检：左右反相段的低频必须是真的反相
    seg = data[SR * 31: SR * 32]
    corr = float(np.corrcoef(seg[:, 0], seg[:, 1])[0, 1])
    print(f"31–32s 左右相关系数 = {corr:.3f}（应明显为负，说明低频确实反相）")
    sys.exit(0 if corr < -0.3 else 1)
