#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, readFile, readdir, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { syncReleaseVersion } from "./sync-release-version.mjs";

export const CANDIDATE_SCHEMA = "grok.release-candidate/v1";
export const QA_SCHEMA = "grok.release-qa/v1";
export const QUALIFICATION_SCHEMA = "grok.release-qualification/v1";
export const RELEASE_BRANCH = "release-please--branches--master--components--grok-desktop";
const SHA256 = /^[0-9a-f]{64}$/;
const SHA = /^[0-9a-f]{40}$/;
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const MAX_RECORD_BYTES = 1024 * 1024;
const MAX_QA_BYTES = 64 * 1024;
const MAX_FILES = 64;
const REQUIRED_WINDOWS_CHECKS = Object.freeze([
  "artifactDigest",
  "unsignedInstaller",
  "portableRuntime",
  "canonicalInstallRoot",
  "electronStart",
  "daemonIpc",
  "officialComponent",
  "cdp",
  "deepLink",
  "repair",
  "uninstall",
]);

const allowedReleasePath = /^(?:CHANGELOG\.md|\.release-please-manifest\.json|package\.json|apps\/desktop\/package\.json|Cargo\.toml|Cargo\.lock|flake\.nix|crates\/[^/]+\/Cargo\.toml)$/;

export function validateReleasePr(pr, expected) {
  assertObject(pr, "pull request");
  const { repository, baseSha, headSha, version } = expected;
  assertSha(baseSha, "base SHA");
  assertSha(headSha, "head SHA");
  assertVersion(version);
  if (pr.state !== "open" || pr.draft === true) throw new Error("release pull request must be open and ready");
  if (pr.base?.ref !== "master" || pr.base?.sha !== baseSha) throw new Error("release pull request base is stale");
  if (pr.head?.sha !== headSha) throw new Error("release pull request head is not canonical");
  if (pr.head?.repo?.full_name !== repository || pr.base?.repo?.full_name !== repository) {
    throw new Error("release pull request must remain in the canonical repository");
  }
  const automated = pr.head?.ref === RELEASE_BRANCH && pr.title === `chore(master): release ${version}`;
  const manual = new RegExp(`^release-please-manual-v${escapeRegExp(version)}-[1-9]\\d*$`).test(pr.head?.ref ?? "")
    && pr.title === `chore(release): v${version}`;
  if (!automated && !manual) throw new Error("release pull request branch or title is not canonical");
}

export function validateChangedPaths(paths) {
  if (!Array.isArray(paths) || paths.length === 0) throw new Error("release pull request has no generated changes");
  const unique = new Set();
  for (const entry of paths) {
    if (typeof entry !== "string" || !allowedReleasePath.test(entry) || unique.has(entry)) {
      throw new Error(`release pull request changed a forbidden path: ${String(entry)}`);
    }
    unique.add(entry);
  }
  for (const required of ["CHANGELOG.md", ".release-please-manifest.json", "package.json"]) {
    if (!unique.has(required)) throw new Error(`release pull request is missing ${required}`);
  }
  return [...unique].toSorted();
}

export async function validateGeneratedTree(baseRoot, candidateRoot, version, changedPaths) {
  assertVersion(version);
  validateChangedPaths(changedPaths);
  syncReleaseVersion(baseRoot, version);
  const crateEntries = await readdir(path.join(baseRoot, "crates"), { withFileTypes: true });
  const generatedPaths = [
    "package.json",
    "apps/desktop/package.json",
    "Cargo.toml",
    "Cargo.lock",
    "flake.nix",
    ...crateEntries.filter((entry) => entry.isDirectory()).map((entry) => `crates/${entry.name}/Cargo.toml`),
  ];
  for (const entry of generatedPaths) {
    const [expected, actual] = await Promise.all([
      readFile(path.join(baseRoot, entry)),
      readFile(path.join(candidateRoot, entry)),
    ]);
    if (!expected.equals(actual)) throw new Error(`${entry} differs from deterministic version synchronization`);
  }
  const manifest = await readBoundedJson(path.join(candidateRoot, ".release-please-manifest.json"), 4096);
  if (Object.keys(manifest).length !== 1 || manifest["."] !== version) {
    throw new Error("release manifest does not contain only the expected version");
  }
  const [baseChangelog, candidateChangelog] = await Promise.all([
    readFile(path.join(baseRoot, "CHANGELOG.md"), "utf8"),
    readFile(path.join(candidateRoot, "CHANGELOG.md"), "utf8"),
  ]);
  if (!candidateChangelog.endsWith(baseChangelog)) throw new Error("release changelog rewrites existing history");
  const addition = candidateChangelog.slice(0, -baseChangelog.length);
  if (Buffer.byteLength(addition) < 1 || Buffer.byteLength(addition) > 64 * 1024) {
    throw new Error("release changelog addition is empty or oversized");
  }
  const headings = addition.match(new RegExp(`^## \\[?${escapeRegExp(version)}\\]?`, "gm")) ?? [];
  if (headings.length !== 1) throw new Error("release changelog must add exactly one version heading");
}

