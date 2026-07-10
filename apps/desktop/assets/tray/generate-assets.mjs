import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const assetDirectory = dirname(fileURLToPath(import.meta.url));
const sizes = [16, 20, 24, 32];
const themes = ["dark", "light"];

function runMagick(arguments_) {
  const result = spawnSync("magick", arguments_, {
    cwd: assetDirectory,
    encoding: "utf8",
    env: { ...process.env, SOURCE_DATE_EPOCH: "0" },
  });

  if (result.error) {
    throw new Error(`ImageMagick is required: ${result.error.message}`);
  }

  if (result.status !== 0) {
    throw new Error(result.stderr.trim() || `ImageMagick exited with ${result.status}`);
  }
}

for (const theme of themes) {
  const source = join(assetDirectory, `tray-${theme}.svg`);
  const pngs = [];

  for (const size of sizes) {
    const output = join(assetDirectory, `tray-${theme}-${size}.png`);
    pngs.push(output);
    runMagick([
      "-background",
      "none",
      "-density",
      "384",
      source,
      "-filter",
      "Lanczos",
      "-resize",
      `${size}x${size}`,
      "-gravity",
      "center",
      "-extent",
      `${size}x${size}`,
      "-alpha",
      "on",
      "-colorspace",
      "sRGB",
      "-strip",
      "-define",
      "png:color-type=6",
      "-define",
      "png:exclude-chunk=date,time",
      `PNG32:${output}`,
    ]);
  }

  runMagick([
    ...pngs,
    "-alpha",
    "on",
    "-colorspace",
    "sRGB",
    "-strip",
    join(assetDirectory, `tray-${theme}.ico`),
  ]);
}

console.log("Generated tray PNG and ICO assets with ImageMagick 7.");
