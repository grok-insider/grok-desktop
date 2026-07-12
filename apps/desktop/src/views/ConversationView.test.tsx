import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { ClientResult, ConversationDetail, ConversationTurnDetail } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { ConversationView } from "./ConversationView";

class ConversationClient extends MockDesktopClient {
  private readonly testListeners = new Set<(conversation: ConversationDetail) => void>();

  constructor(private readonly loadConversation: () => Promise<ClientResult<ConversationDetail>>) {
    super();
  }

  override async getConversation(_threadId: string): Promise<ClientResult<ConversationDetail>> {
    return this.loadConversation();
  }

  override subscribeConversation(
    _threadId: string,
    listener: (conversation: ConversationDetail) => void,
  ): () => void {
    this.testListeners.add(listener);
    return () => this.testListeners.delete(listener);
  }

  publish(value: ConversationDetail): void {
    for (const listener of this.testListeners) listener(structuredClone(value));
  }
}

function renderConversation(client: MockDesktopClient) {
  return render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={["/conversations/thread-1"]}>
        <LocationProbe />
        <Routes>
          <Route path="/conversations/:threadId" element={<ConversationView />} />
          <Route path="/" element={<h1>Home</h1>} />
          <Route path="/setup" element={<h1>Setup destination</h1>} />
        </Routes>
      </MemoryRouter>
    </DesktopClientProvider>,
  );
}

function LocationProbe() {
  const location = useLocation();
  return <span data-testid="current-location" hidden>{location.pathname}</span>;
}

function conversation(overrides: Partial<ConversationDetail> = {}): ConversationDetail {
  return {
    id: "thread-1",
    title: "Durable conversation",
    projectName: "Atlas launch",
    mode: "chat",
    branchName: "Main",
    branchCount: 1,
    branches: [{
      threadId: "thread-1",
      label: "Main",
      title: "Durable conversation",
      kind: "main",
      forkDepth: 0,
      current: true,
    }],
    lineage: { origin: "original", rootThreadId: "thread-1", forkDepth: 0 },
    messages: [],
    turns: [],
    ...overrides,
  };
}

function success(value: ConversationDetail): ClientResult<ConversationDetail> {
  return { status: "success", value };
}

