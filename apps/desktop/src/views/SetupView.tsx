import { useEffect, useState, type ReactNode } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Check,
  CircleAlert,
  ExternalLink,
  KeyRound,
  LoaderCircle,
  ShieldCheck,
  Sparkles,
  Trash2,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import { Badge } from "@/components/ui/badge";
import { Button as ButtonPrimitive } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import { Button, PageHeader } from "../components/ui";
import { useDesktopClient } from "../services/DesktopClientContext";
import type { AccountSetupState, ReadinessCheck } from "../services/desktopClient";
import { GROK_BUILD_AUTH_UNAVAILABLE_REASON } from "../services/productAvailability";

type SetupStep = "grok" | "api" | "readiness";

const SETUP_STEPS: ReadonlyArray<{
  id: SetupStep;
  label: string;
  detail: string;
}> = [
  { id: "grok", label: "Grok account", detail: "Official Grok Build OAuth" },
  { id: "api", label: "xAI API key", detail: "Optional direct API access" },
  { id: "readiness", label: "Readiness", detail: "Capabilities and Limited Mode" },
];

const READINESS_PRESENTATION = {
  ready: {
    label: "Ready",
    badge: "success" as const,
    iconClassName: "bg-success-soft text-success",
  },
  optional: {
    label: "Optional",
    badge: "neutral" as const,
    iconClassName: "bg-muted text-muted-foreground",
  },
  action_required: {
    label: "Action required",
    badge: "warning" as const,
    iconClassName: "bg-warning-soft text-warning",
  },
  unavailable: {
    label: "Unavailable",
    badge: "warning" as const,
    iconClassName: "bg-warning-soft text-warning",
  },
} satisfies Record<
  ReadinessCheck["state"],
  { label: string; badge: "success" | "neutral" | "warning"; iconClassName: string }
>;

