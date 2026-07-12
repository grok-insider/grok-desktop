#!/usr/bin/env node

import {
  DEFAULT_CDP_HOST,
  parseCdpProbeArguments,
  waitForElectronCdp,
} from "./dev-cdp-utils.mjs";
import {
  CDP_QUALITY_ROUTES,
  CDP_QUALITY_VIEWPORTS,
  assertAccessibilityProbe,
  assertNoHorizontalOverflow,
  assertNoUnexpectedConsoleEntries,
  assertReducedMotionProbe,
  assertRouteProbe,
  isRouteProbeReady,
} from "./electron-cdp-quality-utils.mjs";
import {
  CDP_SEMANTIC_CONTRAST_PAIRS,
  assertFocusIndicatorProbe,
  assertSemanticContrastProbe,
} from "./electron-cdp-contrast-utils.mjs";

const EXPECTED_PRODUCTION_CSP = "default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const MAX_CDP_MESSAGE_BYTES = 4 * 1024 * 1024;

async function main() {
  try {
    const options = parseCdpProbeArguments(process.argv.slice(2));
    const discovery = await waitForElectronCdp(options.port, options.timeoutMs);
    const report = await probeProductionRenderer(discovery.webSocketDebuggerUrl, options.timeoutMs);

    process.stdout.write([
      `Electron CDP quality probe passed at http://${DEFAULT_CDP_HOST}:${options.port}`,
      `Browser: ${discovery.browser}`,
      `Target: ${discovery.targetUrl}`,
      `Document: ${report.title}`,
      `Read-only routes: ${report.routeCount}`,
      `Responsive viewports: ${report.viewportLabels.join(", ")}`,
      `Responsive route/viewport probes: ${report.responsiveProbeCount}`,
      `AX and reduced-motion routes: ${report.deepRouteCount}`,
      `Semantic contrast pairs: ${report.contrastPairCount}`,
      "",
    ].join("\n"));
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    process.stderr.write(`test:e2e:electron: ${detail}\n`);
    process.exitCode = 1;
  }
}

async function probeProductionRenderer(webSocketDebuggerUrl, timeoutMs) {
  const session = new CdpSession(webSocketDebuggerUrl, timeoutMs);
  const unexpectedConsoleEntries = [];
  let originalHash;
  let operationError;
  let report;
  const cleanupFailures = [];

  try {
    await session.connect();
    session.onEvent((message) => collectUnexpectedConsoleEntry(message, unexpectedConsoleEntries));
    await session.command("Runtime.enable");
    await session.command("Log.enable");

    const renderer = await waitForRenderer(session, timeoutMs);
    assertProductionRenderer(renderer);
    originalHash = await session.evaluate("window.location.hash");
    if (typeof originalHash !== "string" || originalHash.length > 2048) {
      throw new Error("renderer returned an invalid current route");
    }

    let viewportLabels = [];
    let responsiveProbeCount = 0;
    for (const route of CDP_QUALITY_ROUTES) {
      await navigateToReadOnlyRoute(session, route, timeoutMs);
      assertRouteProbe(route, await routeProbe(session, route));

      const routeViewportLabels = await probeResponsiveLayouts(session);
      if (viewportLabels.length === 0) viewportLabels = routeViewportLabels;
      responsiveProbeCount += routeViewportLabels.length;

      const accessibility = await probeAccessibility(session);
      assertAccessibilityProbe(accessibility.dom, accessibility.nodes, {
        headings: route.expectedAxHeadings,
        navigationNames: route.expectedAxNavigationNames,
      });
      await probeReducedMotion(session);
    }

    const contrast = await probeSemanticContrast(session);
    assertSemanticContrastProbe(contrast.pairs);
    assertFocusIndicatorProbe(contrast.focusIndicator);

    await restoreRoute(session, originalHash);
    await settleRenderer(session);
    await delay(150);
    assertNoUnexpectedConsoleEntries(unexpectedConsoleEntries);

    const policy = await probeContentSecurityPolicy(session);
    if (policy.contentSecurityPolicy !== EXPECTED_PRODUCTION_CSP) {
      throw new Error("renderer is not using the strict production Content Security Policy");
    }
    if (policy.inlineScriptExecuted) {
      throw new Error("renderer Content Security Policy allowed a nonce-less inline script");
    }

    report = {
      title: renderer.title,
      routeCount: CDP_QUALITY_ROUTES.length,
      viewportLabels,
      responsiveProbeCount,
      deepRouteCount: CDP_QUALITY_ROUTES.length,
      contrastPairCount: contrast.pairs.length,
    };
  } catch (error) {
    operationError = error;
  } finally {
    if (session.connected) {
      await bestEffort(session.command("Emulation.clearDeviceMetricsOverride"), cleanupFailures);
      await bestEffort(session.command("Emulation.setEmulatedMedia", { features: [] }), cleanupFailures);
      if (typeof originalHash === "string") {
        await bestEffort(restoreRoute(session, originalHash), cleanupFailures);
      }
      // The deliberate nonce-less CSP sentinel emits one Chromium security-log
      // entry. Clear it so a repeat probe does not treat its own prior evidence
      // as an application error when Log.enable replays the target buffer.
      await bestEffort(session.command("Log.clear"), cleanupFailures);
      session.close();
    }
  }
  if (operationError) throw operationError;
  if (cleanupFailures.length > 0) {
    throw new Error("CDP quality probe could not restore renderer emulation state");
  }
  return report;
}

