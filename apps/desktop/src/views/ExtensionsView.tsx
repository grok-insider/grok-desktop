import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  ArrowDownToLine,
  Blocks,
  Check,
  ChevronRight,
  CircleAlert,
  Globe2,
  HardDrive,
  History,
  LoaderCircle,
  PlugZap,
  RefreshCw,
  Search,
  ShieldCheck,
  Sparkles,
  Star,
  X,
} from "lucide-react";
import { PageHeader } from "../components/ui";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { ExtensionSummary, ManagedIntegrationDetail } from "../services/desktopClient";

type ExtensionFilter = "all" | ExtensionSummary["kind"];
type ManagedAction = "install" | "update" | "rollback";
type Notice = { message: string; tone: "status" | "error" };

const filters: { id: ExtensionFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "built-in", label: "Built-in" },
  { id: "managed", label: "Managed" },
  { id: "mcp", label: "MCP" },
];

export function ExtensionsView() {
  const client = useDesktopClient();
  const { snapshot, loading } = useDesktopSnapshot();
  const [query, setQuery] = useState("");
  const [wisp, setWisp] = useState<ManagedIntegrationDetail | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [filter, setFilter] = useState<ExtensionFilter>("all");
  const [openingWisp, setOpeningWisp] = useState(false);
  const wispReturnFocusRef = useRef<HTMLButtonElement | null>(null);

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const extensions = snapshot?.extensions.filter((item) => {
    const matchesKind = filter === "all" || item.kind === filter;
    const searchable = `${item.name} ${item.description} ${item.permissions.join(" ")}`.toLocaleLowerCase();
    return matchesKind && searchable.includes(normalizedQuery);
  }) ?? [];
  const connected = extensions.filter((item) => item.status === "connected");
  const available = extensions.filter((item) => item.status !== "connected");
  const isolationReady = snapshot?.capabilities.some(
    (item) => ["work", "mcp", "shell"].includes(item.id) && item.available,
  ) ?? false;

  const openWisp = async (returnFocusTarget: HTMLButtonElement) => {
    wispReturnFocusRef.current = returnFocusTarget;
    setOpeningWisp(true);
    setNotice(null);
    try {
      const result = await client.getManagedIntegration("wisp");
      if (result.status === "success") setWisp(result.value);
      else setNotice({ message: result.reason, tone: "error" });
    } finally {
      setOpeningWisp(false);
    }
  };

  const changeWisp = async (action: ManagedAction) => {
    const result = await client.changeManagedIntegration("wisp", action);
    if (result.status === "success") {
      setWisp(result.value);
      setNotice({ message: `Wisp ${action} completed.`, tone: "status" });
    } else {
      setNotice({ message: result.reason, tone: "error" });
    }
  };

  const closeWisp = () => {
    setWisp(null);
    queueMicrotask(() => wispReturnFocusRef.current?.focus());
  };

  const selectFilterFromKeyboard = (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % filters.length;
    if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + filters.length) % filters.length;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = filters.length - 1;
    if (nextIndex === null) return;
    event.preventDefault();
    setFilter(filters[nextIndex].id);
    const tabs = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role='tab']");
    tabs?.[nextIndex]?.focus();
  };

  return (
    <div className="min-h-full px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <div className="mx-auto max-w-[1440px]">
        <PageHeader
          title="Managed integrations (preview)"
          description="Daemon-reported integrations and managed add-on readiness."
          actions={
            <Button disabled title="Adding MCP servers requires daemon protocol support" variant="outline">
              <PlugZap size={15} aria-hidden="true" /> Add MCP server unavailable
            </Button>
          }
        />

        <section
          aria-label="Extension isolation status"
          className={cn(
            "mb-4 flex min-h-16 items-center gap-3 rounded-lg border px-4 py-3",
            loading && "border-info/25 bg-info-soft text-info",
            !loading && isolationReady && "border-success/25 bg-success-soft text-success",
            !loading && !isolationReady && "border-warning/25 bg-warning-soft text-warning",
          )}
        >
          {loading ? (
            <LoaderCircle className="shrink-0 animate-spin" size={19} aria-hidden="true" />
          ) : isolationReady ? (
            <ShieldCheck className="shrink-0" size={19} aria-hidden="true" />
          ) : (
            <CircleAlert className="shrink-0" size={19} aria-hidden="true" />
          )}
          <div className="min-w-0 flex-1">
            <h2 className="m-0 text-body font-semibold text-foreground">
              {loading
                ? "Checking extension isolation"
                : isolationReady
                  ? "Extension isolation backend is ready"
                  : "Extension isolation is unavailable"}
            </h2>
            <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">
              {loading
                ? "Waiting for daemon capability checks."
                : isolationReady
                  ? "Daemon capability checks passed for isolated tool execution."
                  : "No isolated tool capability is currently available."}
            </p>
          </div>
          <Button
            className="max-[680px]:hidden"
            disabled
            title="Permission management requires daemon protocol support"
            variant="ghost"
          >
            Review unavailable
          </Button>
        </section>

        <div className="flex items-center justify-between gap-4 rounded-lg border border-border bg-card p-2 max-[680px]:flex-col max-[680px]:items-stretch">
          <div className="relative min-w-[240px] max-w-md flex-1 max-[680px]:min-w-0">
            <Search
              className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-subtle-foreground"
              size={16}
              aria-hidden="true"
            />
            <label htmlFor="extensions-search" className="sr-only">Search extensions</label>
            <Input
              id="extensions-search"
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search extensions"
              className="pl-9"
            />
          </div>
          <div
            className="flex max-w-full items-center gap-1 overflow-x-auto rounded-md bg-muted p-1"
            role="tablist"
            aria-label="Extension type"
          >
            {filters.map((item, index) => {
              const selected = filter === item.id;
              return (
                <Button
                  key={item.id}
                  role="tab"
                  aria-selected={selected}
                  tabIndex={selected ? 0 : -1}
                  variant={selected ? "secondary" : "ghost"}
                  size="sm"
                  className={cn("min-w-fit", selected && "bg-card font-semibold shadow-raised hover:bg-card")}
                  onClick={() => setFilter(item.id)}
                  onKeyDown={(event) => selectFilterFromKeyboard(event, index)}
                >
                  {item.label}
                </Button>
              );
            })}
          </div>
        </div>

        <ExtensionSection
          title="Connected"
          description="Integrations confirmed available by daemon capability checks"
          emptyMessage="No connected integrations match this search."
          items={connected}
          renderAction={() => (
            <Button disabled title="Connection details require daemon protocol support" variant="outline">
              <Check className="text-success" size={14} aria-hidden="true" />
              <span className="text-success">Connected</span>
              <ChevronRight size={15} aria-hidden="true" />
            </Button>
          )}
        />

        <ExtensionSection
          title="Managed and unavailable"
          description="Only signed, daemon-managed changes can run; unsupported integrations stay unavailable"
          emptyMessage="No managed or unavailable integrations match this search."
          items={available}
          renderAction={(item) => item.id === "wisp" ? (
            <Button onClick={(event) => void openWisp(event.currentTarget)} disabled={openingWisp}>
              {openingWisp && <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />}
              View details
            </Button>
          ) : (
            <Button disabled title={item.permissions.join(". ")} variant="outline">Unavailable</Button>
          )}
        />
      </div>

      {notice && !wisp && <NoticeToast notice={notice} onDismiss={() => setNotice(null)} />}

      {wisp && (
        <WispDetail
          detail={wisp}
          notice={notice}
          returnFocusTarget={wispReturnFocusRef.current}
          onDismissNotice={() => setNotice(null)}
          onClose={closeWisp}
          onAction={changeWisp}
        />
      )}
    </div>
  );
}