export async function createCandidateRecord(input, downloadsRoot, artifactsApi) {
  assertObject(input, "candidate input");
  assertVersion(input.version);
  assertSha(input.baseSha, "base SHA");
  assertSha(input.sourceSha, "source SHA");
  assertSha(input.treeSha, "tree SHA");
  assertPositiveInteger(input.workflowRunId, "workflow run ID");
  assertPositiveInteger(input.workflowRunAttempt, "workflow run attempt");
  if (!Array.isArray(input.artifacts) || input.artifacts.length !== 2) throw new Error("candidate must contain two artifacts");
  assertObject(artifactsApi, "artifacts API response");
  if (!Array.isArray(artifactsApi.artifacts)) throw new Error("artifacts API response is incomplete");
  const apiById = new Map(artifactsApi.artifacts.map((artifact) => [artifact.id, artifact]));
  const artifacts = [];
  for (const claimed of input.artifacts) {
    assertObject(claimed, "claimed artifact");
    if (!new Set(["linux", "windows"]).has(claimed.platform)) throw new Error("candidate artifact platform is invalid");
    assertPositiveInteger(claimed.id, "artifact ID");
    const api = apiById.get(claimed.id);
    if (!api || api.expired === true || api.name !== claimed.name) throw new Error("candidate artifact API binding is invalid");
    const archiveDigest = normalizeDigest(api.digest ?? claimed.archiveDigest);
    if (claimed.archiveDigest && normalizeDigest(claimed.archiveDigest) !== archiveDigest) {
      throw new Error("candidate artifact archive digest changed");
    }
    const root = path.join(downloadsRoot, claimed.platform);
    artifacts.push({
      platform: claimed.platform,
      id: claimed.id,
      name: claimed.name,
      archiveDigest,
      archiveSize: positiveInteger(api.size_in_bytes, "artifact archive size"),
      files: await hashTree(root),
    });
  }
  artifacts.sort((left, right) => left.platform.localeCompare(right.platform));
  if (new Set(artifacts.map(({ platform }) => platform)).size !== 2) throw new Error("candidate artifact platforms must be unique");
  return {
    schema: CANDIDATE_SCHEMA,
    repository: input.repository,
    version: input.version,
    channel: input.channel,
    baseSha: input.baseSha,
    sourceSha: input.sourceSha,
    treeSha: input.treeSha,
    workflowRunId: input.workflowRunId,
    workflowRunAttempt: input.workflowRunAttempt,
    artifacts,
  };
}

export async function verifyCandidateRecord(record, downloadsRoot) {
  validateCandidateRecord(record);
  for (const artifact of record.artifacts) {
    const actual = await hashTree(path.join(downloadsRoot, artifact.platform));
    if (JSON.stringify(actual) !== JSON.stringify(artifact.files)) throw new Error(`${artifact.platform} candidate payload changed`);
  }
  return record;
}

export function createQualification(record, qa, context) {
  validateCandidateRecord(record);
  validateQa(qa, record);
  assertPositiveInteger(context.workflowRunId, "qualification run ID");
  assertPositiveInteger(context.workflowRunAttempt, "qualification run attempt");
  if (typeof context.actor !== "string" || !/^[A-Za-z0-9-]{1,39}$/.test(context.actor)) {
    throw new Error("qualification actor is invalid");
  }
  return {
    schema: QUALIFICATION_SCHEMA,
    candidate: record,
    qa,
    qualification: {
      workflowRunId: context.workflowRunId,
      workflowRunAttempt: context.workflowRunAttempt,
      actor: context.actor,
    },
  };
}

