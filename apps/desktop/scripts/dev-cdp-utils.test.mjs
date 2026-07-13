import assert from "node:assert/strict";
import net from "node:net";
import path from "node:path";
import test from "node:test";
import {
  DEFAULT_CDP_PORT,
  DEFAULT_CDP_PROFILE,
  assertLoopbackPortAvailable,
  developmentNativeBuildArguments,
  developmentInstallationId,
  parseCdpProbeArguments,
  parseDevCdpArguments,
  productionRendererBuildEnvironment,
  resolveCdpProfileDirectory,
  validateElectronCdpDiscovery,
} from "./dev-cdp-utils.mjs";

test("derives a bounded daemon installation id from the validated CDP profile", () => {
  assert.equal(developmentInstallationId("qa-local"), "cdp-qa-local");
  assert.throws(() => developmentInstallationId("../shared"), /profile/);
});

test("development launchers build the daemon with the official ACP descriptor", () => {
  assert.deepEqual(developmentNativeBuildArguments(), [
    "build", "--locked",
    "--package", "grok-daemon",
    "--package", "grok-host-tools-mcp",
    "--features", "grok-daemon/debug-acp-descriptor",
  ]);
});

test("parses bounded CDP launcher and probe options", () => {
  assert.deepEqual(parseDevCdpArguments([]), { profile: DEFAULT_CDP_PROFILE, port: DEFAULT_CDP_PORT });
  assert.deepEqual(parseDevCdpArguments(["--profile", "qa-arm64", "--port=9321"]), { profile: "qa-arm64", port: 9321 });
  assert.deepEqual(parseDevCdpArguments(["--", "--profile", "qa-root-script"]), { profile: "qa-root-script", port: DEFAULT_CDP_PORT });
  assert.deepEqual(parseCdpProbeArguments(["--timeout-ms", "20000"]), { port: DEFAULT_CDP_PORT, timeoutMs: 20_000 });
  assert.throws(() => parseDevCdpArguments(["--profile", "../personal"]), /profile/);
  assert.throws(() => parseDevCdpArguments(["--port", "80"]), /between 1024/);
  assert.throws(() => parseDevCdpArguments(["--unknown", "value"]), /unknown option/);
  assert.throws(() => parseCdpProbeArguments(["--timeout-ms", "999"]), /between 1000/);
});

test("places named profiles in private OS state roots", () => {
  assert.equal(
    resolveCdpProfileDirectory("qa-local", { XDG_STATE_HOME: "/state" }, "linux", "/home/test"),
    path.join("/state", "grok-desktop", "cdp-profiles", "qa-local"),
  );
  assert.equal(
    resolveCdpProfileDirectory("qa-local", {}, "win32", "C:\\Users\\Test"),
    path.win32.join("C:\\Users\\Test", "AppData", "Local", "grok-desktop", "cdp-profiles", "qa-local"),
  );
  assert.throws(() => resolveCdpProfileDirectory("qa", { XDG_STATE_HOME: "relative" }, "linux", "/home/test"), /absolute/);
});

test("removes preview and development-server inputs from production renderer builds", () => {
  assert.deepEqual(productionRendererBuildEnvironment({
    PATH: "/bin",
    VITE_BROWSER_PREVIEW: "true",
    VITE_DEV_SERVER_URL: "https://untrusted.invalid",
  }), { PATH: "/bin" });
});

test("preflight rejects a CDP port already owned by another listener", async (t) => {
  const listener = net.createServer();
  await new Promise((resolve, reject) => {
    listener.once("error", reject);
    listener.listen({ host: "127.0.0.1", port: 0 }, resolve);
  });
  t.after(() => listener.close());
  const address = listener.address();
  assert.ok(address && typeof address === "object");
  await assert.rejects(assertLoopbackPortAvailable(address.port), /not available/);
});

test("accepts only loopback discovery for the Grok Desktop page target", () => {
  const version = {
    Browser: "Chrome/123",
    webSocketDebuggerUrl: "ws://127.0.0.1:9250/devtools/browser/browser-id",
  };
  const targets = [{
    id: "target-id",
    type: "page",
    url: "grok-desktop://app/index.html",
    webSocketDebuggerUrl: "ws://127.0.0.1:9250/devtools/page/target-id",
  }];
  assert.deepEqual(validateElectronCdpDiscovery(version, targets, 9250), {
    browser: "Chrome/123",
    targetId: "target-id",
    targetUrl: "grok-desktop://app/index.html",
    webSocketDebuggerUrl: "ws://127.0.0.1:9250/devtools/page/target-id",
  });
  assert.throws(
    () => validateElectronCdpDiscovery({ ...version, webSocketDebuggerUrl: "ws://0.0.0.0:9250/devtools/browser/id" }, targets, 9250),
    /loopback/,
  );
  assert.throws(() => validateElectronCdpDiscovery(version, [{ ...targets[0], url: "https://example.com" }], 9250), /renderer target/);
});
