const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

export function resolveReleasePolicy(version) {
  const match = VERSION_PATTERN.exec(version);
  if (!match) throw new Error("release version is not canonical semantic versioning");
  const [, major, minor, patch, prerelease] = match;
  const core = `${major}.${minor}.${patch}`;
  const previewLine = major === "0" && minor === "0";
  if (prerelease) {
    const ordinal = /(?:^|\.)(\d+)$/.exec(prerelease)?.[1];
    if (!ordinal || Number(ordinal) < 1 || Number(ordinal) > 65_534) {
      throw new Error("release prerelease must end in an ordinal from 1 through 65534");
    }
    return { channel: "beta", nativeVersion: `${core}.${Number(ordinal)}`, prerelease: true };
  }
  if (previewLine) return { channel: "beta", nativeVersion: `${core}.1`, prerelease: true };
  return { channel: "stable", nativeVersion: `${core}.65535`, prerelease: false };
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) {
  const version = process.argv[2];
  if (!version || process.argv.length !== 3) throw new Error("usage: node scripts/release-policy.mjs <version>");
  process.stdout.write(`${JSON.stringify(resolveReleasePolicy(version))}\n`);
}
