import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from "react";
import {
  Activity,
  ArrowRight,
  CircleAlert,
  Clock3,
  FileText,
  Folder,
  LoaderCircle,
  MessageSquareText,
  Plus,
  Settings2,
} from "lucide-react";
import { useNavigate, useParams } from "react-router-dom";
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
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { PageHeader, RunStatus } from "../components/ui";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { DesktopSnapshot, LibraryItem, ProjectSummary } from "../services/desktopClient";
import { SETTINGS_PERSISTENCE_UNAVAILABLE_REASON } from "../services/productAvailability";

const tabs = ["Overview", "Conversations", "Files", "Automations", "Instructions"] as const;
type ProjectTab = (typeof tabs)[number];

const SAFE_ACCENT = /^#[0-9a-f]{6}$/i;

const libraryRevisionDetail = (item: LibraryItem) => item.contentVersion === undefined
  ? `Metadata r${item.metadataRevision}`
  : `Version ${item.contentVersion}`;

export function ProjectsView() {
  const { snapshot, loading } = useDesktopSnapshot();
  const client = useDesktopClient();
  const { projectId } = useParams();
  const navigate = useNavigate();
  const createButtonRef = useRef<HTMLButtonElement>(null);
  const [tab, setTab] = useState<ProjectTab>("Overview");
  const [createOpen, setCreateOpen] = useState(false);
  const selected = projectId ? snapshot?.projects.find((project) => project.id === projectId) : undefined;

  useEffect(() => {
    if (!snapshot) return;
    const firstProjectId = snapshot.projects[0]?.id;
    if (!firstProjectId) return;
    if (!projectId || !snapshot.projects.some((project) => project.id === projectId)) {
      navigate(`/projects/${firstProjectId}`, { replace: true });
    }
  }, [navigate, projectId, snapshot]);

  useEffect(() => {
    setTab("Overview");
  }, [projectId]);

  return (
    <div className="flex h-full min-h-0 flex-col px-[clamp(24px,3.2vw,48px)] pt-8 pb-6 max-[680px]:h-auto max-[680px]:min-h-full max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <PageHeader
        title="Projects"
        description="Shared context, files, and work organized by outcome."
        actions={
          <Button ref={createButtonRef} onClick={() => setCreateOpen(true)}>
            <Plus size={16} aria-hidden="true" /> New project
          </Button>
        }
      />

      <div
        className="mx-auto grid min-h-0 w-full max-w-[1440px] flex-1 grid-cols-[17.5rem_minmax(0,1fr)] overflow-hidden rounded-lg border border-border bg-card max-[1120px]:grid-cols-[13.75rem_minmax(0,1fr)] max-[680px]:flex-none max-[680px]:grid-cols-1 max-[680px]:overflow-visible"
        data-testid="projects-layout"
        aria-busy={loading}
      >
        <aside className="min-h-0 overflow-hidden border-r border-border bg-muted max-[680px]:border-r-0 max-[680px]:border-b">
          <nav
            className="h-full overflow-y-auto p-2 max-[680px]:h-auto max-[680px]:overflow-x-auto max-[680px]:overflow-y-hidden"
            aria-label="Projects"
          >
            {loading ? (
              <ProjectsLoading />
            ) : snapshot?.projects.length ? (
              <ul className="m-0 flex list-none flex-col gap-1 p-0 max-[680px]:flex-row">
                {snapshot.projects.map((project) => {
                  const active = selected?.id === project.id;
                  return (
                    <li className="max-[680px]:shrink-0" key={project.id}>
                      <button
                        type="button"
                        aria-current={active ? "page" : undefined}
                        className={cn(
                          "grid min-h-20 w-full grid-cols-[34px_minmax(0,1fr)_auto] items-start gap-2 rounded-lg border px-2 py-3 text-left outline-none",
                          "transition-[background-color,border-color,box-shadow,transform] duration-150 ease-fluid hover:bg-accent active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-ring",
                          "max-[680px]:min-w-[220px]",
                          active
                            ? "border-input bg-card shadow-raised hover:bg-card"
                            : "border-transparent bg-transparent",
                        )}
                        onClick={() => navigate(`/projects/${project.id}`)}
                      >
                        <ProjectMark project={project} size="small" />
                        <span className="flex min-w-0 flex-col gap-0.5">
                          <strong className="truncate text-body font-semibold text-foreground" title={project.name}>
                            {project.name}
                          </strong>
                          <span className="truncate text-body-sm text-muted-foreground" title={project.description}>
                            {project.description || "No description"}
                          </span>
                          <span className="font-mono text-label font-medium tabular-nums text-subtle-foreground">
                            {project.threads} conversations · {project.updatedAt}
                          </span>
                        </span>
                        {project.activeRunCount > 0 && (
                          <Badge variant="info" aria-label={`${project.activeRunCount} active runs`}>
                            {project.activeRunCount}
                          </Badge>
                        )}
                      </button>
                    </li>
                  );
                })}
              </ul>
            ) : (
              <div className="flex min-h-28 items-center justify-center gap-3 px-3 py-6 text-muted-foreground" role="status">
                <Folder className="shrink-0" size={20} aria-hidden="true" />
                <span className="flex flex-col gap-0.5">
                  <strong className="text-body font-semibold text-foreground">No projects yet</strong>
                  <span className="text-body-sm">Create one to organize context and work.</span>
                </span>
              </div>
            )}
          </nav>
        </aside>

        {selected ? (
          <section
            className="grid min-h-0 min-w-0 grid-rows-[auto_auto_minmax(0,1fr)] overflow-hidden px-6 max-[1120px]:px-4 max-[680px]:block max-[680px]:overflow-visible"
            aria-labelledby="project-detail-heading"
          >
            <header className="flex min-h-[88px] items-center justify-between gap-5 py-3 max-[680px]:flex-col max-[680px]:items-stretch">
              <div className="flex min-w-0 items-center gap-3">
                <ProjectMark project={selected} size="large" />
                <div className="min-w-0">
                  <h2 id="project-detail-heading" className="m-0 truncate text-title font-semibold text-foreground" title={selected.name}>
                    {selected.name}
                  </h2>
                  <p className="m-0 mt-0.5 line-clamp-2 text-body-sm text-muted-foreground">{selected.description || "No description"}</p>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-2 max-[680px]:justify-end">
                <Button variant="outline" onClick={() => navigate("/")}>
                  <MessageSquareText size={15} aria-hidden="true" /> New conversation
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Project settings require daemon workspace support"
                  title="Project settings require daemon workspace support"
                  disabled
                >
                  <Settings2 size={17} aria-hidden="true" />
                </Button>
              </div>
            </header>

            <Tabs
              value={tab}
              onValueChange={(value) => setTab(value as ProjectTab)}
              className="flex min-h-0 flex-1 flex-col gap-0"
            >
              <TabsList
                aria-label="Project sections"
                variant="line"
                className="h-auto w-full max-w-full justify-start gap-5 overflow-x-auto overflow-y-hidden rounded-none border-b border-border p-0"
              >
                {tabs.map((item) => (
                  <TabsTrigger
                    value={item}
                    key={item}
                    className={cn(
                      "h-10 flex-none rounded-none border-0 border-b-2 border-transparent bg-transparent px-0.5 font-mono text-label font-semibold text-subtle-foreground",
                      "transition-[color,transform] duration-150 ease-fluid after:hidden",
                      "hover:text-foreground active:scale-[.98]",
                      "data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground",
                    )}
                  >
                    {item}
                  </TabsTrigger>
                ))}
              </TabsList>
              {tabs.map((item) => (
                <TabsContent
                  value={item}
                  key={item}
                  className="min-h-0 overflow-y-auto py-5 max-[680px]:overflow-visible"
                >
                  {item === "Overview" && <ProjectOverview project={selected} snapshot={snapshot} />}
                  {item === "Conversations" && <ProjectConversations projectName={selected.name} snapshot={snapshot} />}
                  {item === "Files" && <ProjectFiles projectName={selected.name} snapshot={snapshot} />}
                  {item === "Automations" && <ProjectAutomations projectName={selected.name} snapshot={snapshot} />}
                  {item === "Instructions" && <ProjectInstructions instructions={selected.instructions} />}
                </TabsContent>
              ))}
            </Tabs>
          </section>
        ) : (
          <ProjectDetailPlaceholder loading={loading} hasProjects={Boolean(snapshot?.projects.length)} />
        )}
      </div>

      <CreateProjectDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        restoreFocusRef={createButtonRef}
        onSave={async (name, description) => {
          const result = await client.createProject({ name, description });
          if (result.status !== "success") throw new Error(result.reason);
          setCreateOpen(false);
          navigate(`/projects/${result.value.id}`);
        }}
      />
    </div>
  );
}

