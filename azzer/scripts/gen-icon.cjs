// 生成应用图标（PNG + ICO），供 Tauri 打包使用。
//
// 为什么不引 sharp/canvas：本机没有这些原生依赖，且在 Windows 上编译很麻烦。
// 图标是纯几何图形，手写 PNG 编码（含 zlib 压缩与 CRC32）就够了，零依赖。
//
// 图形：深色圆角方块 + 品牌主标 01（A / Waveform）。
// 路径与 design/brand/azzer-mark.svg 是同一组数据，改标识时两边必须同步。
// 用「点到线段距离」把描边栅格化，天然带圆头，且 1px 内做覆盖度抗锯齿。
//
// 用法：node scripts/gen-icon.cjs

const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");

// ---------- PNG 编码 ----------

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crcBuf = Buffer.alloc(4);
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crcBuf]);
}

function encodePng(width, height, rgba) {
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type RGBA
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  // 每行前置 filter 字节 0（None）
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0;
    rgba.copy(raw, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  }
  const idat = zlib.deflateSync(raw, { level: 9 });

  return Buffer.concat([
    sig,
    chunk("IHDR", ihdr),
    chunk("IDAT", idat),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---------- 图形绘制 ----------

const BG = [0x0c, 0x0c, 0x0d, 255];
const ACCENT = [0xe8, 0xb2, 0x7d, 255];

// 品牌主标 01，与 design/brand/azzer-mark.svg 同源（120×120 坐标系）：
//   A 两笔   M27 94 L60 24 L93 94        描边 7
//   波形横杠 M43 70 Q48 64 53 70 T63 70 T73 70   描边 5
const A_LEGS = [
  [27, 94, 60, 24],
  [60, 24, 93, 94],
];

/** 采样一段二次贝塞尔。SVG 的 T 命令把上一段控制点关于终点作镜像。 */
function quad(p0, c, p1, steps = 12) {
  const pts = [];
  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const u = 1 - t;
    pts.push([
      u * u * p0[0] + 2 * u * t * c[0] + t * t * p1[0],
      u * u * p0[1] + 2 * u * t * c[1] + t * t * p1[1],
    ]);
  }
  return pts;
}

function buildWave() {
  const p0 = [43, 70];
  const c1 = [48, 64];
  const p1 = [53, 70];
  const c2 = [2 * p1[0] - c1[0], 2 * p1[1] - c1[1]]; // (58, 76)
  const p2 = [63, 70];
  const c3 = [2 * p2[0] - c2[0], 2 * p2[1] - c2[1]]; // (68, 64)
  const p3 = [73, 70];
  return [
    ...quad(p0, c1, p1),
    ...quad(p1, c2, p2).slice(1),
    ...quad(p2, c3, p3).slice(1),
  ];
}

const WAVE = buildWave();

function distToSeg(px, py, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const l2 = dx * dx + dy * dy;
  let t = l2 === 0 ? 0 : ((px - ax) * dx + (py - ay) * dy) / l2;
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  const qx = ax + t * dx - px;
  const qy = ay + t * dy - py;
  return Math.sqrt(qx * qx + qy * qy);
}

function draw(size) {
  const buf = Buffer.alloc(size * size * 4);
  const r = size * 0.22; // 圆角半径

  // 主标在 120 空间里高 70，映射到图标高度的 68%，四周留白
  const k = (size * 0.68) / 70;
  const X = (x) => size / 2 + (x - 60) * k;
  const Y = (y) => size / 2 + (y - 59) * k;
  const halfA = 3.5 * k;
  const halfW = 2.5 * k;
  const legs = A_LEGS.map(([a, b, c, d]) => [X(a), Y(b), X(c), Y(d)]);
  const wave = WAVE.map(([x, y]) => [X(x), Y(y)]);

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      // 圆角矩形外 → 透明
      const inCornerX = x < r ? r - x : x > size - 1 - r ? x - (size - 1 - r) : 0;
      const inCornerY = y < r ? r - y : y > size - 1 - r ? y - (size - 1 - r) : 0;
      if (inCornerX > 0 && inCornerY > 0) {
        if (inCornerX * inCornerX + inCornerY * inCornerY > r * r) continue;
      }

      const px = x + 0.5;
      const py = y + 0.5;
      let cov = 0;

      for (const [ax, ay, bx, by] of legs) {
        const c = Math.min(1, Math.max(0, halfA + 0.5 - distToSeg(px, py, ax, ay, bx, by)));
        if (c > cov) cov = c;
      }
      for (let i = 1; i < wave.length; i++) {
        const [ax, ay] = wave[i - 1];
        const [bx, by] = wave[i];
        const c = Math.min(1, Math.max(0, halfW + 0.5 - distToSeg(px, py, ax, ay, bx, by)));
        if (c > cov) cov = c;
      }

      const i = (y * size + x) * 4;
      buf[i] = BG[0] + (ACCENT[0] - BG[0]) * cov;
      buf[i + 1] = BG[1] + (ACCENT[1] - BG[1]) * cov;
      buf[i + 2] = BG[2] + (ACCENT[2] - BG[2]) * cov;
      buf[i + 3] = 255;
    }
  }
  return buf;
}

// ---------- ICO 封装（PNG 内嵌，Vista+ 支持） ----------

function encodeIco(entries) {
  // entries: [{ size, png: Buffer }]
  const count = entries.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2); // 1 = ICO
  header.writeUInt16LE(count, 4);

  const dirs = [];
  let offset = 6 + count * 16;
  for (const e of entries) {
    const d = Buffer.alloc(16);
    d[0] = e.size >= 256 ? 0 : e.size;
    d[1] = e.size >= 256 ? 0 : e.size;
    d[2] = 0; // palette
    d[3] = 0; // reserved
    d.writeUInt16LE(1, 4); // planes
    d.writeUInt16LE(32, 6); // bpp
    d.writeUInt32LE(e.png.length, 8);
    d.writeUInt32LE(offset, 12);
    dirs.push(d);
    offset += e.png.length;
  }

  return Buffer.concat([header, ...dirs, ...entries.map((e) => e.png)]);
}

// ---------- 主流程 ----------

const outDir = path.resolve(__dirname, "..", "src-tauri", "icons");
fs.mkdirSync(outDir, { recursive: true });

const sizes = [32, 128, 256];
const pngs = [];
for (const s of sizes) {
  const png = encodePng(s, s, draw(s));
  pngs.push({ size: s, png });
  const file = path.join(outDir, `${s}x${s}.png`);
  fs.writeFileSync(file, png);
  console.log(`写出 ${file}（${png.length} 字节）`);
}

const ico = encodeIco(pngs);
const icoPath = path.join(outDir, "icon.ico");
fs.writeFileSync(icoPath, ico);
console.log(`写出 ${icoPath}（${ico.length} 字节，含 ${sizes.join("/")} 三种尺寸）`);
