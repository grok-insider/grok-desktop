import { ArrowRight, CirclePause, Clock3, FileText, MessageSquareText, MoreHorizontal, Pin, Play, Sparkles } from "lucide-react";
import { Link } from "react-router-dom";
import { Composer } from "../components/Composer";
import { IconButton, RunStatus, SkeletonRows } from "../components/ui";
import { useDesktopSnapshot } from "../services/DesktopClientContext";
import { useNavigate } from "react-router-dom";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";
import { GROK_EXECUTION_UNAVAILABLE_REASON } from "../services/productAvailability";

function SectionHeading({ title, hint, to, linkLabel }: { title: string; hint: string; to: string; linkLabel: string }) {
  return (
    <div className="mb-3 flex items-center justify-between gap-4">
      <div>
        <h2 className="m-0 mb-0.5 text-body-lg font-semibold">{title}</h2>
        <p className="m-0 text-body-sm text-subtle-foreground">{hint}</p>
      </div>
      <Link to={to} className="inline-flex items-center gap-1 text-label font-semibold text-muted-foreground hover:text-foreground">
        {linkLabel} <ArrowRight size={14} />
      </Link>
    </div>
  );
}

export function HomeView() {
  const { snapshot, loading } = useDesktopSnapshot();
  const navigate = useNavigate();
  const today = new Intl.DateTimeFormat(undefined, { weekday: "long", month: "long", day: "numeric" }).format(new Date());

  return (
    <div className="mx-auto min-h-full max-w-[1540px] px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[900px]:px-5 max-[680px]:px-3.5">
      <section className="mx-auto mt-2 mb-9 max-w-[900px] max-[680px]:mt-0">
        <div className="mx-auto mb-5 text-center">
          <p className="m-0 mb-1.5 font-mono text-label font-medium tracking-[0.06em] text-subtle-foreground uppercase">{today}</p>
          <h1 className="m-0 text-display font-semibold tracking-[-0.01em] max-[680px]:text-title-lg">What are we working on?</h1>
        </div>
        <Composer />
      </section>

      <div className="grid grid-cols-[minmax(0,1.65fr)_minmax(290px,0.75fr)] gap-4 max-[1120px]:grid-cols-[minmax(0,1fr)]">
        <section className="min-w-0 rounded-lg border border-border bg-card p-5">
          <SectionHeading title="Recent conversations" hint="Pick up where you left off" to="/projects" linkLabel="View all" />
          {loading ? (
            <SkeletonRows count={4} />
          ) : (
            <div className="-mx-2 -mb-1">
              {snapshot?.threads.map((thread) => (
                <div
                  className="group relative grid min-h-[64px] grid-cols-[minmax(0,1fr)_34px] items-center gap-1 rounded-md border-t border-border/60 py-0.5 pr-1 pl-0.5 first:border-t-transparent hover:bg-muted"
                  key={thread.id}
                >
                  <button
                    className="grid min-h-[59px] min-w-0 grid-cols-[35px_minmax(0,1fr)_112px_15px] items-center gap-2.5 p-1 text-left max-[680px]:grid-cols-[35px_minmax(0,1fr)]"
                    onClick={() => navigate(`/conversations/${thread.id}`)}
                  >
                    <span
                      className={cn(
                        "grid size-[31px] place-items-center rounded-md",
                        thread.mode === "work" ? "bg-info-soft text-info" : "bg-muted text-muted-foreground",
                      )}
                    >
                      {thread.mode === "work" ? <Sparkles size={17} /> : <MessageSquareText size={17} />}
                    </span>
                    <span className="flex min-w-0 flex-col gap-1">
                      <strong className="truncate text-body font-semibold">{thread.title}</strong>
                      <small className="truncate text-body-sm text-subtle-foreground">{thread.preview}</small>
                    </span>
                    <span className="flex flex-col items-end gap-1 text-label whitespace-nowrap text-subtle-foreground max-[680px]:hidden">
                      <small>{thread.projectName}</small>
                      <time className="font-mono tabular-nums">{thread.updatedAt}</time>
                    </span>
                    {thread.pinned && <Pin size={14} className="text-subtle-foreground max-[680px]:hidden" aria-label="Pinned" />}
                  </button>
                  <IconButton label={`Additional actions for ${thread.title} require daemon protocol support`} disabled>
                    <MoreHorizontal size={17} />
                  </IconButton>
                </div>
              ))}
              {snapshot?.threads.length === 0 && (
                <div className="flex min-h-20 items-center justify-center gap-2 p-4 text-subtle-foreground">
                  <MessageSquareText size={19} />
                  <span className="flex flex-col gap-0.5">
                    <strong className="text-body-sm text-muted-foreground">No conversations yet</strong>
                    <small className="text-label">Start with Chat or Work above.</small>
                  </span>
                </div>
              )}
            </div>
          )}
        </section>

        <aside className="min-w-0 rounded-lg border border-border bg-card p-5 max-[1120px]:hidden">
          <SectionHeading title="Active work" hint="Runs across your projects" to="/activity" linkLabel="Open" />
          <div className="flex flex-col gap-2">
            {snapshot?.runs.filter((run) => run.state !== "completed").slice(0, 3).map((run) => (
              <article className="rounded-lg border border-border bg-muted p-3" key={run.id}>
                <div className="flex items-center justify-between gap-2">
                  <RunStatus state={run.state} />
                  <time className="font-mono text-label text-subtle-foreground tabular-nums">{run.updatedAt}</time>
                </div>
                <h3 className="m-0 mt-2 mb-1 text-body leading-snug font-semibold">{run.title}</h3>
                <p className="m-0 mb-2 min-h-7 text-body-sm leading-relaxed text-subtle-foreground">{run.detail}</p>
                <Progress
                  value={run.progress}
                  aria-label={`${run.progress}% complete`}
                  className="h-1 bg-secondary [&>[data-slot=progress-indicator]]:bg-info [&>[data-slot=progress-indicator]]:duration-250"
                />
                <div className="mt-1.5 flex items-center justify-between gap-2 text-label text-subtle-foreground">
                  <span>{run.projectName}</span>
                  <IconButton
                    label={run.state === "paused" ? "Resume unavailable" : "Pause unavailable"}
                    className="size-[27px]"
                    disabled
                    title={GROK_EXECUTION_UNAVAILABLE_REASON}
                  >
                    {run.state === "paused" ? <Play size={16} /> : <CirclePause size={16} />}
                  </IconButton>
                </div>
              </article>
            ))}
            {snapshot?.runs.length === 0 && (
              <div className="flex min-h-20 items-center justify-center gap-2 p-4 text-subtle-foreground">
                <Sparkles size={19} />
                <span className="flex flex-col gap-0.5">
                  <strong className="text-body-sm text-muted-foreground">No active work</strong>
                  <small className="text-label">Work sessions will appear here.</small>
                </span>
              </div>
            )}
          </div>
          <div className="mt-2 grid grid-cols-2 gap-2">
            <span className="flex h-[38px] items-center gap-1.5 rounded-md bg-muted px-2 text-label text-subtle-foreground">
              <FileText size={16} />
              <strong className="font-mono text-body-sm font-semibold text-foreground tabular-nums">{snapshot?.library.length ?? 0}</strong>
              recent files
            </span>
            <span className="flex h-[38px] items-center gap-1.5 rounded-md bg-muted px-2 text-label text-subtle-foreground">
              <Clock3 size={16} />
              <strong className="font-mono text-body-sm font-semibold text-foreground tabular-nums">
                {snapshot?.automations.length ?? 0}
              </strong>
              {" "}saved definitions
            </span>
          </div>
        </aside>
      </div>
    </div>
  );
}
