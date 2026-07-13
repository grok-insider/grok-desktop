import { useEffect, useRef, useState } from "react";
import {
  CalendarClock,
  CheckCircle2,
  CircleAlert,
  Clock3,
  History,
  LoaderCircle,
  MoreHorizontal,
  Play,
  Plus,
  RefreshCw,
  X,
} from "lucide-react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { PageHeader } from "../components/PageHeader";
import { formatAutomationSchedule, TIME_ZONE_OPTIONS, defaultAutomationSchedule } from "../services/automationSchedule";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { AutomationDraft, AutomationSchedule, AutomationSummary } from "../services/desktopClient";
import { AUTOMATION_DEFINITION_ONLY_REASON } from "../services/productAvailability";

const selectTriggerClassName = "h-[34px] w-full min-w-0 bg-card px-3 font-normal text-body text-foreground";

export function AutomationsView() {
  const client = useDesktopClient();
  const { snapshot, loading } = useDesktopSnapshot();
  const newButtonRef = useRef<HTMLButtonElement>(null);
  const detailReturnFocusRef = useRef<HTMLButtonElement | null>(null);
  const editorReturnFocusRef = useRef<HTMLButtonElement | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [editor, setEditor] = useState<AutomationDraft | null>(null);
  const [notice, setNotice] = useState("");
  const automation = snapshot?.automations.find((item) => item.id === selected) ?? null;
  const projects = snapshot?.projects.map((project) => ({ id: project.id, name: project.name })) ?? [];
  const canCreate = Boolean(snapshot?.projects.length);
  const schedulerState = snapshot?.connection.automationScheduler?.state;
  const schedulerStatus = automationSchedulerStatus(schedulerState);
  const schedulerArmed = schedulerState === "kernel_initialized_execution_enabled"
    || snapshot?.capabilities.find((capability) => capability.id === "automations")?.available === true;

  const openNewDefinition = () => {
    if (!snapshot?.projects[0]) return;
    editorReturnFocusRef.current = newButtonRef.current;
    setEditor(emptyDraft(snapshot.projects[0].id, schedulerArmed));
  };

  const saveDefinition = async (draft: AutomationDraft) => {
    const result = await client.saveAutomation({
      ...draft,
      name: draft.name.trim(),
      prompt: draft.prompt.trim(),
      enabled: draft.enabled ?? schedulerArmed,
    });
    if (result.status !== "success") throw new Error(result.reason);
    setEditor(null);
    const active = result.value.enabled;
    setNotice(
      draft.id
        ? (active ? "Definition updated and scheduled." : "Definition updated.")
        : (active ? "Definition saved and scheduled." : "Definition saved inactive."),
    );
  };

  return (
    <div className="h-full min-h-0 overflow-y-auto px-[clamp(24px,3.2vw,48px)] pt-8 pb-10 max-[680px]:px-4 max-[680px]:pt-6">
      <PageHeader
        title="Automation definitions"
        description={
          schedulerArmed
            ? "Save recurring task definitions. The daemon scheduler claims due occurrences and links durable runs."
            : "Save recurring task definitions. Scheduling and execution stay inactive until the daemon scheduler is armed."
        }
        actions={
          <Button
            ref={newButtonRef}
            disabled={!canCreate}
            title={loading ? "Wait for projects to load." : canCreate ? undefined : "Create a project before adding a definition."}
            onClick={openNewDefinition}
          >
            <Plus size={16} aria-hidden="true" /> New definition
          </Button>
        }
      />

      <div className="mx-auto flex w-full max-w-[1440px] flex-col gap-4">
        <div className={cn(
          "flex min-h-16 items-start gap-3 rounded-lg border p-3",
          schedulerArmed
            ? "border-success/25 bg-success-soft text-success"
            : "border-warning/25 bg-warning-soft text-warning",
        )}>
          <Clock3 className="mt-0.5 shrink-0" size={18} aria-hidden="true" />
          <div className="min-w-0">
            <strong className="text-body font-semibold text-foreground">
              {schedulerArmed ? "Scheduler armed" : "Definitions are inactive"}
            </strong>
            <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">
              {schedulerArmed
                ? "Enabled definitions are claimed by the daemon. Interrupted non-idempotent runs require review and are never auto-replayed."
                : AUTOMATION_DEFINITION_ONLY_REASON}
            </p>
          </div>
        </div>

        <div className="grid grid-cols-3 gap-3 max-[680px]:grid-cols-1" aria-label="Automation summary">
          <SummaryMetric icon={CalendarClock} label="Saved definitions" value={loading ? null : String(snapshot?.automations.length ?? 0)} />
          <SummaryMetric icon={Clock3} label="Scheduler status" value={loading ? null : schedulerStatus} />
          <SummaryMetric icon={CheckCircle2} label="Recorded runs" value={loading ? null : "0"} />
        </div>

        <section
          className="overflow-hidden rounded-lg border border-border bg-card"
          aria-labelledby="saved-definitions-title"
          aria-busy={loading}
        >
          <header className="flex min-h-12 items-center justify-between gap-3 border-b border-border bg-muted px-4 py-2">
            <div>
              <h2 id="saved-definitions-title" className="m-0 text-title-sm font-semibold text-foreground">Saved definitions</h2>
              <p className="m-0 text-body-sm text-muted-foreground">
                {schedulerArmed
                  ? "Enabled definitions are scheduled by the daemon journal."
                  : "Timing and policy only; no runs are scheduled."}
              </p>
            </div>
            <Badge variant={schedulerArmed ? "success" : "neutral"}>
              {schedulerArmed ? "Scheduler live" : "Inactive"}
            </Badge>
          </header>

          {loading ? (
            <DefinitionsLoading />
          ) : snapshot?.automations.length ? (
            <ul className="m-0 list-none divide-y divide-border p-0">
              {snapshot.automations.map((item) => (
                <DefinitionRow
                  automation={item}
                  key={item.id}
                  onOpen={(trigger) => {
                    detailReturnFocusRef.current = trigger;
                    setSelected(item.id);
                  }}
                />
              ))}
            </ul>
          ) : (
            <div className="flex min-h-32 items-center justify-center gap-3 p-6 text-center">
              <span className="grid size-10 shrink-0 place-items-center rounded-lg bg-info-soft text-info" aria-hidden="true">
                <CalendarClock size={19} />
              </span>
              <span className="flex max-w-md flex-col items-start gap-0.5 text-left">
                <strong className="text-body font-semibold text-foreground">No definitions yet</strong>
                <span className="text-body-sm text-muted-foreground">
                  {snapshot?.projects.length
                    ? "Save a recurring task definition for future scheduler support."
                    : "Create a project before adding a definition."}
                </span>
              </span>
            </div>
          )}
        </section>

        <section className="rounded-lg border border-border bg-card" aria-labelledby="automation-history-title">
          <header className="flex items-center justify-between gap-4 border-b border-border px-4 py-3 max-[480px]:items-start">
            <div>
              <h2 id="automation-history-title" className="m-0 text-title-sm font-semibold text-foreground">Run history</h2>
              <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">History will appear after a qualified scheduler is connected.</p>
            </div>
            <Button variant="ghost" disabled title={AUTOMATION_DEFINITION_ONLY_REASON}>
              <History size={15} aria-hidden="true" /> Export unavailable
            </Button>
          </header>
          <div className="flex min-h-24 items-center justify-center gap-2 p-5 text-body-sm text-muted-foreground">
            <History size={16} aria-hidden="true" /> No automation runs have been recorded.
          </div>
        </section>
      </div>

      {notice && (
        <div
          className="fixed right-4 bottom-4 z-100 flex min-h-[44px] max-w-[min(420px,calc(100vw-32px))] items-center gap-3 rounded-lg border border-input bg-card py-2 pr-2 pl-3 text-body shadow-dialog"
          role="status"
          aria-live="polite"
        >
          <span>{notice}</span>
          <Button
            className="ml-auto"
            type="button"
            variant="ghost"
            size="icon"
            aria-label="Dismiss"
            title="Dismiss"
            onClick={() => setNotice("")}
          >
            <X size={15} aria-hidden="true" />
          </Button>
        </div>
      )}

      <AutomationDetail
        automation={automation}
        restoreFocusRef={detailReturnFocusRef}
        onClose={() => setSelected(null)}
        onEdit={() => {
          const schedule = automation?.scheduleConfig;
          if (!automation || !schedule) return;
          editorReturnFocusRef.current = detailReturnFocusRef.current;
          setEditor(draftFrom(automation, schedule));
          setSelected(null);
        }}
      />
      <AutomationEditor
        draft={editor}
        projects={projects}
        restoreFocusRef={editorReturnFocusRef}
        onDraftChange={setEditor}
        onClose={() => setEditor(null)}
        onSave={saveDefinition}
      />
    </div>
  );
}

