# AI 音乐生成与翻唱系统 · 架构设计方案 v1.1

**状态**：待审核
**版本说明**：项目已定名 **Azzer**，总方案为 `Azzer-design-plan-v3.md`。本文保留为生成侧（创作、局部编辑、IR、Graft、调度）的详细设计参考；翻唱路线已被 v2.0 / v3 取代。读本文前先确认你要看的是"生成与编辑"还是"翻唱"。
**定位**：可控性 / 可编辑性 / 翻唱深度定制，不做一键生成质量军备竞赛
**文档性质**：只含契约、签名、数据结构、阈值与验收标准，**不含实现代码**
**核对日期**：2026-11-08

**v1.1 修订记录（2026-11-08）**

自我审查（见 `自我审查报告-能否比肩Suno-v1.md`）发现 v1.0 有六处缺陷，本版已修：

| # | 缺陷 | 修订位置 |
|---|---|---|
| D1 | 对标表过期，低估 Suno v5.5（Voices / Custom Models / Cover / Studio 段落级编辑 / 12 轨 / Remove FX / Remix 换和弦） | §1.3 重写 |
| D2 | TPA 未区分 render / replace 两条翻唱路径，对 replace 恒判失败 | §16.2 拆 TPA / SFR |
| D3 | SECS 与 EDR 被当作可同时达标，实际存在帕累托冲突 | §16.4 改三档预设 |
| D4 | 母带章节过薄，撑不起"商业发行级" | §12.4 补全母带链 |
| D5 | 无人声叠层，单轨人声天生比整轨生成薄 | §12.4 后补 12.4.1 |
| D6 | 缺与商业系统的对比基准；LoRA / 音色模型 / 声音模型三词混用 | §0.5、§16.7 |

**核心结论未变，但边界收窄**：本系统在音色归属、演绎保留、编辑粒度、数据归属四项上能赢；在整体音质、中文咬字、出歌速度三项上不能赢。**对外不得宣称"超越 Suno"**，理由与数据见 §20 与审查报告 §2。

---

## 0. 结论先行

### 0.1 命题判断

**站得住——但仅限于"差异化"这一层。** YuE2 的出现让"可控可编辑"从策略选择变成技术必然；但"效果比肩甚至超越 Suno"这个目标站不住（v1.1 追加结论，论证见 §20 与审查报告）。

三条硬证据：

1. YuE2 的原生架构就是 `符号规划 → 语义扩展 → 声学解码`（`plan() → generate_semantic() → synthesize() → decode()`），并且支持 `abc=` 直接传入乐谱、`cot=off` 关闭它自己的规划。这意味着**我们可以在不训任何模型的前提下，把规划权完全拿到自己手里**，只让它做渲染器。
2. YuE2 官方零样本翻唱评测：完整曲谱条件下 CLEWS 检索首位命中率 **71.3%**，去掉曲谱后 **0.3%**。240 倍差距说明一件事——翻唱相似度的上限由"有没有符号层"决定，不由音色模型决定。这正是差异化的立足点。
3. ACE-Step 1.5 官方就支持 repaint（局部重绘）、cover、vocal2BGM、stem separation、metadata control（BPM/调性/拍号/时长），MIT 许可，<4GB 显存。局部编辑所需的工程原语它已经给全了。

### 0.2 四处承重墙（必须在审核时先看这四条）

| # | 承重墙 | 事实 | 架构后果 |
|---|---|---|---|
| **W1** | **YuE2 权重是 CC BY-NC 4.0，非商业** | 代码 Apache-2.0，权重 CC BY-NC 4.0 + 附加创作者许可；个人可商用自己产出的歌，公司需商业授权 | 引擎必须可替换；发行版**不捆绑权重**；调度器按 `commercial` 标志拒绝非商业引擎；商业化前必须完成授权谈判或换引擎 |
| **W2** | **本机 8GB VRAM 跑不动 YuE2 官方配置** | 官方要求 24GB VRAM + 24GB RAM（BF16）；RTX 4090 实测峰值 14.08GiB，71s 出 3.6 分钟歌；Q8_0 量化权重约 4.8GB，社区 8GB 运行属"非官方支持" | 三档硬件策略（本机开发 / 24GB 生产 / 远端 worker）；本机主力引擎必须是 ACE-Step，YuE2 只做量化试验与远端调用 |
| **W3** | **符号层局部修改 ≠ 声学层局部重渲染** | YuE2 是全曲一次性 AR 规划 + flow matching，改一个音符要整曲重跑；ACE-Step repaint 只在它自己的潜空间内局部重绘，**不保证服从我们的目标音高** | 必须设计 Graft（嫁接）协议 + 引擎二次路由 + Seam QA 自动降级。这是全系统最容易做砸的地方，第 6 章专章处理 |
| **W4**（v1.1 追加） | **整体音质打不过 Suno，这是架构决定的，不是优化不够** | Suno 从零生成整轨、用模型内部 12 轨表示分离、内置母带；我们从分离干声重建，多一道不可逆损失，且人声是单轨（Suno 是多层） | 目标不能定在"整体音质超越"。可达性边界见 §20；补强路线（母带链、人声叠层、分离升级）见 §12.4.1 / §12.4.2，只能缩小差距，不能反超 |

### 0.3 一句话架构

> **LLM 只产出 Patch，Patch 只作用于 Song IR，IR 是唯一计划权威；两个生成引擎被降级为"渲染器"，渲染按小节网格分片、可缓存、可嫁接；翻唱走"转谱 → 改谱 → 渲染 → 音色转换"四段，而不是在波形上做音色替换。**

### 0.4 关键决策摘要（ADR）

| ID | 决策 | 理由（一句话） |
|---|---|---|
| ADR-01 | Song IR 为本体，ABC 为规范文本视图，MusicXML 仅作导入导出通道 | MusicXML 一个音符几十行 XML，diff 不可读；ABC 人可读写、YuE2 原生消费 |
| ADR-02 | 计划权收归 IR：引擎自带 planner 一律禁用或旁路 | 否则"可控"是幻觉——控制权在别人模型里 |
| ADR-03 | 编辑一律表达为 EditOp Patch，禁止"整曲重新生成"作为编辑手段 | 只有这样"未修改部分不变"才能被断言验证 |
| ADR-04 | 渲染粒度为 RenderUnit（小节对齐），不是整曲 | 局部编辑延迟从"全曲重跑"降到"片段重绘" |
| ADR-05 | 引擎进程隔离（独立 venv + sidecar），不共进程 | ACE-Step 要 Python 3.11、YuE2 要 3.12，依赖必然冲突 |
| ADR-06 | 缓存内容寻址 + DAG 失效，key 含引擎版本与 adapter 哈希 | 引擎升级后静默复用旧音频是最隐蔽的正确性事故 |
| ADR-07 | 人声轨拼接与伴奏轨拼接走两套算法 | 伴奏交叉淡化听不出来，人声交叉淡化会变成二重唱 |
| ADR-08 | 所有训练数据过 RightsRecord 门禁，仓库内不出现任何抓取脚本 | 合规是产品前提，不是上线前补丁 |

### 0.5 三个容易混淆的"模型"（必须先分清）

用户口中的"放入我的声音模型然后训练"，实际可能指三件不同的事，数据要求相反，产出也不同。混用会导致"我放了 8 首歌，为什么唱出来不像我"。

| 说法 | 系统内实体 | 需要的数据 | 学到什么 | 不能做什么 |
|---|---|---|---|---|
| "用我的音色唱" | `voice_model`（RVC `.pth` + `.index`） | **10–20 分钟干声**（无伴奏、无混响、无和声） | 音色、共振峰、发声习惯 | 不继承唱功；源唱得差，转换后仍是差唱功 |
| "写出来像我的风格" | `lora_style`（ACE-Step LoRA） | **8–10 首完整成品** | 配器、和声偏好、编曲习惯、制作美学 | 不学音色；不能让人听出"是你" |
| "让 AI 唱得像某位歌手" | `lora_voice` / 商用克隆 | 需该歌手的**明示授权**（RightsRecord `scope` 含 `train`） | 音色 + 部分风格 | 无授权一律拒绝，不做"技术上可行"的例外 |

**验收含义**：`voice_model` 与 `lora_style` 是两条独立流水线，评估指标不同（前者看 SECS/EDR，后者看 FAD/风格相似度），不能合并验收。原 v1.0 把两者并称"LoRA 微调"，是概念混淆，本节更正。

---

## 1. 定位与边界

### 1.1 差异化命题拆成三条可验证假设

| 假设 | 验证方式 | 失败判据 |
|---|---|---|
| H1：用户愿意为"改一个音符"付费，而不是重抽一次 | EditAcceptRate ≥ 60%，RoundsToAccept 中位数 ≤ 4 | 用户 80% 的操作仍是 reroll |
| H2：符号层编辑能被听出来且不被听出接缝 | TPA ≥ 0.90 且 UPS_wave = 1.0（Graft 路径硬断言） | 接缝被盲听识别率 > 30% |
| H3：走符号层的翻唱比纯音色替换更像"翻唱"而非"换声" | CLEWS 首位命中率 ≥ 50%，SECS ≥ 0.82 | 命中率 < 20%（说明符号层没起作用） |

### 1.2 不做清单

- 不做"一句话出爆款"的端到端比拼。生成质量只要求"不低于开源第一梯队"，不追求超越商业闭源。
- 不做实时演唱（real-time singing）。延迟目标最低档是 8s 级，不是 200ms 级。
- 不做通用 DAW。编辑能力收敛到"旋律/和弦/结构/段落"四类，音频波形的自由剪辑只做裁剪与淡化。
- 不做声音克隆的"任意人声"能力。只做**已授权音色库**内的转换。
- 不在本机训练 YuE2。官方未提供训练脚本，不自研（见 15.3）。

### 1.3 对标维度（v1.1 重写，核对日期 2026-11-08）

**先纠正 v1.0 的错误**：原表把商业产品写成"多为重抽或段级重绘""和弦重配不支持"，是基于过期信息的判断。按 2026-11 的实际能力，Suno v5.5 已具备下列功能，其中至少四项覆盖了我们原本想差异化的点。

| 维度 | Suno v5.5（2026-11 核对） | 本系统 | 判定 |
|---|---|---|---|
| 音色克隆 | Voices：录音 + 随机短语验证，仅本人可用，18+，部分地区不可用 | RVC 本地训练，音色归用户所有，无地域限制 | **胜**（胜在归属与可移植，不在相似度） |
| 风格微调 | Custom Models：≥6 首自供曲目，私有微调 | ACE-Step LoRA：8–10 首，本地训练 | **打平**（我们胜在可离线、可审计） |
| 上传音频翻唱 | Cover：≤8 分钟，保旋律改风格，可与 Persona 组合 | 转谱 → 改谱 → 渲染 / 或 vocal2BGM + RVC | **打平**（见下一行差异） |
| 翻唱是否保留原演绎 | Cover 是**重新演绎**，phrasing 会变 | replace 模式保留原唱全部 F0 / timing / 气口 | **胜**（结构性优势） |
| 局部编辑粒度 | Studio 段落级重生成（verse/chorus 整段），保留其余 | **音符级** patch + 片段重渲染 | **胜**（差距窄于 v1.0 的论证） |
| 改完保留其余部分 | 段落级保证，无样本级断言 | `UPS_wave = 1.0` 硬断言（graft 路径） | **胜**（可验证性） |
| 和弦重配 | Remix 模式：换和弦进行，保持人声 phrasing 对齐 | `reharmonize` 算子，旋律轨不动 | **打平** |
| 分轨 | 12 轨，**用模型内部多轨表示**，对自家生成轨比 Demucs 干净 | Demucs / MDX23C 信号级分离，对第三方成品有损失 | **负** |
| 去混响 | Remove FX（剥离人声混响/延迟尾巴） | UVR DeReverb / WPE + 双路 F0 | **打平** |
| 并排试听 | Alternates 面板 | A/B 双轨 + 差异高亮 | **打平** |
| 整体音质 / 母带 | 整轨生成 + 内置母带 | 分离干声重建 + 自建母带链（§12.4） | **负** |
| 中文咬字 | v5 主流语种强 | ACE-Step 官方自认 vocal 粗糙；YuE2 中文无公开证据 | **负（待实测）** |
| 出歌速度 | <1 分钟 2 首 | 端到端 5–20 分钟 | **负（数量级）** |
| 离线 / 数据不出本机 | 云端，不可离线 | 全本地 | **胜** |

**结论：5 胜 5 平 4 负。** 输的四项里，整体音质与出歌速度是架构决定的（无法优化到打平），分轨是对手主场（内部多轨表示），中文咬字待实测。

> 口径说明：本节按 14 个细分维度计数；§20.1 把它们归并成 7 个用户可感知维度，得出 4 胜 3 负。两处数字不同是口径差异，不是矛盾。

**因此差异化只能落在这四句上**，其余表述一律删掉：

1. 改到**音符**，不是改到段落。
2. 翻唱保留**原唱这一版演绎**，不是重新唱一遍。
3. 模型与音频**归你**，可离线、可迁移、无地域限制。
4. "未修改部分不变"是**可断言的**，不是宣传语。

**禁止表述**（会在大规模盲测里被打脸）：

- "效果超越 Suno"——2025 年末盲测中 Suno v5 片段被误判为真人的比例已达 60–70%，音质不是可竞争维度。
- "达到商业发行水准"——分离损失与单轨人声是硬上限。
- 引用 YuE2 的 best-of-8 分数（6.96）当作单次能力——单次是 6.73。

---

## 2. 引擎现状尽调（方案的事实基础）

> 数据来自公开资料核对（2026-11-08），实际接入前必须用 `probe()` 重新采集并写入 manifest，不得硬编码。

| 引擎 | 能力 | 输入 | 输出 | 延迟参考 | 显存 | 许可证 | 维护状态 | 本系统角色 |
|---|---|---|---|---|---|---|---|---|
| **YuE2-3B** | text2song、score2song（`abc=`）、零样本 cover、agentic edit | 歌词 + 风格 prompt；或 ABC 谱；或 SheetSage2 转出的谱 | 48kHz 立体声整曲（人声+伴奏同轨） | 4090：3.6 分钟歌 ≈ 71s | 官方 24GB BF16；Q8_0 ≈4.8GB | 代码 Apache-2.0 / **权重 CC BY-NC 4.0** | 活跃（v0.1.6，2026-09-09） | **整曲高质量渲染**（渲染器模式） |
| **SheetSage2** | 音频 → 可编辑主旋律谱（无和弦） | 干声或整曲 | ABC/MIDI 旋律 | 数十秒/首 | 低 | 随 YuE2 发布 | 活跃 | **翻唱入口：转谱** |
| **MERT2** | 全曲级音乐表征 | 音频 | embedding（632M×2 编码器） | 快 | 中 | 同上 | 活跃 | **评估指标 UPS_emb / 相似度** |
| **ACE-Step 1.5** | text2music、cover、repaint、extract、lego、complete、vocal2BGM、multi-track、metadata control、LRC、质量评分、LoRA | 文本 + 歌词 + 参考音频 + 区间 | 48kHz | A100 <2s/首；3090 <10s/首 | <4GB | **MIT** | 活跃 | **快速草稿 / 局部重绘 / 伴奏生成 / LoRA 宿主** |
| **RVC** | 歌声/语音音色转换（top-1 retrieval 抑制源泄漏） | 源干声 + `.pth` + `.index` | 转换后干声 | 实时级 | <8GB（小模型） | **MIT** | 活跃（2026-07 仍有版本） | **高保真翻唱主路径** |
| **So-VITS-SVC** | 歌声转换 | 源干声 + 模型 | 干声 | 中 | 中 | **AGPL-3.0** | **2023-11 已归档** | **不列入主路径**（AGPL 传染 + 停维护）；仅作可选插件 |
| **Demucs v4** | 4 轨分离 | 混音 | vocals/drums/bass/other | 中等 | ~2GB | MIT | 稳定 | **分离主路径** |
| **MDX23C / RoFormer 系** | 高质量分离 | 混音 | 2 或 4 轨 | 慢 | 中 | 各异 | 活跃 | 备选（质量优先时） |

