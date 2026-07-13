#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
const INTERNAL_DEPENDENCY = /^(grok-[a-z0-9-]+\s*=\s*\{[^\n]*\bpath\s*=\s*"[^"]+"[^\n]*\bversion\s*=\s*")[^"]+("[^\n]*\})$/gm;

export function assertVersion(version) {
  if (!SEMVER.test(version)) throw new Error(`invalid release version: ${version}`);
}

function replaceExactly(text, pattern, replacement, label) {
  const matches = [...text.matchAll(new RegExp(pattern.source, pattern.flags.includes("g") ? pattern.flags : `${pattern.flags}g`))];
  if (matches.length !== 1) throw new Error(`${label}: expected one version site, found ${matches.length}`);
  return text.replace(pattern, replacement);
}

function updateJsonVersion(path, version, check) {
  const original = readFileSync(path, "utf8");
  const value = JSON.parse(original);
  if (typeof value.version !== "string") throw new Error(`${path}: missing string version`);
  value.version = version;
  const updated = `${JSON.stringify(value, null, 2)}\n`;
  if (check && original !== updated) throw new Error(`${path}: version is not ${version}`);
  if (!check && original !== updated) writeFileSync(path, updated);
}

function updateText(path, transform, check) {
  const original = readFileSync(path, "utf8");
  const updated = transform(original);
  if (check && original !== updated) throw new Error(`${path}: release version drift`);
  if (!check && original !== updated) writeFileSync(path, updated);
}

export function syncReleaseVersion(root, version, { check = false, updateCargoLock = true } = {}) {
  assertVersion(version);
  updateJsonVersion(join(root, "package.json"), version, check);
  updateJsonVersion(join(root, "apps/desktop/package.json"), version, check);

  updateText(
    join(root, "Cargo.toml"),
    (text) => replaceExactly(
      text,
      /(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*$)/m,
      `$1${version}$2`,
      "Cargo workspace",
    ),
    check,
  );

  const crates = join(root, "crates");
  for (const entry of readdirSync(crates, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = join(crates, entry.name, "Cargo.toml");
    updateText(manifest, (text) => text.replace(INTERNAL_DEPENDENCY, `$1${version}$2`), check);
  }

  updateText(
    join(root, "flake.nix"),
    (text) => replaceExactly(
      text,
      /(pname\s*=\s*"grok-integration-runner";\s*\n\s*version\s*=\s*")[^"]+(";)/,
      `$1${version}$2`,
      "Nix package",
    ),
    check,
  );

  if (updateCargoLock && !check) {
    execFileSync("cargo", ["update", "--workspace"], { cwd: root, stdio: "inherit" });
  }
  if (updateCargoLock && check) {
    const lock = readFileSync(join(root, "Cargo.lock"), "utf8");
    const workspacePackages = [...lock.matchAll(/\[\[package\]\]\nname = "grok-[^"]+"\nversion = "([^"]+)"/g)];
    if (workspacePackages.length === 0 || workspacePackages.some((match) => match[1] !== version)) {
      throw new Error("Cargo.lock: workspace package version drift");
    }
  }
}

function parseArguments(argv) {
  const args = [...argv];
  const check = args.includes("--check");
  const skipCargoLock = args.includes("--skip-cargo-lock");
  const rootIndex = args.indexOf("--root");
  const root = rootIndex >= 0 ? resolve(args[rootIndex + 1] ?? "") : resolve(dirname(fileURLToPath(import.meta.url)), "..");
  if (rootIndex >= 0) args.splice(rootIndex, 2);
  const positional = args.filter((arg) => !arg.startsWith("--"));
  let version = positional[0];
  if (check && !version) version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
  if (!version) throw new Error("usage: sync-release-version.mjs <version> [--check] [--root PATH]");
  return { check, root, skipCargoLock, version };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const { check, root, skipCargoLock, version } = parseArguments(process.argv.slice(2));
    syncReleaseVersion(root, version, { check, updateCargoLock: !skipCargoLock });
    console.log(`release version ${version} is synchronized`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
