import { useMemo, useState, type KeyboardEvent } from "react";
import {
  Check,
  CheckCircle2,
  ChevronRight,
  Circle,
  CirclePause,
  Clock3,
  FileDiff,
  MoreHorizontal,
  Play,
  ShieldAlert,
  X,
} from "lucide-react";
import { useSearchParams } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { IconButton, PageHeader, RunStatus } from "../components/ui";
import { useDesktopSnapshot } from "../services/DesktopClientContext";
import type { RunState, RunSummary } from "../services/desktopClient";
import { GROK_EXECUTION_UNAVAILABLE_REASON } from "../services/productAvailability";

const activityFilters = [
  { id: "all", label: "All" },
  { id: "needs_input", label: "Needs input" },
  { id: "running", label: "Running" },
  { id: "completed", label: "Completed" },
] as const;

type ActivityFilter = (typeof activityFilters)[number]["id"];

const runGridClass = cn(
  "grid grid-cols-[minmax(13.75rem,1fr)_6.75rem_7.5rem_4.5rem_1rem] items-center gap-2.5",
  "max-[1120px]:grid-cols-[minmax(11.875rem,1fr)_6.5rem_5.75rem_3.5rem_1rem]",
  "max-[680px]:grid-cols-[minmax(0,1fr)_auto_1rem]",
);

const needsInput = (state: RunState) => state === "awaiting_approval" || state === "interrupted_needs_review";

const matchesFilter = (run: RunSummary, filter: ActivityFilter) => {
  if (filter === "needs_input") return needsInput(run.state);
  if (filter === "running") return ["running", "planning", "queued", "paused"].includes(run.state);
  if (filter === "completed") return run.state === "completed";
  return true;
};

const progressValue = (progress: number) => Math.min(100, Math.max(0, progress));

