/**
 * 一键验收：把散落的检查串成一条命令。
 *
 * 顺序按「便宜且能早失败的先跑」：
 *   1. 分层依赖方向（纯文本解析，毫秒级）
 *   2. Rust 测试（编译占大头）
 *   3. 前端类型检查
 *   4. 前端生产构建
 *
 * 用法：
 *   node scripts/check-all.cjs          # 全跑
 *   node scripts/check-all.cjs --fast   # 跳过 Rust 测试（只查结构，约 10 秒）
 */
const { spawnSync } = require("node:child_process");
const path = require("node:path");

const AZ = path.resolve(__dirname, "..");
const WEB = path.join(AZ, "apps", "web");
const fast = process.argv.includes("--fast");

const results = [];

function run(label, cmd, args, cwd) {
  process.stdout.write(`\n=== ${label} ===\n`);
  const r = spawnSync(cmd, args, {
    cwd,
    encoding: "buffer",
    timeout: 20 * 60 * 1000,
  });
  const out = (r.stdout || Buffer.alloc(0)).toString("utf8");
  const err = (r.stderr || Buffer.alloc(0)).toString("utf8");
  const ok = r.status === 0;
  // 只回显尾部，避免刷屏
  const tail = (out + err).split(/\r?\n/).filter(Boolean).slice(-12);
  for (const line of tail) console.log(`  ${line}`);
  results.push({ label, ok, status: r.status });
  return ok;
}

const nodeExe = process.execPath;

// 1. 分层
run("分层依赖方向", nodeExe, [path.join(AZ, "scripts", "check-layering.cjs")], AZ);

// 2. Rust 测试
if (!fast) {
  run(
    "Rust 测试（core / dsp / pipeline）",
    nodeExe,
    [
      path.join(AZ, "scripts", "cargo-run.cjs"),
      "test",
      "-p",
      "azzer-core",
      "-p",
      "azzer-dsp",
      "-p",
      "azzer-pipeline",
      "-p",
      "azzer-api",
    ],
    AZ
  );
} else {
  results.push({ label: "Rust 测试", ok: true, status: "skipped(--fast)" });
}

// 3. 前端类型检查
run(
  "前端类型检查",
  nodeExe,
  [path.join(WEB, "node_modules", "typescript", "bin", "tsc"), "--noEmit"],
  WEB
);

// 4. 前端构建
run(
  "前端生产构建",
  nodeExe,
  [path.join(WEB, "node_modules", "vite", "bin", "vite.js"), "build"],
  WEB
);

console.log("\n=== 汇总 ===");
let bad = 0;
for (const r of results) {
  const mark = r.ok ? "PASS" : "FAIL";
  if (!r.ok) bad++;
  console.log(`  ${mark}  ${r.label}${r.ok ? "" : `（exit=${r.status}）`}`);
}
console.log(
  bad === 0
    ? "\nRESULT: 全部通过"
    : `\nRESULT: ${bad} 项未通过，按上面第一个 FAIL 的日志往下查`
);
process.exit(bad === 0 ? 0 : 1);