function completedConversation(): ConversationDetail {
  return conversation({
    messages: [
      {
        id: "message-user",
        role: "user",
        content: "Review the persisted launch plan.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      },
      {
        id: "message-assistant",
        role: "assistant",
        content: "The persisted launch plan is internally consistent.",
        state: "complete",
        createdAt: "10:15 AM",
        citations: [],
        attachments: [],
      },
    ],
    turns: [{
      id: "turn-1",
      state: "completed",
      revision: 2,
      modelId: "grok-4.3",
      userMessageId: "message-user",
      assistantMessageId: "message-assistant",
      usage: { inputTokens: 42, outputTokens: 24, costInUsdTicks: 7 },
      lineage: { origin: "original", retryDepth: 0 },
      retryEligibility: "source_completed",
    }],
  });
}

function childConversation(id: string, kind: "branch" | "edit_and_branch" | "regenerate"): ConversationDetail {
  const source = completedConversation();
  return {
    ...source,
    id,
    branchName: "Branch 2",
    branchCount: 2,
    branches: [
      { ...source.branches[0], current: false },
      {
        threadId: id,
        label: "Branch 2",
        title: source.title,
        kind,
        forkDepth: 1,
        current: true,
      },
    ],
    lineage: {
      origin: "fork",
      rootThreadId: "thread-1",
      parentThreadId: "thread-1",
      sourceTurnId: "turn-1",
      sourceMessageId: kind === "edit_and_branch" ? "message-user" : "message-assistant",
      kind,
      forkDepth: 1,
    },
  };
}

describe("ConversationView", () => {
  it("renders an accessible transcript skeleton while the durable conversation loads", () => {
    const client = new ConversationClient(() => new Promise(() => undefined));
    const { container } = renderConversation(client);

    expect(screen.getByRole("status", { name: "Loading conversation" })).toHaveAttribute("aria-busy", "true");
    expect(container.querySelectorAll("[data-slot='skeleton']")).toHaveLength(9);
  });

  it("offers a single focusable compose action when the conversation is empty", async () => {
    const client = new ConversationClient(async () => success(conversation()));
    renderConversation(client);

    expect(await screen.findByRole("heading", { name: "Start this conversation" })).toBeInTheDocument();
    expect(screen.getByText("No messages have been submitted yet. Write the first message below.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Write the first message" }));
    expect(screen.getByLabelText("Reply to Grok")).toHaveFocus();
  });

  it("announces load errors and routes recovery through visible actions", async () => {
    const client = new ConversationClient(async () => ({
      status: "unavailable",
      reason: "The daemon could not read this conversation.",
    }));
    renderConversation(client);

    expect(await screen.findByRole("heading", { name: "Conversation unavailable" })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("The daemon could not read this conversation.");

    fireEvent.click(screen.getByRole("button", { name: "Open setup" }));
    expect(screen.getByRole("heading", { name: "Setup destination" })).toBeInTheDocument();
  });

  it("keeps submitted turns immutable and submits Enter as one asynchronous start request", async () => {
    let finishRequest: ((result: ClientResult<{ messageId: string; turnId: string }>) => void) | undefined;
    const client = new ConversationClient(async () => success(conversation({
      messages: [
        {
          id: "message-user",
          role: "user",
          content: "Review the persisted plan.",
          state: "complete",
          createdAt: "10:14 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-assistant",
          role: "assistant",
          content: "The persisted plan is internally consistent.",
          state: "complete",
          createdAt: "10:15 AM",
          citations: [],
          attachments: [],
        },
      ],
    })));
    const send = vi.spyOn(client, "sendConversationMessage").mockImplementation(
      () => new Promise((resolve) => {
        finishRequest = resolve;
      }),
    );
    renderConversation(client);

    expect(await screen.findByText("Review the persisted plan.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Editing is unavailable for inherited prompts" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Regeneration is unavailable for inherited responses" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Branching is unavailable for inherited responses" })).toBeDisabled();
    expect(screen.queryByText("Streaming")).not.toBeInTheDocument();

    const prompt = screen.getByLabelText("Reply to Grok");
    fireEvent.click(screen.getByRole("button", { name: "Enable Search" }));
    fireEvent.change(prompt, { target: { value: "Submit this durable turn" } });
    fireEvent.keyDown(prompt, { key: "Enter", shiftKey: true });
    expect(send).not.toHaveBeenCalled();

    fireEvent.keyDown(prompt, { key: "Enter" });
    expect(send).toHaveBeenCalledOnce();
    expect(send).toHaveBeenCalledWith("thread-1", "Submit this durable turn", [], true);
    expect(screen.getByRole("button", { name: "Send reply" })).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("status", { name: "" })).toHaveTextContent("Submitting the durable Grok request");

    await act(async () => {
      finishRequest?.({
        status: "success",
        value: { messageId: "message-user-2", turnId: "turn-2" },
      });
    });
    expect(prompt).toHaveValue("");
    expect(screen.getByRole("button", { name: "Enable Search" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: "Send reply" })).toHaveAttribute("aria-busy", "false");
  });

  it("navigates between daemon-owned conversation branches from the branch selector", async () => {
    const source = completedConversation();
    const client = new ConversationClient(async () => success({
      ...source,
      branchCount: 2,
      branches: [
        source.branches[0],
        {
          threadId: "thread-child-select",
          label: "Branch 2",
          title: source.title,
          kind: "branch",
          forkDepth: 1,
          current: false,
        },
      ],
    }));
    renderConversation(client);

    const selector = await screen.findByRole("combobox", { name: "Conversation branch" });
    expect(selector).toHaveValue("thread-1");
    expect(screen.getByText("2", { selector: "[data-slot='badge']" })).toBeInTheDocument();

    fireEvent.change(selector, { target: { value: "thread-child-select" } });
    await waitFor(() => {
      expect(screen.getByTestId("current-location")).toHaveTextContent("/conversations/thread-child-select");
    });
  });

  it("creates a provider-free branch from the exact completed assistant response without confirmation", async () => {
    const parent = completedConversation();
    const parentBefore = structuredClone(parent);
    const client = new ConversationClient(async () => success(parent));
    const branch = vi.spyOn(client, "branchConversation").mockResolvedValue(
      success(childConversation("thread-child-branch", "branch")),
    );
    renderConversation(client);

    const createBranch = await screen.findByRole("button", {
      name: "New branch from latest completed response",
    });
    expect(createBranch).toHaveAttribute(
      "title",
      "Create a child conversation without sending a new xAI request",
    );
    fireEvent.click(createBranch);

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(branch).toHaveBeenCalledWith("thread-1", "message-assistant");
      expect(screen.getByTestId("current-location")).toHaveTextContent("/conversations/thread-child-branch");
    });
    expect(parent).toEqual(parentBefore);
  });

  it("edits an exact prompt into a confirmed billable child branch and restores focus on Escape", async () => {
    const parent = completedConversation();
    const parentBefore = structuredClone(parent);
    const client = new ConversationClient(async () => success(parent));
    const edit = vi.spyOn(client, "editConversationMessage").mockResolvedValue(
      success(childConversation("thread-child-edit", "edit_and_branch")),
    );
    renderConversation(client);

    const trigger = await screen.findByRole("button", { name: "Edit prompt in a new branch" });
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "Edit prompt in a new branch" });
    expect(dialog).toHaveTextContent("The parent conversation stays unchanged.");
    expect(dialog).toHaveTextContent("sends a new billable request to xAI");
    const editedPrompt = screen.getByRole("textbox", { name: "Edited prompt" });
    await waitFor(() => expect(editedPrompt).toHaveFocus());
    expect(editedPrompt).toHaveValue("Review the persisted launch plan.");

    const confirm = screen.getByRole("button", { name: "Create edited branch" });
    expect(confirm).toBeDisabled();
    fireEvent.change(editedPrompt, { target: { value: "" } });
    expect(confirm).toBeDisabled();
    fireEvent.change(editedPrompt, { target: { value: "Review the persisted launch plan." } });
    expect(confirm).toBeDisabled();
    fireEvent.change(editedPrompt, { target: { value: "Review the persisted launch plan with a risk table." } });
    expect(confirm).toBeEnabled();

    fireEvent.keyDown(document, { key: "Escape", code: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(edit).not.toHaveBeenCalled();

    fireEvent.click(trigger);
    const reopenedPrompt = await screen.findByRole("textbox", { name: "Edited prompt" });
    await waitFor(() => expect(reopenedPrompt).toHaveFocus());
    expect(reopenedPrompt).toHaveValue("Review the persisted launch plan.");
    fireEvent.change(reopenedPrompt, {
      target: { value: "Review the persisted launch plan with a risk table." },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create edited branch" }));

    await waitFor(() => {
      expect(edit).toHaveBeenCalledWith(
        "thread-1",
        "message-user",
        "Review the persisted launch plan with a risk table.",
      );
      expect(screen.getByTestId("current-location")).toHaveTextContent("/conversations/thread-child-edit");
    });
    expect(parent).toEqual(parentBefore);
  });

  it("keeps a failed Regenerate confirmation open with its exact unavailable reason", async () => {
    const parent = completedConversation();
    const parentBefore = structuredClone(parent);
    const client = new ConversationClient(async () => success(parent));
    const regenerate = vi.spyOn(client, "regenerateConversationMessage").mockResolvedValue({
      status: "unavailable",
      reason: "The recorded xAI account generation is no longer available.",
    });
    renderConversation(client);

    const trigger = await screen.findByRole("button", { name: "Regenerate response in a new branch" });
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "Regenerate in a new branch?" });
    expect(dialog).toHaveTextContent("The parent conversation stays unchanged.");
    expect(dialog).toHaveTextContent("sends a new billable request to xAI");
    expect(dialog).toHaveTextContent("same recorded prompt and frozen context again");
    fireEvent.click(screen.getByRole("button", { name: "Regenerate response" }));

    await waitFor(() => {
      expect(regenerate).toHaveBeenCalledWith("thread-1", "message-assistant");
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The recorded xAI account generation is no longer available.",
    );
    expect(screen.getByRole("dialog", { name: "Regenerate in a new branch?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Regenerate response" })).toBeEnabled();
    expect(screen.getByTestId("current-location")).toHaveTextContent("/conversations/thread-1");
    expect(parent).toEqual(parentBefore);
  });

  it("keeps active, review-required, and inherited fork actions disabled with honest labels", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [
        {
          id: "message-active-user",
          role: "user",
          content: "Wait for this active request.",
          state: "complete",
          createdAt: "10:14 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-active-assistant",
          role: "assistant",
          content: "A durable response is still arriving.",
          state: "streaming",
          createdAt: "10:15 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-review-user",
          role: "user",
          content: "Review this uncertain request.",
          state: "complete",
          createdAt: "10:16 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-review-assistant",
          role: "assistant",
          content: "This partial output needs review.",
          state: "stopped",
          createdAt: "10:17 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-inherited-user",
          role: "user",
          content: "This prompt was inherited into the child.",
          state: "complete",
          createdAt: "10:18 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-inherited-assistant",
          role: "assistant",
          content: "This response was inherited into the child.",
          state: "complete",
          createdAt: "10:19 AM",
          citations: [],
          attachments: [],
        },
      ],
      turns: [
        {
          id: "turn-active",
          state: "provider_started",
          revision: 1,
          modelId: "grok-4.3",
          userMessageId: "message-active-user",
          assistantMessageId: "message-active-assistant",
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "source_in_progress",
        },
        {
          id: "turn-review",
          state: "interrupted_needs_review",
          revision: 2,
          modelId: "grok-4.3",
          userMessageId: "message-review-user",
          assistantMessageId: "message-review-assistant",
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "source_interrupted_needs_review",
        },
      ],
    })));
    renderConversation(client);

    await screen.findByText("Wait for this active request.");
    for (const label of [
      "Editing is unavailable while this request is active",
      "Regeneration is unavailable while this response is active",
      "Branching is unavailable while this response is active",
      "Editing is unavailable while this request needs review",
      "Regeneration is unavailable while this response needs review",
      "Branching is unavailable while this response needs review",
      "Editing is unavailable for inherited prompts",
      "Regeneration is unavailable for inherited responses",
      "Branching is unavailable for inherited responses",
    ]) {
      expect(screen.getByRole("button", { name: label })).toBeDisabled();
    }
    expect(screen.getByText("Inherited prompt · editing unavailable")).toBeVisible();
    expect(screen.getByText("Inherited response · fork actions unavailable")).toBeVisible();
    expect(screen.getByRole("button", { name: "New branch from latest completed response" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New branch from latest completed response" })).toHaveAttribute(
      "title",
      "A completed Grok response is required",
    );
  });

  it("keeps interrupted_needs_review explicit and never offers an automatic retry", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-user",
        role: "user",
        content: "Perform the request once.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      }],
      turns: [{
        id: "turn-1",
        state: "interrupted_needs_review",
        revision: 2,
        modelId: "daemon-owned-model",
        userMessageId: "message-user",
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: { origin: "original", retryDepth: 0 },
        retryEligibility: "source_interrupted_needs_review",
      }],
    })));
    renderConversation(client);

    const outcome = await screen.findByRole("alert");
    expect(outcome).toHaveAttribute("data-turn-state", "interrupted_needs_review");
    expect(outcome).toHaveTextContent("Review required");
    expect(outcome).toHaveTextContent("may have reached xAI");
    expect(outcome).toHaveTextContent("cannot be retried automatically");
    expect(screen.queryByRole("button", { name: /retry/i })).not.toBeInTheDocument();
    expect(screen.queryByText("daemon-owned-model")).not.toBeInTheDocument();
  });

  it("projects progressive durable text into one stable assistant row", async () => {
    const first = conversation({
      messages: [
        {
          id: "message-user",
          role: "user",
          content: "Stream the durable response.",
          state: "complete",
          createdAt: "10:14 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "conversation-stream-turn-1",
          role: "assistant",
          content: "First durable chunk",
          state: "streaming",
          createdAt: "10:14 AM",
          citations: [],
          attachments: [],
        },
      ],
      turns: [{
        id: "turn-1",
        state: "provider_started",
        revision: 1,
        modelId: "grok-4.3",
        userMessageId: "message-user",
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: { origin: "original", retryDepth: 0 },
        retryEligibility: "source_in_progress",
      }],
    });
    const client = new ConversationClient(async () => success(first));
    renderConversation(client);

    const firstChunk = await screen.findByText("First durable chunk");
    const assistantRow = firstChunk.closest("article");
    expect(assistantRow).not.toBeNull();
    expect(firstChunk.closest("[aria-live='polite']")).toBeNull();
    expect(screen.getByText("Grok is responding.")).toHaveClass("sr-only");
    expect(screen.getByText("Streaming")).toBeInTheDocument();

    const viewport = screen.getByTestId("conversation-transcript-viewport");
    Object.defineProperties(viewport, {
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, value: 100, writable: true },
      clientHeight: { configurable: true, value: 400 },
    });
    fireEvent.scroll(viewport);
    expect(screen.getByRole("button", { name: "Jump to latest" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Jump to latest" }));
    expect(screen.queryByRole("button", { name: "Jump to latest" })).not.toBeInTheDocument();

    client.publish(conversation({
      ...first,
      messages: first.messages.map((message) => message.role === "assistant"
        ? { ...message, content: "First durable chunk, followed by the next durable chunk." }
        : message),
    }));
    const expanded = await screen.findByText("First durable chunk, followed by the next durable chunk.");
    expect(expanded.closest("article")).toBe(assistantRow);
  });

  it("offers an exact Stop control and distinguishes post-dispatch review", async () => {
    const active = conversation({
      messages: [{
        id: "message-user",
        role: "user",
        content: "Stop this once.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      }],
      turns: [
        {
          id: "turn-stale-recovery",
          state: "reserved",
          revision: 0,
          modelId: "grok-4.3",
          userMessageId: "message-user",
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "source_in_progress",
        },
        {
          id: "turn-1",
          state: "provider_started",
          revision: 1,
          modelId: "grok-4.3",
          userMessageId: "message-user",
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "source_in_progress",
        },
      ],
    });
    const client = new ConversationClient(async () => success(active));
    const cancel = vi.spyOn(client, "cancelConversationTurn").mockResolvedValue({
      status: "success",
      value: { ...active.turns[1], state: "interrupted_needs_review", revision: 2 },
    });
    renderConversation(client);

    const stop = await screen.findByRole("button", { name: "Stop Grok response" });
    expect(stop).toHaveAttribute("title", "Stop generation and mark the dispatched request for review");
    fireEvent.click(stop);
    await waitFor(() => expect(cancel).toHaveBeenCalledWith({ turnId: "turn-1", expectedRevision: 1 }));
    expect(await screen.findByText(/cannot be retried automatically/i)).toBeInTheDocument();
  });

  it("labels pre-dispatch cancellation separately from uncertain interruption", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-user",
        role: "user",
        content: "Cancel before dispatch.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      }],
      turns: [{
        id: "turn-1",
        state: "cancelled",
        revision: 1,
        modelId: "grok-4.3",
        userMessageId: "message-user",
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: { origin: "original", retryDepth: 0 },
        retryEligibility: "allowed",
      }],
    })));
    renderConversation(client);

    const outcome = await screen.findByText("Grok request cancelled");
    expect(outcome.closest("[data-turn-state='cancelled']")).toHaveTextContent(
      "cancelled before a response was committed",
    );
    expect(screen.queryByText("Review required")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry request" })).toBeEnabled();
  });

  it("submits an exact eligible Retry with an announced busy state", async () => {
    let finishRetry: ((result: ClientResult<ConversationTurnDetail>) => void) | undefined;
    const source: ConversationTurnDetail = {
      id: "turn-failed",
      state: "failed",
      revision: 2,
      modelId: "grok-4.3",
      userMessageId: "message-user",
      failure: { kind: "unavailable", message: "The provider is unavailable.", retryable: true },
      usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
      lineage: { origin: "original", retryDepth: 0 },
      retryEligibility: "allowed",
    };
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-user",
        role: "user",
        content: "Retry this exact request.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      }],
      turns: [source],
    })));
    const retry = vi.spyOn(client, "retryConversationTurn").mockImplementation(
      () => new Promise((resolve) => {
        finishRetry = resolve;
      }),
    );
    renderConversation(client);

    const button = await screen.findByRole("button", { name: "Retry request" });
    fireEvent.click(button);
    expect(retry).toHaveBeenCalledWith({ sourceTurnId: "turn-failed", expectedRevision: 2 });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("status")).toHaveTextContent("Retrying request");

    await act(async () => {
      finishRetry?.({
        status: "success",
        value: {
          ...source,
          id: "turn-retried",
          state: "reserved",
          revision: 0,
          userMessageId: "message-user-retried",
          failure: undefined,
          lineage: { origin: "retry", sourceTurnId: source.id, retryDepth: 1 },
          retryEligibility: "source_in_progress",
        },
      });
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Retry request submitted.");
  });

  it("keeps non-retryable failures actionless", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-user",
        role: "user",
        content: "Do not repeat this invalid request.",
        state: "complete",
        createdAt: "10:14 AM",
        citations: [],
        attachments: [],
      }],
      turns: [{
        id: "turn-failed",
        state: "failed",
        revision: 2,
        modelId: "grok-4.3",
        userMessageId: "message-user",
        failure: { kind: "invalid_request", message: "The request is invalid.", retryable: false },
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: { origin: "original", retryDepth: 0 },
        retryEligibility: "failure_not_retryable",
      }],
    })));
    renderConversation(client);

    expect(await screen.findByText("Grok request failed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry request" })).not.toBeInTheDocument();
  });

  it("labels a retried user request without exposing account identifiers", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [
        {
          id: "message-source",
          role: "user",
          content: "Retry this exact request.",
          state: "complete",
          createdAt: "10:14 AM",
          citations: [],
          attachments: [],
        },
        {
          id: "message-retry",
          role: "user",
          content: "Retry this exact request.",
          state: "complete",
          createdAt: "10:16 AM",
          citations: [],
          attachments: [],
        },
      ],
      turns: [
        {
          id: "turn-source",
          state: "failed",
          revision: 2,
          modelId: "grok-4.3",
          userMessageId: "message-source",
          failure: { kind: "unavailable", message: "Provider unavailable.", retryable: true },
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "not_newest",
        },
        {
          id: "turn-retry",
          state: "provider_started",
          revision: 1,
          modelId: "grok-4.3",
          userMessageId: "message-retry",
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "retry", sourceTurnId: "turn-source", retryDepth: 1 },
          retryEligibility: "source_in_progress",
        },
      ],
    })));
    renderConversation(client);

    expect(await screen.findByText("Retry 1")).toBeInTheDocument();
    expect(screen.queryByText(/credential-generation|account id/i)).not.toBeInTheDocument();
  });

  it("focuses the responsive inspector and restores its trigger on Escape", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-assistant",
        role: "assistant",
        content: "The finding is supported by a durable source record.",
        state: "complete",
        createdAt: "10:15 AM",
        citations: [{
          id: "citation-1",
          title: "Customer research",
          url: "https://example.com/research",
          domain: "example.com",
          excerpt: "The persisted source excerpt.",
        }],
        attachments: [],
      }],
    })));
    renderConversation(client);

    const trigger = await screen.findByRole("button", { name: /Customer research/ });
    trigger.focus();
    fireEvent.click(trigger);

    const close = await screen.findByRole("button", { name: "Close inspector" });
    await waitFor(() => expect(close).toHaveFocus());
    expect(screen.getByRole("complementary", { name: "Customer research" })).toHaveClass("max-[680px]:h-full");
    expect(screen.getByRole("region", { name: "Conversation transcript" })).toHaveClass("max-[680px]:hidden");
    expect(screen.getByTestId("conversation-workspace")).toHaveClass("max-[680px]:block");

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("button", { name: "Close inspector" })).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("opens citation URLs only after the explicit native-broker action", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-assistant",
        role: "assistant",
        content: "Use the cited source.",
        state: "complete",
        createdAt: "10:15 AM",
        citations: [{
          id: "citation-1",
          title: "xAI documentation",
          url: "https://docs.x.ai/docs/guides#sources",
          domain: "docs.x.ai",
          excerpt: "A canonical source excerpt.",
        }],
        attachments: [],
      }],
    })));
    const openExternal = vi.spyOn(client, "openExternalUrl").mockResolvedValue({
      status: "success",
      value: undefined,
    });
    const { container } = renderConversation(client);

    fireEvent.click(await screen.findByRole("button", { name: /xAI documentation/ }));
    expect(container.querySelector("a[href='https://docs.x.ai/docs/guides#sources']")).toBeNull();
    expect(openExternal).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Open source in browser" }));
    await waitFor(() => expect(openExternal).toHaveBeenCalledWith("https://docs.x.ai/docs/guides#sources"));
  });

  it("keeps a rejected untrusted citation inside the inspector and reports the broker failure", async () => {
    const client = new ConversationClient(async () => success(conversation({
      messages: [{
        id: "message-assistant",
        role: "assistant",
        content: "This citation is untrusted provider output.",
        state: "complete",
        createdAt: "10:15 AM",
        citations: [{
          id: "citation-unsafe",
          title: "Unsafe local source",
          url: "file:///tmp/source.html",
          domain: "",
          excerpt: "Untrusted source metadata.",
        }],
        attachments: [],
      }],
    })));
    vi.spyOn(client, "openExternalUrl").mockResolvedValue({
      status: "unavailable",
      reason: "External URL must be canonical credential-free HTTPS.",
    });
    const { container } = renderConversation(client);

    fireEvent.click(await screen.findByRole("button", { name: /Unsafe local source/ }));
    expect(container.querySelector("a[href^='file:']")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open source in browser" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "External URL must be canonical credential-free HTTPS.",
    );
  });
});
