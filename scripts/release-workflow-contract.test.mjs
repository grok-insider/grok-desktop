import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("bootstraps v0.0.1 without letting Release Please publish it", () => {
  const config = JSON.parse(read("release-please-config.json"));
  const root = config.packages["."];
  assert.equal(config["skip-github-release"], true);
  assert.equal(root["initial-version"], "0.0.1");
  assert.equal(root.versioning, "always-bump-patch");
  assert.equal(
    root["pull-request-title-pattern"],
    "chore${scope}: release${component} ${version}",
  );
});

test("finalizes only an exact qualified release and is idempotent at the expected tag", () => {
  const workflow = read(".github/workflows/finalize-manual-release.yml");
  assert.match(workflow, /token: \$\{\{ secrets\.RELEASE_PLZ_TOKEN \}\}/);
  assert.match(workflow, /name: release-qualification/);
  assert.match(workflow, /release-candidate\/v1/);
  assert.match(workflow, /verify-qualification/);
  assert.match(workflow, /test "\$head_tree" = "\$merge_tree"/);
  assert.match(workflow, /chore\(master\): release \$\{version\}/);
  assert.match(workflow, /github\.event_name == 'workflow_dispatch' && github\.actor == 'grok-insider'/);
  assert.match(workflow, /git ls-remote --exit-code origin refs\/heads\/master/);
  assert.match(workflow, /git push origin "v\$\{version\}"/);
  assert.match(workflow, /Tag v\$\{version\} already exists at the qualified commit/);
  assert.ok(workflow.indexOf("verify-qualification") < workflow.indexOf('git tag -a "v${version}"'));
  assert.match(workflow, /--remove-label "autorelease: pending"/);
  assert.match(workflow, /--add-label "autorelease: tagged"/);
});

test("builds production artifacts only in the pre-tag reusable workflow", () => {
  const workflow = read(".github/workflows/release-build.yml");
  assert.match(workflow, /workflow_call:/);
  assert.match(workflow, /ref: \$\{\{ inputs\.source_sha \}\}/);
  assert.match(workflow, /retention-days: 30/g);
  assert.match(workflow, /CACHIX_AUTH_TOKEN:/);
  assert.match(workflow, /cachix\/cachix-action@v16/);
  assert.match(workflow, /name: grok-insider/);
  assert.match(workflow, /cachix push grok-insider "\$store_path"/);
  assert.match(workflow, /nix-portable-runtime\.json/);
  assert.match(workflow, /nix build --print-build-logs --out-link "\$runtime" \.#portableLinuxRuntime/);
  assert.match(workflow, /inspectPortableLinuxRuntimeFile/);
  assert.match(workflow, /--acp-pinned-manifest release\/components\/grok-build\/linux-x64\.json/);
  assert.match(workflow, /name: Build Windows x64 core[\s\S]*runs-on: windows-latest/);
  assert.match(workflow, /rustup default stable/);
  assert.match(workflow, /resolve-windows-release-toolchain\.mjs --arch x64/);
  assert.match(workflow, /pnpm --filter @grok-desktop\/desktop build:windows-daemon `\n\s+--arch x64/);
  assert.match(workflow, /pnpm --filter @grok-desktop\/desktop package:windows-core `\n\s+--arch x64/);
  assert.match(workflow, /unsigned Windows packaging rejects ambient signing input/);
  assert.doesNotMatch(workflow, /runs-on: \[self-hosted, windows, x64\]/);
  assert.doesNotMatch(workflow, /GROK_MSIX_|PREVIEW_PFX|\.appinstaller\b|\.msix\b/);
});

test("candidate and qualification workflows bind exact protected evidence", () => {
  const candidate = read(".github/workflows/release-candidate.yml");
  const qualification = read(".github/workflows/qualify-release-candidate.yml");
  assert.match(candidate, /github\.ref == 'refs\/heads\/master'/);
  assert.match(candidate, /uses: \.\/\.github\/workflows\/release-build\.yml/);
  assert.match(candidate, /secrets: inherit/);
  assert.match(candidate, /release-candidate\.mjs validate-pr/);
  assert.match(candidate, /git merge-base --is-ancestor "\$base_sha" "\$HEAD_SHA"/);
  assert.match(candidate, /artifact-ids: \$\{\{ needs\.build\.outputs\.linux_artifact_id \}\}/);
  assert.doesNotMatch(candidate, /contents: write|beta-release|gh release create|git tag/);
  assert.match(qualification, /name: beta-candidate/);
  assert.match(qualification, /retention-days: 90/);
  assert.match(qualification, /release-candidate\/v1/);
  assert.match(qualification, /gh run rerun/);
});

test("publishes exact qualified bytes without rebuilding after the tag", () => {
  const workflow = read(".github/workflows/release.yml");
  assert.match(workflow, /tags:\s*\n\s*- "v\*\.\*\.\*"/);
  assert.match(workflow, /Qualification-Run:/);
  assert.match(workflow, /name: release-qualification/);
  assert.match(workflow, /verify-qualification/);
  assert.match(workflow, /verify-record/);
  assert.match(workflow, /artifact-ids: \$\{\{ needs\.resolve\.outputs\.linux_artifact_id \}\}/);
  assert.match(workflow, /release-qualification\.json/);
  assert.match(workflow, /win32:x64:exe:nsis-installer/);
  assert.match(workflow, /intentionally unsigned/);
  assert.match(workflow, /Microsoft Defender SmartScreen warning/);
  assert.match(workflow, /windowsCodeSigning "unsigned"/);
  assert.match(workflow, /gh release create "\$RELEASE_TAG" release-assets\/\*/);
  assert.doesNotMatch(workflow, /package:linux|package:windows-core|build:windows-daemon|runs-on: \[self-hosted/);
});

test("preflights unsigned Windows build inputs without certificate material", () => {
  const workflow = read(".github/workflows/release-prerequisites.yml");
  assert.match(workflow, /name: Preview Windows build inputs/);
  assert.match(workflow, /environment: beta-windows-build/);
  assert.match(workflow, /GROK_WINDOWS_MAX_TESTED_VERSION/);
  assert.match(workflow, /GROK_UPDATE_TRUSTED_KEYS_JSON/);
  assert.match(workflow, /GROK_XAI_COMPONENT_PROVENANCE_EVIDENCE_ID/);
  assert.match(workflow, /GROK_XAI_COMPONENT_REDISTRIBUTION_EVIDENCE_ID/);
  assert.match(workflow, /name: Preview hosted Windows toolchain/);
  assert.match(workflow, /runs-on: windows-latest/);
  assert.match(workflow, /resolve-windows-release-toolchain\.mjs --arch x64 --skip-cargo-hydration/);
  assert.doesNotMatch(workflow, /GROK_WINDOWS_CARGO_PATH|GROK_WINDOWS_RUSTC_PATH|GROK_WINDOWS_LINKER_PATH|GROK_WINDOWS_CARGO_CACHE|GROK_WINDOWS_TOOLCHAIN_ENV_JSON/);
  assert.doesNotMatch(workflow, /GROK_ACP_CATALOG_TRUSTED_KEYS/);
  assert.doesNotMatch(workflow, /windows-signing|GROK_MSIX_|SIGNTOOL|TIMESTAMP_SERVER|SIGNER_SHA1|SIGN_ARGS_JSON/);
  assert.doesNotMatch(workflow, /PREVIEW_CERT|PFX|\.cer\b|\.appinstaller\b|\.msix\b/);
});
