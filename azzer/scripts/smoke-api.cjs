// API 冒烟测试
//
// 验证四件事：
//   1. /health 无需 token 可访问
//   2. /v1/system/info 不带 token 返回 401
//   3. 带错误 token 返回 401
//   4. 带正确 token 返回 200，且 runtime.providers 非空
//
// 用法：
//   node scripts/smoke-api.cjs
//   AZZER_RUNTIME_FILE=D:\path\runtime.json node scripts/smoke-api.cjs
//
// 退出码：0 全过，1 有失败。

const path = require("node:path");
const fs = require("node:fs");

// 运行期文件在 <工作目录>/.build/runtime.json，由 serve.rs 推导写入
const RUNTIME =
  process.env.AZZER_RUNTIME_FILE ||
  path.resolve(__dirname, "..", "..", ".build", "runtime.json");

function fail(msg) {
  console.error(`  ✗ ${msg}`);
  process.exitCode = 1;
  return false;
}

function pass(msg) {
  console.log(`  ✓ ${msg}`);
  return true;
}

async function jsonOrText(resp) {
  const text = await resp.text();
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

(async () => {
  if (!fs.existsSync(RUNTIME)) {
    console.error(`找不到运行期文件：${RUNTIME}`);
    console.error("先启动服务：node scripts/cargo-run.cjs run -p azzer-api --bin serve");
    process.exit(1);
  }
  const { port, token } = JSON.parse(fs.readFileSync(RUNTIME, "utf8"));
  const base = `http://127.0.0.1:${port}`;
  console.log(`目标 ${base}`);

  const h = await fetch(`${base}/health`);
  const hb = await jsonOrText(h);
  h.status === 200 && hb && hb.ok
    ? pass(`/health 200 (${JSON.stringify(hb)})`)
    : fail(`/health 期望 200+ok，实得 ${h.status} ${JSON.stringify(hb)}`);

  const noAuth = await fetch(`${base}/v1/system/info`);
  const nb = await jsonOrText(noAuth);
  noAuth.status === 401
    ? pass(`无 token → 401 (${nb?.error?.code ?? "?"})`)
    : fail(`无 token 期望 401，实得 ${noAuth.status}`);

  const badAuth = await fetch(`${base}/v1/system/info`, {
    headers: { Authorization: "Bearer deadbeef" },
  });
  badAuth.status === 401
    ? pass("错误 token → 401")
    : fail(`错误 token 期望 401，实得 ${badAuth.status}`);

  const ok = await fetch(`${base}/v1/system/info`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  const body = await jsonOrText(ok);
  if (ok.status !== 200) {
    fail(`正确 token 期望 200，实得 ${ok.status} ${JSON.stringify(body)}`);
  } else {
    const providers = body?.runtime?.providers ?? [];
    providers.length > 0
      ? pass(`正确 token → 200，EP 共 ${providers.length} 个：${providers.map((p) => p.name).join(", ")}`)
      : fail("providers 为空");
    body?.runtime?.recommended
      ? pass(`推荐后端：${body.runtime.recommended}（GPU 加速=${body.gpu_accelerated}）`)
      : fail("recommended 缺失");
    body?.runtime?.init_error
      ? fail(`ORT 初始化报错：${body.runtime.init_error}`)
      : pass("ORT 初始化正常");
  }

  console.log(process.exitCode ? "\n有失败项" : "\n全部通过");
})();
