import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ArrowLeft,
  ArrowUp,
  Braces,
  Check,
  ChevronDown,
  CircleAlert,
  Copy,
  ExternalLink,
  FileText,
  GitBranch,
  Globe2,
  LoaderCircle,
  MessageSquareText,
  MoreHorizontal,
  Pencil,
  RefreshCw,
  RotateCcw,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import { useNavigate, useParams } from "react-router-dom";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { MarkdownMessage } from "../components/MarkdownMessage";
import { formatUsageLine } from "../lib/usageFormat";
import { IconButton } from "../components/ui";
import { useDesktopClient } from "../services/DesktopClientContext";
import type {
  ClientResult,
  ConversationArtifact,
  ConversationCitation,
  ConversationDetail,
  ConversationMessage,
  ConversationTurnDetail,
} from "../services/desktopClient";
import { copyToClipboard } from "../utils/clipboard";

type Inspector =
  | { kind: "citation"; value: ConversationCitation }
  | { kind: "artifact"; value: ConversationArtifact }
  | null;

type Notice = { kind: "error" | "status"; message: string } | null;

type ForkDialogState =
  | { kind: "edit"; message: ConversationMessage; turn: ConversationTurnDetail }
  | { kind: "regenerate"; message: ConversationMessage; turn: ConversationTurnDetail }
  | null;