function automationSchedulerStatus(
  state:
    | "kernel_initialized_execution_disabled"
    | "kernel_initialized_execution_enabled"
    | "recovery_pending_execution_disabled"
    | "degraded_execution_disabled"
    | undefined,
): string {
  if (state === "kernel_initialized_execution_enabled") return "Execution enabled";
  if (state === "kernel_initialized_execution_disabled") return "Kernel initialized";
  if (state === "recovery_pending_execution_disabled") return "Recovering";
  return "Unavailable";
}

function SummaryMetric({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof CalendarClock;
  label: string;
  value: string | null;
}) {
  return (
    <div className="flex min-h-[72px] items-center gap-3 rounded-lg border border-border bg-card px-4 py-3">
      <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-muted text-subtle-foreground" aria-hidden="true">
        <Icon size={18} />
      </span>
      <div className="flex min-w-0 flex-col gap-0.5">
        {value === null ? <Skeleton className="h-[22px] w-16" /> : <strong className="font-mono text-title-sm font-semibold text-foreground tabular-nums">{value}</strong>}
        <span className="text-label text-muted-foreground">{label}</span>
      </div>
    </div>
  );
}

function DefinitionsLoading() {
  return (
    <div className="flex flex-col" role="status" aria-label="Loading automation definitions" aria-live="polite">
      <span className="sr-only">Loading automation definitions</span>
      {Array.from({ length: 3 }, (_, index) => (
        <div className="grid min-h-[76px] grid-cols-[minmax(13rem,1.1fr)_minmax(20rem,1.5fr)_auto] items-center gap-4 border-b border-border px-4 py-3 last:border-b-0 max-[900px]:grid-cols-1" key={index}>
          <div className="flex items-center gap-3"><Skeleton className="size-9 shrink-0" /><div className="flex flex-1 flex-col gap-2"><Skeleton className="h-3 w-36" /><Skeleton className="h-3 w-24" /></div></div>
          <div className="grid grid-cols-3 gap-3"><Skeleton className="h-8" /><Skeleton className="h-8" /><Skeleton className="h-8" /></div>
          <Skeleton className="h-[34px] w-20" />
        </div>
      ))}
    </div>
  );
}

