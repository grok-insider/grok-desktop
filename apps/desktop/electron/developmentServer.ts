const loopbackHosts = new Set(["127.0.0.1", "[::1]"]);

export function resolveDevelopmentServerUrl(isPackaged: boolean, rawValue: string | undefined): string | undefined {
  if (isPackaged || !rawValue) return undefined;
  try {
    const value = new URL(rawValue);
    if (value.protocol !== "http:" || !loopbackHosts.has(value.hostname)) return undefined;
    if (value.username || value.password || value.search || value.hash || value.pathname !== "/") return undefined;
    if (!value.port || Number(value.port) < 1 || Number(value.port) > 65_535) return undefined;
    return value.origin;
  } catch {
    return undefined;
  }
}
