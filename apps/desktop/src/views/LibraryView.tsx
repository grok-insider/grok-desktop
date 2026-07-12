import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import {
  ArrowDownToLine,
  Braces,
  CircleAlert,
  File,
  FileImage,
  FileSpreadsheet,
  FileText,
  Film,
  Grid2X2,
  Image,
  Info,
  List,
  LoaderCircle,
  Plus,
  Search,
  ShieldCheck,
  Sparkles,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { PageHeader } from "../components/ui";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type {
  ArtifactOpenResult,
  ClientResult,
  DesktopClient,
  DesktopSnapshot,
  LibraryItem,
  MediaCreation,
} from "../services/desktopClient";

type LibraryFilter = "all" | LibraryItem["type"];
type LibraryTab = "files" | "images" | "videos";

// Media/Imagine creation is not a shipped product surface until daemon media IPC lands.
const LIBRARY_TABS: ReadonlyArray<{
  id: LibraryTab;
  label: string;
  icon: typeof FileText;
}> = [
  { id: "files", label: "Files", icon: FileText },
];

const FILE_FILTERS: ReadonlyArray<{
  id: LibraryFilter;
  label: string;
}> = [
  { id: "all", label: "All" },
  { id: "document", label: "Documents" },
  { id: "data", label: "Data" },
  { id: "image", label: "Images" },
  { id: "video", label: "Videos" },
  { id: "code", label: "Code" },
];

const libraryRevisionLabel = (item: LibraryItem) => item.contentVersion === undefined
  ? `metadata r${item.metadataRevision}`
  : `v${item.contentVersion}`;

const MEDIA_STATUS = {
  queued: { label: "Queued", variant: "neutral" as const },
  generating: { label: "Generating", variant: "info" as const },
  completed: { label: "Completed", variant: "success" as const },
  failed: { label: "Failed", variant: "destructive" as const },
  cancelled: { label: "Cancelled", variant: "neutral" as const },
} satisfies Record<MediaCreation["status"], { label: string; variant: "neutral" | "info" | "success" | "destructive" }>;

const iconFor = (type: LibraryItem["type"]) =>
  type === "image"
    ? FileImage
    : type === "video"
      ? Video
      : type === "code"
        ? Braces
        : type === "data"
          ? FileSpreadsheet
          : FileText;

export function LibraryView() {
  const [tab, setTab] = useState<LibraryTab>("files");
  const client = useDesktopClient();
  const { snapshot, loading } = useDesktopSnapshot();
  const filesCapability = snapshot?.capabilities.find((capability) => capability.id === "files");

  const selectTabFromKeyboard = (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % LIBRARY_TABS.length;
    if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + LIBRARY_TABS.length) % LIBRARY_TABS.length;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = LIBRARY_TABS.length - 1;
    if (nextIndex === null) return;

    event.preventDefault();
    setTab(LIBRARY_TABS[nextIndex].id);
    const tabs = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role='tab']");
    tabs?.[nextIndex]?.focus();
  };

  return (
    <div className="min-h-full overflow-y-auto px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <div className="mx-auto max-w-[1540px]">
        <PageHeader
          title="Library"
          description="Daemon-reported files and imported artifacts. Media generation is not available in this build."
          actions={
            tab === "files" ? (
              <ImportArtifactAction
                client={client}
                snapshot={snapshot}
                loading={loading}
                filesAvailable={filesCapability?.available === true}
                filesReason={filesCapability?.reason}
              />
            ) : undefined
          }
        />

        <div
          className="mb-4 flex gap-1 border-b border-border"
          role="tablist"
          aria-label="Library sections"
          aria-orientation="horizontal"
        >
          {LIBRARY_TABS.map((item, index) => {
            const selected = tab === item.id;
            const Icon = item.icon;
            return (
              <button
                key={item.id}
                id={`library-${item.id}-tab`}
                type="button"
                role="tab"
                aria-selected={selected}
                aria-controls={`library-${item.id}-panel`}
                tabIndex={selected ? 0 : -1}
                className={cn(
                  "flex h-10 items-center gap-2 border-b-2 border-transparent px-3 text-body font-medium text-muted-foreground outline-none",
                  "transition-[background-color,border-color,color,transform] duration-150 ease-fluid hover:bg-muted hover:text-foreground active:scale-[.98] focus-visible:ring-[3px] focus-visible:ring-ring",
                  selected && "border-primary font-semibold text-foreground",
                )}
                onClick={() => setTab(item.id)}
                onKeyDown={(event) => selectTabFromKeyboard(event, index)}
              >
                <Icon size={16} aria-hidden="true" />
                {item.label}
              </button>
            );
          })}
        </div>

        <div
          id="library-files-panel"
          role="tabpanel"
          aria-labelledby="library-files-tab"
          tabIndex={tab === "files" ? 0 : -1}
          hidden={tab !== "files"}
        >
          {tab === "files" && (
            <FilesLibrary
              snapshot={snapshot}
              loading={loading}
              filesAvailable={filesCapability?.available === true}
              filesReason={filesCapability?.reason}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function ImportArtifactAction({
  client,
  snapshot,
  loading,
  filesAvailable,
  filesReason,
}: {
  client: DesktopClient;
  snapshot: DesktopSnapshot | null;
  loading: boolean;
  filesAvailable: boolean;
  filesReason?: string;
}) {
  const projects = useMemo(() => snapshot?.projects ?? [], [snapshot?.projects]);
  const preview = snapshot?.connection.interfacePreview === true;
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [projectId, setProjectId] = useState("");
  const [status, setStatus] = useState<{
    kind: "success" | "cancelled" | "error";
    message: string;
  } | null>(null);

  useEffect(() => {
    if (!projects.some((project) => project.id === projectId)) {
      setProjectId(projects[0]?.id ?? "");
    }
  }, [projectId, projects]);

  const unavailableReason = loading
    ? "The Library is still loading."
    : preview
      ? "File import is available only in the installed desktop application."
      : !filesAvailable
        ? filesReason ?? "Local artifact content is unavailable."
        : projects.length === 0
          ? "Create an active project before importing a file."
          : undefined;

  const submit = async () => {
    if (!projectId || pending || unavailableReason) return;
    setPending(true);
    setStatus(null);
    try {
      const result = await client.importArtifact(projectId);
      if (result.status === "success") {
        setOpen(false);
        setStatus({ kind: "success", message: `${result.value.name} was added to the Library.` });
      } else if (result.status === "cancelled") {
        setOpen(false);
        setStatus({ kind: "cancelled", message: result.reason });
      } else {
        setStatus({ kind: "error", message: result.reason });
      }
    } catch (error: unknown) {
      setStatus({
        kind: "error",
        message: error instanceof Error ? error.message : "The file could not be imported.",
      });
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="flex max-w-sm flex-col items-end gap-1 max-[680px]:w-full max-[680px]:items-stretch">
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogTrigger asChild>
          <Button
            disabled={Boolean(unavailableReason) || pending}
            title={unavailableReason}
            onClick={() => setStatus(null)}
          >
            <ArrowDownToLine size={15} aria-hidden="true" /> Import file
          </Button>
        </DialogTrigger>
        <DialogContent aria-describedby="artifact-import-description">
          <DialogHeader>
            <DialogTitle>Import a file</DialogTitle>
            <DialogDescription id="artifact-import-description">
              Choose the project that will own the imported file. Grok Desktop will ask you to select exactly one file next.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <label className="text-body font-medium text-foreground" htmlFor="artifact-import-project">
              Project
            </label>
            <select
              id="artifact-import-project"
              className="h-[34px] w-full rounded-md border border-input bg-card px-3 text-body text-foreground outline-none transition-[border-color] hover:border-input-hover focus-visible:ring-[3px] focus-visible:ring-ring"
              value={projectId}
              disabled={pending}
              onChange={(event) => setProjectId(event.target.value)}
            >
              {projects.map((project) => (
                <option key={project.id} value={project.id}>{project.name}</option>
              ))}
            </select>
          </div>
          {pending && (
            <p className="m-0 flex items-center gap-2 text-body-sm text-muted-foreground" role="status">
              <LoaderCircle className="animate-spin" size={15} aria-hidden="true" /> Waiting for file selection…
            </p>
          )}
          {status?.kind === "error" && (
            <p className="m-0 text-body-sm text-destructive" role="alert">{status.message}</p>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" disabled={pending} onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="button" disabled={pending || !projectId} onClick={() => void submit()}>
              {pending && <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />}
              {pending ? "Importing…" : "Choose file"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {status && status.kind !== "error" && (
        <p
          className={cn(
            "m-0 text-right text-body-sm max-[680px]:text-left",
            status.kind === "success" ? "text-success" : "text-muted-foreground",
          )}
          role="status"
          aria-live="polite"
        >
          {status.message}
        </p>
      )}
    </div>
  );
}

function FilesLibrary({
  snapshot,
  loading,
  filesAvailable,
  filesReason,
}: {
  snapshot: DesktopSnapshot | null;
  loading: boolean;
  filesAvailable: boolean;
  filesReason?: string;
}) {
  const [filter, setFilter] = useState<LibraryFilter>("all");
  const [query, setQuery] = useState("");
  const [layout, setLayout] = useState<"grid" | "list">("grid");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selectedTriggerRef = useRef<HTMLButtonElement | null>(null);
  const itemTriggerRefs = useRef(new Map<string, HTMLButtonElement>());
  const libraryItemsRef = useRef<HTMLElement | null>(null);
  const [removalAnnouncement, setRemovalAnnouncement] = useState("");
  const [removalFocusRequest, setRemovalFocusRequest] = useState<{
    removedId: string;
    priorIndex: number;
  } | null>(null);

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const items = useMemo(
    () =>
      snapshot?.library.filter((item) => {
        const matchesFilter = filter === "all" || item.type === filter;
        const searchable = `${item.name} ${item.projectName}`.toLocaleLowerCase();
        return matchesFilter && searchable.includes(normalizedQuery);
      }) ?? [],
    [filter, normalizedQuery, snapshot],
  );
  const selected = items.find((item) => item.id === selectedId);
  const hasActiveFilter = filter !== "all" || normalizedQuery.length > 0;

  useEffect(() => {
    if (!removalFocusRequest || items.some((item) => item.id === removalFocusRequest.removedId)) return;
    const next = items[Math.min(removalFocusRequest.priorIndex, items.length - 1)];
    setRemovalFocusRequest(null);
    queueMicrotask(() => {
      if (next) itemTriggerRefs.current.get(next.id)?.focus();
      else libraryItemsRef.current?.focus();
    });
  }, [items, removalFocusRequest]);

  const closeInspector = () => {
    setSelectedId(null);
    queueMicrotask(() => selectedTriggerRef.current?.focus());
  };

  const clearSelection = () => {
    setSelectedId(null);
    selectedTriggerRef.current = null;
  };

  const clearFilters = () => {
    setFilter("all");
    setQuery("");
    clearSelection();
  };

  const artifactRemoved = (item: LibraryItem, outcome: "removed" | "pending") => {
    setRemovalFocusRequest({
      removedId: item.id,
      priorIndex: Math.max(0, items.findIndex((candidate) => candidate.id === item.id)),
    });
    setRemovalAnnouncement(
      outcome === "pending"
        ? `${item.name} was removed from Library; private cleanup is still continuing. The original file remains untouched.`
        : `${item.name} was removed from Grok Desktop. The original file remains untouched.`,
    );
    setSelectedId(null);
    selectedTriggerRef.current = null;
  };

  return (
    <>
      <div className="flex min-h-14 items-center gap-2 rounded-lg border border-border bg-card p-2 max-[680px]:flex-wrap">
        <div className="relative min-w-[240px] max-w-md flex-1 max-[680px]:min-w-0 max-[680px]:basis-full">
          <Search
            className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-subtle-foreground"
            size={16}
            aria-hidden="true"
          />
          <label className="sr-only" htmlFor="library-search">Search library</label>
          <Input
            id="library-search"
            type="search"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              clearSelection();
            }}
            placeholder="Search files and artifacts"
            className="pr-9 pl-9"
          />
          {query && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute top-0 right-0"
              aria-label="Clear library search"
              onClick={() => {
                setQuery("");
                clearSelection();
              }}
            >
              <X size={15} aria-hidden="true" />
            </Button>
          )}
        </div>

        <div
          className="flex max-w-full items-center gap-1 overflow-x-auto rounded-md bg-muted p-1"
          role="group"
          aria-label="File type"
        >
          {FILE_FILTERS.map((item) => {
            const selectedFilter = filter === item.id;
            return (
              <Button
                key={item.id}
                type="button"
                variant={selectedFilter ? "secondary" : "ghost"}
                size="sm"
                aria-pressed={selectedFilter}
                className={cn("min-w-fit", selectedFilter && "bg-card text-foreground shadow-raised hover:bg-card")}
                onClick={() => {
                  setFilter(item.id);
                  clearSelection();
                }}
              >
                {item.label}
              </Button>
            );
          })}
        </div>

        <div className="ml-auto flex items-center gap-1 border-l border-border pl-2" role="group" aria-label="Library layout">
          <Button
            type="button"
            variant={layout === "grid" ? "secondary" : "ghost"}
            size="icon"
            aria-label="Grid view"
            aria-pressed={layout === "grid"}
            className={cn(layout === "grid" && "bg-accent text-foreground hover:bg-accent")}
            onClick={() => setLayout("grid")}
          >
            <Grid2X2 size={16} aria-hidden="true" />
          </Button>
          <Button
            type="button"
            variant={layout === "list" ? "secondary" : "ghost"}
            size="icon"
            aria-label="List view"
            aria-pressed={layout === "list"}
            className={cn(layout === "list" && "bg-accent text-foreground hover:bg-accent")}
            onClick={() => setLayout("list")}
          >
            <List size={17} aria-hidden="true" />
          </Button>
        </div>
      </div>

      <div
        className={cn(
          "mt-4 grid min-h-[500px] min-w-0 grid-cols-1 gap-4",
          selected && "grid-cols-[minmax(0,1fr)_18rem] max-[900px]:grid-cols-1",
        )}
      >
        {removalAnnouncement && (
          <p className="sr-only" role="status" aria-live="polite" aria-atomic="true">
            {removalAnnouncement}
          </p>
        )}
        <section
          ref={libraryItemsRef}
          tabIndex={-1}
          className={cn(
            "min-w-0",
            layout === "grid"
              ? "grid auto-rows-max grid-cols-[repeat(auto-fill,minmax(190px,1fr))] content-start gap-3"
              : "flex flex-col overflow-hidden rounded-lg border border-border bg-card",
          )}
          aria-label="Library items"
          aria-busy={loading}
        >
          {loading ? (
            <LibrarySkeleton layout={layout} />
          ) : (
            items.map((item) => (
              <LibraryItemCard
                key={item.id}
                item={item}
                layout={layout}
                selected={selectedId === item.id}
                onTriggerChange={(target) => {
                  if (target) itemTriggerRefs.current.set(item.id, target);
                  else itemTriggerRefs.current.delete(item.id);
                }}
                onSelect={(target) => {
                  selectedTriggerRef.current = target;
                  setSelectedId(item.id);
                }}
              />
            ))
          )}

          {!loading && items.length === 0 && (
            <div
              className={cn(
                "flex min-h-52 flex-col items-center justify-center px-6 py-10 text-center",
                layout === "grid" && "col-span-full rounded-lg border border-border bg-card",
              )}
            >
              <span className="grid size-11 place-items-center rounded-lg bg-muted text-muted-foreground">
                <File size={22} aria-hidden="true" />
              </span>
              <h2 className="m-0 mt-3 text-title-sm font-semibold text-foreground">
                {hasActiveFilter ? "No matching items" : "No library items"}
              </h2>
              <p className="m-0 mt-1 max-w-[55ch] text-body-sm text-muted-foreground">
                {hasActiveFilter
                  ? "Try a different search or clear the file type filter."
                  : "Files and artifacts will appear after the daemon reports them."}
              </p>
              {hasActiveFilter && (
                <Button className="mt-4" variant="outline" onClick={clearFilters}>Clear filters</Button>
              )}
            </div>
          )}
        </section>

        {selected && (
          <ArtifactInspector
            item={selected}
            onClose={closeInspector}
            onRemoved={artifactRemoved}
            filesAvailable={filesAvailable}
            filesReason={filesReason}
          />
        )}
      </div>
    </>
  );
}

function LibrarySkeleton({ layout }: { layout: "grid" | "list" }) {
  if (layout === "list") {
    return (
      <div className="flex flex-col" role="status" aria-label="Loading library items">
        {Array.from({ length: 5 }).map((_, index) => (
          <div className="flex h-16 items-center gap-3 border-b border-border p-2 last:border-b-0" key={index}>
            <Skeleton className="h-12 w-14 shrink-0" />
            <div className="flex flex-1 flex-col gap-2">
              <Skeleton className="h-3 w-2/5" />
              <Skeleton className="h-3 w-3/5" />
            </div>
          </div>
        ))}
        <span className="sr-only">Loading library items</span>
      </div>
    );
  }

  return (
    <div className="contents" role="status" aria-label="Loading library items">
      {Array.from({ length: 6 }).map((_, index) => (
        <div className="overflow-hidden rounded-lg border border-border bg-card" key={index}>
          <Skeleton className="h-28 rounded-none" />
          <div className="flex flex-col gap-2 p-3">
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-3 w-1/2" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        </div>
      ))}
      <span className="sr-only">Loading library items</span>
    </div>
  );
}

function LibraryItemCard({
  item,
  layout,
  selected,
  onTriggerChange,
  onSelect,
}: {
  item: LibraryItem;
  layout: "grid" | "list";
  selected: boolean;
  onTriggerChange(target: HTMLButtonElement | null): void;
  onSelect(target: HTMLButtonElement): void;
}) {
  const AssetIcon = iconFor(item.type);

  return (
    <article
      className={cn(
        "min-w-0 overflow-hidden bg-card transition-[background-color,border-color,box-shadow,transform] duration-150 ease-fluid",
        layout === "grid" ? "rounded-lg border border-border" : "border-b border-border last:border-b-0",
        "hover:border-input-hover hover:shadow-raised",
        selected && "border-input-hover bg-accent shadow-raised",
      )}
    >
      <button
        ref={onTriggerChange}
        type="button"
        className={cn(
          "flex w-full min-w-0 text-left text-foreground outline-none transition-transform duration-150 ease-fluid active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-inset focus-visible:ring-ring",
          layout === "grid" ? "min-h-44 flex-col" : "min-h-16 items-center",
        )}
        aria-pressed={selected}
        onClick={(event) => onSelect(event.currentTarget)}
      >
        <span
          className={cn(
            "grid shrink-0 place-items-center bg-muted text-muted-foreground",
            layout === "grid" ? "h-28 w-full border-b border-border" : "ml-2 h-12 w-14 rounded-md",
          )}
        >
          <AssetIcon size={layout === "grid" ? 28 : 22} aria-hidden="true" />
        </span>
        <span className="flex min-w-0 flex-1 flex-col gap-1 p-3">
          <strong className="truncate text-body font-semibold text-foreground" title={item.name}>{item.name}</strong>
          <span className="truncate text-body-sm text-muted-foreground" title={item.projectName}>{item.projectName}</span>
          <span className="font-mono text-label text-subtle-foreground tabular-nums">
            {item.size} · {libraryRevisionLabel(item)} · {item.updatedAt}
          </span>
        </span>
      </button>
    </article>
  );
}

function ArtifactInspector({
  item,
  onClose,
  onRemoved,
  filesAvailable,
  filesReason,
}: {
  item: LibraryItem;
  onClose(): void;
  onRemoved(item: LibraryItem, outcome: "removed" | "pending"): void;
  filesAvailable: boolean;
  filesReason?: string;
}) {
  const client = useDesktopClient();
  const AssetIcon = iconFor(item.type);
  const [opening, setOpening] = useState(false);
  const [removeOpen, setRemoveOpen] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [removeError, setRemoveError] = useState("");
  const cancelRemovalRef = useRef<HTMLButtonElement | null>(null);
  const removalInFlightRef = useRef(false);
  const removalSucceededRef = useRef(false);
  const [openStatus, setOpenStatus] = useState<{
    kind: "success" | "failed" | "review";
    message: string;
  } | null>(null);

  useEffect(() => {
    setOpening(false);
    setOpenStatus(null);
    setRemoveOpen(false);
    setRemoving(false);
    setRemoveError("");
    removalInFlightRef.current = false;
    removalSucceededRef.current = false;
  }, [item.contentVersion, item.id]);

  const unavailableReason = item.contentVersion === undefined
    ? "This item has no immutable content version to open."
    : !filesAvailable
      ? filesReason ?? "Local artifact opening is unavailable."
      : undefined;
  const removeUnavailableReason = item.contentVersion === undefined
    ? "This item has no immutable content version to remove."
    : !filesAvailable
      ? filesReason ?? "Local artifact removal is unavailable."
      : undefined;

  const openArtifact = async () => {
    const contentVersion = item.contentVersion;
    if (contentVersion === undefined || unavailableReason || opening) return;
    setOpening(true);
    setOpenStatus(null);
    try {
      const result = await client.openArtifact(item.id, contentVersion);
      if (result.status !== "success") {
        setOpenStatus({ kind: "failed", message: result.reason });
        return;
      }
      if (result.value.status === "opened") {
        setOpenStatus({ kind: "success", message: `Opened content version ${contentVersion}.` });
      } else if (result.value.status === "failed") {
        setOpenStatus({
          kind: "failed",
          message: artifactOpenFailureMessage(contentVersion, result.value.failureCode),
        });
      } else {
        setOpenStatus({
          kind: "review",
          message: `Opening content version ${contentVersion} was interrupted and needs review. It was not replayed.`,
        });
      }
    } catch (error: unknown) {
      setOpenStatus({
        kind: "failed",
        message: error instanceof Error ? error.message : "The artifact could not be opened.",
      });
    } finally {
      setOpening(false);
    }
  };

  const removeArtifact = async () => {
    const contentVersion = item.contentVersion;
    if (contentVersion === undefined || removeUnavailableReason || removalInFlightRef.current) return;
    removalInFlightRef.current = true;
    setRemoving(true);
    setRemoveError("");
    try {
      const result = await client.removeArtifact(item.id, item.metadataRevision, contentVersion);
      if (result.status === "pending") {
        removalSucceededRef.current = true;
        setRemoveOpen(false);
        onRemoved(item, "pending");
        return;
      }
      if (result.status !== "success") {
        setRemoveError(result.reason);
        return;
      }
      removalSucceededRef.current = true;
      setRemoveOpen(false);
      onRemoved(item, "removed");
    } catch {
      setRemoveError("The local imported copy could not be removed. Check its current status, then try again.");
    } finally {
      removalInFlightRef.current = false;
      setRemoving(false);
    }
  };

  return (
    <aside
      className="self-start overflow-hidden rounded-lg border border-border bg-card"
      aria-labelledby="artifact-inspector-title"
    >
      <header className="flex min-h-16 items-center justify-between gap-3 border-b border-border p-3">
        <div className="min-w-0">
          <span className="font-mono text-label font-semibold tracking-[.06em] text-subtle-foreground uppercase">
            {item.type}
          </span>
          <h2 id="artifact-inspector-title" className="m-0 mt-1 truncate text-title-sm font-semibold text-foreground" title={item.name}>
            {item.name}
          </h2>
        </div>
        <Button type="button" variant="ghost" size="icon" aria-label="Close preview" onClick={onClose}>
          <X size={18} aria-hidden="true" />
        </Button>
      </header>

      <div className="grid h-44 place-items-center border-b border-border bg-muted text-muted-foreground">
        <AssetIcon size={52} aria-hidden="true" />
      </div>

      <dl className="m-0 divide-y divide-border px-3">
        <MetadataRow label="Project" value={item.projectName} />
        <MetadataRow label="Size" value={item.size} mono />
        <MetadataRow
          label={item.contentVersion === undefined ? "Metadata revision" : "Content version"}
          value={item.contentVersion === undefined ? `r${item.metadataRevision}` : `v${item.contentVersion}`}
          mono
        />
        <MetadataRow label="Updated" value={item.updatedAt} mono />
      </dl>

      <footer className="flex flex-wrap gap-2 border-t border-border p-3">
        <Button
          className="flex-1"
          disabled={Boolean(unavailableReason) || opening}
          title={unavailableReason ?? (opening ? "Opening is in progress." : undefined)}
          onClick={() => void openArtifact()}
        >
          {opening && <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />}
          {opening ? "Opening…" : "Open"}
        </Button>
        <Button className="flex-1" variant="outline" disabled title="File export requires daemon protocol support">
          <ArrowDownToLine size={15} aria-hidden="true" /> Export unavailable
        </Button>
        <Dialog
          open={removeOpen}
          onOpenChange={(open) => {
            if (removalInFlightRef.current) return;
            removalSucceededRef.current = false;
            setRemoveError("");
            setRemoveOpen(open);
          }}
        >
          <DialogTrigger asChild>
            <Button
              className="basis-full"
              type="button"
              variant="destructive"
              disabled={Boolean(removeUnavailableReason)}
              title={removeUnavailableReason}
            >
              <Trash2 size={15} aria-hidden="true" /> Remove local copy
            </Button>
          </DialogTrigger>
          <DialogContent
            role="alertdialog"
            aria-describedby="artifact-remove-description"
            showCloseButton={false}
            onEscapeKeyDown={(event) => {
              if (removalInFlightRef.current) event.preventDefault();
            }}
            onOpenAutoFocus={(event) => {
              event.preventDefault();
              cancelRemovalRef.current?.focus();
            }}
            onCloseAutoFocus={(event) => {
              if (removalSucceededRef.current) event.preventDefault();
            }}
          >
            <DialogHeader>
              <DialogTitle>Remove {item.name} from Grok Desktop?</DialogTitle>
              <DialogDescription id="artifact-remove-description">
                This permanently removes Grok Desktop&apos;s local imported copy of content version {item.contentVersion} ({item.size}). The original file you selected remains untouched. This cannot be undone in Grok Desktop. Files already opened in another app may remain available there.
              </DialogDescription>
            </DialogHeader>
            {removing && (
              <p className="m-0 flex items-center gap-2 text-body-sm text-muted-foreground" role="status">
                <LoaderCircle className="animate-spin" size={15} aria-hidden="true" /> Removing local copy…
              </p>
            )}
            {removeError && <p className="m-0 text-body-sm text-destructive" role="alert">{removeError}</p>}
            <DialogFooter>
              <Button
                ref={cancelRemovalRef}
                type="button"
                variant="outline"
                disabled={removing}
                onClick={() => setRemoveOpen(false)}
              >
                Cancel
              </Button>
              <Button
                type="button"
                variant="destructive-solid"
                disabled={removing}
                onClick={() => void removeArtifact()}
              >
                {removing && <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />}
                {removing ? "Removing…" : "Remove local copy"}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </footer>
      {openStatus && (
        <p
          className={cn(
            "m-0 border-t border-border px-3 py-2 text-body-sm",
            openStatus.kind === "success"
              ? "text-success"
              : openStatus.kind === "review"
                ? "text-warning"
                : "text-destructive",
          )}
          role={openStatus.kind === "failed" ? "alert" : "status"}
          aria-live={openStatus.kind === "failed" ? "assertive" : "polite"}
        >
          {openStatus.message}
        </p>
      )}
    </aside>
  );
}

function artifactOpenFailureMessage(
  contentVersion: number,
  failureCode: Extract<ArtifactOpenResult, { status: "failed" }>["failureCode"],
): string {
  switch (failureCode) {
    case "content_unavailable":
      return `Content version ${contentVersion} is missing from local storage. Import the file again to create a verified copy.`;
    case "platform_unavailable":
      return `The desktop open service is unavailable. Check your desktop session, then try content version ${contentVersion} again.`;
    case "deadline_exceeded":
      return `Opening content version ${contentVersion} timed out before desktop dispatch. Try again.`;
    case "integrity_failure":
      return `Content version ${contentVersion} failed its local integrity check and was not opened. Import the file again to create a verified copy.`;
    case "interrupted_before_dispatch":
      return `Opening content version ${contentVersion} stopped before desktop dispatch. Try again.`;
  }
}

function MetadataRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex min-h-9 items-center justify-between gap-3 py-2">
      <dt className="text-body-sm text-muted-foreground">{label}</dt>
      <dd className={cn("m-0 min-w-0 truncate text-right text-body-sm text-foreground", mono && "font-mono tabular-nums")} title={value}>
        {value}
      </dd>
    </div>
  );
}

function MediaLibrary({ kind }: { kind: "image" | "video" }) {
  const client = useDesktopClient();
  const [result, setResult] = useState<ClientResult<MediaCreation[]> | null>(null);
  const [prompt, setPrompt] = useState("");
  const [aspectRatio, setAspectRatio] = useState("16:9");
  const [duration, setDuration] = useState("6s");
  const [creating, setCreating] = useState(false);
  const [cancellingId, setCancellingId] = useState<string | null>(null);
  const [selected, setSelected] = useState<MediaCreation | null>(null);
  const selectedTriggerRef = useRef<HTMLButtonElement | null>(null);
  const acceptsMediaEventsRef = useRef(false);

  const load = async () => {
    try {
      const next = await client.listMediaCreations(kind);
      acceptsMediaEventsRef.current = next.status === "success";
      setResult(next);
    } catch (error: unknown) {
      acceptsMediaEventsRef.current = false;
      setResult({
        status: "unavailable",
        reason: error instanceof Error ? error.message : "Media status could not be loaded.",
      });
    }
  };

  useEffect(() => {
    let active = true;
    let initialSettled = false;
    let pendingCreations: MediaCreation[] | null = null;

    acceptsMediaEventsRef.current = false;
    const unsubscribe = client.subscribeMediaCreations(kind, (creations) => {
      if (!active) return;
      if (acceptsMediaEventsRef.current) {
        setResult({ status: "success", value: creations });
      } else if (!initialSettled) {
        pendingCreations = creations;
      }
    });

    void client
      .listMediaCreations(kind)
      .then((next) => {
        if (!active) return;
        initialSettled = true;
        acceptsMediaEventsRef.current = next.status === "success";
        setResult(
          next.status === "success" && pendingCreations
            ? { status: "success", value: pendingCreations }
            : next,
        );
        pendingCreations = null;
      })
      .catch((error: unknown) => {
        if (active) {
          initialSettled = true;
          acceptsMediaEventsRef.current = false;
          pendingCreations = null;
          setResult({
            status: "unavailable",
            reason: error instanceof Error ? error.message : "Media status could not be loaded.",
          });
        }
      });
    return () => {
      active = false;
      acceptsMediaEventsRef.current = false;
      unsubscribe();
    };
  }, [client, kind]);

  const creations = result?.status === "success" ? result.value : [];
  const active = creations.filter((item) => item.status === "queued" || item.status === "generating");
  const history = creations.filter((item) => item.status !== "queued" && item.status !== "generating");

  const create = async () => {
    const nextPrompt = prompt.trim();
    if (!nextPrompt || creating) return;

    setCreating(true);
    try {
      const next = await client.createMedia({
        kind,
        prompt: nextPrompt,
        aspectRatio,
        duration: kind === "video" ? duration : undefined,
      });
      if (next.status === "success") {
        setPrompt("");
        await load();
      } else {
        acceptsMediaEventsRef.current = false;
        setResult(next);
      }
    } catch (error: unknown) {
      acceptsMediaEventsRef.current = false;
      setResult({
        status: "unavailable",
        reason: error instanceof Error ? error.message : "The creation request could not be submitted.",
      });
    } finally {
      setCreating(false);
    }
  };

  const cancel = async (creationId: string) => {
    setCancellingId(creationId);
    try {
      const next = await client.cancelMedia(creationId);
      if (next.status === "success") await load();
      else {
        acceptsMediaEventsRef.current = false;
        setResult(next);
      }
    } catch (error: unknown) {
      acceptsMediaEventsRef.current = false;
      setResult({
        status: "unavailable",
        reason: error instanceof Error ? error.message : "The creation could not be cancelled.",
      });
    } finally {
      setCancellingId(null);
    }
  };

  const closeProvenance = () => {
    setSelected(null);
    queueMicrotask(() => selectedTriggerRef.current?.focus());
  };

  if (!result) return <MediaLibrarySkeleton />;

  if (result.status !== "success") {
    return (
      <CapabilityRequired
        title={`${kind === "image" ? "Image" : "Video"} creation unavailable`}
        reason={result.reason}
        showSetup={result.status === "configuration_required"}
      />
    );
  }

  return (
    <div>
      <section className="rounded-lg border border-border bg-card p-4" aria-labelledby={`${kind}-create-title`}>
        <div className="mb-4 flex items-center gap-3">
          <span className="grid size-[34px] shrink-0 place-items-center rounded-md bg-accent text-foreground">
            <Sparkles size={18} aria-hidden="true" />
          </span>
          <div>
            <h2 id={`${kind}-create-title`} className="m-0 text-title-sm font-semibold text-foreground">Create with Grok Imagine</h2>
            <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">
              Create {kind === "image" ? "images" : "videos"} through the daemon with a user-owned xAI API key.
            </p>
          </div>
        </div>

        <label className="mb-1.5 block text-body font-semibold text-foreground" htmlFor="media-prompt">Prompt</label>
        <Textarea
          id="media-prompt"
          rows={3}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          placeholder={kind === "image" ? "Describe the image to create" : "Describe the video and motion"}
          className="resize-y"
        />

        <div className="mt-3 grid grid-cols-[8rem_7rem_minmax(0,1fr)_auto] items-end gap-2 max-[680px]:grid-cols-2">
          <label className="flex flex-col gap-1 text-body-sm font-medium text-muted-foreground">
            Aspect ratio
            <select
              className="h-[34px] rounded-md border border-input bg-card px-2 text-body text-foreground outline-none transition-[border-color,box-shadow] duration-150 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring"
              value={aspectRatio}
              onChange={(event) => setAspectRatio(event.target.value)}
            >
              <option>16:9</option>
              <option>1:1</option>
              <option>9:16</option>
              <option>4:3</option>
            </select>
          </label>

          {kind === "video" ? (
            <label className="flex flex-col gap-1 text-body-sm font-medium text-muted-foreground">
              Duration
              <select
                className="h-[34px] rounded-md border border-input bg-card px-2 text-body text-foreground outline-none transition-[border-color,box-shadow] duration-150 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring"
                value={duration}
                onChange={(event) => setDuration(event.target.value)}
              >
                <option>6s</option>
                <option>8s</option>
                <option>10s</option>
              </select>
            </label>
          ) : (
            <span aria-hidden="true" />
          )}

          <span aria-hidden="true" className="max-[680px]:hidden" />
          <Button
            className="max-[680px]:col-span-2"
            disabled={!prompt.trim() || creating}
            onClick={() => void create()}
          >
            {creating ? <LoaderCircle className="animate-spin" size={15} aria-hidden="true" /> : <Plus size={15} aria-hidden="true" />}
            {creating ? `Creating ${kind}` : `Create ${kind}`}
          </Button>
        </div>
      </section>

      {active.length > 0 && (
        <section className="mt-6" aria-labelledby={`${kind}-queue-title`}>
          <SectionHeading
            id={`${kind}-queue-title`}
            title="Queue"
            description="The desktop daemon continues reported creations after this window closes"
            meta={<Badge variant="info">{active.length} active</Badge>}
          />
          <div className="overflow-hidden rounded-lg border border-border bg-card">
            {active.map((item) => {
              const status = MEDIA_STATUS[item.status];
              const progress = Math.max(0, Math.min(100, item.progress));
              return (
                <article className="grid min-h-16 grid-cols-[44px_minmax(0,1fr)_auto] items-center gap-3 border-b border-border p-2 last:border-b-0" key={item.id}>
                  <span className="grid size-11 place-items-center rounded-md bg-accent text-foreground">
                    {kind === "image" ? <Image size={22} aria-hidden="true" /> : <Film size={22} aria-hidden="true" />}
                  </span>
                  <div className="min-w-0">
                    <div className="flex min-w-0 items-center gap-2">
                      <strong className="truncate text-body font-semibold text-foreground" title={item.prompt}>{item.prompt}</strong>
                      <Badge variant={status.variant}>{status.label}</Badge>
                    </div>
                    <span className="mt-0.5 block text-body-sm text-muted-foreground">
                      {item.status === "queued" ? "Waiting for capacity" : `${progress}% complete`}
                    </span>
                    <div
                      className="mt-1.5 h-1 overflow-hidden rounded-full bg-secondary"
                      role="progressbar"
                      aria-label={`Progress for ${item.prompt}`}
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
                  <Button
                    variant="ghost"
                    disabled={cancellingId === item.id}
                    onClick={() => void cancel(item.id)}
                  >
                    {cancellingId === item.id && <LoaderCircle className="animate-spin" size={15} aria-hidden="true" />}
                    {cancellingId === item.id ? "Cancelling" : "Cancel"}
                  </Button>
                </article>
              );
            })}
          </div>
        </section>
      )}

      <section className="mt-6" aria-labelledby={`${kind}-history-title`}>
        <SectionHeading
          id={`${kind}-history-title`}
          title="History"
          description="Daemon-reported creations with provenance metadata"
        />
        {history.length > 0 ? (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(205px,1fr))] gap-3">
            {history.map((item) => (
              <MediaHistoryCard
                key={item.id}
                item={item}
                onSelect={(target) => {
                  selectedTriggerRef.current = target;
                  setSelected(item);
                }}
              />
            ))}
          </div>
        ) : (
          <div className="flex min-h-48 flex-col items-center justify-center rounded-lg border border-border bg-card px-6 py-10 text-center">
            <span className="grid size-11 place-items-center rounded-lg bg-muted text-muted-foreground">
              {kind === "image" ? <Image size={22} aria-hidden="true" /> : <Film size={22} aria-hidden="true" />}
            </span>
            <h3 className="m-0 mt-3 text-title-sm font-semibold text-foreground">No creation history</h3>
            <p className="m-0 mt-1 text-body-sm text-muted-foreground">Completed, failed, and cancelled creations will appear here.</p>
          </div>
        )}
      </section>

      {selected && <CreationProvenance creation={selected} onClose={closeProvenance} />}
    </div>
  );
}

