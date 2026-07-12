import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import {
  Activity,
  AlertTriangle,
  Blocks,
  Clock3,
  FolderKanban,
  Library,
  LoaderCircle,
  MessageSquareText,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Search,
  Settings,
  ShieldCheck,
  Sparkles,
  X,
} from "lucide-react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { DesktopNavigationRoute } from "../contracts/bridge";
import type { WorkspaceSearchHit } from "../services/desktopClient";
import { IconButton } from "./ui";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

const navigation = [
  { to: "/", label: "Home", icon: MessageSquareText },
  { to: "/projects", label: "Projects", icon: FolderKanban },
  { to: "/activity", label: "Activity", icon: Activity },
  { to: "/library", label: "Library", icon: Library },
  { to: "/automations", label: "Definitions", icon: Clock3 },
  // Extensions/Wisp install lifecycle is not a shipped product surface yet.
];

/* Sidebar text collapses at the rail breakpoint or when the user collapses the shell. */
const railHidden = (collapsed: boolean) => cn("max-[1120px]:hidden", collapsed && "hidden");

const navItemClass = (isActive: boolean, collapsed: boolean) =>
  cn(
    "relative flex h-[38px] min-w-0 items-center gap-2.5 rounded-md px-2.5 text-body font-medium whitespace-nowrap text-muted-foreground transition-colors duration-150",
    "hover:bg-sidebar-accent/55 hover:text-foreground",
    isActive && "bg-sidebar-accent font-semibold text-foreground shadow-raised",
    "max-[1120px]:mx-auto max-[1120px]:w-10 max-[1120px]:justify-center max-[1120px]:px-0",
    collapsed && "mx-auto w-10 justify-center px-0",
    "max-[680px]:m-0 max-[680px]:h-11 max-[680px]:w-[42px]",
  );

export function rendererPathForDesktopNavigationRoute(route: DesktopNavigationRoute): string {
  if (route.route === "project") return `/projects/${route.projectId}`;
  if (route.route === "conversation") return `/conversations/${route.threadId}`;
  if (route.route === "home") return "/";
  return `/${route.route}`;
}

