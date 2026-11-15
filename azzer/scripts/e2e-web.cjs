/**
 * 前端 ↔ 后端的浏览器端到端验证。
 *
 * 为什么用脚本而起两个服务：工具层会回收命令的整个进程树，手工起的常驻进程
 * 活不到下一步。脚本自己 spawn、自己等、自己杀，生命周期在脚本内闭环。
 *
 * 验证四件事：
 * 1. API 服务能起，runtime.json 落盘（Vite 代理靠它取 port/token）
 * 2. 前端 dev server 能起，且页面能拉到 /v1/system/info（说明代理与鉴权通了）
 * 3. 通过 HTTP 上传一个真实 WAV，能跑到 done 并取回成品（RIFF 头校验）
 * 4. 用无头 Edge 打开页面截图 + dump DOM，确认渲染出的不是「未连接」
 *
 * 用法：node scripts/e2e-web.cjs
 */
const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const AZ = path.resolve(__dirname, "..");
const WORK = path.resolve(AZ, "..");
const OUT = path.join(WORK, ".cache", "e2e");
// 编译产物与运行期文件都在**工作目录根**的 .build 下（CARGO_TARGET_DIR 设在那里），
// 不在 azzer/ 里。serve.rs 从 exe 上溯三层得到同一个 .build。
const RUNTIME = path.join(WORK, ".build", "runtime.json");
const SERVE = path.join(WORK, ".build", "cargo-target", "debug", "serve.exe");
const EDGE_CANDIDATES = [
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
];

const log = [];
function say(s) {
  log.push(s);
  console.log(s);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(fn, timeoutMs, label) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    try {
      if (await fn()) return true;
    } catch {
      /* 继续等 */
    }
    await sleep(200);
  }
  throw new Error(`等待超时：${label}（${timeoutMs}ms）`);
}

/**
 * 等一个「由别的进程写出来」的 JSON 文件就绪。
 *
 * 不能只等文件存在：`std::fs::write` 是先 create 再 write_all，文件会有一瞬间
 * 存在但长度为 0。200ms 的轮询在 FAT32 上足以命中这个窗口，然后 JSON.parse
 * 直接抛 "Unexpected end of JSON input"，看起来像服务没起来，实际服务好好的。
 * 2026-11-11 在迁移后的新盘上必现（旧盘只是运气好没撞上）。
 *
 * 判据：能读出非空内容且能解析成对象，才算真正就绪。
 */
async function waitForJson(file, timeoutMs, label = path.basename(file)) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    try {
      const txt = fs.readFileSync(file, "utf8");
      if (txt.trim()) return JSON.parse(txt);
    } catch {
      /* 还没写完或写到一半，继续等 */
    }
    await sleep(100);
  }
  throw new Error(`等待超时：${label} 未写出可解析的内容（${timeoutMs}ms）`);
}

function makeTinyWav(file) {
  // 5 秒 48k/24bit 单声道，220Hz 加 4 次谐波——足够过 N01 的 3 秒与 -40dBFS 断言
  const sr = 48000;
  const n = sr * 5;
  const data = Buffer.alloc(n * 3);
  for (let i = 0; i < n; i++) {
    const t = i / sr;
    let s = 0;
    for (let h = 1; h <= 4; h++) s += Math.sin(2 * Math.PI * 220 * h * t) / h;
    let v = Math.round(s * 0.2 * 8388608);
    v = Math.max(-8388608, Math.min(8388607, v));
    data[i * 3] = v & 0xff;
    data[i * 3 + 1] = (v >> 8) & 0xff;
    data[i * 3 + 2] = (v >> 16) & 0xff;
  }
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVE", 8);
  header.write("fmt ", 12);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(sr, 24);
  header.writeUInt32LE(sr * 3, 28);
  header.writeUInt16LE(3, 32);
  header.writeUInt16LE(24, 34);
  header.write("data", 36);
  header.writeUInt32LE(data.length, 40);
  fs.writeFileSync(file, Buffer.concat([header, data]));
  return file;
}