function ProjectsLoading() {
  return (
    <div className="flex flex-col gap-2" role="status" aria-label="Loading projects" aria-live="polite">
      <span className="sr-only">Loading projects</span>
      {Array.from({ length: 3 }).map((_, index) => (
        <Skeleton className="h-20 rounded-lg" key={index} />
      ))}
    </div>
  );
}

function ProjectDetailPlaceholder({ loading, hasProjects }: { loading: boolean; hasProjects: boolean }) {
  if (loading) {
    return (
      <section className="min-w-0 px-6 py-5" aria-label="Loading project details">
        <div className="flex items-center gap-3" role="status">
          <Skeleton className="size-10 rounded-lg" />
          <div className="flex flex-1 flex-col gap-2">
            <Skeleton className="h-5 w-40" />
            <Skeleton className="h-3 w-full max-w-80" />
          </div>
          <span className="sr-only">Loading project details</span>
        </div>
      </section>
    );
  }

  return (
    <section className="flex min-h-72 min-w-0 flex-col items-center justify-center px-6 py-12 text-center" aria-label="Project details">
      <span className="grid size-12 place-items-center rounded-full bg-secondary text-muted-foreground" aria-hidden="true">
        <Folder size={22} />
      </span>
      <h2 className="m-0 mt-4 text-title-sm font-semibold text-foreground">
        {hasProjects ? "Opening a project" : "No projects yet"}
      </h2>
      <p className="m-0 mt-1 max-w-[44ch] text-body text-muted-foreground" role={hasProjects ? "status" : undefined}>
        {hasProjects
          ? "Taking you to the first available project."
          : "Create a project to group its conversations, files, and work."}
      </p>
    </section>
  );
}

