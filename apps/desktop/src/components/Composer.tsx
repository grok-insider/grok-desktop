import { useEffect, useRef, useState } from "react";
import {
  ArrowUp,
  AtSign,
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
import { IconButton } from "./IconButton";
import { VoiceOverlay } from "./VoiceOverlay";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";

type ImagineToolKind = "image" | "video";

/* Radix Select forbids empty-string item values; sentinel for "no project". */
const NO_PROJECT_VALUE = "__no-project__";

const modeTabClassName = cn(
  "h-7 min-w-[69px] rounded-md px-3 text-body-sm font-semibold text-muted-foreground transition-colors duration-150",
  "hover:text-foreground data-[state=active]:bg-card data-[state=active]:text-foreground data-[state=active]:shadow-raised",
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
  const modelOverrideRef = useRef(modelOverride);
  modelOverrideRef.current = modelOverride;

  useEffect(() => {
    if (!projectId && snapshot?.projects[0]) setProjectId(snapshot.projects[0].id);
  }, [projectId, snapshot]);

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
      <Tabs value={mode} onValueChange={(value) => setMode(value as "chat" | "work")} className="mb-2 ml-1 w-fit">
        <TabsList aria-label="Conversation mode" className="h-auto gap-0.5 rounded-lg bg-secondary p-[3px]">
          <TabsTrigger value="chat" className={modeTabClassName}>Chat</TabsTrigger>
          <TabsTrigger
            value="work"
            className={modeTabClassName}
            disabled={!workAvailable}
            title={!workAvailable ? workCapability?.reason : undefined}
          >
            Work
          </TabsTrigger>
        </TabsList>
      </Tabs>
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
              <span className="flex items-center gap-1 font-mono text-label text-muted-foreground">
                Aspect
                <Select value={aspectRatio} onValueChange={setAspectRatio}>
                  <SelectTrigger
                    aria-label="Aspect ratio"
                    size="sm"
                    className="h-7 gap-1 bg-card px-1.5 font-mono text-label text-foreground"
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent position="popper">
                    {["1:1", "16:9", "9:16", "3:2", "2:3"].map((ratio) => (
                      <SelectItem key={ratio} value={ratio} className="font-mono text-label">
                        {ratio}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </span>
              {imagineKind === "video" && (
                <span className="flex items-center gap-1 font-mono text-label text-muted-foreground">
                  Duration
                  <Select value={duration} onValueChange={setDuration}>
                    <SelectTrigger
                      aria-label="Video duration"
                      size="sm"
                      className="h-7 gap-1 bg-card px-1.5 font-mono text-label text-foreground"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent position="popper">
                      {["6s", "10s"].map((value) => (
                        <SelectItem key={value} value={value} className="font-mono text-label">
                          {value}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </span>
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
          <div className="flex items-center gap-1">
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
            <DropdownMenu open={toolsOpen} onOpenChange={setToolsOpen}>
              <DropdownMenuTrigger asChild>
                <IconButton
                  label="Tools"
                  disabled={mode !== "chat" || submitting || !anyImagineTool}
                  title={!anyImagineTool ? "Imagine tools require a ready xAI media capability" : "Chat tools"}
                  className={cn(toolsOpen && "border-border bg-muted")}
                >
                  <Plus size={18} />
                </IconButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                side="top"
                align="start"
                aria-label="Composer tools"
                className="min-w-[220px] rounded-xl border-input p-1.5 shadow-overlay"
              >
                <DropdownMenuItem
                  className="gap-2 rounded-md px-2 py-2 text-body-sm text-foreground"
                  disabled={!imagineImageAvailable}
                  title={!imagineImageAvailable ? imagineImageCapability?.reason : undefined}
                  onSelect={() => openImagineTool("image")}
                >
                  <ImageIcon size={16} aria-hidden="true" />
                  Imagine image
                </DropdownMenuItem>
                <DropdownMenuItem
                  className="gap-2 rounded-md px-2 py-2 text-body-sm text-foreground"
                  disabled={!imagineVideoAvailable}
                  title={!imagineVideoAvailable ? imagineVideoCapability?.reason : undefined}
                  onSelect={() => openImagineTool("video")}
                >
                  <Video size={16} aria-hidden="true" />
                  Imagine video
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
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
        <span className="flex items-center gap-1">
          <AtSign size={14} aria-hidden="true" />
          <Select
            value={projectId || NO_PROJECT_VALUE}
            onValueChange={(value) => setProjectId(value === NO_PROJECT_VALUE ? "" : value)}
          >
            <SelectTrigger
              aria-label="Project"
              size="sm"
              className="h-auto max-w-[170px] gap-1 rounded-sm border-0 bg-transparent px-1 py-0.5 font-mono text-label text-muted-foreground shadow-none"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent position="popper">
              <SelectItem value={NO_PROJECT_VALUE} className="font-mono text-label">No project</SelectItem>
              {snapshot?.projects.map((project) => (
                <SelectItem value={project.id} key={project.id} className="font-mono text-label">
                  {project.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </span>
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

/* Sentinel radio value for "use the daemon default model" (no override). */
const DEFAULT_MODEL_VALUE = "__default-model__";

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
  const defaultModelId = catalog?.preference.selectedModelId;
  const visibleLabel = modelMenuTriggerLabel({
    overrideModelId,
    defaultModelId,
  });

  return (
    /*
     * Non-modal: "Set as default" keeps the menu open while saving, and the
     * rest of the composer must stay accessible (no aria-hidden/scroll lock).
     */
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger
        className="inline-flex h-[30px] max-w-56 items-center gap-1.5 rounded-md border border-transparent px-2 text-label font-medium text-muted-foreground transition-colors outline-none hover:border-border hover:bg-muted focus-visible:ring-[3px] focus-visible:ring-ring aria-expanded:border-input aria-expanded:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
        aria-label={`Choose model, ${visibleLabel}`}
        disabled={disabled}
        title={disabled
          ? "Model overrides apply to Chat conversations only"
          : overrideModelId ?? defaultModelId
            ? `Canonical id: ${overrideModelId ?? defaultModelId}`
            : undefined}
      >
        <span className="truncate">{visibleLabel}</span>
        <ChevronDown className="shrink-0" size={14} aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="end"
        aria-label="Chat model"
        // Radix defaults aria-labelledby to the trigger, which would override
        // the aria-label above as the accessible name.
        aria-labelledby={undefined}
        className="w-[min(360px,calc(100vw-32px))] rounded-xl border-input p-0 shadow-overlay"
      >
        <div className="border-b border-border px-3 py-2">
          <p className="m-0 text-body-sm font-semibold text-foreground">Model for this conversation</p>
          <p className="m-0 mt-0.5 text-label text-muted-foreground">A temporary choice clears after the conversation starts.</p>
        </div>
        {catalog && (
          <DropdownMenuRadioGroup
            className="p-1.5"
            value={overrideModelId ?? DEFAULT_MODEL_VALUE}
            onValueChange={(value) => {
              onOverrideChange(value === DEFAULT_MODEL_VALUE ? undefined : value);
            }}
          >
            <DropdownMenuRadioItem value={DEFAULT_MODEL_VALUE} className="rounded-md">
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">
                  {defaultModelId ? `Default · ${modelDisplayLabel(defaultModelId)}` : "Default"}
                </span>
                {defaultModelId ? (
                  <span className="mt-0.5 block truncate font-mono text-label text-subtle-foreground">
                    {defaultModelId}
                  </span>
                ) : null}
              </span>
            </DropdownMenuRadioItem>
            {selectableModels.map((model) => (
              <div className="flex items-center gap-1" key={model.id}>
                <DropdownMenuRadioItem
                  value={model.id}
                  className="min-w-0 flex-1 rounded-md"
                  disabled={status !== "ready"}
                  title={model.id}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium">{modelDisplayLabel(model.id)}</span>
                    <span className="mt-0.5 block truncate font-mono text-label text-subtle-foreground">{model.id}</span>
                  </span>
                </DropdownMenuRadioItem>
                {model.id !== defaultModelId && (
                  <DropdownMenuItem
                    aria-label={`Set ${modelDisplayLabel(model.id)} as default`}
                    className="shrink-0 rounded-md px-2 py-1.5 text-label font-medium text-muted-foreground"
                    disabled={savingModelId !== null || status !== "ready"}
                    onSelect={(event) => {
                      // Saving the default keeps the menu open to show progress.
                      event.preventDefault();
                      void setDefaultModel(model.id);
                    }}
                  >
                    {savingModelId === model.id ? "Saving…" : "Set as default"}
                  </DropdownMenuItem>
                )}
              </div>
            ))}
          </DropdownMenuRadioGroup>
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
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

