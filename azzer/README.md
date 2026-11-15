# Azzer

把任意一条真人演唱，变成目标音色唱的、听不出是 AI 的、可发布的那一版。

**当前状态**：M1-R7（A 入口链路 `N01→N02→N09→N05→N15→N16` 打通，137 项测试通过，浏览器端到端 10 项断言全过；M2 的分离与音色转换尚未开始）
**设计文档**：`../docs/Azzer-design-plan-v3.md`（总案）、`../docs/Azzer-tech-stack-v3.2.md`（技术栈定稿）

---

## 里程碑进度

| # | 项 | 状态 |
|---|---|---|
| 1 | 工具链（Rust + MSVC 注入 + cargo 包装） | ✅ 可用（137 个单测跑通） |
| 2 | `azzer-core`：HumanizeScore 六项判定 | ✅ 含负区间防溢出测试 |
| 3 | `azzer-infer`：ONNX Runtime 加载与 EP 探测 | ✅ ORT 1.26.0 + CUDA EP 实测生效 |
| 4 | `azzer-api`：axum + token 鉴权 + 上传/轮询/取成品 | ✅ 8 个测试（含全链路回归）+ 端到端冒烟 |
| 5 | 前端：Vite + React + Canvas 波形 + 双轨 A/B | ✅ 构建通过，Edge headless 10 项断言 |
| 6 | `azzer-shell`（实际在 `src-tauri/`）：Tauri 壳 + `initialization_script` 注入 bridge | ⏳ 代码就绪，首次编译（300+ 依赖）未做 |
| 7 | 图标（PNG + ICO，零依赖生成） | ✅ 32/128/256 + ico，图形与品牌主标 01 同源（`../design/brand/azzer-mark.svg`） |

### 本机关键约束（已实测）

- **工作目录所在的 F 盘是 FAT32**，Rust 编译会报 `拒绝访问 (os error 5)`。必须关增量编译并单线程：`CARGO_INCREMENTAL=0`、`CARGO_BUILD_JOBS=1`。`scripts/cargo-run.cjs` 已设为默认。**时长分两种，别混**：冷缓存首次（含 468 个依赖包）**18 分 29 秒**；依赖就绪后只重编自有 crate 约 7 分钟（2026-11-11 实测）。
- **没有系统级 CUDA**：不走 NVIDIA 安装包，改用 pip wheel 把 DLL 解到 `native/`，运行时把 `native/cuda/bin` 前置到进程 PATH。不写系统目录、不改注册表、不设永久环境变量。
- **DirectML 不可用**：`onnxruntime-directml` 停在 1.24.4，与当前 `ort` / ORT 版本链不兼容。GPU 只走 CUDA（版本链三方锁定，见下）。
- **所有产物都在工作目录内**：编译缓存在 `.build/cargo-target`，下载缓存与解压在 `.cache/`，原生库在 `native/`。

## 原生库布局

```
native/
├── onnxruntime/          onnxruntime.dll + providers（由 fetch-ort 或 fetch-cuda 部署）
└── cuda/bin/             CUDA 12.x + cuDNN 9 运行库（16 个 dll，约 2.0 GB，fetch-cuda 部署）
```

**版本链三方锁定**——任一升级前必须重跑 `scripts/verify_cuda.py`（它会强制 CUDA EP 真跑一次 MatMul 并与 NumPy 比对，防止静默回退 CPU）：

| 组件 | 锁定版本 | 为什么不能随便升 |
|---|---|---|
| `ort` crate | `=2.0.0-rc.10` | rc.15+ 要求 ORT ≥1.31，而 1.30/1.31 的 CUDA EP 只认 CUDA 13 |
| `onnxruntime-gpu` | 1.26.0 | 从 1.27 起 PyPI 的 GPU 包默认 CUDA 13，而 cu13 的 cublas/cudart pip 包仍是 0.0.1 占位 |
| CUDA / cuDNN | 12.9 / 9.28 | 与上面两项匹配；三者必须同时对齐 |

`ort` 版本必须写 `=` 精确锁定：Cargo 对 pre-release 的 caret 范围会把更高的 rc 也算进来（写 `2.0.0-rc.9` 实际装到 rc.15）。另外必须设 `ORT_SKIP_DOWNLOAD=1`，否则 build script 会去 GitHub 拉二进制并无限期挂住（本机不可达）。

分别由以下脚本生成（都在工作目录内下载与解压，幂等）：