export function validateQualification(value, expected = {}) {
  assertObject(value, "qualification");
  if (value.schema !== QUALIFICATION_SCHEMA) throw new Error("qualification schema is unsupported");
  validateCandidateRecord(value.candidate);
  validateQa(value.qa, value.candidate);
  assertObject(value.qualification, "qualification context");
  assertPositiveInteger(value.qualification.workflowRunId, "qualification run ID");
  assertPositiveInteger(value.qualification.workflowRunAttempt, "qualification run attempt");
  for (const [field, label] of [["version", "version"], ["sourceSha", "source SHA"], ["treeSha", "tree SHA"]]) {
    if (expected[field] !== undefined && value.candidate[field] !== expected[field]) {
      throw new Error(`qualification ${label} does not match`);
    }
  }
  return value;
}

export function validateCandidateRecord(record) {
  assertObject(record, "candidate record");
  if (record.schema !== CANDIDATE_SCHEMA) throw new Error("candidate record schema is unsupported");
  assertVersion(record.version);
  assertSha(record.baseSha, "base SHA");
  assertSha(record.sourceSha, "source SHA");
  assertSha(record.treeSha, "tree SHA");
  assertPositiveInteger(record.workflowRunId, "workflow run ID");
  assertPositiveInteger(record.workflowRunAttempt, "workflow run attempt");
  if (!Array.isArray(record.artifacts) || record.artifacts.length !== 2) throw new Error("candidate record artifact count is invalid");
  const platforms = new Set();
  for (const artifact of record.artifacts) {
    assertObject(artifact, "candidate artifact");
    if (!new Set(["linux", "windows"]).has(artifact.platform) || platforms.has(artifact.platform)) {
      throw new Error("candidate record platform is invalid");
    }
    platforms.add(artifact.platform);
    assertPositiveInteger(artifact.id, "artifact ID");
    if (typeof artifact.name !== "string" || !/^release-(?:linux|windows)-x64$/.test(artifact.name)) throw new Error("artifact name is invalid");
    normalizeDigest(artifact.archiveDigest);
    assertPositiveInteger(artifact.archiveSize, "artifact archive size");
    validateFileRecords(artifact.files);
  }
}

export function validateQa(qa, record) {
  assertObject(qa, "QA record");
  if (qa.schema !== QA_SCHEMA) throw new Error("QA schema is unsupported");
  assertObject(qa.candidate, "QA candidate binding");
  for (const field of ["version", "sourceSha", "treeSha", "workflowRunId", "workflowRunAttempt"]) {
    if (qa.candidate[field] !== record[field]) throw new Error(`QA candidate ${field} does not match`);
  }
  const linux = artifactFor(record, "linux");
  const windows = artifactFor(record, "windows");
  if (qa.linux?.status !== "passed" || qa.linux?.artifactId !== linux.id || qa.linux?.wispCdp !== true) {
    throw new Error("Linux Wisp/CDP qualification is incomplete");
  }
  if (qa.windows?.status !== "passed" || qa.windows?.artifactId !== windows.id) throw new Error("Windows qualification is incomplete");
  if (typeof qa.windows.vmBaseSha256 !== "string" || !SHA256.test(qa.windows.vmBaseSha256)) throw new Error("Windows VM base digest is invalid");
  if (typeof qa.windows.harnessVersion !== "string" || !/^[A-Za-z0-9._-]{1,64}$/.test(qa.windows.harnessVersion)) {
    throw new Error("Windows harness version is invalid");
  }
  assertObject(qa.windows.checks, "Windows checks");
  for (const check of REQUIRED_WINDOWS_CHECKS) {
    if (qa.windows.checks[check] !== true) throw new Error(`Windows check ${check} did not pass`);
  }
}

async function hashTree(root) {
  const rootMetadata = await stat(root);
  if (!rootMetadata.isDirectory()) throw new Error("artifact download root is not a directory");
  const files = [];
  await walk(root, "", files);
  if (files.length === 0 || files.length > MAX_FILES) throw new Error("artifact payload file count is invalid");
  return files.toSorted((left, right) => left.path.localeCompare(right.path));
}

async function walk(root, relative, files) {
  const entries = await readdir(path.join(root, relative), { withFileTypes: true });
  for (const entry of entries) {
    const next = path.posix.join(relative.split(path.sep).join(path.posix.sep), entry.name);
    const absolute = path.join(root, ...next.split("/"));
    const metadata = await lstat(absolute);
    if (metadata.isSymbolicLink()) throw new Error("artifact payload contains a symbolic link");
    if (metadata.isDirectory()) await walk(root, next, files);
    else if (metadata.isFile() && metadata.size > 0 && metadata.size <= 2 * 1024 * 1024 * 1024) {
      files.push({ path: next, size: metadata.size, sha256: await sha256File(absolute) });
    } else throw new Error("artifact payload contains an unsupported entry");
  }
}