### 2.1 尽调得出的三个工程结论

**结论一：So-VITS-SVC 出局。** AGPL-3.0 且 2023-11 归档。把它放进主链路等于给产品埋一个传染性许可证 + 一个永不再修的依赖。RVC 是 MIT 且仍在更新，音色泄漏控制（top-1 retrieval）也更好。用户要求的"RVC 或 So-VITS-SVC"，技术上应选 RVC，So-VITS-SVC 保留为插件位。

**结论二：ACE-Step 自己也有 LLM planner（Qwen3）。** 这与"我们的 LLM 规划器"职责直接重叠。必须在设计上明确：**ACE-Step 的 planner 只允许用于它自己的 caption 润色，不得参与结构决策**。具体做法是走它的 metadata control（BPM/key/拍号/时长）+ 显式段落标签，禁用它自动扩写结构。

**结论三：YuE2 的 `abc=` 入参是整个方案的技术支点。** 它让我们能把一个 3B 大模型降级为"昂贵的声码器"。没有这个参数，"符号层可编辑"就只能做到"改完谱再让它自由发挥"，可控性会漏掉一半。

---

## 3. 系统总览

### 3.1 分层架构

```
┌──────────────────────────────────────────────────────────────────────┐
│ L5  体验层    React + TS：乐谱编辑 / 钢琴卷帘 / A-B 双轨 / 差异高亮      │
├──────────────────────────────────────────────────────────────────────┤
│ L4  编排层    FastAPI：会话、Patch 校验、影响分析、任务编排、SSE 推送      │
├──────────────────────────────────────────────────────────────────────┤
│ L3  计划层    Song IR（ScoreGraph + Performance + RenderPlan）           │
│               LLM Planner（产出 Patch，不产出音频，不产出整曲）           │
├──────────────────────────────────────────────────────────────────────┤
│ L2  调度层    队列 / 显存仲裁 / 缓存(DAG) / 引擎路由 / Graft / Seam QA    │
├──────────────────────────────────────────────────────────────────────┤
│ L1  引擎层    [YuE2] [ACE-Step] [SheetSage2] [RVC] [Demucs] [DSP 链]     │
│               每个引擎 = 独立 venv 的 sidecar 进程 + Capability Manifest  │
├──────────────────────────────────────────────────────────────────────┤
│ L0  资产层    SQLite(元数据/Revision DAG/Rights) + 内容寻址音频库         │
└──────────────────────────────────────────────────────────────────────┘
```

**层间硬约束**（违反即架构腐化）：

- L4 不得直接 import 任何引擎 SDK。只通过 `Engine` 协议。
- L3 不得包含任何采样率、显存、模型名。IR 是纯音乐语义。
- L2 是唯一知道"哪个引擎在跑、还剩多少显存"的层。
- L1 不得互相调用。引擎之间的一切协作由 L2 编排（例如"先分离再转换"）。
- L0 的音频只按内容哈希寻址，不允许业务代码拼路径。

### 3.2 唯一计划权威原则

这是全系统的宪法条款：

> **任何引擎的内部 planner 输出，都必须先被解析成 Song IR 的一个候选 revision，经校验后才能进入渲染。引擎不得直接决定段落结构、调性、BPM、和弦进行。**

落地手段：

| 引擎 | 剥夺规划权的方式 |
|---|---|
| YuE2 | 传 `abc=` 自有谱 + `cot=off`（或 `cot=melody` 仅让它补伴奏）；只在"从零起草"时允许 `cot=full`，且结果必须导入 IR 供用户审阅 |
| ACE-Step | 显式传 BPM/key/拍号/时长/段落标签；禁用 query rewriting 与 simple mode；repaint 时传固定上下文 |

### 3.3 引擎能力矩阵（调度器据此路由）

| 能力 | YuE2 | ACE-Step | RVC | Demucs | DSP |
|---|---|---|---|---|---|
| 从零出整曲 | ✅ | ✅（快） | — | — | — |
| 按给定谱渲染 | ✅（`abc=`） | ⚠️ 部分（metadata + 歌词标签，不读谱） | — | — | — |
| 局部重绘（跟随目标音高） | ❌ | ⚠️ 不保证 | — | — | — |
| 音色转换 | — | ⚠️ cover 模式（风格级） | ✅（音色级） | — | — |
| 分离 | — | ✅（extract） | — | ✅ | — |
| 人声转伴奏 | — | ✅（vocal2BGM） | — | ✅ | — |
| 转谱 | SheetSage2 | ⚠️ 音频理解（BPM/调性，非旋律谱） | — | — | — |
| LoRA | ❌ 官方无 | ✅ | ✅（per-voice 微调） | — | — |
| 确定性（同 seed 同结果） | ❌ | ❌ | ⚠️ | ✅ | ✅ |

> ⚠️ 标记的格子表示"能力存在但不满足我们的精度要求"，必须由 Seam QA / TPA 检测兜底，不达标自动升级到上一档引擎。

---

## 4. 符号中间表示（Song IR）

### 4.1 表示选型：为什么是"三层 + 双通道"

| 候选 | 优点 | 缺点 | 判决 |
|---|---|---|---|
| 纯 ABC | 人可读写、Git diff 友好、YuE2 原生消费、体积极小 | 表现力弱（力度/演奏法/混音参数无位置）、无稳定 ID、并发编辑冲突 | **采用为 L0 规范文本** |
| 纯 MusicXML | 生态最广（MuseScore/DAW）、表现力强 | 一个音符几十行、diff 不可读、往返有损、解析慢 | **仅作导入导出通道** |
| MIDI | 通用、可听 | 无段落语义、无和弦语义、文本不可读、时间精度靠 tick 易漂 | 仅作试听中间产物 |
| 自研 JSON | 完全可控 | 用户无法用文本编辑器改、生态为零 | **采用为 L1（内部结构），但必须能从 L0 无损往返** |

**结论**：三层表示，各司其职。

```
L0  ScoreDoc      ABC 文本（YuE2 子集 + 扩展注释）   ← 人类编辑 / Git / 交付引擎
      ↕ parse / serialize（必须可往返，往返测试进 CI）
L1  ScoreGraph    结构化图（Note/Chord/Section/StableID）  ← Patch 作用对象 / 校验 / 分析
      ↕ compile
L2  RenderPlan    渲染指令（分片/重叠区/引擎参数/seed/graft 边界）  ← 调度器输入
```

### 4.2 符号层与表现层分离（关键设计）

ABC 表达不了力度曲线、颤音深度、气声比例、混响发送量。这些东西如果硬塞进 ABC 的装饰符，会毁掉可读性；如果塞进音频，就毁掉可编辑性。

**方案：双层存储，同 revision 绑定。**

| 层 | 内容 | 载体 | 谁改 |
|---|---|---|---|
| 符号层 `ScoreGraph` | 音高、时值、小节、段落、和弦、歌词音节、力度记号（pp/mf/f 级） | ABC ↔ JSON | 用户（乐谱编辑器）/ LLM（patch） |
| 表现层 `Performance` | 每音符 velocity 曲线、vibrato depth/rate、气声比、articulation、人声技法标签、每段混音预设 | JSON（按 note ID 索引） | 用户（曲线编辑器）/ 引擎回填（从源演唱提取） |

**为什么重要**：第 16 章的"情感细节保留度"必须在表现层有落点。翻唱时，从源演唱提取的 F0 微轮廓、能量包络、颤音特征写入 `Performance`，渲染时作为条件传入——这才是"保留情感细节"的可实现路径，而不是指望音色模型自己悟。

### 4.3 稳定 ID 与定位

**MVP 方案（确定性哈希 + 序列对齐）**

```
NoteID = "n:" + sha1(f"{section_id}|{track}|{bar}|{onset_ticks}|{pitch}")[:12]
```

- 编辑后重新解析，用 LCS/序列对齐（对每小节的 note 序列做最长公共子序列）把旧 ID 迁移到新节点，迁移失败的标为 `orphan`。
- 优点：零额外存储，ABC 保持干净。
- 缺点：同一位置改音高会换 ID（视为"删旧增新"，符合语义）；复杂重排时迁移率下降。

**V2 方案（ID 内嵌 + 3-way merge）**

- ABC 中用注释承载 ID：`"^nid=a1b2c3"` 或行尾 `% nid=a1b2c3`（选择取决于 abcjs 的注释保留行为，需实测）。
- 文本编辑模式独占：进入文本编辑时锁定结构化编辑，提交时以"上次生成的 ABC"为 base 做 3-way merge。

**审核建议**：MVP 走哈希方案，`id_migration_rate` 作为监控指标（低于 90% 时触发 V2）。不要一开始就上 CRDT，那是给多人实时协作准备的，当前场景（单用户 + LLM）用不上。

### 4.4 核心数据结构

#### SongIR（顶层）

| 字段 | 类型 | 说明 | 约束 |
|---|---|---|---|
| `song_id` | string(ulid) | 歌曲 ID | 不可变 |
| `revision` | int | 单调递增 | 每次 patch 应用 +1 |
| `parent_revision` | int \| null | 分支原点 | 用于 revision DAG |
| `score_doc` | string | L0 ABC 文本 | 必须通过 parse |
| `score_graph` | ScoreGraph | L1 派生结构 | 由 score_doc 解析，缓存 |
| `performance` | Performance | 表现层 | 按 NoteID 索引 |
| `meta` | SongMeta | 标题/语言/时长目标/风格标签 | — |
| `provenance` | Provenance | 谁改的（user/llm/heuristic）+ 模型版本 + prompt hash | 每次 patch 写入 |
| `created_at` | int(ms) | — | — |

#### ScoreGraph

```jsonc
{
  "tempo_map": [{"bar": 1, "bpm": 96}, {"bar": 17, "bpm": 104}],
  "key": {"tonic": "A", "mode": "minor", "confidence": 0.92},
  "time_signature": [{"bar": 1, "num": 4, "den": 4}],
  "sections": [
    {
      "id": "S2",
      "label": "chorus",
      "start_bar": 9,
      "end_bar": 16,
      "energy": 0.82,
      "tracks": ["vocal_lead", "harmony", "inst"]
    }
  ],
  "tracks": [
    {
      "id": "vocal_lead",
      "role": "vocal",
      "instrument": "voice",
      "notes": [
        {
          "id": "n:9f3c1a2b4d5e",
          "bar": 9, "onset_ticks": 0, "dur_ticks": 480,
          "pitch": 69, "pitch_cents": 0,
          "lyric": "夜", "slur": false, "tie": false,
          "source": "planned"        // planned | transcribed | imported | user
        }
      ]
    }
  ],
  "chords": [
    {"id": "c:77aa12", "bar": 9, "beat": 1, "symbol": "Am7", "root": 57, "quality": "min7", "inversion": 0}
  ],
  "lyrics": [{"section": "S2", "text": "夜色在窗外", "lang": "zh"}],
  "form": ["intro", "verse", "chorus", "verse", "chorus", "bridge", "chorus", "outro"]
}
```

`ticks` 定义：**PPQ = 480**，全曲绝对 tick（小节起点由 tempo_map + time_signature 推导），避免"局部 tick 基准"导致的跨段错位。

#### Performance

```jsonc
{
  "per_note": {
    "n:9f3c1a2b4d5e": {
      "velocity": 0.72,
      "vibrato": {"depth_cents": 38, "rate_hz": 5.4, "onset_ratio": 0.35},
      "breathiness": 0.18,
      "articulation": "legato",
      "f0_curve": "f0/{note_id}.npy"      // 可选，仅翻唱链路有
    }
  },
  "per_section": {
    "S2": {"mix_preset": "chorus_wide", "reverb_send": 0.22, "double_track": true}
  }
}
```

#### RenderPlan / RenderUnit

```jsonc
{
  "plan_id": "rp:...",
  "song_id": "...", "revision": 7,
  "global": {"engine_hint": "yue2", "seed": 12345, "target_sr": 48000, "stems": ["vocal", "instrumental"]},
  "units": [
    {
      "unit_id": "u:S2:bars9-16",
      "section": "S2", "bar_range": [9, 16],
      "time_range_s": [16.0, 30.0],
      "score_slice_hash": "sha256:...",
      "status": "dirty",                  // clean | dirty | rendering | failed
      "engine": null,                     // 运行时由路由决定
      "overlap": {"pre_s": 0.25, "post_s": 0.25},
      "graft": {"mode": "crossfade_equal_power", "anchor": "bar"}
    }
  ]
}
```

### 4.5 IR 校验规则（可执行清单，进 CI）

| 规则 | 检查 | 失败处理 |
|---|---|---|
| V1 | ABC 能被 `abc2midi` 无错编译（错误带行号） | 拒绝 patch，回传行号给 LLM 重试 |
| V2 | 解析后小节数 = 声明小节数，无"小节过载" | 拒绝 |
| V3 | 每音符 pitch ∈ [21, 108]（钢琴范围）；人声轨额外限制在配置音域内（默认 C3–C6） | 拒绝或自动八度折叠并告警 |
| V4 | 拍号/小节时长一致：`sum(dur_ticks) == bar_len * n` | 拒绝 |
| V5 | 和弦符号能解析（music21 `chordSymbolFigureFromChord` 反向验证） | 降级为"仅根音"并告警 |
| V6 | 歌词音节数 = 人声轨 note 数（按段对齐，允许 1 对多 melisma 显式标注） | 告警不阻断（melisma 合法） |
| V7 | 段落不重叠、覆盖全曲、无空洞 | 拒绝 |
| V8 | 调性一致：非转调段落内，和弦外音比例 ≤ 30% | 告警 |

V1 是硬闸门：**ABC 语法错误绝不进入渲染**。LLM 生成的谱有 10–20% 概率语法坏，必须重试而非容错解析。

---

## 5. 编辑模型：Patch 而非重写

### 5.1 EditOp 算子集（封闭集合，不可扩展给 LLM）

| op | 目标 | 参数 | 是否影响声学 | 典型延迟预算 |
|---|---|---|---|---|
| `set_note_pitch` | NoteID | `pitch`, `cents?` | 是（该小节） | 8s |
| `set_note_duration` | NoteID | `dur_ticks` | 是（本节） | 12s |
| `insert_note` / `delete_note` | 位置 | Note 对象 | 是（本节） | 12s |
| `transpose_section` | SectionID | `semitones` | 是（本节 + 人声重唱） | 20s |
| `set_chord` | ChordID | `symbol` | 是（本节） | 10s |
| `reharmonize` | SectionID | `style`, `density` | 是（本节） | 15s |
| `set_tempo` | bar range | `bpm` | **是（全曲）** | 90s |
| `set_section_boundary` | SectionID | `start_bar`, `end_bar` | **是（全曲）** | 90s |
| `duplicate_section` | SectionID | `insert_after` | **是（其后全部）** | 90s |
| `set_lyric_syllable` | NoteID | `text` | 是（该音符人声） | 15s |
| `set_mix_param` | SectionID | `param`, `value` | 否（仅重混） | 2s |
| `set_performance` | NoteID | `velocity`/`vibrato`/… | 否（重合成或重混） | 3s |

