import { readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { parseArgs } from "node:util";
import { sha256File } from "./release-utils.mjs";
import { signUpdateManifest } from "./update-manifest.mjs";

const { values } = parseArgs({
  options: {
    artifact: { type: "string" },
    architecture: { type: "string" },
    channel: { type: "string" },
    "critical": { type: "boolean", default: false },
    "key-id": { type: "string" },
    "minimum-protocol": { type: "string", default: "29" },
    "minimum-schema": { type: "string", default: "27" },
    "native-package-version": { type: "string" },
    out: { type: "string" },
    platform: { type: "string" },
    "private-key": { type: "string" },
    "release-notes-url": { type: "string" },
    rollout: { type: "string", default: "100" },
    "artifact-url": { type: "string" },
    "artifact-kind": { type: "string" },
    version: { type: "string" },
  },
  strict: true,
});

function required(name) {
  const value = values[name];
  if (typeof value !== "string" || value.length === 0) throw new Error(`--${name} is required`);
  return value;
}

function integer(name) {
  const raw = required(name);
  if (!/^\d{1,10}$/.test(raw)) throw new Error(`--${name} must be an integer`);
  return Number(raw);
}

const artifactPath = path.resolve(required("artifact"));
const privateKeyPath = path.resolve(required("private-key"));
const outputPath = path.resolve(required("out"));
const artifact = await stat(artifactPath);
if (!artifact.isFile()) throw new Error("--artifact must be a regular file");
const privateKey = await readFile(privateKeyPath, "utf8");
const signed = signUpdateManifest({
  schemaVersion: 3,
  product: "grok-desktop",
  version: required("version"),
  nativePackageVersion: required("native-package-version"),
  channel: required("channel"),
  platform: required("platform"),
  architecture: required("architecture"),
  publishedAt: Math.floor(Date.now() / 1000),
  minimumProtocolVersion: integer("minimum-protocol"),
  minimumSchemaVersion: integer("minimum-schema"),
  rolloutPercentage: integer("rollout"),
  critical: values.critical,
  artifact: {
    kind: required("artifact-kind"),
    url: required("artifact-url"),
    size: artifact.size,
    sha256: await sha256File(artifactPath),
  },
  releaseNotesUrl: required("release-notes-url"),
}, privateKey, required("key-id"));
await writeFile(outputPath, `${JSON.stringify(signed, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
