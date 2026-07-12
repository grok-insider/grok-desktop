const TEXT_CONTRAST_MINIMUM = 4.5;
const INDICATOR_CONTRAST_MINIMUM = 3;

function semanticPair(label, foregroundToken, backgroundToken, minimumRatio = TEXT_CONTRAST_MINIMUM) {
  return Object.freeze({ label, foregroundToken, backgroundToken, minimumRatio });
}

// This is deliberately an allowlist of documented DESIGN.md pairings. A broad
// scan of arbitrary rendered elements cannot reliably resolve image, gradient,
// translucent, inherited, or overlapping backgrounds and creates noisy false
// positives. These pairs are opaque semantic contracts with known surfaces.
export const CDP_SEMANTIC_CONTRAST_PAIRS = Object.freeze([
  semanticPair("body text on canvas", "--foreground", "--background"),
  semanticPair("card text on card", "--card-foreground", "--card"),
  semanticPair("popover text on popover", "--popover-foreground", "--popover"),
  semanticPair("primary text on primary", "--primary-foreground", "--primary"),
  semanticPair("secondary text on secondary", "--secondary-foreground", "--secondary"),
  semanticPair("secondary text on canvas", "--muted-foreground", "--background"),
  semanticPair("secondary text on muted", "--muted-foreground", "--muted"),
  semanticPair("secondary text on card", "--muted-foreground", "--card"),
  semanticPair("tertiary text on canvas", "--subtle-foreground", "--background"),
  semanticPair("tertiary text on card", "--subtle-foreground", "--card"),
  semanticPair("accent text on accent", "--accent-foreground", "--accent"),
  semanticPair("destructive text on destructive", "--destructive-foreground", "--destructive"),
  semanticPair("destructive status on soft surface", "--destructive", "--destructive-soft"),
  semanticPair("info status on soft surface", "--info", "--info-soft"),
  semanticPair("success status on soft surface", "--success", "--success-soft"),
  semanticPair("warning status on soft surface", "--warning", "--warning-soft"),
  semanticPair("sidebar text on sidebar", "--sidebar-foreground", "--sidebar"),
  semanticPair("sidebar secondary text on sidebar", "--muted-foreground", "--sidebar"),
  semanticPair("sidebar primary text on primary", "--sidebar-primary-foreground", "--sidebar-primary"),
  semanticPair("sidebar accent text on accent", "--sidebar-accent-foreground", "--sidebar-accent"),
  semanticPair("focus ring on canvas", "--ring", "--background", INDICATOR_CONTRAST_MINIMUM),
  semanticPair("focus ring on card", "--ring", "--card", INDICATOR_CONTRAST_MINIMUM),
  semanticPair("focus ring on sidebar", "--sidebar-ring", "--sidebar", INDICATOR_CONTRAST_MINIMUM),
]);

export function parseCssSrgbColor(value) {
  if (typeof value !== "string") return undefined;
  const token = value.trim();
  const hex = /^#([\da-f]{3,4}|[\da-f]{6}|[\da-f]{8})$/iu.exec(token);
  if (hex) return parseHexColor(hex[1]);

  const functional = /^(rgba?)\((.*)\)$/iu.exec(token);
  if (!functional) return undefined;
  return parseFunctionalColor(functional[1].toLowerCase(), functional[2]);
}

export function wcagContrastRatio(foregroundValue, backgroundValue) {
  const foreground = parseCssSrgbColor(foregroundValue);
  const background = parseCssSrgbColor(backgroundValue);
  if (!foreground || !background || background.alpha !== 1) return Number.NaN;

  const composited = foreground.alpha === 1
    ? foreground
    : {
        red: compositeChannel(foreground.red, background.red, foreground.alpha),
        green: compositeChannel(foreground.green, background.green, foreground.alpha),
        blue: compositeChannel(foreground.blue, background.blue, foreground.alpha),
        alpha: 1,
      };
  const foregroundLuminance = relativeLuminance(composited);
  const backgroundLuminance = relativeLuminance(background);
  return (Math.max(foregroundLuminance, backgroundLuminance) + 0.05)
    / (Math.min(foregroundLuminance, backgroundLuminance) + 0.05);
}

export function assertSemanticContrastProbe(probes) {
  if (!Array.isArray(probes) || probes.length !== CDP_SEMANTIC_CONTRAST_PAIRS.length) {
    throw new Error("semantic contrast probe returned an incomplete pair set");
  }

  const expectedByLabel = new Map(CDP_SEMANTIC_CONTRAST_PAIRS.map((pair) => [pair.label, pair]));
  const seenLabels = new Set();
  for (const probe of probes) {
    const expected = expectedByLabel.get(probe?.label);
    if (!expected || seenLabels.has(probe.label)) {
      throw new Error("semantic contrast probe returned an unexpected or duplicate pair");
    }
    seenLabels.add(probe.label);
    if (probe.foregroundToken !== expected.foregroundToken
      || probe.backgroundToken !== expected.backgroundToken
      || probe.minimumRatio !== expected.minimumRatio) {
      throw new Error(`semantic contrast pair ${expected.label} did not match its configured contract`);
    }
    if (!probe.foregroundDefined || !probe.backgroundDefined) {
      throw new Error(`semantic contrast pair ${expected.label} references an undefined token`);
    }

    const foreground = parseCssSrgbColor(probe.foregroundColor);
    const background = parseCssSrgbColor(probe.backgroundColor);
    if (!foreground || !background || foreground.alpha !== 1 || background.alpha !== 1) {
      throw new Error(`semantic contrast pair ${expected.label} did not resolve to opaque sRGB colors`);
    }
    const ratio = wcagContrastRatio(probe.foregroundColor, probe.backgroundColor);
    if (!Number.isFinite(ratio) || ratio + Number.EPSILON < expected.minimumRatio) {
      throw new Error(`semantic contrast pair ${expected.label} is below ${expected.minimumRatio}:1`);
    }
  }
  return probes.length;
}

