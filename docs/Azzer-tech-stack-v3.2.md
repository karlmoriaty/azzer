# Azzer · 技术栈定稿 v3.2

**性质**：定稿。取代 v3.1 的方案对比，作为开工依据
**已定决策**（2026-11-08）：
1. 产品形态 = **桌面应用（Tauri 壳 + WebView2）**
2. 平台 = **只 Windows x64**（架构留跨平台抽象）
3. 首版 = **含音色模型训练**（同版本交付）

> **⚠️ 2026-11-08 晚新增约束（设计师明确要求，优先级高于本文档以下所有旧决策）**
>
> **本机所有语音模型不得绑定在 Azzer 内。** 理由有两条：一是需要自定义的权限自由，二是本机所有语音模型都是残缺、不完美的。
>
> 由此覆盖掉下文三处旧写法：
> - ~~Python 运行时「标配，非可选」~~ → **改为可选推理容器，用户可完全不装**
> - ~~「纯 Rust 降级路径作废」~~ → **改为无模型路径是必须保住的一等公民**，A 入口（同音色直通）不依赖任何模型
> - ~~「模型权重安装包内置 / 首次运行按需下载」~~ → **改为零模型分发**，软件不内置、不下载、不托管任何语音模型
>
> 详见 §6.1「模型不绑定」。

**语言分工**：Rust 主语言（~90% 代码）+ Python 仅作**可选**的模型推理与训练运行时（用户可完全不装）
**核对日期**：2026-11-08

---

## 0. 三个决策带来的变化

| 决策 | 变化 | 影响 |
|---|---|---|
| 桌面应用（Tauri） | 前端跑在 WebView2，Rust 是进程主体 | 分发从"起服务"变成"双击 exe"；接口要同时支持 IPC 与本地 HTTP |
| 只 Windows | 只验 CUDA + DirectML 两条后端 | 工期省 4 周；放弃 CoreML；`ort` 的 feature 只开 `cuda`、`directml` |
| 含训练 | 新增训练流水线（7 阶段）+ GPU 排队 | 工期 +3 周；必须解决"训练独占 GPU"与"推理要即时"的冲突 |

**已被 2026-11-08 晚的新约束覆盖**：Python 不是标配，模型也不是。「无模型路径」保住 A 入口（同音色直通）与 M1 的 N01/N02；装了模型且用户自行声明后，才额外获得分离、转换、扩散重建等能力。**D1 概率按「有模型 50–70% / 无模型 35–50%」分别估计，不再用单一数字承诺。**

---

## 1. 架构定案

```
┌───────────────────────────────────────────────────────────────┐
│ Tauri 壳（Rust，单进程主体）                                     │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐ │
│  │ WebView2                                                  │ │
│  │  React 18 + TS + Vite                                     │ │
│  │  波形 Canvas / WebAudio 播放器 / 参数面板 / A-B 对比        │ │
│  └───────────────────────┬──────────────────────────────────┘ │
│                          │ 本地 HTTP（127.0.0.1:随机端口 + token）│
│  ┌───────────────────────▼──────────────────────────────────┐ │
│  │ axum 服务（嵌在 Tauri 进程内，tokio runtime）              │ │
│  │  api      路由 / SSE / token 校验 / 静态资源               │ │
│  │  orch     DAG 编排 / 门禁 / 回退 / 幂等 / GPU 队列          │ │
│  │  dsp      解码·重采样·混响·humanize·母带·响度（纯 Rust）    │ │
│  │  infer    引擎适配：OnnxEngine（本地）/ SidecarEngine（远程）│ │
│  │  store    SQLite（sqlx）+ 内容寻址文件树                    │ │
│  └───────────────────────┬──────────────────────────────────┘ │
│                          │                                     │
│  ┌───────────────────────▼──────────────────────────────────┐ │
│  │ ort（ONNX Runtime）                                        │ │
│  │  htdemucs.onnx · rmvpe.onnx · contentvec.onnx · rvc_{v}.onnx│ │
│  │  EP: CUDA 优先 → DirectML 兜底 → CPU                        │ │
│  └──────────────────────────────────────────────────────────┘ │
└──────────────────────────┬────────────────────────────────────┘
                           │ HTTP（127.0.0.1:随机端口，仅推理端点）
┌──────────────────────────▼────────────────────────────────────┐
│ Python 推理容器（Tauri externalBin 拉起的 sidecar）              │
│  MDX23C / UVR 分离 · AudioSR 修复 · ACE-Step 和声 · YuE2        │
│  RVC 训练（PyTorch 反向传播 + faiss 索引）                       │
└───────────────────────────────────────────────────────────────┘
```

### 1.1 为什么本地 HTTP 而不是纯 Tauri IPC

| 方案 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| 纯 Tauri `invoke` | 无端口、无 token、类型自动生成 | 大文件（几十 MB 音频）走 IPC 要序列化；SSE 无原生对应；**浏览器无法调试** | 不用 |
| 纯本地 HTTP | 调试期可直接开 Chrome；SSE 原生；大文件走流式 | 需处理端口冲突与 token | **采用** |
| 混合（控制走 IPC，数据走 HTTP） | 各取所长 | 两套协议，错误处理翻倍 | 不用 |

**最终**：全部走本地 HTTP。Tauri 启动时绑 `127.0.0.1:0`（系统分配端口），生成一次性 token 注入 WebView 的 `window.__AZZER__`，前端每个请求带 `Authorization: Bearer`。`Content-Security-Policy` 只允许该端口。

这个选择的额外收益：**开发期不需要 Tauri**，`cargo run` + `pnpm dev` 就能在浏览器里做完整联调，Tauri 只在打包验证时启用。

### 1.2 进程模型

| 进程 | 生命周期 | 职责 | 崩溃影响 |
|---|---|---|---|
| `azzer.exe`（Tauri 主体） | 常驻 | UI 壳、axum、DAG、DSP、ONNX 推理 | 全部中断 |
| `azzer-infer.exe`（Python sidecar 包装） | 按需启动，空闲 15 分钟退出 | 三模型分离、扩散修复、ACE-Step、YuE2 | 降级到纯 Rust 路径并告警 |
| `azzer-train.exe`（Python sidecar） | 训练任务期间独占 | RVC 训练 | 训练失败，checkpoint 保留可续 |

**两个 sidecar 分开**，因为训练与推理的依赖不完全一致，且训练 OOM 不该带走推理进程。

---

## 2. 目录结构

```
azzer/
├── Cargo.toml                    # workspace
├── src-tauri/                    # Tauri 壳
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── icons/
│   └── src/main.rs               # 启动 axum + 打开窗口 + 注入 token
├── crates/
│   ├── azzer-api/                # axum 路由、SSE、token 中间件
│   ├── azzer-orch/               # 16 节点 DAG、门禁、回退、幂等、GPU 队列
│   ├── azzer-dsp/                # 解码/重采样/滤波/混响/压缩/humanize/母带
│   ├── azzer-infer/              # Engine trait + OnnxEngine + SidecarEngine
│   ├── azzer-store/              # sqlx + 内容寻址存储 + 授权台账
│   ├── azzer-ir/                 # Song IR（ScoreGraph/Performance/Patch）
│   └── azzer-eval/               # 37 项指标（纯函数 + 单测）
├── apps/web/                     # React + TS + Vite
│   ├── src/features/waveform/    # 自研 Canvas 波形
│   ├── src/features/player/      # WebAudio 多轨同步
│   ├── src/features/params/      # 参数面板 + 即时估价
│   └── src/api/                  # 生成的 TS client
├── py/
│   ├── infer/                    # 推理服务（FastAPI，仅模型端点）
│   │   ├── engines/separate.py   # MDX23C + UVR + 融合
│   │   ├── engines/repair.py     # AudioSR 扩散重建
│   │   ├── engines/ace_step.py   # 和声 / vocal2BGM
│   │   └── engines/yue2.py       # 重制档
│   ├── train/                    # RVC 训练（PyTorch + faiss）
│   │   ├── dataset.py  quality.py  train.py  index.py
│   └── pyproject.toml
├── models/                       # 权重清单（不含本体，git-ignored）
│   └── manifest.json             # id / sha256 / url / license
└── data/                         # 运行时数据（git-ignored）
    ├── renders/  voices/  adapters/  rights/  azzer.db
```

