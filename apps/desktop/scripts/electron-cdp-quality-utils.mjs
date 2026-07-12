export const CDP_QUALITY_ROUTES = Object.freeze([
  Object.freeze({ path: "/", pathPrefix: "/", heading: "What are we working on?" }),
  Object.freeze({ path: "/projects", pathPrefix: "/projects", heading: "Projects" }),
  Object.freeze({ path: "/activity", pathPrefix: "/activity", heading: "Activity" }),
  Object.freeze({ path: "/library", pathPrefix: "/library", heading: "Library" }),
  Object.freeze({ path: "/automations", pathPrefix: "/automations", heading: "Automation definitions" }),
  Object.freeze({ path: "/extensions", pathPrefix: "/extensions", heading: "Extensions" }),
  Object.freeze({
    path: "/settings",
    pathPrefix: "/settings",
    heading: "Settings",
    pendingText: "Checking credential status",
    expectedAxHeadings: Object.freeze(["Account"]),
    expectedAxNavigationNames: Object.freeze(["Settings sections"]),
  }),
  Object.freeze({
    path: "/setup",
    pathPrefix: "/setup",
    heading: "Set up Grok Desktop",
    expectedAxHeadings: Object.freeze(["Connect Grok Build"]),
    expectedAxNavigationNames: Object.freeze(["Setup progress"]),
  }),
]);

export const CDP_QUALITY_VIEWPORTS = Object.freeze([
  Object.freeze({ label: "desktop", width: 1440, height: 900 }),
  Object.freeze({ label: "narrow", width: 640, height: 900 }),
]);

const INTERACTIVE_AX_ROLES = new Set([
  "button",
  "checkbox",
  "combobox",
  "link",
  "radio",
  "searchbox",
  "slider",
  "spinbutton",
  "switch",
  "tab",
  "textbox",
]);

export function assertRouteProbe(route, probe) {
  if (!isRouteWithinPrefix(probe.path, route.pathPrefix)) {
    throw new Error(`route ${route.path} resolved outside its canonical path`);
  }
  if (probe.heading !== route.heading) {
    throw new Error(`route ${route.path} did not render its expected primary heading`);
  }
  if (!probe.rootPopulated) throw new Error(`route ${route.path} rendered an empty application root`);
  if (probe.fatalBridgeScreen) throw new Error(`route ${route.path} displayed the fatal bridge-unavailable screen`);
  if (probe.visibleAlertCount !== 0) throw new Error(`route ${route.path} rendered an unexpected visible error alert`);
  if ((probe.busyRegionCount ?? 0) !== 0 || probe.pendingMarkerVisible === true) {
    throw new Error(`route ${route.path} did not finish its read-only loading state`);
  }
}

export function isRouteProbeReady(route, probe) {
  return isRouteWithinPrefix(probe.path, route.pathPrefix)
    && probe.heading === route.heading
    && (probe.busyRegionCount ?? 0) === 0
    && probe.pendingMarkerVisible !== true;
}

export function summarizeAccessibilityTree(nodes) {
  const summary = {
    headings: [],
    mainCount: 0,
    navigationNames: [],
    unnamedInteractiveRoles: [],
  };
  if (!Array.isArray(nodes)) return summary;

  for (const node of nodes) {
    if (!node || node.ignored === true) continue;
    const role = node.role?.value;
    const name = typeof node.name?.value === "string" ? node.name.value.trim() : "";
    if (role === "main") summary.mainCount += 1;
    if (role === "heading" && name) summary.headings.push(name);
    if (role === "navigation" && name) summary.navigationNames.push(name);
    if (INTERACTIVE_AX_ROLES.has(role) && !name) summary.unnamedInteractiveRoles.push(role);
  }
  return summary;
}

export function assertAccessibilityProbe(domProbe, axNodes, expectations = {}) {
  if (domProbe.mainCount !== 1) throw new Error("renderer must expose exactly one main landmark");
  if (!domProbe.skipLinkTargetsMain) throw new Error("renderer skip link does not target the main landmark");

  const summary = summarizeAccessibilityTree(axNodes);
  if (summary.mainCount !== 1) throw new Error("accessibility tree must expose exactly one main landmark");
  if (!summary.navigationNames.includes("Primary navigation")) {
    throw new Error("accessibility tree is missing the named primary navigation landmark");
  }
  if (!summary.headings.includes(domProbe.heading)) {
    throw new Error("accessibility tree is missing the visible primary heading");
  }
  for (const heading of expectations.headings ?? []) {
    if (!summary.headings.includes(heading)) {
      throw new Error(`accessibility tree is missing the expected ${heading} heading`);
    }
  }
  for (const navigationName of expectations.navigationNames ?? []) {
    if (!summary.navigationNames.includes(navigationName)) {
      throw new Error(`accessibility tree is missing the named ${navigationName} navigation landmark`);
    }
  }
  if (summary.unnamedInteractiveRoles.length > 0) {
    throw new Error(`accessibility tree contains ${summary.unnamedInteractiveRoles.length} unnamed interactive controls`);
  }
  return summary;
}

export function assertNoHorizontalOverflow(probe) {
  const tolerance = 1;
  if (probe.viewportWidth !== probe.expectedWidth) {
    throw new Error(`${probe.label} viewport override did not take effect`);
  }
  if (probe.documentScrollWidth > probe.documentClientWidth + tolerance
    || probe.bodyScrollWidth > probe.bodyClientWidth + tolerance
    || probe.outsideViewportCount > 0) {
    throw new Error(`${probe.label} viewport has horizontal page overflow`);
  }
}

export function cssTimeListMaximumMilliseconds(value) {
  if (typeof value !== "string" || value.trim() === "") return Number.NaN;
  const durations = value.split(",").map((part) => {
    const token = part.trim();
    if (/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?ms$/iu.test(token)) return Number.parseFloat(token);
    if (/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?s$/iu.test(token)) return Number.parseFloat(token) * 1000;
    return Number.NaN;
  });
  return durations.some(Number.isNaN) ? Number.NaN : Math.max(...durations);
}

export function assertReducedMotionProbe(probe) {
  if (!probe.prefersReducedMotion) throw new Error("reduced-motion CDP emulation did not take effect");
  const animationMs = cssTimeListMaximumMilliseconds(probe.animationDuration);
  const transitionMs = cssTimeListMaximumMilliseconds(probe.transitionDuration);
  if (!Number.isFinite(animationMs) || !Number.isFinite(transitionMs)) {
    throw new Error("reduced-motion probe returned invalid computed durations");
  }
  if (animationMs > 0.1 || transitionMs > 0.1 || probe.scrollBehavior !== "auto") {
    throw new Error("renderer does not collapse motion under the reduced-motion preference");
  }
}

export function assertNoUnexpectedConsoleEntries(entries) {
  if (!Array.isArray(entries) || entries.length === 0) return;
  const sources = new Set(entries.map((entry) => entry.source));
  throw new Error(`renderer emitted ${entries.length} unexpected error event(s) from ${[...sources].toSorted().join(", ")}`);
}

function isRouteWithinPrefix(path, prefix) {
  if (prefix === "/") return path === "/";
  return path === prefix || path.startsWith(`${prefix}/`) || path.startsWith(`${prefix}?`);
}
