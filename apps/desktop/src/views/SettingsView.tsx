import { useCallback, useEffect, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Bot,
  ChartColumn,
  CircleAlert,
  KeyRound,
  Laptop,
  RefreshCw,
  ShieldCheck,
  UserRound,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { modelDisplayLabel } from "../lib/modelLabels";
import { formatTokenCount, formatUsageLine } from "../lib/usageFormat";
import { Button, PageHeader, Toggle } from "../components/ui";
import { useChatModelCatalog } from "../hooks/useChatModelCatalog";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type {
  AccountSetupState,
  DesktopPreferences,
  SuperGrokEnrollmentStatus,
  UsageSummary,
  UpdateState,
} from "../services/desktopClient";

// Only sections with at least one daemon-backed control are advertised.
// Unfinished product surfaces (execution tools, browser grants, data export,
// accessibility prefs, auto-update) are not listed until their backends ship.
const settingSections = [
  { id: "account", label: "Account", icon: UserRound },
  { id: "general", label: "General", icon: Laptop },
  { id: "models", label: "Models", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartColumn },
] as const;

type SettingsSection = (typeof settingSections)[number]["id"];

const settingsHeadingId = (section: SettingsSection) => `settings-${section}-heading`;

export function SettingsView() {
  const [section, setSection] = useState<SettingsSection>("account");

  const selectSectionFromKeyboard = (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
    let nextIndex: number | null = null;
    if (event.key === "ArrowDown" || event.key === "ArrowRight") {
      nextIndex = (currentIndex + 1) % settingSections.length;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
      nextIndex = (currentIndex - 1 + settingSections.length) % settingSections.length;
    }
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = settingSections.length - 1;
    if (nextIndex === null) return;

    event.preventDefault();
    setSection(settingSections[nextIndex].id);
    const buttons = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("button");
    buttons?.[nextIndex]?.focus();
  };

  return (
    <div className="min-h-full overflow-y-auto px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <div className="mx-auto max-w-[1440px]">
        <PageHeader
          title="Settings"
          description="Daemon-backed account, close-to-tray, and chat model preferences."
        />

        <div className="grid min-h-[590px] grid-cols-[12.25rem_minmax(0,1fr)] gap-6 max-[680px]:block">
          <nav
            className="flex min-w-0 flex-col gap-1 max-[680px]:mb-5 max-[680px]:flex-row max-[680px]:overflow-x-auto max-[680px]:pb-1"
            aria-label="Settings sections"
          >
            {settingSections.map((item, index) => {
              const active = section === item.id;
              return (
                <button
                  key={item.id}
                  type="button"
                  aria-controls="settings-panel"
                  aria-current={active ? "page" : undefined}
                  className={cn(
                    "flex h-[38px] min-w-0 items-center gap-2 rounded-md border border-transparent px-3 text-left text-body font-medium text-muted-foreground outline-none",
                    "transition-[background-color,border-color,color,box-shadow,transform] duration-150 ease-fluid hover:bg-muted hover:text-foreground active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-ring",
                    "max-[680px]:shrink-0",
                    active && "border-border bg-card font-semibold text-foreground shadow-raised hover:bg-card",
                  )}
                  onClick={() => setSection(item.id)}
                  onKeyDown={(event) => selectSectionFromKeyboard(event, index)}
                >
                  <item.icon size={17} aria-hidden="true" />
                  <span>{item.label}</span>
                </button>
              );
            })}
          </nav>

          <section
            id="settings-panel"
            className="min-w-0 max-w-[880px]"
            aria-labelledby={settingsHeadingId(section)}
          >
            {section === "account" && <AccountSettings />}
            {section === "usage" && <UsageSettings />}
            {section === "general" && <GeneralSettings />}
            {section === "models" && <ModelSettings />}
          </section>
        </div>
      </div>
    </div>
  );
}

function SettingsHeading({ section, title, description }: { section: SettingsSection; title: string; description: string }) {
  return (
    <header className="mb-5 border-b border-border pb-4">
      <h2 id={settingsHeadingId(section)} className="m-0 text-title font-semibold text-foreground">
        {title}
      </h2>
      <p className="m-0 mt-1 text-body text-muted-foreground">{description}</p>
    </header>
  );
}

function SettingsGroup({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn("mb-4 overflow-hidden rounded-lg border border-border bg-card last:mb-0", className)}>
      {children}
    </div>
  );
}

