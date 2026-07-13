import { useCallback, useEffect, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  Bot,
  ChartColumn,
  CircleAlert,
  KeyRound,
  Laptop,
  RefreshCw,
  ShieldCheck,
  ShieldAlert,
  FolderOpen,
  Trash2,
  UserRound,
} from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { cn } from "@/lib/utils";
import { modelDisplayLabel } from "../lib/modelLabels";
import { formatTokenCount, formatUsageLine } from "../lib/usageFormat";
import { PageHeader } from "../components/PageHeader";
import { useChatModelCatalog } from "../hooks/useChatModelCatalog";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import { grokBuildAgentRuntimeDetail } from "../services/productAvailability";
import type {
  AccountSetupState,
  DesktopPreferences,
  SuperGrokEnrollmentStatus,
  UsageSummary,
  UpdateState,
  HostExecutionPolicy,
} from "../services/desktopClient";

// Only sections with at least one daemon-backed control are advertised.
// Unfinished product surfaces (execution tools, browser grants, data export,
// accessibility prefs, auto-update) are not listed until their backends ship.
const settingSections = [
  { id: "account", label: "Account", icon: UserRound },
  { id: "general", label: "General", icon: Laptop },
  { id: "work", label: "Work execution", icon: ShieldAlert },
  { id: "models", label: "Models", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartColumn },
] as const;

type SettingsSection = (typeof settingSections)[number]["id"];

const settingsHeadingId = (section: SettingsSection) => `settings-${section}-heading`;

function settingsSectionFromSearch(search: string): SettingsSection | null {
  const requested = new URLSearchParams(search).get("section");
  return settingSections.some(({ id }) => id === requested)
    ? requested as SettingsSection
    : null;
}

export function SettingsView() {
  const location = useLocation();
  const [section, setSection] = useState<SettingsSection>(
    () => settingsSectionFromSearch(location.search) ?? "account",
  );

  useEffect(() => {
    const requested = settingsSectionFromSearch(location.search);
    if (requested) setSection(requested);
  }, [location.search]);

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
            {section === "work" && <WorkExecutionSettings />}
            {section === "models" && <ModelSettings />}
          </section>
        </div>
      </div>
    </div>
  );
}

const HOST_ACKNOWLEDGMENT_PHRASE = "I UNDERSTAND HOST TOOLS CAN CONTROL THIS COMPUTER";