async function sha256File(file) {
  return await new Promise((resolve, reject) => {
    const hash = createHash("sha256");
    const stream = createReadStream(file, { highWaterMark: 1024 * 1024 });
    stream.on("data", (chunk) => hash.update(chunk));
    stream.once("error", reject);
    stream.once("end", () => resolve(hash.digest("hex")));
  });
}

function validateFileRecords(files) {
  if (!Array.isArray(files) || files.length === 0 || files.length > MAX_FILES) throw new Error("artifact file records are invalid");
  let previous = "";
  for (const file of files) {
    assertObject(file, "artifact file");
    if (typeof file.path !== "string" || file.path.startsWith("/") || file.path.includes("..") || file.path <= previous) {
      throw new Error("artifact file path is invalid");
    }
    previous = file.path;
    assertPositiveInteger(file.size, "artifact file size");
    if (typeof file.sha256 !== "string" || !SHA256.test(file.sha256)) throw new Error("artifact file digest is invalid");
  }
}

function artifactFor(record, platform) {
  const artifact = record.artifacts.find((candidate) => candidate.platform === platform);
  if (!artifact) throw new Error(`candidate record is missing ${platform}`);
  return artifact;
}

function normalizeDigest(value) {
  if (typeof value !== "string") throw new Error("artifact archive digest is invalid");
  const normalized = value.startsWith("sha256:") ? value.slice(7) : value;
  if (!SHA256.test(normalized)) throw new Error("artifact archive digest is invalid");
  return `sha256:${normalized}`;
}

function assertVersion(value) {
  if (typeof value !== "string" || !VERSION.test(value)) throw new Error("release version is invalid");
}

function assertSha(value, label) {
  if (typeof value !== "string" || !SHA.test(value)) throw new Error(`${label} is invalid`);
}

function assertObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} is invalid`);
}

function assertPositiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${label} is invalid`);
}

function positiveInteger(value, label) {
  const parsed = typeof value === "number" ? value : Number(value);
  assertPositiveInteger(parsed, label);
  return parsed;
}

async function readBoundedJson(file, maximum = MAX_RECORD_BYTES) {
  const metadata = await stat(file);
  if (!metadata.isFile() || metadata.size < 2 || metadata.size > maximum) throw new Error("JSON input is empty or oversized");
  return JSON.parse(await readFile(file, "utf8"));
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function args(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value === undefined || values.has(name)) throw new Error("release candidate arguments are invalid");
    values.set(name, value);
  }
  return values;
}

async function main(argv) {
  const [command, ...rest] = argv;
  const values = args(rest);
  const required = (name) => {
    const value = values.get(`--${name}`);
    if (!value) throw new Error(`missing --${name}`);
    return value;
  };
  if (command === "validate-pr") {
    const pr = await readBoundedJson(required("pr"));
    const changed = (await readFile(required("changed-paths"), "utf8")).split(/\r?\n/).filter(Boolean);
    validateReleasePr(pr, {
      repository: required("repository"),
      baseSha: required("base-sha"),
      headSha: required("head-sha"),
      version: required("version"),
    });
    validateChangedPaths(changed);
    await validateGeneratedTree(required("base-root"), required("candidate-root"), required("version"), changed);
  } else if (command === "create-record") {
    const record = await createCandidateRecord(
      await readBoundedJson(required("input")),
      required("downloads"),
      await readBoundedJson(required("artifacts-api")),
    );
    await writeFile(required("out"), `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
  } else if (command === "verify-record") {
    await verifyCandidateRecord(await readBoundedJson(required("record")), required("downloads"));
  } else if (command === "qualify") {
    const record = await readBoundedJson(required("record"));
    const qa = await readBoundedJson(required("qa"), MAX_QA_BYTES);
    const qualification = createQualification(record, qa, {
      workflowRunId: positiveInteger(required("run-id"), "qualification run ID"),
      workflowRunAttempt: positiveInteger(required("run-attempt"), "qualification run attempt"),
      actor: required("actor"),
    });
    await writeFile(required("out"), `${JSON.stringify(qualification, null, 2)}\n`, { mode: 0o600 });
  } else if (command === "verify-qualification") {
    validateQualification(await readBoundedJson(required("qualification")), {
      version: required("version"),
      sourceSha: required("source-sha"),
      treeSha: required("tree-sha"),
    });
  } else throw new Error("unknown release candidate command");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
