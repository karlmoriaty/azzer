# Azzer · 技术栈重构方案 v3.1

**起因**：设计方不接受 Python 作为主语言，希望底层用 Rust（C++ 担心内存安全），前端不限但要求稳定与性能
**状态**：已被 `Azzer-tech-stack-v3.2.md` 取代（v3.2 定下桌面形态、只 Windows、含训练，并取消"纯 Rust 降级路径"）。本文保留方案对比与事实核查部分作为决策依据
**核对日期**：2026-11-08

---

## 0. 结论先行

| 问题 | 结论 |
|---|---|
| Rust 能不能做主控 | **能**，而且比 Python 更适合（无 GIL、单二进制分发、内存安全） |
| 能不能 100% 纯 Rust | **不能**。分离只有 htdemucs 一个模型可用；扩散式修复与 ACE-Step 没有 ONNX 路径 |
| 推荐方案 | **Rust 主控 + 可选 Python 推理容器**（容器对用户不可见，只暴露 HTTP） |
| 如果坚持零 Python | 可行，代价是 D1 概率从 50–70% 降到 **35–50%**，且失去"重制演绎"档 |
| 前端 | **React 18 + TypeScript + Vite**，音频用 Web Audio API 原生，波形用 Canvas |
| 界面 | 暗色克制风，暖石强调色，波形为主视觉，键盘优先 |

一句话：**Rust 承担 90% 的代码量与全部业务逻辑；剩下 10% 是"模型怎么跑"，用隔离容器解决，不是"用 Python 写业务"。**

---

## 1. 已核实的事实（决定方案边界）

| # | 事实 | 证据强度 | 影响 |
|---|---|---|---|
| F1 | **RVC 官方支持 ONNX 导出**（`tools/export_onnx.py`，WebUI 内置 Export），输入为 phone/pitch/pitchf/ds/rnd | 官方仓库文档 | Rust 可跑 RVC 推理 |
| F2 | **已有纯 Rust 的 RVC 实现 `vc-rs`**（MIT），ONNX 跑 RVC v2，支持 VST3 与实时 | 第三方项目，2026-10 仍在更新 | 存在性已证明，不必从零验证 |
| F3 | **RMVPE 与 ContentVec 都有 ONNX 版本**，`vc-rs` 直接下载使用 | 同上 | f0 与内容特征可在 Rust 侧跑 |
| F4 | **`charon-audio`**：纯 Rust 的 HTDemucs 4-stem，ONNX Runtime + Symphonia + Rubato | MIT/Apache crate，2026-09 发布 0.1.2 | 分离可纯 Rust |
| F5 | charon 与 PyTorch Demucs 的**数值 parity：最大样本差 1.2e-4，MUSDB18 上 median SDR 相等** | 作者实测（docs/MEASUREMENTS.md） | 质量不是问题，**模型数量**是问题 |
| F6 | charon **只支持 htdemucs**，不支持 htdemucs_ft / 6s / MDX-Net / RoFormer / Open-Unmix | 作者明示 | v3 的"三模型集成分离"无法纯 Rust 实现 |
| F7 | MDX23C / RoFormer 用 complex-valued STFT，ONNX 不原生支持，需用 sin/cos 双实数重写频域分支 | 社区实践记录 | 自己导出成本极高（等于重写整个频域分支） |
| F8 | ACE-Step = Qwen3 LM + DiT + flow matching，动态 shape 多 | 官方架构 | ONNX 化不现实 |
| F9 | AudioSR 一类扩散式修复同理 | 架构决定 | 纯 Rust 侧降级为谐波激励 |
| F10 | YuE2 = AR-NAR MoT + flow matching | 模型卡 | 无 ONNX 路径 |

**F5 与 F6 合起来是本方案的核心矛盾**：Rust 跑 Demucs 的**质量**没问题，但**只能跑一个模型**。v3 的三模型融合（中位数 + 保守取值）在纯 Rust 下退化为单模型。

---

## 2. 两条路与量化代价

### 2.1 方案 A：纯 Rust（零 Python）