function assertProductionRenderer(renderer) {
  if (renderer.protocol !== "grok-desktop:") throw new Error("renderer is not using the production grok-desktop protocol");
  if (!renderer.bridgeAvailable) throw new Error("the isolated preload bridge is unavailable");
  if (renderer.fatalBridgeScreen) throw new Error("renderer displayed the fatal bridge-unavailable screen");
  if (renderer.readyState !== "complete") throw new Error(`renderer document is ${renderer.readyState}`);
}

async function waitForRenderer(session, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastResult;
  while (Date.now() < deadline) {
    lastResult = await session.evaluate(`({
      protocol: window.location.protocol,
      title: document.title,
      readyState: document.readyState,
      bridgeAvailable: typeof window.grokDesktop?.request === "function",
      fatalBridgeScreen: document.body.innerText.includes("Desktop bridge unavailable")
    })`);
    if (lastResult?.readyState === "complete" && lastResult.bridgeAvailable) return lastResult;
    await delay(150);
  }
  return lastResult ?? {};
}

async function navigateToReadOnlyRoute(session, route, timeoutMs) {
  await session.evaluate(`window.location.hash = ${JSON.stringify(`#${route.path}`)}`);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const probe = await routeProbe(session, route);
    if (isRouteProbeReady(route, probe)) {
      await settleRenderer(session);
      return;
    }
    await delay(100);
  }
  throw new Error(`route ${route.path} did not settle before the CDP timeout`);
}

function routeProbe(session, route) {
  return session.evaluate(`(() => {
    const visible = (element) => {
      const style = getComputedStyle(element);
      const bounds = element.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && bounds.width > 0 && bounds.height > 0;
    };
    const main = document.querySelector("main");
    const pendingText = ${JSON.stringify(route.pendingText ?? "")};
    return {
      path: window.location.hash.slice(1) || "/",
      heading: main?.querySelector("h1")?.textContent?.trim() ?? "",
      rootPopulated: Boolean(document.querySelector("#root")?.childElementCount),
      fatalBridgeScreen: document.body.innerText.includes("Desktop bridge unavailable"),
      visibleAlertCount: [...document.querySelectorAll('[role="alert"]')].filter(visible).length,
      busyRegionCount: main?.querySelectorAll('[aria-busy="true"]').length ?? 0,
      pendingMarkerVisible: Boolean(pendingText && main?.innerText.includes(pendingText))
    };
  })()`);
}

