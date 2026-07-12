/** Compact token count for mono UI (e.g. 1.2k, 3.4M). */
export function formatTokenCount(value: number): string {
  if (!Number.isFinite(value) || value < 0) return "0";
  if (value < 1_000) return String(Math.trunc(value));
  if (value < 1_000_000) {
    const scaled = value / 1_000;
    return `${trimDecimal(scaled)}k`;
  }
  const scaled = value / 1_000_000;
  return `${trimDecimal(scaled)}M`;
}

/** Formats xAI cost ticks (1 USD = 1e10 ticks). Hidden when zero. */
export function formatCostTicks(ticks: number): string | null {
  if (!Number.isFinite(ticks) || ticks <= 0) return null;
  const usd = ticks / 10_000_000_000;
  if (usd < 0.01) return "<$0.01";
  return `~$${usd.toFixed(usd < 1 ? 2 : 2)}`;
}

/** One-line usage meta: `1.2k in · 340 out` (+ cost when present). */
export function formatUsageLine(input: {
  inputTokens: number;
  outputTokens: number;
  costInUsdTicks: number;
}): string {
  const base = `${formatTokenCount(input.inputTokens)} in · ${formatTokenCount(input.outputTokens)} out`;
  const cost = formatCostTicks(input.costInUsdTicks);
  return cost ? `${base} · ${cost}` : base;
}

function trimDecimal(value: number): string {
  const fixed = value >= 10 ? value.toFixed(0) : value.toFixed(1);
  return fixed.replace(/\.0$/, "");
}