function MediaLibrarySkeleton() {
  return (
    <div role="status" aria-label="Checking media availability">
      <div className="rounded-lg border border-border bg-card p-4">
        <div className="flex items-center gap-3">
          <Skeleton className="size-[34px]" />
          <div className="flex flex-1 flex-col gap-2">
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-3 w-80 max-w-full" />
          </div>
        </div>
        <Skeleton className="mt-4 h-20 w-full" />
        <Skeleton className="mt-3 ml-auto h-[34px] w-28" />
      </div>
      <div className="mt-6 grid grid-cols-[repeat(auto-fill,minmax(205px,1fr))] gap-3">
        {Array.from({ length: 4 }).map((_, index) => (
          <div className="overflow-hidden rounded-lg border border-border bg-card" key={index}>
            <Skeleton className="h-32 rounded-none" />
            <div className="flex flex-col gap-2 p-3">
              <Skeleton className="h-3 w-4/5" />
              <Skeleton className="h-3 w-3/5" />
            </div>
          </div>
        ))}
      </div>
      <span className="sr-only">Checking media availability</span>
    </div>
  );
}

function SectionHeading({
  id,
  title,
  description,
  meta,
}: {
  id: string;
  title: string;
  description: string;
  meta?: ReactNode;
}) {
  return (
    <div className="mb-3 flex items-end justify-between gap-4">
      <div>
        <h2 id={id} className="m-0 text-title font-semibold text-foreground">{title}</h2>
        <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">{description}</p>
      </div>
      {meta}
    </div>
  );
}