| 环节 | 纯 Rust 能力 | 代价 |
|---|---|---|
| 解码 / 重采样 / 写盘 | ✅ Symphonia + Rubato + Hound | 无 |
| 分离 | ⚠️ 只有 htdemucs | 失去三模型融合；高混响/宽立体声成品上伪影更明显 |
| f0 提取 | ✅ RMVPE ONNX（vc-rs 已验证） | 无 |
| 音色转换 | ✅ RVC ONNX | 无 |
| 高频重建 | ⚠️ 只能谐波激励（扩散式无法 ONNX） | 8–16k 自然度下降，塑料感风险上升 |
| 呼吸/口腔声 | ✅ 纯 DSP | 无 |
| humanize / 空间 / 分段响度 | ✅ 纯 DSP | 无 |
| 人声叠层（和声生成） | ⚠️ 只能 lead 变调，无 AI 和声 | 厚度下降，需靠 double 补 |
| 母带链 | ✅ 纯 DSP | 无 |
| 重制演绎档（YuE2） | ❌ 不可用 | 失去整档功能 |

**量化影响**（基于 v3 §2.3 的环节分解）：

```
原：分离 0.95/0.85 × RVC 0.80 × 修复 0.75 × 拟真化 0.92 = 0.52
纯 Rust：分离 0.90/0.78（单模型） × RVC 0.80 × 修复 0.62（谐波激励） × 拟真化 0.92
        = 0.42（干声）/ 0.36（成品）
best-of-8 ≈ 0.62（干声）/ 0.55（成品）
```

**所以 D1 从 50–70% 降到 35–50%**。这个降幅主要来自"修复只有谐波激励"和"分离单模型"。

### 2.2 方案 B：Rust 主控 + Python 推理容器（推荐）

| 谁 | 做什么 | 代码量占比 |
|---|---|---|
| **Rust** | HTTP 服务、任务编排、DAG 调度、缓存、IR、全部 DSP（重采样/混响/humanize/母带）、文件管理、授权台账、指标计算 | **~90%** |
| **Python 容器** | 只跑三类模型：集成分离（MDX23C/UVR）、扩散式修复、ACE-Step 和声 | ~10%，且**不写业务逻辑** |

**关键设计：容器是可选插件，不是必需依赖。**

```
if 用户启用"高质量模式":
    调用 python-sidecar（分离/修复/和声）
else:
    走纯 Rust 路径（htdemucs + 谐波激励 + lead 变调）
```

这意味着：
- 不想装 Python 的用户，装 Rust 单二进制就能用（方案 A 的能力）
- 要最佳质量的用户，多装一个容器（方案 B 的能力）
- **产品上不强制，能力上可叠加**

### 2.3 方案对比

| 维度 | A 纯 Rust | B Rust + 可选容器 |
|---|---|---|
| 用户需要装什么 | 一个二进制 | 一个二进制 + 可选容器 |
| D1 概率 | 35–50% | 50–70% |
| 重制演绎档 | 无 | 有（容器内跑 YuE2） |
| 分发体积 | ~80MB | 80MB（基础）/ +3GB（容器） |
| 开发复杂度 | 中（需自己实现更多 DSP 降级路径） | 中高（多一层 IPC 与生命周期管理） |
| 许可风险 | 低 | 需核实容器内模型许可 |

**推荐 B**，理由是：A 的 15 个百分点差距全部落在"最影响成败的两个环节"（分离与高频），而这正是目标的核心。

### 2.4 如果坚持零 Python

那么必须接受三件事，写进产品说明：

1. 处理带伴奏成品时，分离残留比三模型融合高（具体差值需 M1 实测补齐）
2. 8kHz 以上是"谐波激励"而非"扩散重建"，高频会有轻微电子味
3. 没有"重制演绎"档（不能从零改写歌曲结构）

---

## 3. 推荐架构

```
┌─────────────────────────────────────────────────────────┐
│ Web 前端（React 18 + TS + Vite）                          │
│ 波形 Canvas / 播放器 WebAudio / 参数面板 / A-B 对比        │
└────────────────────────┬────────────────────────────────┘
                         │ HTTP + SSE
┌────────────────────────▼────────────────────────────────┐
│ Rust 服务（axum + tokio）                                 │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ api      路由 / SSE / 鉴权 / 静态资源                  │ │
│ │ orch     16 节点 DAG / 门禁 / 回退 / 幂等              │ │
│ │ dsp      解码·重采样·混响·humanize·母带·响度（纯 Rust） │ │
│ │ infer    引擎适配层（本地 ONNX / 远程 sidecar 同协议）  │ │
│ │ store    SQLite（sqlx）+ 内容寻址文件树                │ │
│ └─────────────────────────────────────────────────────┘ │
│                                                          │
│  ort (ONNX Runtime)                                      │
│   ├─ htdemucs.onnx       分离（charon 或自建）            │
│   ├─ rmvpe.onnx          f0                              │
│   ├─ contentvec.onnx     内容特征                         │
│   └─ rvc_{voice}.onnx    音色转换                         │
└────────────────────────┬────────────────────────────────┘
                         │ 可选 HTTP（127.0.0.1，仅推理）
┌────────────────────────▼────────────────────────────────┐
│ Python 推理容器（可选，用户显式启用）                      │
│  MDX23C/UVR 分离 · AudioSR 修复 · ACE-Step 和声 · YuE2     │
└─────────────────────────────────────────────────────────┘
```

