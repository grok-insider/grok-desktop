import { useEffect, useState } from "react";
import { ArrowUp, AtSign, ChevronDown, CircleAlert, Mic } from "lucide-react";
import { useNavigate } from "react-router-dom";
import { useDesktopClient, useDesktopSnapshot } from "../services/DesktopClientContext";
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
      const { runId, threadId } = await client.startRun({ prompt: value, mode, projectId: projectId || undefined, searchEnabled: false, researchEnabled: false });
      setPrompt("");
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
            <button
              className="inline-flex h-[30px] items-center gap-1.5 rounded-md border border-transparent px-2 font-mono text-label font-medium text-muted-foreground transition-colors hover:border-border hover:bg-muted"
              disabled
              title="Automatic model selection is fixed until model discovery is exposed by the daemon"
            >
              Auto <ChevronDown size={14} />
            </button>
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