**封闭集合的意义**：LLM 只能从这 12 个算子里选，无法"偷偷重写整首"。这是"未修改部分不变"的第一道保证。

### 5.2 Patch 应用语义

```
apply(ScoreGraph, Patch) -> Result<ScoreGraph', Conflict[]>
```

纯函数，无副作用，无 I/O。要求：

1. **原子性**：一个 Patch 内的多个 op 要么全成功，要么全回滚。不做部分应用。
2. **冲突定义**：两个 op 操作同一 NoteID 的同一字段且值不同 → `CONFLICT`；op 目标 ID 不存在 → `TARGET_MISSING`；op 结果违反 V3/V4/V7 → `INVALID_RESULT`。
3. **幂等**：同一 Patch 重复应用结果相同（对 `set_*` 类成立；`insert_note` 需带客户端生成的 `op_id` 去重）。
4. **可逆**：每个 op 保存 `inverse`（旧值快照），支持 undo 到任意 revision。

### 5.3 LLM 规划器的输出契约

**输入**：`{user_prompt, current_score_summary, selection?, constraints}`

**输出**（严格 JSON，JSON Schema 校验，失败重试 ≤ 2 次）：

```jsonc
{
  "intent": "change_chord_progression",
  "patch": [
    {"op": "set_chord", "target": "c:77aa12", "symbol": "Fmaj7"},
    {"op": "set_chord", "target": "c:88bb34", "symbol": "G7"}
  ],
  "explanation": "把副歌前半的和声从 Am–F 改为 F–G，制造上行动力",
  "affected_estimate": ["S2"],
  "confidence": 0.78
}
```

**约束**：

- 输出必须是 patch，不能是 ABC 全文。若 LLM 返回整曲，视为非法输出，拒绝并要求重试。
- `explanation` 是必填项，直接显示给用户（可控性体验的一部分：用户要知道改了什么、为什么）。
- `confidence < 0.5` 时不自动应用，转为"建议卡片"让用户点选。
- 规划器不接触音频，不接触引擎，只接触 IR 摘要（压缩后的 section 级统计 + 选中区间原文）。**上下文只给选中区间 ± 1 小节**，避免它"顺手"改别处。

### 5.4 影响分析与 AffectedSpan

```
AffectedSpan = expand(direct_span(op), rules) ∩ song_range
```

| op 类 | direct_span | 扩张规则 |
|---|---|---|
| 音符级（pitch/dur/insert/delete/lyric） | 该音符所在小节 | 向前后各 1 小节（乐句边界），封顶到 section 边界 |
| 和弦级（set_chord/reharmonize） | 该和弦起至下一和弦 | 扩到和声进行边界（同 section 内连续和弦组） |
| 段落级（transpose） | 整个 section | 前后各 0（段落边界即乐句边界） |
| 全局（tempo/boundary/duplicate） | 全曲 | 不扩张，直接全曲 dirty |

**关键约定**：`AffectedSpan` 必须对齐到 `RenderUnit` 网格。若不对齐，向上取整到整 unit。这保证"缓存命中"是二值的（unit 要么干净要么脏），不需要做样本级局部拼接。

### 5.5 Revision DAG

```
rev0 (LLM 草案) ──rev1 (改副歌和弦) ──rev2 (改主歌旋律)
                        └──rev2b (从 rev1 分叉：另一版副歌)
```

- 每次 patch → 新 revision，不改旧 revision（不可变）。
- 渲染产物绑定 `(revision, unit_id)`，跨 revision 共享（内容寻址去重）。
- UI 默认展示线性历史，"对比"功能允许任意两个 revision 做 A/B。
- 删除 revision 只删引用，产物按 LRU 回收。

---

## 6. 渲染与嫁接（本系统技术核心）

> 这一章决定产品成立与否。核心矛盾：**用户要"改一个音符 3 秒听到"，而高质量引擎是"整曲 71 秒重跑"。**

### 6.1 分区渲染网格

- 网格粒度：**小节**。`RenderUnit` = 连续小节区间，默认按 section 切分，section 内超过 16 小节再均分。
- 每个 unit 独立渲染，边界带 `overlap`（默认 250ms 前后）。
- 为什么按小节而不是按秒：patch 的影响天然按小节表达，缓存命中率最高；按秒切会产生大量跨 unit 的 patch，命中率崩。

### 6.2 引擎路由决策树

```
render(unit, span)
  │
  ├─ span 覆盖全曲（>60% 时长）？──是──► YuE2 全曲渲染（batch 队列，90s）
  │                                    └─ YuE2 不可用 → ACE-Step 全曲（fast 队列，30s，质量降级）
  │
  └─ 否（局部）
       ├─ 变更只涉及 mix/performance？──是──► DSP 重混（2-3s，不重跑模型）
       │
       ├─ 变更涉及人声旋律？──是──► ① ACE-Step repaint 试渲染
       │                            ② 跑 TPA 检测
       │                              ├─ TPA ≥ 0.90 → 采用（8-15s）
       │                              └─ TPA < 0.90 → 升级 YuE2 全曲（90s）+ 人声轨 graft
       │
       └─ 变更只涉及伴奏/和弦/结构？──是──► ACE-Step repaint（8-12s）
                                            └─ Seam QA 不过 → YuE2 全曲
```

**这个决策树是"延迟"与"保真"的显式权衡表**，不是实现细节。产品上要把它暴露给用户：编辑面板显示"本次编辑预计 8s / 高保真需 90s"两个按钮。

### 6.3 Graft（嫁接）协议

新渲染片段与缓存旧片段的拼接，分轨两套算法。

#### 6.3.1 伴奏/器乐轨：等功率交叉淡化

| 步骤 | 操作 | 参数 |
|---|---|---|
| 1 | 样本级对齐到小节边界 | 48kHz，`round(bar_time * 48000)` |
| 2 | 响度预归一 | 新片段与旧片段的 `integrated_loudness` 先各自对齐到全曲目标（±0.3 LU 内），**避免拼接点出现响度台阶** |
| 3 | 等功率交叉淡化 | 重叠 250ms，`w_in = sin(π/2·t)`, `w_out = cos(π/2·t)` |
| 4 | 相位检查 | 重叠区内两信号互相关峰值延迟 \|τ\| > 5ms → 视为相位失配，改为"硬切 + 20ms 淡化"（宁可听不出接缝也不要有梳状滤波） |
| 5 | 混响尾巴 | 旧片段在切点后延长 200ms 淡出（保留原混响尾），新片段从切点前 50ms 淡入 |
| 6 | 总线处理 | 全曲重跑 limiter（不要在片段上做 limiter，会破坏整体动态） |

#### 6.3.2 人声轨：不许交叉淡化

**原因**：人声是单音源 + 强 vibrato + 中文连续元音，交叉淡化会产生"二重唱"、"相位鬼影"、元音形变。盲听极易识别。

**替代方案（按优先级）**：

| 方案 | 做法 | 适用 |
|---|---|---|
| A. 乐句边界硬切 | 只在**换气点**（能量谷 + 静音段）切，切点前后各 15ms 淡化 | 首选。需先做静音检测（`librosa.effects.split`，阈值 -35dB） |
| B. 整段重唱 | 让引擎重渲染**整个 section 的人声**，不做段内拼接 | 次选。成本高但绝对干净 |
| C. 音素对齐拼接 | 用 MFA/WhisperX 拿到音素边界，在辅音闭塞段（closure）切 | 高成本，只在 A/B 都不可用时用 |

**产品含义**：**人声旋律的局部编辑，优先作用于"整段"而不是"整音符"**。UI 上要明确：改人声一个音符 → 默认重唱整个乐句（4–8 小节），并标注"该段人声已重唱"。这是诚实的交互，而不是假装无缝。

#### 6.3.3 硬断言

```
assert UPS_wave(unit_not_in_AffectedSpan) == 1.0
```

对未受影响区间的音频，与上一 revision 逐样本比对，必须**完全一致**（bit-exact，容差 0）。若不等，说明缓存/拼接逻辑有 bug，直接失败并告警，不允许"差不多"。

这一条把"保留未修改部分"从宣传语变成了 CI 里的一条断言。

### 6.4 Seam QA 自动检测

拼接后立即跑，全自动，超阈值降级：

| 检测项 | 方法 | 阈值 | 超标动作 |
|---|---|---|---|
| 响度台阶 | 切点前后 200ms 的短时 LUFS 差 | > 1.5 LU | 重做响度预归一 |
| 谱通量突变 | 切点 ±50ms 的 spectral flux 与全曲 P95 比较 | > 2× P95 | 降级为全曲重渲染 |
| 立体声相关性骤降 | 切点前后 100ms 的 L/R 相关系数 | 差 > 0.2 | 检查相位，转硬切 |
| 人声连续性 | 切点处 F0 跳变 | > 100 cents | 降级为整段重唱 |
| 静音段污染 | 切点是否落在静音区 | 否 | 记录 `seam_risk=high`，UI 提示 |

`seam_risk` 写进 RenderResult，累积到评估指标（第 16 章 EDR/接缝盲听）。

### 6.5 必须全曲重渲染的情形（不要试图优化）

1. tempo 变化（整曲时间轴变了，所有 unit 缓存失效）
2. 调性变化（全局音高，人声全部要重唱）
3. 段落增删/重排（时间轴变了）
4. 曲长变化（outro 延长等）
5. 引擎或 adapter 版本升级（缓存 key 变了，全 dirty）
6. Seam QA 连续 2 次降级

前 4 项应在 UI 上明确提示"此操作将重新渲染全曲（约 90s）"，让用户预期正确。

---

## 7. 数据流

### 7.1 创作链路（自然语言 → 可编辑作品）

```
用户 prompt
  → [L4] 意图解析（风格/情绪/语言/时长/参考曲）
  → [L3] LLM Planner: 产出 SongPlan（结构骨架 + 调性 + BPM，不含音符）
  → [L3] 符号起草：二选一
        (a) LLM 直接产 ABC 草稿 → 校验 → ScoreGraph
        (b) 调 YuE2 plan(cot=full) 产 ABC → 导入 IR 供审阅（保留 provenance=engine）
  → [L4] 返回 IR + 文本预览（用户此时可改谱，未花渲染成本）
  → 用户确认 → [L2] 全曲渲染（YuE2，90s）
  → [L2] 分离 stem（Demucs）→ [L0] 入库
  → [L5] 播放器 + 乐谱联动
```

**关键点**：符号草稿与音频渲染是两次独立确认。用户可以先在谱上改 10 分钟，一分钱渲染不花。这是"可控"的体验基础。

### 7.2 局部编辑链路（核心差异化）

```
用户选中音符 → 改音高（或自然语言"这句再高一点"）
  → [L3] 生成 EditOp
  → [L4] Patch 校验（V1–V8）→ 冲突检查
  → [L3] AffectedSpan 计算
  → [L4] 返回预估：{受影响小节, 预计耗时, 引擎档位}
  → 用户确认 → [L2] 路由（6.2 决策树）
  → [L1] 局部渲染（ACE-Step repaint，8s）
  → [L2] TPA 检测 → 不过则升级 YuE2
  → [L2] Graft 拼接 → Seam QA
  → [L0] 新 revision 入库
  → [L5] A/B 播放（旧 revision vs 新 revision，同步游标）
```

**延迟预算（本机 ACE-Step 路径）**：patch 提交→首声 ≤ 8s（P50），≤ 20s（P95）。

### 7.3 翻唱链路（转谱 → 改谱 → 渲染 → 转音色）

```
源音频（用户上传，需 RightsRecord）
  → [L1] Demucs 分离 → vocals.wav
  → [L1] 去混响 + 降噪 + 响度归一
  → [L1] SheetSage2 转谱 → 旋律 ABC（无和弦）
  → [L3] 导入 IR，补和弦（LLM reharmonize 或用户手填）
  → 用户改谱（改调、改段落、改旋律、保留/替换歌词）
  → [L2] YuE2 渲染（abc= 自有谱，cot=melody）→ 新人声 + 新伴奏
    或
  → [L2] ACE-Step vocal2BGM（保留原人声，只换伴奏）
  → [L1] RVC 音色转换：源人声 → 目标音色（逐段 pitch shift）
  → [L1] 音高修正（目标 = IR 音高，不是"修到最近半音"）
  → [L1] 混音 → 响度 → 导出
  → [L0] 产出 + 评估报告（render 路线：SECS/EDR/TPA；replace 路线：SECS/EDR/SFR）
```

**两条翻唱路线要并存**：

| 路线 | 何时用 | 音色来源 | 相似度 |
|---|---|---|---|
| A. 渲染式（YuE2 + RVC） | 要改编曲/改结构/改调 | 目标音色模型 | 中高 |
| B. 替换式（vocal2BGM + RVC） | 只换伴奏、保留原唱演绎 | 目标音色模型 | 最高（原唱的情感细节全在） |

路线 B 是"懂行"的选择：保留原唱的全部 timing/F0/情感细节，只换音色和伴奏。这是"情感细节保留度"最高的路径，应当作为默认推荐。

### 7.4 人声转伴奏（vocal2BGM）

```
vocals.wav（干声）
  → [L1] ACE-Step vocal2BGM（BPM/key 从 IR 或音频理解获得）
  → 产出 2-3 个候选伴奏（不同风格标注）
  → 用户选择 → [L1] 与人声混音 → 响度 → 导出
```

注意：ACE-Step 官方自认 vocal 质量偏粗糙，但 vocal2BGM 是它的强项（伴奏生成不需要咬字）。这条链路可以放心用。

---

## 8. 接口定义

### 8.1 外部 REST（FastAPI，`/v1`）

| 方法 | 路径 | 用途 | 请求要点 | 响应 | 幂等 | 超时 |
|---|---|---|---|---|---|---|
| POST | `/projects` | 建项目 | `{name, lang, rights_context}` | `{project_id}` | 否 | 5s |
| POST | `/songs` | 建歌并起草 | `{project_id, prompt, duration_s, ref_audio?}` | `{song_id, revision:0, score_abc, plan_summary}` | 否 | 120s（同步起草） |
| GET | `/songs/{id}/score` | 取谱 | `?format=abc\|musicxml\|graph&revision=` | 对应格式 | 是 | 5s |
| PUT | `/songs/{id}/score` | 整谱替换（文本编辑） | `{base_revision, abc_text}` | `{revision, conflicts[]}` | 否 | 30s |
| POST | `/songs/{id}/edits` | 应用 patch | `{base_revision, patch[], dry_run?}` | `{revision, affected_span, cost_estimate}` | 是（`op_id`） | 20s |
| POST | `/songs/{id}/renders` | 触发渲染 | `{revision, scope: full\|span\|mix, units?}` | `{job_id}` | 是（内容哈希） | 5s |
| GET | `/jobs/{job_id}` | 查任务 | — | `{status, progress, stage, artifacts}` | 是 | 3s |
| GET | `/jobs/{job_id}/events` | SSE 进度 | — | `event: stage/progress/artifact/error` | — | 长连接 |
| GET | `/songs/{id}/revisions` | 版本列表 | — | `[{revision, parent, summary, created_at}]` | 是 | 5s |
| GET | `/songs/{id}/diff` | 两版差异 | `?a=&b=` | `{note_diffs[], chord_diffs[], section_diffs[]}` | 是 | 10s |
| POST | `/covers` | 翻唱 | `{source_asset_id, voice_id, mode: render\|replace, key_shift}` | `{job_id}` | 是 | 5s |
| POST | `/stems/vocal2bgm` | 人声转伴奏 | `{vocal_asset_id, style_tags[], bpm?, key?}` | `{job_id}` | 是 | 5s |
| POST | `/stems/separate` | 分离 | `{asset_id, model}` | `{job_id}` | 是 | 5s |
| POST | `/voices` | 建音色（训练任务） | `{name, asset_ids[], consent_ref, epochs}` | `{voice_id, job_id}` | 否 | 10s |
| GET | `/voices` | 音色列表 | — | `[{voice_id, name, engine, eval, status}]` | 是 | 5s |
| GET | `/eval/{artifact_id}` | 指标报告 | — | `EvalReport` | 是 | 30s |
| GET | `/health` | 引擎健康 | — | `{engines:[{id,status,vram_free_mb,queue_depth}]}` | 是 | 3s |

