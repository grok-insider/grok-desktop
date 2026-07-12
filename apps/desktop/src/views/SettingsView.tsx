import { useEffect, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Accessibility,
  Bot,
  ChevronRight,
  CircleAlert,
  Database,
  Download,
  Globe2,
  HardDrive,
  KeyRound,
  Laptop,
  Moon,
  Network,
  RefreshCw,
  ShieldCheck,
  Sun,
  UserRound,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { Button, PageHeader, Toggle } from "../components/ui";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { AccountSetupState, ChatModelCatalog, DesktopPreferences } from "../services/desktopClient";
import { SETTINGS_PERSISTENCE_UNAVAILABLE_REASON } from "../services/productAvailability";

// Only sections with at least one daemon-backed control are advertised.
// Unfinished product surfaces (execution tools, browser grants, data export,
// accessibility prefs, auto-update) are not listed until their backends ship.
const settingSections = [
  { id: "account", label: "Account", icon: UserRound },
  { id: "general", label: "General", icon: Laptop },
  { id: "models", label: "Models", icon: Bot },
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

function UnavailableSelect({ label }: { label: string }) {
  return (
    <select
      className="h-[34px] min-w-36 rounded-md border border-input bg-card px-2 text-body text-muted-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 max-[680px]:w-full"
      aria-label={label}
      disabled
    >
      <option>Unavailable</option>
    </select>
  );
}

function SettingsUnavailableNotice() {
  return (
    <aside
      className="mb-4 flex min-h-16 items-start gap-3 rounded-lg border border-warning/25 bg-warning-soft p-4"
      aria-label="Preferences unavailable"
    >
      <CircleAlert className="mt-0.5 shrink-0 text-warning" size={18} aria-hidden="true" />
      <div className="min-w-0">
        <h3 className="m-0 text-body font-semibold text-foreground">Preferences unavailable</h3>
        <p className="m-0 mt-1 text-body-sm text-muted-foreground">{SETTINGS_PERSISTENCE_UNAVAILABLE_REASON}</p>
      </div>
    </aside>
  );
}

function AccountSettings() {
  const client = useDesktopClient();
  const [account, setAccount] = useState<AccountSetupState | null>(null);
  const [accountError, setAccountError] = useState("");
  const navigate = useNavigate();

  useEffect(() => {
    let active = true;
    void client
      .getAccountSetup()
      .then((value) => {
        if (active) setAccount(value);
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
            Subscription sign-in status is not exposed by the daemon.
          </p>
          <Badge className="mt-2" variant="warning">
            <ShieldCheck size={13} aria-hidden="true" /> Status unavailable
          </Badge>
        </div>
        <Button className="max-[680px]:w-full" onClick={() => navigate("/setup")}>
          Open setup
        </Button>
      </div>

      <SettingsGroup>
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
          <Button className="max-[680px]:w-full" onClick={() => navigate("/setup")}>
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
    </>
  );
}

function ModelSettings() {
  const client = useDesktopClient();
  const [catalog, setCatalog] = useState<ChatModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setCatalog(null);
    void client
      .getChatModelCatalog()
      .then((value) => {
        if (!active) return;
        setCatalog(value);
        setCatalogError("");
      })
      .catch(() => {
        if (active) {
          setCatalog(null);
          setCatalogError("The official xAI model catalog is unavailable. Check the API key and network, then retry.");
        }
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [client]);

  const refreshCatalog = async () => {
    if (loading || saving) return;
    setLoading(true);
    setCatalog(null);
    setCatalogError("");
    try {
      setCatalog(await client.getChatModelCatalog());
    } catch {
      setCatalog(null);
      setCatalogError("The official xAI model catalog is unavailable. Check the API key and network, then retry.");
    } finally {
      setLoading(false);
    }
  };

  const selectModel = async (modelId: string) => {
    if (!catalog || saving || !catalog.models.some((model) => model.id === modelId && model.textConversationReady)) return;
    setSaving(true);
    setCatalogError("");
    try {
      const preference = await client.selectChatModel({
        expectedRevision: catalog.preference.revision,
        modelId,
      });
      setCatalog({ ...catalog, preference, selectedModelReady: true });
    } catch {
      setCatalog(null);
      setCatalogError("The model selection outcome could not be reconciled with live daemon readiness. Retry discovery before using Chat.");
    } finally {
      setSaving(false);
    }
  };

  const selectableModels = catalog?.models.filter((model) => model.textConversationReady) ?? [];
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
            <select
              className="h-[34px] min-w-52 max-w-[min(32rem,50vw)] truncate rounded-md border border-input bg-card px-2 font-mono text-body-sm text-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 max-[680px]:w-full max-[680px]:max-w-full"
              aria-label="Default chat model"
              value={catalog?.preference.selectedModelId ?? ""}
              disabled={!catalog || loading || saving || selectableModels.length === 0}
              onChange={(event) => void selectModel(event.target.value)}
            >
              {!catalog && <option value="">Unavailable</option>}
              {catalog && !selectedInCatalog && (
                <option value={catalog.preference.selectedModelId} disabled>
                  {catalog.preference.selectedModelId} — unavailable
                </option>
              )}
              {selectableModels.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.id}{model.id === catalog?.defaultModelId ? " — product default" : ""}
                </option>
              ))}
            </select>
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
              {catalog?.preference.selectedModelId ?? "No live model selection"}
            </h3>
            {catalog && (
              <Badge variant={catalog.selectedModelReady ? "success" : "warning"}>
                {catalog.selectedModelReady ? "Ready" : "Not ready"}
              </Badge>
            )}
          </div>
          <p className="m-0 mt-1 text-body-sm text-muted-foreground">
            {catalog
              ? catalog.selectedModelReady
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