function ExtensionSection({
  title,
  description,
  emptyMessage,
  items,
  renderAction,
}: {
  title: string;
  description: string;
  emptyMessage: string;
  items: ExtensionSummary[];
  renderAction(item: ExtensionSummary): ReactNode;
}) {
  return (
    <section className="mt-6" aria-labelledby={`extensions-${title.toLocaleLowerCase().replaceAll(" ", "-")}`}>
      <div className="mb-3">
        <h2
          id={`extensions-${title.toLocaleLowerCase().replaceAll(" ", "-")}`}
          className="m-0 text-title-sm font-semibold text-foreground"
        >
          {title}
        </h2>
        <p className="m-0 mt-0.5 text-body-sm text-subtle-foreground">{description}</p>
      </div>
      <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
        {items.map((item) => <ExtensionCard key={item.id} item={item} action={renderAction(item)} />)}
        {items.length === 0 && (
          <p className="col-span-full m-0 rounded-lg border border-dashed border-input bg-muted px-4 py-8 text-center text-body text-muted-foreground">
            {emptyMessage}
          </p>
        )}
      </div>
    </section>
  );
}

function ExtensionCard({ item, action }: { item: ExtensionSummary; action: ReactNode }) {
  const ExtensionIcon = item.id === "browser" ? Globe2 : item.id === "filesystem" ? HardDrive : item.id === "wisp" ? Sparkles : Blocks;
  const kindLabel = item.kind === "mcp" ? "MCP" : item.kind === "managed" ? "Managed" : "Built-in";
  return (
    <article className="grid min-h-36 grid-cols-[40px_minmax(0,1fr)_auto] gap-3 rounded-lg border border-border bg-card p-4 max-[680px]:grid-cols-[40px_minmax(0,1fr)]">
      <div
        className={cn(
          "grid size-10 place-items-center rounded-lg",
          item.kind === "built-in" && "bg-info-soft text-info",
          item.kind === "managed" && "bg-warning-soft text-warning",
          item.kind === "mcp" && "bg-muted text-muted-foreground",
        )}
        aria-hidden="true"
      >
        <ExtensionIcon size={20} />
      </div>
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="m-0 text-body font-semibold text-foreground">{item.name}</h3>
          {item.recommended && (
            <Badge variant="success"><Star size={11} aria-hidden="true" /> Recommended</Badge>
          )}
          <Badge variant="outline">{kindLabel}</Badge>
        </div>
        <p className="m-0 mt-2 min-h-10 text-body-sm text-muted-foreground">{item.description}</p>
        <div className="mt-2 flex flex-wrap gap-1.5" aria-label={`${item.name} permissions and requirements`}>
          {item.permissions.map((permission) => (
            <Badge key={permission} variant={item.status === "attention" ? "warning" : "neutral"}>
              {item.status === "attention" && <CircleAlert size={12} aria-hidden="true" />}
              {permission}
            </Badge>
          ))}
        </div>
      </div>
      <div className="self-center max-[680px]:col-span-full max-[680px]:[&_[data-slot=button]]:w-full">{action}</div>
    </article>
  );
}