**错误契约**（统一结构，前端只需要处理一套）：

```jsonc
{
  "error": {
    "code": "SCORE_PARSE_FAILED",
    "message": "第 12 小节第 3 拍音符时值超出小节长度",
    "detail": {"bar": 12, "beat": 3, "rule": "V4"},
    "recoverable": true,
    "suggest": "reduce_duration | split_note"
  }
}
```

错误码清单：`SCORE_PARSE_FAILED` / `PATCH_CONFLICT` / `PATCH_INVALID` / `ENGINE_UNAVAILABLE` / `VRAM_EXHAUSTED` / `RIGHTS_DENIED` / `TPA_BELOW_THRESHOLD` / `SEAM_RISK_HIGH` / `JOB_CANCELLED` / `UPSTREAM_TIMEOUT`。

`RIGHTS_DENIED` 与 `TPA_BELOW_THRESHOLD` 必须可被前端识别并给出**具体下一步**（前者指向授权台账，后者指向"用高保真模式重试"按钮），不允许只说"失败"。

### 8.2 SSE 事件

| event | data | 用途 |
|---|---|---|
| `stage` | `{stage: "plan"\|"render"\|"graft"\|"qa"\|"mix", unit_id?}` | 进度条分段 |
| `progress` | `{pct, eta_s}` | 数值进度 |
| `artifact` | `{artifact_id, kind, url, duration_s}` | 增量出结果（先出片段再出全曲） |
| `warning` | `{code, message}` | 降级提示（如"TPA 不足，已升级引擎"） |
| `error` | `{code, message, recoverable}` | 失败 |

**增量出结果是差异化体验的关键**：局部编辑时先推 `artifact`（该 unit 的音频）让用户 8 秒内听到，再推全曲混音。

### 8.3 内部 Engine 协议（Python Protocol，只签实现体不写）

```python
class Engine(Protocol):
    manifest: EngineManifest

    def probe(self) -> EngineHealth:
        """启动时与每 60s 调用一次。返回 {status, vram_free_mb, model_loaded, warm}"""

    def estimate(self, req: "RenderRequest") -> "CostEstimate":
        """返回 {eta_s, vram_peak_mb, quality_tier}。调度器用它做准入与路由。"""

    def render(self, req: "RenderRequest", ctx: "JobContext") -> "RenderResult":
        """唯一渲染入口。禁止在内部做重试；重试由 L2 负责（保证幂等）。"""

    def cancel(self, job_id: str) -> None:
        """协作式取消。必须在 ≤2s 内响应 ctx.cancelled。"""

    def unload(self) -> None:
        """释放显存。换出模型前调用。"""
```

**契约细节**（写进接口文档，不是建议）：

- `render` 必须把产物写到 `ctx.out_dir`，返回相对路径，不得返回内存 buffer（跨进程）。
- `render` 内不得读数据库、不得发网络请求（除模型自身的推理服务）。
- `estimate` 必须比 `render` 快 100 倍以上，否则调度器会阻塞。
- 每个引擎必须声明 `deterministic: bool`。非确定性引擎的缓存 key 必须含 `seed` 与 `attempt`。

`RenderRequest` 关键字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `score_abc` | string \| null | 走谱渲染时必填（YuE2 `abc=` 路径） |
| `scope` | `"full"` \| `"unit"` \| `"mix"` | 渲染范围 |
| `bar_range` | `[int,int]` \| null | unit 渲染时的区间 |
| `stems` | `["vocal","instrumental"]` | 需要哪些 stem |
| `constraints` | `{bpm?, key?, time_sig?, duration_s?}` | 硬约束，引擎必须服从或报错 |
| `context_audio` | `{pre_uri, post_uri}` \| null | repaint 用的上下文（前后各 1-2s） |
| `params` | dict | 引擎私有参数透传 |
| `seed` | int | 非确定性引擎必填 |

`RenderResult`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `artifacts` | `[{kind, path, sr, channels, duration_s, loudness_lufs}]` | 产物清单 |
| `engine_meta` | `{engine_id, version, adapter_hash, seed_used, wall_time_s, vram_peak_mb}` | 写入缓存 key 与审计 |
| `metrics` | `{tpa?, secs?, ...}` | 引擎自报指标（可选，以我方复算为准） |
| `warnings` | `[string]` | 降级信息 |

### 8.4 Capability Manifest

```jsonc
{
  "engine_id": "yue2",
  "version": "0.1.6",
  "adapter_version": "1.0.0",
  "capabilities": ["text2song", "score2song", "zero_shot_cover"],
  "inputs": {"abc": true, "lyrics": true, "reference_audio": false, "context_window_s": 0},
  "outputs": {"stems": false, "sr": 48000, "channels": 2, "max_duration_s": 600},
  "latency_class": "batch",          // interactive | fast | batch
  "deterministic": false,
  "resources": {"vram_peak_mb": 24576, "ram_peak_mb": 24576, "python": "3.12"},
  "license": {
    "code": "Apache-2.0",
    "weights": "CC-BY-NC-4.0",
    "commercial_use": false,
    "attribution_required": true,
    "note": "公司商用需另行取得权重授权"
  },
  "health_check": {"endpoint": "/health", "timeout_s": 5}
}
```

**`license.commercial_use` 是调度器的硬门禁输入**，不是文档字段。项目级 `rights_context` 为 `commercial` 时，`commercial_use:false` 的引擎直接不可选，前端灰显并给出原因。

### 8.5 降级契约

每一档降级都要有明确的触发条件、用户可见提示、以及"降级后指标是否达标"的判定：

| 触发 | 降级动作 | 用户可见 | 达标判定 |
|---|---|---|---|
| YuE2 显存不足 | 走 ACE-Step 全曲 | "当前使用快速引擎，音质为草稿级" | TPA 检测仍跑，但不承诺人声质量 |
| ACE-Step repaint TPA < 0.90 | 升级 YuE2 全曲 | "局部重绘未能准确命中目标音高，已切换到高保真重渲染（约 90s）" | 以 YuE2 结果为准 |
| Seam QA 连续 2 次不过 | 全曲重渲染 | "拼接接缝检测未通过，已自动重渲染整曲" | — |
| RVC 音色模型缺失 | 拒绝翻唱，提示先训练音色 | "该音色尚未训练完成" | — |
| LoRA adapter 与引擎版本不匹配 | 拒绝加载，回退 base | "该风格包与当前引擎版本不兼容" | — |

---

## 9. 关键数据结构（字段级）

### 9.1 Artifact

| 字段 | 类型 | 说明 |
|---|---|---|
| `artifact_id` | string | `sha256(canonical_params)` |
| `kind` | `vocal`\|`instrumental`\|`mix`\|`stem_drums`\|`stem_bass`\|`stem_other`\|`f0`\|`midi` | — |
| `path` | string | 内容寻址相对路径 `renders/ab/abcdef….flac` |
| `sr` / `channels` / `duration_s` | int/int/float | — |
| `loudness_lufs` / `true_peak_db` | float/float | 入库时测量，不信任引擎自报 |
| `source` | `{engine_id, version, adapter_hash, seed, revision, unit_id}` | 血缘 |
| `rights_ref` | string | 关联 RightsRecord（生成物继承输入的权利约束） |

### 9.2 RightsRecord（合规核心）

| 字段 | 类型 | 说明 |
|---|---|---|
| `asset_id` | string | 资产 ID（音频/歌词/音色） |
| `type` | `composition`\|`master`\|`voice`\|`lyrics` | 权利类型（词曲/录音/音色/歌词分开） |
| `owner` | string | 权利人 |
| `license_type` | `owned`\|`exclusive`\|`non_exclusive`\|`cc0`\|`cc_by`\|`statutory` | — |
| `scope` | `["train","render","commercial","cover"]` | 允许的用途集合 |
| `territory` | string[] | 地域（ISO 3166） |
| `valid_from` / `valid_until` | date | 期限 |
| `consent_ref` | string | 授权书文件的 sha256（**必须是文件哈希，不是"口头同意"**） |
| `revoked_at` | date \| null | 撤销时间 |
| `evidence` | string[] | 证据链路径（合同扫描件、邮件、平台协议条款快照） |

**三道门禁**（纯函数，可单测）：

```
assert_trainable(asset, purpose) -> bool   // 进入 LoRA 数据集前
assert_renderable(asset, purpose) -> bool  // 渲染/翻唱前
assert_publishable(artifact) -> bool       // 导出/发布前，向所有上游资产递归
```

任一门禁失败 → `RIGHTS_DENIED`，且错误信息要指明**缺哪个 scope、哪份材料**，不能只说"无权限"。

### 9.3 AdapterManifest（LoRA / 音色模型）

| 字段 | 类型 | 说明 |
|---|---|---|
| `adapter_id` | string | — |
| `base_engine` / `base_version` | string | **强绑定**，跨版本禁止静默加载 |
| `kind` | `lora_style`\|`voice_model`\|`lora_voice` | — |
| `dataset_manifest_hash` | string | 训练集清单哈希（可追溯到每首歌的 RightsRecord） |
| `train_config` | `{epochs, lr, batch, resolution, seed}` | 可复现 |
| `eval` | `{secs, fad, mos, tpa}` | 训练后评估结果，**未达标不发布** |
| `rights_ref` | string | 训练数据授权汇总 |
| `status` | `draft`\|`eval_failed`\|`published`\|`quarantined` | 撤销传播时置 quarantine |

---

## 10. 模型调度

### 10.1 硬件档位

| 档位 | 硬件 | 常驻引擎 | 可用能力 | 用途 |
|---|---|---|---|---|
| **T0 本机开发** | RTX 4060 Laptop 8GB / RAM 16GB | ACE-Step 1.5(2B) + RVC + Demucs | 草稿、局部重绘、翻唱、伴奏 | 开发调试、交互验证 |
| **T1 单卡生产** | 24GB（4090/3090） | YuE2(量化或 BF16) + ACE-Step（分时换出） | 全曲高质量 + 全部编辑 | 主生产 |
| **T2 远端** | 24–80GB，可多卡 | 全部 | 批量、LoRA 训练 | 训练与高峰 |

**T0 的现实约束**（必须在验收里写明）：YuE2 在 8GB 上属"非官方支持"，只能跑 Q8_0 量化且可能 OOM。**T0 不承诺 YuE2 可用**，只承诺 ACE-Step 路径全通。这是诚实的工程边界，不要用"社区说可以"来糊弄验收。

### 10.2 显存仲裁器

```
VRAM_BUDGET_MB = 8192（T0） - 800（驱动/显示） = 7392
```

| 规则 | 说明 |
|---|---|
| 准入 | `engine.estimate().vram_peak_mb + Σ(已加载引擎 vram) > budget` → 排队，不加载 |
| 换出 | 按 `last_used_at` + `reload_cost_s` 加权选受害者；`reload_cost` 高的最后换 |
| 预留 | 为 Demucs/RVC 预留 1.5GB（它们是编辑链的必经环节，不能等） |
| 硬上限 | 单引擎峰值超 budget 的 90% → 直接拒绝并报 `VRAM_EXHAUSTED`（不要让 CUDA OOM 兜底，OOM 后进程状态不可信） |

### 10.3 队列与优先级

| 队列 | 内容 | 并发 | 超时 | 抢占 |
|---|---|---|---|---|
| `interactive` | repaint、mix、preview | 1 | 30s | 可抢占 batch（batch 任务暂停或 requeue） |
| `fast` | ACE-Step 全曲、vocal2BGM、分离 | 1 | 120s | 被 interactive 抢占 |
| `batch` | YuE2 全曲、翻唱全链 | 1 | 600s | 不可抢占，可被暂停 |
| `train` | LoRA、RVC 训练 | 1（独占） | 4h | 独占，训练期间 interactive 走"仅 DSP"降级 |

**同队列 FIFO + 优先级数值**（用户前台操作 priority=0，预取 priority=10）。不做复杂调度，单机场景收益极低。

### 10.4 冷启动与预热

| 引擎 | 冷启动（估） | 预热策略 |
|---|---|---|
| ACE-Step 2B | 20–40s | 应用启动即加载，常驻 |
| YuE2 量化 | 60–120s | 首次请求后保持 15 分钟，超时换出 |
| RVC | 3–5s（含 index 加载） | 按音色 LRU 缓存最近 3 个 |
| Demucs | 5–8s | 常驻（占用小） |

**首响优化**：应用启动后异步预热 ACE-Step；用户进入编辑页时预取该曲所有 unit 的 stem 到内存（不重渲染）。这两条能把"第一次编辑"的体感从 40s 降到 8s。

### 10.5 失败与幂等

- `job_id = sha256(render_key)`。同一 key 重复提交 → 直接返回已有 job，不新建。
- 重试策略：网络/超时类 → 指数退避 3 次；OOM → 不重试，降级到低显存引擎；模型内部异常 → 换 seed 重试 1 次，仍失败则报错。
- 部分失败：unit 级渲染失败不影响其他 unit，全曲混音时用"上一 revision 的该 unit + 告警"，并在产物里标记 `degraded_units`。**不允许静默用旧片段冒充新片段**——这是最恶劣的正确性事故。

### 10.6 远端 worker 协议

同一 `Engine` 协议，transport 换成 HTTP：

```
POST /engine/{engine_id}/render   → 同步或 202 + job
GET  /engine/{engine_id}/health
```

- 请求体与本地 `RenderRequest` 完全一致（序列化后的 JSON）。
- 产物通过预签名 URL 回传，或走共享对象存储（内容哈希路径一致，无需传输）。
- 远端 worker 的 manifest 必须声明 `license` 与 `vram_peak_mb`，调度器一视同仁。
- **不做跨机器模型分片**。单卡放不下的模型就走量化或换引擎，不做 pipeline parallel（复杂度与收益不成比例）。

---

## 11. 缓存策略

### 11.1 三级缓存

| 级别 | Key | 值 | 命中收益 |
|---|---|---|---|
| **C1 解析缓存** | `sha256(abc_text + ppq)` | `ScoreGraph`（序列化） | 省 100–500ms 解析 |
| **C2 渲染缓存** | `sha256(unit_score_slice + engine_id + engine_version + adapter_hash + params + seed + stem_set)` | stem 音频（flac）+ metrics | 省 8–90s，**核心** |
| **C3 混音缓存** | `sha256(stem_set_hashes + mix_preset + loudness_target)` | 成品音频 | 省 2–5s |

