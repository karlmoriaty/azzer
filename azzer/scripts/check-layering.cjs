/**
 * 分层依赖方向检查。
 *
 * 为什么需要它：六层架构目前在文档里只是口头约定，代码里没有任何 crate 边界
 * 能机械地保证它。最容易出问题的一步是 M2 接 ONNX 时图方便在 azzer-pipeline
 * 里直接 `use ort` —— 那会把「调度层不知道推理 SDK」这条彻底破坏，而且编译
 * 照样通过，没人会发现。这个脚本把它变成可执行的检查。
 *
 * 层级（上依赖下）：
 *   L1 体验    apps/web、src-tauri
 *   L2 编排    azzer-api
 *   L3 计划    azzer-core::dag（契约）
 *   L4 调度    azzer-pipeline（节点执行、幂等、缓存）
 *   L5 引擎    azzer-dsp、azzer-infer
 *   L6 资产    azzer-core（内容寻址存储、模型声明）
 *
 * 用法：node scripts/check-layering.cjs
 * 退出码：0 全通过，1 有违规
 */
const fs = require("node:fs");
const path = require("node:path");

const AZ = path.resolve(__dirname, "..");

/** 每个 crate 的层级与禁止依赖。 */
const RULES = {
  "azzer-core": {
    layer: "L3/L6 契约与资产",
    forbiddenCrates: [
      "azzer-dsp",
      "azzer-infer",
      "azzer-pipeline",
      "azzer-api",
      "azzer-shell",
    ],
    forbiddenLibs: [
      ["ort", "契约层不能绑定推理 SDK"],
      ["hound", "音频编解码属于引擎层"],
      ["rustfft", "频域处理属于引擎层"],
      ["axum", "契约层不涉及 HTTP"],
      ["tauri", "契约层不涉及宿主"],
    ],
  },
  "azzer-dsp": {
    layer: "L5 引擎（纯 DSP）",
    forbiddenCrates: ["azzer-infer", "azzer-pipeline", "azzer-api", "azzer-shell"],
    forbiddenLibs: [
      ["ort", "DSP 层不碰推理"],
      ["axum", "DSP 层不碰 HTTP"],
      ["tauri", "DSP 层不碰宿主"],
    ],
  },
  "azzer-infer": {
    layer: "L5 引擎（推理）",
    forbiddenCrates: ["azzer-dsp", "azzer-pipeline", "azzer-api", "azzer-shell"],
    forbiddenLibs: [
      ["axum", "引擎层不碰 HTTP"],
      ["tauri", "引擎层不碰宿主"],
    ],
  },
  "azzer-pipeline": {
    layer: "L4 调度",
    forbiddenCrates: ["azzer-api", "azzer-shell"],
    // 这条是本脚本存在的首要理由
    forbiddenLibs: [
      ["ort", "调度层必须走 Engine 协议，不能直接持有推理 SDK"],
      ["axum", "调度层不碰 HTTP"],
      ["tauri", "调度层不碰宿主"],
      ["hound", "编解码由 azzer-dsp 承担，调度层不该自己读 WAV"],
    ],
  },
  "azzer-api": {
    layer: "L2 编排接口",
    forbiddenCrates: ["azzer-shell"],
    forbiddenLibs: [["tauri", "API 层不依赖宿主，否则无法脱离 Tauri 调试"]],
  },
  "azzer-shell": {
    layer: "L1 体验宿主",
    forbiddenCrates: [],
    forbiddenLibs: [],
  },
};

/** 粗读 Cargo.toml 的 [dependencies] / [dev-dependencies] / [build-dependencies] 段。 */
function readDeps(tomlPath) {
  const text = fs.readFileSync(tomlPath, "utf8");
  const deps = new Set();
  let section = "";
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    if (line.startsWith("[")) {
      section = line.replace(/[[\]]/g, "");
      continue;
    }
    if (!/^(dependencies|dev-dependencies|build-dependencies)$/.test(section)) continue;
    // 形如 `name = ...` 或 `name.workspace = true`
    const m = line.match(/^([A-Za-z0-9_-]+)\s*(?:\.\w+)?\s*=/);
    if (m) deps.add(m[1]);
  }
  return deps;
}

function locateCrate(name) {
  if (name === "azzer-shell") return path.join(AZ, "src-tauri", "Cargo.toml");
  return path.join(AZ, "crates", name, "Cargo.toml");
}

const problems = [];
const ok = [];

for (const [name, rule] of Object.entries(RULES)) {
  const toml = locateCrate(name);
  if (!fs.existsSync(toml)) {
    problems.push(`${name}：找不到 ${path.relative(AZ, toml)}`);
    continue;
  }
  const deps = readDeps(toml);

  for (const bad of rule.forbiddenCrates) {
    if (deps.has(bad)) {
      problems.push(`${name}（${rule.layer}）依赖了 ${bad} —— 方向反了（下层依赖上层）`);
    }
  }
  for (const [lib, why] of rule.forbiddenLibs) {
    if (deps.has(lib)) {
      problems.push(`${name}（${rule.layer}）依赖了 ${lib} —— ${why}`);
    }
  }
  ok.push(`${name}：${rule.layer}，${deps.size} 个直接依赖，无越界`);
}

// 反向检查：确认每个内部 crate 至少被一个上层依赖，避免出现无人引用的孤岛
const allInternal = new Set(Object.keys(RULES));
const referencedBy = new Map();
for (const name of allInternal) {
  const toml = locateCrate(name);
  if (!fs.existsSync(toml)) continue;
  for (const dep of readDeps(toml)) {
    if (allInternal.has(dep)) {
      const arr = referencedBy.get(dep) ?? [];
      arr.push(name);
      referencedBy.set(dep, arr);
    }
  }
}

for (const name of allInternal) {
  if (name === "azzer-shell") continue; // 宿主是入口，没人依赖它
  if (!referencedBy.has(name)) {
    problems.push(`${name} 没有被任何 crate 依赖 —— 可能是孤岛`);
  }
}

console.log("=== 分层检查 ===");
for (const line of ok) console.log(`  OK  ${line}`);
console.log("\n=== 内部依赖图 ===");
for (const [name, by] of [...referencedBy.entries()].sort()) {
  console.log(`  ${name}  ←  ${by.join(", ")}`);
}

if (problems.length) {
  console.log("\n=== 违规 ===");
  for (const p of problems) console.log(`  FAIL  ${p}`);
  process.exit(1);
}
console.log("\nRESULT: PASS —— 依赖方向符合六层约定");