**边界铁律**：`py/` 下不允许出现业务逻辑（不能读数据库、不能管任务状态、不能做文件命名）。它只接受"音频 + 参数"，返回"音频 + 指标"。

---

## 3. 训练流水线（新增）

首版含训练，需要完整的七个阶段。设计原则与推理链一致：**Rust 管调度与质检，Python 只管训练循环**。

| 阶段 | 执行方 | 输入 | 输出 | 门禁 |
|---|---|---|---|---|
| T1 采集导入 | Rust | 用户音频 | `assets/{id}.wav` | 格式/时长 ≥10 分钟 |
| T2 质量质检 | Rust（纯 DSP） | 音频 | `quality.json` | SNR ≥25dB、RT60 <0.3s、峰值 ≤-3dBFS、无削顶 |
| T3 切分 | Rust | 音频 | `segments/*.wav` | 3–10s、-35dB 阈值、保留 150ms 气口；片段数 ≥30 |
| T4 特征提取 | Rust（ONNX） | 片段 | `f0.npz` / `feat.npy` | f0 八度错误率 ≤3%（RMVPE + CREPE 互校） |
| T5 训练 | **Python** | 特征 + 片段 | `G_*.pth`（每 5 epoch） | loss 收敛、无 NaN；支持暂停续训 |
| T6 索引构建 | **Python**（faiss） | 训练集特征 | `added_*.index` | 索引条数与片段数偏差 ≤5% |
| T7 帕累托扫描 | Rust（ONNX） | 模型 + 3 首测试曲 | `pareto_front.json` | 平衡档 SECS ≥0.82 且 EDR ≥0.80 |

**T4 在 Rust 侧做**的理由：feat/f0 提取是纯前向推理，ONNX 化后比 Python 快，且能与推理链复用同一套缓存。

**T7 在 Rust 侧做**的理由：需要跑 3×3=9 次转换 + 指标计算，全在 Rust 里循环比跨进程快一个数量级。

**只有 T5/T6 必须 Python**（反向传播与 faiss 没有 Rust 等价物）。

### 3.1 训练的 GPU 排队

训练独占 GPU 是硬约束（PyTorch 会吃满显存）。所以：

```
GPU 租约（lease）：
  推理任务：短租（≤60s/次），可并发 1 个
  训练任务：长租（小时级），独占，期间推理降级到 CPU 或排队

调度规则：
  - 训练任务启动前，等待当前推理任务完成（≤60s）
  - 训练期间新到的推理请求：
      交互式（预览）→ 走 CPU（慢 5-8 倍）或直接拒绝并提示"训练中"
      批量（best-of-8）→ 排队，训练结束后执行
  - 训练每 5 epoch 存 checkpoint，用户可随时暂停，释放 GPU
```

**UI 必须显示**：训练进度（epoch / 预计剩余）、当前 GPU 占用、暂停按钮、"训练期间无法做实时预览"的明确提示。

### 3.2 训练的数据门禁（不可绕过）

| 检查 | 阈值 | 失败动作 |
|---|---|---|
| 总时长 | ≥10 分钟（推荐 15–30） | 拒绝，说明差多少 |
| SNR | ≥25dB | 拒绝，建议重录或去噪后重试 |
| RT60 | <0.3s | 拒绝，建议去混响后重试 |
| 削顶率 | <0.01% | 拒绝 |
| 音域覆盖 | 低/中/高各区 ≥20% | 警告（可继续，但高音会崩） |
| 力度覆盖 | 弱/中/强各 ≥20% | 警告 |
| 授权 | `assert_trainable(scope=train)` | **拒绝**（硬门禁） |

---

## 4. 首版功能边界

### 4.1 做

| 功能 | 说明 |
|---|---|
| 三入口翻唱 | A 直通 / B 换音色 / C 带伴奏成品 |
| 三档预设 | 原件修复 / 精修翻唱 / 重制演绎 |
| 音色训练 | 七阶段全链，含帕累托扫描 |
| 三模型集成分离 | 经 Python 容器 |
| 扩散式高频重建 | 经 Python 容器 |
| 人声叠层 | double + 三度和声（ACE-Step）+ ad-lib |
| 拟真化 | 呼吸贴回 / humanize / 空间匹配 / 分段响度 / 底噪 |
| 母带链 | 七步，含真峰值与 LRA 门禁 |
| best-of-N | 分层筛选，8/4 take |
| 指标与报告 | 37 项 + 诊断树提示 |
| 音色库 | 导入 .pth / 新建 / 帕累托可视化 |
| 授权台账 | 三门禁 + 撤销级联 |

### 4.2 不做（首版）

| 不做 | 原因 |
|---|---|
| 重制演绎的完整谱面编辑器 | 首版只支持"导入 ABC + 微调"，不做钢琴卷帘 |
| 局域网多设备访问 | 只绑 127.0.0.1 |
| macOS / Linux | 只 Windows |
| DAW 插件（VST3） | 架构上留口子（axum 可被插件宿主调用），首版不做 |
| 实时演唱 | 延迟目标是分钟级 |
| 云端推理 | 全部本地 |

---

## 5. 安装包与模型分发

**模型不绑定后，分发体积不再是模型决定的。** 软件只带自己：

| 组件 | 体积（估） | 分发方式 |
|---|---|---|
| `azzer.exe`（Tauri + Rust + 前端） | ~25MB | 安装包内置 |
| ONNX Runtime + CUDA EP（运行时，非模型） | ~2.2GB | 本机绿色部署，见 §8.6 |
| Python 运行时 | — | **不分发**，由用户自行准备（可完全不装） |
| 任何语音模型权重 | — | **不分发、不内置、不下载、不托管** |

**模型从"安装包的一部分"变成"用户自己管理的外部资产"**：用户在工作目录内自行放置、自行声明路径、自行决定启用哪个。软件只读引用，不写入、不升级、不删除。缺省状态是零模型，此时 A 入口与 N01/N02 必须照常工作。

**首次运行引导**（三步，每步可跳过）：
1. 检测 GPU 与驱动 → 报告"CUDA 可用 / 仅 CPU"
2. 扫描用户配置的模型目录 → 报告"已声明 N 个 / 未声明（当前走无模型路径）"，**不触发任何下载**
3. 说明如何自行声明模型（可跳过）

### 5.1 模型不绑定（不可动摇）

**约束原文**：本机所有语音模型都不绑定在软件内。一是需要自定义的权限自由，二是本机所有模型都是残缺、不完美的。

**四条硬规则**：

| # | 规则 | 代码里的体现 |
|---|---|---|
| 1 | **不内置** | 安装包、仓库、编译产物里都不含任何模型权重；`native/` 只放运行时 DLL（ONNX Runtime / CUDA），不含模型 |
| 2 | **不假设** | 启动时不检查"必需模型是否存在"；缺模型是正常状态，不是错误 |
| 3 | **不下载** | 无自动下载、无首次运行拉取、无模型仓库 URL |
| 4 | **不托管** | 模型路径由用户在 `models.toml` 自行声明；软件只读，不写入/升级/删除/移动 |

**权限自由落在哪**：用户决定模型放在哪个目录、用哪个版本、什么时候换、给不给 Azzer 用。Azzer 只持有 `ModelRef`（一个声明：ID、角色、路径、可选校验和），运行时按需校验文件是否还在；文件不在就降级，不尝试修复。

