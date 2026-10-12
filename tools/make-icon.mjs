// Draws assets/icon.ico from code so the icon has no binary source to keep.
// Run with: node tools/make-icon.mjs
import { writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

const SIZES = [16, 20, 24, 32, 40, 48, 64, 256];
const SS = 4; // supersamples per axis

// Coverage of the icon at a point in a 0..1 square: returns [r, g, b, a].
function shade(x, y) {
  // Rounded square background.
  const r = 0.2, m = 0.03;
  const dx = Math.max(Math.abs(x - 0.5) - (0.5 - m - r), 0);
  const dy = Math.max(Math.abs(y - 0.5) - (0.5 - m - r), 0);
  if (Math.hypot(dx, dy) > r) return null;

  // Four white crop corners.
  const w = 0.075, len = 0.22, lo = 0.2, hi = 0.8;
  const inCorner = (cx, cy, sx, sy) => {
    const ux = (x - cx) * sx, uy = (y - cy) * sy;
    return (ux >= 0 && ux <= len && uy >= 0 && uy <= w) || (uy >= 0 && uy <= len && ux >= 0 && ux <= w);
  };
  if (inCorner(lo, lo, 1, 1) || inCorner(hi, lo, -1, 1) || inCorner(lo, hi, 1, -1) || inCorner(hi, hi, -1, -1)) {
    return [255, 255, 255, 255];
  }
  // Centre dot.
  if (Math.hypot(x - 0.5, y - 0.5) < 0.085) return [255, 255, 255, 255];

  // Blue, slightly lighter at the top.
  const t = y;
  return [Math.round(59 - 22 * t), Math.round(130 - 31 * t), Math.round(246 - 11 * t), 255];
}

function render(size) {
  const px = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const c = shade((x + (sx + 0.5) / SS) / size, (y + (sy + 0.5) / SS) / size);
          if (!c) continue;
          r += c[0]; g += c[1]; b += c[2]; a += 1;
        }
      }
      const i = (y * size + x) * 4;
      if (a) { px[i] = r / a; px[i + 1] = g / a; px[i + 2] = b / a; }
      px[i + 3] = Math.round((a / (SS * SS)) * 255);
    }
  }
  return px;
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
function png(size, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6; // 8-bit RGBA
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0)),
  ]);
}

const images = SIZES.map((s) => png(s, render(s)));
const header = Buffer.alloc(6 + 16 * SIZES.length);
header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(SIZES.length, 4);
let offset = header.length;
SIZES.forEach((s, i) => {
  const e = 6 + i * 16;
  header[e] = s >= 256 ? 0 : s; header[e + 1] = s >= 256 ? 0 : s;
  header.writeUInt16LE(1, e + 4); header.writeUInt16LE(32, e + 6);
  header.writeUInt32LE(images[i].length, e + 8); header.writeUInt32LE(offset, e + 12);
  offset += images[i].length;
});
writeFileSync(new URL('../assets/icon.ico', import.meta.url), Buffer.concat([header, ...images]));
writeFileSync(new URL('../assets/icon.png', import.meta.url), images[SIZES.length - 1]);
console.log('wrote assets/icon.ico and assets/icon.png');
