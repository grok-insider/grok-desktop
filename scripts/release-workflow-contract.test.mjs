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
  assert.match(workflow, /ref: \$\{\{ github\.event\.pull_request\.merge_commit_sha \}\}/);
  assert.match(workflow, /chore\(master\): release grok-desktop \$\{version\}/);
  assert.match(workflow, /git push origin "v\$\{version\}"/);
});

test("publishes binaries only from an immutable version tag", () => {
  const workflow = read(".github/workflows/release.yml");
  assert.match(workflow, /tags:\s*\n\s*- "v\*\.\*\.\*"/);
  assert.match(workflow, /test "\$GITHUB_REF_NAME" = "v\$version"/);
  assert.match(workflow, /gh release create "\$GITHUB_REF_NAME" release-assets\/\*/);
});