function WorkExecutionSettings() {
  const client = useDesktopClient();
  const navigate = useNavigate();
  const { snapshot } = useDesktopSnapshot();
  const [policy, setPolicy] = useState<HostExecutionPolicy | null>(null);
  const [account, setAccount] = useState<AccountSetupState | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [step, setStep] = useState<1 | 2 | 3>(1);
  const [roots, setRoots] = useState<string[]>([]);
  const [filesystemRead, setFilesystemRead] = useState(true);
  const [filesystemWrite, setFilesystemWrite] = useState(false);
  const [processExecute, setProcessExecute] = useState(false);
  const [broadScopeAcknowledged, setBroadScopeAcknowledged] = useState(false);
  const [typedAcknowledgment, setTypedAcknowledgment] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      const [nextPolicy, nextAccount] = await Promise.all([
        client.getHostExecutionPolicy(),
        client.getAccountSetup(),
      ]);
      setPolicy(nextPolicy);
      setAccount(nextAccount);
      setError("");
    } catch {
      setError("Host Tools policy is unavailable.");
    }
  }, [client]);

  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    if (!policy?.active) return;
    setRoots(policy.pathRoots);
    setFilesystemRead(policy.filesystemRead);
    setFilesystemWrite(policy.filesystemWrite);
    setProcessExecute(policy.processExecute);
    setBroadScopeAcknowledged(policy.broadScopeAcknowledged);
  }, [policy]);

  const selectFolder = async () => {
    try {
      const selected = await client.selectHostWorkFolder();
      if (selected && !roots.includes(selected) && roots.length < 8) setRoots([...roots, selected]);
    } catch {
      setError("The folder chooser is unavailable.");
    }
  };

  const enroll = async () => {
    if (!policy || typedAcknowledgment.trim() !== HOST_ACKNOWLEDGMENT_PHRASE) return;
    setBusy(true);
    setError("");
    try {
      const next = await client.enrollHostExecution({
        expectedRevision: policy.revision,
        acknowledgmentVersion: policy.requiredAcknowledgmentVersion,
        typedAcknowledgment,
        filesystemRead,
        filesystemWrite,
        processExecute,
        pathRoots: roots,
        broadScopeAcknowledged,
      });
      setPolicy(next);
      setDialogOpen(false);
      setStep(1);
      setTypedAcknowledgment("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Host Tools enrollment failed.");
    } finally {
      setBusy(false);
    }
  };

  const mutateRuntime = async (action: "prepare" | "deactivate" | "revoke") => {
    if (!policy) return;
    setBusy(true);
    setError("");
    try {
      const next = action === "prepare"
        ? await client.prepareHostWorkRuntime()
        : action === "deactivate"
          ? await client.deactivateHostWorkRuntime()
          : await client.revokeHostExecution(policy.revision);
      setPolicy(next);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Host Tools settings could not be changed.");
    } finally {
      setBusy(false);
    }
  };

  const hostEffective = snapshot?.workExecution.mode === "host_direct";
  const agentRuntime = snapshot?.connection.agentRuntime;
  const runtimeAvailable = snapshot?.connection.interfacePreview === true || agentRuntime?.healthy === true;
  const accountConnected = account?.grokBuild === "connected";
  const runtimePrerequisite = !runtimeAvailable
    ? grokBuildAgentRuntimeDetail(agentRuntime)
    : !accountConnected
      ? "Connect Grok Build in Setup before preparing Host Tools."
      : "Enrollment is saved, but Work stays unavailable until the runtime is prepared.";
  const canPrepareRuntime = runtimeAvailable && accountConnected;

  return (
    <>
      <SettingsHeading
        section="work"
        title="Work execution"
        description="Choose whether Grok may use explicitly scoped tools on this computer. Isolated Work remains preferred when available."
      />
      {error && <p className="mb-4 text-body text-destructive" role="alert">{error}</p>}
      <SettingsGroup>
        <SettingRow
          title="Host Tools"
          description={policy?.active
            ? "Enrolled paths and tool classes are enforced by the local daemon."
            : "Off by default. Enabling requires a risk review, selected folders, and typed acknowledgment."}
        >
          {policy?.active ? (
            <Badge variant={hostEffective ? "warning" : "neutral"}>
              {hostEffective ? "HOST TOOLS active" : policy.runtimePrepared ? "Prepared" : "Not prepared"}
            </Badge>
          ) : (
            <Button disabled={!policy || busy} onClick={() => setDialogOpen(true)}>Review risks and enable</Button>
          )}
        </SettingRow>
        {policy?.active && (
          <>
            <SettingRow
              title="Runtime"
              description={policy.runtimePrepared
                ? "The authenticated Host Work role is ready for new Work sessions."
                : runtimePrerequisite}
            >
              <Button
                variant="outline"
                disabled={busy || (!policy.runtimePrepared && account === null)}
                onClick={() => {
                  if (!policy.runtimePrepared && !canPrepareRuntime) {
                    void navigate("/setup");
                    return;
                  }
                  void mutateRuntime(policy.runtimePrepared ? "deactivate" : "prepare");
                }}
              >
                {policy.runtimePrepared ? "Deactivate runtime" : canPrepareRuntime ? "Prepare Host Tools" : "Open Setup"}
              </Button>
            </SettingRow>
            <SettingRow
              title="Granted scope"
              description={`${policy.pathRoots.length} folder${policy.pathRoots.length === 1 ? "" : "s"} · ${[
                policy.filesystemRead && "read",
                policy.filesystemWrite && "write",
                policy.processExecute && "run programs",
              ].filter(Boolean).join(", ")}`}
            >
              <Button variant="outline" disabled={busy} onClick={() => setDialogOpen(true)}>Review or replace</Button>
            </SettingRow>
            <SettingRow
              title="Revoke Host Tools"
              description="Stops new host tool dispatch immediately. Interrupted side effects remain marked for review."
            >
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="destructive" disabled={busy}>
                    <Trash2 size={15} aria-hidden="true" /> Revoke
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Revoke Host Tools?</AlertDialogTitle>
                    <AlertDialogDescription>
                      New host tool dispatch stops immediately. Any side effect interrupted in progress will require manual review and will not replay automatically.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Keep enabled</AlertDialogCancel>
                    <AlertDialogAction variant="destructive" onClick={() => void mutateRuntime("revoke")}>
                      Revoke Host Tools
                    </AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            </SettingRow>
          </>
        )}
      </SettingsGroup>

      <Dialog open={dialogOpen} onOpenChange={(open) => { if (!busy) setDialogOpen(open); }}>
        <DialogContent className="max-w-[640px]">
          <DialogHeader>
            <DialogTitle>Enable Host Tools · Step {step} of 3</DialogTitle>
            <DialogDescription>
              Host Tools runs with your user account on this computer and does not have the utility-guest boundary.
            </DialogDescription>
          </DialogHeader>

          {step === 1 && (
            <div className="space-y-3 text-body text-muted-foreground">
              <p className="m-0">Files, pages, tool metadata, and model output can contain prompt injection. Approved programs can access the network as your user.</p>
              <p className="m-0">A malicious or mistaken instruction could modify files, run programs, or disclose data inside the scope you grant.</p>
              <p className="m-0 rounded-lg border border-warning/30 bg-warning-soft p-3 text-warning">Isolated Work is recommended when available. You can revoke Host Tools at any time.</p>
            </div>
          )}

          {step === 2 && (
            <div className="space-y-4">
              <div>
                <div className="mb-2 flex items-center justify-between gap-3">
                  <strong className="text-body font-semibold">Allowed folders</strong>
                  <Button variant="outline" onClick={() => void selectFolder()} disabled={roots.length >= 8}>
                    <FolderOpen size={15} aria-hidden="true" /> Add folder
                  </Button>
                </div>
                {roots.length === 0 ? (
                  <p className="m-0 rounded-md bg-muted p-3 text-body-sm text-muted-foreground">Choose at least one folder. The daemon will canonicalize and validate it.</p>
                ) : roots.map((root) => (
                  <div key={root} className="mb-2 flex items-center gap-2 rounded-md border border-border px-3 py-2 font-mono text-label">
                    <span className="min-w-0 flex-1 truncate" title={root}>{root}</span>
                    <Button variant="ghost" size="sm" onClick={() => setRoots(roots.filter((item) => item !== root))}>Remove</Button>
                  </div>
                ))}
              </div>
              <div className="grid gap-3 rounded-lg border border-border p-3">
                {[
                  { id: "host-tools-read", label: "Read files and list folders", checked: filesystemRead, setter: setFilesystemRead },
                  { id: "host-tools-write", label: "Write or replace files (approval each time)", checked: filesystemWrite, setter: setFilesystemWrite },
                  { id: "host-tools-process", label: "Run any program as you (approval each time)", checked: processExecute, setter: setProcessExecute },
                ].map(({ id, label, checked, setter }) => (
                  <label key={id} htmlFor={id} className="flex items-start gap-3 text-body">
                    <Checkbox id={id} checked={checked} onCheckedChange={(value) => setter(value === true)} />
                    <span>{label}</span>
                  </label>
                ))}
              </div>
              <label htmlFor="host-tools-broad-scope" className="flex items-start gap-3 text-body text-muted-foreground">
                <Checkbox id="host-tools-broad-scope" checked={broadScopeAcknowledged} onCheckedChange={(value) => setBroadScopeAcknowledged(value === true)} />
                <span>I understand that selecting my home folder or an entire drive grants access across that broad scope.</span>
              </label>
            </div>
          )}

          {step === 3 && (
            <div className="space-y-3">
              <p className="m-0 text-body text-muted-foreground">Type this exact phrase to confirm:</p>
              <code className="block rounded-md bg-muted p-3 font-mono text-body-sm text-foreground">{HOST_ACKNOWLEDGMENT_PHRASE}</code>
              <label htmlFor="host-tools-ack" className="text-body font-semibold">Acknowledgment</label>
              <Input id="host-tools-ack" value={typedAcknowledgment} onChange={(event) => setTypedAcknowledgment(event.target.value)} autoComplete="off" />
            </div>
          )}

          <DialogFooter>
            {step > 1 && <Button variant="outline" disabled={busy} onClick={() => setStep((step - 1) as 1 | 2)}>Back</Button>}
            {step < 3 ? (
              <Button
                disabled={step === 2 && (roots.length === 0 || (!filesystemRead && !filesystemWrite && !processExecute))}
                onClick={() => setStep((step + 1) as 2 | 3)}
              >Continue</Button>
            ) : (
              <Button disabled={busy || typedAcknowledgment.trim() !== HOST_ACKNOWLEDGMENT_PHRASE} onClick={() => void enroll()}>
                Enable Host Tools
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
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
            Host authentication is managed in Setup. Work requires either qualified isolation or explicitly enrolled and prepared Host Tools.
          </p>
          <Badge className="mt-2" variant="neutral">
            <ShieldCheck size={13} aria-hidden="true" /> Use Setup
          </Badge>
        </div>
        <Button variant="outline" className="max-[680px]:w-full" onClick={() => navigate("/setup")}>
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
          <Button variant="outline" className="max-[680px]:w-full" onClick={() => navigate("/setup?step=api")}>Manage SuperGrok</Button>
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
          <Button variant="outline" className="max-[680px]:w-full" onClick={() => navigate("/setup?step=api")}>
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
        updateChannel: previous.updateChannel,
      });
      setPreferences(updated);
    } catch {
      setPreferences(previous);
      setPreferenceError("Close behavior was not saved. The previous setting has been restored; try again.");
    } finally {
      setSavingPreference(false);
    }
  };

  const updateChannel = async (channel: "stable" | "beta") => {
    if (!preferences || savingPreference || channel === preferences.updateChannel) return;
    const previous = preferences;
    setPreferenceError("");
    setSavingPreference(true);
    setPreferences({ ...previous, updateChannel: channel });
    try {
      const updated = await client.updateDesktopPreferences({
        expectedRevision: previous.revision,
        keepRunningInNotificationArea: previous.keepRunningInNotificationArea,
        updateChannel: channel,
      });
      setPreferences(updated);
      setUpdate(await client.getUpdateState());
    } catch {
      setPreferences(previous);
      setPreferenceError("Update channel was not saved. The previous setting has been restored; try again.");
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
            <Switch
              checked={preferences?.keepRunningInNotificationArea ?? true}
              disabled={!preferences || savingPreference}
              onCheckedChange={(checked) => void updateCloseBehavior(checked)}
              aria-label="Keep running in notification area"
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
          title="Update channel"
          description="Stable receives production releases. Beta receives signed previews and may change more frequently."
        >
          <Select
            value={preferences?.updateChannel ?? "stable"}
            disabled={!preferences || savingPreference || update?.phase === "downloaded"}
            onValueChange={(value) => void updateChannel(value as "stable" | "beta")}
          >
            <SelectTrigger className="h-9 w-32" aria-label="Update channel">
              <SelectValue />
            </SelectTrigger>
            <SelectContent position="popper" align="end">
              <SelectItem value="stable">Stable</SelectItem>
              <SelectItem value="beta">Beta</SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title="Application updates"
          description={updateDescription(update)}
        >
          {update?.phase === "downloaded" ? (
            <Button variant="outline" onClick={() => void client.installUpdate()}>Restart to update</Button>
          ) : (
            <Button
              disabled={!update || update.phase === "unsupported" || checkingUpdate || update.phase === "checking"}
              onClick={() => void checkForUpdates()}
              variant="outline"
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
  if (update.phase === "checking") return `Version ${update.currentVersion}. Checking the ${update.channel} channel.`;
  if (update.phase === "available") return `Downloading version ${update.targetVersion || "the latest release"}.`;
  if (update.phase === "downloaded") return `Version ${update.targetVersion || "the latest release"} is ready to install.`;
  if (update.phase === "failed") return `Version ${update.currentVersion}. The update check failed; try again.`;
  if (update.phase === "not_available") return `Version ${update.currentVersion} is current on the ${update.channel} channel.`;
  return `Version ${update.currentVersion}. Automatic ${update.channel}-channel updates are enabled.`;
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
          <Button variant="outline" className="max-[680px]:w-full" onClick={() => void refreshCatalog()} disabled={loading}>
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
          <Button variant="outline" className="max-[680px]:w-full" onClick={() => void refreshCatalog()} disabled={loading || saving}>
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
            <Button className="mt-2" variant="outline" onClick={() => void load()}>
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
