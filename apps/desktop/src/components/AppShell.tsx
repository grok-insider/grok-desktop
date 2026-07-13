import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import {
  Activity,
  AlertTriangle,
  Clock3,
  FolderKanban,
  Library,
  LoaderCircle,
  MessageSquareText,
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
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarRail,
  SidebarTrigger,
  sidebarStateFromStorage,
} from "@/components/ui/sidebar";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

const navigation = [
  { to: "/", label: "Home", icon: MessageSquareText },
  { to: "/projects", label: "Projects", icon: FolderKanban },
  { to: "/activity", label: "Activity", icon: Activity },
  { to: "/library", label: "Library", icon: Library },
  { to: "/automations", label: "Definitions", icon: Clock3 },
  // /extensions exists as a daemon-reported honesty surface only; Wisp is not
  // catalogued or installable until signed lifecycle IPC ships.
];

export function rendererPathForDesktopNavigationRoute(route: DesktopNavigationRoute): string {
  if (route.route === "project") return `/projects/${route.projectId}`;
  if (route.route === "conversation") return `/conversations/${route.threadId}`;
  if (route.route === "home") return "/";
  return `/${route.route}`;
}

export function AppShell() {
  const [searchOpen, setSearchOpen] = useState(false);
  const [appVersion, setAppVersion] = useState("");
  const searchTriggerRef = useRef<HTMLButtonElement>(null);
  const { snapshot } = useDesktopSnapshot();
  const location = useLocation();
  const navigate = useNavigate();

  useEffect(() => setSearchOpen(false), [location.pathname]);
  useEffect(() => {
    const bridge = window.grokDesktop;
    if (!bridge) return;
    let active = true;
    void (async () => {
      try {
        const response = await bridge.request({ kind: "runtime.info" });
        if (active && response?.kind === "runtime.info" && typeof response.version === "string") {
          setAppVersion(response.version);
        }
      } catch {
        // The version label is cosmetic; stay silent when the bridge cannot answer.
      }
    })();
    return () => {
      active = false;
    };
  }, []);
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
  const isActiveRoute = (to: string) =>
    to === "/" ? location.pathname === "/" : location.pathname === to || location.pathname.startsWith(`${to}/`);

  /*
   * The routed view is memoized so shell-local state changes (sidebar toggle,
   * search dialog) do not re-render the active view. Router context updates
   * still propagate through the stable element.
   */
  const outlet = useMemo(() => <Outlet />, []);

  return (
    <SidebarProvider defaultOpen={sidebarStateFromStorage() ?? true}>
      <a
        className="fixed top-2 left-3 z-1000 -translate-y-[150%] rounded-lg bg-primary px-3 py-2 text-body text-primary-foreground focus:translate-y-0"
        href="#main-content"
      >
        Skip to content
      </a>
      <Sidebar collapsible="icon">
        <SidebarHeader>
          <NavLink
            to="/"
            className="flex h-[42px] min-w-0 items-center gap-2.5 rounded-md px-1 group-data-[collapsible=icon]:justify-center group-data-[collapsible=icon]:px-0"
            aria-label="Grok Desktop home"
          >
            {/* Plex Mono ships 400/500/600 only and font-synthesis is off — 600 is mono's boldest. */}
            <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-primary font-mono text-body font-semibold text-primary-foreground">
              G
            </span>
            <span className="truncate text-body-lg font-semibold group-data-[collapsible=icon]:hidden">Grok Desktop</span>
          </NavLink>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton
                asChild
                tooltip="New conversation"
                className="h-[38px] justify-center rounded-lg bg-primary text-body font-semibold text-primary-foreground hover:bg-primary-hover hover:text-primary-foreground active:bg-primary-hover active:text-primary-foreground"
              >
                <NavLink to="/" aria-label="New conversation" title="New conversation">
                  <Plus aria-hidden="true" />
                  <span className="group-data-[collapsible=icon]:hidden">New conversation</span>
                </NavLink>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupContent>
              <nav aria-label="Primary navigation">
                <SidebarMenu>
                  {navigation.map((item) => (
                    <SidebarMenuItem key={item.to}>
                      <SidebarMenuButton
                        asChild
                        isActive={isActiveRoute(item.to)}
                        tooltip={item.label}
                        className="h-[38px] text-body font-medium text-muted-foreground data-[active=true]:font-semibold data-[active=true]:shadow-raised"
                      >
                        <NavLink
                          to={item.to}
                          end={item.to === "/"}
                          aria-label={
                            item.label === "Activity" && needsInput > 0
                              ? `${item.label}, ${needsInput} items need input`
                              : item.label
                          }
                        >
                          <item.icon aria-hidden="true" />
                          <span>{item.label}</span>
                        </NavLink>
                      </SidebarMenuButton>
                      {item.label === "Activity" && needsInput > 0 && (
                        <SidebarMenuBadge className="grid h-[19px] min-w-[19px] place-items-center rounded-full bg-warning px-1 font-mono text-label font-semibold text-white tabular-nums">
                          {needsInput}
                        </SidebarMenuBadge>
                      )}
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </nav>
            </SidebarGroupContent>
          </SidebarGroup>
          <SidebarGroup className="group-data-[collapsible=icon]:hidden">
            <SidebarGroupLabel className="font-mono text-label font-semibold tracking-[0.06em] text-muted-foreground uppercase">
              Projects
            </SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {snapshot?.projects.slice(0, 3).map((project) => (
                  <SidebarMenuItem key={project.id}>
                    <SidebarMenuButton asChild className="h-[34px] text-body-sm text-muted-foreground">
                      <NavLink to={`/projects/${project.id}`}>
                        <span
                          className="size-2 shrink-0 rounded-[2px]"
                          style={{ background: project.accent }}
                          aria-hidden="true"
                        />
                        <span className="truncate">{project.name}</span>
                      </NavLink>
                    </SidebarMenuButton>
                    {project.activeRunCount > 0 && (
                      <SidebarMenuBadge className="font-mono text-label text-muted-foreground tabular-nums">
                        {project.activeRunCount}
                      </SidebarMenuBadge>
                    )}
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
        <SidebarFooter className="border-t border-sidebar-border">
          {appVersion && (
            <span
              className="px-2 font-mono text-label text-subtle-foreground group-data-[collapsible=icon]:hidden"
              title={`Grok Desktop ${appVersion}`}
            >
              v{appVersion}
            </span>
          )}
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton
                asChild
                isActive={isActiveRoute("/settings")}
                tooltip="Settings"
                className="h-[38px] text-body font-medium text-muted-foreground data-[active=true]:font-semibold data-[active=true]:shadow-raised"
              >
                <NavLink to="/settings" aria-label="Settings">
                  <Settings aria-hidden="true" />
                  <span>Settings</span>
                </NavLink>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
          <div className="flex h-12 min-w-0 items-center gap-2 px-2 py-1 group-data-[collapsible=icon]:justify-center group-data-[collapsible=icon]:px-0">
            <span className="grid size-[29px] shrink-0 place-items-center rounded-full bg-card font-mono text-label font-semibold text-muted-foreground">
              GD
            </span>
            <span className="flex min-w-0 flex-col group-data-[collapsible=icon]:hidden">
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
                "ml-auto size-[7px] shrink-0 rounded-full ring-[3px] group-data-[collapsible=icon]:hidden",
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
        </SidebarFooter>
        <SidebarRail />
      </Sidebar>
      <SidebarInset id="main-content" tabIndex={-1} className="h-svh min-w-0 overflow-hidden">
        <header className="flex h-[54px] shrink-0 items-center gap-3 border-b border-border bg-background/95 px-[18px]">
          <SidebarTrigger className="-ml-1.5 shrink-0" />
          <button
            ref={searchTriggerRef}
            className="flex h-[34px] w-[min(390px,42vw)] items-center gap-2 rounded-lg border border-input bg-card px-3 text-left text-body-sm text-subtle-foreground shadow-raised transition-colors hover:border-input-hover hover:text-muted-foreground"
            onClick={() => setSearchOpen(true)}
          >
            <Search size={16} aria-hidden="true" />
            <span className="truncate">Search conversations, projects, and files</span>
            <Kbd className="ml-auto h-auto border border-border px-1.5 font-mono text-label text-subtle-foreground max-[900px]:hidden">
              Ctrl K
            </Kbd>
          </button>
          <div className="ml-auto flex items-center gap-2">
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
        <div className="min-h-0 min-w-0 flex-1 scroll-smooth overflow-auto">
          {snapshot && interfacePreview && (
            <Alert
              variant="warning"
              role="status"
              className="mx-[clamp(24px,3.2vw,48px)] mt-3 -mb-4 w-auto items-center px-3 py-2 max-[680px]:mx-3.5"
            >
              <AlertTriangle size={16} />
              <AlertTitle className="text-label">Interface preview</AlertTitle>
              <AlertDescription className="text-label">
                Sample data only. No Grok provider request or local tool execution is available.
              </AlertDescription>
            </Alert>
          )}
          {snapshot && !daemonConnected && !interfacePreview && (
            <Alert
              variant={daemonConnecting ? "info" : "warning"}
              role="status"
              className="mx-[clamp(24px,3.2vw,48px)] mt-3 -mb-4 w-auto grid-cols-[0_1fr_auto] items-center px-3 py-2 has-[>svg]:grid-cols-[calc(var(--spacing)*4)_1fr_auto] max-[680px]:mx-3.5"
            >
              {daemonConnecting ? <LoaderCircle size={16} className="animate-spin" /> : <AlertTriangle size={16} />}
              <AlertTitle className="text-label">{daemonConnecting ? "Starting local daemon" : "Limited Mode"}</AlertTitle>
              {snapshot.connection.reason && (
                <AlertDescription className="text-label">
                  {snapshot.connection.reason}
                </AlertDescription>
              )}
              {!daemonConnecting && (
                <NavLink
                  to="/settings"
                  className="col-start-3 row-span-2 row-start-1 self-center text-label font-semibold underline-offset-2 hover:underline"
                >
                  Open settings
                </NavLink>
              )}
            </Alert>
          )}
          {outlet}
        </div>
      </SidebarInset>
      {searchOpen && (
        <SearchDialog
          onClose={() => setSearchOpen(false)}
          restoreFocusRef={searchTriggerRef}
        />
      )}
    </SidebarProvider>
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
        <Command
          // Results come from the daemon; never let cmdk re-filter them.
          shouldFilter={false}
          // cmdk names the input through its own hidden label element.
          label="Search everything"
          className="bg-transparent [&_[data-slot=command-input-wrapper]]:h-[55px] [&_[data-slot=command-input-wrapper]]:gap-2.5 [&_[data-slot=command-input-wrapper]]:border-border [&_[data-slot=command-input-wrapper]]:pr-3 [&_[data-slot=command-input-wrapper]]:pl-4"
        >
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder="Search everything"
            aria-label="Search everything"
            aria-describedby="workspace-search-status"
            aria-invalid={queryValidationError ? "true" : undefined}
            maxLength={256}
            className="text-body-lg text-foreground placeholder:text-subtle-foreground"
            trailing={
              <>
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
              </>
            }
          />
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
          <CommandList className="max-h-[370px] p-2">
            {searching && <SearchResultsSkeleton />}
            {!searching && queryValidationError && (
              <Alert variant="destructive" className="m-2 w-auto p-3">
                <AlertDescription className="text-body-sm leading-5 text-destructive">{queryValidationError}</AlertDescription>
              </Alert>
            )}
            {!searching && !queryValidationError && matchingSearchError && (
              <Alert variant="destructive" className="m-2 w-auto p-3">
                <AlertDescription className="text-body-sm leading-5 text-destructive">{matchingSearchError}</AlertDescription>
                <Button
                  className="col-start-2 mt-3 justify-self-start"
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => setRetry((value) => value + 1)}
                >
                  Retry search
                </Button>
              </Alert>
            )}
            {!searching && !queryValidationError && !matchingSearchError && results.length > 0 && (
              <CommandGroup
                heading={query ? "Results" : "Recent"}
                className="p-0 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:font-mono [&_[cmdk-group-heading]]:text-label [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:tracking-[0.06em] [&_[cmdk-group-heading]]:text-subtle-foreground [&_[cmdk-group-heading]]:uppercase"
              >
                {results.map((result) => (
                  <CommandItem
                    key={`${result.kind}-${result.id}`}
                    value={`${result.kind}-${result.id}`}
                    onSelect={() => {
                      navigate(result.to);
                      onClose();
                    }}
                    className="min-h-12 cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-1.5"
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
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
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
          </CommandList>
        </Command>
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
