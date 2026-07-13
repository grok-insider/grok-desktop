import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";

export const DEFAULT_CDP_HOST = "127.0.0.1";
export const DEFAULT_CDP_PORT = 9250;
export const DEFAULT_CDP_PROFILE = "qa-local";
export const DEFAULT_CDP_TIMEOUT_MS = 15_000;

export function developmentInstallationId(profile) {
  return `cdp-${parseProfileName(profile)}`;
}

export function developmentNativeBuildArguments() {
  return [
    "build", "--locked",
    "--package", "grok-daemon",
    "--package", "grok-host-tools-mcp",
    "--features", "grok-daemon/debug-acp-descriptor",
  ];
}

const profilePattern = /^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,62}[A-Za-z0-9])?$/;

export function parseDevCdpArguments(arguments_) {
  const values = parseNamedArguments(arguments_, new Set(["profile", "port"]));
  return {
    profile: parseProfileName(values.get("profile") ?? DEFAULT_CDP_PROFILE),
    port: parsePort(values.get("port") ?? String(DEFAULT_CDP_PORT)),
  };
}

export function parseCdpProbeArguments(arguments_) {
  const values = parseNamedArguments(arguments_, new Set(["port", "timeout-ms"]));
  return {
    port: parsePort(values.get("port") ?? String(DEFAULT_CDP_PORT)),
    timeoutMs: parseTimeout(values.get("timeout-ms") ?? String(DEFAULT_CDP_TIMEOUT_MS)),
  };
}

export function resolveCdpProfileDirectory(
  profile,
  environment = process.env,
  platform = process.platform,
  homeDirectory = os.homedir(),
) {
  const safeProfile = parseProfileName(profile);
  const paths = platform === "win32" ? path.win32 : path.posix;
  let stateRoot;
  if (platform === "win32") {
    stateRoot = environment.LOCALAPPDATA ?? paths.join(homeDirectory, "AppData", "Local");
  } else if (platform === "darwin") {
    stateRoot = paths.join(homeDirectory, "Library", "Application Support");
  } else {
    stateRoot = environment.XDG_STATE_HOME ?? paths.join(homeDirectory, ".local", "state");
  }
  if (!paths.isAbsolute(stateRoot)) throw new Error("the local state directory must be absolute");
  return paths.join(stateRoot, "grok-desktop", "cdp-profiles", safeProfile);
}

export function productionRendererBuildEnvironment(environment = process.env) {
  const sanitized = { ...environment };
  delete sanitized.VITE_BROWSER_PREVIEW;
  delete sanitized.VITE_DEV_SERVER_URL;
  return sanitized;
}

export async function assertLoopbackPortAvailable(port) {
  const validatedPort = parsePort(String(port));
  const server = net.createServer();
  server.unref();
  try {
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen({ host: DEFAULT_CDP_HOST, port: validatedPort, exclusive: true }, resolve);
    });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`CDP port ${validatedPort} is not available on ${DEFAULT_CDP_HOST}: ${detail}`, { cause: error });
  } finally {
    if (server.listening) {
      await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
    }
  }
}

export async function waitForElectronCdp(port, timeoutMs = DEFAULT_CDP_TIMEOUT_MS) {
  const validatedPort = parsePort(String(port));
  const validatedTimeout = parseTimeout(String(timeoutMs));
  const deadline = Date.now() + validatedTimeout;
  let lastError;

  while (Date.now() < deadline) {
    try {
      const version = await requestCdpJson(validatedPort, "/json/version");
      const targets = await requestCdpJson(validatedPort, "/json/list");
      return validateElectronCdpDiscovery(version, targets, validatedPort);
    } catch (error) {
      lastError = error;
      await delay(150);
    }
  }

  const detail = lastError instanceof Error ? lastError.message : "CDP did not respond";
  throw new Error(`Electron CDP was not ready on ${DEFAULT_CDP_HOST}:${validatedPort} within ${validatedTimeout}ms: ${detail}`, { cause: lastError });
}

