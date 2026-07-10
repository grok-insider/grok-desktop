import { isIP } from "node:net";

const MAX_EXTERNAL_URL_BYTES = 8_192;
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const DNS_TOP_LEVEL_LABEL = /^[a-z]{2,63}$/;
const BLOCKED_HOST_SUFFIXES = [
  "localhost",
  "local",
  "localdomain",
  "home.arpa",
  "internal",
  "intranet",
  "lan",
  "home",
  "corp",
  "invalid",
  "test",
  "example",
  "onion",
] as const;

/**
 * Parses a renderer-provided navigation target into the one form Electron main
 * may hand to the operating-system browser.
 *
 * Canonical serialization is intentionally strict: the caller must supply the
 * exact WHATWG serialization (including the root slash, lowercase scheme and
 * host, and no default port). This keeps alternate URL spellings from
 * acquiring different meanings in another parser at the shell boundary.
 */
export function parseExternalHttpsUrl(value: unknown): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || Buffer.byteLength(value, "utf8") > MAX_EXTERNAL_URL_BYTES
    || hasControlOrWhitespace(value)
  ) {
    throw new TypeError("external URL is invalid");
  }

  let candidate: URL;
  try {
    candidate = new URL(value);
  } catch {
    throw new TypeError("external URL is invalid");
  }

  if (
    candidate.protocol !== "https:"
    || candidate.username !== ""
    || candidate.password !== ""
    || candidate.port !== ""
    || candidate.href !== value
    || !hasCanonicalPercentEscapes(value)
  ) {
    throw new TypeError("external URL must be canonical credential-free HTTPS");
  }

  const hostname = candidate.hostname;
  const unbracketedHostname = hostname.startsWith("[") && hostname.endsWith("]")
    ? hostname.slice(1, -1)
    : hostname;
  if (isIP(unbracketedHostname) !== 0) {
    throw new TypeError("external URL must not use an IP literal");
  }

  const labels = hostname.split(".");
  if (
    hostname.length > 253
    || labels.length < 2
    || labels.some((label) => !DNS_LABEL.test(label) || label.startsWith("xn--"))
    || !DNS_TOP_LEVEL_LABEL.test(labels.at(-1) ?? "")
  ) {
    throw new TypeError("external URL host is ambiguous");
  }
  if (BLOCKED_HOST_SUFFIXES.some((suffix) => hostname === suffix || hostname.endsWith(`.${suffix}`))) {
    throw new TypeError("external URL host is local or private");
  }

  return candidate.href;
}

function hasControlOrWhitespace(value: string): boolean {
  return Array.from(value).some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x20
      || (codePoint >= 0x7f && codePoint <= 0x9f)
      || /\s/u.test(character);
  });
}

function hasCanonicalPercentEscapes(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    if (value[index] !== "%") continue;
    const escape = value.slice(index + 1, index + 3);
    if (!/^[0-9A-F]{2}$/.test(escape)) return false;
    const byte = Number.parseInt(escape, 16);
    if (
      byte <= 0x1f
      || byte === 0x7f
      || isUnreservedUrlByte(byte)
    ) {
      return false;
    }
    index += 2;
  }
  return true;
}

function isUnreservedUrlByte(byte: number): boolean {
  return (byte >= 0x41 && byte <= 0x5a)
    || (byte >= 0x61 && byte <= 0x7a)
    || (byte >= 0x30 && byte <= 0x39)
    || byte === 0x2d
    || byte === 0x2e
    || byte === 0x5f
    || byte === 0x7e;
}