export function SetupView() {
  const client = useDesktopClient();
  const navigate = useNavigate();
  const [step, setStep] = useState<SetupStep>("grok");
  const [setup, setSetup] = useState<AccountSetupState | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");

  useEffect(() => {
    let active = true;
    void client
      .getAccountSetup()
      .then((value) => {
        if (active) setSetup(value);
      })
      .catch((error: unknown) => {
        if (active) {
          setNotice(error instanceof Error ? error.message : "Account readiness could not be loaded.");
        }
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [client]);

  const enrollKey = async () => {
    setBusy(true);
    setNotice("");
    try {
      const result = await client.enrollXaiApiKey();
      if (result.status === "success") {
        setSetup(result.value);
        setNotice("xAI API key stored in the operating system vault.");
      } else {
        setNotice(result.reason);
      }
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "The xAI API key could not be added.");
    } finally {
      setBusy(false);
    }
  };

  const removeKey = async () => {
    setBusy(true);
    setNotice("");
    try {
      const result = await client.deleteXaiApiKey();
      if (result.status === "success") {
        setSetup(result.value);
        setConfirmDelete(false);
        setNotice("xAI API key removed from the operating system vault.");
      } else {
        setNotice(result.reason);
      }
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "The xAI API key could not be removed.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="min-h-full overflow-y-auto px-[clamp(24px,3.2vw,48px)] pt-8 pb-11 max-[680px]:px-4 max-[680px]:pt-6 max-[680px]:pb-8">
      <div className="mx-auto max-w-[1180px]">
        <PageHeader
          title="Set up Grok Desktop"
          description="Connect official Grok access and review local readiness."
          actions={
            <Button variant="ghost" onClick={() => navigate(-1)}>
              <ArrowLeft size={15} aria-hidden="true" /> Back
            </Button>
          }
        />

        <div className="grid grid-cols-[15rem_minmax(0,1fr)] gap-6 max-[900px]:grid-cols-[12rem_minmax(0,1fr)] max-[680px]:block">
          <nav
            className="flex min-w-0 flex-col gap-1 max-[680px]:mb-4 max-[680px]:flex-row max-[680px]:overflow-x-auto max-[680px]:pb-1"
            aria-label="Setup progress"
          >
            {SETUP_STEPS.map((item, index) => {
              const active = step === item.id;
              const complete =
                (item.id === "grok" && setup?.grokBuild === "connected") ||
                (item.id === "api" && setup?.xaiApiKey === "configured");

              return (
                <button
                  key={item.id}
                  type="button"
                  disabled={busy}
                  aria-current={active ? "step" : undefined}
                  className={cn(
                    "grid min-h-16 min-w-0 grid-cols-[28px_minmax(0,1fr)_16px] items-center gap-3 rounded-lg border border-transparent px-3 py-2 text-left text-muted-foreground outline-none",
                    "transition-[background-color,border-color,color,transform,box-shadow] duration-150 ease-fluid hover:bg-muted active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-ring disabled:active:scale-100",
                    "max-[680px]:min-w-48 max-[680px]:shrink-0",
                    active && "border-input bg-card text-foreground shadow-raised hover:bg-card",
                  )}
                  onClick={() => setStep(item.id)}
                >
                  <span
                    className={cn(
                      "grid size-7 place-items-center rounded-full bg-secondary font-mono text-label font-semibold tabular-nums text-muted-foreground",
                      active && "bg-primary text-primary-foreground",
                    )}
                    aria-hidden="true"
                  >
                    {index + 1}
                  </span>
                  <span className="flex min-w-0 flex-col gap-0.5">
                    <strong className="text-body font-semibold text-current">{item.label}</strong>
                    <span className={cn("text-label leading-4", active ? "text-muted-foreground" : "text-subtle-foreground")}>
                      {item.detail}
                    </span>
                  </span>
                  <span className="grid size-4 place-items-center text-success">
                    {complete ? (
                      <>
                        <Check size={15} aria-hidden="true" />
                        <span className="sr-only">Complete</span>
                      </>
                    ) : null}
                  </span>
                </button>
              );
            })}
          </nav>

          <section
            className="min-h-[536px] min-w-0 rounded-lg border border-border bg-card p-12 max-[900px]:p-8 max-[680px]:min-h-[504px] max-[680px]:p-4"
            aria-busy={loading || busy}
            aria-labelledby={`${step}-setup-heading`}
          >
            {step === "grok" && (
              <div className="max-w-[65ch]">
                <SetupHeading icon={<Sparkles size={22} />} id="grok-setup-heading" title="Connect Grok Build" />
                <p className="m-0 text-body-lg text-muted-foreground">
                  Subscription access is owned by the official Grok Build client. Grok Desktop does not collect browser
                  cookies or embed an unofficial OAuth client.
                </p>

                {loading ? (
                  <CredentialStatusSkeleton label="Loading Grok account status" />
                ) : setup?.grokBuild === "connected" ? (
                  <SuccessNotice
                    title="Grok account connected"
                    detail="The daemon verified the official subscription session."
                  />
                ) : (
                  <>
                    <Button
                      className="mt-6"
                      variant="primary"
                      disabled={busy || setup?.checks.find((check) => check.id === "grok_auth")?.state === "unavailable"}
                      title={
                        setup?.checks.find((check) => check.id === "grok_auth")?.state === "unavailable"
                          ? GROK_BUILD_AUTH_UNAVAILABLE_REASON
                          : "Authenticate with the official Grok Build component"
                      }
                      onClick={() => {
                        void (async () => {
                          setBusy(true);
                          setNotice("");
                          try {
                            const begin = await client.beginGrokBuildAuth();
                            if (begin.status !== "success") {
                              setNotice(begin.reason ?? GROK_BUILD_AUTH_UNAVAILABLE_REASON);
                              return;
                            }
                            const complete = await client.completeGrokBuildAuth();
                            if (complete.status === "success") {
                              setSetup(complete.value);
                              setNotice("Grok Build host authentication completed.");
                            } else {
                              setNotice(complete.reason ?? GROK_BUILD_AUTH_UNAVAILABLE_REASON);
                            }
                          } catch (caught) {
                            setNotice(
                              caught instanceof Error ? caught.message : GROK_BUILD_AUTH_UNAVAILABLE_REASON,
                            );
                          } finally {
                            setBusy(false);
                          }
                        })();
                      }}
                    >
                      <ExternalLink size={15} aria-hidden="true" />
                      {setup?.checks.find((check) => check.id === "grok_auth")?.state === "unavailable"
                        ? "Connection unavailable"
                        : "Connect Grok Build"}
                    </Button>
                    <p className="mt-3 mb-0 text-body-sm text-muted-foreground" role="status">
                      {setup?.checks.find((check) => check.id === "grok_auth")?.detail
                        ?? GROK_BUILD_AUTH_UNAVAILABLE_REASON}
                    </p>
                  </>
                )}

                <Button className="mt-6 px-0" variant="ghost" onClick={() => setStep("api")}>
                  Continue in Limited Mode <ArrowRight size={14} aria-hidden="true" />
                </Button>
              </div>
            )}

            {step === "api" && (
              <div className="max-w-[65ch]">
                <SetupHeading icon={<KeyRound size={22} />} id="api-setup-heading" title="xAI API key" />
                <p className="m-0 text-body-lg text-muted-foreground">
                  Direct xAI capabilities require a user-owned key stored through a native secure credential prompt.
                  Subscription OAuth remains separate.
                </p>

                {loading ? (
                  <CredentialStatusSkeleton label="Loading credential status" />
                ) : setup?.xaiApiKey === "configured" ? (
                  <>
                    <SuccessNotice
                      title="xAI API key configured"
                      detail="The credential is held by the operating system vault."
                    />
                    <div className="mt-4 flex gap-2">
                      <Button variant="danger" disabled={busy} onClick={() => setConfirmDelete(true)}>
                        <Trash2 size={15} aria-hidden="true" /> Remove key
                      </Button>
                    </div>
                  </>
                ) : (
                  <div className="mt-6 flex gap-2">
                    <Button
                      variant="primary"
                      loading={busy}
                      disabled={busy || !setup}
                      onClick={() => void enrollKey()}
                    >
                      <KeyRound size={15} aria-hidden="true" /> Add xAI API key
                    </Button>
                  </div>
                )}

                <Button className="mt-6 px-0" variant="ghost" disabled={busy} onClick={() => setStep("readiness")}>
                  Continue to readiness <ArrowRight size={14} aria-hidden="true" />
                </Button>
              </div>
            )}

            {step === "readiness" && (
              <div>
                <SetupHeading icon={<ShieldCheck size={22} />} id="readiness-setup-heading" title="Capability readiness" />
                <p className="m-0 max-w-[65ch] text-body-lg text-muted-foreground">
                  Unavailable capabilities remain disabled. Grok Desktop never substitutes unsandboxed host execution.
                </p>

                {loading ? (
                  <ReadinessSkeleton />
                ) : setup ? (
                  <div className="mt-6 overflow-hidden rounded-lg border border-border" role="list">
                    {setup.checks.map((check) => (
                      <ReadinessItem key={check.id} check={check} />
                    ))}
                  </div>
                ) : null}

                {setup?.limitedMode && (
                  <div className="mt-4 flex gap-3 rounded-lg border border-warning/25 bg-warning-soft p-4 text-warning">
                    <ShieldCheck className="mt-0.5 shrink-0" size={18} aria-hidden="true" />
                    <div>
                      <strong className="text-body font-semibold">Limited Mode is active</strong>
                      <p className="mt-1 mb-0 max-w-[65ch] text-body-sm text-muted-foreground">
                        Local projects, drafts, files, and history remain available. Work, tools, and other unavailable
                        capabilities stay disabled.
                      </p>
                    </div>
                  </div>
                )}

                <Button className="mt-6" variant="primary" onClick={() => navigate("/")}>
                  Open Grok Desktop <ArrowRight size={15} aria-hidden="true" />
                </Button>
              </div>
            )}

            {notice && (
              <p className="mt-6 mb-0 border-t border-border pt-4 text-body-sm text-warning" role="status" aria-live="polite">
                {notice}
              </p>
            )}
          </section>
        </div>
      </div>

      {confirmDelete && (
        <DeleteApiKeyDialog
          busy={busy}
          onClose={() => {
            if (!busy) setConfirmDelete(false);
          }}
          onConfirm={() => void removeKey()}
        />
      )}
    </div>
  );
}

function SetupHeading({ icon, id, title }: { icon: ReactNode; id: string; title: string }) {
  return (
    <>
      <span className="mb-5 grid size-12 place-items-center rounded-lg bg-accent text-primary" aria-hidden="true">
        {icon}
      </span>
      <h2 className="mt-0 mb-2 text-title-lg font-semibold text-foreground" id={id}>
        {title}
      </h2>
    </>
  );
}

function SuccessNotice({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="mt-6 flex max-w-[65ch] items-start gap-3 rounded-lg border border-success/25 bg-success-soft p-4 text-success">
      <Check className="mt-0.5 shrink-0" size={17} aria-hidden="true" />
      <span className="flex flex-col gap-0.5">
        <strong className="text-body font-semibold">{title}</strong>
        <span className="text-body-sm text-muted-foreground">{detail}</span>
      </span>
    </div>
  );
}

function CredentialStatusSkeleton({ label }: { label: string }) {
  return (
    <div
      className="mt-6 flex max-w-[65ch] items-center gap-3 rounded-lg border border-border bg-muted p-4"
      role="status"
      aria-label={label}
    >
      <Skeleton className="size-5 shrink-0" />
      <div className="flex flex-1 flex-col gap-2">
        <Skeleton className="h-3 w-40" />
        <Skeleton className="h-3 w-64 max-w-full" />
      </div>
    </div>
  );
}

function ReadinessSkeleton() {
  return (
    <div
      className="mt-6 overflow-hidden rounded-lg border border-border"
      role="status"
      aria-label="Loading capability readiness"
    >
      {Array.from({ length: 4 }, (_, index) => (
        <div className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)_80px] items-center gap-3 border-b border-border p-3 last:border-b-0" key={index}>
          <Skeleton className="size-8" />
          <div className="flex flex-col gap-2">
            <Skeleton className="h-3 w-36" />
            <Skeleton className="h-3 w-64 max-w-full" />
          </div>
          <Skeleton className="h-5 w-20 rounded-full" />
        </div>
      ))}
    </div>
  );
}

function ReadinessItem({ check }: { check: ReadinessCheck }) {
  const presentation = READINESS_PRESENTATION[check.state];
  return (
    <div
      className="grid min-h-16 grid-cols-[32px_minmax(0,1fr)_auto] items-center gap-3 border-b border-border p-3 last:border-b-0 max-[680px]:grid-cols-[32px_minmax(0,1fr)]"
      role="listitem"
    >
      <span className={cn("grid size-8 place-items-center rounded-md", presentation.iconClassName)} aria-hidden="true">
        {check.state === "ready" ? <Check size={16} /> : <CircleAlert size={16} />}
      </span>
      <span className="flex min-w-0 flex-col gap-0.5">
        <strong className="text-body font-semibold text-foreground">{check.label}</strong>
        <span className="text-body-sm text-muted-foreground">{check.detail}</span>
      </span>
      <Badge className="max-[680px]:col-start-2 max-[680px]:justify-self-start" variant={presentation.badge}>
        {presentation.label}
      </Badge>
    </div>
  );
}

function DeleteApiKeyDialog({ busy, onClose, onConfirm }: { busy: boolean; onClose: () => void; onConfirm: () => void }) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        className="max-w-[480px]"
        showCloseButton={!busy}
        onEscapeKeyDown={(event) => {
          if (busy) event.preventDefault();
        }}
        onInteractOutside={(event) => {
          if (busy) event.preventDefault();
        }}
      >
        <DialogHeader>
          <DialogTitle>Remove xAI API key?</DialogTitle>
          <DialogDescription>Direct xAI capability readiness will be removed immediately.</DialogDescription>
        </DialogHeader>
        <p className="m-0 text-body text-muted-foreground">
          The key will be deleted from the operating system credential vault. Local projects and Grok Build files are not
          affected.
        </p>
        <DialogFooter className="mt-2 max-[480px]:flex-col-reverse max-[480px]:[&>*]:w-full">
          <ButtonPrimitive variant="outline" disabled={busy} onClick={onClose}>
            Cancel
          </ButtonPrimitive>
          <ButtonPrimitive variant="destructive-solid" disabled={busy} onClick={onConfirm}>
            {busy ? (
              <LoaderCircle size={15} className="animate-spin" aria-hidden="true" />
            ) : (
              <Trash2 size={15} aria-hidden="true" />
            )}
            Remove key
          </ButtonPrimitive>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