function ProjectMark({ project, size }: { project: ProjectSummary; size: "small" | "large" }) {
  const style: CSSProperties | undefined = SAFE_ACCENT.test(project.accent)
    ? { backgroundColor: project.accent }
    : undefined;
  const initial = project.name.trim().charAt(0).toUpperCase() || "P";

  return (
    <span
      className={cn(
        "grid shrink-0 place-items-center rounded-lg bg-primary font-mono font-semibold text-primary-foreground",
        size === "small" ? "size-[34px] text-body-sm" : "size-10 text-body",
      )}
      style={style}
      aria-hidden="true"
    >
      {initial}
    </span>
  );
}

function ProjectOverview({ project, snapshot }: { project: ProjectSummary; snapshot: DesktopSnapshot | null }) {
  const navigate = useNavigate();
  const runs = snapshot?.runs.filter((run) => run.projectName === project.name) ?? [];
  const assets = snapshot?.library.filter((item) => item.projectName === project.name) ?? [];
  const activeRuns = runs.filter((run) => [
    "queued",
    "planning",
    "awaiting_approval",
    "running",
    "paused",
    "interrupted_needs_review",
  ].includes(run.state)).length;

  return (
    <div className="grid grid-cols-2 gap-x-6 gap-y-5 max-[900px]:grid-cols-1">
      <dl className="col-span-full m-0 grid grid-cols-4 gap-2 max-[900px]:grid-cols-2 max-[480px]:grid-cols-1" aria-label="Project summary">
        <ProjectMetric icon={<MessageSquareText size={17} />} label="Conversations" value={project.threads} />
        <ProjectMetric icon={<Activity size={17} />} label="Active runs" value={activeRuns} />
        <ProjectMetric icon={<Folder size={17} />} label="Linked folders" value={project?.folders ?? 0} />
        <ProjectMetric icon={<FileText size={17} />} label="Recent files" value={assets.length} />
      </dl>

      <ProjectSection title="Current work" description="Active and recently completed runs">
        <ProjectCollection emptyMessage="No runs in this project yet.">
          {runs.map((run) => (
            <ProjectCollectionRow
              key={run.id}
              icon={<Activity size={17} />}
              title={run.title}
              detail={run.detail}
              status={<RunStatus state={run.state} />}
              onClick={() => navigate(`/activity?run=${run.id}`)}
            />
          ))}
        </ProjectCollection>
      </ProjectSection>

      <ProjectSection title="Recent files" description="Artifacts and source material">
        <ProjectCollection emptyMessage="No files are linked to this project yet.">
          {assets.map((item) => (
            <ProjectCollectionRow
              key={item.id}
              icon={<FileText size={17} />}
              title={item.name}
              detail={`${item.size} · ${libraryRevisionDetail(item)}`}
              meta={item.updatedAt}
              onClick={() => navigate("/library")}
            />
          ))}
        </ProjectCollection>
      </ProjectSection>
    </div>
  );
}

