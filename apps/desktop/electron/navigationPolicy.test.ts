// @vitest-environment node
import { describe, expect, it } from "vitest";
import { contentSecurityPolicy } from "../vite.config.js";
import { denyRendererPermission, isAllowedAppNavigation } from "./navigationPolicy.js";
import { rendererContentSecurityPolicy } from "./rendererSecurityPolicy.js";

describe("Electron renderer security policy", () => {
  it("allows only hash changes on the exact application document", () => {
    expect(isAllowedAppNavigation("grok-desktop://app/index.html#/settings", "grok-desktop://app/index.html")).toBe(true);
    expect(isAllowedAppNavigation("grok-desktop://app/other.html", "grok-desktop://app/index.html")).toBe(false);
    expect(isAllowedAppNavigation("https://example.com/", "grok-desktop://app/index.html")).toBe(false);
  });

  it("does not treat another development path or port as the application", () => {
    const document = "http://127.0.0.1:5173/";
    expect(isAllowedAppNavigation("http://127.0.0.1:5173/#/activity", document)).toBe(true);
    expect(isAllowedAppNavigation("http://127.0.0.1:5173/admin", document)).toBe(false);
    expect(isAllowedAppNavigation("http://127.0.0.1:5174/", document)).toBe(false);
  });

  it("denies permissions and keeps production connect-src local", () => {
    expect(denyRendererPermission()).toBe(false);
    expect(contentSecurityPolicy(false)).toContain("connect-src 'self';");
    expect(contentSecurityPolicy(false)).not.toContain("ws:");
    expect(contentSecurityPolicy(false)).toContain("style-src 'self';");
    expect(contentSecurityPolicy(false)).not.toContain("style-src 'self' 'unsafe-inline'");
    expect(contentSecurityPolicy(true)).toContain("ws://127.0.0.1:*");
    expect(contentSecurityPolicy(true)).toContain("style-src 'self' 'unsafe-inline'");
    expect(rendererContentSecurityPolicy(false, "header")).toContain("frame-ancestors 'none'");
    expect(rendererContentSecurityPolicy(false, "meta")).not.toContain("frame-ancestors");
  });
});
