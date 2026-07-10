/** Converts Electron's opaque Windows HWND buffer into the daemon's unsigned token. */
export function nativeWindowToken(handle: Buffer): bigint {
  if (handle.byteLength !== 4 && handle.byteLength !== 8) {
    throw new Error("Electron returned an invalid native window handle");
  }
  const token = handle.byteLength === 8
    ? handle.readBigUInt64LE(0)
    : BigInt(handle.readUInt32LE(0));
  if (token === 0n) throw new Error("Electron returned an invalid native window handle");
  return token;
}

/** Resolves the platform-specific owner token without reading unsupported native handles. */
export function credentialEnrollmentParentWindowToken(
  getNativeWindowHandle: () => Buffer,
  platform: NodeJS.Platform = process.platform,
): bigint {
  if (platform === "linux") return 0n;
  if (platform !== "win32") {
    throw new Error("native xAI credential enrollment is unavailable on this platform");
  }
  return nativeWindowToken(getNativeWindowHandle());
}
