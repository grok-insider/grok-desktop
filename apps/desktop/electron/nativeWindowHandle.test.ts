// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import {
  credentialEnrollmentParentWindowToken,
  nativeWindowToken,
} from "./nativeWindowHandle.js";

describe("nativeWindowToken", () => {
  it("decodes 32-bit and 64-bit little-endian Windows handles", () => {
    const narrow = Buffer.alloc(4);
    narrow.writeUInt32LE(0x89abcdef);
    expect(nativeWindowToken(narrow)).toBe(0x89abcdefn);

    const wide = Buffer.alloc(8);
    wide.writeBigUInt64LE(0xfedcba9876543210n);
    expect(nativeWindowToken(wide)).toBe(0xfedcba9876543210n);
  });

  it("rejects malformed or null Windows handles", () => {
    expect(() => nativeWindowToken(Buffer.alloc(3))).toThrow("invalid native window handle");
    expect(() => nativeWindowToken(Buffer.alloc(8))).toThrow("invalid native window handle");
  });
});

describe("credentialEnrollmentParentWindowToken", () => {
  it("strictly decodes an HWND only on Windows", () => {
    const handle = Buffer.alloc(8);
    handle.writeBigUInt64LE(0x1234n);
    const getNativeWindowHandle = vi.fn(() => handle);

    expect(credentialEnrollmentParentWindowToken(getNativeWindowHandle, "win32")).toBe(0x1234n);
    expect(getNativeWindowHandle).toHaveBeenCalledOnce();
  });

  it("uses the Unix no-owner sentinel without reading a native handle", () => {
    const getNativeWindowHandle = vi.fn(() => Buffer.alloc(8, 1));

    expect(credentialEnrollmentParentWindowToken(getNativeWindowHandle, "linux")).toBe(0n);
    expect(getNativeWindowHandle).not.toHaveBeenCalled();
  });

  it("fails closed on unsupported platforms without reading a native handle", () => {
    const getNativeWindowHandle = vi.fn(() => Buffer.alloc(8, 1));

    expect(() => credentialEnrollmentParentWindowToken(getNativeWindowHandle, "darwin"))
      .toThrow("unavailable on this platform");
    expect(getNativeWindowHandle).not.toHaveBeenCalled();
  });
});