export function AppShell() {
  const [collapsed, setCollapsed] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchTriggerRef = useRef<HTMLButtonElement>(null);
  const { snapshot } = useDesktopSnapshot();
  const location = useLocation();
  const navigate = useNavigate();

  useEffect(() => setSearchOpen(false), [location.pathname]);
  useEffect(() => window.grokDesktop?.onNavigationRoute((route) => {
    navigate(rendererPathForDesktopNavigationRoute(route));
  }), [navigate]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const needsInput = snapshot?.runs.filter((run) =>
    run.state === "awaiting_approval" || run.state === "interrupted_needs_review"
  ).length ?? 0;
  const interfacePreview = snapshot?.connection.interfacePreview === true;
  const daemonConnected = snapshot?.connection.state === "online" && !interfacePreview;
  const daemonConnecting = snapshot?.connection.state === "connecting";
  const connectionState = snapshot?.connection.state ?? "connecting";

  return (
    <div
      className={cn(
        "grid h-screen grid-cols-[250px_minmax(0,1fr)] transition-[grid-template-columns] duration-200 ease-fluid",
        collapsed && "grid-cols-[72px_minmax(0,1fr)]",
        "max-[1120px]:grid-cols-[72px_minmax(0,1fr)] max-[680px]:block max-[680px]:h-auto",
      )}
    >
      <a
        className="fixed top-2 left-3 z-1000 -translate-y-[150%] rounded-lg bg-primary px-3 py-2 text-body text-primary-foreground focus:translate-y-0"
        href="#main-content"
      >
        Skip to content
      </a>
      <aside
        className={cn(
          "flex min-w-0 flex-col gap-2 overflow-hidden border-r border-sidebar-border bg-sidebar px-2.5 py-3",
          "max-[680px]:fixed max-[680px]:inset-x-0 max-[680px]:top-auto max-[680px]:bottom-0 max-[680px]:z-20 max-[680px]:h-[60px] max-[680px]:flex-row max-[680px]:items-center max-[680px]:gap-0.5 max-[680px]:border-t max-[680px]:border-r-0 max-[680px]:p-2",
        )}
        aria-label="Sidebar"
      >
        <div
          className={cn(
            "flex h-[42px] items-center justify-between gap-2 pr-0.5 pl-1",
            "max-[1120px]:h-[74px] max-[1120px]:flex-wrap max-[1120px]:justify-center",
            collapsed && "h-[74px] flex-wrap justify-center",
            "max-[680px]:hidden",
          )}
        >
          <NavLink to="/" className="flex min-w-0 items-center gap-2.5" aria-label="Grok Desktop home">
            {/* Plex Mono ships 400/500/600 only and font-synthesis is off — 600 is mono's boldest. */}
            <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-primary font-mono text-body font-semibold text-primary-foreground">
              G
            </span>
            <span className={cn("truncate text-body-lg font-semibold", railHidden(collapsed))}>Grok Desktop</span>
          </NavLink>
          <IconButton
            label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            onClick={() => setCollapsed((value) => !value)}
          >
            {collapsed ? <PanelLeftOpen size={17} /> : <PanelLeftClose size={17} />}
          </IconButton>
        </div>

        <Button
          className={cn(
            "mx-0.5 mt-1 mb-1 h-[38px] gap-2 rounded-lg",
            "max-[1120px]:mx-auto max-[1120px]:w-10 max-[1120px]:px-0",
            collapsed && "mx-auto w-10 px-0",
            "max-[680px]:hidden",
          )}
          aria-label="New conversation"
          title="New conversation"
          onClick={() => window.location.assign("#/")}
        >
          <Plus size={17} aria-hidden="true" />
          <span className={railHidden(collapsed)}>New conversation</span>
        </Button>

        <nav
          className="flex flex-col gap-0.5 max-[680px]:min-w-0 max-[680px]:flex-1 max-[680px]:flex-row max-[680px]:justify-around"
          aria-label="Primary navigation"
        >
          {navigation.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === "/"}
              aria-label={
                item.label === "Activity" && needsInput > 0 ? `${item.label}, ${needsInput} items need input` : item.label
              }
              className={({ isActive }) => navItemClass(isActive, collapsed)}
            >
              <item.icon size={18} aria-hidden="true" />
              <span className={cn(railHidden(collapsed), "max-[680px]:hidden")}>{item.label}</span>
              {item.label === "Activity" && needsInput > 0 && (
                <span
                  className={cn(
                    "ml-auto grid h-[19px] min-w-[19px] place-items-center rounded-full bg-warning px-1 font-mono text-label font-semibold text-white tabular-nums",
                    "max-[1120px]:absolute max-[1120px]:-top-px max-[1120px]:-right-0.5 max-[1120px]:ml-0",
                    collapsed && "absolute -top-px -right-0.5 ml-0",
                  )}
                  aria-label={`${needsInput} items need input`}
                >
                  {needsInput}
                </span>
              )}
            </NavLink>
          ))}
        </nav>

        <div
          className={cn(
            "flex min-h-0 flex-col gap-0.5 overflow-y-auto pt-3",
            railHidden(collapsed),
            "max-[1120px]:hidden max-[680px]:hidden",
          )}
        >
          <div className="px-2.5 pb-2 font-mono text-label font-semibold tracking-[0.06em] text-muted-foreground uppercase">
            Projects
          </div>
          {snapshot?.projects.slice(0, 3).map((project) => (
            <NavLink
              to={`/projects/${project.id}`}
              className="flex h-[34px] min-w-0 items-center gap-2 rounded-md px-2.5 text-body-sm whitespace-nowrap text-muted-foreground hover:bg-sidebar-accent/55 hover:text-foreground"
              key={project.id}
            >
              <span className="size-2 shrink-0 rounded-[2px]" style={{ background: project.accent }} />
              <span className="truncate">{project.name}</span>
              {project.activeRunCount > 0 && (
                <span className="ml-auto font-mono text-label text-muted-foreground tabular-nums">
                  {project.activeRunCount}
                </span>
              )}
            </NavLink>
          ))}
        </div>

        <div className="mt-auto flex flex-col gap-1.5 border-t border-sidebar-border pt-2 max-[680px]:m-0 max-[680px]:border-t-0 max-[680px]:p-0">
          <NavLink to="/settings" aria-label="Settings" className={({ isActive }) => navItemClass(isActive, collapsed)}>
            <Settings size={18} aria-hidden="true" />
            <span className={cn(railHidden(collapsed), "max-[680px]:hidden")}>Settings</span>
          </NavLink>
          <div
            className={cn(
              "flex h-12 min-w-0 items-center gap-2 px-2 py-1",
              "max-[1120px]:justify-center max-[1120px]:px-0",
              collapsed && "justify-center px-0",
              "max-[680px]:hidden",
            )}
          >
            <span className="grid size-[29px] shrink-0 place-items-center rounded-full bg-card font-mono text-label font-semibold text-muted-foreground">
              GD
            </span>
            <span className={cn("flex min-w-0 flex-col", railHidden(collapsed))}>
              <strong className="text-label font-semibold">{interfacePreview ? "Interface preview" : "Local workspace"}</strong>
              <small className="text-label text-muted-foreground">
                {interfacePreview
                  ? "No provider execution"
                  : snapshot?.connection.state === "online"
                    ? "Local daemon connected"
                    : "Limited mode"}
              </small>
            </span>
            <span
              className={cn(
                "ml-auto size-[7px] shrink-0 rounded-full ring-[3px]",
                railHidden(collapsed),
                interfacePreview
                  ? "bg-warning ring-warning-soft"
                  : connectionState === "online"
                    ? "bg-success ring-success-soft"
                    : connectionState === "degraded"
                      ? "bg-warning ring-warning-soft"
                      : connectionState === "connecting"
                        ? "bg-info ring-info-soft"
                        : "bg-input ring-muted",
              )}
              aria-hidden="true"
            />
          </div>
        </div>
      </aside>

      <div className="grid min-h-0 min-w-0 grid-rows-[54px_minmax(0,1fr)] bg-background max-[680px]:min-h-screen max-[680px]:grid-rows-[50px_minmax(0,1fr)] max-[680px]:pb-[60px]">
        <header className="flex items-center gap-3 border-b border-border bg-background/95 px-[18px] max-[680px]:px-2.5">
          <button
            ref={searchTriggerRef}
            className="flex h-[34px] w-[min(390px,42vw)] items-center gap-2 rounded-lg border border-input bg-card px-3 text-left text-body-sm text-subtle-foreground shadow-raised transition-colors hover:border-input-hover hover:text-muted-foreground max-[680px]:w-full"
            onClick={() => setSearchOpen(true)}
          >
            <Search size={16} aria-hidden="true" />
            <span className="truncate">Search conversations, projects, and files</span>
            <kbd className="ml-auto rounded-sm border border-border bg-muted px-1.5 font-mono text-label text-subtle-foreground max-[900px]:hidden">
              Ctrl K
            </kbd>
          </button>
          <div className="ml-auto flex items-center gap-2 max-[680px]:hidden">
            <span
              className={cn(
                "inline-flex h-7 items-center gap-1.5 font-mono text-label font-medium max-[900px]:hidden",
                daemonConnected ? "text-success" : "text-warning",
              )}
              title={snapshot?.connection.reason}
            >
              {interfacePreview ? (
                <AlertTriangle size={15} />
              ) : daemonConnecting ? (
                <LoaderCircle size={15} className="animate-spin" />
              ) : daemonConnected ? (
                <ShieldCheck size={15} />
              ) : (
                <AlertTriangle size={15} />
              )}
              {interfacePreview
                ? "Interface preview"
                : daemonConnecting
                  ? "Starting daemon"
                  : daemonConnected
                    ? "Daemon connected"
                    : "Limited mode"}
            </span>
          </div>
        </header>
        <main id="main-content" tabIndex={-1} className="min-h-0 min-w-0 scroll-smooth overflow-auto">
          {snapshot && interfacePreview && (
            <div
              className="mx-[clamp(24px,3.2vw,48px)] mt-3 -mb-4 flex min-h-[42px] items-center gap-2 rounded-lg border border-warning/30 bg-warning-soft px-3 py-2 text-warning max-[680px]:mx-3.5"
              role="status"
            >
              <AlertTriangle size={16} />
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <strong className="text-label">Interface preview</strong>
                <small className="truncate text-label text-muted-foreground max-[680px]:whitespace-normal">
                  Sample data only. No Grok provider request or local tool execution is available.
                </small>
              </span>
            </div>
          )}
          {snapshot && !daemonConnected && !interfacePreview && (
            <div
              className={cn(
                "mx-[clamp(24px,3.2vw,48px)] mt-3 -mb-4 flex min-h-[42px] items-center gap-2 rounded-lg border px-3 py-2 max-[680px]:mx-3.5",
                daemonConnecting ? "border-info/25 bg-info-soft text-info" : "border-warning/30 bg-warning-soft text-warning",
              )}
              role="status"
            >
              {daemonConnecting ? <LoaderCircle size={16} className="animate-spin" /> : <AlertTriangle size={16} />}
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <strong className="text-label">{daemonConnecting ? "Starting local daemon" : "Limited Mode"}</strong>
                {snapshot.connection.reason && (
                  <small className="truncate text-label text-muted-foreground max-[680px]:whitespace-normal">
                    {snapshot.connection.reason}
                  </small>
                )}
              </span>
              {!daemonConnecting && (
                <NavLink to="/settings" className="text-label font-semibold underline-offset-2 hover:underline">
                  Open settings
                </NavLink>
              )}
            </div>
          )}
          <Outlet />
        </main>
      </div>
      {searchOpen && (
        <SearchDialog
          onClose={() => setSearchOpen(false)}
          restoreFocusRef={searchTriggerRef}
        />
      )}
    </div>
  );
}

