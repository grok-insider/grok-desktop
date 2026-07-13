import { useState, type ReactNode } from "react";
import {
  Blocks,
  Check,
  ChevronRight,
  CircleAlert,
  Globe2,
  HardDrive,
  LoaderCircle,
  PlugZap,
  Search,
  ShieldCheck,
} from "lucide-react";
import { PageHeader } from "../components/PageHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { useDesktopSnapshot } from "../services/DesktopClientContext";
import type { ExtensionSummary } from "../services/desktopClient";

type ExtensionFilter = "all" | ExtensionSummary["kind"];

const filters: { id: ExtensionFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "built-in", label: "Built-in" },
  { id: "managed", label: "Managed" },
  { id: "mcp", label: "MCP" },
];

/**
 * Product extensions surface. Managed add-on install (including Wisp) is not
 * advertised: first-party Wisp under integrations/first-party/wisp remains
 * development source material until signed lifecycle IPC ships.
 */
export function ExtensionsView() {
  const { snapshot, loading } = useDesktopSnapshot();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<ExtensionFilter>("all");

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const extensions = snapshot?.extensions.filter((item) => {
    // Never surface Wisp install cards even if a stale snapshot still lists them.
    if (item.id === "wisp") return false;
    const matchesKind = filter === "all" || item.kind === filter;
    const searchable = `${item.name} ${item.description} ${item.permissions.join(" ")}`.toLocaleLowerCase();
    return matchesKind && searchable.includes(normalizedQuery);
  }) ?? [];
  const connected = extensions.filter((item) => item.status === "connected");
  const available = extensions.filter((item) => item.status !== "connected");
  const isolationReady = snapshot?.capabilities.some(
    (item) => ["work", "mcp", "shell"].includes(item.id) && item.available,
  ) ?? false;

  return (
    <div className="min-h-full px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <div className="mx-auto max-w-[1440px]">
        <PageHeader
          title="Integrations"
          description="Daemon-reported built-in surfaces. Managed add-on install is not available in this build."
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
                  : "No isolated tool capability is currently available. Wisp install is not a product surface in this build."}
            </p>
          </div>
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
          <Tabs value={filter} onValueChange={(value) => setFilter(value as ExtensionFilter)}>
            <TabsList aria-label="Extension filters" className="h-auto flex-wrap gap-1 rounded-lg bg-transparent p-0">
              {filters.map((item) => (
                <TabsTrigger
                  key={item.id}
                  value={item.id}
                  className={cn(
                    "h-7 flex-none rounded-md px-2.5 text-body-sm font-semibold text-muted-foreground",
                    "hover:bg-muted hover:text-foreground data-[state=active]:bg-secondary data-[state=active]:text-secondary-foreground data-[state=active]:shadow-none",
                  )}
                >
                  {item.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
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
          title="Unavailable"
          description="Managed install and unsupported integrations stay unavailable until daemon protocol support lands"
          emptyMessage="No unavailable integrations match this search."
          items={available}
          renderAction={(item) => (
            <Button disabled title={item.permissions.join(". ")} variant="outline">Unavailable</Button>
          )}
        />
      </div>
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
  const ExtensionIcon = item.id === "browser" ? Globe2 : item.id === "filesystem" ? HardDrive : Blocks;
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