function MediaHistoryCard({ item, onSelect }: { item: MediaCreation; onSelect(target: HTMLButtonElement): void }) {
  const status = MEDIA_STATUS[item.status];

  return (
    <article className="relative overflow-hidden rounded-lg border border-border bg-card transition-[border-color,box-shadow] duration-150 hover:border-input-hover hover:shadow-raised">
      <button
        type="button"
        className="w-full text-left text-foreground outline-none transition-transform duration-150 ease-fluid active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-inset focus-visible:ring-ring"
        onClick={(event) => onSelect(event.currentTarget)}
      >
        <span className="grid h-32 place-items-center border-b border-border bg-accent text-foreground">
          {item.status === "failed" ? (
            <CircleAlert size={28} aria-hidden="true" />
          ) : item.status === "cancelled" ? (
            <X size={28} aria-hidden="true" />
          ) : item.kind === "video" ? (
            <Film size={28} aria-hidden="true" />
          ) : (
            <Image size={28} aria-hidden="true" />
          )}
        </span>
        <span className="flex min-w-0 flex-col gap-2 p-3">
          <span className="flex items-start justify-between gap-2">
            <strong className="line-clamp-2 text-body font-semibold text-foreground" title={item.prompt}>{item.prompt}</strong>
            <Badge variant={status.variant}>{status.label}</Badge>
          </span>
          <span className="font-mono text-label text-subtle-foreground tabular-nums">
            {item.aspectRatio}{item.duration ? ` · ${item.duration}` : ""} · {item.createdAt}
          </span>
        </span>
      </button>
      {item.provenance.createdWithGrok && (
        <Badge className="pointer-events-none absolute top-2 left-2 shadow-raised" variant="success">
          <ShieldCheck size={12} aria-hidden="true" /> Created with Grok
        </Badge>
      )}
    </article>
  );
}