**残缺模型的处理原则**：不把本机任何模型的输出当作质量基准或回归金标准。验收用合成信号 + 独立交叉计算；模型只作为"用户自愿接入的一个可替换环节"。这直接决定了 §8.8 里 R1 的夹具为什么是合成 WAV 而不是本机翻唱素材。

**对架构的影响**：

- L2（编排层）与 L4（引擎层）之间只走 `Engine` trait，**模型名、路径、格式不进 L3 计划层**。
- **六层的完整定义、crate 映射与禁止项见 `Azzer-分层映射.md`**（2026-11-09 补）。该文档配套可执行检查 `azzer/scripts/check-layering.cjs`，把「L4 不许 `use ort`」这类规则从口头约定变成能挡住编译的东西。
- 幂等键里的 `engine_version` / `adapter_hash` 照旧，但 `model` 由声明产生，不由代码常量产生。
- 能力分档：无模型 → A 入口可用；用户声明分离模型 → B 入口可用；声明转换模型 → C 入口可用。缺哪档就隐藏哪档入口，不弹错误。
- M1 的 R1（N01/N02）完全不碰模型，正好是这条约束的第一个实测点。

---

## 6. 接口设计（Tauri 场景下的调整）

沿用 v3 §7.1 的 REST 结构，但补三条桌面应用特有的：

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/v1/system/info` | 启动时一次：GPU/驱动/显存/模型就绪状态/版本 |
| POST | `/v1/system/download` | 触发模型下载（带 SSE 进度） |
| POST | `/v1/train/{voice_id}/pause` · `/resume` | 训练控制 |

**token 中间件**：除 `/health` 外全部要求 `Authorization: Bearer {token}`。token 在进程启动时随机生成，只注入 WebView，不写日志、不落盘。

**SSE 事件补充**：`gpu` 事件（训练/推理切换时推送，前端据此禁用交互按钮）。

---

## 7. 实施路线

| 里程碑 | 内容 | 工期 | 验收（可执行） |
|---|---|---|---|
| **M0** 骨架 | Tauri 壳 + axum + token + 前端骨架 + 本地 HTTP 联调 | 1.5 周 | 双击 exe 出窗口；`curl` 带 token 能通；不带 token 401 |
| **M1** 单链跑通 | N01–N16 全链，A 入口（同音色直通）走通；分离/转换环节**仅在用户自行声明模型后**启用 | 2 周 | 一条 4 分钟源端到端 **≤12 分钟**（A 入口）；37 项全出 |

**M1 口径收口（2026-11-08）**：v3 §10 写「≤12 分钟、37 项全出」，本表原先写「≤6 分钟、37 项出报告」，两个数会打架。按约定冲突以 `Azzer-design-plan-v3.md` 为准，**验收线取 ≤12 分钟**；6 分钟降为纯 Rust 路径的优化目标，不作为验收条件。另外本表原写的「分离(htdemucs)」在模型不绑定约束下不再是 M1 必需项，改为可选环节。
| **M2** 音色转换 | ONNX RVC + 四门禁 + Python 容器接入 + 分档 | 2 周 | 导入 .pth 能转换；八度错 ≤1%；容器崩溃能降级 |
| **M3** 训练 | T1–T7 全链 + GPU 排队 + 帕累托可视化 | 3 周 | 15 分钟干声训出模型；平衡档 SECS ≥0.82；训练可暂停续 |
| **M4** 拟真化 | 呼吸/humanize/空间/分段响度/扩散重建 | 2.5 周 | HumanizeScore ≥0.75；BreathRetention ≥0.90；LRA ≥8 |
| **M5** best-of-N 与判别 | 分层筛选 + 37 项 + Turing 测试 | 2 周 | **识破率 ≤40% 且 ≤同条件 Suno ×1.1** |
| **M6** 收口 | 母带七步 + 诊断树 + 安装包 + 首次运行引导 | 1.5 周 | 安装包可装可卸；48k/24bit + stems 导出 |

**合计 14.5 周**（约 3.5 个月）。M3 与 M4 可以部分并行（不同的人做训练与拟真化）。

**关键路径**：M1 → M2 → M3。M4 依赖 M2 的转换产物，M5 依赖 M4。

---

## 8. 本机环境实测（2026-11-08，已跑通）

开工前把环境探到底了，结果如下。**结论：不需要安装任何东西，但有一个坑要绕。**

| 项 | 实测 | 结论 |
|---|---|---|
| Rust | 1.98.1 `stable-x86_64-pc-windows-msvc` | ✅ 已装，目标三元组正确 |
| MSVC | BuildTools 2022 + MSVC **14.44.35207**，`link.exe` 在 `VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64` | ⚠️ **未注册到 VS Installer**（`vswhere -all` 返回 `[]`），rustc 探测不到 |
| Windows SDK | 10.0.26100.0（另有 22621.0） | ✅ |
| WebView2 | 157.0.4341.42 | ✅ Tauri 可用 |
| Node | v22.22.2 | ✅ |
| Python | 3.12 在 `C:\Users\asus\AppData\Local\Programs\Python\Python312`；系统默认 3.14.4 | ✅ 用 3.12 建 venv（3.14 太新，PyTorch 无轮子） |
| GPU | RTX 4060 Laptop 8188 MiB，驱动 581.42 | ✅ CUDA 可用 |
| 磁盘 | C 171G / D 145G / E 119G / **G 29G** | ⚠️ `target` 与模型放 D，不放 G |
| cargo 镜像 | ustc sparse | ✅ 首次拉依赖约 2 分钟，之后 0.4s |

### 8.1 唯一的坑与绕法

Build Tools 装了完整组件（`link.exe`、头文件、库都在），但**没有注册到 VS Installer**，所以：

```
$ rustc hello.rs
error: linker `link.exe` not found
```

**不需要重装或加装组件**，只要手工注入环境变量，等价于 `vcvars64.bat`：

```
PATH    += {MSVC}\bin\Hostx64\x64  ; {SDK}\bin\{ver}\x64
INCLUDE += {MSVC}\include ; {SDK}\Include\{ver}\{ucrt,um,shared,winrt}
LIB     += {MSVC}\lib\x64 ; {SDK}\Lib\{ver}\{ucrt,um}\x64
```

已脚本化为 `azzer/scripts/msvc-env.ps1` 与 `azzer/scripts/cargo-run.cjs`，版本号自动探测，Build Tools 升级后无需改脚本。

### 8.2 第二个坑：PowerShell 把 cargo 的 stderr 当异常

cargo 把编译进度写 stderr，PowerShell 在 `2>&1` 时抛 `NativeCommandError`，即使 `$ErrorActionPreference = Continue` 也可能中断脚本（实测日志文件都没写出来）。

**绕法**：用 Node 的 `spawnSync` 捕获 buffer，再原样转发。同时避开本机控制台编码 936 对 UTF-8 输出的破坏。这就是 `cargo-run.cjs` 存在的理由。

### 8.3 第三个坑：`ort` 的二进制下载不可用（影响分发设计）

`ort` 默认的 `download-binaries` feature 会从 GitHub Releases 拉 onnxruntime 原生库。**本机访问 GitHub 不可达，`ort-sys` 的 build script 会无限期挂住**——实测 15 分钟零输出，`target/debug/build/ort-sys-*/` 下连 `output` 与 `stderr` 都没生成。

**解法：改 `load-dynamic`，运行时加载 dll。**

| 项 | 做法 |
|---|---|
| 编译期 | `ort` 开 `load-dynamic`，不开 `download-binaries`；不链接原生库 |
| 运行期 | `ORT_DYLIB_PATH` 指向 `native/onnxruntime/onnxruntime.dll` |
| 取 dll | 从阿里云 pip 源下 `onnxruntime-directml` 的 wheel（约 100–200MB），解开取 `onnxruntime.dll` |
| 自动化 | `scripts/fetch-ort.ps1`：下载、解压、落到 `native/onnxruntime/`，幂等（已存在则跳过） |
| 编译工具 | `scripts/cargo-run.cjs` 自动设 `ORT_DYLIB_PATH`，不需要手工配环境变量 |

**架构影响（必须记住）**：Azzer 不能"纯 `cargo install` 就完事"。分发时必须自带 `onnxruntime.dll`，且该 dll 决定可用 EP：

- `onnxruntime.dll`（CPU 版）：只有 CPU EP
- `onnxruntime.dll`（DirectML 版）：CPU + DirectML EP
- CUDA EP 需要另外的 `onnxruntime_providers_cuda.dll` + CUDA 运行库

所以安装包构建流程里要显式包含 native 目录，并在首次启动时校验 dll 存在与 EP 列表。

### 8.4 第四个坑：ORT / CUDA / ort 三方版本链

这是本轮最花时间的一处。链条上任意一环错位，CUDA EP 都会静默回退到 CPU。

| 尝试 | 结果 |
|---|---|
| `ort` rc.15 + ORT 1.31.1 GPU | rc.15 要求 ORT ≥1.31 ✓，但 1.31 的 CUDA EP **只认 CUDA 13**，报 `cublasLt64_13.dll is missing` |
| `ort` rc.14 + ORT 1.30.0 GPU | 同样报 `Require cuDNN 9.* and CUDA 13.*` |
| 找 cu13 的 NVIDIA pip 包 | `nvidia-cudnn-cu13` 有真包，但 `nvidia-cublas-cu13` / `cuda-runtime-cu13` / `cufft-cu13` 只有 **0.0.1 占位包**，不可用 |
| **`ort` rc.10 + ORT 1.26.0 GPU** | ✅ 1.26 是 PyPI 上最后一个默认 CUDA 12.8 的 GPU 包，与 cu12 的 NVIDIA 运行库匹配 |

**官方文档依据**：从 1.27 起，发布到 PyPI 的 `onnxruntime-gpu` 默认用 CUDA 13.0 构建；1.26 及更早默认 CUDA 12.8。1.27–1.30 另有 CUDA 12 专用包，但不在 PyPI 主线上。

**最终版本链**：

```
ort =2.0.0-rc.10  ←→  onnxruntime-gpu 1.26.0  ←→  CUDA 12.9 / cuDNN 9.28 / cuBLAS 12.9
```

**四个必须记住的细节**：

1. Cargo 对 pre-release 版本的 caret 范围会把更高的 rc 也算进去：写 `ort = "2.0.0-rc.9"` 实际装到 **rc.15**。必须写 `=2.0.0-rc.10` 精确锁定。
2. **不能用 rc.9**：它与自动解析出的 `ort-sys` rc.10 不兼容，编译时报 267 个函数指针类型错误。ort 与 ort-sys 必须同版本，所以选 rc.10。
3. rc.10 的 API：`init().commit()` 可用，但 `available_providers()` 已移除（只剩 `ExecutionProvider::is_available()` 逐个查询）。所以代码按编译期 feature 报告 EP，并用 CUDA DLL 齐全与否判定实际可用性。load-dynamic 认 **`ORT_DYLIB_PATH` 环境变量**（rc.15 是 `init_from(path)`）。
4. **必须设 `ORT_SKIP_DOWNLOAD=1`**：ort rc.10 的默认 feature 仍含 `download-binaries`，build script 会去 GitHub 拉二进制并无限期挂住（实测 25 分钟零输出）。已固化到 `cargo-run.cjs`。

**DirectML 已排除**：`onnxruntime-directml` 停在 1.24.4，与 ort 的版本要求不兼容；且 CUDA 路线已跑通，没有理由再用它。

### 8.5 第五个坑：工作目录是 FAT32，Rust 编译会失败

`G:` 卷的文件系统是 **FAT32**（卷名 VIKTOR，约 29GB）。FAT32 没有 NTFS 的原子重命名语义：

```
error: failed to write ...\deps\libonce_cell-xxxx.rmeta: 拒绝访问 (os error 5)
```

**不是权限问题**（`icacls` 显示"所有用户完全控制"），是文件系统能力问题。

**解法：关增量编译 + 单线程。**

```
CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=1
```

`scripts/cargo-run.cjs` 已设为默认。代价是**每次全量重编**，单 crate 约 7 分钟。

**FAT32 的 4GB 单文件上限**：目前最大的是 `onnxruntime_providers_cuda.dll`（324.8MB）与 `cublasLt64_12.dll`（637.7MB），安全；但后续 YuE2 权重（约 5GB）**不能放 G 盘**。

### 8.6 第六个坑：CUDA 绿色部署

用户要求"不装系统、只放工作目录"，所以不走 NVIDIA 安装包，改用 pip wheel 解出 DLL：

| 包 | 版本 | 提供的 DLL |
|---|---|---|
| `onnxruntime-gpu` | 1.26.0 | `onnxruntime.dll`、`onnxruntime_providers_cuda.dll`（324.8MB）、`providers_shared`、`providers_tensorrt` |
| `nvidia-cuda-runtime-cu12` | 12.9.79 | `cudart64_12.dll` |
| `nvidia-cublas-cu12` | 12.9.2.10 | `cublas64_12.dll`、`cublasLt64_12.dll`（637.7MB） |
| `nvidia-cudnn-cu12` | 9.28.1.4 | `cudnn64_9.dll` 及依赖 |
| `nvidia-cufft-cu12` | 11.4.1.4 | `cufft64_11.dll`（273.8MB） |

部署位置（**全部在工作目录内**）：

```
azzer/native/onnxruntime/    ORT dll          （约 340MB）
azzer/native/cuda/bin/       CUDA + cuDNN    （约 1.9GB，共 16 个 dll）
.cache/cuda-wheel/           下载的 wheel     （约 1.6GB，可删）
.build/cargo-target/         Rust 编译产物
```

**运行时如何被找到**：`azzer-infer` 启动时把 `native/cuda/bin` 前置到**进程级** `PATH`，并设 `ORT_DYLIB_PATH`。不设系统环境变量、不写注册表、不动 `C:\Program Files`。进程退出即失效。

`scripts/fetch-cuda.ps1` 负责下载、解压、部署、校验五个关键 DLL；幂等，已部署则跳过；断点续传（本机 cufft 曾下载到 54.8MB 时网络超时，加缓存判断与 `--timeout 180 --retries 5` 后补下成功）。

### 8.7 CUDA 验证结果（决定性证据）

"装了 CUDA" ≠ "CUDA 能用"。`scripts/verify_cuda.py` 用 Python 生成一个最小 ONNX 模型（MatMul），**强制**指定 `CUDAExecutionProvider`，跑一次真推理并与 NumPy 比对：

```
onnxruntime version : 1.26.0
available providers : ['TensorrtExecutionProvider', 'CUDAExecutionProvider', 'CPUExecutionProvider']
providers in use    : ['CUDAExecutionProvider', 'CPUExecutionProvider']
max abs error       : 1.9073486328125e-06
RESULT: PASS - CUDA provider served the inference
```

`providers in use` 里出现 `CUDAExecutionProvider` 且数值误差在浮点精度内，说明 GPU 确实执行了算子，不是静默回退。

**Rust 侧同样验证过**（`/v1/system/info` 实际返回）：

```json
{
  "backend": "CUDAExecutionProvider",
  "gpu_accelerated": true,
  "ready": true,
  "runtime": {
    "cuda_bin": "G:\\workspace0101\\work001\\azzer\\native\\cuda\\bin",
    "cuda_dlls": ["cudart64_12.dll", "cublas64_12.dll", "cublasLt64_12.dll",
                  "cudnn64_9.dll", "cufft64_11.dll"],
    "dylib_path": "...\\azzer\\native\\onnxruntime\\onnxruntime.dll",
    "init_error": null,
    "ort_version": "1.26.0",
    "recommended": "CUDAExecutionProvider"
  }
}
```

```
scripts/smoke-api.cjs
  ✓ /health 200
  ✓ 无 token → 401
  ✓ 错 token → 401
  ✓ 正确 token → 200，EP 2 个：CUDAExecutionProvider, CPUExecutionProvider
  ✓ 推荐后端 CUDAExecutionProvider（GPU 加速 true）
  ✓ ORT 初始化正常
  → 全部通过
