import assert from "node:assert/strict";
import test from "node:test";
import {
  GRAPHICS_FALLBACK_MARKER,
  softwareFallbackArguments,
} from "./graphics-launch-utils.mjs";

test("software fallback replaces graphics policy exactly once", () => {
  assert.deepEqual(
    softwareFallbackArguments([
      "--inspect=9229",
      "--grok-graphics-backend=wayland",
      GRAPHICS_FALLBACK_MARKER,
    ]),
    ["--inspect=9229", "--grok-graphics-backend=software", GRAPHICS_FALLBACK_MARKER],
  );
});
