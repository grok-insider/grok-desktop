// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import {
  applyGraphicsPolicy,
  graphicsRelaunchArguments,
  GRAPHICS_FALLBACK_MARKER,
  resolveGraphicsPolicy,
} from "./graphicsPolicy.js";

const environment = (overrides: Partial<Parameters<typeof resolveGraphicsPolicy>[0]> = {}) => ({
  platform: "linux" as NodeJS.Platform,
  argv: [] as string[],
  waylandDisplay: "wayland-1",
  x11Display: ":0",
  glxVendor: "mesa",
  gbmBackend: "",
  nvidiaDriverPresent: false,
  ...overrides,
});

describe("graphicsPolicy", () => {
  it("keeps platform defaults off Linux", () => {
    expect(resolveGraphicsPolicy(environment({ platform: "win32" }))).toMatchObject({
      backend: "auto",
      reason: "platform_default",
    });
  });

  it("selects the only available Linux display backend", () => {
    expect(resolveGraphicsPolicy(environment({ x11Display: "" }))).toMatchObject({ backend: "wayland", reason: "wayland_only" });
    expect(resolveGraphicsPolicy(environment({ waylandDisplay: "" }))).toMatchObject({ backend: "x11", reason: "x11_only" });
    expect(resolveGraphicsPolicy(environment({ waylandDisplay: "", x11Display: "" }))).toMatchObject({ backend: "auto", reason: "headless" });
  });

  it("uses XWayland for mixed NVIDIA sessions and Wayland for Mesa", () => {
    expect(resolveGraphicsPolicy(environment({ glxVendor: "nvidia" }))).toMatchObject({ backend: "x11", reason: "nvidia_risk" });
    expect(resolveGraphicsPolicy(environment({ nvidiaDriverPresent: true }))).toMatchObject({ backend: "x11", reason: "nvidia_risk" });
    expect(resolveGraphicsPolicy(environment())).toMatchObject({ backend: "wayland", reason: "wayland_preferred" });
  });

  it("honors one valid override and fails malformed or conflicting values back to auto policy", () => {
    expect(resolveGraphicsPolicy(environment({ argv: ["--grok-graphics-backend=software"] }))).toMatchObject({ backend: "software", reason: "explicit" });
    expect(resolveGraphicsPolicy(environment({ argv: ["--grok-graphics-backend=broken"] }))).toMatchObject({ backend: "wayland", warning: "invalid_override" });
    expect(resolveGraphicsPolicy(environment({ argv: ["--grok-graphics-backend=x11", "--grok-graphics-backend=wayland"] }))).toMatchObject({ backend: "wayland", warning: "conflicting_overrides" });
  });

  it("applies only graphics switches and software acceleration policy", () => {
    const app = { commandLine: { appendSwitch: vi.fn() }, disableHardwareAcceleration: vi.fn() };
    applyGraphicsPolicy(resolveGraphicsPolicy(environment({ argv: ["--grok-graphics-backend=x11"] })), app);
    expect(app.commandLine.appendSwitch).toHaveBeenCalledWith("ozone-platform", "x11");
    expect(app.disableHardwareAcceleration).not.toHaveBeenCalled();

    applyGraphicsPolicy(resolveGraphicsPolicy(environment({ argv: ["--grok-graphics-backend=software"] })), app);
    expect(app.disableHardwareAcceleration).toHaveBeenCalledOnce();
  });

  it("constructs one sanitized software fallback argument set", () => {
    expect(graphicsRelaunchArguments([
      "/app/resources/app.asar",
      "--grok-graphics-backend=wayland",
      GRAPHICS_FALLBACK_MARKER,
      "grok://chat/1",
    ])).toEqual([
      "/app/resources/app.asar",
      "grok://chat/1",
      "--grok-graphics-backend=software",
      GRAPHICS_FALLBACK_MARKER,
    ]);
  });
});