```

这一步不是可选的：ORT 在 CUDA EP 加载失败时会**静默回退 CPU 并只打一条 warning**，只看"没报错"会得出错误结论。

### 8.8 已验证的最小闭环

```
cargo test -p azzer-api -p azzer-core -p azzer-infer   （rc.10 最终版本下重跑）
  → azzer-api     6 passed（4 条鉴权 + 2 条 token 比较）
  → azzer-core   10 passed（HumanizeScore 六项 + 边界）
  → azzer-infer   3 passed（dll 定位、EP 列表、探测缓存、CUDA DLL 一致性）
  → target 落在 <工作目录>/.build/cargo-target
  → 合计 19 passed / 0 failed

scripts/smoke-api.cjs
  ✓ /health 200
  ✓ 无 token → 401
  ✓ 错 token → 401
  ✓ 正确 token → 200，EP 2 个：CUDAExecutionProvider, CPUExecutionProvider
  ✓ 推荐后端 CUDAExecutionProvider（GPU 加速 true）
  ✓ ORT 初始化正常
  → 全部通过

Tauri 窗口（上一轮 M0 验证，代码未改）
  main window title: Azzer
  listen: 127.0.0.1:<随机端口>
  → 窗口内嵌服务，无 token 访问 /v1/system/info 返回 401

