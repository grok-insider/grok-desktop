import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";

const assetDirectory = dirname(fileURLToPath(import.meta.url));
const sizes = [16, 20, 24, 32];
const themes = [
  { name: "dark", fill: "#ffffff" },
  { name: "light", fill: "#252d29" },
];
const pngSignature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function paeth(left, up, upperLeft) {
  const estimate = left + up - upperLeft;
  const leftDistance = Math.abs(estimate - left);
  const upDistance = Math.abs(estimate - up);
  const upperLeftDistance = Math.abs(estimate - upperLeft);
  if (leftDistance <= upDistance && leftDistance <= upperLeftDistance) return left;
  return upDistance <= upperLeftDistance ? up : upperLeft;
}

function inspectPng(buffer, expectedSize, label) {
  assert(buffer.subarray(0, 8).equals(pngSignature), `${label}: invalid PNG signature`);

  let offset = 8;
  let width;
  let height;
  let bitDepth;
  let colorType;
  let interlace;
  const imageData = [];

  while (offset < buffer.length) {
    const length = buffer.readUInt32BE(offset);
    const type = buffer.toString("ascii", offset + 4, offset + 8);
    const data = buffer.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      interlace = data[12];
    } else if (type === "IDAT") {
      imageData.push(data);
    }
    offset += length + 12;
  }

  assert(width === expectedSize && height === expectedSize, `${label}: expected ${expectedSize}x${expectedSize}, got ${width}x${height}`);
  assert(bitDepth === 8 && colorType === 6, `${label}: expected 8-bit RGBA PNG`);
  assert(interlace === 0, `${label}: interlaced PNGs are not supported`);

  const bytesPerPixel = 4;
  const rowBytes = width * bytesPerPixel;
  const inflated = inflateSync(Buffer.concat(imageData));
  assert(inflated.length === height * (rowBytes + 1), `${label}: unexpected decoded size`);

  const previous = Buffer.alloc(rowBytes);
  let sourceOffset = 0;
  let minimumAlpha = 255;
  let maximumAlpha = 0;

  for (let row = 0; row < height; row += 1) {
    const filter = inflated[sourceOffset];
    sourceOffset += 1;
    const current = Buffer.alloc(rowBytes);

    for (let column = 0; column < rowBytes; column += 1) {
      const encoded = inflated[sourceOffset + column];
      const left = column >= bytesPerPixel ? current[column - bytesPerPixel] : 0;
      const up = previous[column];
      const upperLeft = column >= bytesPerPixel ? previous[column - bytesPerPixel] : 0;
      let predictor;
      if (filter === 0) predictor = 0;
      else if (filter === 1) predictor = left;
      else if (filter === 2) predictor = up;
      else if (filter === 3) predictor = Math.floor((left + up) / 2);
      else if (filter === 4) predictor = paeth(left, up, upperLeft);
      else throw new Error(`${label}: unsupported PNG filter ${filter}`);
      current[column] = (encoded + predictor) & 0xff;
    }

    for (let alpha = 3; alpha < rowBytes; alpha += bytesPerPixel) {
      minimumAlpha = Math.min(minimumAlpha, current[alpha]);
      maximumAlpha = Math.max(maximumAlpha, current[alpha]);
    }
    current.copy(previous);
    sourceOffset += rowBytes;
  }

  assert(minimumAlpha === 0 && maximumAlpha === 255, `${label}: expected transparent and opaque pixels`);
}

function inspectIco(buffer, label) {
  assert(buffer.readUInt16LE(0) === 0 && buffer.readUInt16LE(2) === 1, `${label}: invalid ICO header`);
  const count = buffer.readUInt16LE(4);
  assert(count === sizes.length, `${label}: expected ${sizes.length} images, got ${count}`);
  const dimensions = [];

  for (let index = 0; index < count; index += 1) {
    const entry = 6 + index * 16;
    const width = buffer[entry] || 256;
    const height = buffer[entry + 1] || 256;
    const bitsPerPixel = buffer.readUInt16LE(entry + 6);
    const byteLength = buffer.readUInt32LE(entry + 8);
    const imageOffset = buffer.readUInt32LE(entry + 12);
    assert(width === height, `${label}: non-square ${width}x${height} entry`);
    assert(bitsPerPixel === 32, `${label}: ${width}px entry is not 32-bit`);
    dimensions.push(width);

    const image = buffer.subarray(imageOffset, imageOffset + byteLength);
    if (image.subarray(0, 8).equals(pngSignature)) {
      inspectPng(image, width, `${label}:${width}`);
      continue;
    }

    const headerSize = image.readUInt32LE(0);
    const dibWidth = image.readInt32LE(4);
    const dibHeight = Math.abs(image.readInt32LE(8)) / 2;
    const dibBitsPerPixel = image.readUInt16LE(14);
    assert(headerSize >= 40 && dibWidth === width && dibHeight === height, `${label}:${width}: invalid DIB dimensions`);
    assert(dibBitsPerPixel === 32, `${label}:${width}: expected 32-bit DIB`);
    const pixels = image.subarray(headerSize, headerSize + width * height * 4);
    let minimumAlpha = 255;
    let maximumAlpha = 0;
    for (let alpha = 3; alpha < pixels.length; alpha += 4) {
      minimumAlpha = Math.min(minimumAlpha, pixels[alpha]);
      maximumAlpha = Math.max(maximumAlpha, pixels[alpha]);
    }
    assert(minimumAlpha === 0 && maximumAlpha === 255, `${label}:${width}: expected transparent and opaque pixels`);
  }

  assert(dimensions.toSorted((a, b) => a - b).join(",") === sizes.join(","), `${label}: unexpected dimensions ${dimensions.join(",")}`);
}

for (const theme of themes) {
  const svgName = `tray-${theme.name}.svg`;
  const svg = await readFile(join(assetDirectory, svgName), "utf8");
  assert(svg.includes('viewBox="0 0 32 32"'), `${svgName}: expected a 32x32 viewBox`);
  assert(svg.includes(`fill="${theme.fill}"`), `${svgName}: expected ${theme.fill} glyph fill`);
  assert(!/<(?:rect|image)\b/.test(svg), `${svgName}: tray source must not contain a background`);
  assert((svg.match(/<path\b/g) ?? []).length === 1, `${svgName}: expected one glyph path`);

  for (const size of sizes) {
    const name = `tray-${theme.name}-${size}.png`;
    inspectPng(await readFile(join(assetDirectory, name)), size, name);
  }

  const icoName = `tray-${theme.name}.ico`;
  inspectIco(await readFile(join(assetDirectory, icoName)), icoName);
}

console.log("Validated tray SVG, PNG, and ICO assets (theme, dimensions, and alpha).");
