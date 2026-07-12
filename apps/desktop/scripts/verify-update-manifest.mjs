import { createPublicKey } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { parseArgs } from "node:util";
import { verifySignedUpdateManifest } from "./update-manifest.mjs";

const { values } = parseArgs({
  options: {
    manifest: { type: "string" },
    "trust-file": { type: "string" },
    platform: { type: "string" },
    architecture: { type: "string" },
    version: { type: "string" },
  },
  strict: true,
});

function required(name) {
  const value = values[name];
  if (typeof value !== "string" || value.length < 1) throw new Error(`--${name} is required`);
  return value;
}

const trustRaw = await readFile(path.resolve(required("trust-file")));
if (trustRaw.byteLength < 1 || trustRaw.byteLength > 65_536) throw new Error("update trust is invalid");
let trust;
try { trust = JSON.parse(trustRaw.toString("utf8")); } catch { throw new Error("update trust is invalid"); }
if (!trust || typeof trust !== "object" || Array.isArray(trust)) throw new Error("update trust is invalid");
const entries = Object.entries(trust);
if (entries.length < 1 || entries.length > 8) throw new Error("update trust is invalid");
const keys = new Map(entries.map(([keyId, encoded]) => {
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(keyId) || typeof encoded !== "string") {
    throw new Error("update trust is invalid");
  }
  const der = Buffer.from(encoded, "base64");
  if (der.toString("base64") !== encoded) throw new Error("update trust is invalid");
  const key = createPublicKey({ key: der, format: "der", type: "spki" });
  if (key.asymmetricKeyType !== "ed25519"
      || !Buffer.from(key.export({ format: "der", type: "spki" })).equals(der)) {
    throw new Error("update trust is invalid");
  }
  return [keyId, key];
}));
const envelopeRaw = await readFile(path.resolve(required("manifest")));
if (envelopeRaw.byteLength < 1 || envelopeRaw.byteLength > 1024 * 1024) throw new Error("update manifest is invalid");
let envelope;
try { envelope = JSON.parse(envelopeRaw.toString("utf8")); } catch { throw new Error("update manifest is invalid"); }
const manifest = verifySignedUpdateManifest(envelope, keys);
if (manifest.platform !== required("platform") || manifest.architecture !== required("architecture")
    || manifest.version !== required("version") || manifest.channel !== "stable") {
  throw new Error("update manifest does not match the release target");
}
process.stdout.write(`${JSON.stringify({ ok: true, version: manifest.version, platform: manifest.platform, architecture: manifest.architecture })}\n`);