function DefinitionRow({ automation, onOpen }: { automation: AutomationSummary; onOpen(trigger: HTMLButtonElement): void }) {
  return (
    <li className="grid min-h-[76px] grid-cols-[minmax(13rem,1.1fr)_minmax(20rem,1.5fr)_auto] items-center gap-4 px-4 py-3 max-[900px]:grid-cols-[minmax(0,1fr)_auto] max-[900px]:gap-y-3 max-[560px]:grid-cols-1">
      <button
        className="flex min-h-[44px] min-w-0 items-center gap-3 rounded-md text-left outline-none transition-[background-color,transform] duration-150 hover:bg-muted active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-ring"
        type="button"
        aria-label={`View ${automation.name}`}
        onClick={(event) => onOpen(event.currentTarget)}
      >
        <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-info-soft text-info" aria-hidden="true">
          <RefreshCw size={17} />
        </span>
        <span className="flex min-w-0 flex-col gap-0.5">
          <strong className="truncate text-body font-semibold text-foreground" title={automation.name}>{automation.name}</strong>
          <span className="truncate text-body-sm text-muted-foreground" title={automation.projectName}>{automation.projectName}</span>
        </span>
      </button>

      <dl className="m-0 grid min-w-0 grid-cols-3 gap-4 max-[900px]:col-span-2 max-[560px]:col-span-1 max-[480px]:grid-cols-1">
        <DefinitionValue label="Schedule" value={automation.schedule} />
        <DefinitionValue label="Next run" value={automation.nextRun} mono />
        <div className="min-w-0">
          <dt className="text-label text-subtle-foreground">Last result</dt>
          <dd className="m-0 mt-1"><Badge variant="neutral"><Clock3 size={12} aria-hidden="true" /> Not run</Badge></dd>
        </div>
      </dl>

      <div className="flex items-center justify-end gap-2 max-[900px]:col-start-2 max-[900px]:row-start-1 max-[560px]:col-start-1 max-[560px]:row-start-auto max-[560px]:justify-start">
        <span className="text-label font-medium text-muted-foreground">
          {automation.enabled ? "Enabled" : "Inactive"}
        </span>
        <Switch
          checked={automation.enabled}
          disabled
          aria-label={`${automation.name} is ${automation.enabled ? "enabled" : "inactive"}`}
        />
        <Button
          type="button"
          variant="ghost"
          size="icon"
          disabled
          aria-label="Additional automation actions require daemon protocol support"
          title="Additional automation actions require daemon protocol support"
        >
          <MoreHorizontal size={17} aria-hidden="true" />
        </Button>
      </div>
    </li>
  );
}

