import { forwardRef, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  ArrowUp,
  AtSign,
  Check,
  ChevronDown,
  CircleAlert,
  Globe2,
  Image as ImageIcon,
  Mic,
  Plus,
  RefreshCw,
  Video,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
import type { StartRunInput } from "../services/desktopClient";
import { useChatModelCatalog } from "../hooks/useChatModelCatalog";
import { modelDisplayLabel, modelMenuTriggerLabel } from "../lib/modelLabels";
import { IconButton } from "./ui";
import { VoiceOverlay } from "./VoiceOverlay";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type ImagineToolKind = "image" | "video";

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
  const [searchEnabled, setSearchEnabled] = useState(false);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [imagineKind, setImagineKind] = useState<ImagineToolKind | null>(null);
  const [mediaPrompt, setMediaPrompt] = useState("");
  const [aspectRatio, setAspectRatio] = useState("1:1");
  const [duration, setDuration] = useState("6s");
  const [creatingMedia, setCreatingMedia] = useState(false);
  const chatAvailable = snapshot?.capabilities.some(
    (capability) => capability.id === "chat" && capability.available,
  ) === true;
  const models = useChatModelCatalog(chatAvailable);
  const toolsRootRef = useRef<HTMLDivElement>(null);
  const modelOverrideRef = useRef(modelOverride);
  modelOverrideRef.current = modelOverride;

  useEffect(() => {
    if (!projectId && snapshot?.projects[0]) setProjectId(snapshot.projects[0].id);
  }, [projectId, snapshot]);

  useEffect(() => {
    if (!toolsOpen) return;
    const onPointer = (event: PointerEvent) => {
      if (!toolsRootRef.current?.contains(event.target as Node)) setToolsOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setToolsOpen(false);
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [toolsOpen]);

  const capabilitiesLoaded = (snapshot?.capabilities.length ?? 0) > 0;
  const interfacePreview = snapshot?.connection.interfacePreview === true;
  const modeCapability = snapshot?.capabilities.find((item) => item.id === mode);
  const modeAvailable = capabilitiesLoaded && modeCapability?.available === true;
  const workCapability = snapshot?.capabilities.find((item) => item.id === "work");
  const workAvailable = capabilitiesLoaded && workCapability?.available === true;
  const imagineImageCapability = snapshot?.capabilities.find((item) => item.id === "imagine_image");
  const imagineVideoCapability = snapshot?.capabilities.find((item) => item.id === "imagine_video");
  const imagineImageAvailable = capabilitiesLoaded && imagineImageCapability?.available === true;
  const imagineVideoAvailable = capabilitiesLoaded && imagineVideoCapability?.available === true;
  const anyImagineTool = imagineImageAvailable || imagineVideoAvailable;

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
        searchEnabled,
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

  const openImagineTool = (kind: ImagineToolKind) => {
    setToolsOpen(false);
    setImagineKind(kind);
    setMediaPrompt(prompt.trim());
    setError("");
  };

  const createImagine = async () => {
    if (!imagineKind || creatingMedia) return;
    const value = mediaPrompt.trim();
    if (!value) {
      setError("Describe the image or video to create.");
      return;
    }
    // Capture before async work so a concurrent model change cannot be blamed on tools.
    const modelBefore = modelOverrideRef.current;
    setCreatingMedia(true);
    setError("");
    try {
      const result = await client.createMedia({
        kind: imagineKind,
        prompt: value,
        aspectRatio,
        ...(imagineKind === "video" ? { duration } : {}),
      });
      if (result.status !== "success") {
        setError(result.reason);
        return;
      }
      if (modelOverrideRef.current !== modelBefore) {
        // Tools must never change the selected chat model; restore if something else raced.
        setModelOverride(modelBefore);
      }
      setImagineKind(null);
      setMediaPrompt("");
      setAnnouncement(
        imagineKind === "image"
          ? "Image creation started. Open Library to follow progress."
          : "Video creation started. Open Library to follow progress.",
      );
      navigate(imagineKind === "image" ? "/library/images" : "/library/videos");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Media creation failed.");
    } finally {
      setCreatingMedia(false);
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
        {imagineKind && (
          <div
            className="mt-2 rounded-lg border border-border bg-muted/60 px-3 py-2.5"
            aria-label={imagineKind === "image" ? "Imagine image tool" : "Imagine video tool"}
          >
            <div className="mb-2 flex items-center justify-between gap-2">
              <p className="m-0 text-body-sm font-semibold text-foreground">
                {imagineKind === "image" ? "Imagine image" : "Imagine video"}
              </p>
              <button
                type="button"
                className="text-label font-medium text-muted-foreground hover:text-foreground"
                onClick={() => {
                  setImagineKind(null);
                  setError("");
                }}
              >
                Cancel
              </button>
            </div>
            <label htmlFor="imagine-prompt" className="sr-only">
              Media prompt
            </label>
            <textarea
              id="imagine-prompt"
              value={mediaPrompt}
              onChange={(event) => setMediaPrompt(event.target.value)}
              rows={2}
              placeholder={imagineKind === "image" ? "Describe the image to create" : "Describe the video and motion"}
              className="mb-2 block w-full resize-none rounded-md border border-input bg-card px-2 py-1.5 text-body-sm text-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring"
            />
            <div className="flex flex-wrap items-center gap-2">
              <label className="flex items-center gap-1 font-mono text-label text-muted-foreground">
                Aspect
                <select
                  aria-label="Aspect ratio"
                  value={aspectRatio}
                  onChange={(event) => setAspectRatio(event.target.value)}
                  className="h-7 rounded-md border border-input bg-card px-1.5 text-label text-foreground"
                >
                  {["1:1", "16:9", "9:16", "3:2", "2:3"].map((ratio) => (
                    <option key={ratio} value={ratio}>{ratio}</option>
                  ))}
                </select>
              </label>
              {imagineKind === "video" && (
                <label className="flex items-center gap-1 font-mono text-label text-muted-foreground">
                  Duration
                  <select
                    aria-label="Video duration"
                    value={duration}
                    onChange={(event) => setDuration(event.target.value)}
                    className="h-7 rounded-md border border-input bg-card px-1.5 text-label text-foreground"
                  >
                    {["6s", "10s"].map((value) => (
                      <option key={value} value={value}>{value}</option>
                    ))}
                  </select>
                </label>
              )}
              <Button
                className="ml-auto"
                disabled={!mediaPrompt.trim() || creatingMedia}
                onClick={() => void createImagine()}
              >
                {creatingMedia ? "Starting…" : "Create"}
              </Button>
            </div>
            <p className="m-0 mt-2 text-label text-subtle-foreground">
              Uses the Imagine capability. Does not change the chat model for this conversation.
            </p>
          </div>
        )}
        <div className="flex items-center justify-between gap-3 border-t border-border/70 pt-2">
          <div className="relative flex items-center gap-1" ref={toolsRootRef}>
            <IconButton
              label={searchEnabled ? "Disable Search" : "Enable Search"}
              disabled={mode !== "chat" || submitting || !modeAvailable}
              title="Allow this message to use official xAI web and X search"
              className={cn(searchEnabled && "border-border bg-muted text-foreground")}
              aria-pressed={searchEnabled}
              onClick={() => setSearchEnabled((value) => !value)}
            >
              <Globe2 size={18} />
            </IconButton>
            <IconButton
              label="Tools"
              disabled={mode !== "chat" || submitting || !anyImagineTool}
              title={!anyImagineTool ? "Imagine tools require a ready xAI media capability" : "Chat tools"}
              className={cn(toolsOpen && "border-border bg-muted")}
              onClick={() => setToolsOpen((value) => !value)}
            >
              <Plus size={18} />
            </IconButton>
            {toolsOpen && (
              <div
                className="absolute bottom-[calc(100%+8px)] left-0 z-30 min-w-[220px] overflow-hidden rounded-xl border border-input bg-popover p-1.5 shadow-overlay"
                role="menu"
                aria-label="Composer tools"
              >
                <button
                  type="button"
                  role="menuitem"
                  className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-body-sm text-foreground hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
                  disabled={!imagineImageAvailable}
                  title={!imagineImageAvailable ? imagineImageCapability?.reason : undefined}
                  onClick={() => openImagineTool("image")}
                >
                  <ImageIcon size={16} aria-hidden="true" />
                  Imagine image
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-body-sm text-foreground hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
                  disabled={!imagineVideoAvailable}
                  title={!imagineVideoAvailable ? imagineVideoCapability?.reason : undefined}
                  onClick={() => openImagineTool("video")}
                >
                  <Video size={16} aria-hidden="true" />
                  Imagine video
                </button>
              </div>
            )}
          </div>
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

/**
 * The model menu is portaled to <body> with fixed coordinates because the
 * composer lives inside the scrolling <main> region: a CSS-anchored drop-up
 * gets clipped by that ancestor's overflow when the composer sits near the
 * top of the view (e.g. Home). The placement flips below the trigger when
 * there is not enough room above it.
 */
const MODEL_MENU_TRIGGER_GAP = 8;
const MODEL_MENU_VIEWPORT_MARGIN = 12;
const MODEL_MENU_MAX_HEIGHT = 420;
const MODEL_MENU_MIN_HEIGHT = 160;

type ModelMenuPlacement = {
  top?: number;
  bottom?: number;
  right: number;
  maxHeight: number;
};

function modelMenuPlacement(trigger: HTMLElement): ModelMenuPlacement {
  const rect = trigger.getBoundingClientRect();
  const spaceAbove = rect.top - MODEL_MENU_TRIGGER_GAP - MODEL_MENU_VIEWPORT_MARGIN;
  const spaceBelow = window.innerHeight - rect.bottom - MODEL_MENU_TRIGGER_GAP - MODEL_MENU_VIEWPORT_MARGIN;
  const openUpward = spaceAbove >= MODEL_MENU_MAX_HEIGHT || spaceAbove >= spaceBelow;
  const right = Math.max(window.innerWidth - rect.right, MODEL_MENU_VIEWPORT_MARGIN);
  const available = Math.max(openUpward ? spaceAbove : spaceBelow, MODEL_MENU_MIN_HEIGHT);
  return {
    ...(openUpward
      ? { bottom: window.innerHeight - rect.top + MODEL_MENU_TRIGGER_GAP }
      : { top: rect.bottom + MODEL_MENU_TRIGGER_GAP }),
    right,
    maxHeight: Math.min(available, MODEL_MENU_MAX_HEIGHT),
  };
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
  const [placement, setPlacement] = useState<ModelMenuPlacement | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const firstItemRef = useRef<HTMLButtonElement>(null);
  const defaultModelId = catalog?.preference.selectedModelId;
  const visibleLabel = modelMenuTriggerLabel({
    overrideModelId,
    defaultModelId,
  });
  const closeAndRestoreFocus = useCallback(() => {
    setOpen(false);
    queueMicrotask(() => triggerRef.current?.focus());
  }, []);

  useLayoutEffect(() => {
    if (!open) {
      setPlacement(null);
      return;
    }
    const reposition = () => {
      if (triggerRef.current) setPlacement(modelMenuPlacement(triggerRef.current));
    };
    reposition();
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    return () => {
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!rootRef.current?.contains(target) && !menuRef.current?.contains(target)) setOpen(false);
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

  const positioned = placement !== null;
  useEffect(() => {
    if (open && positioned) firstItemRef.current?.focus();
  }, [open, positioned]);

  return (
    <div className="relative" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className="inline-flex h-[30px] max-w-56 items-center gap-1.5 rounded-md border border-transparent px-2 text-label font-medium text-muted-foreground transition-colors hover:border-border hover:bg-muted aria-expanded:border-input aria-expanded:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
        aria-label={`Choose model, ${visibleLabel}`}
        aria-haspopup="menu"
        aria-controls="composer-model-menu"
        aria-expanded={open}
        disabled={disabled}
        title={disabled
          ? "Model overrides apply to Chat conversations only"
          : overrideModelId ?? defaultModelId
            ? `Canonical id: ${overrideModelId ?? defaultModelId}`
            : undefined}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="truncate">{visibleLabel}</span>
        <ChevronDown className="shrink-0" size={14} aria-hidden="true" />
      </button>
      {open && placement && createPortal(
        <div
          id="composer-model-menu"
          ref={menuRef}
          className="fixed z-50 flex w-[min(360px,calc(100vw-32px))] flex-col overflow-hidden rounded-xl border border-input bg-popover shadow-overlay"
          style={{
            top: placement.top,
            bottom: placement.bottom,
            right: placement.right,
            maxHeight: placement.maxHeight,
          }}
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
          <div className="shrink-0 border-b border-border px-3 py-2">
            <p className="m-0 text-body-sm font-semibold text-foreground">Model for this conversation</p>
            <p className="m-0 mt-0.5 text-label text-muted-foreground">A temporary choice clears after the conversation starts.</p>
          </div>
          {catalog && (
            <div className="min-h-0 flex-1 overflow-y-auto p-1.5">
              <ModelChoice
                ref={firstItemRef}
                label={defaultModelId ? `Default · ${modelDisplayLabel(defaultModelId)}` : "Default"}
                secondary={defaultModelId}
                selected={!overrideModelId}
                onChoose={() => {
                  onOverrideChange(undefined);
                  closeAndRestoreFocus();
                }}
              />
              {selectableModels.map((model) => (
                <div className="group flex items-center gap-1 rounded-md hover:bg-muted" key={model.id} role="none">
                  <ModelChoice
                    label={modelDisplayLabel(model.id)}
                    secondary={model.id}
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
                      aria-label={`Set ${modelDisplayLabel(model.id)} as default`}
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
        </div>,
        document.body,
      )}
    </div>
  );
}

function matchesMenuNavigationKey(key: string): key is "ArrowDown" | "ArrowUp" | "Home" | "End" {
  return key === "ArrowDown" || key === "ArrowUp" || key === "Home" || key === "End";
}

const ModelChoice = forwardRef<HTMLButtonElement, {
  label: string;
  secondary?: string;
  selected: boolean;
  onChoose(): void;
  disabled?: boolean;
}>(function ModelChoice({ label, secondary, selected, onChoose, disabled = false }, ref) {
  return (
    <button
      ref={ref}
      type="button"
      role="menuitemradio"
      aria-checked={selected}
      disabled={disabled}
      title={secondary}
      className="flex min-h-[34px] min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left text-body-sm text-foreground hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50"
      onClick={onChoose}
    >
      <span className="flex size-4 shrink-0 items-center justify-center" aria-hidden="true">
        {selected && <Check size={14} />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate font-medium">{label}</span>
        {secondary ? (
          <span className="mt-0.5 block truncate font-mono text-label text-subtle-foreground">{secondary}</span>
        ) : null}
      </span>
    </button>
  );
});