async function probeResponsiveLayouts(session) {
  const labels = [];
  try {
    for (const viewport of CDP_QUALITY_VIEWPORTS) {
      await session.command("Emulation.setDeviceMetricsOverride", {
        width: viewport.width,
        height: viewport.height,
        deviceScaleFactor: 1,
        mobile: false,
        screenWidth: viewport.width,
        screenHeight: viewport.height,
      });
      await settleRenderer(session);
      const probe = await session.evaluate(`(() => {
        const viewportWidth = window.innerWidth;
        const horizontalContainment = new Set(["auto", "clip", "hidden", "scroll"]);
        const boundedByHorizontalScroller = (element) => {
          let ancestor = element.parentElement;
          while (ancestor && ancestor !== document.body && ancestor !== document.documentElement) {
            if (ancestor.tagName === "MAIN") return false;
            const style = getComputedStyle(ancestor);
            const bounds = ancestor.getBoundingClientRect();
            if (horizontalContainment.has(style.overflowX)
              && bounds.left >= -1
              && bounds.right <= viewportWidth + 1) return true;
            ancestor = ancestor.parentElement;
          }
          return false;
        };
        const outsideViewport = [...document.body.querySelectorAll("*")].filter((element) => {
          const style = getComputedStyle(element);
          const bounds = element.getBoundingClientRect();
          if (style.display === "none" || style.visibility === "hidden" || bounds.width === 0 || bounds.height === 0) return false;
          const outside = bounds.left < -1 || bounds.right > viewportWidth + 1;
          return outside && !boundedByHorizontalScroller(element);
        });
        return {
          label: ${JSON.stringify(viewport.label)},
          expectedWidth: ${viewport.width},
          viewportWidth,
          documentClientWidth: document.documentElement.clientWidth,
          documentScrollWidth: document.documentElement.scrollWidth,
          bodyClientWidth: document.body.clientWidth,
          bodyScrollWidth: document.body.scrollWidth,
          outsideViewportCount: outsideViewport.length
        };
      })()`);
      assertNoHorizontalOverflow(probe);
      labels.push(`${viewport.label} ${viewport.width}x${viewport.height}`);
    }
  } finally {
    await session.command("Emulation.clearDeviceMetricsOverride");
    await settleRenderer(session);
  }
  return labels;
}

async function probeAccessibility(session) {
  const dom = await session.evaluate(`(() => {
    const skipLink = [...document.querySelectorAll('a[href="#main-content"]')]
      .find((element) => element.textContent?.trim() === "Skip to content");
    return {
      mainCount: document.querySelectorAll("main").length,
      heading: document.querySelector("main h1")?.textContent?.trim() ?? "",
      skipLinkTargetsMain: Boolean(skipLink && document.querySelector("#main-content")?.tagName === "MAIN")
    };
  })()`);
  const tree = await session.command("Accessibility.getFullAXTree");
  return { dom, nodes: tree.nodes };
}

async function probeReducedMotion(session) {
  try {
    await session.command("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-reduced-motion", value: "reduce" }],
    });
    await settleRenderer(session);
    const probe = await session.evaluate(`(() => {
      const sentinel = document.createElement("div");
      sentinel.setAttribute("aria-hidden", "true");
      sentinel.style.position = "fixed";
      sentinel.style.width = "0";
      sentinel.style.height = "0";
      sentinel.style.pointerEvents = "none";
      sentinel.style.animationDuration = "1s";
      sentinel.style.scrollBehavior = "smooth";
      sentinel.style.transitionDuration = "1s";
      document.body.append(sentinel);
      const style = getComputedStyle(sentinel);
      const result = {
        prefersReducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
        animationDuration: style.animationDuration,
        transitionDuration: style.transitionDuration,
        scrollBehavior: style.scrollBehavior
      };
      sentinel.remove();
      return result;
    })()`);
    assertReducedMotionProbe(probe);
  } finally {
    await session.command("Emulation.setEmulatedMedia", { features: [] });
  }
}