function SettingRow({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <div className="flex min-h-[72px] items-center justify-between gap-6 border-b border-border px-4 py-3 last:border-b-0 max-[680px]:flex-col max-[680px]:items-stretch max-[680px]:gap-3">
      <div className="min-w-0">
        <h3 className="m-0 text-body font-semibold text-foreground">{title}</h3>
        <p className="m-0 mt-1 max-w-[65ch] text-body-sm text-subtle-foreground">{description}</p>
      </div>
      <div className="shrink-0 max-[680px]:w-full max-[680px]:[&_[data-slot=button]]:w-full">{children}</div>
    </div>
  );
}

function AccountSettings() {
  const client = useDesktopClient();
  const [account, setAccount] = useState<AccountSetupState | null>(null);
  const [superGrok, setSuperGrok] = useState<SuperGrokEnrollmentStatus | null>(null);
  const [accountError, setAccountError] = useState("");
  const navigate = useNavigate();

  useEffect(() => {
    let active = true;
    void Promise.all([client.getAccountSetup(), client.getSuperGrokEnrollmentStatus()])
      .then(([value, superGrokStatus]) => {
        if (active) {
          setAccount(value);
          setSuperGrok(superGrokStatus);
        }
      })
      .catch(() => {
        if (active) setAccountError("Credential status unavailable");
      });
    return () => {
      active = false;
    };
  }, [client]);

  const keyConfigured = account?.xaiApiKey === "configured";
  const keyStatus = accountError || (account ? (keyConfigured ? "Configured" : "Not configured") : "Checking credential status");
  const keyDetail = accountError
    ? "Open setup to retry the local daemon and operating system vault."
    : keyConfigured
      ? "Stored in the operating system credential vault"
      : "Add a key through the secure operating system credential prompt";

  return (
    <>
      <SettingsHeading
        section="account"
        title="Account"
        description="Daemon-owned credential status and official Grok readiness."
      />

      <div className="mb-4 flex min-h-[88px] items-center gap-4 rounded-lg border border-border bg-card p-4 max-[680px]:flex-wrap">
        <span
          className="grid size-12 shrink-0 place-items-center rounded-full bg-accent font-mono text-body font-semibold text-foreground"
          aria-hidden="true"
        >
          GD
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="m-0 text-title-sm font-semibold text-foreground">Grok subscription</h3>
          <p className="m-0 mt-0.5 text-body-sm text-muted-foreground">
            Host authentication is managed in Setup. Work still requires isolation readiness.
          </p>
          <Badge className="mt-2" variant="neutral">
            <ShieldCheck size={13} aria-hidden="true" /> Use Setup
          </Badge>
        </div>
        <Button className="max-[680px]:w-full" onClick={() => navigate("/setup")}>
          Open setup
        </Button>
      </div>

      <SettingsGroup>
        <div className="flex min-h-[68px] items-center gap-3 border-b border-border px-4 py-3 max-[680px]:flex-wrap">
          <ShieldCheck className="shrink-0 text-subtle-foreground" size={18} aria-hidden="true" />
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <strong className="text-body font-semibold text-foreground">SuperGrok plan · API</strong>
            <span className="text-body-sm text-subtle-foreground">
              {superGrok?.state === "connected"
                ? "Connected and active for new Chat conversations"
                : "Connect through the official xAI device flow in Setup"}
            </span>
          </span>
          <Badge variant={superGrok?.state === "connected" ? "success" : "neutral"}>
            {superGrok?.state === "connected" ? "Connected" : "Not connected"}
          </Badge>
          <Button className="max-[680px]:w-full" onClick={() => navigate("/setup?step=api")}>Manage SuperGrok</Button>
        </div>

        <header className="flex min-h-16 items-center justify-between gap-4 border-b border-border px-4 py-3">
          <div className="min-w-0">
            <h3 className="m-0 text-body font-semibold text-foreground">xAI API key</h3>
            <p className="m-0 mt-1 text-body-sm text-muted-foreground">Daemon-owned operating system vault status.</p>
          </div>
          <Badge variant="neutral">Optional</Badge>
        </header>

        <div
          className="flex min-h-[68px] items-center gap-3 px-4 py-3 max-[680px]:flex-wrap"
          role={accountError ? "alert" : "status"}
          aria-live={accountError ? "assertive" : "polite"}
        >
          <KeyRound className="shrink-0 text-subtle-foreground" size={18} aria-hidden="true" />
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <strong className={cn("text-body font-semibold", accountError ? "text-destructive" : "text-foreground")}>
              {keyStatus}
            </strong>
            <span className="text-body-sm text-subtle-foreground">{keyDetail}</span>
          </span>
          <Button className="max-[680px]:w-full" onClick={() => navigate("/setup?step=api")}>
            {accountError ? "Retry" : keyConfigured ? "Manage" : "Add key"}
          </Button>
        </div>

        <p className="m-0 flex items-start gap-2 border-t border-success/20 bg-success-soft px-4 py-3 text-body-sm text-success">
          <ShieldCheck className="mt-0.5 shrink-0" size={15} aria-hidden="true" />
          <span>Stored credentials are never returned to the renderer or shared with tools.</span>
        </p>
      </SettingsGroup>

    </>
  );
}

