import { useEffect, useRef, useState } from "react";
import { AudioLines, CircleAlert, Headphones, Mic, MicOff, PhoneOff, RotateCcw, X } from "lucide-react";
import { useNavigate } from "react-router-dom";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useDesktopClient } from "../services/DesktopClientContext";
import type { VoiceSession, VoiceSetup } from "../services/desktopClient";

const stateLabels: Record<VoiceSession["state"], string> = {
  connecting: "Connecting",
  listening: "Listening",
  speaking: "Grok is speaking",
  interrupted: "Interrupted",
  ended: "Session ended",
};

function VoiceStateMark({ state }: { state: VoiceSession["state"] | "checking" }) {
  const interrupted = state === "interrupted";
  const speaking = state === "speaking";
  const Icon = interrupted ? MicOff : speaking ? AudioLines : Mic;

  return (
    <span
      className={cn(
        "grid size-16 place-items-center rounded-full bg-info-soft text-info",
        speaking && "bg-success-soft text-success",
        interrupted && "bg-warning-soft text-warning",
      )}
      aria-hidden="true"
    >
      <Icon size={26} />
    </span>
  );
}

export function VoiceOverlay({ onClose }: { onClose(): void }) {
  const client = useDesktopClient();
  const navigate = useNavigate();
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const returnTargetRef = useRef<HTMLElement | null>(
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );
  const [setup, setSetup] = useState<VoiceSetup | null>(null);
  const [session, setSession] = useState<VoiceSession | null>(null);
  const [input, setInput] = useState("");
  const [output, setOutput] = useState("");
  const [busy, setBusy] = useState(true);
  const [reason, setReason] = useState("");

  useEffect(() => {
    let active = true;
    void client
      .getVoiceSetup()
      .then(async (value) => {
        if (!active) return;
        setSetup(value);
        setInput(value.selectedInputId);
        setOutput(value.selectedOutputId);
        if (value.capability !== "ready") {
          setReason(value.reason ?? "Voice is unavailable.");
          return;
        }

        const result = await client.startVoiceSession(value.selectedInputId, value.selectedOutputId);
        if (!active) return;
        if (result.status === "success") setSession(result.value);
        else setReason(result.reason);
      })
      .catch((error: unknown) => {
        if (active) setReason(error instanceof Error ? error.message : "Voice status could not be loaded.");
      })
      .finally(() => {
        if (active) setBusy(false);
      });

    return () => {
      active = false;
    };
  }, [client]);

  useEffect(() => {
    const returnTarget = returnTargetRef.current;
    return () => {
      queueMicrotask(() => returnTarget?.focus());
    };
  }, []);

  const changeState = async (state: "listening" | "interrupted" | "ended") => {
    if (!session) return;
    const result = await client.setVoiceSessionState(session.id, state);
    if (result.status === "success") setSession(result.value);
    else setReason(result.reason);
    if (state === "ended" && result.status === "success") onClose();
  };

  const statusLabel = busy ? "Checking devices" : session ? stateLabels[session.state] : "Unavailable";

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          closeButtonRef.current?.focus();
        }}
        className={cn(
          "top-1/2 flex max-h-[calc(100dvh-32px)] w-[min(560px,calc(100vw-32px))] -translate-y-1/2 flex-col gap-0 overflow-hidden p-0",
          "max-[680px]:inset-0 max-[680px]:h-dvh max-[680px]:max-h-dvh max-[680px]:w-full max-[680px]:translate-x-0 max-[680px]:translate-y-0 max-[680px]:rounded-none max-[680px]:border-0",
        )}
      >
        <DialogHeader className="min-h-[58px] shrink-0 flex-row items-center justify-between gap-4 border-b border-border px-4 py-2 text-left">
          <div className="flex min-w-0 items-center gap-3">
            <AudioLines className="shrink-0 text-muted-foreground" size={18} aria-hidden="true" />
            <div className="min-w-0">
              <DialogTitle>Voice with Grok</DialogTitle>
              <DialogDescription className="font-mono text-label font-medium text-subtle-foreground">
                {statusLabel}
              </DialogDescription>
            </div>
          </div>
          <DialogClose asChild>
            <Button ref={closeButtonRef} variant="ghost" size="icon" aria-label="Close voice" title="Close voice">
              <X size={18} aria-hidden="true" />
            </Button>
          </DialogClose>
        </DialogHeader>

        {busy ? (
          <div className="flex min-h-80 flex-1 flex-col items-center justify-center px-6 py-16 text-center" role="status" aria-live="polite">
            <VoiceStateMark state="checking" />
            <h2 className="mb-1 mt-4 text-title font-semibold text-foreground">Connecting</h2>
            <p className="m-0 max-w-[42ch] text-body text-muted-foreground">
              Checking the daemon’s Voice capability and selected audio devices.
            </p>
          </div>
        ) : reason ? (
          <div className="flex min-h-80 flex-1 flex-col items-center justify-center px-6 py-16 text-center" role="alert">
            <span className="grid size-16 place-items-center rounded-full bg-warning-soft text-warning" aria-hidden="true">
              <CircleAlert size={28} />
            </span>
            <h2 className="mb-1 mt-4 text-title font-semibold text-foreground">
              {setup?.capability === "configuration_required" ? "Voice needs setup" : "Voice unavailable"}
            </h2>
            <p className="mb-5 mt-0 max-w-[48ch] text-body text-muted-foreground">{reason}</p>
            {setup?.capability === "configuration_required" && (
              <Button
                onClick={() => {
                  onClose();
                  navigate("/setup");
                }}
              >
                Open setup
              </Button>
            )}
          </div>
        ) : session ? (
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="flex shrink-0 flex-col items-center px-5 pb-4 pt-6 text-center" role="status" aria-live="polite">
              <VoiceStateMark state={session.state} />
              <h2 className="mb-0 mt-3 text-title font-semibold text-foreground">{stateLabels[session.state]}</h2>
            </div>

            <div
              className="min-h-28 flex-1 overflow-y-auto border-t border-border px-5 py-3"
              aria-label="Live captions"
              aria-live="polite"
            >
              {session.captions.length === 0 ? (
                <p className="m-0 py-5 text-center text-body text-muted-foreground">Captions will appear here.</p>
              ) : (
                <div className="flex flex-col gap-3">
                  {session.captions.map((caption, index) => (
                    <p className="m-0 grid grid-cols-[40px_minmax(0,1fr)] gap-3" key={`${caption.speaker}-${index}`}>
                      <strong className="font-mono text-label font-semibold text-subtle-foreground">
                        {caption.speaker === "you" ? "You" : "Grok"}
                      </strong>
                      <span className="text-body-lg text-foreground">
                        {caption.text}
                        {!caption.final && (
                          <>
                            <span
                              className="ml-1 inline-block h-3 w-0.5 animate-pulse bg-info align-[-1px] motion-reduce:animate-none"
                              aria-hidden="true"
                            />
                            <span className="sr-only"> Streaming</span>
                          </>
                        )}
                      </span>
                    </p>
                  ))}
                </div>
              )}
            </div>

            <div className="grid shrink-0 grid-cols-2 gap-3 border-t border-border bg-muted px-4 py-3 max-[680px]:grid-cols-1">
              <label className="flex min-w-0 flex-col gap-1 font-mono text-label font-medium text-muted-foreground">
                <span className="flex items-center gap-1.5">
                  <Mic size={14} aria-hidden="true" />
                  Microphone
                </span>
                <select
                  className="h-[34px] min-w-0 rounded-md border border-input bg-card px-2 font-sans text-body text-foreground outline-none transition-colors hover:border-input-hover focus-visible:ring-[3px] focus-visible:ring-ring"
                  value={input}
                  onChange={(event) => setInput(event.target.value)}
                >
                  {setup?.inputDevices.map((device) => (
                    <option key={device.id} value={device.id}>
                      {device.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="flex min-w-0 flex-col gap-1 font-mono text-label font-medium text-muted-foreground">
                <span className="flex items-center gap-1.5">
                  <Headphones size={14} aria-hidden="true" />
                  Speakers
                </span>
                <select
                  className="h-[34px] min-w-0 rounded-md border border-input bg-card px-2 font-sans text-body text-foreground outline-none transition-colors hover:border-input-hover focus-visible:ring-[3px] focus-visible:ring-ring"
                  value={output}
                  onChange={(event) => setOutput(event.target.value)}
                >
                  {setup?.outputDevices.map((device) => (
                    <option key={device.id} value={device.id}>
                      {device.label}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <div className="flex shrink-0 justify-center gap-4 px-4 py-4">
              <Button
                variant="outline"
                size="icon"
                className="size-11 rounded-full"
                aria-label={session.state === "interrupted" ? "Resume listening" : "Interrupt"}
                title={session.state === "interrupted" ? "Resume listening" : "Interrupt"}
                onClick={() => void changeState(session.state === "interrupted" ? "listening" : "interrupted")}
              >
                {session.state === "interrupted" ? <RotateCcw size={19} aria-hidden="true" /> : <MicOff size={19} aria-hidden="true" />}
              </Button>
              <Button
                variant="destructive-solid"
                size="icon"
                className="size-11 rounded-full"
                aria-label="End voice session"
                title="End voice session"
                onClick={() => void changeState("ended")}
              >
                <PhoneOff size={19} aria-hidden="true" />
              </Button>
            </div>
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