function SearchDialog({
  onClose,
  restoreFocusRef,
}: {
  onClose(): void;
  restoreFocusRef: RefObject<HTMLButtonElement | null>;
}) {
  const navigate = useNavigate();
  const client = useDesktopClient();
  const { snapshot } = useDesktopSnapshot();
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<{ query: string; hits: WorkspaceSearchHit[]; hasMore: boolean } | null>(null);
  const [searchingQuery, setSearchingQuery] = useState<string | null>(null);
  const [searchError, setSearchError] = useState<{ query: string; message: string } | null>(null);
  const [retry, setRetry] = useState(0);
  const normalizedQuery = query.trim();
  const queryByteLength = new TextEncoder().encode(normalizedQuery).byteLength;
  const queryValidationError = queryByteLength > 256
    ? "Workspace search is limited to 256 UTF-8 bytes. Shorten the query to continue."
    : "";
  const matchingSearchResults = searchResults?.query === normalizedQuery ? searchResults : null;
  const matchingSearchError = searchError?.query === normalizedQuery ? searchError.message : "";
  const searching = searchingQuery === normalizedQuery && normalizedQuery.length > 0 && !queryValidationError;

  const recent = useMemo(() => {
    if (!snapshot) return [];
    return [
      ...snapshot.threads.map((thread) => ({ id: thread.id, kind: "thread" as const, title: thread.title, snippet: "", meta: thread.projectName, to: `/conversations/${thread.id}` })),
      ...snapshot.projects.map((project) => ({ id: project.id, kind: "project" as const, title: project.name, snippet: project.description, meta: "Project", to: `/projects/${project.id}` })),
      ...snapshot.library.map((item) => ({ id: item.id, kind: "artifact" as const, title: item.name, snippet: "", meta: item.projectName, to: "/library" })),
    ];
  }, [snapshot]);

  useEffect(() => {
    if (!normalizedQuery) {
      setSearchResults(null);
      setSearchError(null);
      setSearchingQuery(null);
      return;
    }
    if (queryValidationError) {
      setSearchResults(null);
      setSearchError(null);
      setSearchingQuery(null);
      return;
    }
    let active = true;
    setSearchingQuery(normalizedQuery);
    setSearchError(null);
    setSearchResults(null);
    const timeout = window.setTimeout(() => {
      void client.searchWorkspace({ query: normalizedQuery, limit: 8 })
        .then((results) => {
          if (active) setSearchResults({ query: normalizedQuery, hits: results.hits, hasMore: results.hasMore });
        })
        .catch(() => {
          if (active) {
            setSearchResults(null);
            setSearchError({
              query: normalizedQuery,
              message: "Workspace search is unavailable. Check the local daemon and try again.",
            });
          }
        })
        .finally(() => {
          if (active) setSearchingQuery(null);
        });
    }, 180);
    return () => {
      active = false;
      window.clearTimeout(timeout);
    };
  }, [client, normalizedQuery, queryValidationError, retry]);

  const results = useMemo(() => {
    if (!normalizedQuery) return recent.slice(0, 6);
    const projectNames = new Map(snapshot?.projects.map((project) => [project.id, project.name]) ?? []);
    return (matchingSearchResults?.hits ?? []).map((hit) => ({
      ...hit,
      meta: searchResultMeta(hit, projectNames.get(hit.projectId)),
      to: searchResultTarget(hit),
    }));
  }, [matchingSearchResults, normalizedQuery, recent, snapshot]);

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          restoreFocusRef.current?.focus();
        }}
        className="gap-0 overflow-hidden p-0"
        aria-label="Search Grok Desktop"
        aria-describedby={undefined}
      >
        <DialogTitle className="sr-only">Search Grok Desktop</DialogTitle>
        <div className="flex h-[55px] items-center gap-2.5 border-b border-border pr-3 pl-4 text-subtle-foreground">
          <Search size={18} aria-hidden="true" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search everything"
            aria-label="Search everything"
            aria-describedby="workspace-search-status"
            aria-invalid={queryValidationError ? "true" : undefined}
            maxLength={256}
            className="min-w-0 flex-1 rounded-md border-0 bg-transparent px-1 py-1 text-body-lg text-foreground outline-none placeholder:text-subtle-foreground focus-visible:ring-[3px] focus-visible:ring-ring"
          />
          <span
            className={cn(
              "shrink-0 font-mono text-label tabular-nums",
              queryValidationError ? "text-destructive" : "text-subtle-foreground",
            )}
            aria-hidden="true"
          >
            {queryByteLength}/256 B
          </span>
          <IconButton label="Close search" onClick={onClose}>
            <X size={17} />
          </IconButton>
        </div>
        <div className="max-h-[370px] overflow-y-auto p-2">
          <div className="px-2 py-1.5 font-mono text-label font-semibold tracking-[0.06em] text-subtle-foreground uppercase">
            {query ? "Results" : "Recent"}
          </div>
          <span id="workspace-search-status" className="sr-only" role="status" aria-live="polite">
            {searching
              ? "Searching the local workspace"
              : queryValidationError
                ? queryValidationError
                : matchingSearchError
                  ? matchingSearchError
                : normalizedQuery
                  ? `${results.length} workspace results`
                  : `${results.length} recent items`}
          </span>
          {searching && <SearchResultsSkeleton />}
          {!searching && queryValidationError && (
            <div className="m-2 rounded-lg border border-destructive/25 bg-destructive-soft p-3" role="alert">
              <p className="m-0 text-body-sm leading-5 text-destructive">{queryValidationError}</p>
            </div>
          )}
          {!searching && !queryValidationError && matchingSearchError && (
            <div className="m-2 rounded-lg border border-destructive/25 bg-destructive-soft p-3" role="alert">
              <p className="m-0 text-body-sm leading-5 text-destructive">{matchingSearchError}</p>
              <Button className="mt-3" type="button" size="sm" variant="outline" onClick={() => setRetry((value) => value + 1)}>
                Retry search
              </Button>
            </div>
          )}
          {!searching && !queryValidationError && !matchingSearchError && results.map((result) => (
            <button
              key={`${result.kind}-${result.id}`}
              type="button"
              onClick={() => {
                navigate(result.to);
                onClose();
              }}
              className="flex min-h-12 w-full cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left outline-none transition-colors hover:bg-muted focus-visible:ring-[3px] focus-visible:ring-ring"
            >
              <Sparkles size={16} aria-hidden="true" />
              <span className="flex min-w-0 flex-col gap-0.5">
                <strong className="truncate text-body font-semibold">{result.title}</strong>
                <small className="text-label text-subtle-foreground">{result.meta}</small>
                {result.snippet && (
                  <span className="line-clamp-2 text-body-sm leading-4 text-muted-foreground [overflow-wrap:anywhere]">
                    {result.snippet}
                  </span>
                )}
              </span>
            </button>
          ))}
          {!searching && !queryValidationError && !matchingSearchError && normalizedQuery && results.length === 0 && (
            <p className="m-0 my-5 px-4 text-center text-body-sm text-subtle-foreground">
              No matching workspace items. Try fewer or different terms.
            </p>
          )}
          {!searching && !queryValidationError && !matchingSearchError && matchingSearchResults?.hasMore && (
            <p className="m-0 px-3 py-2 text-center text-label text-subtle-foreground" role="status">
              More matches are available. Refine the search to narrow the results.
            </p>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function SearchResultsSkeleton() {
  return (
    <div className="flex flex-col gap-1 p-1" role="status" aria-label="Searching workspace">
      {Array.from({ length: 3 }).map((_, index) => (
        <div className="flex min-h-12 items-center gap-2.5 px-2.5 py-1.5" key={index}>
          <Skeleton className="size-4 shrink-0" />
          <span className="flex min-w-0 flex-1 flex-col gap-1.5">
            <Skeleton className="h-3 w-2/3" />
            <Skeleton className="h-2.5 w-1/3" />
          </span>
        </div>
      ))}
    </div>
  );
}

function searchResultTarget(hit: WorkspaceSearchHit): string {
  if ((hit.kind === "thread" || hit.kind === "message") && hit.threadId) {
    return `/conversations/${hit.threadId}`;
  }
  if (hit.kind === "project") return `/projects/${hit.id}`;
  if (hit.kind === "artifact") return "/library";
  return "/automations";
}

function searchResultMeta(hit: WorkspaceSearchHit, projectName?: string): string {
  const label = {
    project: "Project",
    thread: "Conversation",
    message: "Message",
    artifact: "Library item",
    automation: "Automation",
  }[hit.kind];
  return projectName && hit.kind !== "project" ? `${label} · ${projectName}` : label;
}