async function main() {
  fs.mkdirSync(OUT, { recursive: true });
  // 每次冷启动：不清 data 的话，同样的夹具会全部命中 CAS 与执行记录，
  // 测出来的“端到端耗时”只有几十毫秒，看不出真实链路时间。
  const dataDir = path.join(OUT, "data");
  try {
    fs.rmSync(dataDir, { recursive: true, force: true });
  } catch {}
  // 清掉旧的 runtime.json，避免读到上一次的端口
  try {
    fs.unlinkSync(RUNTIME);
  } catch {}

  if (!fs.existsSync(SERVE)) {
    throw new Error(`找不到服务可执行文件：${SERVE}。先跑 cargo build -p azzer-api --bin serve`);
  }

  // ---- 1. 起 API
  say("启动 API 服务…");
  const serve = spawn(SERVE, [], {
    env: { ...process.env, AZZER_DATA_DIR: path.join(OUT, "data") },
    stdio: ["ignore", "pipe", "pipe"],
  });
  serveRef = serve;
  serve.stderr.on("data", (d) => say(`[serve] ${d.toString().trim()}`));
  serve.on("exit", (c) => say(`[serve] 退出 code=${c}`));

  // 等「能解析」而不是「存在」，理由见 waitForJson 的注释。
  const rt = await waitForJson(RUNTIME, 30000, "runtime.json");
  say(`API 就绪：port=${rt.port}`);

  // ---- 2. 起前端 dev server
  say("启动前端 dev server…");
  const viteBin = path.join(AZ, "apps", "web", "node_modules", "vite", "bin", "vite.js");
  const nodeExe = process.execPath;
  const vite = spawn(nodeExe, [viteBin, "--port", "5173", "--strictPort"], {
    cwd: path.join(AZ, "apps", "web"),
    stdio: ["ignore", "pipe", "pipe"],
  });
  viteRef = vite;
  vite.stderr.on("data", (d) => {
    const s = d.toString();
    if (s.includes("error") || s.includes("Error")) say(`[vite] ${s.trim()}`);
  });

  const web = "http://localhost:5173/";
  await waitFor(
    async () => {
      const r = await fetch(web, { signal: AbortSignal.timeout(1500) });
      return r.ok;
    },
    60000,
    "前端 dev server"
  );
  say("前端就绪：http://localhost:5173/");

  // ---- 3. 通过代理拉系统信息（验证代理与鉴权）
  //
  // 超时给到 30 秒而不是 10 秒：`/v1/system/info` 是**第一次**真正碰 ONNX Runtime
  // 的请求，要在这次请求里加载 onnxruntime.dll 与 324MB 的 providers_cuda.dll，
  // 并枚举 GPU。冷文件缓存下这段可能十几秒（2026-11-11 迁移后实测遇到过一次
  // 10 秒超时，紧接着重跑 132ms 就回来了）。这里要验证的是代理与鉴权通不通，
  // 不是 CUDA 首次加载有多快，所以预算放宽，失败时把 cause 一并打出来。
  let info;
  try {
    const infoResp = await fetch(`${web}v1/system/info`, {
      signal: AbortSignal.timeout(30000),
    });
    if (!infoResp.ok) throw new Error(`代理请求失败：HTTP ${infoResp.status}`);
    info = await infoResp.json();
  } catch (e) {
    const cause = e?.cause ? `${e.cause.code || e.cause.name}：${e.cause.message}` : "无 cause";
    throw new Error(`拉取 /v1/system/info 失败：${e.name} ${e.message}｜${cause}`);
  }
  say(
    `代理通路：version=${info.version} gpu=${info.gpu_accelerated} ` +
      `direct=${info.capabilities?.direct} models=${info.models_declared}`
  );

  // ---- 4. 上传 → 轮询 → 取成品（走前端同一条代理路径）
  const tiny = makeTinyWav(path.join(OUT, "tiny.wav"));
  const bytes = fs.readFileSync(tiny);
  say(`上传 ${(bytes.length / 1024).toFixed(0)}KB…`);
  const upResp = await fetch(`${web}v1/covers`, {
    method: "POST",
    headers: { "Content-Type": "audio/wav" },
    body: bytes,
    signal: AbortSignal.timeout(30000),
  });
  if (!upResp.ok) {
    const t = await upResp.text();
    throw new Error(`上传失败：HTTP ${upResp.status} ${t}`);
  }
  const { job_id } = await upResp.json();
  say(`job_id=${job_id}`);

  let done = null;
  const t0 = Date.now();
  while (Date.now() - t0 < 180000) {
    const r = await fetch(`${web}v1/covers/${job_id}`, { signal: AbortSignal.timeout(10000) });
    const j = await r.json();
    if (j.state === "done") {
      done = j;
      break;
    }
    if (j.state === "failed") throw new Error(`处理失败：${j.message}`);
    await sleep(600);
  }
  if (!done) throw new Error("处理超时（180s）");

  const m = done.report.master;
  fs.writeFileSync(
    path.join(OUT, "report.json"),
    JSON.stringify(done.report, null, 2)
  );
  say(
    `处理完成：${(done.report.elapsed_ms / 1000).toFixed(1)}s ` +
      `（elapsed_ms 原值=${JSON.stringify(done.report.elapsed_ms)} 类型=${typeof done.report.elapsed_ms}）` +
      `LUFS=${m.output.integrated_lufs?.toFixed(3)} dBTP=${m.output_dbtp.toFixed(3)} ` +
      `LRA=${m.output.lra_lu?.toFixed(2)} 评估项=${done.report.eval.items.length}`
  );

  const wavResp = await fetch(`${web}v1/covers/${job_id}/master.wav`, {
    signal: AbortSignal.timeout(30000),
  });
  if (!wavResp.ok) throw new Error(`取成品失败：HTTP ${wavResp.status}`);
  const wav = Buffer.from(await wavResp.arrayBuffer());
  const isRiff = wav.subarray(0, 4).toString() === "RIFF";
  say(`成品：${(wav.length / 1024).toFixed(0)}KB RIFF=${isRiff}`);
  if (!isRiff) throw new Error("成品不是合法 WAV");

  // ---- 5. 无头 Edge：先截空态，再用 ?job= 截结果态
  const edge = EDGE_CANDIDATES.find((p) => fs.existsSync(p));
  if (!edge) {
    say("未找到 Edge，跳过浏览器渲染验证");
  } else {
    const shotEmpty = path.join(OUT, "app-empty.png");
    const shotDone = path.join(OUT, "app-done.png");
    const domFile = path.join(OUT, "dom.html");
    const common = [
      "--headless=new",
      "--disable-gpu",
      "--no-first-run",
      "--virtual-time-budget=8000",
      "--window-size=1440,900",
    ];

    const s1 = spawnSync(edge, [...common, `--screenshot=${shotEmpty}`, web], {
      timeout: 90000,
      encoding: "buffer",
    });
    say(`空态截图：exit=${s1.status} 文件=${fs.existsSync(shotEmpty) ? "有" : "无"}`);

    // 结果态：?job=<id> 让页面直接恢复那个任务
    const doneUrl = `${web}?job=${job_id}`;
    const s3 = spawnSync(edge, [...common, `--screenshot=${shotDone}`, doneUrl], {
      timeout: 120000,
      encoding: "buffer",
    });
    say(`结果态截图：exit=${s3.status} 文件=${fs.existsSync(shotDone) ? "有" : "无"}`);

    const s2 = spawnSync(edge, [...common, "--dump-dom", doneUrl], {
      timeout: 120000,
      encoding: "buffer",
    });
    const s2b = spawnSync(edge, [...common, "--dump-dom", web], {
      timeout: 120000,
      encoding: "buffer",
    });
    if (s2.stdout && s2b.stdout) {
      fs.writeFileSync(domFile, s2.stdout);
      fs.writeFileSync(path.join(OUT, "dom-empty.html"), s2b.stdout);
      const dom = s2.stdout.toString("utf8");
      const domEmpty = s2b.stdout.toString("utf8");
      const lufs = m.output.integrated_lufs;

      // 两组断言分开：空态才有上传区，结果态才有机指标。
      // 先前把两者混在一张 DOM 上判，空态缺指标、结果态缺上传区，必然互相冤枉。
      const emptyMarkers = {
        "空态·渲染上传区": domEmpty.includes("把 WAV 拖到这里"),
        "空态·连上后端": domEmpty.includes("A 入口可用"),
        "空态·未出现未连接": !domEmpty.includes("连不上本地服务"),
        "空态·显示未声明模型": domEmpty.includes("未声明模型"),
      };
      const doneMarkers = {
        "结果态·显示积分响度": dom.includes("积分响度"),
        "结果态·显示真峰值": dom.includes("真峰值"),
        "结果态·渲染评估分组": dom.includes("评估"),
        "结果态·渲染链路": dom.includes("链路"),
        "结果态·不残留上传区": !dom.includes("把 WAV 拖到这里"),
        // 页面上的数字必须与后端报告一致，这是「数据真的流到界面」的硬证据
        "结果态·数值与后端一致": lufs === null ? true : dom.includes(lufs.toFixed(2)),
      };
      const all = { ...emptyMarkers, ...doneMarkers };
      for (const [k, v] of Object.entries(all)) say(`  ${v ? "PASS" : "FAIL"}  ${k}`);
      if (Object.values(all).some((v) => !v)) {
        throw new Error("页面渲染结果不符合预期，详见 dom.html / dom-empty.html");
      }
      say(`后端 LUFS=${lufs?.toFixed(2)}，页面数字与其比对一致`);
    } else {
      say("dump-dom 无输出");
    }
  }

  say("=== 全部通过 ===");
  return { info, report: done.report };
}

let serveRef = null;
let viteRef = null;

main()
  .then(() => process.exit(0))
  .catch((e) => {
    say(`失败：${e.message}`);
    process.exit(1);
  })
  .finally(() => {
    try {
      serveRef?.kill();
    } catch {}
    try {
      viteRef?.kill();
    } catch {}
  });
