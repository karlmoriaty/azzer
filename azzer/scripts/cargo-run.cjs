// Cargo 调用包装（Node 版）
//
// 为什么不用 PowerShell 直接调：cargo 把进度与编译信息写 stderr，
// PowerShell 在 2>&1 时会把 stderr 当作 NativeCommandError 抛出，
// 即使 ErrorActionPreference=Continue 也可能中断脚本。
// Node 的 spawnSync 用 buffer 捕获，完全绕开这个问题，也不经过控制台编码。
//
// 用法：
//   node scripts/cargo-run.cjs test
//   node scripts/cargo-run.cjs check
//   node scripts/cargo-run.cjs build --release
//
// 退出码 = cargo 的退出码；输出原样打到 stdout/stderr。

const { spawnSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

const ROOT = path.resolve(__dirname, "..");
const BT = "C:\\Program Files (x86)\\Microsoft Visual Studio\\2022\\BuildTools";
const SDK = "C:\\Program Files (x86)\\Windows Kits\\10";

function latestDir(parent, parse) {
  const items = fs
    .readdirSync(parent, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => d.name);
  if (items.length === 0) throw new Error(`空目录：${parent}`);
  items.sort((a, b) => parse(b) - parse(a));
  return items[0];
}

const ver = (s) => {
  const m = s.match(/\d+(\.\d+)*/);
  return m ? Number(m[0].split(".").map((n) => n.padStart(4, "0")).join("")) : 0;
};

const msvcVer = latestDir(path.join(BT, "VC", "Tools", "MSVC"), ver);
const sdkVer = latestDir(path.join(SDK, "Lib"), ver);
const msvcRoot = path.join(BT, "VC", "Tools", "MSVC", msvcVer);

const env = { ...process.env };
env.PATH = [
  path.join(msvcRoot, "bin", "Hostx64", "x64"),
  path.join(SDK, "bin", sdkVer, "x64"),
  env.PATH || "",
].join(";");
env.INCLUDE = [
  path.join(msvcRoot, "include"),
  path.join(SDK, "Include", sdkVer, "ucrt"),
  path.join(SDK, "Include", sdkVer, "um"),
  path.join(SDK, "Include", sdkVer, "shared"),
  path.join(SDK, "Include", sdkVer, "winrt"),
].join(";");
env.LIB = [
  path.join(msvcRoot, "lib", "x64"),
  path.join(SDK, "Lib", sdkVer, "ucrt", "x64"),
  path.join(SDK, "Lib", sdkVer, "um", "x64"),
].join(";");
// 所有编译产物与中间文件都放在工作目录内，不污染系统盘或其他盘。
// 工作目录：本脚本上两级（跟随项目所在盘，不写死）
const WORKSPACE = path.resolve(ROOT, "..");
env.CARGO_TARGET_DIR = env.CARGO_TARGET_DIR || path.join(WORKSPACE, ".build", "cargo-target");

// 工作目录所在的卷是 FAT32，不具备 NTFS 的原子重命名与并发写入语义，
// rustc 多线程写 deps/ 会报 "拒绝访问 (os error 5)"。
// 关掉增量编译并单线程，实测可稳定编译（代价是全量重编约 7 分钟）。
// 若把 .build 换到 NTFS 盘，可以设 CARGO_INCREMENTAL=1 CARGO_BUILD_JOBS=8 提速。
env.CARGO_INCREMENTAL = env.CARGO_INCREMENTAL || "0";
env.CARGO_BUILD_JOBS = env.CARGO_BUILD_JOBS || "1";

// ort rc.9 的默认 feature 仍含 download-binaries，build script 会去 GitHub
// 拉 onnxruntime 二进制并无限期挂住（本机不可达）。我们用 load-dynamic
// 只加载项目内的 dll，所以明确跳过下载。
// 依据：ort-sys build.rs 的 ORT_SKIP_DOWNLOAD（值 "1" 或 "true" 生效）。
env.ORT_SKIP_DOWNLOAD = env.ORT_SKIP_DOWNLOAD || "1";

// onnxruntime 用 load-dynamic 模式：编译期不下二进制（本机访问 GitHub 不可达），
// 运行时按 ORT_DYLIB_PATH 加载。dll 由 scripts/fetch-ort.cjs 从 pip 源取。
env.ORT_DYLIB_PATH =
  env.ORT_DYLIB_PATH || path.join(ROOT, "native", "onnxruntime", "onnxruntime.dll");

// cargo 的位置因机器而异，不写死：先看 CARGO 环境变量，再试常见安装位置，
// 最后交给 PATH。原先硬编码了一条本机迁移后的绝对路径，换台机器就跑不起来。
function findCargo() {
  if (process.env.CARGO && fs.existsSync(process.env.CARGO)) return process.env.CARGO;
  const home = process.env.USERPROFILE || process.env.HOME || "";
  const exe = process.platform === "win32" ? "cargo.exe" : "cargo";
  const candidates = [
    home ? path.join(home, ".cargo", "bin", exe) : null,
    process.platform === "win32" ? "C:\\Program Files\\Rust\\bin\\cargo.exe" : "/usr/local/bin/cargo",
  ].filter(Boolean);
  for (const c of candidates) {
    if (fs.existsSync(c)) return c;
  }
  return "cargo";
}
const cargo = findCargo();
const args = process.argv.slice(2);
if (args.length === 0) args.push("build");

if (process.env.AZZER_VERBOSE === "1") {
  console.log(`MSVC=${msvcVer} SDK=${sdkVer} TARGET=${env.CARGO_TARGET_DIR}`);
}

const r = spawnSync(cargo, args, {
  cwd: ROOT,
  env,
  encoding: "buffer",
  stdio: ["inherit", "pipe", "pipe"],
  maxBuffer: 64 * 1024 * 1024,
});

if (r.error) {
  console.error("spawn 失败：", r.error.message);
  process.exit(127);
}
if (r.stdout && r.stdout.length) process.stdout.write(r.stdout);
if (r.stderr && r.stderr.length) process.stderr.write(r.stderr);
process.exit(r.status === null ? 1 : r.status);
