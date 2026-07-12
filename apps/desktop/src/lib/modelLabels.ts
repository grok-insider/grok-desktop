/**
 * Product-facing labels for official xAI chat model ids.
 * Canonical ids remain authoritative; labels never invent models not in the catalog.
 */

const KNOWN_LABELS: Readonly<Record<string, string>> = {
  "grok-4.3": "Grok 4.3",
  "grok-4.5": "Grok 4.5",
  "grok-4.20-0309-non-reasoning": "Grok 4.20",
  "grok-4.20-0309-reasoning": "Grok 4.20 Reasoning",
  "grok-4.20-multi-agent-0309": "Grok 4.20 Multi-agent",
  "grok-build-0.1": "Grok Build",
};

/**
 * Returns a short product label for a catalog model id.
 * Unknown ids are humanized from the id string without adding fake catalog entries.
 */
export function modelDisplayLabel(modelId: string): string {
  const id = modelId.trim();
  if (!id) return "Unknown model";
  const known = KNOWN_LABELS[id];
  if (known) return known;
  return humanizeModelId(id);
}

/** Trigger text for the composer model control. */
export function modelMenuTriggerLabel(options: {
  overrideModelId?: string;
  defaultModelId?: string;
}): string {
  if (options.overrideModelId) {
    return modelDisplayLabel(options.overrideModelId);
  }
  if (options.defaultModelId) {
    return `Default · ${modelDisplayLabel(options.defaultModelId)}`;
  }
  return "Choose model";
}

function humanizeModelId(id: string): string {
  // grok-4.3 → Grok 4.3; grok-code-fast-1 → Grok Code Fast 1
  const parts = id.split(/[-_]+/).filter(Boolean);
  if (parts.length === 0) return id;
  return parts
    .map((part, index) => {
      if (/^\d+(\.\d+)*$/.test(part)) return part;
      if (index === 0 && part.toLowerCase() === "grok") return "Grok";
      return part.charAt(0).toUpperCase() + part.slice(1).toLowerCase();
    })
    .join(" ");
}
