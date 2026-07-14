import assert from "node:assert/strict";
import test from "node:test";
import { resolveReleasePolicy } from "./release-policy.mjs";

test("locks the 0.0.z line to beta prereleases without a SemVer suffix", () => {
  assert.deepEqual(resolveReleasePolicy("0.0.1"), {
    channel: "beta", nativeVersion: "0.0.1.1", prerelease: true,
  });
});

test("keeps later canonical versions stable and maps suffixed previews by ordinal", () => {
  assert.deepEqual(resolveReleasePolicy("0.1.0"), {
    channel: "stable", nativeVersion: "0.1.0.65535", prerelease: false,
  });
  assert.deepEqual(resolveReleasePolicy("1.2.3-beta.7"), {
    channel: "beta", nativeVersion: "1.2.3.7", prerelease: true,
  });
});

test("rejects ambiguous tags and prereleases without a bounded ordinal", () => {
  assert.throws(() => resolveReleasePolicy("v0.0.1"), /canonical/);
  assert.throws(() => resolveReleasePolicy("1.2.3-beta"), /ordinal/);
  assert.throws(() => resolveReleasePolicy("1.2.3-beta.65535"), /ordinal/);
});