```powershell
# CPU 版（只用 CPU 时）
powershell -ExecutionPolicy Bypass -File scripts/fetch-ort.ps1

# GPU 版（CUDA 12.x + cuDNN 9，会替换 onnxruntime.dll 为 GPU 版本）
powershell -ExecutionPolicy Bypass -File scripts/fetch-cuda.ps1
```

## 验收与检查

```powershell
# 一键：分层依赖方向 + Rust 测试 + 前端类型检查 + 前端构建
node scripts/check-all.cjs

# 只查结构，跳过 Rust 测试（约 10 秒）
node scripts/check-all.cjs --fast

# 单独跑分层检查（改 crate 依赖后必跑）
node scripts/check-layering.cjs

# 浏览器端到端：自管启停 API 与前端，上传→处理→截图→断言
node scripts/e2e-web.cjs

# 对任意一条人声跑完整 A 入口，并判读「这条源现在能算出什么」
# （37 项里有值 / 不适用各几项、HumanizeScore 缺哪个子项）
node scripts/check-source.cjs D:\path\to\vocal.wav
```

**分层检查不是摆设**。它守的是「L4 调度层不许直接 `use ort`」这类规则——违反了编译照样通过，没人会发现。分层定义与禁止项见 `../docs/Azzer-分层映射.md`。

## M1-R2b 已交付（母带完整七步）

补齐第 1–5 步，与 R2a 的第 6、7 步串成完整链：`低频单声道化 → 动态 EQ 去浑浊 → 多段压缩 → 谐波饱和 → 立体声增强 → 真峰值限幅 → 两遍响度归一`。

分频提供两种，按用途分开：`LinkwitzRiley4`（独立双路径，适合替换一段）与 `ComplementarySplit`（`HP = x − LP`，精确重建，适合保留一段）。两者不能互相替代，详见 `../docs/Azzer-tech-stack-v3.2.md` §9.4。

完整七步实测：4 分钟源 14 秒，输出 **-14.0003 LUFS / -1.1000 dBTP**，1 轮达标，第二轮 661ms 全命中；Python 交叉验证 5 项 diff 全 0。

**运行前注意**：`cargo test` 不会更新 `target/debug/*.exe`，端到端验证前要显式 `cargo build -p azzer-pipeline --bin run_a_entry`，否则跑的是旧可执行文件。

**覆盖缺口**：夹具是单声道，第 1、5 步在端到端里按设计跳过，目前只有合成双声道单测覆盖；立体声源的完整七步尚未跑过。

## M1-R2a 已交付（母带第 6-7 步 + A 入口闭环）

**测什么**：EBU R128 积分响度（400ms 块 / 75% 重叠 / 两级门限）、短时 3s、LRA、真峰值（4× 插值）。
**做什么**：真峰值限幅（lookahead 5ms）+ 两遍响度归一（最多 3 轮，只做线性增益，不用压缩代替归一）。
**没做什么**：母带第 1–5 步（低频单声道化 / 动态 EQ / 多段压缩 / 谐波饱和 / 立体声增强）留到 R2b。

```powershell
# A 入口：同音色直通，零模型可跑
# 路径用相对工作目录的写法，换盘符不用改（脚本 cwd 是 azzer/）
node scripts/cargo-run.cjs run -p azzer-pipeline --bin run_a_entry -- `
  ..\.cache\r1\fixture.wav `
  ..\.cache\r2a\run `
  ..\.cache\r2a\report.json

# 成品导出到 <运行根>/out/master.wav
# 交叉验证（Python 独立实现 BS.1770，与 Rust 比对）
python scripts\r2a_verify.py <原始WAV> <master.wav> <report.json>
```

实测：4 分钟源端到端 11.5 秒，输出 -14.00 LUFS / -5.95 dBTP，第二轮 430ms 全命中缓存；Python 交叉验证五项 diff 全为 0。

**三处精度取舍**（代码注释里也写了）：真峰值用 4× Catmull-Rom 而非标准 FIR（>16kHz 会低估）；LRA 百分位用线性插值；夹具含 30 秒静音，其 LRA 45.98 不能当音乐参考。

## M1-R1 已交付（N01 ingest + N02 grade，零模型可跑）

**架构约束（新）**：本机所有语音模型都不绑定在软件内。不内置、不假设、不下载、不托管；用户自行在 `models.json` 声明路径（模板见 `models.example.json`）。缺模型是正常状态，A 入口照常可用。详见 `../docs/Azzer-tech-stack-v3.2.md` §5.1。

| crate | 内容 |
|---|---|
| `azzer-core`（扩展） | 16 节点五要素契约、幂等键 `sha256(上游+参数+引擎版本+adapter_hash)`、CAS 内容寻址、模型注册、`Engine` 协议 + `NoEngine` |
| `azzer-dsp`（新） | WAV 24bit 解码/编码、48k 归一化（48k 输入零损失直通）、质检五项（有效带宽 / SNR / 削顶率 / 估计 RT60 / 码率）、S/A/B/C 分级 |
| `azzer-pipeline`（新） | N01/N02 契约节点、CAS 产物落盘、契约断言与失败回退、R1 报告与 CLI |

```powershell
# 生成 4 分钟回归夹具与 Python 独立基准
python scripts/r1_fixture.py gen

