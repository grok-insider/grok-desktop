import { forwardRef, useCallback, useEffect, useRef, useState } from "react";
import { ArrowUp, AtSign, Check, ChevronDown, CircleAlert, Mic, RefreshCw } from "lucide-react";
import { useNavigate } from "react-router-dom";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { StartRunInput } from "../services/desktopClient";
import { useChatModelCatalog } from "../hooks/useChatModelCatalog";
import { IconButton } from "./ui";
import { VoiceOverlay } from "./VoiceOverlay";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const modeTabClass = (active: boolean) =>
  cn(
    "h-7 min-w-[69px] rounded-md px-3 text-body-sm font-semibold transition-colors duration-150",
    active ? "bg-card text-foreground shadow-raised" : "text-muted-foreground hover:text-foreground",
  );

export function Composer() {
  const client = useDesktopClient();
  const { snapshot } = useDesktopSnapshot();
  const navigate = useNavigate();
  const [mode, setMode] = useState<"chat" | "work">("chat");
  const [prompt, setPrompt] = useState("");
  const [projectId, setProjectId] = useState(snapshot?.projects[0]?.id ?? "");
  const [recording, setRecording] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [announcement, setAnnouncement] = useState("");
  const [error, setError] = useState("");
  const [modelOverride, setModelOverride] = useState<string | undefined>();
  const models = useChatModelCatalog();

  useEffect(() => {
    if (!projectId && snapshot?.projects[0]) setProjectId(snapshot.projects[0].id);
  }, [projectId, snapshot]);

  const capabilitiesLoaded = (snapshot?.capabilities.length ?? 0) > 0;
  const interfacePreview = snapshot?.connection.interfacePreview === true;
  const modeCapability = snapshot?.capabilities.find((item) => item.id === mode);
  const modeAvailable = capabilitiesLoaded && modeCapability?.available === true;
  const workCapability = snapshot?.capabilities.find((item) => item.id === "work");
  const workAvailable = capabilitiesLoaded && workCapability?.available === true;

  const submit = async () => {
    const value = prompt.trim();
    if (!value || submitting) return;
    setSubmitting(true);
    setError("");
    try {
      const input: StartRunInput & { modelId?: string } = {
        prompt: value,
        mode,
        projectId: projectId || undefined,
        searchEnabled: false,
        researchEnabled: false,
        ...(mode === "chat" && modelOverride ? { modelId: modelOverride } : {}),
      };
      const { runId, threadId } = await client.startRun(input);
      setPrompt("");
      if (mode === "chat") setModelOverride(undefined);
      setAnnouncement(mode === "work" ? "Work started" : "Conversation started");
      if (mode === "work") navigate(`/activity?run=${runId}`);
      else navigate(`/conversations/${threadId}`);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "The request could not be started.");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <section className="w-full" aria-label="Start a conversation">
      <div className="mb-2 ml-1 flex w-fit gap-0.5 rounded-lg bg-secondary p-[3px]" role="tablist" aria-label="Conversation mode">
        <button role="tab" aria-selected={mode === "chat"} className={modeTabClass(mode === "chat")} onClick={() => setMode("chat")}>
          Chat
        </button>
        <button
          role="tab"
          aria-selected={mode === "work"}
          className={modeTabClass(mode === "work")}
          disabled={!workAvailable}
          title={!workAvailable ? workCapability?.reason : undefined}
          onClick={() => setMode("work")}
        >
          Work
        </button>
      </div>
      <div className="min-h-[148px] rounded-xl border border-input bg-card px-3.5 pt-3.5 pb-2.5 shadow-overlay transition-[border-color,box-shadow] duration-150 focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring">
        <label htmlFor="main-prompt" className="sr-only">Message Grok</label>
        <textarea
          id="main-prompt"
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
          placeholder={mode === "chat" ? "Ask Grok anything" : "Describe a task for Grok to complete"}
          rows={3}
          className="block min-h-[72px] w-full resize-none border-0 bg-transparent px-1 py-0.5 text-body-lg text-foreground outline-none placeholder:text-subtle-foreground"
        />
        <div className="flex items-center justify-between gap-3 border-t border-border/70 pt-2">
          <div className="flex items-center gap-1" />
          <div className="flex items-center gap-1">
            <ComposerModelMenu
              disabled={mode !== "chat" || submitting}
              overrideModelId={modelOverride}
              onOverrideChange={setModelOverride}
              {...models}
            />
            <IconButton
              label={recording ? "Stop voice input" : "Start voice input"}
              className={cn(recording && "border-destructive/25 bg-destructive-soft text-destructive")}
              onClick={() => setRecording((value) => !value)}
            >
              <Mic size={18} />
            </IconButton>
            <Button
              size="icon"
              disabled={!prompt.trim() || submitting || !modeAvailable || !projectId}
              aria-label="Send message"
              onClick={() => void submit()}
            >
              <ArrowUp size={18} />
            </Button>
          </div>
        </div>
      </div>
      <div className="flex min-h-[29px] items-center justify-between gap-3 px-1 pt-1.5 font-mono text-label text-subtle-foreground">
        <label className="flex items-center gap-1">
          <AtSign size={14} aria-hidden="true" />
          <select
            value={projectId}
            onChange={(event) => setProjectId(event.target.value)}
            aria-label="Project"
            className="max-w-[170px] rounded-sm border-0 bg-transparent font-mono text-label text-muted-foreground"
          >
            <option value="">No project</option>
            {snapshot?.projects.map((project) => <option value={project.id} key={project.id}>{project.name}</option>)}
          </select>
        </label>
        <span className="flex items-center gap-1 max-[680px]:hidden">
          <CircleAlert size={13} /> {interfacePreview ? "Interface preview only" : modeAvailable ? "Grok execution available" : "Grok execution unavailable"}
        </span>
      </div>
      <span className="sr-only" aria-live="polite">{announcement}</span>
      {!modeAvailable && modeCapability?.reason && (
        <p className="m-0 mx-1 mt-1 flex min-h-[27px] items-center text-label text-warning" role="status">
          {modeCapability.reason}
        </p>
      )}
      {modeAvailable && !projectId && (
        <p className="m-0 mx-1 mt-1 flex min-h-[27px] items-center text-label text-warning" role="status">
          Create or select a project before starting Chat.
        </p>
      )}
      {error && (
        <p className="m-0 mx-1 mt-1 flex min-h-[27px] items-center text-label text-destructive" role="alert">
          {error}
        </p>
      )}
      {recording && <VoiceOverlay onClose={() => setRecording(false)} />}
    </section>
  );
}