**C2 的 key 必须包含 `engine_version` 与 `adapter_hash`**。引擎升级后静默复用旧音频，是最难查的一类 bug（听起来"没变化"，实际是新代码没生效）。

### 11.2 失效传播

```
patch → AffectedSpan → 映射到 RenderUnit 集合 → 标记 dirty
     → unit dirty 使其所属 section 的 C3 失效
     → 全曲级参数变化（tempo/key/结构/时长）→ 全部 dirty
```

**不做反向依赖**（不改动已缓存内容），只做正向标记。简化实现：每个 revision 存 `dirty_units: set[str]`，渲染时只跑 dirty，其余从 C2 取。

### 11.3 容量与淘汰

| 项 | 值 |
|---|---|
| 总预算 | 20GB（可配） |
| 单位成本 | `cost = size_mb × (1 + rebuild_time_s / 60)` |
| 淘汰策略 | LRU 加权：`score = last_used_ts / cost`，最小者先删 |
| 保护 | 当前 revision 的产物、最近 3 个 revision 的产物、`published` 标记的产物不淘汰 |
| 去重 | 内容哈希天然去重（同一 unit 在多个 revision 间共享） |

### 11.4 一致性约束

- 缓存条目写入前必须完成 `seam QA`（对拼接产物）与 `loudness` 测量；半成品不入缓存。
- 缓存条目带 `schema_version`，IR 结构升级时整体失效（迁移脚本或清空，不兼容读）。
- 删除只删文件，不删索引记录（索引记录保留 `deleted_at`，用于审计与"为什么这个 unit 重渲染了"的排查）。

---

## 12. 人声处理链

### 12.1 链路顺序（顺序有依据，不可随意调整）

```
① 分离 → ② 去混响 → ③ 降噪 → ④ F0 提取(双路) → ⑤ 音高修正 → ⑥ 音色转换(RVC)
→ ⑦ 转换后校验 → ⑧ 混音 → ⑨ 响度归一 → ⑩ 导出
```

**为什么 ② 在 ③ 之前**：先降噪会残留混响的调制成分，去混响器会把降噪伪影当成混响尾巴放大，产生"水声"。先去混响，噪声在降噪阶段更好处理。

**为什么 ④ 用双路**：RMVPE 对歌声稳健但对气声/弱唱容易八度错；CREPE 全帧更准但慢。两路交叉校验，差异 > 50 cents 的音符标记为"低置信"，UI 提示"该句音高检测置信度低，建议人工确认"，而不是硬修。

### 12.2 各环节选型

| 环节 | 选型 | 参数 | 备注 |
|---|---|---|---|
| ① 分离 | Demucs v4 `htdemucs_ft`（默认）/ MDX23C（质量优先） | shifts=2, overlap=0.25 | 分离质量决定上限，和声密集处最差 |
| ② 去混响 | UVR DeReverb 系 / WPE（传统，可解释） | 按 RT60 估计选强度 | 干声输入可跳过 |
| ③ 降噪 | DeepFilterNet3 / spectral gating（轻量） | 保留 0–12kHz | 过渡降噪会吃掉气声（情感细节） |
| ④ F0 | RMVPE（主）+ CREPE（校验） | hop=160 @16k, fmin=C2, fmax=C6 | 人声轨音域从 IR 约束 |
| ⑤ 音高修正 | WORLD/pyworld 或 PSOLA | 见 12.3 | **目标是 IR 音高** |
| ⑥ 转换 | RVC（`.pth` + `.index`，f0=rmvpe） | index_rate 0.5–0.75, protect 0.33 | 逐段 pitch shift |
| ⑦ 校验 | SECS + F0 复查 + 包络相关 | 见 16 章 | 不过则回退参数重跑 |
| ⑧ 混音 | pedalboard（EQ/压缩/de-esser/混响 send） | 预设 JSON | 预设可版本化 |
| ⑨ 响度 | EBU R128 两遍 | 流媒体 -14 LUFS / 俱乐部 -9 LUFS；真峰值 ≤ -1 dBTP | 两遍法（先测再线性增益） |
| ⑩ 导出 | WAV 48k/24bit、FLAC、MP3 320、stems 包 | — | 文件名含 revision 与 engine 标记 |

### 12.3 音高修正：目标来自 IR（差异化要点）

常规工具的音高修正是"修到最近的半音"（autotune 逻辑），会把有意的 Blues 音、滑音、装饰音全部抹平——这正是"AI 翻唱没有感情"的主要来源之一。

本系统不同：**修正目标是 IR 里的 MIDI 音高序列**（如果有 `pitch_cents` 偏移则目标带偏移）。

```
对每个音符 n：
  target_cents(t) = 1200*log2(f_target/440) + cents_offset(n)
  measured(t)     = f0_curve(t) 转 cents
  slow(t)  = lowpass(measured - target, cutoff=3Hz)      # 音准偏差（要修）
  fast(t)  = (measured - target) - slow(t)               # vibrato/装饰（要保留）
  corrected(t) = target + fast(t) * preserve_ratio      # preserve_ratio 默认 0.85
```

- `preserve_ratio` 暴露给用户（"音准修正强度"滑块，0.6–1.0），默认 0.85。
- 转音（连续 pitch 变化，如 R&B 的 run）通过检测"音高连续变化段"识别，段内不做逐帧吸附，只做整体偏移校正。
- **这是"情感细节保留"在技术上的落点之一**：修的是慢变偏差，保的是快变细节。

### 12.4 混音与响度规格

| 项 | 规格 |
|---|---|
| 采样率 | 48kHz（全链路统一，禁止中途重采样） |
| 位深 | 32-bit float 内部，24-bit 输出 |
| 人声链 | HPF 80Hz → **分频段 de-esser**（5–6.5kHz ≤2dB / 6.5–9kHz ≤4dB / **9kHz 以上不压**）→ 压缩(3:1, 3ms/80ms) → 存在感 EQ(2–4kHz +2dB) → 混响 send |
| 伴奏链 | 侧链压缩（人声触发，-2dB, 释放 150ms）→ 轻微 glue 压缩 |
| 总线 | limiter（真峰值 -1 dBTP） |
| 响度 | 两遍 loudnorm，目标可配，默认 -14 LUFS |
| 单声道兼容 | 检查 mono 折叠后人声不塌陷（-3dB 内），否则告警 |

> **v2.1 修正说明**：原写"de-esser(6–8kHz)"会让 8kHz 以上的高频重建成果被一并削掉，形成 6–9kHz 的频谱塌陷带，听感是"塑料"或"断层"。改为分频段后，9kHz 以上不再受压，与 v2.0 §4.2 的高频重建互补。完整耦合分析见 v2.1 §2.4。

#### 12.4.1 母带链（v1.1 补全，这是与商业发行差距最大的环节）

v1.0 只写了"limiter + loudnorm"，撑不起"可发布"。实际差距主要在母带。完整链如下，顺序不可随意调换：

| 序 | 处理 | 参数起始值 | 作用 | 常见错误 |
|---|---|---|---|---|
| 1 | 低频单声道化 | <120Hz 合并为 mono，24dB/oct | 避免低频相位抵消，黑胶/俱乐部友好 | 把 200Hz 也合并 → 低频变闷 |
| 2 | 动态 EQ（去浑浊） | 200–400Hz，阈值 -18dB，-2dB 衰减 | 清出人声的胸腔共鸣区 | 静态 EQ 一刀切，动态丢一半 |
| 3 | 多段压缩 | 低 60–120Hz 慢起慢放(30/300ms, 2:1)；中 1–4kHz 中速(10/120ms, 2.5:1)；高 8k+ 快速(3/80ms, 2:1) | 分频段控动态，整体更"紧" | 全段同一比率 → 人声被压扁 |
| 4 | 谐波饱和 | 磁带/电子管模拟，drive 2–4dB，混入 ≤30% | 增加密度与感知响度，不靠 limiter 硬顶 | 饱和过量 → 高频刺、齿音爆 |
| 5 | 立体声增强 | M/S：mid 保持，side +1.5dB（**<200Hz 不加宽**） | 宽度感 | 全频段加宽 → 低频散、单声道折叠塌陷 |
| 6 | 真峰值 limiter | -1.0 dBTP，lookahead 5ms，释放 80ms | 控制峰值，防流媒体转码削顶 | 只用 sample peak → 转码后失真 |
| 7 | 两遍响度归一 | 先测 integrated LUFS，再线性增益到目标 | EBU R128 合规 | 单遍动态压缩式归一 → 动态被吃掉 |

**验收**：`true_peak ≤ -1.0 dBTP`、`integrated_loudness` 与目标偏差 ≤ 0.5 LU、`LRA`（响度范围）不低于 5 LU（低于则说明压得太狠）。

#### 12.4.2 人声叠层与和声（v1.1 补全）

商业歌的"厚度与空间感"很大程度不来自混音，而来自**有多条人声**。单轨人声怎么混都比 Suno 的整轨生成薄。v1.0 只在 IR 里给了一个 `harmony` 轨位，没有生成逻辑，这是缺口。

| 层 | 生成方式 | 电平 | 注意 |
|---|---|---|---|
| Lead（主唱） | RVC 转换后的干声 | 0 dB 基准 | — |
| Double（双轨） | 同一 `voice_model` 换 seed 重转一次；或对 lead 做 ±8–15ms 时间偏移 + ±5–10 cents 微移调 + 不同 EQ | -12 ~ -18 dB | **必须同源同模型**，否则听起来是两个人 |
| Harmony（和声） | 依据 IR `chords` 推三度/六度，生成独立人声轨（ACE-Step 或 lead 变调 + formant 保持） | -15 ~ -20 dB | 和声音程必须落在和弦内，V5 校验 |
| Ad-lib | ACE-Step 在指定区间生成短促即兴（"oh"/"yeah"/气声） | -18 ~ -24 dB | 只在段落尾巴，不铺满 |

**硬约束**：和声与 double 的**时间基准必须与 lead 完全一致**（同一次 RVC 的 alignment），否则会出现"合唱团不齐"的廉价感。这是叠层最容易做砸的地方。

**验收**：叠层后 `EDR_timing` 不得比单轨下降超过 0.03（叠层不该破坏时间结构）；mono 折叠后 lead 不得低于 -3dB。

### 12.5 导出规格

| 用途 | 格式 | 响度 |
|---|---|---|
| 母带交付 | WAV 48k/24bit | -14 LUFS（或用户指定） |
| 流媒体 | FLAC 48k/24bit | 同上 |
| 分享 | MP3 320kbps | 同上 |
| 二次制作 | stems 包（vocals/drums/bass/other + 干声未处理版） | 不做响度归一（保留动态） |

---

## 13. 前后端技术选型

### 13.1 前端

| 层 | 选型 | 理由 | 备选 |
|---|---|---|---|
| 框架 | React 18 + TypeScript + Vite | 生态、类型安全、构建快 | Svelte（更轻但生态弱） |
| 状态 | Zustand | 编辑态高频更新，Redux 太重 | Jotai |
| 数据 | TanStack Query | 缓存/重试/失效语义现成 | SWR |
| 乐谱渲染（编辑） | **abcjs** | 原生 ABC（与 YuE2 对齐）、MIT、轻量、可直接改文本 | VexFlow（要自己写交互，工作量大 3 倍） |
| 乐谱渲染（只读高质量） | OpenSheetMusicDisplay | MusicXML 排版专业，适合打印/审阅 | MuseScore 嵌入（重） |
| 钢琴卷帘 | 自研 Canvas/WebGL | 需要音符级拖拽、力度曲线、音高微调，现成库都不够贴 | 无合适备选 |
| 波形 | wavesurfer.js v7 | 成熟、支持 region | — |
| 音频播放 | WebAudio + 自研调度器 | 需要多轨同步 + A/B 无缝切换 | Tone.js（可选，MIT） |
| 差异可视化 | 自研（基于 `/diff` 接口） | 需要"改了哪些音符"的色块标注 | — |

**四个必须自研的交互组件**（这是差异化的界面载体）：

1. **A/B 双轨对比播放器**：两个 revision 同步游标，一键切换（交叉淡化 20ms），支持"只切人声/只切伴奏"。
2. **音符级差异高亮**：改音高 = 橙色、改时值 = 蓝色、新增 = 绿色、删除 = 红色删除线；hover 显示旧值。
3. **局部渲染范围预览**：编辑前在地图上高亮"将重渲染的区间"，让用户知道代价。
4. **接缝风险提示条**：Seam QA 判 `high` 时，在波形对应位置打标记，点击可"改为整段重唱"。

### 13.2 后端

| 层 | 选型 | 理由 |
|---|---|---|
| Web 框架 | FastAPI + uvicorn | 异步、SSE 原生、OpenAPI 自动生成 |
| 任务队列 | **SQLite 表 + asyncio worker**（MVP）→ Arq/Redis（V2） | 单机不需要分布式队列；SQLite 事务足够保证幂等 |
| 元数据 | SQLite（WAL 模式） | 零运维；Revision DAG 用递归 CTE 查询 |
| 音频存储 | 本地内容寻址文件树 | 简单、可 rsync、可换 S3 |
| 音频处理 | ffmpeg + soundfile + librosa + pedalboard | ffmpeg 本机已有多份，无需额外安装 |
| 符号处理 | music21（解析/校验/转换）+ abcmidi（编译验证） | music21 生态最全；abcmidi 提供最严格的 ABC 语法检查 |
| 试听合成 | fluidsynth + SF2 音源 | 无 GPU 也能听谱（M0 的关键） |
| 观测 | structlog + 每 job 的 timing spans（JSON） | 延迟指标必须可归因到阶段 |

**为什么不用 Celery**：单机、单 GPU、任务数少。Celery 带来的 broker 部署成本与调试复杂度远超收益。SQLite + asyncio 在 1 个 worker 下完全够用，且任务状态与业务数据在同一事务里，天然一致。

### 13.3 双 venv 与进程隔离（不是洁癖，是依赖冲突的必然）

| 引擎 | Python 要求 | 关键依赖 |
|---|---|---|
| ACE-Step 1.5 | 3.11 | torch 2.x + 特定 CUDA 轮子 |
| YuE2 | 3.12（官方） | torch 2.x + 特定版本 |
| RVC | 3.10–3.12 | fairseq（对版本极敏感）、faiss |
| 主服务 | 3.12 | FastAPI、music21 |

**结论**：不可能共用一个 venv。因此：

```
services/engines/yue2/.venv/       # python3.12
services/engines/ace_step/.venv/   # python3.11
services/engines/rvc/.venv/        # python3.11
services/api/.venv/                # python3.12
```

每个引擎以 sidecar 进程运行，暴露 HTTP（内部回环），由 L2 的 `RemoteEngineAdapter` 调用。**本地引擎与远端引擎走同一个适配器**——这样"上云"只是把 URL 从 `127.0.0.1` 改成远端地址，代码零改动。这是插件化最实在的收益。

---

## 14. 插件式引擎接入

### 14.1 插件形态

