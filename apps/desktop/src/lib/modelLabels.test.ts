import { describe, expect, it } from "vitest";
import { modelDisplayLabel, modelMenuTriggerLabel } from "./modelLabels";

describe("modelDisplayLabel", () => {
  it("maps known official catalog ids to product labels", () => {
    expect(modelDisplayLabel("grok-4.3")).toBe("Grok 4.3");
    expect(modelDisplayLabel("grok-4.5")).toBe("Grok 4.5");
    expect(modelDisplayLabel("grok-build-0.1")).toBe("Grok Build");
    expect(modelDisplayLabel("grok-4.20-0309-reasoning")).toBe("Grok 4.20 Reasoning");
  });

  it("humanizes unknown ids without inventing catalog entries", () => {
    expect(modelDisplayLabel("grok-experimental-foo")).toBe("Grok Experimental Foo");
    expect(modelDisplayLabel("  custom-model-2  ")).toBe("Custom Model 2");
  });

  it("does not treat Imagine media ids as chat product names beyond humanize", () => {
    // Labels never claim Imagine as a chat default product; humanize only.
    expect(modelDisplayLabel("grok-imagine-image")).toBe("Grok Imagine Image");
  });
});

describe("modelMenuTriggerLabel", () => {
  it("prefers override, then default product label", () => {
    expect(modelMenuTriggerLabel({
      overrideModelId: "grok-4.3",
      defaultModelId: "grok-build-0.1",
    })).toBe("Grok 4.3");
    expect(modelMenuTriggerLabel({ defaultModelId: "grok-build-0.1" })).toBe("Default · Grok Build");
    expect(modelMenuTriggerLabel({})).toBe("Choose model");
  });
});
