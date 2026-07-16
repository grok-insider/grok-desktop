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

test("finalizes automated and manual release PRs through the owner token", () => {
  const workflow = read(".github/workflows/finalize-manual-release.yml");
  assert.match(workflow, /startsWith\(github\.event\.pull_request\.head\.ref, 'release-please--'\)/);
  assert.match(workflow, /startsWith\(github\.event\.pull_request\.head\.ref, 'release-please-manual-'\)/);
  assert.match(workflow, /token: \$\{\{ secrets\.RELEASE_PLZ_TOKEN \}\}/);
  assert.match(workflow, /inputs\.source_sha \|\| github\.event\.pull_request\.merge_commit_sha/);
  assert.match(workflow, /chore\(master\): release \$\{version\}/);
  assert.match(workflow, /github\.event_name == 'workflow_dispatch' && github\.actor == 'grok-insider'/);
  assert.match(workflow, /git ls-remote --exit-code origin refs\/heads\/master/);
  assert.match(workflow, /git push origin "v\$\{version\}"/);
  assert.match(workflow, /pull-requests: write/);
  assert.match(workflow, /git merge-base --is-ancestor "\$merge_sha" HEAD/);
  assert.match(workflow, /--remove-label "autorelease: pending"/);
  assert.match(workflow, /--add-label "autorelease: tagged"/);
});

