import type { DesktopNavigationRoute } from "../src/contracts/bridge.js";

/** The only public activation-link contract understood by this desktop build. */
export const DESKTOP_DEEP_LINK_VERSION = 1 as const;

const PUBLIC_DEEP_LINK_PREFIX = "grok-desktop://open/v1/";
const MAX_DEEP_LINK_BYTES = 256;
const MAX_ENTITY_ID_BYTES = 128;

const topLevelRendererHashes = {
  home: "#/",
  projects: "#/projects",
  activity: "#/activity",
  library: "#/library",
  automations: "#/automations",
  extensions: "#/extensions",
  settings: "#/settings",
} as const;

export type DesktopTopLevelDeepLinkRoute = keyof typeof topLevelRendererHashes;

/**
 * Closed v1 activation route union.
 *
 * Links can select an existing view or an opaque daemon-owned entity only.
 * They intentionally cannot carry prompts, commands, file paths, or URLs.
 */
export type DesktopDeepLinkV1 = DesktopNavigationRoute;

export type DesktopDeepLink = DesktopDeepLinkV1;

export type DesktopRendererHash =
  | (typeof topLevelRendererHashes)[DesktopTopLevelDeepLinkRoute]
  | `#/projects/${string}`
  | `#/conversations/${string}`;

/**
 * Parses an untrusted operating-system activation argument without performing
 * navigation or any other state change. Invalid and non-canonical inputs fail
 * closed rather than being repaired or partially interpreted.
 */
export function parseDesktopDeepLink(value: unknown): DesktopDeepLink | null {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_DEEP_LINK_BYTES) return null;
  if (Buffer.byteLength(value, "utf8") > MAX_DEEP_LINK_BYTES || containsControlCharacter(value)) return null;

  // Percent escapes are unnecessary for the deliberately ASCII-only grammar.
  // Rejecting every escape also closes encoded separator, traversal, and
  // double-decoding ambiguities before URL normalization can hide them.
  if (value.includes("%") || value.includes("\\") || value.includes("?") || value.includes("#")) return null;
  if (!value.startsWith(PUBLIC_DEEP_LINK_PREFIX)) return null;

  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return null;
  }

  if (
    parsed.href !== value
    || parsed.protocol !== "grok-desktop:"
    || parsed.hostname !== "open"
    || parsed.username !== ""
    || parsed.password !== ""
    || parsed.port !== ""
    || parsed.search !== ""
    || parsed.hash !== ""
  ) {
    return null;
  }

  const segments = parsed.pathname.split("/").slice(1);
  if (segments.length === 2 && segments[0] === "v1" && isTopLevelRoute(segments[1])) {
    return Object.freeze({ version: DESKTOP_DEEP_LINK_VERSION, route: segments[1] });
  }

  if (segments.length !== 3 || segments[0] !== "v1") return null;
  const [, collection, identifier] = segments;
  if (collection === "projects" && isEntityId(identifier, "project-")) {
    return Object.freeze({ version: DESKTOP_DEEP_LINK_VERSION, route: "project", projectId: identifier });
  }
  if (collection === "conversations" && isEntityId(identifier, "thread-")) {
    return Object.freeze({ version: DESKTOP_DEEP_LINK_VERSION, route: "conversation", threadId: identifier });
  }
  return null;
}

/**
 * Selects the single valid activation link from an untrusted process argv.
 * Multiple valid links are ambiguous and fail closed, even when identical.
 */
export function parseDesktopDeepLinkFromArgv(argv: readonly unknown[]): DesktopDeepLink | null {
  let selected: DesktopDeepLink | null = null;
  for (const argument of argv) {
    const parsed = parseDesktopDeepLink(argument);
    if (!parsed) continue;
    if (selected) return null;
    selected = parsed;
  }
  return selected;
}

/** True when argv appears to be an OS activation attempt for this scheme. */
export function hasDesktopDeepLinkArgument(argv: readonly unknown[]): boolean {
  return argv.some((argument) => typeof argument === "string"
    && argument.slice(0, "grok-desktop:".length).toLowerCase() === "grok-desktop:");
}

/** Maps an already parsed link to the existing renderer's closed HashRouter path. */
export function rendererHashForDesktopDeepLink(link: DesktopDeepLink): DesktopRendererHash {
  if (link.route === "project") return `#/projects/${link.projectId}`;
  if (link.route === "conversation") return `#/conversations/${link.threadId}`;
  return topLevelRendererHashes[link.route];
}

function isTopLevelRoute(value: string | undefined): value is DesktopTopLevelDeepLinkRoute {
  return value !== undefined && Object.hasOwn(topLevelRendererHashes, value);
}

function isEntityId(value: string | undefined, prefix: "project-" | "thread-"): value is string {
  if (
    value === undefined
    || value.length <= prefix.length
    || value.length > MAX_ENTITY_ID_BYTES
    || Buffer.byteLength(value, "utf8") > MAX_ENTITY_ID_BYTES
    || !value.startsWith(prefix)
  ) {
    return false;
  }
  return /^[A-Za-z0-9_-]+$/.test(value.slice(prefix.length));
}

function containsControlCharacter(value: string): boolean {
  return Array.from(value).some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
  });
}