function GeneralSettings() {
  const client = useDesktopClient();
  const [preferences, setPreferences] = useState<DesktopPreferences | null>(null);
  const [preferenceError, setPreferenceError] = useState("");
  const [savingPreference, setSavingPreference] = useState(false);
  const [update, setUpdate] = useState<UpdateState | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);

  useEffect(() => {
    let active = true;
    void client
      .getDesktopPreferences()
      .then((value) => {
        if (active) setPreferences(value);
      })
      .catch(() => {
        if (active) {
          setPreferenceError("Close behavior could not be loaded. Restart Grok Desktop and try again.");
        }
      });
    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    if (update?.phase !== "checking" && update?.phase !== "available") return;
    let active = true;
    const poll = setInterval(() => {
      void client.getUpdateState().then((value) => {
        if (active) setUpdate(value);
      }).catch(() => {
        // Preserve the last trustworthy state; the next poll may recover.
      });
    }, 500);
    return () => {
      active = false;
      clearInterval(poll);
    };
  }, [client, update?.phase]);

  useEffect(() => {
    let active = true;
    void client.getUpdateState().then((value) => {
      if (active) setUpdate(value);
    }).catch(() => {
      if (active) setUpdate(null);
    });
    return () => { active = false; };
  }, [client]);

  const checkForUpdates = async () => {
    if (checkingUpdate) return;
    setCheckingUpdate(true);
    try {
      setUpdate(await client.checkForUpdates());
    } finally {
      setCheckingUpdate(false);
    }
  };

  const updateCloseBehavior = async (keepRunningInNotificationArea: boolean) => {
    if (!preferences || savingPreference) return;
    const previous = preferences;
    setPreferenceError("");
    setSavingPreference(true);
    setPreferences({ ...previous, keepRunningInNotificationArea });
    try {
      const updated = await client.updateDesktopPreferences({
        expectedRevision: previous.revision,
        keepRunningInNotificationArea,
      });
      setPreferences(updated);
    } catch {
      setPreferences(previous);
      setPreferenceError("Close behavior was not saved. The previous setting has been restored; try again.");
    } finally {
      setSavingPreference(false);
    }
  };

  return (
    <>
      <SettingsHeading section="general" title="General" description="Daemon-owned desktop close behavior." />
      <SettingsGroup className="relative">
        <SettingRow
          title="Keep running in notification area"
          description="When on, closing the window hides Grok Desktop so background work can continue. Use Quit from the tray to stop the app."
        >
          <div className="flex flex-col items-end gap-1 max-[680px]:items-start">
            <Toggle
              checked={preferences?.keepRunningInNotificationArea ?? true}
              disabled={!preferences || savingPreference}
              onChange={(checked) => void updateCloseBehavior(checked)}
              label="Keep running in notification area"
            />
            {savingPreference && (
              <span className="text-label text-muted-foreground" role="status">
                Saving close behavior
              </span>
            )}
          </div>
        </SettingRow>
        {preferenceError && (
          <p className="m-0 border-b border-destructive/20 bg-destructive-soft px-4 py-3 text-body-sm text-destructive" role="alert">
            {preferenceError}
          </p>
        )}
      </SettingsGroup>
      <SettingsGroup>
        <SettingRow
          title="Application updates"
          description={updateDescription(update)}
        >
          {update?.phase === "downloaded" ? (
            <Button onClick={() => void client.installUpdate()}>Restart to update</Button>
          ) : (
            <Button
              disabled={!update || update.phase === "unsupported" || checkingUpdate || update.phase === "checking"}
              onClick={() => void checkForUpdates()}
              variant="secondary"
            >
              <RefreshCw className={cn((checkingUpdate || update?.phase === "checking") && "animate-spin")} size={14} />
              Check now
            </Button>
          )}
        </SettingRow>
      </SettingsGroup>
    </>
  );
}