export function ActivityView() {
  const { snapshot, loading } = useDesktopSnapshot();
  const [searchParams] = useSearchParams();
  const [filter, setFilter] = useState<ActivityFilter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const filtered = useMemo(
    () => snapshot?.runs.filter((run) => matchesFilter(run, filter)) ?? [],
    [filter, snapshot],
  );
  const requestedId = searchParams.get("run");
  const selected = filtered.find((run) => run.id === (selectedId ?? requestedId)) ?? filtered[0];
  const needsInputCount = snapshot?.runs.filter((run) => needsInput(run.state)).length ?? 0;

  const selectFilterFromKeyboard = (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      nextIndex = (currentIndex + 1) % activityFilters.length;
    }
    if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      nextIndex = (currentIndex - 1 + activityFilters.length) % activityFilters.length;
    }
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = activityFilters.length - 1;
    if (nextIndex === null) return;

    event.preventDefault();
    setFilter(activityFilters[nextIndex].id);
    const tabs = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]");
    tabs?.[nextIndex]?.focus();
  };

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden px-[clamp(24px,3.2vw,48px)] pt-8 pb-6 max-[900px]:h-auto max-[900px]:overflow-visible max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <PageHeader title="Activity" description="Monitor work, review decisions, and inspect running tasks." />

      <div
        className="mx-auto mb-3 flex w-full max-w-[1440px] items-center gap-1 overflow-x-auto rounded-md bg-secondary p-1"
        role="tablist"
        aria-label="Activity filters"
      >
        {activityFilters.map((item, index) => {
          const active = filter === item.id;
          return (
            <Button
              id={`activity-filter-${item.id}`}
              key={item.id}
              type="button"
              role="tab"
              aria-controls="activity-filter-panel"
              aria-selected={active}
              aria-label={item.id === "needs_input" ? `${item.label}, ${needsInputCount}` : item.label}
              tabIndex={active ? 0 : -1}
              variant={active ? "secondary" : "ghost"}
              size="sm"
              className={cn(
                "min-w-fit",
                active && "bg-card font-semibold text-foreground shadow-raised hover:bg-card",
              )}
              onClick={() => setFilter(item.id)}
              onKeyDown={(event) => selectFilterFromKeyboard(event, index)}
            >
              {item.label}
              {item.id === "needs_input" && (
                <span
                  className="grid h-[19px] min-w-[19px] place-items-center rounded-full bg-warning-soft px-1 font-mono text-label font-semibold text-warning tabular-nums"
                  aria-hidden="true"
                >
                  {needsInputCount}
                </span>
              )}
            </Button>
          );
        })}
      </div>

      <div
        id="activity-filter-panel"
        role="tabpanel"
        aria-labelledby={`activity-filter-${filter}`}
        className={cn(
          "mx-auto grid min-h-0 w-full max-w-[1440px] flex-1 overflow-hidden rounded-lg border border-border bg-card",
          selected ? "grid-cols-[minmax(31.25rem,1.35fr)_minmax(19.375rem,.65fr)]" : "grid-cols-1",
          "max-[900px]:grid-cols-1 max-[900px]:overflow-visible max-[900px]:border-0 max-[900px]:bg-transparent",
        )}
      >
        <section
          id="activity-run-list"
          className={cn(
            "min-w-0 overflow-y-auto bg-card max-[900px]:overflow-visible max-[900px]:rounded-lg max-[900px]:border max-[900px]:border-border",
            selected && "border-r border-border max-[900px]:border-r-0",
          )}
          aria-label="Runs"
          aria-busy={loading}
        >
          <div
            className={cn(
              runGridClass,
              "sticky top-0 z-2 h-[38px] border-b border-border bg-muted px-3 text-label font-semibold text-subtle-foreground",
            )}
            aria-hidden="true"
          >
            <span>Task</span>
            <span>Status</span>
            <span className="max-[680px]:hidden">Progress</span>
            <span className="max-[680px]:hidden">Updated</span>
            <span />
          </div>

          {loading ? (
            <ActivityRowsSkeleton />
          ) : (
            filtered.map((run) => {
              const runProgress = progressValue(run.progress);
              const isSelected = selected?.id === run.id;
              return (
                <button
                  key={run.id}
                  type="button"
                  className={cn(
                    runGridClass,
                    "min-h-16 w-full border-b border-border px-3 py-2 text-left outline-none transition-[background-color,box-shadow,transform] duration-150 ease-fluid",
                    "hover:bg-muted active:scale-[.995] focus-visible:ring-[3px] focus-visible:ring-inset focus-visible:ring-ring",
                    isSelected && "bg-accent shadow-[inset_2px_0_var(--info)] hover:bg-accent",
                  )}
                  aria-pressed={isSelected}
                  aria-controls="activity-run-inspector"
                  onClick={() => setSelectedId(run.id)}
                >
                  <span className="flex min-w-0 flex-col gap-1">
                    <strong className="truncate text-body font-semibold text-foreground">{run.title}</strong>
                    <small className="truncate text-label text-subtle-foreground">{run.projectName}</small>
                  </span>
                  <RunStatus state={run.state} />
                  <span className="flex items-center gap-2 max-[680px]:hidden">
                    <span
                      className="h-1 w-16 overflow-hidden rounded-full bg-secondary max-[1120px]:w-10"
                      role="progressbar"
                      aria-label={`${run.title} progress`}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={runProgress}
                    >
                      <span
                        className="block h-full origin-left rounded-full bg-info"
                        style={{ transform: `scaleX(${runProgress / 100})` }}
                      />
                    </span>
                    <small className="font-mono text-label text-subtle-foreground tabular-nums">{runProgress}%</small>
                  </span>
                  <time className="font-mono text-label text-subtle-foreground tabular-nums max-[680px]:hidden">
                    {run.updatedAt}
                  </time>
                  <ChevronRight className="text-subtle-foreground" size={16} aria-hidden="true" />
                </button>
              );
            })
          )}

          {!loading && filtered.length === 0 && (
            <div className="flex min-h-48 flex-col items-center justify-center px-6 text-center" role="status">
              <CheckCircle2 className="text-subtle-foreground" size={26} aria-hidden="true" />
              <h2 className="m-0 mt-3 text-title-sm font-semibold text-foreground">Nothing here</h2>
              <p className="m-0 mt-1 text-body text-muted-foreground">No work matches this filter.</p>
            </div>
          )}
        </section>

        {selected && <RunInspector run={selected} />}
      </div>
    </div>
  );
}