# 跑 R1 闭环（第三参数是输出 JSON，由 Rust 直接写 UTF-8，避开 PowerShell 的 GBK 解码）
node scripts/cargo-run.cjs run -p azzer-pipeline --bin run_r1 -- `
  ..\.cache\r1\fixture.wav `
  ..\.cache\r1\run `
  ..\.cache\r1\report.json

# 交叉验证
python scripts/r1_fixture.py verify ..\.cache\r1\report.json
```

**实测结果**：55 个测试全过；Python 独立基准 7 项全 PASS（SNR 差 2.06e-08）；干净 root 首次 `cache_hit=false`、二次 `true`，幂等键稳定；`models_declared=0`、`direct=true`、`separate=false`，未触发任何模型下载。

**R1 里的诚实标注**：重采样是线性插值占位（M1-R2 换 `rubato`）；有效带宽用 6 点 Goertzel 粗估（M1-R2 换 `rustfft`）；RT60 用 T20 外推，估计不出时返回 `null` 而不是填 0。这些不影响 N01/N02 的契约与幂等验证。

## 已通过的验证（2026-11-11 迁移到 F 盘后复验）

```
cargo test（4 个 crate，共 137 条）
  azzer-core      27 passed
  azzer-dsp       94 passed
  azzer-pipeline   8 passed
  azzer-api        8 passed   ← 含 4 条鉴权测试与 1 条「HTTP 进、HTTP 出」全链路回归

node scripts/check-all.cjs --fast
  PASS  分层依赖方向
  PASS  前端类型检查
  PASS  前端生产构建（29 模块 / 159.5 KB）

node scripts/e2e-web.cjs
  10 项断言全过；页面数值与后端一致（页面 -14.12 / 后端 -14.124）
  5 秒源处理 0.97–1.0 s，37 项全出，成品 703 KB RIFF 合法

GPU 通路
  backend = CUDAExecutionProvider，gpu_accelerated = true
  （ORT 1.26.0 + CUDA 12.9 + cuDNN 9；ort rc.10 会提示期望 1.22.x，可忽略）

R1 数值交叉验证（Python 独立实现 vs Rust）
  duration / peak / clip / bitrate / SNR / 带宽 / RT60 七项全 PASS，最大偏差 7.6e-07
```

**e2e 脚本修过一个真 bug（2026-11-11）**：`serve` 用 `std::fs::write` 写 `runtime.json`，它先创建文件再写入，中间有一段「文件存在但内容为空」的窗口。原来的脚本只轮询「文件存在」，在 FAT32 上必被这个窗口命中，然后 `JSON.parse` 抛 `Unexpected end of JSON input`，看起来像服务没起来。改成等「内容能解析成 JSON」才算就绪。**这类"等外部进程写的文件"一律不能只判存在。**

## 快速开始

```powershell
# 编译
node scripts/cargo-run.cjs build

# 测试
node scripts/cargo-run.cjs test

# 起本地服务（开发期，不开 Tauri）
node scripts/cargo-run.cjs run -p azzer-api --bin serve

# 另开一个终端，冒烟测试 token 与运行时探测
node scripts/smoke-api.cjs

# 前端（另开终端）
cd apps/web
npm run dev
```

`cargo-run.cjs` 会把端口与 token 之外的一切原样转发；`serve` bin 会把
`{port, token}` 写到 `<工作目录>/.build/runtime.json`，Vite 代理读它并代填
`Authorization`，所以前端代码里不出现 token。

## 为什么用 Node 包装 cargo

两条本机特有的原因，换机器后可以不用这层包装：

