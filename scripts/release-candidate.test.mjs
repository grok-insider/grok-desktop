import assert from "node:assert/strict";
import test from "node:test";

import {
  CANDIDATE_SCHEMA,
  QA_SCHEMA,
  QUALIFICATION_SCHEMA,
  RELEASE_BRANCH,
  createQualification,
  validateChangedPaths,
  validateQualification,
  validateReleasePr,
} from "./release-candidate.mjs";

const sha = (digit) => digit.repeat(40);
const digest = (digit) => digit.repeat(64);

function pullRequest() {
  return {
    state: "open",
    draft: false,
    title: "chore(master): release 0.0.12",
    base: { ref: "master", sha: sha("a"), repo: { full_name: "grok-insider/grok-desktop" } },
    head: { ref: RELEASE_BRANCH, sha: sha("b"), repo: { full_name: "grok-insider/grok-desktop" } },
  };
}

function record() {
  return {
    schema: CANDIDATE_SCHEMA,
    repository: "grok-insider/grok-desktop",
    version: "0.0.12",
    channel: "beta",
    baseSha: sha("a"),
    sourceSha: sha("b"),
    treeSha: sha("c"),
    workflowRunId: 42,
    workflowRunAttempt: 1,
    artifacts: ["linux", "windows"].map((platform, index) => ({
      platform,
      id: index + 10,
      name: `release-${platform}-x64`,
      archiveDigest: `sha256:${digest(String(index + 1))}`,
      archiveSize: 100 + index,
      files: [{ path: `${platform}.bin`, size: 12, sha256: digest(String(index + 3)) }],
    })),
  };
}

function qa(candidate = record()) {
  return {
    schema: QA_SCHEMA,
    candidate: {
      version: candidate.version,
      sourceSha: candidate.sourceSha,
      treeSha: candidate.treeSha,
      workflowRunId: candidate.workflowRunId,
      workflowRunAttempt: candidate.workflowRunAttempt,
    },
    linux: { status: "passed", artifactId: 10, wispCdp: true },
    windows: {
      status: "passed",
      artifactId: 11,
      vmBaseSha256: digest("d"),
      harnessVersion: "qga-cdp-v1",
      checks: Object.fromEntries([
        "artifactDigest", "unsignedInstaller", "portableRuntime", "canonicalInstallRoot",
        "electronStart", "daemonIpc", "officialComponent", "cdp", "deepLink", "repair", "uninstall",
      ].map((name) => [name, true])),
    },
  };
}

test("accepts only the exact same-repository automated release pull request", () => {
  validateReleasePr(pullRequest(), {
    repository: "grok-insider/grok-desktop",
    baseSha: sha("a"),
    headSha: sha("b"),
    version: "0.0.12",
  });
  const fork = pullRequest();
  fork.head.repo.full_name = "attacker/grok-desktop";
  assert.throws(() => validateReleasePr(fork, {
    repository: "grok-insider/grok-desktop", baseSha: sha("a"), headSha: sha("b"), version: "0.0.12",
  }), /canonical repository/);
});

test("accepts a canonical administrator milestone branch", () => {
  const pr = pullRequest();
  pr.head.ref = "release-please-manual-v0.0.12-7";
  pr.title = "chore(release): v0.0.12";
  validateReleasePr(pr, {
    repository: "grok-insider/grok-desktop", baseSha: sha("a"), headSha: sha("b"), version: "0.0.12",
  });
});

test("rejects product, workflow, and duplicate release diff paths", () => {
  assert.deepEqual(validateChangedPaths([
    "package.json", ".release-please-manifest.json", "CHANGELOG.md", "Cargo.lock",
  ]), [".release-please-manifest.json", "CHANGELOG.md", "Cargo.lock", "package.json"]);
  assert.throws(() => validateChangedPaths(["package.json", "CHANGELOG.md", ".release-please-manifest.json", "src/main.ts"]), /forbidden path/);
  assert.throws(() => validateChangedPaths(["package.json", "package.json"]), /forbidden path/);
});

test("qualification binds exact candidate and every Windows acceptance check", () => {
  const candidate = record();
  const value = createQualification(candidate, qa(candidate), { workflowRunId: 99, workflowRunAttempt: 2, actor: "grok-insider" });
  assert.equal(value.schema, QUALIFICATION_SCHEMA);
  validateQualification(value, { version: candidate.version, sourceSha: candidate.sourceSha, treeSha: candidate.treeSha });
  value.qa.windows.checks.daemonIpc = false;
  assert.throws(() => validateQualification(value), /daemonIpc/);
});

test("rejects stale qualification identity", () => {
  const candidate = record();
  const value = createQualification(candidate, qa(candidate), { workflowRunId: 99, workflowRunAttempt: 1, actor: "grok-insider" });
  assert.throws(() => validateQualification(value, { version: "0.0.12", sourceSha: sha("e"), treeSha: candidate.treeSha }), /source SHA/);
});