**为什么 Rust 与 ONNX 同进程**：ONNX Runtime 是 C++ 库，`ort` 提供安全绑定。同进程省掉一次 IPC 与序列化，对"每首歌要跑 8 次推理"的 best-of-N 场景收益明显。

**为什么 Python 走独立进程**：依赖冲突（ACE-Step 要 Python 3.11、YuE2 要 3.12）、崩溃隔离（模型 OOM 不该带走主服务）、以及"用户可以不装"。

---

## 4. Rust 技术选型

| 关注点 | Crate | 理由 | 备选 |
|---|---|---|---|
| 异步运行时 | `tokio` | 事实标准 | — |
| Web 框架 | `axum` | 与 tokio/tower 同源，类型安全，SSE 一等公民 | actix-web（更快但生态分裂） |
| 序列化 | `serde` + `serde_json` | 标准 | — |
| ONNX 推理 | `ort` | ONNX Runtime 官方 Rust 绑定，支持 CUDA/CoreML/DirectML | `tract`（纯 Rust，但对复杂算子支持不全） |
| 音频解码 | `symphonia` | 支持 WAV/FLAC/MP3/AAC/OGG/ALAC/AIFF，纯 Rust | — |
| 重采样 | `rubato` | 高质量 sinc 重采样，时间对齐有测试 | — |
| WAV/FLAC 写 | `hound` / `flacenc` | 简单可靠 | — |
| DSP 原语 | `dasp` + 自研 | 采样级运算 | `fundsp`（图式 DSP，适合快速搭原型） |
| 响度（EBU R128） | `ebur128` | 标准实现 | 自研（BS.1770 公式不复杂） |
| FFT | `rustfft` | 成熟，支持实数 FFT | `realfft`（更方便） |
| 并行 | `rayon` | 分离分段并行、best-of-N 并行 | — |
| 数据库 | `sqlx`（SQLite） | 编译期 SQL 校验，异步 | `rusqlite`（同步，更简单） |
| 日志/追踪 | `tracing` + `tracing-subscriber` | 结构化 span，天然对应 DAG 节点 | — |
| 错误 | `thiserror` + `anyhow` | 库/应用分层 | — |
| 配置 | `figment` 或 `serde` + TOML | — | — |
| CLI | `clap` | 标准 | — |
| 测试 | 内置 + `insta`（快照） | 指标计算的快照回归 | — |

**不引入的**：`diesel`（宏重、编译慢）、`actix`（生态分裂）、`ndarray`（除非做矩阵运算，`ort` 直接吃 `Vec<f32>` 更省事）。

**双引擎同一协议**：本地 ONNX 引擎与远程 Python 容器都实现同一个 `Engine` trait：

```rust
#[async_trait]
pub trait Engine: Send + Sync {
    fn manifest(&self) -> &EngineManifest;
    async fn probe(&self) -> EngineHealth;
    async fn estimate(&self, req: &RenderRequest) -> Result<CostEstimate>;
    async fn render(&self, req: RenderRequest, ctx: JobContext) -> Result<RenderResult>;
    async fn cancel(&self, job_id: &str) -> Result<()>;
}
```

`LocalOnnxEngine` 与 `SidecarEngine` 是两个实现，调度层不区分。**这是"上云只改 URL"的前提。**

---

## 5. 前端选型