function ActivityRowsSkeleton() {
  return (
    <div className="flex flex-col gap-1 p-1" role="status" aria-label="Loading activity">
      <span className="sr-only">Loading activity</span>
      {Array.from({ length: 5 }).map((_, index) => (
        <div className={cn(runGridClass, "min-h-16 px-2")} key={index}>
          <span className="flex flex-col gap-2">
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-2.5 w-1/3" />
          </span>
          <Skeleton className="h-5 w-20 rounded-full" />
          <Skeleton className="h-1 w-24 max-[680px]:hidden" />
          <Skeleton className="h-3 w-12 max-[680px]:hidden" />
          <Skeleton className="size-4" />
        </div>
      ))}
    </div>
  );
}

function RunInspector({ run }: { run: RunSummary }) {
  const runProgress = progressValue(run.progress);
  const control = runControl(run.state);

  return (
    <aside
      id="activity-run-inspector"
      className="min-w-0 overflow-y-auto p-5 max-[900px]:mt-3 max-[900px]:overflow-visible max-[900px]:rounded-lg max-[900px]:border max-[900px]:border-border max-[900px]:bg-card max-[680px]:p-4"
      aria-labelledby="activity-run-inspector-title"
    >
      <header className="flex justify-between gap-3 border-b border-border pb-4">
        <div className="min-w-0">
          <RunStatus state={run.state} />
          <h2 id="activity-run-inspector-title" className="m-0 mt-2.5 text-title-sm font-semibold text-foreground">
            {run.title}
          </h2>
          <p className="m-0 mt-1 text-label text-subtle-foreground">{run.projectName}</p>
        </div>
        <IconButton label="Additional run actions require daemon protocol support" disabled>
          <MoreHorizontal size={18} aria-hidden="true" />
        </IconButton>
      </header>

      <section className="border-b border-border py-4" aria-labelledby="run-progress-heading">
        <div className="mb-2 flex items-center justify-between gap-4">
          <h3 id="run-progress-heading" className="m-0 text-body-sm font-medium text-muted-foreground">Progress</h3>
          <strong className="font-mono text-body-sm font-semibold text-foreground tabular-nums">{runProgress}%</strong>
        </div>
        <div
          className="h-1 w-full overflow-hidden rounded-full bg-secondary"
          role="progressbar"
          aria-label="Run progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={runProgress}
        >
          <span
            className="block h-full origin-left rounded-full bg-info"
            style={{ transform: `scaleX(${runProgress / 100})` }}
          />
        </div>
        <p className="m-0 mt-2 text-body-sm leading-5 text-muted-foreground">{run.detail}</p>
      </section>

      {run.state === "interrupted_needs_review" && <InterruptedReviewNotice />}
      {run.approval && <ApprovalNotice approval={run.approval} />}

      <section className="border-b border-border py-4" aria-labelledby="run-plan-heading">
        <h3 id="run-plan-heading" className="m-0 mb-3 text-body font-semibold text-foreground">Plan</h3>
        <ol className="m-0 list-none p-0">
          {run.steps.map((step, index) => {
            const stateLabel = step.state === "done" ? "Completed" : step.state === "active" ? "In progress" : "Waiting";
            return (
              <li className="flex min-h-10 items-center gap-2.5" key={`${step.label}-${index}`}>
                {step.state === "done" ? (
                  <CheckCircle2 className="shrink-0 text-success" size={18} aria-hidden="true" />
                ) : step.state === "active" ? (
                  <Clock3 className="shrink-0 text-info" size={18} aria-hidden="true" />
                ) : (
                  <Circle className="shrink-0 text-subtle-foreground" size={18} aria-hidden="true" />
                )}
                <span className="flex min-w-0 flex-col">
                  <strong className="text-body-sm font-semibold text-foreground">{step.label}</strong>
                  <small className="text-label text-subtle-foreground">
                    Step {index + 1} of {run.steps.length} · {stateLabel}
                  </small>
                </span>
              </li>
            );
          })}
        </ol>
      </section>

      <section className="border-b border-border py-4" aria-labelledby="run-deliverables-heading">
        <h3 id="run-deliverables-heading" className="m-0 mb-3 text-body font-semibold text-foreground">Deliverables</h3>
        <Button
          type="button"
          variant="outline"
          className="h-auto min-h-12 w-full justify-start gap-2.5 px-3 py-2 text-left whitespace-normal"
          disabled
          title="Diff retrieval requires daemon workspace support"
        >
          <FileDiff className="shrink-0 text-subtle-foreground" size={18} aria-hidden="true" />
          <span className="flex min-w-0 flex-1 flex-col items-start">
            <strong className="text-body-sm font-semibold text-foreground">Output unavailable</strong>
            <small className="text-label font-normal text-subtle-foreground">No daemon deliverable data is connected.</small>
          </span>
          <ChevronRight className="shrink-0 text-subtle-foreground" size={16} aria-hidden="true" />
        </Button>
      </section>

      <footer className="flex flex-wrap items-end justify-between gap-3 pt-4">
        {control ? (
          <div className="min-w-0 flex-1">
            <Button type="button" variant="outline" disabled title={GROK_EXECUTION_UNAVAILABLE_REASON}>
              {control === "Resume" ? <Play size={15} aria-hidden="true" /> : <CirclePause size={15} aria-hidden="true" />}
              {control} unavailable
            </Button>
            <p className="m-0 mt-2 max-w-[54ch] text-label text-muted-foreground">
              {GROK_EXECUTION_UNAVAILABLE_REASON}
            </p>
          </div>
        ) : <span />}
        <Button
          type="button"
          variant="ghost"
          disabled
          title="Run-to-thread navigation requires daemon workspace support"
        >
          Open conversation
        </Button>
      </footer>
    </aside>
  );
}