```
services/engines/{engine_id}/
├── manifest.json          # Capability Manifest（必填，启动时校验）
├── engine.py              # 实现 Engine 协议（唯一入口）
├── server.py              # 可选：HTTP sidecar 包装（本地引擎默认走这个）
├── params.schema.json     # params 透传的 schema（前端据此生成控件）
├── requirements.txt
├── README.md              # 能力边界、已知缺陷、参数含义
└── tests/
    ├── test_contract.py   # 协议契约测试（所有引擎同一套）
    └── test_smoke.py      # 最小可用渲染（10s 片段，CI 里跑）
```

### 14.2 契约测试（所有引擎必须通过同一套）

| 测试 | 断言 |
|---|---|
| `test_manifest_valid` | manifest 通过 JSON Schema；`license` 字段齐全 |
| `test_probe` | 3s 内返回健康状态 |
| `test_estimate_cheap` | `estimate()` 耗时 < `render()` 的 1/100 且 < 200ms |
| `test_render_smoke` | 给定 10s ABC 片段，产出 ≥1 个 artifact，采样率与 manifest 一致 |
| `test_cancel` | 取消后 2s 内 `render` 返回 `JOB_CANCELLED` |
| `test_no_side_effect` | 渲染前后数据库与网络无外部请求（用 mock 拦截断言） |
| `test_deterministic_claim` | 声明 `deterministic:true` 的引擎，同 seed 两次渲染的哈希必须一致 |

**这套测试是"插件式"的实质**：没有契约测试，插件化只是"多个 if-else"。

### 14.3 能力协商与降级矩阵

调度器在启动时拉取所有 manifest，构建能力表。渲染请求按 `capabilities` 过滤候选，再按 `(quality_tier, eta, vram)` 排序。

降级矩阵必须显式声明在配置里（不是代码里 if）：

```jsonc
{
  "route_rules": [
    {"need": "score2song", "prefer": ["yue2"], "fallback": ["ace_step"], "on_fail": "explicit_error"},
    {"need": "repaint", "prefer": ["ace_step"], "fallback": ["yue2_full"], "on_fail": "degrade_notice"},
    {"need": "voice_convert", "prefer": ["rvc"], "fallback": [], "on_fail": "explicit_error"}
  ]
}
```

### 14.4 接入一个新引擎的六步

1. 写 `manifest.json`（能力、资源、许可证、确定性）。
2. 实现 `engine.py`（5 个方法）。
3. 过契约测试（`pytest tests/test_contract.py`）。
4. 注册到 `route_rules`（声明 prefer/fallback）。
5. 跑金标准集（20 首 × 5 类编辑），产出基线报告。
6. 灰度：按项目维度 5% → 50% → 100%，观察 TPA/UPS/延迟。

### 14.5 版本策略

- manifest 带 `adapter_version`，与引擎自身 `version` 分开。适配层改动 → `adapter_version` +1。
- 缓存 key 用 `engine_version + adapter_hash`，任一变化即失效。
- 引擎升级必须重跑金标准集，指标退化 > 5% 阻断升级（配置里可临时豁免，但需记录原因与到期日）。

---

## 15. 数据合规与 LoRA

### 15.1 原则

> 训练与微调数据**全部有据可查**。系统不包含任何抓取、爬取、批量下载公开音乐的功能，也不接受来源不明的数据集。

工程上如何保证（不是口号）：

| 措施 | 落地 |
|---|---|
| 无爬虫代码 | 仓库 CI 扫描：禁止出现 `requests`/`scrapy`/`yt-dlp` 对音乐站点域的调用；模型下载只允许 manifest 里声明的官方源 |
| 数据集门禁 | 数据集目录必须有 `manifest.json`，每项带 `rights_ref`；加载器解析前先跑 `assert_trainable` |
| 权重来源校验 | 下载后校验 sha256 与官方发布值一致，记录到 `provenance` |
| 生成物继承 | 产物 `rights_ref` 继承所有输入的最小权限集（交集，不是并集） |
| 撤销传播 | `revoked_at` 置位 → 级联 `quarantine` 所有派生 adapter 与产物，禁止导出 |

### 15.2 LoRA 流水线（ACE-Step 宿主）

官方口径：8–10 首目标风格音频，3090(12GB) 约 1 小时。本机 8GB 需实测，预期需要 `batch=1` + gradient checkpointing，可能仍 OOM——**因此 LoRA 训练默认排到 T2 远端，不在 T0 承诺**。

| 阶段 | 操作 | 产出 | 门禁 |
|---|---|---|---|
| 1 收集 | 用户上传 / 自有录制 | 音频 + RightsRecord | `assert_trainable(scope=train)` |
| 2 清洗 | 去混响、降噪、响度归一、切片（30–60s） | 训练片段 | 片段 RMS 在 -23±3 LUFS |
| 3 标注 | 自动：歌词转写 + caption 生成（ACE-Step Training tab）+ 人工校对 | 标注 JSON | 歌词与音频时长对齐误差 < 200ms |
| 4 训练 | LoRA（rank 16–32，lr 1e-4，epochs 依数据量） | `adapter.safetensors` | 训练 loss 收敛 + 无 NaN |
| 5 评估 | 生成 5 个固定 prompt × 3 seed，算 FAD/SECS/MOS | EvalReport | FAD 不超过 base 的 1.15×；风格相似度主观 ≥ 3.5/5 |
| 6 注册 | 写 AdapterManifest，绑定 base_version + dataset hash | adapter 记录 | 全部字段齐全 |
| 7 灰度 | 内部试用 → 发布 | — | — |

**关于 YuE2 的 LoRA**：官方仓库**未提供训练脚本**，社区有 adapter 尝试但未形成稳定方案。本方案**不承诺 YuE2 的 LoRA**。风格定制优先走"ACE-Step LoRA + IR 层风格约束（和弦词汇、节奏型、配器标签）"的组合，后者虽然不如 LoRA 精细，但可控、可解释、零训练成本。

**关于 RVC 的"LoRA"**：RVC 是 per-voice 全参微调（不是 LoRA），10–20 分钟干声即可。文档层面统一叫 `voice_model`，不混用 LoRA 术语，避免评审时概念漂移。

### 15.2.1 RVC 音色模型训练流水线（v1.1 补齐）

v1.0 只写了 LoRA（风格），漏了用户最先要做的"训练我的音色"。这是缺口，本节补齐。歌声训练与语音训练的参数取向不同，不能套语音教程。

| 阶段 | 操作 | 参数 / 判据 | 门禁 |
|---|---|---|---|
| 1 采集 | 用户上传或录制 | 48kHz 单声道，总时长 **15–30 分钟**（歌声的 f0 动态是说话的 3–5 倍，10 分钟覆盖不足，原 10–20 分钟口径已被 v2.0 §4.5.5 修正），**必须干声** | 含伴奏 / 明显混响 → 拒绝，提示重录或先跑去混响 |
| 2 质检 | 客观检测 | SNR ≥ 25dB；无削顶（峰值 < -1dBFS）；噪声底 < -50dBFS；估计 RT60 < 0.3s | 任一不过 → 先走 §12.2 的②③，处理后复检 |
| 3 切分 | 静音切分 | 3–10s 片段，阈值 -35dB，保留前后 150ms 气口 | 片段数 < 30 → 数据不足，拒绝训练 |
| 4 特征 | F0 + 内容特征 | F0 **必须用 RMVPE**（歌声音域宽，pm/harvest 会八度错）；音域按实测设 fmin/fmax（默认 C2–C6） | f0 八度错误率 > 3% → 换 f0 提取器或人工修正 |
| 5 训练 | RVC v2 | `batch` 按显存（8GB ≈ 4–6）；epoch 20–40 起步，按过拟合信号调；`save_every_epoch` 5 | loss 振荡不收敛 / 出现 NaN → 中止 |
| 6 索引 | faiss index | 用训练集特征建 `.index`，供 `index_rate` 检索 | 索引条数与片段数偏差 > 5% → 重建 |
| 7 评估 | 帕累托前沿扫描（§16.4） | 3 档 × 3 音色测试曲，跑 SECS/EDR/SFR | 平衡档 SECS < 0.82 或 EDR < 0.80 → **判训练失败**，不发布 |
| 8 注册 | 写 `AdapterManifest` | 绑定 base 版本、dataset hash、`pareto_front` | 字段不全 → 拒绝入库 |

**歌声训练的四个特有坑**（语音教程不会讲）：

| 坑 | 表现 | 处理 |
|---|---|---|
| 混响被学进音色 | 转换后自带"浴室感"，越听越假 | 训练前必须去混响；宁可损失一点高频，也不要带混响训 |
| 气声段八度错 | 弱唱、气声处 f0 跳到低八度，转换后突然变男低音 | 用 RMVPE + 对 `voicing` 置信度低的帧做插值，不硬取 |
| 高音区数据不足 | 副歌高音转换后发紧、破音 | 采集时要求覆盖歌曲实际音域；训练集里高音占比 < 15% 时提示补录 |
| 过拟合 | 训练集内 SECS 0.95、换歌就崩到 0.7 | 留 3 首**没进训练集**的歌做留出集，只认留出集指标 |

**留出集是硬要求**：评估只用训练集内的歌，等于自欺。`AdapterManifest.eval` 必须标明 `holdout: true` 才允许发布。

### 15.3 授权台账的运营要求

- 每个音色必须绑定 `consent_ref`（授权书文件哈希）。没有授权书的音色**不能上传**，不是"上传后审核"。
- 授权书模板需包含：使用范围（训练/生成/商用）、地域、期限、撤销条款、声音主体身份证明。
- 撤销必须可执行：`revoked_at` 置位 → 后台任务 24h 内完成级联隔离，并生成"已下架产物清单"。
- 审计：每季度导出一次 `rights_audit.csv`（asset / 类型 / 授权类型 / 到期日 / 派生数量）。

---

## 16. 评估体系

### 16.1 指标总表

| # | 维度 | 指标 | 定义 | 目标 | 采集 |
|---|---|---|---|---|---|
| 1 | 生成质量 | FAD | Fréchet Audio Distance（VGGish/CLAP 嵌入，对 200 首授权 held-out 集） | ≤ base 引擎的 1.2× | 离线，每版本 |
| 2 | 生成质量 | CLAP score | 文本-音频对齐余弦 | ≥ 0.30（基线校准） | 离线 |
| 3 | 生成质量 | 结构合规率 | 段落边界与 IR 偏差 ≤ 1 小节的段落占比 | ≥ 90% | 每次渲染 |
| 4 | 生成质量 | MOS | 5 级双盲，≥20 人 × 30 样本 | ≥ 3.6 | 每里程碑 |
| 5 | **符号编辑** | **TPA**（仅 render 路径） | 见 16.2 | ≥ 0.90 | 每次局部渲染 |
| 6 | **符号编辑** | TPA_hard | ±25 cents 严格版 | ≥ 0.75 | 每次局部渲染 |
| 7 | **符号编辑** | OnsetErr | 实际起音与网格偏差中位数 | ≤ 30ms | 每次局部渲染 |
| 8 | **未修改部分** | **UPS_emb** | 见 16.3 | ≥ 0.97 | 每次局部渲染 |
| 9 | **未修改部分** | UPS_lsd | log-mel 距离中位数 | ≤ 1.5dB(伴奏)/3.0dB(人声) | 每次局部渲染 |
| 10 | **未修改部分** | **UPS_wave** | 逐样本比对 | **= 1.0（硬断言，仅 graft 路径）** | 每次局部渲染 |
| 11 | 翻唱音色 | SECS | 说话人嵌入余弦 | ≥ 0.82（平衡档） | 每次翻唱 |
| 12 | 翻唱音色 | ΔSECS | 源音色相似度下降幅度 | ≥ 0.25 | 每次翻唱 |
| 13 | 翻唱音色 | WER | Whisper large-v3 转写 vs 歌词 | ≤ 5% | 每次翻唱 |
| 14 | **情感细节** | **EDR** 综合 | 见 16.4 | ≥ 0.80（平衡档，三档见 16.4） | 每次翻唱 |
| 15 | 延迟 | L_edit P50/P95 | patch→可试听 | ≤ 8s / ≤ 20s | 持续 |
| 16 | 延迟 | L_full P50 | 全曲渲染 | ≤ 90s（T1） | 持续 |
| 17 | 延迟 | TTFA | 首段可播放 | ≤ 3s（缓存命中）/8s（未命中） | 持续 |
| 18 | 延迟 | cache_hit | C2 命中率 | ≥ 40%（迭代场景） | 持续 |
| 19 | 可控性 | EditAcceptRate | 首次 patch 即被接受比例 | ≥ 60% | 在线 |
| 20 | 可控性 | RoundsToAccept | 达"可导出"的编辑轮次中位数 | ≤ 4 | 在线 |
| 21 | 可控性 | BlastRadius | 实际改变时长 / 请求影响区间时长 | ≤ 1.5 | 每次局部渲染 |
| 22 | 可控性 | PlanEditRatio | 用户改 IR 的次数 / 总操作数 | ≥ 0.5 | 在线 |
| 23 | 可控性 | SUS | 7 点量表 4 题（可控感） | ≥ 75 | 每里程碑 |
| 24 | 接缝 | SeamBlindRate | 盲听识别出接缝的比例 | ≤ 10% | 每版本 |
| 25 | **翻唱保真**（v1.1） | **SFR** | 见 16.2，仅 replace 路径 | pitch ≥ 0.95 / time ≤ 25ms | 每次翻唱 |
| 26 | 母带（v1.1） | 真峰值 / LRA | 见 12.4.1 | ≤ -1.0 dBTP / ≥ 5 LU | 每次导出 |
| 27 | **对比基准**（v1.1） | Q1 胜率（音色更像） | 见 16.7 | ≥ 60%，置信下界 > 50% | 每大版本 |
| 28 | **对比基准**（v1.1） | Q2 胜率（更接近原唱处理） | 见 16.7 | ≥ 60%，置信下界 > 50% | 每大版本 |

> 表中 1–4、11–14、25–28 的适用范围不同：TPA 只在 render 路径成立，SFR / SECS / EDR 只在翻唱路径成立，不可跨路径套用（v1.0 将 TPA 列为通用指标，是错误）。

### 16.2 音高类指标：TPA（render 路径）与 SFR（replace 路径）

**先说清一个 v1.0 的错误**：翻唱有两条路径（§7.3），音高来源完全不同。

- **render 路径**：让引擎按 IR 的谱重新唱，音高由 IR 给定 → 用 **TPA** 衡量"有没有唱到目标音高"。
- **replace 路径**：保留原唱演绎、只换音色，音高来自源演唱，IR 里没有目标音高 → **TPA 在这里无意义**（会恒判 0），必须换指标。

v1.0 只给了 TPA，等于 replace 路径没有音高质控。本版补齐。

#### TPA（Target Pitch Accuracy，目标音高匹配率）——仅 render 路径

```
对每个被 patch 修改的音符 n（按修改后值）：
  window = [onset_target - 50ms, onset_target + dur + 50ms]
  frames = f0_curve 落在 window 内且 voicing==1 的帧
  hit(f) = |cents(f0(f)) - cents(target_pitch(n))| <= 50
  r(n)   = Σ hit(f) / |frames|
TPA = Σ (r(n) * dur(n)) / Σ dur(n)        # 按时长加权
```

- 人声轨与器乐轨**分开报告**（人声受咬字影响，通常低 5–10 个百分点）。
- 音符目标音高含 `pitch_cents` 偏移时，阈值改为 ±50 cents 相对偏移后目标。
- 若某音符 `frames` 为空（完全无声），记为 0 并单独统计 `silent_notes`，不掩盖。
- **前置条件**：`render_mode == "render"`。其他模式调用该函数直接抛错，不允许静默返回 0。