function WispDetail({
  detail,
  notice,
  returnFocusTarget,
  onDismissNotice,
  onClose,
  onAction,
}: {
  detail: ManagedIntegrationDetail;
  notice: Notice | null;
  returnFocusTarget: HTMLButtonElement | null;
  onDismissNotice(): void;
  onClose(): void;
  onAction(action: ManagedAction): Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const action: ManagedAction | null = detail.state === "available"
    ? "install"
    : detail.state === "update_available"
      ? "update"
      : detail.state === "rollback_available"
        ? "rollback"
        : null;
  const actionBlockedReason = action
    ? detail.checks.find((check) => check.state === "action_required")?.detail
    : undefined;
  const progress = Math.max(0, Math.min(100, detail.progress ?? 0));
  const actionLabel = detail.state === "installing"
    ? "Installing signed component"
    : action === "install"
      ? "Install Wisp"
      : action === "update"
        ? `Update to ${detail.availableVersion}`
        : action === "rollback"
          ? `Rollback to ${detail.rollbackVersion}`
          : "Wisp is up to date";

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent
        showCloseButton={false}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          returnFocusTarget?.focus();
        }}
        className="inset-y-0 top-0 right-0 left-auto flex h-dvh w-[min(475px,100vw)] max-w-none translate-x-0 flex-col gap-0 overflow-y-auto rounded-none border-y-0 border-r-0 p-5 shadow-dialog data-[state=open]:slide-in-from-right-4 max-[480px]:w-screen"
      >
        <DialogHeader className="flex-row items-start justify-between gap-4 border-b border-border pb-4 text-left">
          <div className="flex min-w-0 items-center gap-3">
            <span className="grid size-10 shrink-0 place-items-center rounded-lg bg-warning-soft text-warning" aria-hidden="true">
              <Sparkles size={20} />
            </span>
            <div className="min-w-0">
              <Badge variant="success"><Star size={11} aria-hidden="true" /> Recommended managed add-on</Badge>
              <DialogTitle className="mt-1 text-title">Wisp</DialogTitle>
              <DialogDescription className="mt-0.5">
                Signed, versioned component lifecycle managed outside the renderer.
              </DialogDescription>
            </div>
          </div>
          <DialogClose asChild>
            <Button aria-label="Close" title="Close" variant="ghost" size="icon">
              <X size={18} aria-hidden="true" />
            </Button>
          </DialogClose>
        </DialogHeader>

        {notice && <NoticeToast notice={notice} onDismiss={onDismissNotice} />}

        <div className="my-4 grid grid-cols-[1fr_auto_1fr] items-center gap-2 rounded-lg border border-border bg-muted p-3">
          <VersionValue label="Installed" value={detail.installedVersion ?? "Not installed"} />
          <ChevronRight className="text-subtle-foreground" size={16} aria-hidden="true" />
          <VersionValue label="Available" value={detail.availableVersion} />
        </div>

        <div className="mb-4 flex items-center justify-between gap-3">
          <span className="text-label font-semibold uppercase tracking-[0.06em] text-subtle-foreground">Component state</span>
          <ManagedStateBadge state={detail.state} />
        </div>

        {detail.state === "installing" && (
          <div className="mb-4 flex items-center gap-3 rounded-lg bg-info-soft p-3 text-info" aria-live="polite">
            <LoaderCircle size={16} className="shrink-0 animate-spin" aria-hidden="true" />
            <div className="min-w-0 flex-1">
              <div className="mb-2 flex items-center justify-between gap-3 text-body-sm font-semibold">
                <span>Installing signed component</span>
                <span className="font-mono tabular-nums">{progress}%</span>
              </div>
              <div
                className="h-1 overflow-hidden rounded-full bg-secondary"
                role="progressbar"
                aria-label="Wisp installation progress"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={progress}
              >
                <span
                  className="block h-full origin-left rounded-full bg-info transition-transform duration-200 ease-fluid"
                  style={{ transform: `scaleX(${progress / 100})` }}
                />
              </div>
            </div>
          </div>
        )}

        <DetailSection title="Readiness checks">
          <div className="flex flex-col gap-2">
            {detail.checks.map((check) => (
              <div
                key={check.label}
                className={cn(
                  "flex items-start gap-2 rounded-md p-2",
                  check.state === "ready" ? "bg-success-soft text-success" : "bg-warning-soft text-warning",
                )}
              >
                {check.state === "ready" ? (
                  <Check className="mt-0.5 shrink-0" size={15} aria-hidden="true" />
                ) : (
                  <CircleAlert className="mt-0.5 shrink-0" size={15} aria-hidden="true" />
                )}
                <span className="flex min-w-0 flex-col">
                  <strong className="text-body-sm font-semibold text-foreground">{check.label}</strong>
                  <span className="text-label text-muted-foreground">{check.detail}</span>
                </span>
              </div>
            ))}
          </div>
        </DetailSection>

        <DetailSection title="Requested permissions">
          <ul className="m-0 flex list-none flex-col gap-2 p-0">
            {detail.permissions.map((permission) => (
              <li key={permission} className="flex items-start gap-2 text-body-sm text-muted-foreground">
                <ShieldCheck className="mt-0.5 shrink-0 text-success" size={14} aria-hidden="true" />
                {permission}
              </li>
            ))}
          </ul>
        </DetailSection>

        <DetailSection title="What’s new">
          <ul className="m-0 flex list-none flex-col gap-2 p-0">
            {detail.releaseNotes.map((note) => (
              <li key={note} className="flex items-start gap-2 text-body-sm text-muted-foreground">
                <RefreshCw className="mt-0.5 shrink-0 text-info" size={13} aria-hidden="true" />
                {note}
              </li>
            ))}
          </ul>
        </DetailSection>

        {detail.rollbackVersion && (
          <div className="mt-4 flex gap-2 rounded-lg border border-warning/25 bg-warning-soft p-3 text-warning">
            <History className="mt-0.5 shrink-0" size={16} aria-hidden="true" />
            <span className="flex min-w-0 flex-col">
              <strong className="text-body-sm font-semibold text-foreground">Signed rollback available</strong>
              <span className="text-label text-muted-foreground">
                Version {detail.rollbackVersion} can be restored without changing grants.
              </span>
            </span>
          </div>
        )}

        <DialogFooter className="mt-auto border-t border-border pt-4 max-[480px]:flex-col-reverse">
          <Button
            className="max-[480px]:w-full"
            disabled={!action || Boolean(actionBlockedReason) || busy}
            title={actionBlockedReason ?? (!action ? "No managed component change is currently available." : undefined)}
            onClick={async () => {
              if (!action) return;
              setBusy(true);
              try {
                await onAction(action);
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? (
              <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />
            ) : action === "install" ? (
              <ArrowDownToLine size={15} aria-hidden="true" />
            ) : action === "rollback" ? (
              <History size={15} aria-hidden="true" />
            ) : (
              <RefreshCw size={15} aria-hidden="true" />
            )}
            {actionLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function VersionValue({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="text-label text-subtle-foreground">{label}</span>
      <strong className="truncate font-mono text-body-sm font-semibold text-foreground tabular-nums" title={value}>{value}</strong>
    </div>
  );
}

function ManagedStateBadge({ state }: { state: ManagedIntegrationDetail["state"] }) {
  const labels: Record<ManagedIntegrationDetail["state"], string> = {
    available: "Available",
    installing: "Installing",
    installed: "Installed",
    update_available: "Update available",
    rollback_available: "Rollback available",
  };
  const variants: Record<ManagedIntegrationDetail["state"], "neutral" | "info" | "success" | "warning"> = {
    available: "neutral",
    installing: "info",
    installed: "success",
    update_available: "info",
    rollback_available: "warning",
  };
  return (
    <Badge variant={variants[state]}>
      <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
      {labels[state]}
    </Badge>
  );
}

function DetailSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="py-4">
      <Separator className="mb-4" />
      <h3 className="m-0 mb-3 text-body font-semibold text-foreground">{title}</h3>
      {children}
    </section>
  );
}

function NoticeToast({ notice, onDismiss }: { notice: Notice; onDismiss(): void }) {
  return (
    <div
      className={cn(
        "fixed right-5 bottom-5 z-[110] flex max-w-[min(420px,calc(100vw-32px))] items-center gap-3 rounded-lg border bg-card px-4 py-3 text-body shadow-overlay",
        notice.tone === "error" ? "border-destructive/25 text-destructive" : "border-success/25 text-success",
      )}
      role={notice.tone === "error" ? "alert" : "status"}
    >
      <span className="min-w-0 flex-1 text-foreground">{notice.message}</span>
      <Button
        aria-label="Dismiss"
        title="Dismiss"
        variant="ghost"
        size="icon"
        className="size-[27px]"
        onClick={onDismiss}
      >
        <X size={14} aria-hidden="true" />
      </Button>
    </div>
  );
}
