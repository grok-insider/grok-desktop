import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  CDP_SEMANTIC_CONTRAST_PAIRS,
  assertFocusIndicatorProbe,
  assertSemanticContrastProbe,
  parseCssSrgbColor,
  wcagContrastRatio,
} from "./electron-cdp-contrast-utils.mjs";

const RESOLVED_TOKEN_COLORS = Object.freeze({
  "--accent": "rgb(226, 233, 229)",
  "--accent-foreground": "rgb(29, 33, 31)",
  "--background": "rgb(244, 246, 245)",
  "--card": "rgb(255, 255, 255)",
  "--card-foreground": "rgb(29, 33, 31)",
  "--destructive": "rgb(165, 69, 69)",
  "--destructive-foreground": "rgb(255, 255, 255)",
  "--destructive-soft": "rgb(250, 234, 234)",
  "--foreground": "rgb(29, 33, 31)",
  "--info": "rgb(69, 107, 132)",
  "--info-soft": "rgb(230, 240, 245)",
  "--muted": "rgb(247, 248, 247)",
  "--muted-foreground": "rgb(94, 102, 98)",
  "--popover": "rgb(255, 255, 255)",
  "--popover-foreground": "rgb(29, 33, 31)",
  "--primary": "rgb(37, 45, 41)",
  "--primary-foreground": "rgb(244, 246, 245)",
  "--ring": "rgb(69, 107, 132)",
  "--secondary": "rgb(233, 237, 235)",
  "--secondary-foreground": "rgb(29, 33, 31)",
  "--sidebar": "rgb(233, 237, 235)",
  "--sidebar-accent": "rgb(255, 255, 255)",
  "--sidebar-accent-foreground": "rgb(29, 33, 31)",
  "--sidebar-foreground": "rgb(29, 33, 31)",
  "--sidebar-primary": "rgb(37, 45, 41)",
  "--sidebar-primary-foreground": "rgb(244, 246, 245)",
  "--sidebar-ring": "rgb(69, 107, 132)",
  "--subtle-foreground": "rgb(104, 112, 105)",
  "--success": "rgb(63, 114, 85)",
  "--success-soft": "rgb(230, 241, 234)",
  "--warning": "rgb(138, 87, 52)",
  "--warning-soft": "rgb(246, 236, 227)",
});

function validContrastProbes() {
  return CDP_SEMANTIC_CONTRAST_PAIRS.map((pair) => ({
    ...pair,
    foregroundDefined: true,
    backgroundDefined: true,
    foregroundColor: RESOLVED_TOKEN_COLORS[pair.foregroundToken],
    backgroundColor: RESOLVED_TOKEN_COLORS[pair.backgroundToken],
  }));
}

test("parses bounded CSS sRGB formats and rejects unsupported or out-of-range colors", () => {
  assert.deepEqual(parseCssSrgbColor("#fff"), { red: 255, green: 255, blue: 255, alpha: 1 });
  assert.deepEqual(parseCssSrgbColor("#456b8457"), { red: 69, green: 107, blue: 132, alpha: 87 / 255 });
  assert.deepEqual(parseCssSrgbColor("rgb(69, 107, 132)"), { red: 69, green: 107, blue: 132, alpha: 1 });
  assert.deepEqual(parseCssSrgbColor("rgb(69 107 132 / 34%)"), { red: 69, green: 107, blue: 132, alpha: 0.34 });
  assert.deepEqual(parseCssSrgbColor("rgba(100%, 0%, 0%, 0.5)"), { red: 255, green: 0, blue: 0, alpha: 0.5 });
  for (const invalid of ["", "transparent", "oklch(50% 0.1 160)", "rgb(256, 0, 0)", "rgb(0 0 / 1)", "#12"]) {
    assert.equal(parseCssSrgbColor(invalid), undefined);
  }
});

test("computes WCAG contrast including alpha compositing against an opaque surface", () => {
  assert.equal(wcagContrastRatio("#000", "#fff"), 21);
  assert.ok(Math.abs(wcagContrastRatio("#777", "#fff") - 4.478089) < 0.000001);
  assert.ok(wcagContrastRatio("rgb(69 107 132 / 34%)", "#f4f6f5") < 3);
  assert.ok(wcagContrastRatio("#456b84", "#f4f6f5") > 3);
  assert.ok(Number.isNaN(wcagContrastRatio("#456b84", "rgb(255 255 255 / 50%)")));
});

test("accepts every explicit semantic pair and rejects missing, translucent, and low-contrast results", () => {
  const valid = validContrastProbes();
  assert.equal(assertSemanticContrastProbe(valid), CDP_SEMANTIC_CONTRAST_PAIRS.length);
  assert.throws(() => assertSemanticContrastProbe(valid.slice(1)), /incomplete pair set/);
  assert.throws(
    () => assertSemanticContrastProbe(valid.map((probe, index) => index === 0
      ? { ...probe, foregroundColor: "rgba(29, 33, 31, 0.9)" }
      : probe)),
    /opaque sRGB/,
  );
  assert.throws(
    () => assertSemanticContrastProbe(valid.map((probe, index) => index === 0
      ? { ...probe, foregroundColor: "rgb(145, 145, 145)" }
      : probe)),
    /below 4.5:1/,
  );
});

test("requires the actual focus outline to be an opaque semantic ring with sufficient contrast", () => {
  const valid = {
    outlineColor: "rgb(69, 107, 132)",
    outlineStyle: "solid",
    outlineWidth: "3px",
    outlineOffset: "2px",
    ringColor: "rgb(69, 107, 132)",
    canvasColor: "rgb(244, 246, 245)",
    cardColor: "rgb(255, 255, 255)",
  };
  assert.doesNotThrow(() => assertFocusIndicatorProbe(valid));
  assert.throws(
    () => assertFocusIndicatorProbe({ ...valid, outlineColor: "rgba(69, 107, 132, 0.34)" }),
    /opaque semantic ring/,
  );
  assert.throws(() => assertFocusIndicatorProbe({ ...valid, outlineWidth: "2px" }), /at least 3px/);
});

test("renderer focus classes and base outline never alpha-blend the semantic ring", () => {
  const sourceRoot = fileURLToPath(new URL("../src", import.meta.url));
  const violations = sourceFiles(sourceRoot).flatMap((path) => {
    const contents = readFileSync(path, "utf8");
    return /(?:focus|focus-visible|focus-within):ring-ring\/[0-9]+/gu.test(contents) ? [path] : [];
  });
  assert.deepEqual(violations, []);

  const styles = readFileSync(join(sourceRoot, "styles.css"), "utf8");
  assert.match(styles, /:focus-visible\s*\{[^}]*outline:\s*3px solid var\(--ring\);[^}]*outline-offset:\s*2px;/su);
});

function sourceFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...sourceFiles(path));
    else if ([".css", ".ts", ".tsx"].includes(extname(entry.name)) && !entry.name.includes(".test.")) files.push(path);
  }
  return files;
}