async function probeSemanticContrast(session) {
  const pairs = await session.evaluate(`(() => {
    const configuredPairs = ${JSON.stringify(CDP_SEMANTIC_CONTRAST_PAIRS)};
    const rootStyle = getComputedStyle(document.documentElement);
    const resolver = document.createElement("span");
    resolver.setAttribute("aria-hidden", "true");
    resolver.style.position = "fixed";
    resolver.style.width = "1px";
    resolver.style.height = "1px";
    resolver.style.clipPath = "inset(50%)";
    resolver.style.pointerEvents = "none";
    document.body.append(resolver);
    try {
      return configuredPairs.map((pair) => {
        resolver.style.color = "var(" + pair.foregroundToken + ")";
        resolver.style.backgroundColor = "var(" + pair.backgroundToken + ")";
        const style = getComputedStyle(resolver);
        return {
          ...pair,
          foregroundDefined: rootStyle.getPropertyValue(pair.foregroundToken).trim().length > 0,
          backgroundDefined: rootStyle.getPropertyValue(pair.backgroundToken).trim().length > 0,
          foregroundColor: style.color,
          backgroundColor: style.backgroundColor
        };
      });
    } finally {
      resolver.remove();
    }
  })()`);

  const sentinelId = "grok-desktop-cdp-focus-contrast-sentinel";
  const sentinelCreated = await session.evaluate(`(() => {
    if (document.getElementById(${JSON.stringify(sentinelId)})) return false;
    const sentinel = document.createElement("button");
    sentinel.id = ${JSON.stringify(sentinelId)};
    sentinel.type = "button";
    sentinel.setAttribute("aria-hidden", "true");
    sentinel.style.position = "fixed";
    sentinel.style.width = "1px";
    sentinel.style.height = "1px";
    sentinel.style.clipPath = "inset(50%)";
    sentinel.style.pointerEvents = "none";
    document.body.append(sentinel);
    return true;
  })()`);
  if (!sentinelCreated) throw new Error("focus contrast sentinel id is already in use");

  let nodeId;
  try {
    await session.command("DOM.enable");
    await session.command("CSS.enable");
    const documentTree = await session.command("DOM.getDocument", { depth: 0, pierce: false });
    const query = await session.command("DOM.querySelector", {
      nodeId: documentTree.root.nodeId,
      selector: `#${sentinelId}`,
    });
    nodeId = query.nodeId;
    if (!Number.isInteger(nodeId) || nodeId <= 0) throw new Error("focus contrast sentinel was not attached");
    await session.command("CSS.forcePseudoState", {
      nodeId,
      forcedPseudoClasses: ["focus", "focus-visible"],
    });
    const focusIndicator = await session.evaluate(`(() => {
      const sentinel = document.getElementById(${JSON.stringify(sentinelId)});
      if (!sentinel) return {};
      const resolver = document.createElement("span");
      resolver.setAttribute("aria-hidden", "true");
      resolver.style.position = "fixed";
      resolver.style.width = "1px";
      resolver.style.height = "1px";
      resolver.style.clipPath = "inset(50%)";
      document.body.append(resolver);
      try {
        const style = getComputedStyle(sentinel);
        const outline = {
          outlineColor: style.outlineColor,
          outlineStyle: style.outlineStyle,
          outlineWidth: style.outlineWidth,
          outlineOffset: style.outlineOffset
        };
        const resolveColor = (property, token) => {
          resolver.style[property] = "var(" + token + ")";
          return getComputedStyle(resolver)[property];
        };
        return {
          ...outline,
          ringColor: resolveColor("color", "--ring"),
          canvasColor: resolveColor("backgroundColor", "--background"),
          cardColor: resolveColor("backgroundColor", "--card")
        };
      } finally {
        resolver.remove();
      }
    })()`);
    return { pairs, focusIndicator };
  } finally {
    if (nodeId) {
      await session.command("CSS.forcePseudoState", { nodeId, forcedPseudoClasses: [] }).catch(() => undefined);
    }
    await session.evaluate(`document.getElementById(${JSON.stringify(sentinelId)})?.remove()`).catch(() => undefined);
  }
}

