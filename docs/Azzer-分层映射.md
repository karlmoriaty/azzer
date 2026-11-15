# Azzer 分层映射与依赖方向

**日期**：2026-11-09
**状态**：已落地，有可执行检查（`scripts/check-layering.cjs`）

---

## 为什么要有这份文档

六层架构此前只是口头约定。v3.2 全文只有一句提到 L2/L4，代码里没有任何地方使用层编号，**也没有 crate 边界能机械地保证它**。

具体的风险在 M2：接 ONNX 时最容易图方便在 `azzer-pipeline` 里直接 `use ort`，那会把「调度层不知道推理 SDK」这条彻底破坏——而且编译照样通过，没人会发现。所以这份文档的作用不是整理，是**把约定变成可检查的规则**。

---

## 六层定义与 crate 映射

| 层 | 职责 | 对应 crate | 不许做的事 |
|---|---|---|---|
| **L1 体验** | 用户看得见的一切：界面、宿主进程 | `apps/web`（React）、`src-tauri`（`azzer-shell`） | 不直接算 DSP、不直接读写内容寻址存储 |
| **L2 编排** | HTTP 接口、任务生命周期、并发控制 | `azzer-api` | 不依赖宿主（否则无法脱离 Tauri 调试）、不做音频处理 |
| **L3 计划** | DAG 契约：节点编号、五要素、幂等键算法 | `azzer-core::dag` | 不含采样率、不含模型名、不含引擎 SDK |
| **L4 调度** | 节点执行、契约断言、执行记录缓存 | `azzer-pipeline` | **不直接持有推理 SDK**、不自己读 WAV |
| **L5 引擎** | 真正算东西的地方 | `azzer-dsp`（DSP）、`azzer-infer`（推理） | 不知道调用方是谁、不碰 HTTP |
| **L6 资产** | 内容寻址存储、模型声明 | `azzer-core::asset`、`azzer-core::model` | 不依赖任何内部 crate |

**`azzer-core` 同时承担 L3 与 L6**：它是「纯契约 + 纯资产」，被所有上层依赖，自己不依赖任何内部 crate。这个合并是刻意的——契约与资产都是「事实的定义」，放在一起不会引入方向问题。

---

## 依赖方向（只允许上依赖下）

```
L1  apps/web ──HTTP──▶ azzer-api
    src-tauri ──────▶ azzer-api
L2               azzer-api
                     │
L4            azzer-pipeline
                 │       │
L5        azzer-dsp   azzer-infer
                 │       │
L3/L6        azzer-core
```

当前实际依赖图（由检查脚本自动输出）：

```
azzer-core      ←  azzer-dsp, azzer-infer, azzer-pipeline, azzer-api
azzer-dsp       ←  azzer-pipeline, azzer-api
azzer-infer     ←  azzer-api
azzer-pipeline  ←  azzer-api
azzer-api       ←  azzer-shell
```

---

## 明确的禁止项

这些不是风格偏好，每一条都对应一个具体的破坏方式：

| 禁止 | 违反后的后果 |
|---|---|
| `azzer-core` 依赖任何内部 crate | 契约变成有环依赖的中心，全项目无法独立编译 |
| `azzer-dsp` / `azzer-infer` 依赖 `azzer-pipeline` / `azzer-api` | 引擎知道调用方，就再也换不了调用方（例如从 HTTP 改成批处理） |
| `azzer-pipeline` 依赖 `ort` | 调度层持有推理 SDK，「上云只改 URL」直接失效 |
| `azzer-pipeline` 依赖 `hound` | 编解码职责泄漏到调度层，改音频格式要动两层 |
| `azzer-api` 依赖 `tauri` | API 层无法脱离宿主单独起服务，开发期无法用浏览器联调 |
| 任何 crate 是孤岛（没人依赖） | 写了但没接上，等于没写 |

**为什么 `azzer-infer` 允许依赖 `ort`**：它本来就是 L5 引擎层的推理实现，持有 SDK 是它的职责。约束的是「除它之外谁都不许碰」。

---

## 检查方式

```powershell
node scripts/check-layering.cjs
```

脚本做三件事：

1. 读每个 crate 的 `Cargo.toml`（含 dev / build dependencies），按上表逐条比对禁止项；
2. 输出实际依赖图；
3. 反向检查每个内部 crate 是否至少被一个上层依赖，防止出现「写了但没接上」的孤岛。

退出码 0 = 通过，1 = 有违规并列出具体位置。

---

## 当前状态（2026-11-09）

```
OK  azzer-core       L3/L6 契约与资产   5 个直接依赖   无越界
OK  azzer-dsp        L5 引擎（纯 DSP）  5 个直接依赖   无越界
OK  azzer-infer      L5 引擎（推理）    1 个直接依赖   无越界
OK  azzer-pipeline   L4 调度            6 个直接依赖   无越界
OK  azzer-api        L2 编排接口       14 个直接依赖   无越界
OK  azzer-shell      L1 体验宿主        7 个直接依赖   无越界
RESULT: PASS
```

---

## 尚未机械保证的部分

脚本查的是**编译期依赖**。以下三条目前只能靠人守，写在 §5.1 与各模块注释里：

1. **L3 的计划层不含采样率与模型名** —— 没有类型系统能挡住有人在 `NodeSpec` 里塞一个 `sample_rate: u32`。
2. **L4 走 `Engine` 协议而不直接调用 `azzer-infer`** —— `azzer-pipeline` 确实没依赖 `azzer-infer`（脚本会报），但它也不依赖 `Engine` 的**实现**，因为现在只有 `NoEngine`。M2 接第一个真实引擎时这条才会真正被检验。
3. **L1 不直接拼 HTTP** —— 前端所有请求都过 `api/client.ts`，但没人挡住某个组件里直接 `fetch`。

第 1、3 条可以在 code review 时守；第 2 条等 M2 才有意义。