function ProjectMetric({ icon, label, value }: { icon: ReactNode; label: string; value: number }) {
  return (
    <div className="grid min-h-[76px] grid-cols-[24px_minmax(0,1fr)] grid-rows-2 content-center rounded-lg border border-border bg-card px-3 py-3">
      <dt className="contents">
        <span className="row-span-2 self-center text-muted-foreground" aria-hidden="true">{icon}</span>
        <span className="col-start-2 row-start-2 text-label font-medium text-subtle-foreground">{label}</span>
      </dt>
      <dd className="col-start-2 row-start-1 m-0 font-mono text-title-sm font-semibold tabular-nums text-foreground">{value}</dd>
    </div>
  );
}

function ProjectSection({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <section className="min-w-0">
      <header className="flex min-h-12 items-center justify-between gap-4">
        <div>
          <h3 className="m-0 text-body-lg font-semibold text-foreground">{title}</h3>
          <p className="m-0 text-body-sm text-muted-foreground">{description}</p>
        </div>
      </header>
      {children}
    </section>
  );
}

function ProjectCollection({ children, emptyMessage }: { children: ReactNode[]; emptyMessage: string }) {
  return (
    <div className="border-t border-border">
      {children.length ? children : (
        <p className="m-0 min-h-20 px-3 py-6 text-center text-body-sm text-muted-foreground" role="status">
          {emptyMessage}
        </p>
      )}
    </div>
  );
}

function ProjectCollectionRow({
  icon,
  title,
  detail,
  meta,
  status,
  onClick,
}: {
  icon: ReactNode;
  title: string;
  detail: string;
  meta?: string;
  status?: ReactNode;
  onClick(): void;
}) {
  return (
    <button
      type="button"
      className="group flex min-h-[58px] w-full items-center gap-2 border-b border-border bg-transparent px-2 py-2 text-left outline-none transition-[background-color,transform] duration-150 ease-fluid hover:bg-muted active:scale-[.995] focus-visible:ring-[3px] focus-visible:ring-inset focus-visible:ring-ring"
      onClick={onClick}
    >
      <span className="shrink-0 text-muted-foreground" aria-hidden="true">{icon}</span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <strong className="truncate text-body-sm font-semibold text-foreground" title={title}>{title}</strong>
        <span className="truncate text-label text-muted-foreground" title={detail}>{detail}</span>
      </span>
      {status}
      {meta && (
        <span className="shrink-0 font-mono text-label font-medium tabular-nums text-subtle-foreground max-[480px]:hidden">
          {meta}
        </span>
      )}
      <ArrowRight className="shrink-0 text-subtle-foreground transition-transform duration-150 group-hover:translate-x-0.5" size={15} aria-hidden="true" />
    </button>
  );
}

function ProjectConversations({ projectName, snapshot }: { projectName: string; snapshot: DesktopSnapshot | null }) {
  const navigate = useNavigate();
  const threads = snapshot?.threads.filter((thread) => thread.projectName === projectName) ?? [];

  return (
    <ProjectCollection emptyMessage="No conversations are attached to this project yet.">
      {threads.map((thread) => (
        <ProjectCollectionRow
          key={thread.id}
          icon={<MessageSquareText size={18} />}
          title={thread.title}
          detail={thread.preview}
          meta={thread.updatedAt}
          onClick={() => navigate(`/conversations/${thread.id}`)}
        />
      ))}
    </ProjectCollection>
  );
}

function ProjectFiles({ projectName, snapshot }: { projectName: string; snapshot: DesktopSnapshot | null }) {
  const navigate = useNavigate();
  const files = snapshot?.library.filter((item) => item.projectName === projectName) ?? [];

  return (
    <ProjectCollection emptyMessage="No files are linked to this project yet.">
      {files.map((item) => (
        <ProjectCollectionRow
          key={item.id}
          icon={<FileText size={18} />}
          title={item.name}
          detail={`${item.type} · ${item.size} · ${libraryRevisionDetail(item)}`}
          meta={item.updatedAt}
          onClick={() => navigate("/library")}
        />
      ))}
    </ProjectCollection>
  );
}