function DefinitionValue({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-label text-subtle-foreground">{label}</dt>
      <dd className={cn("m-0 mt-0.5 [overflow-wrap:anywhere] text-body-sm text-muted-foreground", mono && "font-mono tabular-nums")}>{value}</dd>
    </div>
  );
}

function AutomationDetail({
  automation,
  restoreFocusRef,
  onClose,
  onEdit,
}: {
  automation: AutomationSummary | null;
  restoreFocusRef: React.RefObject<HTMLButtonElement | null>;
  onClose(): void;
  onEdit(): void;
}) {
  return (
    <Sheet open={automation !== null} onOpenChange={(open) => { if (!open) onClose(); }}>
      {automation && (
        <SheetContent
          side="right"
          className="w-[min(430px,100vw)] gap-0 overflow-y-auto p-5 shadow-dialog sm:max-w-none"
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            restoreFocusRef.current?.focus();
          }}
        >
          <SheetHeader className="border-b border-border p-0 pb-4 pr-9">
            <Badge variant="neutral" className="mb-1">Inactive definition</Badge>
            <SheetTitle className="text-title">{automation.name}</SheetTitle>
            <SheetDescription>{automation.projectName}</SheetDescription>
          </SheetHeader>

          <DetailSection title="Task">
            <p className="m-0 whitespace-pre-wrap text-body text-muted-foreground">{automation.prompt ?? "No task prompt recorded."}</p>
          </DetailSection>

          <DetailSection title="Schedule definition">
            <dl className="m-0 flex flex-col gap-3">
              <DetailValue label="Schedule" value={automation.schedule} />
              <DetailValue label="Execution" value="Not scheduled" mono />
              {automation.scheduleConfig && (
                <>
                  <DetailValue label="IANA timezone" value={automation.scheduleConfig.timeZoneIana} mono />
                  <DetailValue label="Windows mapping" value={automation.scheduleConfig.timeZoneWindows ?? "Not resolved until scheduler support is connected"} />
                </>
              )}
              <DetailValue label="Missed run policy" value={automation.missedRunPolicy === "skip" ? "Skip" : "Run once when available"} />
              <DetailValue label="Overlap policy" value={automation.overlapPolicy === "queue_one" ? "Queue one run" : "Skip overlap"} />
            </dl>
          </DetailSection>

          <DetailSection title="Run status">
            <div className="flex items-start gap-2 rounded-lg bg-warning-soft p-3 text-warning">
              <CircleAlert className="mt-0.5 shrink-0" size={16} aria-hidden="true" />
              <p className="m-0 text-body-sm text-muted-foreground">{AUTOMATION_DEFINITION_ONLY_REASON}</p>
            </div>
          </DetailSection>

          <SheetFooter className="mt-auto flex-row justify-end border-t border-border p-0 pt-4 max-[480px]:flex-col-reverse">
            <Button className="max-[480px]:w-full" variant="outline" disabled title={AUTOMATION_DEFINITION_ONLY_REASON}>
              <Play size={15} aria-hidden="true" /> Run unavailable
            </Button>
            <Button
              className="max-[480px]:w-full"
              disabled={!automation.scheduleConfig}
              title={automation.scheduleConfig
                ? undefined
                : "This schedule cannot be edited safely. Create a new definition instead."}
              onClick={onEdit}
            >
              Edit definition
            </Button>
          </SheetFooter>
        </SheetContent>
      )}
    </Sheet>
  );
}

function DetailSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="border-b border-border py-4 last:border-b-0">
      <h3 className="m-0 mb-2 text-body font-semibold text-foreground">{title}</h3>
      {children}
    </section>
  );
}

function DetailValue({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-3 text-body-sm max-[380px]:grid-cols-1 max-[380px]:gap-0.5">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={cn("m-0 [overflow-wrap:anywhere] text-right text-foreground max-[380px]:text-left", mono && "font-mono tabular-nums")}>{value}</dd>
    </div>
  );
}

function AutomationEditor({
  draft,
  projects,
  restoreFocusRef,
  onDraftChange,
  onClose,
  onSave,
}: {
  draft: AutomationDraft | null;
  projects: { id: string; name: string }[];
  restoreFocusRef: React.RefObject<HTMLButtonElement | null>;
  onDraftChange(draft: AutomationDraft): void;
  onClose(): void;
  onSave(draft: AutomationDraft): Promise<void>;
}) {
  const nameRef = useRef<HTMLInputElement>(null);
  const open = draft !== null;
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!open) return;
    setSaving(false);
    setError("");
  }, [open]);

  if (!draft) return <Dialog open={false} />;

  const update = <K extends keyof AutomationDraft>(key: K, value: AutomationDraft[K]) => {
    onDraftChange({ ...draft, [key]: value });
  };
  const updateSchedule = <K extends keyof AutomationSchedule>(key: K, value: AutomationSchedule[K]) => {
    onDraftChange({ ...draft, schedule: { ...draft.schedule, [key]: value } });
  };
  const valid = Boolean(draft.projectId && draft.name.trim() && draft.prompt.trim() && draft.schedule.localTime);

  const submit = async () => {
    if (!valid || saving) return;
    setSaving(true);
    setError("");
    try {
      await onSave(draft);
    } catch (submitError) {
      setError(submitError instanceof Error ? submitError.message : "The definition could not be saved.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open onOpenChange={(nextOpen) => { if (!nextOpen && !saving) onClose(); }}>
      <DialogContent
        className="top-[min(8vh,72px)] grid max-h-[calc(100dvh-32px)] w-[min(680px,calc(100vw-32px))] grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0 max-[680px]:top-0 max-[680px]:h-dvh max-[680px]:max-h-dvh max-[680px]:w-screen max-[680px]:rounded-none max-[680px]:border-y-0"
        showCloseButton={!saving}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          nameRef.current?.focus();
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          restoreFocusRef.current?.focus();
        }}
        onEscapeKeyDown={(event) => { if (saving) event.preventDefault(); }}
      >
        <DialogHeader className="border-b border-border px-5 py-4 pr-12">
          <DialogTitle>{draft.id ? "Edit definition" : "New definition"}</DialogTitle>
          <DialogDescription>{AUTOMATION_DEFINITION_ONLY_REASON}</DialogDescription>
        </DialogHeader>

        <form
          className="grid min-h-0 grid-rows-[minmax(0,1fr)_auto]"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="grid min-h-0 grid-cols-2 content-start gap-4 overflow-y-auto p-5 max-[680px]:grid-cols-1">
            <label className="flex min-w-0 flex-col gap-1 text-body font-semibold text-foreground" htmlFor="automation-name">
              Name
              <Input
                ref={nameRef}
                id="automation-name"
                required
                autoComplete="off"
                disabled={saving}
                value={draft.name}
                placeholder="Morning research scan"
                onChange={(event) => update("name", event.target.value)}
              />
            </label>

            {draft.id ? (
              <div className="flex min-w-0 flex-col gap-1">
                <label className="text-body font-semibold text-foreground" htmlFor="automation-project">Project</label>
                <Input
                  id="automation-project"
                  className="bg-muted text-muted-foreground"
                  readOnly
                  aria-describedby="automation-project-help"
                  value={projects.find((project) => project.id === draft.projectId)?.name ?? "Project unavailable"}
                />
                <span id="automation-project-help" className="text-label font-normal text-muted-foreground">
                  Project ownership cannot be changed after creation.
                </span>
              </div>
            ) : (
              <label className="flex min-w-0 flex-col gap-1 text-body font-semibold text-foreground" htmlFor="automation-project">
                Project
                <Select
                  value={draft.projectId}
                  disabled={saving}
                  onValueChange={(value) => update("projectId", value)}
                >
                  <SelectTrigger id="automation-project" className={selectTriggerClassName}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent position="popper">
                    {projects.map((project) => (
                      <SelectItem value={project.id} key={project.id}>{project.name}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>
            )}

            <label className="col-span-2 flex min-w-0 flex-col gap-1 text-body font-semibold text-foreground max-[680px]:col-span-1" htmlFor="automation-prompt">
              Task prompt
              <Textarea
                id="automation-prompt"
                className="resize-y font-normal"
                rows={4}
                required
                disabled={saving}
                value={draft.prompt}
                placeholder="Describe the recurring work"
                onChange={(event) => update("prompt", event.target.value)}
              />
            </label>

            <fieldset className="col-span-2 min-w-0 rounded-lg border border-border p-3 max-[680px]:col-span-1">
              <legend className="px-1 text-body font-semibold text-foreground">Timing definition</legend>
              <div className="grid grid-cols-3 gap-3 max-[560px]:grid-cols-1">
                <EditorSelect
                  label="Frequency"
                  value={draft.schedule.frequency}
                  disabled={saving}
                  onChange={(value) => updateSchedule("frequency", value as AutomationSchedule["frequency"])}
                  options={[
                    { value: "daily", label: "Every day" },
                    { value: "weekdays", label: "Weekdays" },
                    { value: "weekly", label: "Weekly" },
                    { value: "monthly", label: "Monthly" },
                  ]}
                />

                {draft.schedule.frequency === "weekly" && (
                  <EditorSelect
                    label="Day of week"
                    value={String(draft.schedule.weekday ?? 1)}
                    disabled={saving}
                    onChange={(value) => updateSchedule("weekday", Number(value) as AutomationSchedule["weekday"])}
                    options={["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
                      .map((day, index) => ({ value: String(index), label: day }))}
                  />
                )}

                {draft.schedule.frequency === "monthly" && (
                  <label className="flex min-w-0 flex-col gap-1 text-body-sm font-semibold text-muted-foreground" htmlFor="automation-day-of-month">
                    Day of month
                    <Input
                      id="automation-day-of-month"
                      type="number"
                      min={1}
                      max={31}
                      disabled={saving}
                      value={draft.schedule.dayOfMonth ?? 1}
                      onChange={(event) => updateSchedule("dayOfMonth", Math.min(31, Math.max(1, Number(event.target.value))))}
                    />
                  </label>
                )}

                <label className="flex min-w-0 flex-col gap-1 text-body-sm font-semibold text-muted-foreground" htmlFor="automation-local-time">
                  Local time
                  <Input id="automation-local-time" type="time" required disabled={saving} value={draft.schedule.localTime} onChange={(event) => updateSchedule("localTime", event.target.value)} />
                </label>

                <EditorSelect
                  label="IANA timezone"
                  value={draft.schedule.timeZoneIana}
                  disabled={saving}
                  onChange={(value) => onDraftChange({ ...draft, schedule: { ...draft.schedule, timeZoneIana: value, timeZoneWindows: undefined } })}
                  options={TIME_ZONE_OPTIONS.map((zone) => ({ value: zone, label: zone }))}
                />

                <label className="col-span-3 flex min-w-0 flex-col gap-1 text-body-sm font-semibold text-muted-foreground max-[560px]:col-span-1" htmlFor="automation-windows-timezone">
                  Windows timezone mapping
                  <Input
                    id="automation-windows-timezone"
                    className="bg-muted font-mono text-body-sm text-muted-foreground"
                    readOnly
                    value={draft.schedule.timeZoneWindows ?? "Not resolved until scheduler support is connected"}
                  />
                </label>
              </div>

              <div className="mt-3 flex items-start gap-2 rounded-lg bg-info-soft p-3 text-info">
                <Clock3 className="mt-0.5 shrink-0" size={16} aria-hidden="true" />
                <span className="flex min-w-0 flex-col gap-0.5">
                  <strong className="text-body-sm font-semibold text-foreground">Inactive timing definition</strong>
                  <span className="text-body-sm text-muted-foreground">{formatAutomationSchedule(draft.schedule)}. No run is scheduled.</span>
                </span>
              </div>
            </fieldset>

            <EditorSelect
              label="Missed run policy"
              value={draft.missedRunPolicy}
              disabled={saving}
              onChange={(value) => update("missedRunPolicy", value as AutomationDraft["missedRunPolicy"])}
              options={[
                { value: "run_once", label: "Run once when available" },
                { value: "skip", label: "Skip missed run" },
              ]}
            />

            <EditorSelect
              label="Overlapping runs"
              value={draft.overlapPolicy}
              disabled={saving}
              onChange={(value) => update("overlapPolicy", value as AutomationDraft["overlapPolicy"])}
              options={[
                { value: "queue_one", label: "Queue one run" },
                { value: "skip", label: "Skip overlap" },
              ]}
            />

            <div className="col-span-2 flex min-h-16 items-center justify-between gap-4 rounded-lg border border-border bg-muted p-3 max-[680px]:col-span-1">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <strong className="text-body font-semibold text-foreground">Inactive</strong>
                  <Badge variant="neutral">Not scheduled</Badge>
                </div>
                <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">Saving this definition never starts or schedules a run.</p>
              </div>
              <Switch checked={false} disabled aria-label="Definition is inactive" />
            </div>

            {error && (
              <Alert variant="destructive" className="col-span-2 border-transparent p-3 max-[680px]:col-span-1">
                <CircleAlert size={16} aria-hidden="true" />
                <AlertDescription className="text-body-sm text-destructive">{error}</AlertDescription>
              </Alert>
            )}
          </div>

          <DialogFooter className="border-t border-border px-5 py-3 max-[480px]:flex-col-reverse">
            <Button className="max-[480px]:w-full" type="button" variant="outline" disabled={saving} onClick={onClose}>Cancel</Button>
            <Button className="max-[480px]:w-full" type="submit" disabled={!valid || saving}>
              {saving && <LoaderCircle className="animate-spin motion-reduce:animate-none" size={15} aria-hidden="true" />}
              Save definition
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function EditorSelect({
  label,
  value,
  disabled,
  onChange,
  options,
}: {
  label: string;
  value: string;
  disabled: boolean;
  onChange(value: string): void;
  options: Array<{ value: string; label: string }>;
}) {
  const id = `automation-${label.toLowerCase().replaceAll(" ", "-")}`;
  return (
    <label className="flex min-w-0 flex-col gap-1 text-body-sm font-semibold text-muted-foreground" htmlFor={id}>
      {label}
      <Select value={value} disabled={disabled} onValueChange={onChange}>
        <SelectTrigger id={id} className={selectTriggerClassName}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent position="popper">
          {options.map((option) => (
            <SelectItem value={option.value} key={option.value}>{option.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
    </label>
  );
}

function emptyDraft(projectId = "", enabled = false): AutomationDraft {
  return {
    name: "",
    projectId,
    prompt: "",
    schedule: defaultAutomationSchedule(),
    missedRunPolicy: "run_once",
    overlapPolicy: "queue_one",
    enabled,
  };
}

function draftFrom(value: AutomationSummary, schedule: AutomationSchedule): AutomationDraft {
  return {
    id: value.id,
    name: value.name,
    projectId: value.projectId,
    prompt: value.prompt ?? "",
    schedule: structuredClone(schedule),
    missedRunPolicy: value.missedRunPolicy ?? "run_once",
    overlapPolicy: value.overlapPolicy ?? "queue_one",
    enabled: value.enabled,
  };
}