前端（Vite dev + Edge headless 截图）
  ✓ 双轨波形、游标同步、ResizeObserver 自适应
  ✓ 右栏运行时信息从真实接口读取
  ✓ 构建产物 151 kB / gzip 50 kB
```

**我没跑完整 `cargo test --workspace`**：FAT32 单线程下重编 Tauri 全部依赖超过 30 分钟，中止了。Tauri 壳的代码本轮未改动，且上一轮已实测窗口能启动、内嵌服务能鉴权，所以不影响本轮结论。

### 8.9 顺手修掉的两个真实缺陷

**一、`in_range` 负区间溢出。** `HumanizeScore` 的区间归一化，v2.0 写的是 `1 - (lo - x) / lo`（用下限做分母）。这在**负区间**上会溢出：底噪区间 [-62, -48]，实测 -70 时得 `1 - (-62+70)/(-62) = 1.129 > 1`，归一化失效。

改为**用区间宽度做分母并钳制到 [0,1]**：`1 - (lo - x) / (hi - lo)`。已在 `azzer-core/src/humanize.rs` 实现，并有 `in_range_negative_interval_does_not_overflow` 测试守住。**v2.0 §10.1 的公式以此实现为准。**

**二、`native` 目录找不到。** `native_dir()` 原本只查 `<祖先>/native`，但开发期布局是 `<工作目录>/azzer/native`，而 exe 在 `<工作目录>/.build/cargo-target/debug/`，上溯只能到 `<工作目录>`，永远差一层 `azzer/`。表现为 `cuda_bin: null`、`CUDA EP priority=0`、`gpu_accelerated: false`——明明 DLL 都在，系统却报告没有 GPU。已改为同时检查 `<祖先>/native` 与 `<祖先>/azzer/native`。

---

## 9. 开工清单（下一步就是这里）

| 步骤 | 动作 | 状态 |
|---|---|---|
| 1 | 环境探测（Rust/MSVC/SDK/WebView2/Node/Python/GPU/磁盘） | ✅ 完成 |
| 2 | `azzer` workspace + `azzer-core` + cargo 包装脚本 | ✅ 完成（10 测试通过） |
| 3 | ONNX Runtime 加载 | ✅ 完成（ORT 1.26.0 GPU 版） |
| 4 | axum + token 鉴权 + `/health` + `/v1/system/info` | ✅ 完成（6 测试 + 端到端冒烟） |
| 5 | 前端骨架（Vite + React + Canvas 波形） | ✅ 完成（截图验证） |
| 6 | Tauri 壳 + `initialization_script` 注入 bridge | ✅ 完成（窗口标题 Azzer，内嵌服务监听 127.0.0.1） |
| 7 | **GPU 加速（CUDA EP）** | ✅ 完成（CUDA 12.9 + cuDNN 9.28，真推理 PASS，见 §8.7） |
| 8 | **全部产物移入工作目录** | ✅ 完成（`.build/`、`.cache/`、`native/`，未改工作目录外任何位置） |

**M0 已收口，M1-R1 已交付。** R1 做完了 N01/N02、16 节点契约骨架、CAS、模型注册与零模型闭环；下一步是 M1-R2（N03/N04 分离 + N15 母带，把 A 入口跑成最短闭环），GPU 已可用。

### 9.2 M1-R1 交付记录（2026-11-08 晚）

| 项 | 结果 |
|---|---|
| 范围 | N01 ingest、N02 grade、16 节点五要素骨架、CAS、模型注册、`Engine` 协议 |
| 测试 | 55 passed / 0 failed（core 27 / dsp 15 / pipeline 4 / infer 3 / api 6） |
| 交叉验证 | `scripts/r1_fixture.py`：4 分钟 48k/24bit 合成夹具，Python 独立实现五项指标，7 项全 PASS |
| 幂等 | 干净 root 首次 `cache_hit=false`、二次 `true`，n01/n02 幂等键稳定 |
| 零模型 | `models_declared=0`、`direct=true`、`separate=false`、未创建 `models.json`、未触发下载 |
| 全部产物 | 均在 `G:\workspace0101\work001` 内 |

**交叉验证抓到的一个真实缺陷**：RT60 的 T20 外推原本从全局峰值一直搜到文件末尾，跨越后续独立乐句，夹具算出 217.86 秒的荒谬值。已把搜索窗口限制在峰值后 3 秒，找不到就返回 `None`（显式 `not_applicable`），Rust 与 Python 双方一致标 null。**这是「报全绿前先证伪自己」的实例：夹具不是形式，它真找出了错。**

### 9.3 M1-R2a 交付记录（2026-11-09）

**范围**：母带链的第 6、7 步 + A 入口端到端闭环。没有做第 1–5 步。

| 项 | 内容 |
|---|---|
| 测量层 | EBU R128 积分响度（400ms 块 / 75% 重叠 / 两级门限）、短时 3s、LRA（10th–95th 百分位）、真峰值（4× Hermite） |
| 母带 | 真峰值限幅（lookahead 5ms、release 100ms）+ 两遍响度归一（最多 3 轮，只做线性增益） |
| 闭环 | N01 → N02 → N15，`run_a_entry` 命令行入口，成品导出到 `<运行根>/out/master.wav` |
| 零模型 | 全程不需要任何语音模型；`models_declared=0`、`direct=true` |

**实测（4 分钟 / 48k / 24bit 合成夹具）**：

```
端到端耗时            11 535 ms        （M1 验收线 720 000 ms）
输入                  -11.24 LUFS / -3.18 dBTP
输出                  -14.00 LUFS / -5.95 dBTP
LRA                   45.98 LU
增益 / 限幅           -2.76 dB / 0.00 dB（1 轮达标）
第二轮                430 ms，n02/n15 computed=false（完全命中执行记录）
```

**交叉验证**：`scripts/r2a_verify.py` 用 scipy 独立实现 BS.1770 与真峰值，五项与 Rust **完全一致**（diff 均为 0.0000）。

**为什么先做测量再做好听**：N15 的契约断言（真峰值 / LRA / 响度偏差）全是测量值。尺子不准，后面五步做得好不好无从判断。第 1–5 步（低频单声道化、动态 EQ、多段压缩、谐波饱和、立体声增强）留到 R2b。

**R2a 的三处精度取舍（都已在代码注释写明）**：

| 项 | 现状 | 影响 |
|---|---|---|
| 真峰值 | 4× Catmull-Rom 插值，非 BS.1770 附录的 FIR | 对 >16kHz 成分会低估；密集采样时 TP 与 SP 相等是正常的 |
| LRA 百分位 | 线性插值（标准未规定插值方式） | 同一输入结果恒定 |
| 夹具 LRA 45.98 | 夹具含 30 秒静音段，短时响度在那里极低 | **不能当作音乐 LRA 的参考值**；真实歌曲的 LRA 在 5–15 LU 区间 |

**同时修掉的一处契约错误**：`NODE_SPECS` 里 N15 的失败回退原本写成 `Reject`，与 v3 §3.4「LRA <8 → 回 N14 降压缩比，仍不达标告警不阻断」不符，已改为 `Retry { node: N14, max_attempts: 1 }`。

### 9.4 M1-R2b 交付记录（2026-11-09）：母带七步全通

**范围**：补齐第 1–5 步，与 R2a 的第 6、7 步串成完整链（顺序不可换）。

| 步 | 实现 | 关键参数 |
|---|---|---|
| 1 | 低频单声道化 | <120Hz 合并，LR4 独立双路径 |
| 2 | 动态 EQ 去浑浊 | 200–400Hz 带，按包络动态衰减，最多 -2dB |
| 3 | 多段压缩 | 低 30/300ms 2:1；中 10/120ms 2.5:1；高 3/80ms 2:1 |
| 4 | 谐波饱和 | 3dB，混入钳制在 ≤30% |
| 5 | 立体声增强 | M/S，side 的 >200Hz 部分 +1.5dB，低频不加宽 |
| 6–7 | 限幅 + 两遍归一 | 见 §9.3 |

**分频基础件**：提供两种，按用途分开，这是本轮最重要的设计决定。

| 类型 | 性质 | 用途 |
|---|---|---|
| `LinkwitzRiley4`（独立双路径） | LP 与 HP 各 4 阶，相加**幅度全通但有相位翻转**（440Hz 处约 45°） | 替换其中一段：第 1、3 步 |
| `ComplementarySplit`（`HP = x − LP`） | 相加**精确等于输入**（误差 <1e-6） | 精确保留剩余部分：第 2、5 步 |

一开始只做了一种互补式分频，第 1 步的低频反相分量泄漏到高频路径（残留能量 0.154）；换成独立路径后又发现互补式才是第 5 步需要的。**两种不能互相替代**，已在各自注释里写明原因。

**修掉的一个真缺陷**：限幅器只按采样峰值压，没给 4× 插值留余量。前五步改变波形后，真峰值停在 **-0.85 dBTP**（超 -1.0 上限 0.15dB），N15 契约直接失败。R2a 没暴露是因为那时不需要限幅。已改为按 `真峰值 − 采样峰值` 的差值预先扣掉余量，并加回归测试 `limiter_leaves_true_peak_headroom_when_gain_is_large`。

**实测（4 分钟 / 48k / 24bit 单声道夹具，完整七步）**：

```
端到端耗时        14 006 ms
原始源            -11.2397 LUFS / -3.1847 dBTP
前五步之后        -17.0549 LUFS / -3.9053 dBTP（压缩让整体轻了 5.8dB）
输出              -14.0003 LUFS / -1.1000 dBTP
LRA               40.02 LU
增益 / 限幅       +3.055 dB / 0.250 dB（1 轮达标）
第二轮            661 ms，n02/n15 computed=false
零模型            models_declared=0
交叉验证          5 项 diff 全 0（Python scipy 独立实现）
```

`peak_after_each` = 0.6808 → 0.6717 → 0.6717 → 0.6272 → 0.6272：动态 EQ 与饱和各削掉一点峰值，没有哪一步意外放大。

**本轮已知覆盖缺口（诚实标注）**：夹具是单声道，所以第 1、5 步在端到端里按设计跳过（`mono=false`、`width=false`）。这两步只在合成双声道信号的单测里验证过（低频反相合并、同相 side 保持），**尚未用立体声源跑完整七步**。下一步补立体声夹具。

**另外一处流程教训**：`cargo test` 不会更新 `target/debug/*.exe`。第一次端到端跑的是 R2a 编译的旧可执行文件，数值与旧版一模一样、报告里也没有 `chain` 字段，差点误判成"接好了"。端到端验证前必须显式 `cargo build --bin`。

### 9.5 M1-R3 代码检查与立体声验证（2026-11-09）

**检查范围**：crate 分层、模块职责、立体声路径（R2b 遗留的覆盖缺口）、性能热点。

**结构结论**：分层守住了——`azzer-core` 只有契约与内容寻址，无音频处理；`azzer-dsp` 不依赖推理 SDK；`azzer-pipeline` 只依赖 core 与 dsp。无循环依赖。

**查到并修掉的四个问题，三个都在立体声路径上：**

| # | 问题 | 后果 | 修法 |
|---|---|---|---|
| 1 | 多段压缩给每个声道各建一个压缩器 | 左右增益不同 → **声像左右漂移** | 每帧取所有声道最大绝对值算增益，全声道同乘一条增益曲线 |
| 2 | 限幅器前视是 O(n×L) 双重循环 | 4 分钟源 27 亿次比较，10 分钟源翻倍 | 单调队列做滑动窗口最小值，O(n) |
| 3 | 压缩器包络对电平做一阶平滑 | 正弦每周期只有峰值附近几个样本高于包络，**attack 爬不上去，等于不压缩**（输入 0.8 输出 0.79999） | 改成峰值检波（立即抓峰 + 指数释放），attack/release 改作用在**增益**上 |
| 4 | 压缩包络初值取首个样本 | 首样本常是 0 → 包络被钳到 -6160dB | 与 #3 一并解决：包络初值取阈值线性值 |

**#3 的连锁判断值得记一笔**：修 #4 时我先试过「用全段峰值预热」，启动确实不漏压了，但整段都以最高峰为基准，实测把峰值从 0.606 压到 0.268（-7dB）——等于用压缩代替了响度归一，动态被吃掉。最终方案是峰值检波，压缩量由实际电平驱动。三种初值的效果都写在 `Compressor` 的注释里。

**立体声端到端（4 分钟 / 48k / 24bit 双声道）**：

```
端到端          28 176 ms（双声道比单声道慢一倍，主要是分频与压缩的运算量）
原始源          -10.33 LUFS / -5.95 dBTP
前五步之后      -17.79 LUFS / -8.96 dBTP
输出            -14.0000 LUFS / -5.1747 dBTP
LRA             36.59 LU
增益 / 限幅     +3.79 dB / 0.00 dB（1 轮达标）
链状态          mono_lows=true、width=true（第 1、5 步这次真的跑了）
peak_after_each 0.6309 → 0.6059 → 0.3651 → 0.3564 → 0.3564（压缩约 -4dB）
单声道折叠      -1.61 dB（N14 红线 ≥-3dB）
交叉验证        5 项 diff 全 0
```

**测试**：core 27 + dsp 59 + pipeline 8。dsp 新增 6 个：声道联动、压缩确实降电平、慢 attack 放过瞬态（把固有行为写成测试防止误判）、单调队列与朴素实现逐点一致、单调队列边界、单声道兼容。

**立体声夹具**：`scripts/stereo_fixture.py` 生成 240 秒双声道，刻意分四段——同相（不该被改）、60Hz 反相（低频必须被合并）、左右频段不同（制造真实 side 能量）、衰减尾音。

**仍未覆盖**：>2 声道的多声道（5.1 等）没有处理，`mono_lows` 会把所有声道平均、`stereo_width` 只处理前两路。v1 只支持单/双声道，这是明确的支持边界，不是缺陷。

### 9.6 M1-R4 N09 修复接入（2026-11-09）

**范围**：v3 §5.3 修复五步中的 ①②③（谱减去噪、条件去混响、高频重建），A 入口补成 N01 → N02 → N09 → N15。④ formant 校正只在移调时需要，⑤ 底噪注入按文档要求在混音前，均不在本轮。

**新增基础件 `stft.rs`**：2048 点 / 75% 重叠 / Hann 窗，`rustfft` 后端。**不加任何处理时必须完美重建**，这条有测试守着（误差 <1e-5）。

**两个真缺陷，都在 STFT 上，都是端到端才暴露的：**

| # | 缺陷 | 表现 | 根因 | 修法 |
|---|---|---|---|---|
| 1 | OLA 边界窗平方和极小 | 修复后峰值 **700**（正常应 <1） | Hann 窗首样本 w=0、第二样本 w²≈5.5e-12，用它做归一化除数把浮点误差放大几百倍 | 两端各补一个帧长的零，让每个样本都被完整帧覆盖 |
| 2 | 只改正频率破坏共轭对称 | 逆变换结果不是实信号 | 调用方只改前 1025 个 bin，负频率那半没同步 | 在 `process` 内部强制 `X[N-k] = conj(X[k])`，任何调用方都不会踩 |

**为什么单测没抓到**：原来的重建测试是「完全不改」（对称与边界都未破坏）和「全置零」（对称仍保持），两个极端都恰好绕开了缺陷。补了三个针对性测试：长信号重建、只改半个频谱、修复后峰值有界。

**契约断言的口径具体化**：v3 原文「重建后 8–16kHz 能量比 ≥0.6」没有定义分母，这里具体化为 `8–16kHz 能量 / 4–8kHz 能量`，理由是这是判断高频空洞最直接的量。

**实测（4 分钟双声道，完整链 N01→N02→N09→N15）**：

```
端到端          32 328 ms
输出            -14.0000 LUFS / -4.3126 dBTP / LRA 37.35
修复            rt60=5.79s → 去混响已触发；谱减与高频重建均执行
高频            8–16kHz 能量提升 +25.81 dB
peak_after_each 0.5649 → 0.5476 → 0.3540 → 0.3459 → 0.3460（无异常放大）
单声道折叠      -1.57 dB（红线 ≥-3dB）
交叉验证        5 项 diff 全 0
```

**`meets_hf_ratio = false` 是诚实的结论，不是缺陷**：立体声夹具是合成信号，最高只有 6kHz，8–16kHz 本来就没有内容。谐波激励把 4–8kHz 搬上去只得到 +25.8dB 的相对提升，绝对量仍远达不到 0.6 的比值。N09 因此标 Degraded 并记录修复方式，符合 v3 §3.4「重建不达标告警不阻断」。

**这个夹具不能用来评价高频重建的实际听感**——真实人声在 4–8kHz 有丰富内容，搬运后的 8–16kHz 会有实质东西。要判断重建质量必须用真实人声素材，那是 M2 的事。

**同时补上的一处边界**：N01 现在拒绝 >2 声道的源，并给出「先下混再导入」的明确提示。之前是静默走 `mono_lows` 的全声道平均、`stereo_width` 只处理前两路，结果不可预期。

### 9.7 M1-R5 f0 提取接入（2026-11-09）

**范围**：N05 基频与清浊音提取，零模型路径（YIN + 归一化自相关双路）。A 入口变成 N01 → N02 → N09 → N05 → N15。

**为什么用双路**：v3 的契约是「双路差异 >600 cents 帧占比 ≤1%」，本身就是要求两套独立算法交叉校验。两路一致时取平均降低单路误差，分歧时取周期度更高的一路并计入不一致率。

**性能处理**：f0 只关心 65–1200Hz 的周期性，48kHz 是浪费。先低通到 3.5kHz 再抽取到 8kHz，每帧计算量降到 1/36。

**单测（81 个全过，f0 占 9 个）**：

| 测试 | 验证什么 | 结果 |
|---|---|---|
| 纯正弦 220Hz | 基本检出 | 误差 <5Hz |
| 锯齿波 200Hz | **不误判成倍频**（f0 检测最典型的失效） | 正确 |
| 类人声（基频+6 次谐波+音量起伏） | 双路一致率 | **<1%，契约达标** |
| 类人声 + 底噪 | 噪声下的鲁棒性 | **<5%** |
| 200→400Hz 滑音 | 跟随能力 | 正确 |
| 白噪声 / 静音 | 不产生伪浊音帧 | 浊音占比 <20% / 0 |

**端到端（4 分钟双声道）**：

```
端到端          70 887 ms（f0 提取让总耗时从 32s 涨到 71s）
母带            -14.0000 LUFS / -4.3126 dBTP（不受影响）
f0 帧数         23 997（10ms hop），浊音 77.4%
f0 中位数       124.6 Hz，范围 65.8–1003.3 Hz
双路不一致率    50.19%，meets=false
交叉验证        5 项 diff 全 0
```

**50.19% 是夹具特性，不是实现缺陷**，判断依据是上面那组单测：类人声信号上一致率 <1%。立体声夹具是**多频正弦分段混合**（60 / 120 / 220 / 1000 / 4000 / 6000 Hz），段间没有谐波关系，30–60 秒还是纯噪声。这种信号本来就没有稳定的基频，两路给出不同答案是正确的。

**由此确定的适用边界（必须写进后续设计）**：

1. **f0 只对人声有意义**，不能拿音乐混合信号验证，也不能拿它的不一致率判断实现好坏。
2. **A 入口不做分离**，f0 是在未分离的全混音上提的，伴奏会干扰。真实歌曲的 f0 可信度必须用分离后的人声验证（M2）。
3. 端到端报告里 N05 因此标 `Degraded`，如实反映可信度受限，不粉饰成 Done。

**性能记录**：debug 构建下端到端 71 秒，其中 f0 约 38 秒。M1 验收线是 4 分钟源 ≤12 分钟，仍有 10 倍余量；若后续需要，把 YIN 的差函数改成 FFT 加速还能再降一个数量级。

### 9.8 M1-R6 N16 评估接入 + humanize 的设计裁决（2026-11-09）

**先裁决一处文档矛盾。** v3 §5 里对 A 入口有两条冲突的描述：

- 第 31 行：A 直通 = 跳过 RVC，只做「修复 + **拟真化** + 母带」
- 第 49 行：原件修复（默认）= 不过 RVC、不修音高、**不注入**，只做修复 + 混音 + 母带

**按第一条不可动摇判断裁决：A 入口不注入 humanize。** 理由：源就是真人唱的，气口、音准微偏差、时值弹性本来就在里面。对它做音高抖动注入不是「补不完美」，是**把已有的不完美做坏**——这正是「源才是资产，所有模型的作用是别把它做坏」要禁止的事。humanize 只对经过 RVC 转换的 AI 人声有意义（B/C 入口）。这条写进代码注释，不留在文档里当悬案。

**N16 评估接入**。A 入口链补成 N01 → N02 → N09 → N05 → N15 → N16。

**铁律实现**：`MetricValue` 是枚举而不是 `f64`，缺项只能是 `NotApplicable(原因)`，**结构上就不可能填 0**。有测试断言「37 项一个不多一个不少」「每个 not_applicable 必须带非空原因」。

**实测（4 分钟双声道）**：

```
端到端      42 936 ms
37 项       8 项有实测值，29 项 not_applicable
未达标      1 项：延迟/cache_hit（首次运行无缓存，目标 ≥40%）

有值的 8 项：
  拟真度      HNR            9.045（无参考，只报数值不判达标）
  拟真度      底噪匹配        +4.91 dB（≤6dB 达标）
  RVC 质量    八度错误率      0.001（≤1% 达标）
  RVC 质量    音域越界率      0.000（≤3% 达标）
  延迟        端到端          42.9 s（≤20min 达标）
  延迟        缓存命中        0（首次运行，不达标）
  母带        真峰值          -4.313 dBTP（≤-1.0 达标）
  母带        LRA             37.345 LU（≥8 达标）

HumanizeScore 已算子项：H1 音准偏差 std 22.68 cents、H3 LRA 37.35、H5 底噪 -60.27 dBFS
```

**29 项为什么是 not_applicable**：生成质量（FAD/CLAP/MOS）、翻唱音色（SECS/WER）、情感细节（EDR）全部依赖模型；未修改部分（UPS 三项）在 A 入口没有「未修改区段」这个概念；可控性与接缝（7 项）需要多轮交互与人工评审数据；HumanizeScore 六子项里 H2/H4/H6 分别缺 onset 检测、N10 呼吸、颤音检测。每一项的原因都写在报告里，不是一句「暂不支持」。

**注意 H1 = 22.68 cents 不能当人声证据**：这个数是「检测出的 f0 相对最近半音的偏差标准差」，在合成的多频夹具上不反映真实音准。真实人声的音准分布要用分离后的干声测，那是 M2 的事。

**N16 刻意不做执行记录缓存**：它依赖端到端耗时这类运行属性，同一份音频两次跑结论本就不同。这与 N01–N15 的处理不同，是明确的设计选择。

### 9.9 M1-R7 感知层检测（起音 / 呼吸 / 颤音）（2026-11-10）

**背景**：N16 的 37 项里，拟真度 5 项此前有 4 项标 `not_applicable`，HumanizeScore 的六子项只有三项能算，加权无从谈起。这轮把缺的三个检测补上。

**新增 `azzer-dsp/src/percept.rs`**：

| 检测 | 方法 | 已知局限 |
|---|---|---|
| 起音 | 谱通量峰值 + 局部统计阈值 + 最小间隔 | 强伴奏下会过检；A 入口未分离，这条影响真实存在 |
| 呼吸 | voicing=0 + 能量高于底噪 6dB + 频谱重心 2–6kHz | 清辅音（s/sh/f）与呼吸频谱接近，会混判 |
| 颤音 | f0 去趋势后扫 4–8Hz 归一化自相关 | f0 本身不可靠时结果不可信 |

**查到的两个真实算法问题**（都是端到端才暴露）：

1. **稳态音被判出多个起音**：只用「局部均值 + 1.2σ」当阈值时，稳态信号的谱通量本来就在 0 附近抖动，微小波动也能超过阈值。加了绝对下限（全局最大值的 8%）才压住。
2. **段落停顿被算成节奏抖动**：4 分钟分段夹具上实测 `jitter = 44997ms`，因为只检出四五个起音，间隔 30–120 秒全被当成节奏弹性。改为**只统计 ≤2 秒的相邻间隔**——换气与段落停顿不是节奏，这条对真实演唱同样成立。

**H2 的口径说明**：v3 原文是 `std(onset_measured - onset_grid)`，需要节拍网格。A 入口拿不到乐谱，这里改用**相邻起音间隔的标准差**（self-referential jitter）：真人起音间隔本就有 10–40ms 弹性，节拍器般的精确反而说明是机器。替代口径写在代码注释与返回值里，不藏在实现里。

**H4 不返回 Option 的理由**：数不出呼吸就是 0 次，而 0 次本身就是「不像真人」的证据，不该标成「无法计算」。

**BreathRetention 也能算了**：口径是母带前后的呼吸事件数之比。**这不是整条链路的呼吸保留率**——`before` 已经是 N09 修复后的信号，分离与转换阶段的呼吸损失不在这个数里。A 入口不过 RVC，所以够用；B/C 入口接进来时要换成真正的 N04 基线。

**实测（4 分钟双声道夹具）**：

```
端到端      35 760 ms
37 项       8 项有实测值，29 项 not_applicable
HumanizeScore   仍 not_applicable
  H1 音准偏差 std     22.68 cents
  H3 LRA              37.35 LU
  H4 呼吸             0.00 次/分
  H5 底噪             -60.27 dBFS
  H2 起音 jitter      缺（夹具只有四五个起音，连续间隔不足 2 个）
  H6 颤音             缺（夹具没有颤音调制）
BreathRetention not_applicable（母带前信号里没有呼吸事件）
```

**为什么仍算不出 HumanizeScore**：合成夹具是分段稳态正弦，既没有连续音符（H2 无输入）也没有颤音（H6 无输入），更没有呼吸（H4 = 0）。三个检测器在单测里用合成信号验证过——5Hz/30cents 颤音能测准、两段高重心噪声被识别为呼吸、5 秒停顿不再污染 jitter——但夹具本身不提供这些特征。

**和 f0 那一轮是同一个结论**：实现可以靠合成信号验证，**评分必须靠真实人声**。这条不解决，拟真度评估就一直是「代码写好了但没被现实检验」。

测试：dsp 94 个（percept 占 7 个）。

### 9.1 遗留约束

| 项 | 说明 |
|---|---|
| 版本链锁定 | `ort =2.0.0-rc.10` + `onnxruntime-gpu 1.26.0` + CUDA 12.x。三者任一升级前必须重跑 `verify_cuda.py` |
| FAT32 4GB 上限 | YuE2 权重（约 5GB）不能放 G 盘，需放 D/E 或拆分 |
| 编译速度 | FAT32 单线程全量重编约 7 分钟；若要提速需把 `.build` 换到 NTFS 盘 |
| `onnxruntime-gpu` 1.27+ | 默认 CUDA 13，cu13 的 cublas pip 包尚未可用；不建议升级 |

---

## 10. 风险

| ID | 风险 | 影响 | 缓解 |
|---|---|---|---|
| T1 | Tauri + WebView2 的音频性能 | 波形卡顿 | 波形用 Canvas + rAF 直改，不走 React 状态；M0 就压测 |
| T2 | ONNX Runtime CUDA EP 与驱动版本不匹配 | 装完跑不起来 | M0 先做环境探测，把不匹配变成可读提示而非崩溃 |
| T3 | charon-audio 的 CUDA 路径未实测（作者无 N 卡） | 可能只能 CPU | M0 优先验证；不行则自建 htdemucs ONNX 导出 |
| T4 | Python 容器首次下载失败（网络） | 用户卡在引导 | 断点续传 + 手动离线包导入 |
| T5 | 训练与推理抢 GPU | 训练时无法预览 | GPU 租约 + 明确 UI 提示 + 训练可暂停 |
| T6 | 安装包近 1GB 被误认为异常 | 用户流失 | 官网与引导页明写体积构成 |
| T7 | RVC ONNX 与 PyTorch 版的音质差异 | 转换质量下降 | M2 做 A/B 数值对比（SECS/EDL），差异 >3% 则改回 PyTorch 路径 |
| T8 | faiss 索引在 Windows 上的构建 | 训练卡在 T6 | 备选 hnswlib 或纯 numpy 暴力检索（10 分钟数据量下可接受） |

---

**定稿。下一步是 M0：装 Rust 工具链、起 Tauri 空壳、跑通带 token 的本地 HTTP。**
