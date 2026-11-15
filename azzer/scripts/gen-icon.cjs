// 生成应用图标（PNG + ICO），供 Tauri 打包使用。
//
// 为什么不引 sharp/canvas：本机没有这些原生依赖，且在 Windows 上编译很麻烦。
// 图标是纯几何图形，手写 PNG 编码（含 zlib 压缩与 CRC32）就够了，零依赖。
//
// 图形：深色圆角方块 + 暖石色波形（五根竖条，中间最高）。
// 这既是品牌标识，也点明产品是音频工具。
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

function draw(size) {
  const buf = Buffer.alloc(size * size * 4);
  const r = size * 0.22; // 圆角半径
  const bars = 5;
  const gapRatio = 0.06;
  const barW = (size * 0.62) / bars;
  const gap = size * gapRatio;
  const totalW = bars * barW + (bars - 1) * gap;
  const x0 = (size - totalW) / 2;
  // 每根竖条的高度比例（中间最高，模拟波形）
  const heights = [0.34, 0.62, 0.9, 0.55, 0.3];
  const cy = size / 2;

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4;

      // 圆角矩形外 → 透明
      const inCornerX = x < r ? r - x : x > size - 1 - r ? x - (size - 1 - r) : 0;
      const inCornerY = y < r ? r - y : y > size - 1 - r ? y - (size - 1 - r) : 0;
      if (inCornerX > 0 && inCornerY > 0) {
        if (inCornerX * inCornerX + inCornerY * inCornerY > r * r) continue;
      }

      let color = BG;

      for (let b = 0; b < bars; b++) {
        const bx = x0 + b * (barW + gap);
        if (x >= bx && x < bx + barW) {
          const h = size * heights[b];
          if (y >= cy - h / 2 && y <= cy + h / 2) color = ACCENT;
        }
      }

      buf[i] = color[0];
      buf[i + 1] = color[1];
      buf[i + 2] = color[2];
      buf[i + 3] = color[3];
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
