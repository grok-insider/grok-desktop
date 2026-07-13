import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { assertVersion, syncReleaseVersion } from "./sync-release-version.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "grok-release-version-"));
  mkdirSync(join(root, "apps/desktop"), { recursive: true });
  mkdirSync(join(root, "crates/grok-a"), { recursive: true });
  mkdirSync(join(root, "crates/grok-b"), { recursive: true });
  writeFileSync(join(root, "package.json"), '{"name":"grok-desktop","version":"0.1.0"}\n');
  writeFileSync(join(root, "apps/desktop/package.json"), '{"name":"@grok-desktop/desktop","version":"0.1.0"}\n');
  writeFileSync(join(root, "Cargo.toml"), '[workspace]\nmembers=[]\n\n[workspace.package]\nversion = "0.1.0"\n');
  writeFileSync(join(root, "crates/grok-a/Cargo.toml"), '[package]\nname = "grok-a"\nversion.workspace = true\n\n[dependencies]\ngrok-b = { path = "../grok-b", version = "0.1.0" }\nserde = "1.0.0"\n');
  writeFileSync(join(root, "crates/grok-b/Cargo.toml"), '[package]\nname = "grok-b"\nversion.workspace = true\n');
  writeFileSync(join(root, "flake.nix"), 'pname = "grok-integration-runner";\nversion = "0.1.0";\n');
  return root;
}

test("synchronizes stable and prerelease version sites without touching external dependencies", () => {
  const root = fixture();
  syncReleaseVersion(root, "0.2.0-beta.1", { updateCargoLock: false });
  syncReleaseVersion(root, "0.2.0-beta.1", { check: true, updateCargoLock: false });
  assert.equal(JSON.parse(readFileSync(join(root, "package.json"))).version, "0.2.0-beta.1");
  const manifest = readFileSync(join(root, "crates/grok-a/Cargo.toml"), "utf8");
  assert.match(manifest, /grok-b = \{ path = "\.\.\/grok-b", version = "0\.2\.0-beta\.1" \}/);
  assert.match(manifest, /serde = "1\.0\.0"/);
});

test("check mode rejects drift", () => {
  const root = fixture();
  assert.throws(
    () => syncReleaseVersion(root, "0.1.1", { check: true, updateCargoLock: false }),
    /release version drift|version is not/,
  );
});

test("rejects malformed versions", () => {
  for (const version of ["v1.2.3", "1.2", "01.2.3", "1.2.3 beta"]) {
    assert.throws(() => assertVersion(version), /invalid release version/);
  }
});