1. **MSVC 未注册到 VS Installer**。Build Tools 装在 `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`，但 `vswhere -all` 返回空数组，导致 rustc 自动探测不到 `link.exe`，报 `linker link.exe not found`。`scripts/cargo-run.cjs` 手工拼出 `PATH` / `INCLUDE` / `LIB`，等价于 `vcvars64.bat`。
2. **PowerShell 会把 cargo 的 stderr 当异常**。cargo 把进度写 stderr，`2>&1` 时 PowerShell 抛 `NativeCommandError`，即使 `$ErrorActionPreference = Continue` 也可能中断。Node 的 `spawnSync` 用 buffer 捕获，绕开这个问题，也不受控制台编码（本机 936）影响。

`scripts/msvc-env.ps1` 是同一套逻辑的 PowerShell 版，供只跑 rustc 的场景使用。

## 目录

```
azzer/
├── Cargo.toml              workspace（6 个成员）
├── crates/
│   ├── azzer-core/         纯逻辑（无 IO、无音频依赖，可脱机单测）
│   ├── azzer-dsp/          DSP 与评估（解码/重采样/STFT/f0/修复/母带/感知）
│   ├── azzer-pipeline/     16 节点契约与实现、CAS 落盘、A 入口 CLI
│   ├── azzer-infer/        ONNX Runtime 加载与 EP 探测
│   └── azzer-api/          axum 路由 + token 鉴权 + 上传/轮询/取成品
├── apps/web/               React 前端（Vite + 自研 Canvas 波形）
├── src-tauri/              Tauri 壳，即分层里的 azzer-shell（L1 体验宿主）
├── scripts/
│   ├── cargo-run.cjs       cargo 包装（MSVC 注入 + stderr 安全捕获）
│   ├── msvc-env.ps1        手工注入 MSVC / SDK 环境变量
│   └── build.ps1           PowerShell 版包装
└── models/                 权重清单（本体不入库）
```

crate 现状：

| crate | 状态 | 职责 |
|---|---|---|
| `azzer-core` | 已交付 | 16 节点契约、幂等键、CAS、模型注册、`Engine` 协议 |
| `azzer-dsp` | 已交付 | 解码、重采样、STFT、f0、修复、母带七步、感知、37 项评估 |
| `azzer-pipeline` | 已交付 | N01/N02/N05/N09/N15/N16 实现、CAS 落盘、A 入口 CLI |
| `azzer-infer` | 骨架 | ONNX Runtime 加载与 EP 探测；真实引擎待 M2 |
| `azzer-api` | 已交付 | axum 路由、token 鉴权、上传 → 轮询 → 取成品 |
| `azzer-shell` | 代码就绪 | Tauri 壳（在 `src-tauri/`），首次编译未做 |
| `azzer-store` | 未建 | SQLite + 授权台账（M2 起） |
| `azzer-ir` | 未建 | Song IR，仅「重制演绎」档需要 |

## 本机环境（2026-11-08 实测）

| 项 | 实测值 |
|---|---|
| CPU/GPU | RTX 4060 Laptop 8188 MiB，驱动 581.42 |
| Rust | 1.98.1 stable-x86_64-pc-windows-msvc |
| MSVC | BuildTools 2022 + MSVC 14.44.35207（**未注册**，靠脚本注入） |
| Windows SDK | 10.0.26100.0（另有 22621.0） |
| WebView2 | 157.0.4341.42 |
| Node | v22.22.2 |
| Python | 3.12（`C:\Users\asus\AppData\Local\Programs\Python\Python312`）；系统默认 3.14.4 过新，不用 |
| cargo 镜像 | ustc sparse（首次拉依赖约 2 分钟，之后 0.4s） |
| 编译产物 | `<工作目录>/.build/cargo-target`（跟随工作目录，冷缓存 590 MB） |
| GPU 通路 | ORT 1.26.0 + CUDA 12.9，`CUDAExecutionProvider` 实测生效（2026-11-11 复验） |

## 构建产物体积

编译产物落在工作目录内的 `.build/cargo-target`，可用 `CARGO_TARGET_DIR` 覆盖——指到 NTFS 盘能明显提速。实测冷缓存编译 468 个依赖包后占 **590 MB**；完整 debug 构建一个中等项目通常 2–5 GB，工作目录要留余量。

F 盘 FAT32 的 4 GB 单文件上限对编译产物无影响，但**模型权重不能放这里**（YuE2 约 5 GB，需放 NTFS 盘）。

## 约定

- `azzer-core` 不允许依赖音频库、推理库或文件 IO。指标与规则一律纯函数，带单测。
- 指标函数的"区间归一化"统一用 `in_range`，禁止各处自己写公式（v2.0 的原公式在负区间会溢出，已修正）。
- 新增 crate 前先确认它属于哪个里程碑，不在当前里程碑的直接跳过。