function InterruptedReviewNotice() {
  return (
    <section className="my-4 rounded-lg border border-warning/30 bg-warning-soft p-3" aria-labelledby="run-review-heading">
      <div className="flex items-start gap-2.5 text-warning">
        <ShieldAlert className="mt-0.5 shrink-0" size={18} aria-hidden="true" />
        <div>
          <h3 id="run-review-heading" className="m-0 text-body font-semibold">Manual review required</h3>
          <p className="m-0 mt-1 text-body-sm leading-5 text-muted-foreground">
            This run was interrupted during a potentially non-idempotent side effect. Review the daemon-owned record before taking further action; it will not replay automatically.
          </p>
        </div>
      </div>
    </section>
  );
}

function ApprovalNotice({ approval }: { approval: NonNullable<RunSummary["approval"]> }) {
  const riskLabel = `${approval.risk.charAt(0).toUpperCase()}${approval.risk.slice(1)} risk`;

  return (
    <section className="my-4 rounded-lg border border-warning/30 bg-warning-soft p-3" aria-labelledby="run-approval-heading">
      <div className="flex items-start gap-2.5 text-warning">
        <ShieldAlert className="mt-0.5 shrink-0" size={18} aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 id="run-approval-heading" className="m-0 text-body font-semibold">{approval.title}</h3>
            <Badge variant={approval.risk === "high" ? "destructive" : "warning"}>{riskLabel}</Badge>
          </div>
          <p className="m-0 mt-1 text-label text-warning">Grok needs your approval</p>
        </div>
      </div>
      <p className="my-3 rounded-md bg-card/70 p-2 font-mono text-label leading-4 text-muted-foreground [overflow-wrap:anywhere]">
        {approval.detail}
      </p>
      <p className="m-0 mb-3 text-label leading-4 text-muted-foreground">{GROK_EXECUTION_UNAVAILABLE_REASON}</p>
      <div className="flex flex-wrap justify-end gap-2">
        <Button type="button" variant="outline" disabled title={GROK_EXECUTION_UNAVAILABLE_REASON}>
          <X size={15} aria-hidden="true" /> Decline unavailable
        </Button>
        <Button type="button" disabled title={GROK_EXECUTION_UNAVAILABLE_REASON}>
          <Check size={15} aria-hidden="true" /> Approve once unavailable
        </Button>
      </div>
    </section>
  );
}

function runControl(state: RunState): "Pause" | "Resume" | null {
  if (state === "paused") return "Resume";
  if (state === "running" || state === "planning" || state === "queued") return "Pause";
  return null;
}

export const runStateLabels: Record<RunState, string> = {
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