function probeContentSecurityPolicy(session) {
  return session.evaluate(`(() => {
    const marker = "__grokDesktopCspInlineSentinel";
    delete globalThis[marker];
    const script = document.createElement("script");
    script.textContent = "globalThis." + marker + " = true;";
    document.head.append(script);
    script.remove();
    const inlineScriptExecuted = globalThis[marker] === true;
    delete globalThis[marker];
    return {
      contentSecurityPolicy: document.querySelector('meta[http-equiv="Content-Security-Policy"]')?.content,
      inlineScriptExecuted
    };
  })()`);
}

function restoreRoute(session, hash) {
  return session.evaluate(`window.location.hash = ${JSON.stringify(hash)}`);
}

function settleRenderer(session) {
  return session.evaluate("new Promise((resolve) => setTimeout(() => resolve(true), 75))");
}

function collectUnexpectedConsoleEntry(message, entries) {
  if (message.method === "Runtime.exceptionThrown") {
    entries.push({ source: "exception" });
    return;
  }
  if (message.method === "Runtime.consoleAPICalled"
    && (message.params?.type === "error" || message.params?.type === "assert")) {
    entries.push({ source: `console.${message.params.type}` });
    return;
  }
  if (message.method === "Log.entryAdded" && message.params?.entry?.level === "error") {
    entries.push({ source: "log.error" });
  }
}

async function bestEffort(promise, failures) {
  try {
    await promise;
  } catch (error) {
    failures.push(error);
  }
}

class CdpSession {
  constructor(webSocketDebuggerUrl, timeoutMs) {
    this.webSocketDebuggerUrl = webSocketDebuggerUrl;
    this.timeoutMs = timeoutMs;
    this.connected = false;
    this.nextId = 1;
    this.pending = new Map();
    this.eventListeners = new Set();
  }

  async connect() {
    if (this.connected) return;
    this.socket = new WebSocket(this.webSocketDebuggerUrl);
    this.socket.addEventListener("message", (event) => this.handleMessage(event));
    this.socket.addEventListener("close", () => this.handleClose());
    try {
      await new Promise((resolve, reject) => {
        let settled = false;
        const finish = (callback) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          callback();
        };
        const timer = setTimeout(() => finish(() => reject(new Error("CDP renderer connection timed out"))), this.timeoutMs);
        this.socket.addEventListener("open", () => finish(resolve), { once: true });
        this.socket.addEventListener("error", () => finish(() => reject(new Error("CDP renderer WebSocket connection failed"))), { once: true });
      });
    } catch (error) {
      this.socket.close();
      throw error;
    }
    this.connected = true;
  }

  command(method, params = {}) {
    if (!this.connected || !this.socket) return Promise.reject(new Error("CDP renderer is not connected"));
    const id = this.nextId;
    this.nextId += 1;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP ${method} timed out`));
      }, this.timeoutMs);
      this.pending.set(id, { method, resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const response = await this.command("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (response.exceptionDetails) throw new Error("CDP renderer evaluation failed");
    return response.result?.value;
  }

  onEvent(listener) {
    this.eventListeners.add(listener);
  }

  handleMessage(event) {
    const serialized = String(event.data);
    if (Buffer.byteLength(serialized, "utf8") > MAX_CDP_MESSAGE_BYTES) {
      this.failPending(new Error("CDP renderer message exceeded the response limit"));
      this.close();
      return;
    }
    let message;
    try {
      message = JSON.parse(serialized);
    } catch {
      this.failPending(new Error("CDP renderer returned invalid JSON"));
      this.close();
      return;
    }
    if (typeof message.id === "number") {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(`CDP ${pending.method} failed`));
      else pending.resolve(message.result ?? {});
      return;
    }
    for (const listener of this.eventListeners) listener(message);
  }

  handleClose() {
    this.connected = false;
    this.failPending(new Error("CDP renderer connection closed"));
  }

  failPending(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  close() {
    this.connected = false;
    this.socket?.close();
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

await main();