export function assertFocusIndicatorProbe(probe) {
  if (probe?.outlineStyle !== "solid" || parseCssPixels(probe.outlineWidth) < 3) {
    throw new Error("focus indicator must expose a solid outline at least 3px wide");
  }
  if (parseCssPixels(probe.outlineOffset) < 2) {
    throw new Error("focus indicator must preserve at least 2px of outline separation");
  }

  const outline = parseCssSrgbColor(probe.outlineColor);
  const ring = parseCssSrgbColor(probe.ringColor);
  if (!outline || !ring || outline.alpha !== 1 || ring.alpha !== 1 || !sameSrgbColor(outline, ring)) {
    throw new Error("focus indicator must use the opaque semantic ring color");
  }
  for (const [surface, color] of [["canvas", probe.canvasColor], ["card", probe.cardColor]]) {
    const ratio = wcagContrastRatio(probe.outlineColor, color);
    if (!Number.isFinite(ratio) || ratio + Number.EPSILON < INDICATOR_CONTRAST_MINIMUM) {
      throw new Error(`focus indicator is below ${INDICATOR_CONTRAST_MINIMUM}:1 on ${surface}`);
    }
  }
}

function parseHexColor(hex) {
  const expanded = hex.length <= 4
    ? [...hex].map((component) => `${component}${component}`).join("")
    : hex;
  return {
    red: Number.parseInt(expanded.slice(0, 2), 16),
    green: Number.parseInt(expanded.slice(2, 4), 16),
    blue: Number.parseInt(expanded.slice(4, 6), 16),
    alpha: expanded.length === 8 ? Number.parseInt(expanded.slice(6, 8), 16) / 255 : 1,
  };
}

function parseFunctionalColor(functionName, body) {
  const commaSyntax = body.includes(",");
  let channelTokens;
  let alphaToken;
  if (commaSyntax) {
    const parts = body.split(",").map((part) => part.trim());
    if ((functionName === "rgb" && parts.length !== 3) || (functionName === "rgba" && parts.length !== 4)) {
      return undefined;
    }
    channelTokens = parts.slice(0, 3);
    alphaToken = parts[3];
  } else {
    const slashParts = body.split("/").map((part) => part.trim());
    if (slashParts.length > 2) return undefined;
    channelTokens = slashParts[0].split(/\s+/u).filter(Boolean);
    if (channelTokens.length !== 3) return undefined;
    alphaToken = slashParts[1];
  }

  const channels = channelTokens.map(parseRgbChannel);
  if (channels.some((channel) => channel === undefined)) return undefined;
  const alpha = alphaToken === undefined ? 1 : parseAlphaChannel(alphaToken);
  if (alpha === undefined) return undefined;
  return { red: channels[0], green: channels[1], blue: channels[2], alpha };
}

function parseRgbChannel(token) {
  if (/^(?:\d+(?:\.\d*)?|\.\d+)%$/u.test(token)) {
    const percentage = Number.parseFloat(token);
    return percentage <= 100 ? (percentage / 100) * 255 : undefined;
  }
  if (!/^(?:\d+(?:\.\d*)?|\.\d+)$/u.test(token)) return undefined;
  const value = Number.parseFloat(token);
  return value <= 255 ? value : undefined;
}

function parseAlphaChannel(token) {
  if (/^(?:\d+(?:\.\d*)?|\.\d+)%$/u.test(token)) {
    const percentage = Number.parseFloat(token);
    return percentage <= 100 ? percentage / 100 : undefined;
  }
  if (!/^(?:\d+(?:\.\d*)?|\.\d+)$/u.test(token)) return undefined;
  const value = Number.parseFloat(token);
  return value <= 1 ? value : undefined;
}

function compositeChannel(foreground, background, alpha) {
  return foreground * alpha + background * (1 - alpha);
}

function relativeLuminance(color) {
  const linear = [color.red, color.green, color.blue].map((channel) => {
    const normalized = channel / 255;
    return normalized <= 0.04045
      ? normalized / 12.92
      : ((normalized + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function parseCssPixels(value) {
  if (typeof value !== "string" || !/^(?:\d+(?:\.\d*)?|\.\d+)px$/u.test(value.trim())) return Number.NaN;
  return Number.parseFloat(value);
}

function sameSrgbColor(left, right) {
  const tolerance = 0.01;
  return Math.abs(left.red - right.red) <= tolerance
    && Math.abs(left.green - right.green) <= tolerance
    && Math.abs(left.blue - right.blue) <= tolerance;
}