test("publishes binaries only from an immutable version tag", () => {
  const workflow = read(".github/workflows/release.yml");
  assert.match(workflow, /tags:\s*\n\s*- "v\*\.\*\.\*"/);
  assert.match(workflow, /test "\$GITHUB_REF_NAME" = "v\$version"/);
  assert.match(workflow, /appimagetool\/releases\/download\/1\.9\.1\/appimagetool-x86_64\.AppImage/);
  assert.doesNotMatch(workflow, /appimagetool\/releases\/download\/continuous/);
  assert.match(workflow, /windows_environment=\$\{channel\}-windows-build/);
  assert.doesNotMatch(workflow, /windows-signing/);
  assert.doesNotMatch(workflow, /GROK_MSIX_/);
  assert.doesNotMatch(workflow, /\$\{\{ vars\.GROK_WINDOWS_(?:SIGNTOOL_PATH|TIMESTAMP_SERVER|SIGNER_SHA1|SIGN_ARGS_JSON) \}\}/);
  assert.doesNotMatch(workflow, /\$\{\{ secrets\.GROK_WINDOWS_PREVIEW_CERT/);
  assert.doesNotMatch(workflow, /Provision isolated preview signing certificate/);
  assert.doesNotMatch(workflow, /PREVIEW_PFX|\.cer\b|\.appinstaller\b|\.msix\b/);
  assert.match(workflow, /nix build --print-build-logs --out-link "\$runtime" \.#portableLinuxRuntime/);
  assert.doesNotMatch(workflow, /nix develop --command cargo build/);
  assert.match(workflow, /readelf --program-headers --wide/);
  assert.match(workflow, /inspectPortableLinuxRuntimeFile/);
  assert.match(workflow, /NEEDED\|RPATH\|RUNPATH\|CONFIG\|DEPAUDIT\|AUDIT\|AUXILIARY\|FILTER/);
  assert.match(workflow, /--daemon "\$RUNNER_TEMP\/grok-portable-linux-runtime\/bin\/grok-daemon"/);
  assert.match(workflow, /--host-tools-helper "\$RUNNER_TEMP\/grok-portable-linux-runtime\/bin\/grok-host-tools-mcp"/);
  assert.match(workflow, /--acp-pinned-manifest release\/components\/grok-build\/linux-x64\.json/);
  assert.match(workflow, /\$packageManifest = "release\/components\/grok-build\/windows-x64\.json"/);
  assert.match(
    workflow,
    /Test-Path -LiteralPath \$stage[^]*Remove-Item -LiteralPath \$stage -Recurse -Force[^]*New-Item -ItemType Directory/,
  );
  assert.match(workflow, /Windows core stage must not be a reparse point/);
  assert.match(
    workflow,
    /pnpm --filter @grok-desktop\/desktop build:windows-daemon `\n\s+--arch x64/,
  );
  assert.equal(
    workflow.match(/pnpm --filter @grok-desktop\/desktop build:windows-daemon/g)?.length,
    1,
  );
  assert.doesNotMatch(workflow, /build:windows-daemon -- `/);
  assert.match(workflow, /if \(\$LASTEXITCODE -ne 0\) \{ throw "Windows native runtime build failed" \}/);
  assert.match(workflow, /@\("grok-daemon\.exe", "grok-host-tools-mcp\.exe"\)/);
  assert.match(workflow, /Windows native runtime output is incomplete/);
  assert.match(workflow, /official Grok component download failed/);
  assert.match(workflow, /Windows official component staging is incomplete/);
  assert.match(
    workflow,
    /pnpm --filter @grok-desktop\/desktop package:windows-core `\n\s+--arch x64/,
  );
  assert.doesNotMatch(workflow, /package:windows-core -- `/);
  assert.match(workflow, /if \(\$LASTEXITCODE -ne 0\) \{ throw "Windows core packaging failed" \}/);
  assert.match(workflow, /GrokDesktop-\*-x64\.exe/);
  assert.match(workflow, /win32:x64:exe:nsis-installer/);
  assert.match(workflow, /--artifact-kind "\$artifact_kind"/);
  assert.match(workflow, /path: release-downloads/);
  assert.match(workflow, /Stage the exact release asset allowlist/);
  assert.match(workflow, /node scripts\/stage-release-assets\.mjs release-downloads release-assets "\$channel"/);
  assert.doesNotMatch(workflow, /release:update-manifest -- \\/);
  assert.doesNotMatch(workflow, /release:verify-update-manifest -- \\/);
  assert.match(workflow, /--native-package-version "\$version"/);
  assert.match(workflow, /--platform "\$platform" --architecture "\$architecture" --channel "\$channel"/);
  assert.match(workflow, /intentionally unsigned/);
  assert.match(workflow, /Microsoft Defender SmartScreen warning/);
  assert.match(workflow, /SHA256SUMS/);
  assert.match(workflow, /GitHub artifact attestation/);
  assert.match(workflow, /windowsCodeSigning "unsigned"/);
  assert.match(workflow, /release-assets\/\*\.exe/);
  assert.doesNotMatch(
    workflow,
    /pnpm package:linux[^]*--acp-pinned-manifest apps\/desktop\/release\/components\/grok-build\/linux-x64\.json/,
  );
  assert.match(workflow, /gh release create "\$GITHUB_REF_NAME" release-assets\/\*/);
});

test("preflights unsigned Windows build inputs without certificate material", () => {
  const workflow = read(".github/workflows/release-prerequisites.yml");
  assert.match(workflow, /name: Preview Windows build inputs/);
  assert.match(workflow, /environment: beta-windows-build/);
  assert.match(workflow, /GROK_WINDOWS_CARGO_PATH/);
  assert.match(workflow, /GROK_UPDATE_TRUSTED_KEYS_JSON/);
  assert.match(workflow, /GROK_XAI_COMPONENT_PROVENANCE_EVIDENCE_ID/);
  assert.match(workflow, /GROK_XAI_COMPONENT_REDISTRIBUTION_EVIDENCE_ID/);
  assert.doesNotMatch(workflow, /GROK_ACP_CATALOG_TRUSTED_KEYS/);
  assert.doesNotMatch(workflow, /windows-signing|GROK_MSIX_|SIGNTOOL|TIMESTAMP_SERVER|SIGNER_SHA1|SIGN_ARGS_JSON/);
  assert.doesNotMatch(workflow, /PREVIEW_CERT|PFX|\.cer\b|\.appinstaller\b|\.msix\b/);
});