function ComposerModelMenu({
  catalog,
  status,
  error,
  retry,
  setDefaultModel,
  savingModelId,
  selectableModels,
  overrideModelId,
  onOverrideChange,
  disabled,
}: ReturnType<typeof useChatModelCatalog> & {
  overrideModelId: string | undefined;
  onOverrideChange(modelId: string | undefined): void;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const firstItemRef = useRef<HTMLButtonElement>(null);
  const defaultModelId = catalog?.preference.selectedModelId;
  const visibleLabel = overrideModelId ?? (defaultModelId ? `Default · ${defaultModelId}` : "Choose model");
  const closeAndRestoreFocus = useCallback(() => {
    setOpen(false);
    queueMicrotask(() => triggerRef.current?.focus());
  }, []);

  useEffect(() => {
    if (!open) return;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeAndRestoreFocus();
      }
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [closeAndRestoreFocus, open]);

  useEffect(() => {
    if (open) firstItemRef.current?.focus();
  }, [open]);

  return (
    <div className="relative" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className="inline-flex h-[30px] max-w-52 items-center gap-1.5 rounded-md border border-transparent px-2 font-mono text-label font-medium text-muted-foreground transition-colors hover:border-border hover:bg-muted aria-expanded:border-input aria-expanded:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
        aria-label={`Choose model, ${visibleLabel}`}
        aria-haspopup="menu"
        aria-controls="composer-model-menu"
        aria-expanded={open}
        disabled={disabled}
        title={disabled ? "Model overrides apply to Chat conversations only" : undefined}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="truncate">{visibleLabel}</span>
        <ChevronDown className="shrink-0" size={14} aria-hidden="true" />
      </button>
      {open && (
        <div
          id="composer-model-menu"
          ref={menuRef}
          className="absolute right-0 bottom-[calc(100%+8px)] z-30 w-[min(360px,calc(100vw-32px))] overflow-hidden rounded-xl border border-input bg-popover shadow-overlay"
          role="menu"
          tabIndex={-1}
          aria-label="Chat model"
          onKeyDown={(event) => {
            if (!matchesMenuNavigationKey(event.key)) return;
            const items = [...(menuRef.current?.querySelectorAll<HTMLButtonElement>("[role^='menuitem']:not(:disabled)") ?? [])];
            if (items.length === 0) return;
            event.preventDefault();
            const current = items.indexOf(document.activeElement as HTMLButtonElement);
            const next = event.key === "Home"
              ? 0
              : event.key === "End"
                ? items.length - 1
                : event.key === "ArrowDown"
                  ? current < 0 ? 0 : (current + 1) % items.length
                  : current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length;
            items[next]?.focus();
          }}
        >
          <div className="border-b border-border px-3 py-2">
            <p className="m-0 text-body-sm font-semibold text-foreground">Model for this conversation</p>
            <p className="m-0 mt-0.5 text-label text-muted-foreground">A temporary choice clears after the conversation starts.</p>
          </div>
          {catalog && (
            <div className="max-h-64 overflow-y-auto p-1.5">
              <ModelChoice
                ref={firstItemRef}
                label={`Default · ${defaultModelId}`}
                selected={!overrideModelId}
                onChoose={() => {
                  onOverrideChange(undefined);
                  closeAndRestoreFocus();
                }}
              />
              {selectableModels.map((model) => (
                <div className="group flex items-center gap-1 rounded-md hover:bg-muted" key={model.id} role="none">
                  <ModelChoice
                    label={model.id}
                    selected={overrideModelId === model.id}
                    disabled={status !== "ready"}
                    onChoose={() => {
                      onOverrideChange(model.id);
                      closeAndRestoreFocus();
                    }}
                  />
                  {model.id !== defaultModelId && (
                    <button
                      type="button"
                      role="menuitem"
                      aria-label={`Set ${model.id} as default`}
                      className="mr-1 shrink-0 rounded-md px-2 py-1.5 text-label font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
                      disabled={savingModelId !== null || status !== "ready"}
                      onClick={() => void setDefaultModel(model.id)}
                    >
                      {savingModelId === model.id ? "Saving…" : "Set as default"}
                    </button>
                  )}
                </div>
              ))}
            </div>
          )}
          {!catalog && status === "loading" && (
            <p className="m-0 px-3 py-4 text-body-sm text-muted-foreground" role="status">Discovering available models…</p>
          )}
          {status === "error" && (
            <div className="border-t border-warning/20 bg-warning-soft p-3" role="alert">
              <p className="m-0 text-body-sm text-warning">{error}</p>
              <Button className="mt-2" size="sm" variant="outline" onClick={() => void retry()}>
                <RefreshCw size={14} aria-hidden="true" /> Retry
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function matchesMenuNavigationKey(key: string): key is "ArrowDown" | "ArrowUp" | "Home" | "End" {
  return key === "ArrowDown" || key === "ArrowUp" || key === "Home" || key === "End";
}

const ModelChoice = forwardRef<HTMLButtonElement, {
  label: string;
  selected: boolean;
  onChoose(): void;
  disabled?: boolean;
}>(function ModelChoice({ label, selected, onChoose, disabled = false }, ref) {
  return (
    <button
      ref={ref}
      type="button"
      role="menuitemradio"
      aria-checked={selected}
      disabled={disabled}
      className="flex min-h-[34px] min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left font-mono text-body-sm text-foreground hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
      onClick={onChoose}
    >
      <span className="flex size-4 shrink-0 items-center justify-center" aria-hidden="true">
        {selected && <Check size={14} />}
      </span>
      <span className="truncate">{label}</span>
    </button>
  );
});
