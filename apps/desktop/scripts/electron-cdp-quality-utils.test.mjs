import assert from "node:assert/strict";
import test from "node:test";
import {
  CDP_QUALITY_ROUTES,
  assertAccessibilityProbe,
  assertNoHorizontalOverflow,
  assertNoUnexpectedConsoleEntries,
  assertReducedMotionProbe,
  assertRouteProbe,
  cssTimeListMaximumMilliseconds,
  isRouteProbeReady,
  summarizeAccessibilityTree,
} from "./electron-cdp-quality-utils.mjs";

test("includes Settings and Setup in the deterministic route matrix", () => {
  const settings = CDP_QUALITY_ROUTES.find((route) => route.path === "/settings");
  const setup = CDP_QUALITY_ROUTES.find((route) => route.path === "/setup");

  assert.equal(CDP_QUALITY_ROUTES.length, 8);
  assert.deepEqual(settings?.expectedAxHeadings, ["Account"]);
  assert.deepEqual(settings?.expectedAxNavigationNames, ["Settings sections"]);
  assert.equal(settings?.pendingText, "Checking credential status");
  assert.deepEqual(setup?.expectedAxHeadings, ["Connect Grok Build"]);
  assert.deepEqual(setup?.expectedAxNavigationNames, ["Setup progress"]);
});

test("accepts canonical routes and rejects route, heading, root, and alert failures", () => {
  const route = { path: "/projects", pathPrefix: "/projects", heading: "Projects" };
  const valid = {
    path: "/projects/project-1",
    heading: "Projects",
    rootPopulated: true,
    fatalBridgeScreen: false,
    visibleAlertCount: 0,
    busyRegionCount: 0,
    pendingMarkerVisible: false,
  };
  assert.doesNotThrow(() => assertRouteProbe(route, valid));
  assert.throws(() => assertRouteProbe(route, { ...valid, path: "/projectile" }), /canonical path/);
  assert.throws(() => assertRouteProbe(route, { ...valid, heading: "Wrong" }), /primary heading/);
  assert.throws(() => assertRouteProbe(route, { ...valid, rootPopulated: false }), /empty/);
  assert.throws(() => assertRouteProbe(route, { ...valid, visibleAlertCount: 1 }), /error alert/);
  assert.throws(() => assertRouteProbe(route, { ...valid, busyRegionCount: 1 }), /loading state/);
  assert.throws(() => assertRouteProbe(route, { ...valid, pendingMarkerVisible: true }), /loading state/);
  assert.equal(isRouteProbeReady(route, valid), true);
  assert.equal(isRouteProbeReady(route, { ...valid, busyRegionCount: 1 }), false);
  assert.equal(isRouteProbeReady(route, { ...valid, pendingMarkerVisible: true }), false);
});

test("summarizes the accessibility tree and requires named landmarks and controls", () => {
  const nodes = [
    { role: { value: "main" }, name: { value: "" } },
    { role: { value: "navigation" }, name: { value: "Primary navigation" } },
    { role: { value: "heading" }, name: { value: "Home" } },
    { role: { value: "button" }, name: { value: "Send" } },
    { ignored: true, role: { value: "button" }, name: { value: "" } },
  ];
  assert.deepEqual(summarizeAccessibilityTree(nodes), {
    headings: ["Home"],
    mainCount: 1,
    navigationNames: ["Primary navigation"],
    unnamedInteractiveRoles: [],
  });
  assert.doesNotThrow(() => assertAccessibilityProbe(
    { mainCount: 1, skipLinkTargetsMain: true, heading: "Home" },
    nodes,
    { headings: ["Home"], navigationNames: ["Primary navigation"] },
  ));
  assert.throws(
    () => assertAccessibilityProbe(
      { mainCount: 1, skipLinkTargetsMain: true, heading: "Home" },
      [...nodes, { role: { value: "link" }, name: { value: "" } }],
    ),
    /unnamed interactive/,
  );
  assert.throws(
    () => assertAccessibilityProbe(
      { mainCount: 1, skipLinkTargetsMain: true, heading: "Home" },
      nodes,
      { headings: ["Account"] },
    ),
    /Account heading/,
  );
  assert.throws(
    () => assertAccessibilityProbe(
      { mainCount: 1, skipLinkTargetsMain: true, heading: "Home" },
      nodes,
      { navigationNames: ["Settings sections"] },
    ),
    /Settings sections navigation/,
  );
});

test("bounds page overflow while allowing the one-pixel measurement tolerance", () => {
  const valid = {
    label: "narrow",
    expectedWidth: 640,
    viewportWidth: 640,
    documentClientWidth: 640,
    documentScrollWidth: 641,
    bodyClientWidth: 640,
    bodyScrollWidth: 640,
    outsideViewportCount: 0,
  };
  assert.doesNotThrow(() => assertNoHorizontalOverflow(valid));
  assert.throws(() => assertNoHorizontalOverflow({ ...valid, viewportWidth: 639 }), /did not take effect/);
  assert.throws(() => assertNoHorizontalOverflow({ ...valid, documentScrollWidth: 642 }), /horizontal/);
  assert.throws(() => assertNoHorizontalOverflow({ ...valid, outsideViewportCount: 1 }), /horizontal/);
});

test("parses computed CSS time lists and enforces the reduced-motion ceiling", () => {
  assert.equal(cssTimeListMaximumMilliseconds("0.01ms"), 0.01);
  assert.equal(cssTimeListMaximumMilliseconds("0s, 0.00001s"), 0.01);
  assert.equal(cssTimeListMaximumMilliseconds("1e-05s"), 0.01);
  assert.ok(Number.isNaN(cssTimeListMaximumMilliseconds("calc(1s)")));
  const valid = {
    prefersReducedMotion: true,
    animationDuration: "0.01ms",
    transitionDuration: "0.00001s",
    scrollBehavior: "auto",
  };
  assert.doesNotThrow(() => assertReducedMotionProbe(valid));
  assert.throws(() => assertReducedMotionProbe({ ...valid, transitionDuration: "150ms" }), /does not collapse/);
  assert.throws(() => assertReducedMotionProbe({ ...valid, prefersReducedMotion: false }), /did not take effect/);
});

test("reports console failures without reproducing renderer messages", () => {
  assert.doesNotThrow(() => assertNoUnexpectedConsoleEntries([]));
  assert.throws(
    () => assertNoUnexpectedConsoleEntries([
      { source: "console.error", sensitiveMessage: "never print this" },
      { source: "exception", sensitiveMessage: "or this" },
    ]),
    (error) => error instanceof Error
      && /2 unexpected/.test(error.message)
      && !error.message.includes("never print this")
      && !error.message.includes("or this"),
  );
});