#### SFR（Source Fidelity Ratio，源保真度）——仅 replace 路径

```
Δ = user_key_shift（半音，0 表示原调）
SFR_pitch = P( |cents(f0_out(t)) - cents(f0_src(t)) - 100*Δ| <= 50 )   # 帧占比
SFR_time  = median(|onset_out(i) - onset_src(i)|)                       # ms
SFR_gain  = 1 - |LUFS_out - LUFS_src| / 6                               # 响度一致性，截断到 [0,1]
```

目标：`SFR_pitch ≥ 0.95`、`SFR_time ≤ 25ms`、`SFR_gain ≥ 0.9`。

**为什么 replace 路径的 SFR_pitch 要求比 TPA 更严**：这条路径下模型**不该改音高**。若 SFR_pitch < 0.95，说明 RVC 的 `pitch shift` 设错（全局移调误用于局部段）、或 f0 提取在气声段跳八度。这是最容易漏检的一类事故——听起来"有点怪"但说不出哪怪。

**与 EDR 的分工**：SFR 查"音高/时间有没有漂移"（正确性），EDR 查"情感细节还在不在"（保真度）。两者不可互相替代。

### 16.3 UPS（未修改部分相似度）计算细则

```
区间 = 全曲 - (AffectedSpan ∪ seam_zones)      # seam_zones = 每个接缝 ±100ms
UPS_emb = mean(cos(MERT2(x_new[i]), MERT2(x_old[i])) for i in 区间, 帧级)
UPS_lsd = median(||logmel(x_new) - logmel(x_old)||_2)  over 区间
UPS_wave = max|x_new[i] - x_old[i]|  over 区间          # 必须为 0
```

**为什么三个都要**：`UPS_emb` 反映"听起来一样吗"，`UPS_lsd` 反映"频谱细节一样吗"，`UPS_wave` 反映"代码有没有偷偷重渲染"。只有 `UPS_wave` 是硬断言，另两个是质量指标——因为走 YuE2 全曲重渲染时，未修改区间必然不是 bit-exact，此时 `UPS_wave` 检查应跳过（并记录 `render_mode=full`）。

**这一点必须在实现里写清楚**：`UPS_wave == 1.0` 只在 `render_mode=graft` 时断言。否则指标会被"全曲重渲染"路径永久判失败。

### 16.4 EDR（情感与细节保留度）计算细则

五个子指标，全部可算，加权综合：

| 子项 | 定义 | 权重 | 目标 |
|---|---|---|---|
| `EDR_f0` | 源与输出 F0 轮廓 DTW 距离（半音，先减去全局均值差），取 RMS | 0.30 | ≤ 1.0 semitone |
| `EDR_dyn` | 能量包络（-40~0dB 归一）50ms 窗 Pearson r | 0.20 | ≥ 0.85 |
| `EDR_timing` | 音素/音符时长向量相关性（WhisperX 或 MFA 对齐） | 0.20 | ≥ 0.90 |
| `EDR_emo` | emotion2vec+ 嵌入余弦 + 8 类情感一致率 | 0.20 | cos ≥ 0.85，一致率 ≥ 0.80 |
| `EDR_vib` | 4–8Hz 频带 F0 调制能量比（输出/源） | 0.10 | 0.8–1.2 |

```
EDR = 0.30*(1 - min(EDR_f0/1.0,1)) + 0.20*max(EDR_dyn,0) + 0.20*max(EDR_timing,0)
    + 0.20*(0.5*cos + 0.5*acc) + 0.10*(1 - |1-EDR_vib|)
```

**这套设计的价值**：把"情感细节保留"从玄学变成 5 个可复现数字。评审时可以直接问"哪个子项不达标"，而不是"感觉像不像"。

**注意**：`EDR_emo` 使用 emotion2vec（通用语音情感模型），对**歌唱**的效度未经验证。因此它只作为趋势指标，不作为发布门禁的唯一依据；发布门禁看 `EDR` 综合 + MOS 人工盲测。

#### SECS 与 EDR 的帕累托冲突（v1.1 修正）

v1.0 同时给出 `SECS ≥ 0.82` 和 `EDR ≥ 0.80` 两个门禁，隐含假设两者可以同时达到。**这个假设是错的。**

RVC 的两个核心参数是此消彼长的：

| 参数 | 调高的效果 | 代价 |
|---|---|---|
| `index_rate`（检索特征占比） | 音色更贴目标（SECS↑） | 自然度下降，源演唱的细节被检索片段覆盖（EDR↓） |
| `protect`（清辅音/呼吸保护） | 气声、齿音、呼吸保留更好（EDR↑） | 目标音色被源音色稀释（SECS↓） |

也就是说，`(SECS, EDR)` 落在一条**帕累托前沿**上，不存在"两个都拉满"的工作点。不同源曲、不同音色对，前沿形状还不一样。

**工程结论**：不给单一门禁，给**三档预设**，由用户选，指标按档验收。

| 档位 | `index_rate` | `protect` | SECS 门禁 | EDR 门禁 | 适用 |
|---|---|---|---|---|---|
| 音色优先 | 0.72–0.78 | 0.25 | ≥ 0.85 | ≥ 0.72 | "要像那个人"，源唱功本身好 |
| 平衡（默认） | 0.58–0.62 | 0.33 | ≥ 0.82 | ≥ 0.80 | 通用 |
| 情感优先 | 0.42–0.48 | 0.45 | ≥ 0.78 | ≥ 0.86 | "要那一版的情绪"，源演绎是卖点 |

**实现要求**：训练完成后对每个 `voice_model` 自动扫一遍参数网格（3×3），把前沿画出来存进 `AdapterManifest.eval.pareto_front`。前端据此显示"这个音色在平衡档能到 SECS 0.84 / EDR 0.79"。**如果某音色的前沿整体低于门禁，直接判训练失败，不允许"调参调到过"**——那是过拟合评估集，不是模型可用。

**与 §12.3 的关系**：`preserve_ratio`（音准修正强度）作用在**转换之前**，控制"修掉多少音准偏差"；`index_rate` / `protect` 作用在**转换之中**，控制"换掉多少音色"。两者正交，但都会推高或压低 EDR：前者过高会抹掉 vibrato，后者过高会稀释音色。联合调参时先定 `preserve_ratio`（按源唱功决定），再扫 `index_rate × protect` 的二维网格。

### 16.5 金标准回归集

```
golden/
├── songs/                 # 20 首（10 中文 / 5 英文 / 5 纯器乐），全部授权
├── cases/
│   ├── 01_pitch_up_semitone.json
│   ├── 02_chord_reharmonize.json
│   ├── 03_transpose_section.json
│   ├── 04_lyric_syllable.json
│   ├── 05_tempo_change.json
│   ├── 06_cover_render_mode.json
│   ├── 07_cover_replace_mode.json
│   ├── 08_vocal2bgm.json
│   ├── 09_graft_seam_stress.json      # 故意在长音中间切
│   └── 10_full_rebuild.json
├── expected/              # 每个 case 的期望指标区间
└── run_all.py             # 全量跑，输出 EvalReport
```

**CI 门禁**：任一指标相对基线退化 > 5% → 阻断合并。允许人工豁免，但必须写 `waiver.json`（原因 + 到期日 + 批准人）。

### 16.6 在线体验指标采集

| 事件 | 埋点字段 | 用途 |
|---|---|---|
| `edit_submitted` | `op_types, target_section, span_bars, preview_used` | PlanEditRatio、BlastRadius |
| `edit_accepted` | `time_to_accept, revision_before/after` | EditAcceptRate、RoundsToAccept |
| `edit_reverted` | `revert_reason(user_chosen)` | 分析哪类编辑最常被拒 |
| `listen_ab` | `preferred` | 感知质量趋势 |
| `export` | `export_format, total_edits, elapsed` | 端到端效率 |
| `seam_flag_shown` | `unit_id, risk_level, user_action` | 接缝提示是否被采纳 |

### 16.7 与商业系统的成对偏好基准（v1.1 新增）

MOS 只能说明"还行"，不能说明"和对手比如何"。要做可达性判断，必须有成对对比协议。

**设计**（固定一切变量，只比一个维度）：

```
固定：同一首源曲（已授权）+ 同一个 voice_model + 相同时长 + 相同导出规格
对手：Suno v5.5 Cover + Persona 输出（同源曲上传，同音色意图）
本系统：replace 模式输出（vocal2BGM 保原唱 + RVC 换音色 + 母带链）
呈现：随机顺序、响度对齐到 -14 LUFS、双盲（评委不知道哪边是哪个系统）
```

**三个问题分开问，不合并**：

| 问题 | 选项 | 预期 | 若预期不成立说明什么 |
|---|---|---|---|
| Q1 哪个更像目标歌手的声音？ | A / B / 无差别 | 胜 | RVC 或音色模型有问题，先修训练链 |
| Q2 哪个更接近原唱的演唱处理（气口、转音、强弱）？ | A / B / 无差别 | 胜 | replace 模式没真正保留源，或分离把细节吃掉了 |
| Q3 整体更愿意循环听哪一版？ | A / B / 无差别 | **负** | 这是对手主场，输了属预期，不作为失败 |

**样本与统计**：≥30 名评审 × 20 组配对；报告胜率 + 95% Wilson 置信区间；"无差别"单独统计，不并入胜负。

**判定规则**：

- Q1、Q2 胜率 ≥ 60% 且置信区间下界 > 50% → 差异成立，可对外表述"在音色与演绎保真上优于通用生成工具"。
- Q1 或 Q2 胜率 < 50% → **阻断发布**，回到音色训练链与分离链排查，不允许用"整体听感"糊过去。
- Q3 不作门禁，但需记录趋势；若连续两个版本 Q3 从"负"升到"无差别"，说明母带链起作用了。

**成本**：每轮 600 次盲听，按每人 20 组 × 5 分钟计，约 30 人 × 100 分钟。每个大版本跑一次，不做持续化。

---

## 17. MVP 骨架与里程碑

### 17.1 目录结构

```
repo/
├── apps/
│   └── web/                        # React + Vite + TS
│       ├── src/features/score/     # abcjs 编辑器 + 差额高亮
│       ├── src/features/pianoroll/ # 自研卷帘
│       ├── src/features/ab/        # A/B 双轨播放器
│       ├── src/features/timeline/  # 渲染范围预览
│       └── src/api/                # 生成的 OpenAPI client
├── services/
│   ├── api/                        # FastAPI 编排层
│   │   ├── app/routers/            # songs, edits, renders, covers, voices, eval
│   │   ├── app/orchestrator/       # 影响分析、路由、graft、seam qa
│   │   └── app/planner/            # LLM planner（产出 patch）
│   └── engines/
│       ├── yue2/                   # sidecar + .venv(3.12)
│       ├── ace_step/               # sidecar + .venv(3.11)
│       ├── rvc/                    # sidecar + .venv(3.11)
│       └── dsp/                    # 分离/去混响/音高修正/混音（纯 DSP，可共 venv）
├── packages/
│   ├── score_ir/                   # IR + parse/serialize + patch + 校验（纯 Python）
│   ├── scheduler/                  # 队列 + 显存仲裁 + 缓存
│   ├── eval/                       # 全部指标实现（纯函数 + 单测）
│   └── contracts/                  # JSON Schema、错误码、事件定义
├── data/                           # 内容寻址（git-ignored）
│   ├── renders/  voices/  adapters/  rights/  cache_index.sqlite
├── golden/                         # 回归集
├── infra/
│   ├── bootstrap_venvs.ps1         # 建四个 venv（Windows）
│   ├── models.manifest.json        # 模型清单（URL + sha256，不含权重本体）
│   └── run_local.ps1               # 一键起 api + 四个 sidecar
└── docs/                           # 本文档 + API 文档 + ADR
```

### 17.2 模块职责矩阵

| 模块 | 职责 | 依赖 | 可单测 | 首版规模（估） |
|---|---|---|---|---|
| `packages/score_ir` | IR 模型、parse/serialize、patch 应用、V1–V8 校验 | 无（仅 music21/abcmidi） | ✅ 全纯函数 | 1.5k 行 |
| `packages/scheduler` | 队列、显存仲裁、缓存 key 与失效、引擎路由 | score_ir 类型 | 部分 | 1.2k 行 |
| `packages/eval` | TPA/UPS/EDR/SECS/FAD 计算 | numpy/librosa | ✅ | 1.0k 行 |
| `services/api/orchestrator` | 影响分析、graft、seam QA、revision 管理 | score_ir, scheduler | 部分 | 1.8k 行 |
| `services/api/planner` | LLM 调用、JSON schema 约束、重试 | 外部 LLM | ✅（mock） | 0.5k 行 |
| `services/engines/*` | 各引擎适配 + sidecar | 各自 venv | 契约测试 | 0.3k 行/个 |
| `apps/web` | 四类编辑交互 + A/B | API | 组件测试 | 4k 行 |

### 17.3 里程碑

| 里程碑 | 内容 | 验收（可执行） | 不达标即返工 |
|---|---|---|---|
| **M0** 符号闭环（1–2 周） | IR + ABC 往返 + MusicXML 导出 + fluidsynth 试听 + 文本编辑 | ① 20 首 ABC 往返 `parse→serialize→parse` 结果一致；② V1–V8 全过；③ 能在浏览器改谱并听到 fluidsynth 声音 | 往返有损 > 0 个音符 |
| **M1** 快速编辑闭环（2–3 周） | ACE-Step 接入 + repaint + 分区 graft + 前端乐谱编辑 + A/B | ① 改一个音符，端到端 ≤ 20s；② `UPS_wave == 1.0`（graft 区间）；③ SeamBlindRate ≤ 20%（内部 5 人） | 接缝被 5 人中 2 人以上听出 |
| **M2** 高质量渲染（2 周） | YuE2 接入（`abc=`）+ 全曲渲染 + SheetSage2 转谱 + 零样本 cover | ① 3.6 分钟歌 ≤ 120s（T1）；② 转谱后 cover 的 CLEWS 首位命中率 ≥ 40%；③ TPA ≥ 0.88 | TPA < 0.80（说明 `abc=` 没生效） |
| **M3** 翻唱全链（3–4 周） | RVC 训练与转换 + 人声处理链 + 母带链 + 人声叠层 + 导出 + 双路线（render/replace） | ① SECS ≥ 0.82（平衡档）；② EDR ≥ 0.80（平衡档）；③ SFR_pitch ≥ 0.95；④ WER ≤ 5%；⑤ 真峰值 ≤ -1.0 dBTP；⑥ Q1/Q2 胜率 ≥ 60%；⑦ 导出 48k/24bit + stems | 任一项不达标 |
| **M4** 评估与个性化（2 周） | 指标流水线 + 金标准集 + LoRA（ACE-Step，T2）+ 授权台账 | ① 24 项指标全自动出报告；② 金标准 20×10 用例全跑通；③ LoRA 训练→评估→注册全链路 | LoRA 评估未达标仍可发布 |

### 17.4 首批要写的 12 个文件（按依赖顺序）

