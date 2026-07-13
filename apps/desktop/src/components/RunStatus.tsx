import type { RunState } from "../services/desktopClient";
import { Badge, type badgeVariants } from "@/components/ui/badge";
import type { VariantProps } from "class-variance-authority";

const stateLabels: Record<RunState, string> = {
  queued: "Queued",
  planning: "Planning",
  awaiting_approval: "Needs approval",
  running: "Running",
  paused: "Paused",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted_needs_review: "Needs review",
};

/* State→token mapping documented in DESIGN.md §5 (status chips). */
const stateVariants: Record<RunState, VariantProps<typeof badgeVariants>["variant"]> = {
  queued: "neutral",
  planning: "info",
  awaiting_approval: "warning",
  running: "info",
  paused: "neutral",
  completed: "success",
  failed: "destructive",
  cancelled: "neutral",
  interrupted_needs_review: "warning",
};

export function RunStatus({ state }: { state: RunState }) {
  return (
    <Badge variant={stateVariants[state]}>
      <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
      {stateLabels[state]}
    </Badge>
  );
}
