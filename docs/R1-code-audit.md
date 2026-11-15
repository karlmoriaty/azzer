# R1 代码审核记录

**审核对象**：`azzer-core`（新增 dag/asset/model/engine）、`azzer-dsp`、`azzer-pipeline`
**日期**：2026-11-09
**结论**：发现 4 个需要修的问题（2 个会产生错误结果、2 个是设计缺陷），已全部修复并验证；另有 4 项已知限制记录在案。

---

## P0 会产生错误结果（已修）

### 1. N01 的峰值断言用单声道平均，会误拒合法源

`n01_ingest` 原本用 `decoded.mono()` 求峰值。立体声里只有一侧有信号时，平均会把峰值压低 6dB：

| 源 | 真实峰值 | 单声道平均后 | 结果 |
|---|---|---|---|
| 左 -34.4dBFS、右静音 | -34.4dBFS（合格） | -40.4dBFS | **被误判为「电平过低」而拒绝** |

**修复**：`azzer-dsp::peak_dbfs()` 取所有声道的最大绝对值，N01 改用它。
**测试**：`stereo_with_one_silent_channel_is_accepted`、`peak_is_per_channel_not_mono_average`。

### 2. N02 的削顶率用单声道平均，反相时会漏检

左右声道反相且同时削顶时，单声道混合后接近 0，削顶率算成 0，一条已被切平的源会被判成 A 级。

**修复**：`azzer-dsp::clip_ratio()` 按声道独立统计，分母为全部声道样本总数。
**测试**：`clipping_is_detected_even_when_channels_cancel_out`。

---

## P1 设计缺陷（已修）

### 3. 模型状态查询会反复读整个文件

`ModelRegistry::status()` 每次调用都读全文件算 sha256。300MB 的模型被轮询时会反复读盘。

**修复**：拆成两个方法——
- `presence()`：只判文件在不在，O(1)，供 `capabilities()` 使用；
- `verify()`：完整校验（含 sha256），只在「开始处理前」或用户点验证时显式调用。

代价是 `capabilities()` 发现不了「文件被换成同名错误文件」，需要显式 `verify()`。这是刻意的性能取舍，已在注释与测试中写明。

### 4. 幂等只省「写文件」，不省「算」

原实现里 `cache_hit` 只表示 CAS 产物已存在，五项指标照样重算。幂等键的意义是命中就不重算，否则 M2 的分离节点每次都要白跑 90 秒。

**修复**：新增 `RunCache`（`<运行根>/outcomes/<key前2位>/<key>.json`），按幂等键存「本次计算的结论」。N02 命中时直接复用上次的 `GradeMetrics`，跳过五项计算，并新增 `n02_computed` 字段区分「命中」与「真算过」。

**验证**：

```
run1: n01_cache=false n02_cache=false n02_computed=true
run2: n01_cache=true  n02_cache=true  n02_computed=false
keys equal=true   grade equal=true
```

---

## P2 已知限制（记录，暂不修）

| # | 限制 | 影响 | 何时必须处理 |
|---|---|---|---|
| 5 | CAS 写入无原子性 | 并发写同一摘要可能读到半个文件 | M2 引入并行节点前，改为「写临时文件 + 重命名」 |
| 6 | 源文件整体读入内存 | 4 分钟 24bit 立体声约 69MB，峰值内存约 3–4 倍；10 分钟约 173MB | 支持 10 分钟以上源时改流式解码 |
| 7 | `analyze()` 对空 buffer 返回 -6160 dBFS | 数字荒谬，但 N01 的「时长 >3s」会先拦截，不会流进 N02 | 若把 `analyze` 对外暴露则需要前置校验 |
| 8 | 线性重采样 + 6 点 Goertzel 粗估 | 音质与带宽精度不足 | M1-R2 换 `rubato` 与 `rustfft` |
| 9 | CAS 会把整份源文件也存一份 | 磁盘占用翻倍 | M2 起改为只存引用（源文件本就在磁盘上） |

---

## 修复后的验证

```
cargo test -p azzer-core -p azzer-dsp -p azzer-pipeline   全部通过
  pipeline 8 个（含 2 个新增的声道测试、3 个 RunCache 测试）

交叉验证（4 分钟 48k/24bit 单声道夹具）：
  duration_s / peak_dbfs / clip_ratio / bitrate_bps / snr_db / effective_bandwidth_hz
  → 7 项全 PASS，最大偏差 7.6e-07

零模型：models_declared=0、direct=true、separate=false、未触发下载
```

**审核没发现的问题不代表没有。** 这份记录只覆盖我已经想到的检查面：多声道、边界、幂等语义、文件读写、性能路径。R2 接入母带（EBU R128 / 真峰值限幅）后会再集中审一次数值实现。
