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
  assert.match(workflow, /WindowsIdentity\]::GetCurrent\(\)\.User\.Value/);
  assert.match(workflow, /"\*\$\{runnerSid\}:\(R\)"/);
  assert.doesNotMatch(workflow, /"\$\{env:USERNAME\}:\(R\)"/);
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
  assert.doesNotMatch(
    workflow,
    /pnpm package:linux[^]*--acp-pinned-manifest apps\/desktop\/release\/components\/grok-build\/linux-x64\.json/,
  );
  assert.match(workflow, /gh release create "\$GITHUB_REF_NAME" release-assets\/\*/);
});