function updateDescription(update: UpdateState | null): string {
  if (!update) return "Update status is temporarily unavailable.";
  if (update.phase === "unsupported") {
    return update.reasonCode === "development_install"
      ? `Version ${update.currentVersion}. Automatic updates are available in signed public installations.`
      : `Version ${update.currentVersion}. This installation is updated by its package manager.`;
  }
  if (update.phase === "checking") return `Version ${update.currentVersion}. Checking the stable channel.`;
  if (update.phase === "available") return `Downloading version ${update.targetVersion || "the latest release"}.`;
  if (update.phase === "downloaded") return `Version ${update.targetVersion || "the latest release"} is ready to install.`;
  if (update.phase === "failed") return `Version ${update.currentVersion}. The update check failed; try again.`;
  if (update.phase === "not_available") return `Version ${update.currentVersion} is current on the stable channel.`;
  return `Version ${update.currentVersion}. Automatic stable-channel updates are enabled.`;
}

function ModelSettings() {
  const { snapshot } = useDesktopSnapshot();
  const chatAvailable = snapshot?.capabilities.some(
    (capability) => capability.id === "chat" && capability.available,
  ) === true;
  const {
    catalog,
    status,
    error: catalogError,
    retry: refreshCatalog,
    setDefaultModel: selectModel,
    savingModelId,
    selectableModels,
  } = useChatModelCatalog(chatAvailable);
  const loading = status === "loading";
  const saving = savingModelId !== null;
  const selectedInCatalog = selectableModels.some((model) => model.id === catalog?.preference.selectedModelId);

  return (
    <>
      <SettingsHeading
        section="models"
        title="Models"
        description="Choose the daemon-owned default for new direct xAI Chat turns."
      />
      {catalogError && (
        <aside
          className="mb-4 flex min-h-16 items-start gap-3 rounded-lg border border-warning/25 bg-warning-soft p-4 max-[680px]:flex-wrap"
          role="alert"
        >
          <CircleAlert className="mt-0.5 shrink-0 text-warning" size={18} aria-hidden="true" />
          <div className="min-w-0 flex-1">
            <h3 className="m-0 text-body font-semibold text-foreground">Model catalog unavailable</h3>
            <p className="m-0 mt-1 text-body-sm text-muted-foreground">{catalogError}</p>
          </div>
          <Button className="max-[680px]:w-full" onClick={() => void refreshCatalog()} disabled={loading}>
            <RefreshCw size={15} aria-hidden="true" /> Retry discovery
          </Button>
        </aside>
      )}
      <SettingsGroup>
        <SettingRow
          title="Default chat model"
          description="Validated live against the fixed official xAI model catalog before the daemon commits it."
        >
          {loading && !catalog ? (
            <Skeleton className="h-[34px] w-48 max-[680px]:w-full" aria-label="Discovering official xAI models" />
          ) : (
            <Select
              value={catalog?.preference.selectedModelId ?? ""}
              disabled={!catalog || status !== "ready" || saving || selectableModels.length === 0}
              onValueChange={(value) => void selectModel(value)}
            >
              <SelectTrigger
                aria-label="Default chat model"
                className="h-[34px] min-w-52 max-w-[min(32rem,50vw)] bg-card font-mono text-body-sm text-foreground max-[680px]:w-full max-[680px]:max-w-full"
              >
                <SelectValue placeholder="Unavailable" />
              </SelectTrigger>
              <SelectContent position="popper" align="start">
                {catalog && !selectedInCatalog && (
                  <SelectItem value={catalog.preference.selectedModelId} disabled className="font-mono text-body-sm">
                    {modelDisplayLabel(catalog.preference.selectedModelId)} ({catalog.preference.selectedModelId}) — unavailable
                  </SelectItem>
                )}
                {selectableModels.map((model) => (
                  <SelectItem key={model.id} value={model.id} className="font-mono text-body-sm">
                    {modelDisplayLabel(model.id)}
                    {model.id === catalog?.defaultModelId ? " — product default" : ""}
                    {" · "}
                    {model.id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </SettingRow>
        {saving && (
          <p className="m-0 border-b border-info/20 bg-info-soft px-4 py-3 text-body-sm text-info" role="status">
            Validating and saving the Chat model
          </p>
        )}
      </SettingsGroup>
      <aside className="flex min-h-16 items-start gap-3 rounded-lg border border-border bg-card p-4" aria-label="Model status">
        <Bot className="mt-0.5 shrink-0 text-muted-foreground" size={18} aria-hidden="true" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="m-0 max-w-full break-words text-body font-semibold text-foreground [overflow-wrap:anywhere]">
              {catalog?.preference.selectedModelId
                ? modelDisplayLabel(catalog.preference.selectedModelId)
                : "No live model selection"}
            </h3>
            {catalog?.preference.selectedModelId ? (
              <span className="font-mono text-label text-subtle-foreground">
                {catalog.preference.selectedModelId}
              </span>
            ) : null}
            {catalog && (
              <Badge variant={status === "error" ? "warning" : catalog.selectedModelReady ? "success" : "warning"}>
                {status === "error" ? "Stale" : catalog.selectedModelReady ? "Ready" : "Not ready"}
              </Badge>
            )}
          </div>
          <p className="m-0 mt-1 text-body-sm text-muted-foreground">
            {catalog
              ? status === "error"
                ? "The last verified catalog is retained for context but cannot change the default until discovery succeeds."
                : catalog.selectedModelReady
                ? `${selectableModels.length} text-capable model${selectableModels.length === 1 ? "" : "s"} discovered. New turns use this selection; existing turns never change.`
                : "The persisted selection is absent from the current live catalog. Grok Desktop will not fall back silently."
              : "Add a user-owned xAI API key or retry live discovery to manage the default Chat model."}
          </p>
        </div>
        {catalog && (
          <Button className="max-[680px]:w-full" onClick={() => void refreshCatalog()} disabled={loading || saving}>
            <RefreshCw size={15} aria-hidden="true" /> Refresh
          </Button>
        )}
      </aside>
    </>
  );
}

function UsageSettings() {
  const client = useDesktopClient();
  const [last7, setLast7] = useState<UsageSummary | null>(null);
  const [last30, setLast30] = useState<UsageSummary | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    setStatus("loading");
    setError("");
    try {
      const [week, month] = await Promise.all([
        client.getUsageSummary({ scopeKind: "workspace", window: "last_7_days" }),
        client.getUsageSummary({ scopeKind: "workspace", window: "last_30_days" }),
      ]);
      setLast7(week);
      setLast30(month);
      setStatus("ready");
    } catch (cause) {
      setStatus("error");
      setError(cause instanceof Error ? cause.message : "Usage could not be loaded.");
    }
  }, [client]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <>
      <SettingsHeading
        section="usage"
        title="Usage"
        description="Official completed Chat turns only. Grok Desktop never invents tokens from context size."
      />
      <SettingsGroup>
        {status === "loading" && (
          <div className="space-y-3 p-4" role="status" aria-label="Loading usage">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-full" />
          </div>
        )}
        {status === "error" && (
          <div className="border-b border-warning/20 bg-warning-soft p-4" role="alert">
            <p className="m-0 text-body-sm text-warning">{error}</p>
            <Button className="mt-2" variant="secondary" onClick={() => void load()}>
              <RefreshCw size={14} aria-hidden="true" /> Retry
            </Button>
          </div>
        )}
        {status === "ready" && last7 && last30 && (
          <>
            <UsageWindowRow title="Last 7 days" summary={last7} />
            <UsageWindowRow title="Last 30 days" summary={last30} />
          </>
        )}
      </SettingsGroup>
      <p className="m-0 mt-3 text-body-sm text-subtle-foreground">
        SuperGrok weekly plan allowance is not shown here yet. When available, it will use a separate
        daemon-owned subscription probe — not third-party usage tools.
      </p>
    </>
  );
}

function UsageWindowRow({ title, summary }: { title: string; summary: UsageSummary }) {
  return (
    <div className="flex min-h-[72px] items-center justify-between gap-6 border-b border-border px-4 py-3 last:border-b-0 max-[680px]:flex-col max-[680px]:items-stretch">
      <div className="min-w-0">
        <h3 className="m-0 text-body font-semibold text-foreground">{title}</h3>
        <p className="m-0 mt-1 font-mono text-body-sm text-subtle-foreground">
          {formatUsageLine(summary)} · {formatTokenCount(summary.turnCount)} turns
        </p>
      </div>
      <div className="text-right font-mono text-label text-muted-foreground tabular-nums max-[680px]:text-left">
        <div>{formatTokenCount(summary.inputTokens)} in</div>
        <div>{formatTokenCount(summary.outputTokens)} out</div>
      </div>
    </div>
  );
}
