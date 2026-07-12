export type GraphicsBackend = "auto" | "wayland" | "x11" | "software";

export interface GraphicsEnvironment {
  platform: NodeJS.Platform;
  argv: readonly string[];
  waylandDisplay?: string;
  x11Display?: string;
  glxVendor?: string;
  gbmBackend?: string;
  nvidiaDriverPresent: boolean;
  nixGraphicsEnvironment: boolean;
}

export interface GraphicsPolicy {
  backend: GraphicsBackend;
  reason: "platform_default" | "explicit" | "wayland_only" | "x11_only" | "nvidia_risk" | "nvidia_nix_risk" | "wayland_preferred" | "headless";
  fallbackAttempted: boolean;
  softwarePlatform?: "wayland" | "x11";
  warning?: "invalid_override" | "conflicting_overrides";
}

const BACKEND_PREFIX = "--grok-graphics-backend=";
export const GRAPHICS_FALLBACK_MARKER = "--grok-graphics-fallback-attempted";
export const DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE = 78;

export function resolveGraphicsPolicy(environment: GraphicsEnvironment): GraphicsPolicy {
  const fallbackAttempted = environment.argv.includes(GRAPHICS_FALLBACK_MARKER);
  const overrides = environment.argv
    .filter((argument) => argument.startsWith(BACKEND_PREFIX))
    .map((argument) => argument.slice(BACKEND_PREFIX.length));
  const distinctOverrides = new Set(overrides);
  const validOverride = overrides.length === 1 && isGraphicsBackend(overrides[0])
    ? overrides[0]
    : undefined;
  const warning = distinctOverrides.size > 1 || overrides.length > 1
    ? "conflicting_overrides"
    : overrides.length === 1 && !validOverride
      ? "invalid_override"
      : undefined;

  if (validOverride && validOverride !== "auto") {
    return {
      backend: validOverride,
      reason: "explicit",
      fallbackAttempted,
      softwarePlatform: validOverride === "software" ? availableSoftwarePlatform(environment) : undefined,
    };
  }
  if (environment.platform !== "linux") {
    return { backend: "auto", reason: "platform_default", fallbackAttempted, warning };
  }

  const nvidiaRisk = environment.nvidiaDriverPresent
    || normalized(environment.glxVendor) === "nvidia"
    || normalized(environment.gbmBackend).startsWith("nvidia");
  if (nvidiaRisk && environment.nixGraphicsEnvironment) {
    return {
      backend: "software",
      reason: "nvidia_nix_risk",
      fallbackAttempted,
      warning,
      softwarePlatform: availableSoftwarePlatform(environment),
    };
  }

  const wayland = nonempty(environment.waylandDisplay);
  const x11 = nonempty(environment.x11Display);
  if (wayland && !x11) return { backend: "wayland", reason: "wayland_only", fallbackAttempted, warning };
  if (x11 && !wayland) return { backend: "x11", reason: "x11_only", fallbackAttempted, warning };
  if (!wayland && !x11) return { backend: "auto", reason: "headless", fallbackAttempted, warning };

  return nvidiaRisk
    ? { backend: "x11", reason: "nvidia_risk", fallbackAttempted, warning }
    : { backend: "wayland", reason: "wayland_preferred", fallbackAttempted, warning };
}

export function applyGraphicsPolicy(
  policy: GraphicsPolicy,
  electronApp: {
    commandLine: { appendSwitch(name: string, value?: string): void };
    disableHardwareAcceleration(): void;
  },
): void {
  if (policy.backend === "software") {
    if (policy.softwarePlatform) {
      electronApp.commandLine.appendSwitch("ozone-platform", policy.softwarePlatform);
    }
    electronApp.commandLine.appendSwitch("use-gl", "angle");
    electronApp.commandLine.appendSwitch("use-angle", "swiftshader");
  } else if (policy.backend === "wayland" || policy.backend === "x11") {
    electronApp.commandLine.appendSwitch("ozone-platform", policy.backend);
  }
}

export function graphicsRelaunchArguments(argv: readonly string[]): string[] {
  const retained = argv.filter((argument) =>
    argument !== GRAPHICS_FALLBACK_MARKER && !argument.startsWith(BACKEND_PREFIX)
  );
  return [...retained, `${BACKEND_PREFIX}software`, GRAPHICS_FALLBACK_MARKER];
}

function isGraphicsBackend(value: string | undefined): value is GraphicsBackend {
  return value === "auto" || value === "wayland" || value === "x11" || value === "software";
}

function normalized(value: string | undefined): string {
  return value?.trim().toLowerCase() ?? "";
}

function nonempty(value: string | undefined): boolean {
  return normalized(value).length > 0;
}

function availableSoftwarePlatform(environment: GraphicsEnvironment): "wayland" | "x11" | undefined {
  if (nonempty(environment.waylandDisplay)) return "wayland";
  if (nonempty(environment.x11Display)) return "x11";
  return undefined;
}