| # | 路径 | 关键导出符号 | 说明 |
|---|---|---|---|
| 1 | `packages/contracts/schemas/song_ir.schema.json` | — | IR 的 JSON Schema，前后端共用 |
| 2 | `packages/score_ir/models.py` | `SongIR, ScoreGraph, Note, Chord, Section, Performance` | 数据类，无逻辑 |
| 3 | `packages/score_ir/abc_io.py` | `parse_abc(), to_abc(), roundtrip_ok()` | 往返是硬要求 |
| 4 | `packages/score_ir/validate.py` | `validate(graph) -> list[Violation]` | V1–V8 |
| 5 | `packages/score_ir/patch.py` | `EditOp, apply_patch(), invert()` | 纯函数 |
| 6 | `packages/score_ir/span.py` | `affected_span(op, graph) -> Span` | 影响分析 |
| 7 | `packages/scheduler/cache.py` | `render_key(), lookup(), store(), invalidate()` | 三级缓存 |
| 8 | `packages/scheduler/route.py` | `choose_engine(unit, span, health) -> EngineChoice` | 6.2 决策树 |
| 9 | `packages/eval/metrics.py` | `tpa(), ups(), edr(), secs()` | 全纯函数 + 单测 |
| 10 | `services/engines/base.py` | `Engine` Protocol, `RenderRequest`, `RenderResult` | 协议定义 |
| 11 | `services/api/orchestrator/graft.py` | `graft(old, new, plan) -> (audio, seam_report)` | 6.3 协议 |
| 12 | `apps/web/src/features/ab/ABPlayer.tsx` | — | 差异化体验载体 |

**顺序理由**：1→6 是纯逻辑层，无 GPU 依赖，可在任何机器上开发与单测，是 M0 的全部内容。7→11 依赖 GPU 但可 mock。12 可以并行。

---

## 18. 风险登记册

| ID | 风险 | 触发条件 | 影响 | 缓解 | 残余风险 |
|---|---|---|---|---|---|
| R1 | YuE2 权重非商业 | 产品要收费 | **阻塞商业化** | 引擎可替换 + 不带权重发行 + 商业授权谈判 | 高（法务侧决定） |
| R2 | 8GB 跑不动 YuE2 | 本机实测 OOM | 开发期无法验证高质量路径 | 量化试验 + 远端 worker + ACE-Step 兜底 | 中 |
| R3 | 人声 graft 可听接缝 | 长音中间切、元音连续 | 体验崩塌 | 只在换气点切 + 整段重唱 + Seam QA 降级 | 中 |
| R4 | ACE-Step repaint 不服从目标音高 | TPA < 0.90 | 局部编辑不可信 | TPA 检测 + 自动升级 YuE2 | 低（有兜底） |
| R5 | ACE-Step 官方自认人声粗糙、中文说唱弱 | 中文歌 | 人声质量不达标 | 人声主路径走 YuE2 渲染 + RVC 转换，ACE-Step 只做草稿 | 中 |
| R6 | Python 3.11/3.12 依赖冲突 | 同机部署 | 装不上 | 双 venv + sidecar（架构已按此设计） | 低 |
| R7 | So-VITS-SVC 停维护 + AGPL | 有人要求接入 | 法务与维护风险 | 不列主路径，插件位保留，接入需单独法务评估 | 低 |
| R8 | 翻唱版权双重授权（词曲 + 音色） | 发布/商用 | 法律风险 | RightsRecord 双维度 + `assert_publishable` 递归 | 中（用户行为不可控） |
| R9 | 指标过拟合自建基准 | 只对自己数据集优化 | 上线后体感不符 | 保留外部盲测 + MOS + 真实用户 A/B | 中 |
| R10 | LLM 产出的 patch 音乐性差 | 和弦外音、声部交错 | 用户频繁撤销 | V3/V8 校验 + `confidence` 门限 + 建议卡片模式 | 中 |
| R11 | 缓存 key 漏字段导致脏读 | 引擎升级/adapter 换 | 隐性 bug | key 含 version+hash；金标准回归 | 低 |
| R12 | 全曲重渲染频率过高 | 用户频繁改 tempo/结构 | 延迟不可接受 | UI 明确代价 + 引导（改结构先改谱后渲染） | 中 |

---

## 19. 待实测清单（必须由真机填写，未填则 CI 阻断）

这些量无法从文档推导，只能测。每项都要有测量脚本与写入位置。

| # | 待测项 | 测量方法 | 写入位置 | 阻断规则 |
|---|---|---|---|---|
| M1 | YuE2 Q8_0 在 8GB 上的峰值显存与是否 OOM | `nvidia-smi` 采样 + 3 次 30s 片段渲染 | `manifest.json.resources.vram_peak_mb` | 未填则 T0 不允许路由到 YuE2 |
| M2 | ACE-Step 2B 冷启动与热渲染耗时 | 计时 5 次取 P50 | `manifest.json.latency` | 未填则 estimate 用保守值 30s |
| M3 | ACE-Step repaint 的 TPA 实测分布 | 金标准 case 01/03/04 各 20 次 | `golden/expected/tpa_ace_step.json` | 未填则不允许 repaint 走快路径 |
| M4 | 人声 graft 在中文元音处的盲听识别率 | 10 人 × 20 片段双盲 | `golden/expected/seam_blind.json` | 未填则默认 `seam_risk=high` |
| M5 | RVC 在本机 8GB 的实时率与 SECS 基线 | 3 个音色 × 5 段 | `golden/expected/rvc_baseline.json` | 未填则翻唱功能不可用 |
| M6 | SheetSage2 转谱的音符准确率（中文流行） | 10 首人工标注对比 | `golden/expected/sheetsage2.json` | 未填则 cover 模式标记 beta |
| M7 | Emotion2vec 在歌唱上的效度 | 与 5 人主观评分相关性 | `golden/expected/edr_validity.json` | 相关性 < 0.5 则 EDR_emo 权重降为 0 |

---

## 20. 目标可达性（v1.1 追加）

> 完整论证见 `自我审查报告-能否比肩Suno-v1.md`。本节只放结论与验收口径。

### 20.1 七维判定

| # | 维度 | 判定 | 依据 |
|---|---|---|---|
| 1 | 音色像不像目标人 | 打平略胜 | Suno Voices 可克隆但平台锁定、需验证、部分地区不可用；RVC 本地、可测、可迁移 |
| 2 | 情感 / 演绎细节保留 | **胜** | replace 保留原唱全部 F0 / timing / 气口；Suno Cover 是重新演绎 |
| 3 | 音质 / 混音商业度 | **负** | 我们多一道分离，分离损失不可逆；Suno 无此环节且用内部 12 轨 |
| 4 | 中文咬字 | **负（待实测）** | ACE-Step 官方自认 vocal 粗糙；YuE2 中文无公开证据 |
| 5 | 编辑粒度 | **胜** | 音符级 vs 段落级 |
| 6 | 出歌速度 | **负（数量级）** | 端到端 5–20 分钟 vs <1 分钟 2 首 |
| 7 | 数据归属 / 离线 | **胜** | 全本地、模型可迁移、无地域限制 |

**4 胜 3 负。**（14 个细分维度归并后的粗粒度口径，见 §1.3 的口径说明。）负的三项里，第 3、6 项是架构决定的，不可优化到打平；第 4 项只能靠实测确认，不能假设。

### 20.2 硬件阶梯与可达上限

| 档位 | 能不能做出"对标商业发行" | 说明 |
|---|---|---|
| T0（本机 8GB） | **不能** | YuE2 官方要求 24GB，8GB 属非官方支持；ACE-Step LoRA 官方口径 12GB 起，本机大概率 OOM。T0 只能做 ACE-Step + RVC 的"可用翻唱 + 可控编辑" |
| T1（24GB 单卡） | **接近，但需补齐 §12.4 母带链与 §12.4.2 人声叠层** | 缺这两项，成品仍会明显薄于整轨生成 |
| T2（远端/多卡） | 同 T1，另可做 LoRA 训练与 best-of-N 筛选 | best-of-8 让渲染成本 ×8，延迟从 90s 到 12 分钟 |

### 20.3 对外表述红线

| 禁止 | 替换为 |
|---|---|
| "效果超越 Suno" | "在音色归属、演绎保留、编辑粒度上优于通用生成工具" |
| "达到商业发行水准" | "达到可发布的 demo 水准，母带链可继续精修" |
| 引用 YuE2 best-of-8 的 6.96 作为单次能力 | 同时给 6.73（单次）与 6.96（best-of-8） |
| "AI 自动混音达到专业混音师水平" | "提供可复现的混音预设链，支持人工精修" |

**依据**：2025 年末听众盲测中，Suno v5 短片段被误判为真人演唱的比例约 60–70%。当对手输出已跨过人耳分辨阈值，音质不再是可证明的竞争维度。

### 20.4 验收附加门禁

§17.3 的 M3 验收原为 SECS / EDR / WER 三项。v1.1 追加：

| 追加门禁 | 阈值 | 不达标后果 |
|---|---|---|
| §16.7 Q1 胜率（音色更像） | ≥ 60%，置信下界 > 50% | 阻断发布，回查音色训练链 |
| §16.7 Q2 胜率（更接近原唱处理） | ≥ 60%，置信下界 > 50% | 阻断发布，回查分离与人声链 |
| SFR_pitch（replace 路径） | ≥ 0.95 | 阻断，检查 pitch shift 与 f0 提取 |
| 母带真峰值 | ≤ -1.0 dBTP | 阻断导出 |
| 人声叠层后 EDR_timing 下降 | ≤ 0.03 | 回退叠层参数 |

**注意**：Q3（整体更愿意循环听）**不作门禁**。把它当门禁等于要求在本项目最弱的维度上赢，会导致无止境的返工与造假式调参。

---

## 附录 A · 术语表

| 术语 | 含义 |
|---|---|
| Song IR | 歌曲中间表示，本系统的唯一计划权威 |
| ScoreGraph | IR 的结构化层（音符/和弦/段落/轨道） |
| Performance | IR 的表现层（力度/vibrato/气声/混音） |
| EditOp / Patch | 编辑算子 / 算子集合，是编辑的唯一表达形式 |
| AffectedSpan | 一次编辑在时间轴上实际影响的区间 |
| RenderUnit | 渲染的最小单元（小节对齐） |
| Graft | 新旧音频片段的拼接（分轨两套算法） |
| Seam QA | 拼接接缝的自动检测与降级判据 |
| TPA | 目标音高匹配率（仅 render 路径：引擎按谱重唱） |
| SFR | 源保真度（仅 replace 路径：音高/时间不得相对源漂移） |
| UPS | 未修改部分相似度（emb/lsd/wave 三项） |
| EDR | 情感与细节保留度（5 子项加权） |
| SECS | 说话人嵌入余弦相似度（音色相似度） |
| voice_model | RVC 音色模型（`.pth` + `.index`），per-voice 全参微调，需 10–20 分钟干声 |
| pareto_front | SECS/EDR 参数扫描得到的可行工作点集合（见 §16.4） |
| Capability Manifest | 引擎能力声明，含许可证与资源需求 |
| RightsRecord | 授权台账记录 |
| AdapterManifest | LoRA/音色模型的元数据，强绑定 base 版本 |

## 附录 B · 事实来源与核对日期

| 事实 | 来源类型 | 核对日期 |
|---|---|---|
| YuE2-3B 架构（AR-NAR MoT + flow matching + VAE，48kHz） | 官方模型卡转述 / 多方技术解读 | 2026-11-08 |
| YuE2 `plan/generate_semantic/synthesize/decode` 四段式 + `cot` 参数 + `abc=` 入参 | 社区技术解析（多源一致） | 2026-11-08 |
| YuE2 权重 CC BY-NC 4.0 / 代码 Apache-2.0 | 模型卡 + 多个转载源一致 | 2026-11-08 |
| YuE2 官方 24GB VRAM / 4090 71s 出 3.6 分钟 / 峰值 14.08GiB | 模型卡 + 实测转载 | 2026-11-08 |
| YuE2 零样本 cover CLEWS 71.3% vs 0.3% | 官方评测转述 | 2026-11-08 |
| ACE-Step 1.5：MIT、<4GB VRAM、Qwen3 planner + DiT、5Hz token、LoRA 8 首 1 小时 | 官方 README + 多源 | 2026-11-08 |
| ACE-Step 能力：repaint/cover/vocal2BGM/extract/lego/complete/metadata control | 官方 README + 文档 | 2026-11-08 |
| RVC：MIT、活跃、RMVPE、top-1 retrieval | 仓库元数据 + 多个转载源 | 2026-11-08 |
| So-VITS-SVC：AGPL-3.0、2023-11 归档 | 转载源一致 | 2026-11-08 |
| Suno v5.5：Voices（录音克隆 + 随机短语验证，仅本人，18+，部分地区不可用）、Custom Models（≥6 首自供曲目）、Cover（≤8 分钟，可与 Persona 组合） | 官方产品页 + 多方转载 | 2026-11-08 |
| Suno Studio：段落级重生成、12 轨分离（用于自家生成轨）、Remove FX、Remix 换和弦保持 phrasing、Alternates 并排试听、MIDI 导出 | 第三方 wiki + 评测转述 | 2026-11-08 |
| Suno v5 短片段被盲测误判为真人演唱的比例约 60–70%（长抒情曲更低） | 第三方评测转述 | 2026-11-08 |

> Suno 相关能力均来自第三方转述与产品页，**正式对外引用前必须以官方文档复核**。本方案引用它们仅用于"目标可达性"判断（§20）与对标表（§1.3），不作为我方功能承诺的依据。

> 未在官方仓库直接核对的项目（如 YuE2 `abc=` 的函数级签名、ACE-Step 具体 REST 端点名）在实现时必须重新核对，不得按本文档硬编码。适配层以 `probe()` + manifest 为准。

## 附录 C · 与用户原始需求的对应关系

| 用户要求 | 本文落点 |
|---|---|
| 语言模型作曲规划器 | §3.2、§5.3（只产 patch） |
| 符号乐谱中间表示（ABC/MusicXML） | §4（三层表示 + 双通道） |
| 所有修改先在符号层完成再触发渲染 | §5、§7.2 |
| 局部修改 / 保留未修改部分 | §6.3、§16.3（UPS_wave 硬断言） |
| 分轨处理 / 迭代预览 | §6.1、§8.2（增量 artifact）、§9.1 |
| 整曲高质量接 YuE2 | §2、§6.2、§14 |
| 快速草稿/局部重绘/翻唱/人声转伴奏接 ACE-Step | §6.2、§7.4、§14.3 |
| 高保真翻唱接 RVC / So-VITS-SVC | §2.1（选 RVC）、§12 |
| 人声分离/F0/音高修正/去混响/响度/混音导出 | §12 全章 |
| 模块划分 / 数据流 / 接口 / 数据结构 | §3、§7、§8、§9 |
| 模型调度与缓存 | §10、§11 |
| 前后端选型 | §13 |
| 最小可用原型骨架 | §17（文件级骨架 + 12 个首批文件） |
| 插件式引擎接入 | §14（manifest + 契约测试 + 路由规则） |
| 数据完全授权 / 少量歌曲 LoRA / 不抓取 | §15 |
| 可量化评估指标 | §16（28 项，含公式与阈值） |
| 放入我的声音模型并训练 | §0.5（概念区分）、§15.2.1（RVC 音色训练流水线） |
| 清唱 / 带伴奏歌曲翻唱 | §7.3（双路线）、§12（处理链）、§16.2（SFR） |
| "能否比肩/超越 Suno"的可达性判定 | §20（七维判定 + 表述红线）、§1.3、§16.7（成对偏好基准） |

---

**文档结束。**