export function ConversationView() {
  const { threadId = "" } = useParams();
  const navigate = useNavigate();
  const client = useDesktopClient();
  const [conversation, setConversation] = useState<ConversationDetail | null>(null);
  const [unavailable, setUnavailable] = useState("");
  const [inspector, setInspector] = useState<Inspector>(null);
  const [prompt, setPrompt] = useState("");
  const [searchEnabled, setSearchEnabled] = useState(false);
  const [sending, setSending] = useState(false);
  const [cancellingTurnId, setCancellingTurnId] = useState("");
  const [retryingTurnId, setRetryingTurnId] = useState("");
  const [forkingMessageId, setForkingMessageId] = useState("");
  const [forkDialog, setForkDialog] = useState<ForkDialogState>(null);
  const [forkError, setForkError] = useState("");
  const [notice, setNotice] = useState<Notice>(null);
  const inspectorReturnFocus = useRef<HTMLElement | null>(null);
  const forkDialogReturnFocus = useRef<HTMLElement | null>(null);
  const inspectorCloseButton = useRef<HTMLButtonElement | null>(null);
  const promptInput = useRef<HTMLTextAreaElement | null>(null);
  const transcriptViewport = useRef<HTMLDivElement | null>(null);
  const transcriptEnd = useRef<HTMLDivElement | null>(null);
  const [followingLatest, setFollowingLatest] = useState(true);

  useEffect(() => {
    let active = true;
    setConversation(null);
    setUnavailable("");
    setInspector(null);
    setForkDialog(null);
    setForkingMessageId("");
    setForkError("");
    setFollowingLatest(true);

    void client.getConversation(threadId)
      .then((result) => {
        if (!active) return;
        if (result.status === "success") setConversation(result.value);
        else setUnavailable(result.reason);
      })
      .catch((error: unknown) => {
        if (active) {
          setUnavailable(error instanceof Error ? error.message : "The conversation could not be loaded.");
        }
      });

    const unsubscribe = client.subscribeConversation(threadId, (value) => {
      if (active) setConversation(value);
    });
    return () => {
      active = false;
      unsubscribe();
    };
  }, [client, threadId]);

  useEffect(() => {
    if (!inspector) return;
    queueMicrotask(() => inspectorCloseButton.current?.focus());

    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setInspector(null);
      queueMicrotask(() => inspectorReturnFocus.current?.focus());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inspector]);

  const activeTurn = newestActiveTurn(conversation?.turns ?? []);
  const newestBranchSource = conversation ? newestCompletedAssistant(conversation) : undefined;
  const newestMessage = conversation?.messages.at(-1);

  useEffect(() => {
    if (!newestMessage || !followingLatest) return;
    transcriptEnd.current?.scrollIntoView?.({ block: "end" });
  }, [followingLatest, newestMessage]);

  const updateTranscriptPosition = () => {
    const viewport = transcriptViewport.current;
    if (!viewport) return;
    const distanceFromEnd = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
    setFollowingLatest(distanceFromEnd <= 80);
  };

  const jumpToLatest = () => {
    setFollowingLatest(true);
    transcriptEnd.current?.scrollIntoView?.({ block: "end", behavior: "smooth" });
  };

  const send = async () => {
    if (!prompt.trim() || sending || retryingTurnId || activeTurn) return;
    setSending(true);
    setNotice(null);
    try {
      const result = await client.sendConversationMessage(threadId, prompt.trim(), [], searchEnabled);
      if (result.status === "success") {
        setPrompt("");
        setSearchEnabled(false);
      }
      else setNotice({ kind: "error", message: result.reason });
    } catch (error) {
      setNotice({
        kind: "error",
        message: error instanceof Error ? error.message : "The message could not be sent.",
      });
    } finally {
      setSending(false);
    }
  };

  const retry = async (turn: ConversationTurnDetail) => {
    if (retryingTurnId || activeTurn || turn.retryEligibility !== "allowed") return;
    setRetryingTurnId(turn.id);
    setNotice(null);
    try {
      const result = await client.retryConversationTurn({
        sourceTurnId: turn.id,
        expectedRevision: turn.revision,
      });
      setNotice(result.status === "success"
        ? { kind: "status", message: "Retry request submitted." }
        : { kind: "error", message: result.reason });
    } catch {
      setNotice({ kind: "error", message: "The request could not be retried." });
    } finally {
      setRetryingTurnId("");
    }
  };

  const stop = async (turn: ConversationTurnDetail) => {
    if (cancellingTurnId) return;
    setCancellingTurnId(turn.id);
    setNotice(null);
    try {
      const result = await client.cancelConversationTurn({
        turnId: turn.id,
        expectedRevision: turn.revision,
      });
      if (result.status !== "success") {
        setNotice({ kind: "error", message: result.reason });
      } else if (result.value.state === "cancelled") {
        setNotice({ kind: "status", message: "The request was cancelled before dispatch." });
      } else if (result.value.state === "interrupted_needs_review") {
        setNotice({
          kind: "status",
          message: "Generation stopped after dispatch. Review the partial response; this uncertain request cannot be retried automatically.",
        });
      } else if (result.value.state === "completed") {
        setNotice({ kind: "status", message: "The response completed before it could be stopped." });
      } else {
        setNotice({ kind: "error", message: "The response failed before it could be stopped." });
      }
    } catch (error) {
      setNotice({
        kind: "error",
        message: error instanceof Error ? error.message : "The response could not be stopped.",
      });
    } finally {
      setCancellingTurnId("");
    }
  };

  const openInspector = (next: Exclude<Inspector, null>) => {
    inspectorReturnFocus.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    setInspector(next);
  };

  const closeInspector = () => {
    setInspector(null);
    queueMicrotask(() => inspectorReturnFocus.current?.focus());
  };

  const copyMessage = async (message: ConversationMessage) => {
    try {
      const copied = await copyToClipboard(message.content);
      setNotice(copied
        ? { kind: "status", message: "Message copied." }
        : { kind: "error", message: "Clipboard access is unavailable." });
    } catch {
      setNotice({ kind: "error", message: "Clipboard access is unavailable." });
    }
  };

  const runFork = async (
    message: ConversationMessage,
    operation: () => Promise<ClientResult<ConversationDetail>>,
  ) => {
    if (forkingMessageId) return;
    setForkingMessageId(message.id);
    setNotice(null);
    try {
      const result = await operation();
      if (result.status !== "success") {
        setNotice({ kind: "error", message: result.reason });
        if (forkDialog) setForkError(result.reason);
        return;
      }
      setForkDialog(null);
      setForkError("");
      navigate(`/conversations/${result.value.id}`);
    } catch (error) {
      const reason = error instanceof Error ? error.message : "The conversation branch could not be created.";
      setNotice({
        kind: "error",
        message: reason,
      });
      if (forkDialog) setForkError(reason);
    } finally {
      setForkingMessageId("");
    }
  };

  const branch = (message: ConversationMessage) => runFork(
    message,
    () => client.branchConversation(threadId, message.id),
  );

  const submitForkDialog = (content?: string) => {
    if (!forkDialog) return Promise.resolve();
    if (forkDialog.kind === "edit") {
      return runFork(
        forkDialog.message,
        () => client.editConversationMessage(threadId, forkDialog.message.id, content ?? ""),
      );
    }
    return runFork(
      forkDialog.message,
      () => client.regenerateConversationMessage(threadId, forkDialog.message.id),
    );
  };

  const openForkDialog = (dialog: Exclude<ForkDialogState, null>) => {
    forkDialogReturnFocus.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    setForkError("");
    setForkDialog(dialog);
  };

  const closeForkDialog = () => {
    setForkError("");
    setForkDialog(null);
  };

  if (unavailable) {
    return (
      <ConversationUnavailable
        reason={unavailable}
        onBack={() => navigate("/")}
        onSetup={() => navigate("/setup")}
      />
    );
  }
  if (!conversation) return <ConversationLoading />;

  const inspecting = inspector !== null;

  return (
    <>
      <div
        className="grid h-full min-h-0 min-w-0 grid-rows-[62px_minmax(0,1fr)] overflow-hidden bg-card max-[680px]:h-[calc(100dvh-110px)]"
        data-testid="conversation-page"
      >
      <header className="flex min-w-0 items-center gap-2.5 overflow-hidden border-b border-border px-4 max-[680px]:gap-1.5 max-[680px]:px-2">
        <IconButton label="Back to home" onClick={() => navigate("/")}>
          <ArrowLeft size={18} />
        </IconButton>
        <div className="min-w-0 flex-1">
          <h1 className="m-0 truncate text-body font-semibold text-foreground">{conversation.title}</h1>
          <p className="m-0 mt-0.5 truncate font-mono text-label text-subtle-foreground">
            {conversation.projectName} · {conversation.mode === "work" ? "Work" : "Chat"}
          </p>
        </div>
        <Select
          value={conversation.id}
          onValueChange={(value) => {
            if (value !== conversation.id) navigate(`/conversations/${value}`);
          }}
        >
          <SelectTrigger
            aria-label="Conversation branch"
            className="h-9 max-w-48 min-w-0 gap-1.5 border-input bg-card px-2 font-mono text-label text-foreground shadow-xs max-[680px]:max-w-[126px]"
          >
            <GitBranch className="shrink-0 text-muted-foreground" size={14} aria-hidden="true" />
            <SelectValue />
            <Badge className="min-w-5 shrink-0 px-1.5" variant="neutral">{conversation.branchCount}</Badge>
          </SelectTrigger>
          <SelectContent position="popper" align="end">
            {conversation.branches.map((branchOption) => (
              <SelectItem
                key={branchOption.threadId}
                value={branchOption.threadId}
                className="font-mono text-label"
              >
                {branchOption.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          aria-busy={newestBranchSource ? forkingMessageId === newestBranchSource.message.id : undefined}
          aria-label="New branch from latest completed response"
          className="max-[680px]:size-[34px] max-[680px]:px-0"
          disabled={!newestBranchSource || Boolean(forkingMessageId)}
          onClick={() => newestBranchSource && void branch(newestBranchSource.message)}
          title={newestBranchSource
            ? "Create a child conversation without sending a new xAI request"
            : "A completed Grok response is required"}
          variant="outline"
        >
          {newestBranchSource && forkingMessageId === newestBranchSource.message.id
            ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={14} />
            : <Sparkles size={14} />}
          <span className="max-[680px]:sr-only">New branch</span>
        </Button>
        <IconButton label="No additional conversation actions available" disabled>
          <MoreHorizontal size={18} />
        </IconButton>
      </header>

      <div
        className={cn(
          "grid min-h-0 min-w-0 grid-cols-1",
          inspecting && "grid-cols-[minmax(0,1fr)_minmax(300px,34%)] max-[900px]:grid-cols-[minmax(0,1fr)_300px] max-[680px]:block",
        )}
        data-testid="conversation-workspace"
      >
        <section
          aria-label="Conversation transcript"
          className={cn(
            "grid min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_auto] border-r border-border",
            inspecting && "max-[680px]:hidden",
          )}
        >
          <div className="relative min-h-0">
            <div
              className="h-full overflow-y-auto"
              data-testid="conversation-transcript-viewport"
              onScroll={updateTranscriptPosition}
              ref={transcriptViewport}
            >
              <div className="mx-auto w-full max-w-[760px] px-6 pb-3 pt-6 max-[900px]:px-[18px] max-[680px]:px-3 max-[680px]:py-4">
              {conversation.messages.length === 0 ? (
                <EmptyConversation onCompose={() => promptInput.current?.focus()} />
              ) : (
                conversation.messages.map((message) => {
                  const turn = conversation.turns.find(
                    (item) => item.userMessageId === message.id || item.assistantMessageId === message.id,
                  );
                  return (
                    /*
                     * content-visibility keeps off-screen markdown out of
                     * layout, so container-width changes (sidebar collapse,
                     * window resize) only reflow the visible messages.
                     */
                    <div
                      className="[content-visibility:auto] [contain-intrinsic-size:auto_160px] [&+&]:pt-1"
                      key={message.id}
                    >
                      <MessageBlock
                        message={message}
                        onArtifact={(artifact) => openInspector({ kind: "artifact", value: artifact })}
                        onBranch={() => void branch(message)}
                        onCitation={(citation) => openInspector({ kind: "citation", value: citation })}
                        onCopy={() => copyMessage(message)}
                        onEdit={() => turn && openForkDialog({ kind: "edit", message, turn })}
                        onOpenExternal={(url) => void client.openExternalUrl(url)}
                        onRegenerate={() => turn && openForkDialog({ kind: "regenerate", message, turn })}
                        busy={Boolean(forkingMessageId)}
                        retryDepth={turn?.lineage.origin === "retry" ? turn.lineage.retryDepth : undefined}
                        turn={turn}
                      />
                      {turn && turn.userMessageId === message.id && turn.state !== "completed" ? (
                        <TurnOutcome
                          onRetry={() => void retry(turn)}
                          retrying={retryingTurnId === turn.id}
                          turn={turn}
                        />
                      ) : null}
                    </div>
                  );
                })
              )}
                <div aria-hidden="true" ref={transcriptEnd} />
              </div>
            </div>
            {!followingLatest ? (
              <Button
                className="absolute bottom-3 left-1/2 -translate-x-1/2 shadow-raised"
                onClick={jumpToLatest}
                size="sm"
                variant="outline"
              >
                <ChevronDown size={14} aria-hidden="true" />
                Jump to latest
              </Button>
            ) : null}
          </div>

          <div className="mx-auto w-full max-w-[760px] px-4 pb-4 pt-2 max-[680px]:px-2">
            <div
              className="rounded-xl border border-input bg-card p-2 shadow-overlay transition-[border-color,box-shadow] duration-150 focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring"
            >
              <label className="sr-only" htmlFor="conversation-prompt">Reply to Grok</label>
              <Textarea
                aria-busy={sending}
                className="min-h-12 resize-none border-0 bg-transparent p-1 text-body-lg shadow-none focus-visible:ring-0"
                id="conversation-prompt"
                onChange={(event) => setPrompt(event.target.value)}
                onKeyDown={(event) => {
                  if (
                    event.key === "Enter"
                    && !event.shiftKey
                    && !event.nativeEvent.isComposing
                    && !activeTurn
                  ) {
                    event.preventDefault();
                    void send();
                  }
                }}
                placeholder="Reply to Grok"
                ref={promptInput}
                rows={2}
                value={prompt}
              />
              <div className="flex items-center justify-between border-t border-border pt-2">
                <IconButton
                  aria-pressed={searchEnabled}
                  className={cn(searchEnabled && "border-border bg-muted text-foreground")}
                  disabled={sending || Boolean(activeTurn)}
                  label={searchEnabled ? "Disable Search" : "Enable Search"}
                  onClick={() => setSearchEnabled((value) => !value)}
                  title="Allow this reply to use official xAI web and X search"
                >
                  <Globe2 size={17} />
                </IconButton>
                {activeTurn ? (
                  <Button
                    aria-busy={cancellingTurnId === activeTurn.id}
                    aria-label="Stop Grok response"
                    disabled={Boolean(cancellingTurnId)}
                    onClick={() => void stop(activeTurn)}
                    size="icon"
                    title={activeTurn.state === "reserved"
                      ? "Cancel before provider dispatch"
                      : "Stop generation and mark the dispatched request for review"}
                    variant="outline"
                  >
                    <Square fill="currentColor" size={15} />
                  </Button>
                ) : (
                  <Button
                    aria-busy={sending}
                    aria-label="Send reply"
                    disabled={!prompt.trim() || sending || Boolean(retryingTurnId)}
                    onClick={() => void send()}
                    size="icon"
                    title={sending ? "Submitting the durable Grok request" : "Send reply"}
                  >
                    {sending ? <LoaderCircle className="animate-spin" size={17} /> : <ArrowUp size={17} />}
                  </Button>
                )}
              </div>
            </div>
            {sending ? <p className="sr-only" role="status">Submitting the durable Grok request</p> : null}
            {activeTurn ? (
              <p className="sr-only" role="status">
                {activeTurn.state === "reserved"
                  ? "Grok request reserved. Stop is available before dispatch."
                  : "Grok response is streaming from durable events. Stop will require review."}
              </p>
            ) : null}
            {notice ? (
              <p
                className={cn(
                  "m-0 mt-2 px-1 text-body-sm",
                  notice.kind === "error" ? "text-destructive" : "text-success",
                )}
                role={notice.kind === "error" ? "alert" : "status"}
              >
                {notice.message}
              </p>
            ) : null}
          </div>
        </section>

        {inspector ? (
          <InspectorPanel
            closeButtonRef={inspectorCloseButton}
            inspector={inspector}
            key={`${inspector.kind}-${inspector.value.id}`}
            onClose={closeInspector}
            onOpenExternal={(url) => client.openExternalUrl(url)}
          />
        ) : null}
        </div>
      </div>
      <ConversationForkDialog
        busy={Boolean(forkingMessageId)}
        dialog={forkDialog}
        error={forkError}
        onClose={closeForkDialog}
        onConfirm={(content) => void submitForkDialog(content)}
        returnFocus={forkDialogReturnFocus}
      />
    </>
  );
}

function ConversationLoading() {
  return (
    <div
      aria-busy="true"
      aria-label="Loading conversation"
      className="flex h-full items-center justify-center bg-card p-6"
      role="status"
    >
      <div className="w-full max-w-[760px] space-y-6">
        <span className="sr-only">Loading conversation</span>
        {["w-3/5", "w-4/5", "w-2/3"].map((width) => (
          <div className="grid grid-cols-[28px_minmax(0,1fr)] gap-3" key={width}>
            <Skeleton className="size-7 rounded-lg" />
            <div className="space-y-2">
              <Skeleton className="h-4 w-28" />
              <Skeleton className={cn("h-16", width)} />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function EmptyConversation({ onCompose }: { onCompose(): void }) {
  return (
    <div className="flex min-h-64 flex-col items-center justify-center px-4 text-center">
      <span className="grid size-12 place-items-center rounded-full bg-accent text-foreground" aria-hidden="true">
        <MessageSquareText size={21} />
      </span>
      <h2 className="m-0 mt-4 text-title font-semibold text-foreground">Start this conversation</h2>
      <p className="m-0 mt-1 max-w-[52ch] text-body-lg text-muted-foreground">
        No messages have been submitted yet. Write the first message below.
      </p>
      <Button className="mt-4" onClick={onCompose} variant="outline">Write the first message</Button>
    </div>
  );
}

function MessageBlock({
  message,
  onCopy,
  onCitation,
  onArtifact,
  onEdit,
  onOpenExternal,
  onRegenerate,
  onBranch,
  busy,
  retryDepth,
  turn,
}: {
  message: ConversationMessage;
  onCopy(): Promise<void>;
  onCitation(value: ConversationCitation): void;
  onArtifact(value: ConversationArtifact): void;
  onEdit(): void;
  onOpenExternal(url: string): void;
  onRegenerate(): void;
  onBranch(): void;
  busy: boolean;
  retryDepth?: number;
  turn?: ConversationTurnDetail;
}) {
  const artifact = message.artifact;
  const isUser = message.role === "user";
  const canEdit = isUser
    && turn !== undefined
    && (turn.state === "completed" || turn.state === "cancelled" || turn.state === "failed")
    && turn.userMessageId === message.id;
  const canForkResponse = !isUser
    && turn?.state === "completed"
    && turn.assistantMessageId === message.id;

  return (
    <article className="group relative pb-6 pl-10 max-[680px]:pl-9">
      <header className="flex min-h-7 items-center gap-2">
        <span
          aria-hidden="true"
          className={cn(
            "absolute left-0 top-0 grid size-7 place-items-center text-label font-semibold",
            isUser
              ? "rounded-full bg-secondary text-secondary-foreground"
              : "rounded-lg bg-primary text-primary-foreground",
          )}
        >
          {isUser ? "You" : "G"}
        </span>
        <strong className="text-body-sm font-semibold text-foreground">{isUser ? "You" : "Grok"}</strong>
        <time className="font-mono text-label tabular-nums text-subtle-foreground">{message.createdAt}</time>
        {isUser && retryDepth !== undefined ? (
          <Badge variant="neutral">Retry {retryDepth}</Badge>
        ) : null}
        {message.state === "streaming" ? (
          <Badge variant="info">
            <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
            Streaming
          </Badge>
        ) : null}
        {message.state === "stopped" ? (
          <Badge variant="warning">
            <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
            Stopped
          </Badge>
        ) : null}
        {message.state === "error" ? (
          <Badge variant="destructive">
            <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
            Failed
          </Badge>
        ) : null}
      </header>

      {message.attachments.length > 0 ? (
        <div aria-label="Message attachments" className="mb-2 mt-1 flex flex-wrap gap-2" role="list">
          {message.attachments.map((attachment) => (
            <div
              className="grid min-h-11 grid-cols-[18px_minmax(0,1fr)_16px] items-center gap-2 rounded-md border border-border bg-card px-2 py-1.5"
              key={attachment.id}
              role="listitem"
            >
              <FileText className="text-muted-foreground" size={15} />
              <span className="min-w-0">
                <strong className="block max-w-48 truncate text-body-sm font-medium text-foreground">{attachment.name}</strong>
                <small className="block text-label text-subtle-foreground">{attachment.detail}</small>
              </span>
              {attachment.state === "ready" ? <Check className="text-success" size={14} aria-label="Ready" /> : null}
              {attachment.state === "failed" ? <CircleAlert className="text-destructive" size={14} aria-label="Failed" /> : null}
              {attachment.state === "uploading" || attachment.state === "scanning" ? (
                <LoaderCircle className="animate-spin text-info" size={14} aria-label={attachment.state} />
              ) : null}
            </div>
          ))}
        </div>
      ) : null}

      <div
        className={cn(
          "min-h-[22px]",
          isUser && "whitespace-pre-wrap rounded-lg bg-muted px-3 py-2 text-body-lg leading-[22px] text-foreground",
        )}
      >
        {isUser ? message.content : (
          <MarkdownMessage onOpenExternal={onOpenExternal} streaming={message.state === "streaming"}>
            {message.content}
          </MarkdownMessage>
        )}
      </div>
      {message.state === "streaming" ? <span className="sr-only" role="status">Grok is responding.</span> : null}

      {!isUser && turn?.state === "completed" && (turn.usage.inputTokens > 0 || turn.usage.outputTokens > 0) ? (
        <p className="m-0 mt-1.5 font-mono text-label text-subtle-foreground" aria-label="Response usage">
          {formatUsageLine(turn.usage)}
        </p>
      ) : null}

      {message.citations.length > 0 ? (
        <div aria-label="Sources" className="mt-3 flex flex-wrap gap-2">
          {message.citations.map((citation, index) => (
            <button
              className="flex min-h-[34px] items-center gap-1.5 rounded-md border border-border bg-card py-1 pl-1 pr-2 text-left text-body-sm text-muted-foreground transition-[background-color,border-color,color] duration-150 hover:border-input-hover hover:bg-muted hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring"
              key={citation.id}
              onClick={() => onCitation(citation)}
              type="button"
            >
              <span className="grid size-6 shrink-0 place-items-center rounded-full bg-info-soft font-mono text-label font-semibold text-info">
                {index + 1}
              </span>
              {citation.title}
            </button>
          ))}
        </div>
      ) : null}

      {artifact ? (
        <button
          className="mt-3 grid min-h-14 w-full max-w-[430px] grid-cols-[36px_minmax(0,1fr)_18px] items-center gap-2 rounded-lg border border-input bg-muted p-2 text-left transition-[background-color,border-color,transform] duration-150 hover:border-input-hover hover:bg-accent active:scale-[.99] focus-visible:ring-[3px] focus-visible:ring-ring"
          onClick={() => onArtifact(artifact)}
          type="button"
        >
          <span className="grid size-9 place-items-center rounded-md bg-info-soft text-info" aria-hidden="true">
            <Braces size={17} />
          </span>
          <span className="flex min-w-0 flex-col gap-0.5">
            <strong className="truncate text-body font-semibold text-foreground">{artifact.title}</strong>
            <small className="font-mono text-label text-subtle-foreground">{artifact.kind} · Version {artifact.version}</small>
          </span>
          <Square className="text-muted-foreground" size={14} />
        </button>
      ) : null}

      <footer
        aria-label="Message actions"
        className="mt-2 flex gap-1 opacity-60 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100 max-[680px]:opacity-100"
      >
        <IconButton label="Copy message" onClick={() => void onCopy()}>
          <Copy size={15} />
        </IconButton>
        {isUser ? (
          <IconButton
            label={canEdit ? "Edit prompt in a new branch" : editUnavailableLabel(turn)}
            disabled={!canEdit || busy}
            onClick={onEdit}
          >
            <Pencil size={15} />
          </IconButton>
        ) : (
          <IconButton
            label={canForkResponse ? "Regenerate response in a new branch" : responseUnavailableLabel("regenerate", turn)}
            disabled={!canForkResponse || busy}
            onClick={onRegenerate}
          >
            {busy
              ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={15} />
              : <RefreshCw size={15} />}
          </IconButton>
        )}
        {!isUser ? (
          <IconButton
            label={canForkResponse ? "Branch from this response" : responseUnavailableLabel("branch", turn)}
            disabled={!canForkResponse || busy}
            onClick={onBranch}
          >
            {busy
              ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={15} />
              : <GitBranch size={15} />}
          </IconButton>
        ) : null}
        {!turn ? (
          <span className="ml-1 self-center text-label text-subtle-foreground">
            {isUser ? "Inherited prompt · editing unavailable" : "Inherited response · fork actions unavailable"}
          </span>
        ) : null}
      </footer>
    </article>
  );
}

function ConversationForkDialog({
  dialog,
  busy,
  error,
  onClose,
  onConfirm,
  returnFocus,
}: {
  dialog: ForkDialogState;
  busy: boolean;
  error: string;
  onClose(): void;
  onConfirm(content?: string): void;
  returnFocus: RefObject<HTMLElement | null>;
}) {
  const [content, setContent] = useState("");
  const editedPrompt = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    setContent(dialog?.kind === "edit" ? dialog.message.content : "");
  }, [dialog]);

  const editing = dialog?.kind === "edit";
  const changed = editing && content.length > 0 && content !== dialog.message.content;

  return (
    <Dialog
      open={dialog !== null}
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      {dialog ? (
        <DialogContent
          aria-busy={busy}
          className="max-w-[560px]"
          onEscapeKeyDown={(event) => {
            if (busy) event.preventDefault();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            queueMicrotask(() => returnFocus.current?.focus());
          }}
          onInteractOutside={(event) => {
            if (busy) event.preventDefault();
          }}
          onOpenAutoFocus={(event) => {
            if (!editing) return;
            event.preventDefault();
            queueMicrotask(() => editedPrompt.current?.focus());
          }}
          showCloseButton={!busy}
        >
          <DialogHeader className="pr-8">
            <DialogTitle>
              {editing ? "Edit prompt in a new branch" : "Regenerate in a new branch?"}
            </DialogTitle>
            <DialogDescription>
              The parent conversation stays unchanged. This creates a child conversation and sends a new billable request to xAI using the source turn&apos;s recorded model.
            </DialogDescription>
          </DialogHeader>
          {editing ? (
            <div className="grid gap-1.5">
              <label className="text-body-sm font-medium text-foreground" htmlFor="edited-conversation-prompt">
                Edited prompt
              </label>
              <Textarea
                className="min-h-32 resize-y"
                disabled={busy}
                id="edited-conversation-prompt"
                maxLength={1024 * 1024}
                onChange={(event) => setContent(event.target.value)}
                ref={editedPrompt}
                value={content}
              />
              <p className="m-0 text-label text-subtle-foreground">
                Change the prompt to create an independent child conversation.
              </p>
            </div>
          ) : (
            <div className="rounded-lg border border-warning/25 bg-warning-soft p-3 text-body-sm text-foreground">
              Grok will answer the same recorded prompt and frozen context again. Provider output and usage may differ.
            </div>
          )}
          {error ? (
            <p className="m-0 rounded-md bg-destructive-soft px-3 py-2 text-body-sm text-destructive" role="alert">
              {error}
            </p>
          ) : null}
          <DialogFooter className="max-[480px]:flex-col-reverse max-[480px]:[&>*]:w-full">
            <Button disabled={busy} onClick={onClose} type="button" variant="outline">
              Cancel
            </Button>
            <Button
              aria-busy={busy}
              disabled={busy || (editing && !changed)}
              onClick={() => onConfirm(editing ? content : undefined)}
              type="button"
            >
              {busy ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={15} /> : null}
              {editing ? "Create edited branch" : "Regenerate response"}
            </Button>
          </DialogFooter>
        </DialogContent>
      ) : null}
    </Dialog>
  );
}

function editUnavailableLabel(turn?: ConversationTurnDetail): string {
  if (!turn) return "Editing is unavailable for inherited prompts";
  if (turn.state === "reserved" || turn.state === "provider_started") {
    return "Editing is unavailable while this request is active";
  }
  if (turn.state === "interrupted_needs_review") {
    return "Editing is unavailable while this request needs review";
  }
  return "This prompt cannot be edited into a branch";
}

function responseUnavailableLabel(
  action: "branch" | "regenerate",
  turn?: ConversationTurnDetail,
): string {
  const verb = action === "branch" ? "Branching" : "Regeneration";
  if (!turn) return `${verb} is unavailable for inherited responses`;
  if (turn.state === "reserved" || turn.state === "provider_started") {
    return `${verb} is unavailable while this response is active`;
  }
  if (turn.state === "interrupted_needs_review") {
    return `${verb} is unavailable while this response needs review`;
  }
  return `${verb} requires a completed Grok response`;
}

function TurnOutcome({
  turn,
  onRetry,
  retrying,
}: {
  turn: ConversationTurnDetail;
  onRetry(): void;
  retrying: boolean;
}) {
  const detail = turn.failure?.message ?? {
    reserved: "The request is waiting to be dispatched.",
    provider_started: "The request reached xAI and is awaiting a durable outcome.",
    cancelled: "The request was cancelled before a response was committed.",
    interrupted_needs_review: "The connection ended after dispatch. The request may have reached xAI; review is required and it cannot be retried automatically.",
    failed: "Grok did not return a completed response.",
    completed: "",
  }[turn.state];
  const title = {
    reserved: "Grok request reserved",
    provider_started: "Waiting for a durable outcome",
    cancelled: "Grok request cancelled",
    interrupted_needs_review: "Review required",
    failed: "Grok request failed",
    completed: "Grok request completed",
  }[turn.state];
  const needsReview = turn.state === "interrupted_needs_review";
  const failed = turn.state === "failed";
  const canRetry = turn.retryEligibility === "allowed"
    && (turn.state === "failed" || turn.state === "cancelled");

  return (
    <div
      className={cn(
        "mb-5 ml-10 flex gap-2 rounded-lg border p-3 text-muted-foreground max-[680px]:ml-9",
        turn.state === "reserved" || turn.state === "provider_started"
          ? "border-info/25 bg-info-soft"
          : "border-border bg-muted",
        needsReview && "border-warning/25 bg-warning-soft",
        failed && "border-destructive/25 bg-destructive-soft",
      )}
      data-turn-state={turn.state}
      role={needsReview || failed ? "alert" : "status"}
    >
      <CircleAlert
        className={cn(
          "mt-0.5 shrink-0",
          turn.state === "reserved" || turn.state === "provider_started" ? "text-info" : "text-muted-foreground",
          needsReview && "text-warning",
          failed && "text-destructive",
        )}
        size={16}
      />
      <div className="min-w-0 flex-1">
        <strong className="block text-body-sm font-semibold text-foreground">{title}</strong>
        <p className="m-0 mt-0.5 text-body-sm leading-[18px]">{detail}</p>
        {canRetry ? (
          <Button
            aria-busy={retrying}
            className="mt-2"
            disabled={retrying}
            onClick={onRetry}
            size="sm"
            variant="outline"
          >
            {retrying
              ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={14} />
              : <RefreshCw size={14} />}
            Retry request
          </Button>
        ) : null}
        {retrying ? <span className="sr-only" role="status">Retrying request</span> : null}
      </div>
    </div>
  );
}

function newestActiveTurn(turns: ConversationTurnDetail[]): ConversationTurnDetail | undefined {
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const turn = turns[index];
    if (turn.state === "reserved" || turn.state === "provider_started") return turn;
  }
  return undefined;
}

function newestCompletedAssistant(
  conversation: ConversationDetail,
): { message: ConversationMessage; turn: ConversationTurnDetail } | undefined {
  for (let index = conversation.turns.length - 1; index >= 0; index -= 1) {
    const turn = conversation.turns[index];
    if (turn.state !== "completed" || !turn.assistantMessageId) continue;
    const message = conversation.messages.find((item) => (
      item.id === turn.assistantMessageId
      && item.role === "assistant"
      && item.state === "complete"
    ));
    if (message) return { message, turn };
  }
  return undefined;
}

function InspectorPanel({
  inspector,
  onClose,
  onOpenExternal,
  closeButtonRef,
}: {
  inspector: Exclude<Inspector, null>;
  onClose(): void;
  onOpenExternal(url: string): Promise<ClientResult<void>>;
  closeButtonRef: RefObject<HTMLButtonElement | null>;
}) {
  const titleId = `conversation-${inspector.kind}-title`;
  const [openingSource, setOpeningSource] = useState(false);
  const [sourceError, setSourceError] = useState("");

  const openSource = async (url: string) => {
    if (openingSource) return;
    setOpeningSource(true);
    setSourceError("");
    try {
      const result = await onOpenExternal(url);
      if (result.status !== "success") setSourceError(result.reason);
    } catch (error) {
      setSourceError(error instanceof Error ? error.message : "The source could not be opened.");
    } finally {
      setOpeningSource(false);
    }
  };

  return (
    <aside
      aria-labelledby={titleId}
      className="min-w-0 overflow-y-auto bg-muted max-[680px]:h-full"
    >
      <header className="flex min-h-[69px] items-center justify-between gap-3 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <span className="font-mono text-label font-medium uppercase tracking-[0.06em] text-subtle-foreground">
            {inspector.kind === "citation" ? "Source" : "Artifact preview"}
          </span>
          <h2 className="m-0 mt-1 truncate text-title-sm font-semibold text-foreground" id={titleId}>
            {inspector.value.title}
          </h2>
        </div>
        <Button aria-label="Close inspector" onClick={onClose} ref={closeButtonRef} size="icon" variant="ghost">
          <X size={18} />
        </Button>
      </header>

      {inspector.kind === "citation" ? (
        <div className="p-5 max-[680px]:p-4">
          <span className="flex items-center gap-2 text-body-sm font-medium text-info">
            <Globe2 size={15} /> {inspector.value.domain}
          </span>
          <blockquote className="my-4 border-l-2 border-info bg-card p-4 text-body leading-5 text-foreground">
            {inspector.value.excerpt}
          </blockquote>
          {inspector.value.publishedAt ? (
            <p className="m-0 mb-4 font-mono text-label text-subtle-foreground">
              Published {inspector.value.publishedAt}
            </p>
          ) : null}
          <Button
            aria-busy={openingSource}
            disabled={openingSource}
            onClick={() => void openSource(inspector.value.url)}
          >
            {openingSource
              ? <LoaderCircle className="animate-spin motion-reduce:animate-none" size={14} />
              : <ExternalLink size={14} />}
            Open source in browser
          </Button>
          {sourceError ? (
            <p className="m-0 mt-3 text-body-sm text-destructive" role="alert">{sourceError}</p>
          ) : null}
        </div>
      ) : (
        <div className="p-5 max-[680px]:p-4">
          <div className="mb-3 flex items-center justify-between gap-2">
            <span className="font-mono text-label tabular-nums text-subtle-foreground">
              Version {inspector.value.version}
            </span>
            <Button disabled size="sm" title="Artifact restore requires daemon protocol support" variant="outline">
              <RotateCcw size={14} /> Restore unavailable
            </Button>
          </div>
          <pre className="m-0 min-h-96 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-card p-4 font-mono text-body-sm leading-[18px] text-foreground">
            {inspector.value.content}
          </pre>
          <footer className="mt-3 flex justify-end gap-2 max-[680px]:flex-wrap">
            <Button disabled title="Opening artifacts requires daemon protocol support">Open unavailable</Button>
            <Button disabled title="Version comparison requires daemon protocol support" variant="outline">Compare unavailable</Button>
          </footer>
        </div>
      )}
    </aside>
  );
}

function ConversationUnavailable({
  reason,
  onBack,
  onSetup,
}: {
  reason: string;
  onBack(): void;
  onSetup(): void;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 bg-card p-6 text-center">
      <span className="grid size-12 place-items-center rounded-full bg-warning-soft text-warning" aria-hidden="true">
        <X size={22} />
      </span>
      <h1 className="m-0 mt-2 text-title font-semibold text-foreground">Conversation unavailable</h1>
      <p className="m-0 max-w-[60ch] text-body-lg text-muted-foreground" role="alert">{reason}</p>
      <div className="mt-3 flex gap-2 max-[680px]:w-full max-[680px]:flex-col">
        <Button onClick={onBack} variant="outline"><ArrowLeft size={15} /> Back to home</Button>
        <Button onClick={onSetup}>Open setup</Button>
      </div>
    </div>
  );
}