export function validateElectronCdpDiscovery(version, targets, port) {
  if (!isRecord(version) || typeof version.Browser !== "string" || typeof version.webSocketDebuggerUrl !== "string") {
    throw new Error("CDP version response is missing browser discovery fields");
  }
  assertLoopbackWebSocket(version.webSocketDebuggerUrl, port);
  if (!Array.isArray(targets)) throw new Error("CDP target response is not an array");
  const target = targets.find((candidate) => isRecord(candidate)
    && candidate.type === "page"
    && typeof candidate.url === "string"
    && candidate.url.startsWith("grok-desktop://app/"));
  if (!isRecord(target) || typeof target.webSocketDebuggerUrl !== "string" || typeof target.id !== "string") {
    throw new Error("CDP did not expose the Grok Desktop renderer target");
  }
  assertLoopbackWebSocket(target.webSocketDebuggerUrl, port);
  return {
    browser: version.Browser,
    targetId: target.id,
    targetUrl: target.url,
    webSocketDebuggerUrl: target.webSocketDebuggerUrl,
  };
}

function parseNamedArguments(arguments_, allowed) {
  const values = new Map();
  let separatorSeen = false;
  for (let index = 0; index < arguments_.length; index += 1) {
    const token = arguments_[index];
    if (token === "--" && !separatorSeen) {
      separatorSeen = true;
      continue;
    }
    if (!token.startsWith("--")) throw new Error(`unexpected argument: ${token}`);
    const separator = token.indexOf("=");
    const name = token.slice(2, separator === -1 ? undefined : separator);
    if (!allowed.has(name)) throw new Error(`unknown option: --${name}`);
    if (values.has(name)) throw new Error(`option --${name} may be provided only once`);
    const value = separator === -1 ? arguments_[index + 1] : token.slice(separator + 1);
    if (separator === -1) index += 1;
    if (!value || value.startsWith("--")) throw new Error(`option --${name} requires a value`);
    values.set(name, value);
  }
  return values;
}

function parseProfileName(value) {
  if (!profilePattern.test(value) || value === "." || value === ".." || value.includes("..")) {
    throw new Error("profile must be 1-64 ASCII letters, numbers, dots, underscores, or hyphens without traversal segments");
  }
  return value;
}

function parsePort(value) {
  if (!/^[0-9]+$/.test(value)) throw new Error("CDP port must be an integer");
  const port = Number(value);
  if (!Number.isSafeInteger(port) || port < 1024 || port > 65_535) {
    throw new Error("CDP port must be between 1024 and 65535");
  }
  return port;
}

function parseTimeout(value) {
  if (!/^[0-9]+$/.test(value)) throw new Error("CDP timeout must be an integer");
  const timeout = Number(value);
  if (!Number.isSafeInteger(timeout) || timeout < 1_000 || timeout > 120_000) {
    throw new Error("CDP timeout must be between 1000 and 120000 milliseconds");
  }
  return timeout;
}

function requestCdpJson(port, pathname) {
  return new Promise((resolve, reject) => {
    const request = http.get({ host: DEFAULT_CDP_HOST, port, path: pathname, timeout: 1_000 }, (response) => {
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`CDP ${pathname} returned HTTP ${response.statusCode ?? "unknown"}`));
        return;
      }
      const chunks = [];
      let length = 0;
      response.on("data", (chunk) => {
        length += chunk.length;
        if (length > 1024 * 1024) {
          request.destroy(new Error(`CDP ${pathname} exceeded the response limit`));
          return;
        }
        chunks.push(chunk);
      });
      response.on("end", () => {
        try {
          resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
        } catch (error) {
          reject(new Error(`CDP ${pathname} returned invalid JSON`, { cause: error }));
        }
      });
    });
    request.once("timeout", () => request.destroy(new Error(`CDP ${pathname} timed out`)));
    request.once("error", reject);
  });
}

function assertLoopbackWebSocket(value, port) {
  let url;
  try {
    url = new URL(value);
  } catch (error) {
    throw new Error("CDP returned an invalid WebSocket URL", { cause: error });
  }
  if (url.protocol !== "ws:" || url.hostname !== DEFAULT_CDP_HOST || Number(url.port) !== port) {
    throw new Error("CDP WebSocket discovery must remain on the configured loopback port");
  }
}

function isRecord(value) {
  return typeof value === "object" && value !== null;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