| 关注点 | 选型 | 理由 |
|---|---|---|
| 框架 | **React 18 + TypeScript** | 生态最成熟、类型安全、招人容易；不选 Vue 是因为音频工具需要大量底层 DOM/Canvas 控制，React 的 ref 模型更直接 |
| 构建 | Vite 5 | 冷启动快，HMR 稳 |
| 状态 | **Zustand** | 编辑态高频更新，Redux 太重；Zustand 无 Provider 嵌套 |
| 数据层 | TanStack Query | 缓存/重试/失效语义现成 |
| 路由 | TanStack Router | 类型安全路由，比 React Router 少踩坑 |
| 样式 | **CSS Variables + CSS Modules** | 不引 Tailwind：类名膨胀会拖慢大组件的可读性；设计 token 用变量表达，主题切换零成本 |
| 波形 | **自研 Canvas 2D** | 10 分钟音频约 3×10⁷ 采样点，SVG 必崩；wavesurfer.js 对多轨同步控制不够细 |
| 频谱 | Canvas + 可选 WebGL | 先 2D，性能不够再上 WebGL |
| 播放 | **Web Audio API + AudioWorklet** | 不用 Tone.js：重型封装，且我们需要 sample-accurate 的多轨同步 |
| 图标 | Lucide | 线性、克制、树摇友好 |
| 测试 | Vitest + Playwright | 单元 + 端到端 |

**性能三条硬规矩**：

1. 波形与频谱一律 Canvas，禁止 SVG/DOM 节点。
2. 播放游标用 `requestAnimationFrame` 直接改 canvas，不触发 React 重渲染。
3. 长任务进度用 SSE，前端不做轮询；每条 SSE 消息只更新局部组件。

**不选 Vue 的原因**（用户说随我定，需要给理由）：音频编辑器里"命令式操作 canvas + 细粒度控制重渲染"占比很高，React 的 `useRef` + 受控更新模型更贴合；Vue 的响应式在大量 AudioParam 更新时容易出现意外的 watcher 风暴。

---

## 6. 界面设计

### 6.1 方向

**暗色克制 + 暖石强调 + 波形为主视觉。**

参考来源：ElevenLabs 的音频产品气质（pill 按钮、克制的排版、多层低透明度阴影）+ Linear 的信息密度。**但不照搬 ElevenLabs 的浅色底**——长时间编辑音频，浅色背景会让波形与频谱的对比度不足，且刺眼。

### 6.2 Design Tokens

```
表面（三层）
  --bg-base      #0C0C0D   页面底
  --bg-surface   #141416   面板
  --bg-elevated  #1C1C1F   浮层/卡片
  --bg-hover     #232327   hover

文字（三级）
  --text-primary    #EDEDEF
  --text-secondary  #A1A1A6
  --text-tertiary   #6E6E76

强调（暖石 + 青绿双色）
  --accent-warm   #E8B27D   主操作、当前段
  --accent-cool   #5EC8B0   波形填充、进度
  --accent-danger #E06C5A   削顶、越界

边框 / 分隔
  --border-subtle  rgba(255,255,255,0.06)
  --border-strong  rgba(255,255,255,0.12)
  --focus-ring     0 0 0 1px #5EC8B0, 0 0 0 3px rgba(94,200,176,0.2)

排版
  --font-sans    Inter, "PingFang SC", "Microsoft YaHei", system-ui
  --font-mono    "JetBrains Mono", ui-monospace
  正文 13px/1.55   面板标题 14px/1.4   数字 13px mono（等宽对齐）
  段落标题 18px/1.3   页码/时间码 11px mono

间距   4 的倍数：4 8 12 16 20 24 32 40
圆角   小 6px / 中 10px / 大 14px / pill 999px
```

**为什么强调色用暖石 + 青绿**：暖石（#E8B27D）继承 ElevenLabs 的暖调，用于"人声/当前操作"；青绿（#5EC8B0）用于"波形/数据"，两者在暗底上都过 WCAG AA（对比度 ≥4.5:1）。

### 6.3 主界面布局

```
┌──────┬──────────────────────────────────────┬──────────┐
│ 左栏  │  波形区（主视觉）                      │  右栏    │
│      │  ┌────────────────────────────────┐  │          │
│ 任务  │  │ 原声 ▁▂▃▅▇▅▃▂▁▂▃▅▇            │  │ 入口     │
│ 列表  │  │ 翻唱 ▁▂▃▅▇▅▃▂▁▂▃▅▇  (A/B)      │  │ 预设     │
│      │  └────────────────────────────────┘  │ 参数     │
│ 可折叠│  [▶] 00:12 / 03:48   段落标尺 ▁▁▁▁   │ 预计耗时 │
│      │                                      │          │
│      │  节点进度：●●●○○○○○ 分离中 37%        │ 质检报告 │
└──────┴──────────────────────────────────────┴──────────┘
```

