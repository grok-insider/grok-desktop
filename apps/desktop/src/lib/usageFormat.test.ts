import { describe, expect, it } from "vitest";
import { formatCostTicks, formatTokenCount, formatUsageLine } from "./usageFormat";

describe("usageFormat", () => {
  it("formats token counts compactly", () => {
    expect(formatTokenCount(0)).toBe("0");
    expect(formatTokenCount(999)).toBe("999");
    expect(formatTokenCount(1_200)).toBe("1.2k");
    expect(formatTokenCount(12_000)).toBe("12k");
    expect(formatTokenCount(3_400_000)).toBe("3.4M");
  });

  it("hides zero cost and shows small USD amounts", () => {
    expect(formatCostTicks(0)).toBeNull();
    expect(formatCostTicks(1)).toBe("<$0.01");
    // 1e8 ticks = $0.01
    expect(formatCostTicks(100_000_000)).toBe("~$0.01");
  });

  it("builds a usage line without inventing cost", () => {
    expect(formatUsageLine({ inputTokens: 1200, outputTokens: 340, costInUsdTicks: 0 })).toBe(
      "1.2k in · 340 out",
    );
  });
});