function ProjectAutomations({ projectName, snapshot }: { projectName: string; snapshot: DesktopSnapshot | null }) {
  const navigate = useNavigate();
  const automations = snapshot?.automations.filter((item) => item.projectName === projectName) ?? [];

  return (
    <ProjectCollection emptyMessage="No automation definitions are attached to this project.">
      {automations.map((item) => (
        <ProjectCollectionRow
          key={item.id}
          icon={<Clock3 size={18} />}
          title={item.name}
          detail={item.schedule}
          meta={item.nextRun}
          onClick={() => navigate("/automations")}
        />
      ))}
    </ProjectCollection>
  );
}

function ProjectInstructions({ instructions }: { instructions: string }) {
  return (
    <div className="max-w-[700px]">
      <label className="mb-1 block text-body font-semibold text-foreground" htmlFor="project-instructions">
        Project instructions
      </label>
      <p className="m-0 mb-3 max-w-[65ch] text-body-sm text-muted-foreground" id="project-instructions-help">
        {SETTINGS_PERSISTENCE_UNAVAILABLE_REASON}
      </p>
      <Textarea
        id="project-instructions"
        rows={10}
        value={instructions}
        readOnly
        aria-describedby="project-instructions-help"
        className="resize-y bg-muted"
      />
      <div className="mt-3 flex items-center justify-end gap-3 max-[680px]:flex-col max-[680px]:items-stretch">
        <span className="mr-auto flex items-center gap-1.5 text-body-sm text-warning" role="status">
          <CircleAlert size={15} aria-hidden="true" /> Instructions are not applied to requests.
        </span>
        <Button disabled title={SETTINGS_PERSISTENCE_UNAVAILABLE_REASON}>Save unavailable</Button>
      </div>
    </div>
  );
}

function CreateProjectDialog({
  open,
  onOpenChange,
  onSave,
  restoreFocusRef,
}: {
  open: boolean;
  onOpenChange(open: boolean): void;
  onSave(name: string, description: string): Promise<void>;
  restoreFocusRef: RefObject<HTMLButtonElement | null>;
}) {
  const nameRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) return;
    setName("");
    setDescription("");
    setError("");
  }, [open]);

  const submit = async () => {
    if (!name.trim() || saving) return;
    setSaving(true);
    setError("");
    try {
      await onSave(name.trim(), description.trim());
    } catch (submitError) {
      setError(submitError instanceof Error ? submitError.message : "The project could not be created.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => !saving && onOpenChange(nextOpen)}>
      <DialogContent
        className="w-[min(520px,calc(100vw-32px))] gap-0 p-0"
        showCloseButton={!saving}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          nameRef.current?.focus();
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          restoreFocusRef.current?.focus();
        }}
        onEscapeKeyDown={(event) => saving && event.preventDefault()}
      >
        <DialogHeader className="border-b border-border px-5 py-4 pr-12">
          <DialogTitle>Create project</DialogTitle>
          <DialogDescription>Group related context, files, and work.</DialogDescription>
        </DialogHeader>
        <form
          className="flex flex-col gap-4 p-5"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <label className="flex flex-col gap-1 text-body font-semibold text-foreground" htmlFor="project-name">
            Project name
            <Input
              ref={nameRef}
              id="project-name"
              value={name}
              disabled={saving}
              onChange={(event) => setName(event.target.value)}
              placeholder="New project"
              autoComplete="off"
            />
          </label>
          <label className="flex flex-col gap-1 text-body font-semibold text-foreground" htmlFor="project-description">
            Description
            <Textarea
              id="project-description"
              rows={3}
              value={description}
              disabled={saving}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="What is this project for?"
              className="resize-y font-normal"
            />
          </label>
          {error && (
            <Alert variant="destructive" className="border-transparent px-3 py-2">
              <CircleAlert size={15} aria-hidden="true" />
              <AlertDescription className="text-body-sm text-destructive">{error}</AlertDescription>
            </Alert>
          )}
          <DialogFooter className="mt-1 max-[480px]:flex-col-reverse">
            <Button type="button" variant="outline" disabled={saving} onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!name.trim() || saving}>
              {saving && <LoaderCircle className="animate-spin motion-reduce:animate-none" size={15} aria-hidden="true" />}
              Create project
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