**四条交互原则**（针对"便捷性"）：

1. **上传即反馈**：不等处理，先给质检等级（S/A/B/C）与一句结论（"这条源可以直接用" / "建议重录，高频只到 11kHz"）。
2. **进度到节点**：不显示"处理中"，显示"分离 37% / 预计还需 1 分 20 秒"。用户知道在等什么，才不会以为卡死。
3. **A/B 常驻**：原声与翻唱两条波形上下叠，同一游标，一个键切换。不需要"打开对比面板"。
4. **参数即时估价**：拖动任何参数，右上角显示"预计 8 分 40 秒"，超过预算变琥珀色。

**键盘优先**（生产力工具的标志）：

| 键 | 动作 |
|---|---|
| `空格` | 播放/暂停 |
| `Tab` | A/B 切换（无需修饰键） |
| `1/2/3` | 三档预设 |
| `←/→` | 前后 2 秒 |
| `[` `]` | 标记段落入点/出点 |
| `Enter` | 开始处理 |
| `Esc` | 取消当前任务 |

### 6.4 空状态与错误

| 场景 | 文案（不用"出错了"） |
|---|---|
| 未上传 | "拖一条录音进来。清唱最好，带伴奏的成品也能用，但人声会有分离损失。" |
| C 级源 | "这条录音的高频只到 11kHz。修复能补一部分，但补的是推测的，会显得假。建议换清唱重录。" |
| 超预算 | "这首 6 分 20 秒，best-of-8 需要 24 分钟，超过你设的 20 分钟。改成单次处理约 13 分钟。" |
| 处理失败 | "分离阶段失败（模型超时）。已自动换第二套模型重试，预计多 40 秒。" |

每句都带**原因 + 下一步**，不出现"未知错误"。

---

## 7. 对 v3 的修订清单

| v3 位置 | 原内容 | 改为 |
|---|---|---|
| §9 技术选型 | Python + FastAPI + pedalboard + music21 | 本文 §3–§6 |
| §3.1 六层 | 引擎层为 Python sidecar | 引擎层为 Rust `Engine` trait + ONNX 本地推理 + 可选远程容器 |
| §5.2 分离 | 三模型集成 | 分两档：基础（htdemucs 单模型）/ 高质量（+容器内 MDX23C、UVR 融合） |
| §5.3 ③ 高频重建 | 扩散优先，超时降级谐波 | 分两档：基础（谐波激励）/ 高质量（容器内扩散） |
| §6.3 预算 | 按 Python 链估算 | 需按 Rust 链重测；Rust 侧 DSP 更快，但 ONNX 推理与 Python 差异需实测 |
| §7.2 引擎协议 | Python Protocol | 本文 §4 的 Rust `trait Engine` |
| §10 实施 | 直接写 Python 模块 | M0 改为"Rust 骨架 + ONNX 单模型跑通" |

**未受影响**：§1 产品定义、§2 技术判断、§3.3 DAG、§4 数据结构、§5 的算法与参数、§6.1 参数表、§8 评估体系。这些与语言无关。

---

## 8. 风险

| ID | 风险 | 影响 | 缓解 |
|---|---|---|---|
| S1 | charon-audio 只有 htdemucs，且 CUDA 路径未实测（作者无 N 卡） | 8GB 本机可能只能跑 CPU，速度慢 | M0 先测 CUDA EP；不行则降级 CPU + 分段并行 |
| S2 | ONNX 与 PyTorch 的数值差异 | 音质细微偏移 | charon 已给 parity 数据（1.2e-4）；RVC 侧需自测 |
| S3 | ContentVec / RMVPE 的 ONNX 权重许可 | 商用风险 | 接入前逐项核实，写进 RightsRecord |
| S4 | Rust 音频 DSP 生态不如 Python 成熟 | 某些算法需自研 | 已有 `rubato`/`rustfft`/`ebur128` 覆盖大头；混响/压缩自研（各 ~200 行） |
| S5 | 自研波形组件工作量大 | 前端工期 | 先 Canvas 2D 单游标，多轨与频谱延后到 M3 |
| S6 | 可选容器让分发变复杂 | 用户装不上 | 基础版单二进制；容器用一条 docker compose 或预打包 exe |

---

**一句话：Rust 拿走业务与 DSP，ONNX 拿走模型推理，Python 只作为"可选的模型容器"存在且用户可以不装。**