function CreationProvenance({ creation, onClose }: { creation: MediaCreation; onClose(): void }) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="max-h-[calc(100vh-32px)] max-w-[520px] overflow-y-auto" showCloseButton>
        <DialogHeader>
          <span className="font-mono text-label font-semibold tracking-[.06em] text-subtle-foreground uppercase">Provenance</span>
          <DialogTitle>{creation.kind === "image" ? "Image details" : "Video details"}</DialogTitle>
          <DialogDescription>Daemon-reported creation status and attribution metadata.</DialogDescription>
        </DialogHeader>

        <div className="grid h-48 place-items-center rounded-lg border border-border bg-accent text-foreground">
          {creation.kind === "image" ? <Image size={42} aria-hidden="true" /> : <Film size={42} aria-hidden="true" />}
        </div>

        <dl className="m-0 divide-y divide-border rounded-lg border border-border px-3">
          <MetadataRow label="Generator" value={creation.provenance.generator} />
          <MetadataRow label="Attribution" value={creation.provenance.createdWithGrok ? "Created with Grok" : "Not recorded"} />
          <MetadataRow label="Watermark" value={creation.provenance.watermark ? "Preserved" : "Not present"} />
          <MetadataRow label="Created" value={creation.createdAt} mono />
        </dl>

        <p className="m-0 flex gap-2 rounded-lg border border-success/20 bg-success-soft p-3 text-body-sm text-success">
          <Info className="mt-0.5 shrink-0" size={15} aria-hidden="true" />
          Available watermark and provenance metadata are recorded by the daemon.
        </p>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>Close</Button>
          <Button disabled title="Media export requires daemon protocol support">
            <ArrowDownToLine size={15} aria-hidden="true" /> Export unavailable
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function CapabilityRequired({ title, reason, showSetup }: { title: string; reason: string; showSetup: boolean }) {
  return (
    <section className="flex min-h-96 flex-col items-center justify-center rounded-lg border border-border bg-card px-6 py-12 text-center">
      <span className="grid size-12 place-items-center rounded-lg bg-warning-soft text-warning">
        <ShieldCheck size={23} aria-hidden="true" />
      </span>
      <h2 className="m-0 mt-3 text-title font-semibold text-foreground">{title}</h2>
      <p className="m-0 mt-1 max-w-[65ch] text-body text-muted-foreground">{reason}</p>
      {showSetup && (
        <Button className="mt-4" onClick={() => window.location.assign("#/setup")}>Open setup</Button>
      )}
    </section>
  );
}
