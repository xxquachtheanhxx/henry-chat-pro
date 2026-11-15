import fs from "fs";
import path from "path";

// Tạo các file icon còn thiếu. Icon thật nằm trong src-tauri/icons/ (xem scripts/make-icons.ps1)
// nên script này KHÔNG ghi đè icon đã có.
const iconsDir = path.join("src-tauri", "icons");
fs.mkdirSync(iconsDir, { recursive: true });

// 1x1 Pixel PNG hợp lệ (màu xanh dương) dưới dạng Base64
const pngBase64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
const pngBuffer = Buffer.from(pngBase64, "base64");

const icoHeader = Buffer.alloc(6);
icoHeader.writeUInt16LE(0, 0); // Reserved
icoHeader.writeUInt16LE(1, 2); // Type: 1 = ICO
icoHeader.writeUInt16LE(1, 4); // Count: 1 image

const icoDirEntry = Buffer.alloc(16);
icoDirEntry.writeUInt8(32, 0); // Width
icoDirEntry.writeUInt8(32, 1); // Height
icoDirEntry.writeUInt8(0, 2); // Color palette
icoDirEntry.writeUInt8(0, 3); // Reserved
icoDirEntry.writeUInt16LE(1, 4); // Color planes
icoDirEntry.writeUInt16LE(32, 6); // Bits per pixel
icoDirEntry.writeUInt32LE(pngBuffer.length, 8); // Size of image data
icoDirEntry.writeUInt32LE(22, 12); // Offset (6 + 16 = 22)

const icoBuffer = Buffer.concat([icoHeader, icoDirEntry, pngBuffer]);

const targets = [
  ["32x32.png", pngBuffer],
  ["128x128.png", pngBuffer],
  ["128x128@2x.png", pngBuffer],
  ["icon.icns", pngBuffer],
  ["icon.ico", icoBuffer],
];

let created = 0;
for (const [name, data] of targets) {
  const file = path.join(iconsDir, name);
  if (fs.existsSync(file)) continue;
  fs.writeFileSync(file, data);
  created += 1;
}

console.log(
  created > 0
    ? `✅ Đã tạo ${created} file icon còn thiếu trong src-tauri/icons/`
    : "✅ Icon đã đầy đủ, không ghi đè."
);
