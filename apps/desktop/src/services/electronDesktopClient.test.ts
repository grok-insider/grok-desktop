import { describe, expect, it, vi } from "vitest";
import type {
  BridgeRequest,
  BridgeResponse,
  DaemonArtifact,
  DaemonArtifactOpenReceipt,
  DaemonConversationFork,
  DaemonConversationForkMetadata,
  DaemonConversationTurn,
  DesktopConversationTurnEventNotification,
  DaemonMessage,
  DaemonThread,
  DesktopBridge,
} from "../contracts/bridge";
import { ElectronDesktopClient } from "./electronDesktopClient";
import type { ConversationDetail } from "./desktopClient";
import {
  GROK_EXECUTION_UNAVAILABLE_REASON,
} from "./productAvailability";

const connected = {
  state: "connected" as const,
  serviceVersion: "0.1.0",
  protocolVersion: 16,
  instanceId: "daemon-test",
  automationScheduler: { state: "kernel_initialized_execution_disabled" as const },
  updatedAtUnixMs: 1,
};

describe("ElectronDesktopClient", () => {
  it("routes explicit external navigation through the narrow preload request", async () => {
    const bridge = fakeBridge(async (request) => {
      expect(request).toEqual({
        kind: "desktop.openExternalUrl",
        url: "https://docs.x.ai/docs/guides#sources",
      });
      return { kind: "desktop.externalUrlOpened", accepted: true };
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.openExternalUrl("https://docs.x.ai/docs/guides#sources")).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(bridge.request).toHaveBeenCalledOnce();
  });

  it("fails closed on an unexpected external-navigation bridge response", async () => {
    const bridge = fakeBridge(async () => ({
      kind: "runtime.info",
      platform: "test",
      version: "0",
    }));
    const client = new ElectronDesktopClient(bridge);

    await expect(client.openExternalUrl("https://docs.x.ai/")).rejects.toThrow(
      "invalid external-URL bridge response",
    );
  });

  it("maps native external-navigation rejection to a renderer-safe result", async () => {
    const bridge = fakeBridge(async () => ({
      kind: "desktop.externalUrlOpenFailed",
      reason: "rejected",
    }));
    const client = new ElectronDesktopClient(bridge);

    await expect(client.openExternalUrl("file:///tmp/source.html")).resolves.toEqual({
      status: "unavailable",
      reason: "This source URL is not an allowed canonical public HTTPS address.",
    });
  });

  it("does not expose native IPC rejection details while opening a source", async () => {
    const bridge = fakeBridge(async () => {
      throw new Error("native failure at /private/runtime/path");
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.openExternalUrl("https://docs.x.ai/")).resolves.toEqual({
      status: "unavailable",
      reason: "The operating system could not open this source.",
    });
  });

  it("keeps Work disabled even if an inconsistent daemon reports it available", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.startRun({
      prompt: "Prepare a release brief",
      mode: "work",
      searchEnabled: false,
      researchEnabled: false,
    })).rejects.toThrow(GROK_EXECUTION_UNAVAILABLE_REASON);

    expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual(["daemon.bootstrap"]);
    const snapshot = await client.getSnapshot();
    expect(snapshot.runs).toEqual([]);
    expect(snapshot.capabilities.find((item) => item.id === "chat")).toMatchObject({
      available: true,
      reasonCode: "ready",
    });
    expect(snapshot.capabilities.find((item) => item.id === "work")).toMatchObject({
      available: false,
      reasonCode: "execution_use_case_unavailable",
    });
  });

  it("keeps Automations limited even if an inconsistent daemon reports it available", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.capabilities.push({
      id: "automations",
      label: "Automations",
      source: "desktop",
      authentication: "none",
      availability: "available",
      reasonCode: "ready",
      reason: "Available.",
    });
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const snapshot = await client.getSnapshot();

    expect(snapshot.capabilities.find((item) => item.id === "automations")).toMatchObject({
      available: false,
      availability: "limited",
      reasonCode: "automation_execution_unqualified",
      reason: "The scheduler journal is initialized, but isolated automation execution is not qualified.",
    });
  });

  it("starts durable BYOK Chat asynchronously and restores official citations", async () => {
    const thread = conversationThread();
    const turn = completedTurn(thread);
    const started = activeTurn(thread);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") {
        expect(request).toMatchObject({ projectId: "inbox", title: "Summarize this release plan." });
        return { kind: "daemon.thread", thread };
      }
      if (request.kind === "daemon.startConversationTurn") {
        expect(request).toMatchObject({
          threadId: thread.id,
          content: "Summarize this release plan.",
          modelId: "grok-alternative",
        });
        expect(request.idempotencyKey).toBeTruthy();
        return { kind: "daemon.conversationTurn", turn: started };
      }
      if (request.kind === "daemon.getConversation") {
        return {
          kind: "daemon.conversation",
          thread,
          messages: [turn.userMessage, turn.assistantMessage!],
          turns: [turn],
          forkMetadata: conversationForkMetadata(thread),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.startRun({
      prompt: "Summarize this release plan.",
      mode: "chat",
      projectId: "inbox",
      modelId: "grok-alternative",
      searchEnabled: false,
      researchEnabled: false,
    })).resolves.toEqual({ runId: "run-chat-1", threadId: thread.id });

    const conversation = await client.getConversation(thread.id);
    expect(conversation).toMatchObject({
      status: "success",
      value: {
        turns: [{ id: "turn-chat-1", state: "completed", zeroDataRetention: true }],
        messages: [
          { id: "message-user-1", role: "user", citations: [] },
          { id: "message-assistant-1", role: "assistant", citations: [{ title: "Release guide", domain: "docs.x.ai" }] },
        ],
      },
    });
  });

  it("reuses the composite create key after a lost new-thread response", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const createKeys: string[] = [];
    const startKeys: string[] = [];
    const startThreads: string[] = [];
    let createAttempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") {
        createKeys.push(request.idempotencyKey);
        createAttempts += 1;
        if (createAttempts === 1) throw new Error("create-thread response lost");
        return { kind: "daemon.thread", thread };
      }
      if (request.kind === "daemon.startConversationTurn") {
        startKeys.push(request.idempotencyKey);
        startThreads.push(request.threadId);
        return { kind: "daemon.conversationTurn", turn: started };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const input = {
      prompt: "Summarize this release plan.",
      mode: "chat" as const,
      projectId: "inbox",
      searchEnabled: false,
      researchEnabled: false,
    };

    await expect(client.startRun(input)).rejects.toThrow("create-thread response lost");
    await expect(client.startRun(input)).resolves.toEqual({
      runId: started.run.id,
      threadId: thread.id,
    });

    expect(createKeys).toHaveLength(2);
    expect(createKeys[1]).toBe(createKeys[0]);
    expect(startKeys).toHaveLength(1);
    expect(startThreads).toEqual([thread.id]);
  });

  it("reuses the composite thread and Start key after a lost new-chat Start response", async () => {
    const thread = conversationThread();
    const committedTurn = activeTurn(thread);
    const createKeys: string[] = [];
    const startKeys: string[] = [];
    const startThreads: string[] = [];
    let startAttempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") {
        createKeys.push(request.idempotencyKey);
        return { kind: "daemon.thread", thread };
      }
      if (request.kind === "daemon.startConversationTurn") {
        startKeys.push(request.idempotencyKey);
        startThreads.push(request.threadId);
        startAttempts += 1;
        if (startAttempts === 1) throw new Error("new-chat Start response lost");
        return { kind: "daemon.conversationTurn", turn: committedTurn };
      }
      if (request.kind === "daemon.getConversation") {
        // The best-effort refresh is also ambiguous and deliberately releases
        // the per-thread Start cache; the composite must still retain its key.
        return {
          kind: "daemon.conversation",
          thread,
          messages: [],
          turns: [],
          forkMetadata: conversationForkMetadata(thread),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const input = {
      prompt: "Summarize this release plan.",
      mode: "chat" as const,
      projectId: "inbox",
      searchEnabled: false,
      researchEnabled: false,
    };

    await expect(client.startRun(input)).rejects.toThrow("new-chat Start response lost");
    await expect(client.startRun(input)).resolves.toEqual({
      runId: committedTurn.run.id,
      threadId: thread.id,
    });

    expect(createKeys).toHaveLength(1);
    expect(startKeys).toHaveLength(2);
    expect(startKeys[1]).toBe(startKeys[0]);
    expect(startThreads).toEqual([thread.id, thread.id]);
  });

  it("replaces an ambiguous new-chat composite when canonical input changes", async () => {
    const secondThread = { ...conversationThread(), id: "thread-chat-2", title: "A different prompt" };
    const secondTurn = activeTurn(secondThread);
    const createRequests: Extract<BridgeRequest, { kind: "daemon.createThread" }>[] = [];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") {
        createRequests.push(request);
        if (createRequests.length === 1) throw new Error("first create response lost");
        return { kind: "daemon.thread", thread: secondThread };
      }
      if (request.kind === "daemon.startConversationTurn") {
        expect(request).toMatchObject({ threadId: secondThread.id, content: "A different prompt" });
        return { kind: "daemon.conversationTurn", turn: secondTurn };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const common = {
      mode: "chat" as const,
      projectId: "inbox",
      searchEnabled: false,
      researchEnabled: false,
    };

    await expect(client.startRun({ ...common, prompt: "Summarize this release plan." }))
      .rejects.toThrow("first create response lost");
    await expect(client.startRun({ ...common, prompt: "A different prompt" })).resolves.toEqual({
      runId: secondTurn.run.id,
      threadId: secondThread.id,
    });

    expect(createRequests).toHaveLength(2);
    expect(createRequests[1].idempotencyKey).not.toBe(createRequests[0].idempotencyKey);
    expect(createRequests[1]).toMatchObject({ title: "A different prompt", projectId: "inbox" });
  });

  it("clears the composite after a definitive terminal Start response", async () => {
    const threads = [
      { ...conversationThread(), id: "thread-terminal-1" },
      { ...conversationThread(), id: "thread-terminal-2" },
    ];
    const failedTurns = threads.map((thread, index): DaemonConversationTurn => {
      const base = completedTurn(thread);
      return {
        ...base,
        turnId: `turn-terminal-${index + 1}`,
        state: "failed",
        assistantMessage: undefined,
        run: { ...base.run, id: `run-terminal-${index + 1}`, state: "failed" },
        failure: { kind: "unavailable", message: "The provider is unavailable.", retryable: true },
        citations: [],
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        retryEligibility: "allowed",
      };
    });
    const createKeys: string[] = [];
    const startKeys: string[] = [];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") {
        createKeys.push(request.idempotencyKey);
        return { kind: "daemon.thread", thread: threads[createKeys.length - 1] };
      }
      if (request.kind === "daemon.startConversationTurn") {
        startKeys.push(request.idempotencyKey);
        const index = threads.findIndex((thread) => thread.id === request.threadId);
        return { kind: "daemon.conversationTurn", turn: failedTurns[index] };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const input = {
      prompt: "Summarize this release plan.",
      mode: "chat" as const,
      projectId: "inbox",
      searchEnabled: false,
      researchEnabled: false,
    };

    await expect(client.startRun(input)).rejects.toThrow("The provider is unavailable.");
    await expect(client.startRun(input)).rejects.toThrow("The provider is unavailable.");

    expect(createKeys).toHaveLength(2);
    expect(createKeys[1]).not.toBe(createKeys[0]);
    expect(startKeys).toHaveLength(2);
    expect(startKeys[1]).not.toBe(startKeys[0]);
  });

  it("keeps a failed provider turn visible without inventing an assistant message", async () => {
    const thread = conversationThread();
    const turn: DaemonConversationTurn = {
      ...completedTurn(thread),
      state: "failed",
      assistantMessage: undefined,
      citations: [],
      failure: { kind: "authentication", message: "The xAI API key was rejected.", retryable: false },
      retryEligibility: "failure_not_retryable",
    };
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createThread") return { kind: "daemon.thread", thread };
      if (request.kind === "daemon.startConversationTurn") return { kind: "daemon.conversationTurn", turn };
      if (request.kind === "daemon.getConversation") {
        return {
          kind: "daemon.conversation",
          thread,
          messages: [turn.userMessage],
          turns: [turn],
          forkMetadata: conversationForkMetadata(thread),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.startRun({
      prompt: "Use the official API.",
      mode: "chat",
      projectId: "inbox",
      searchEnabled: false,
      researchEnabled: false,
    })).rejects.toThrow("The xAI API key was rejected.");

    const conversation = await client.getConversation(thread.id);
    expect(conversation.status === "success" && conversation.value.messages).toHaveLength(1);
    expect(conversation.status === "success" && conversation.value.turns[0]).toMatchObject({
      state: "failed",
      failure: { kind: "authentication" },
    });
  });

  it("projects durable text progressively and waits for canonical terminal reload before ACK", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const completed = completedTurn(thread);
    let conversationLoads = 0;
    let finishTerminalReload: ((response: BridgeResponse) => void) | undefined;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        conversationLoads += 1;
        if (conversationLoads === 1) {
          return conversationResponse(thread, started);
        }
        return new Promise((resolve) => {
          finishTerminalReload = resolve;
        });
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const updates: import("./desktopClient").ConversationDetail[] = [];
    client.subscribeConversation(thread.id, (conversation) => updates.push(conversation));
    await client.getConversation(thread.id);

    await deliverConversationEvents(bridge, conversationNotification(started.turnId, [
      { sequence: 1, turnId: started.turnId, kind: "created" },
      {
        sequence: 2,
        turnId: started.turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      {
        sequence: 3,
        turnId: started.turnId,
        kind: "text_appended",
        startUtf8Offset: 0,
        text: "The release plan ",
      },
      {
        sequence: 4,
        turnId: started.turnId,
        kind: "text_appended",
        startUtf8Offset: 17,
        text: "is ready.",
      },
    ], 4));
    expect(updates.at(-1)?.messages.at(-1)).toMatchObject({
      id: `conversation-stream-${started.turnId}`,
      content: "The release plan is ready.",
      state: "streaming",
    });

    let acknowledged = false;
    const terminalDelivery = deliverConversationEvents(bridge, conversationNotification(started.turnId, [{
      sequence: 5,
      turnId: started.turnId,
      kind: "state_changed",
      fromState: "provider_started",
      toState: "completed",
    }], 5)).then(() => {
      acknowledged = true;
    });
    await vi.waitFor(() => expect(conversationLoads).toBe(2));
    expect(acknowledged).toBe(false);

    finishTerminalReload?.(conversationResponse(thread, completed));
    await terminalDelivery;
    expect(acknowledged).toBe(true);
    expect(updates.at(-1)?.messages.at(-1)).toMatchObject({
      id: completed.assistantMessage?.id,
      content: "The release plan is ready.",
      state: "complete",
    });
    expect(updates.at(-1)?.turns[0]).toMatchObject({ state: "completed", revision: 2 });
  });

  it("retries canonical terminal reconciliation when the identical durable edge is replayed", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const completed = completedTurn(thread);
    let conversationLoads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        conversationLoads += 1;
        if (conversationLoads === 1) return conversationResponse(thread, started);
        if (conversationLoads === 2) throw new Error("transient canonical reload failure");
        return conversationResponse(thread, completed);
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const updates: import("./desktopClient").ConversationDetail[] = [];
    client.subscribeConversation(thread.id, (conversation) => updates.push(conversation));
    await client.getConversation(thread.id);
    const terminalReplay = conversationNotification(started.turnId, [
      { sequence: 1, turnId: started.turnId, kind: "created" },
      {
        sequence: 2,
        turnId: started.turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      { sequence: 3, turnId: started.turnId, kind: "text_appended", startUtf8Offset: 0, text: "The release plan is ready." },
      {
        sequence: 4,
        turnId: started.turnId,
        kind: "state_changed",
        fromState: "provider_started",
        toState: "completed",
      },
    ], 4);

    await expect(deliverConversationEvents(bridge, terminalReplay)).rejects.toThrow(
      "transient canonical reload failure",
    );
    await expect(deliverConversationEvents(bridge, terminalReplay)).resolves.toBeUndefined();

    expect(conversationLoads).toBe(3);
    expect(updates.at(-1)?.turns[0]).toMatchObject({ state: "completed", revision: 2 });
    expect(updates.at(-1)?.messages.at(-1)).toMatchObject({
      id: completed.assistantMessage?.id,
      content: "The release plan is ready.",
      state: "complete",
    });
  });

  it("retains two compact historical partial prefixes across later full reloads", async () => {
    const thread = conversationThread();
    const template = completedTurn(thread);
    const failedTurn = (
      turnId: string,
      userMessageId: string,
      sequence: number,
    ): DaemonConversationTurn => ({
      ...template,
      turnId,
      state: "failed",
      assistantMessage: undefined,
      userMessage: { ...template.userMessage, id: userMessageId, sequence },
      run: { ...template.run, id: `run-${turnId}`, state: "failed" },
      failure: { kind: "unavailable", message: "The provider stream ended.", retryable: true },
      citations: [],
      usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
      retryEligibility: sequence === 1 ? "not_newest" : "allowed",
    });
    const first = failedTurn("turn-failed-1", "message-user-failed-1", 1);
    const second = failedTurn("turn-failed-2", "message-user-failed-2", 2);
    const response: Extract<BridgeResponse, { kind: "daemon.conversation" }> = {
      kind: "daemon.conversation",
      thread,
      messages: [first.userMessage, second.userMessage],
      turns: [first, second],
      forkMetadata: conversationForkMetadata(thread),
    };
    let loads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return response;
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const updates: import("./desktopClient").ConversationDetail[] = [];
    client.subscribeConversation(thread.id, (conversation) => updates.push(conversation));
    await client.getConversation(thread.id);
    const terminalEvents = (turnId: string, text: string): DesktopConversationTurnEventNotification => (
      conversationNotification(turnId, [
        { sequence: 1, turnId, kind: "created" },
        {
          sequence: 2,
          turnId,
          kind: "state_changed",
          fromState: "reserved",
          toState: "provider_started",
        },
        { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text },
        {
          sequence: 4,
          turnId,
          kind: "state_changed",
          fromState: "provider_started",
          toState: "failed",
        },
      ], 4)
    );

    await deliverConversationEvents(bridge, terminalEvents(first.turnId, "first partial"));
    await deliverConversationEvents(bridge, terminalEvents(second.turnId, "second partial"));
    await client.getConversation(thread.id);

    const latest = updates.at(-1);
    expect(loads).toBe(4);
    expect(latest?.messages.find((message) => message.id === `conversation-stream-${first.turnId}`))
      .toMatchObject({ content: "first partial", state: "error" });
    expect(latest?.messages.find((message) => message.id === `conversation-stream-${second.turnId}`))
      .toMatchObject({ content: "second partial", state: "error" });
  });

  it("bounds aggregate compact terminal evidence retained by the renderer", () => {
    const bridge = fakeBridge(async () => bootstrapResponse());
    const client = new ElectronDesktopClient(bridge);
    const internal = client as unknown as {
      retainTerminalPrefix(projection: import("./conversationEventProjection").ConversationEventProjection): void;
    };
    const oneMiB = "x".repeat(1024 * 1024);
    for (let index = 0; index < 16; index += 1) {
      internal.retainTerminalPrefix({
        turnId: `turn-bounded-${index}`,
        state: "failed",
        revision: 2,
        text: oneMiB,
        textUtf8Bytes: oneMiB.length,
        textEventCount: 64,
        lastSequence: 67,
        deliveryCursor: 67,
        events: [],
      });
    }

    expect(() => internal.retainTerminalPrefix({
      turnId: "turn-bounded-overflow",
      state: "failed",
      revision: 2,
      text: "x",
      textUtf8Bytes: 1,
      textEventCount: 1,
      lastSequence: 4,
      deliveryCursor: 4,
      events: [],
    })).toThrow("terminal evidence exceeded the renderer recovery limit");
  });

  it("retries an exact eligible source and reconciles its canonical lineage", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const retried = retriedActiveTurn(thread, source);
    const retryKeys: string[] = [];
    let loads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return loads === 1
          ? retryConversationResponse(thread, source)
          : retryConversationResponse(thread, { ...source, retryEligibility: "not_newest" }, retried);
      }
      if (request.kind === "daemon.retryConversationTurn") {
        retryKeys.push(request.idempotencyKey);
        expect(request).toMatchObject({
          sourceTurnId: source.turnId,
          expectedRevision: source.revision,
        });
        expect(request).not.toHaveProperty("content");
        expect(request).not.toHaveProperty("modelId");
        return { kind: "daemon.conversationTurn", turn: retried };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
    await client.getConversation(thread.id);

    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).resolves.toMatchObject({
      status: "success",
      value: {
        id: retried.turnId,
        lineage: { origin: "retry", sourceTurnId: source.turnId, retryDepth: 1 },
      },
    });
    expect(retryKeys).toHaveLength(1);
    expect(loads).toBe(2);
    expect((client as unknown as { conversationRetryMutations: Map<string, unknown> })
      .conversationRetryMutations.size).toBe(0);
    unsubscribe();
  });

  it("retains one Retry key when both the response and canonical refresh are ambiguous", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const retried = retriedActiveTurn(thread, source);
    const retryKeys: string[] = [];
    let loads = 0;
    let attempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        if (loads === 2) throw new Error("canonical refresh unavailable");
        return loads === 1
          ? retryConversationResponse(thread, source)
          : retryConversationResponse(thread, { ...source, retryEligibility: "not_newest" }, retried);
      }
      if (request.kind === "daemon.retryConversationTurn") {
        retryKeys.push(request.idempotencyKey);
        attempts += 1;
        if (attempts === 1) throw new Error("retry response lost");
        return { kind: "daemon.conversationTurn", turn: retried };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
    await client.getConversation(thread.id);

    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).rejects.toThrow("retry response lost");
    expect((client as unknown as { conversationRetryMutations: Map<string, unknown> })
      .conversationRetryMutations.size).toBe(1);
    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).resolves.toMatchObject({ status: "success", value: { id: retried.turnId } });
    expect(retryKeys).toHaveLength(2);
    expect(retryKeys[1]).toBe(retryKeys[0]);
    expect((client as unknown as { conversationRetryMutations: Map<string, unknown> })
      .conversationRetryMutations.size).toBe(0);
    unsubscribe();
  });

  it("reconciles an ambiguous Retry response from canonical lineage without dispatching again", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const retried = retriedActiveTurn(thread, source);
    let loads = 0;
    let attempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return loads === 1
          ? retryConversationResponse(thread, source)
          : retryConversationResponse(thread, { ...source, retryEligibility: "not_newest" }, retried);
      }
      if (request.kind === "daemon.retryConversationTurn") {
        attempts += 1;
        throw new Error("retry response lost");
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
    await client.getConversation(thread.id);

    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).resolves.toMatchObject({ status: "success", value: { id: retried.turnId } });
    expect(attempts).toBe(1);
    expect(loads).toBe(2);
    unsubscribe();
  });

  it("rejects a Retry response that does not derive from the requested source", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const invalid = {
      ...retriedActiveTurn(thread, source),
      lineage: { origin: "original" as const, retryDepth: 0 as const },
    };
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") return retryConversationResponse(thread, source);
      if (request.kind === "daemon.retryConversationTurn") {
        return { kind: "daemon.conversationTurn", turn: invalid };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
    await client.getConversation(thread.id);

    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).rejects.toThrow("invalid lineage");
    unsubscribe();
  });

  it("rejects a Retry response that reuses the source message identity", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const invalid = retriedActiveTurn(thread, source);
    invalid.userMessage = source.userMessage;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") return retryConversationResponse(thread, source);
      if (request.kind === "daemon.retryConversationTurn") {
        return { kind: "daemon.conversationTurn", turn: invalid };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
    await client.getConversation(thread.id);

    await expect(client.retryConversationTurn({
      sourceTurnId: source.turnId,
      expectedRevision: source.revision,
    })).rejects.toThrow("invalid lineage");
    unsubscribe();
  });

  it("rejects inconsistent retry eligibility and self-referential lineage during mapping", async () => {
    const thread = conversationThread();
    const source = retryableFailedTurn(thread);
    const malformed: DaemonConversationTurn = {
      ...source,
      lineage: { origin: "retry", sourceTurnId: source.turnId, retryDepth: 1 },
      retryEligibility: "source_completed",
    };
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return retryConversationResponse(thread, malformed);
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.getConversation(thread.id)).rejects.toThrow(/lineage|eligibility/u);
  });

  it("branches a completed response into a provider-free canonical child with inherited citations", async () => {
    const fixture = conversationForkFixture("branch");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        expect(request).toMatchObject({
          sourceTurnId: fixture.parentTurn.turnId,
          expectedRevision: fixture.parentTurn.revision,
        });
        expect(request.idempotencyKey).toBeTruthy();
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    const loadedParent = await client.getConversation(fixture.parentThread.id);
    if (loadedParent.status !== "success") throw new Error("expected the parent conversation");
    const parentBefore = structuredClone(loadedParent.value);

    const result = await client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    );

    expect(result).toMatchObject({
      status: "success",
      value: {
        id: fixture.childThread.id,
        branchName: "Branch 1",
        branchCount: 2,
        lineage: {
          origin: "fork",
          parentThreadId: fixture.parentThread.id,
          sourceTurnId: fixture.parentTurn.turnId,
          kind: "branch",
        },
        branches: [
          { threadId: fixture.parentThread.id, label: "Main", current: false },
          { threadId: fixture.childThread.id, label: "Branch 1", current: true },
        ],
        messages: [
          { role: "user", content: fixture.parentTurn.userMessage.content, citations: [] },
          {
            role: "assistant",
            content: fixture.parentTurn.assistantMessage!.content,
            citations: [{
              title: "Release guide",
              domain: "docs.x.ai",
              url: "https://docs.x.ai/release",
            }],
          },
        ],
        turns: [],
      },
    });
    expect((client as unknown as { conversations: Map<string, unknown> }).conversations.get(
      fixture.parentThread.id,
    )).toEqual(parentBefore);
    expect(loadedParent.value).toEqual(parentBefore);
    unsubscribe();
  });

  it("accepts a completed Retry fork whose sealed context excludes its failed source prompt", async () => {
    const fixture = conversationForkFixture("branch");
    const failedSource = retryableFailedTurn(fixture.parentThread);
    failedSource.userMessage = {
      ...failedSource.userMessage,
      id: "message-user-failed-before-retry",
      sequence: 1,
      content: fixture.parentTurn.userMessage.content,
    };
    failedSource.retryEligibility = "not_newest";
    fixture.parentTurn.userMessage.sequence = 2;
    fixture.parentTurn.assistantMessage!.sequence = 3;
    fixture.parentTurn.lineage = {
      origin: "retry",
      sourceTurnId: failedSource.turnId,
      retryDepth: 1,
    };
    fixture.parentResponse.messages = [
      failedSource.userMessage,
      fixture.parentTurn.userMessage,
      fixture.parentTurn.assistantMessage!,
    ];
    fixture.parentResponse.turns = [failedSource, fixture.parentTurn];

    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({
      status: "success",
      value: {
        messages: [
          { role: "user", content: fixture.parentTurn.userMessage.content },
          { role: "assistant", content: fixture.parentTurn.assistantMessage!.content },
        ],
      },
    });
    unsubscribe();
  });

  it("preserves hidden system context in the exact raw Branch prefix", async () => {
    const fixture = conversationForkFixture("branch");
    const system: DaemonMessage = {
      ...fixture.parentTurn.userMessage,
      id: "message-system-hidden",
      sequence: 1,
      role: "system",
      content: "Keep the release evidence concise.",
    };
    fixture.parentTurn.userMessage.sequence = 2;
    fixture.parentTurn.assistantMessage!.sequence = 3;
    fixture.parentResponse.messages = [
      system,
      fixture.parentTurn.userMessage,
      fixture.parentTurn.assistantMessage!,
    ];
    const copiedSystem: DaemonMessage = {
      ...system,
      id: "message-system-hidden-copy",
      threadId: fixture.childThread.id,
      derivation: {
        origin: "fork",
        sourceMessageId: system.id,
        sourceTurnId: fixture.parentTurn.turnId,
        contextPosition: 1,
        kind: "context_copy",
      },
    };
    fixture.childResponse.messages[0].sequence = 2;
    if (fixture.childResponse.messages[0].derivation.origin !== "fork") throw new Error("fixture");
    fixture.childResponse.messages[0].derivation.contextPosition = 2;
    fixture.childResponse.messages[1].sequence = 3;
    fixture.childResponse.messages.unshift(copiedSystem);

    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({
      status: "success",
      value: { messages: [{ role: "user" }, { role: "assistant" }] },
    });
    unsubscribe();
  });

  it("preserves every prior completed turn in the exact raw Branch prefix", async () => {
    const fixture = conversationForkFixture("branch");
    prependCompletedContext(fixture);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({
      status: "success",
      value: { messages: [{ role: "user" }, { role: "assistant" }, { role: "user" }, { role: "assistant" }] },
    });
    unsubscribe();
  });

  it("rejects a reordered prior completed-turn context even with contiguous positions", async () => {
    const fixture = conversationForkFixture("branch");
    prependCompletedContext(fixture);
    const first = fixture.childResponse.messages[0];
    const second = fixture.childResponse.messages[1];
    first.sequence = 2;
    second.sequence = 1;
    if (first.derivation.origin !== "fork" || second.derivation.origin !== "fork") throw new Error("fixture");
    first.derivation.contextPosition = 2;
    second.derivation.contextPosition = 1;
    fixture.childResponse.messages[0] = second;
    fixture.childResponse.messages[1] = first;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow("does not preserve the source response");
    unsubscribe();
  });

  it("rejects a Branch that omits a hidden frozen-context message", async () => {
    const fixture = conversationForkFixture("branch");
    const system: DaemonMessage = {
      ...fixture.parentTurn.userMessage,
      id: "message-system-omitted",
      sequence: 1,
      role: "system",
      content: "This instruction must remain frozen.",
    };
    fixture.parentTurn.userMessage.sequence = 2;
    fixture.parentTurn.assistantMessage!.sequence = 3;
    fixture.parentResponse.messages = [system, fixture.parentTurn.userMessage, fixture.parentTurn.assistantMessage!];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow("does not preserve the source response");
    unsubscribe();
  });

  it("does not treat a renderer-only partial projection as frozen fork context", async () => {
    const fixture = conversationForkFixture("branch");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);
    const installed = (client as unknown as { conversations: Map<string, ConversationDetail> })
      .conversations.get(fixture.parentThread.id);
    if (!installed) throw new Error("parent conversation was not installed");
    installed.messages.push({
      id: "conversation-stream-uncertain-prior-turn",
      role: "assistant",
      content: "Presentation-only partial output",
      state: "stopped",
      createdAt: "now",
      citations: [],
      attachments: [],
    });

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({ status: "success", value: { id: fixture.childThread.id } });
    unsubscribe();
  });

  it("edits a completed prompt into a child-owned provider turn without mutating its parent", async () => {
    const fixture = conversationForkFixture("edit_and_branch");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.editAndBranchConversationTurn") {
        expect(request).toMatchObject({
          sourceTurnId: fixture.parentTurn.turnId,
          expectedRevision: fixture.parentTurn.revision,
          content: fixture.editedContent,
        });
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    const loadedParent = await client.getConversation(fixture.parentThread.id);
    if (loadedParent.status !== "success") throw new Error("expected the parent conversation");
    const parentBefore = structuredClone(loadedParent.value);

    const result = await client.editConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.userMessage.id,
      fixture.editedContent,
    );

    expect(result).toMatchObject({
      status: "success",
      value: {
        id: fixture.childThread.id,
        branchName: "Edit 1",
        messages: [
          { role: "user", content: fixture.editedContent },
          {
            id: `conversation-stream-${fixture.startedTurn!.turnId}`,
            role: "assistant",
            content: "",
            state: "streaming",
          },
        ],
        turns: [{
          id: fixture.startedTurn!.turnId,
          state: "provider_started",
          modelId: fixture.parentTurn.modelId,
          lineage: {
            origin: "edit_and_branch",
            sourceTurnId: fixture.parentTurn.turnId,
            retryDepth: 0,
          },
        }],
      },
    });
    expect((client as unknown as { conversations: Map<string, unknown> }).conversations.get(
      fixture.parentThread.id,
    )).toEqual(parentBefore);
    unsubscribe();
  });

  it.each(["failed", "cancelled"] as const)(
    "edits an exact %s source context into a child-owned provider turn",
    async (state) => {
      const fixture = conversationForkFixture("edit_and_branch");
      fixture.parentTurn.state = state;
      fixture.parentTurn.assistantMessage = undefined;
      fixture.parentTurn.run.state = state;
      fixture.parentTurn.citations = [];
      fixture.parentTurn.usage = { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 };
      fixture.parentTurn.zeroDataRetention = undefined;
      fixture.parentTurn.failure = state === "failed"
        ? { kind: "unavailable", message: "Provider unavailable.", retryable: true }
        : undefined;
      fixture.parentTurn.retryEligibility = "allowed";
      fixture.parentResponse.messages = [fixture.parentTurn.userMessage];
      fixture.parentResponse.turns = [fixture.parentTurn];
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return bootstrapResponse();
        if (request.kind === "daemon.getConversation") {
          return request.threadId === fixture.parentThread.id
            ? fixture.parentResponse
            : fixture.childResponse;
        }
        if (request.kind === "daemon.editAndBranchConversationTurn") {
          return { kind: "daemon.conversationFork", fork: fixture.fork };
        }
        throw new Error(`unexpected request ${request.kind}`);
      });
      const client = new ElectronDesktopClient(bridge);
      const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
      await client.getConversation(fixture.parentThread.id);

      await expect(client.editConversationMessage(
        fixture.parentThread.id,
        fixture.parentTurn.userMessage.id,
        fixture.editedContent,
      )).resolves.toMatchObject({ status: "success", value: { id: fixture.childThread.id } });
      unsubscribe();
    },
  );

  it("regenerates a completed response with exact frozen context and the recorded model", async () => {
    const fixture = conversationForkFixture("regenerate");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.regenerateConversationTurn") {
        expect(request).toMatchObject({
          sourceTurnId: fixture.parentTurn.turnId,
          expectedRevision: fixture.parentTurn.revision,
        });
        expect(request).not.toHaveProperty("content");
        expect(request).not.toHaveProperty("modelId");
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.regenerateConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({
      status: "success",
      value: {
        id: fixture.childThread.id,
        branchName: "Regenerate 1",
        messages: [
          { role: "user", content: fixture.parentTurn.userMessage.content },
          {
            id: `conversation-stream-${fixture.startedTurn!.turnId}`,
            role: "assistant",
            content: "",
            state: "streaming",
          },
        ],
        turns: [{
          id: fixture.startedTurn!.turnId,
          modelId: fixture.parentTurn.modelId,
          lineage: { origin: "regenerate", sourceTurnId: fixture.parentTurn.turnId },
        }],
      },
    });
    unsubscribe();
  });

  it("accepts an exact Branch replay after ordinary child turns were appended", async () => {
    const fixture = conversationForkFixture("branch");
    const suffixUser: DaemonMessage = {
      ...fixture.parentTurn.userMessage,
      id: "message-child-suffix-user",
      threadId: fixture.childThread.id,
      sequence: 3,
      content: "Continue with owners.",
    };
    const suffixAssistant: DaemonMessage = {
      ...fixture.parentTurn.assistantMessage!,
      id: "message-child-suffix-assistant",
      threadId: fixture.childThread.id,
      sequence: 4,
      content: "Owners are assigned.",
    };
    const suffixTurn: DaemonConversationTurn = {
      ...fixture.parentTurn,
      turnId: "turn-child-suffix",
      userMessage: suffixUser,
      assistantMessage: suffixAssistant,
      run: {
        ...fixture.parentTurn.run,
        id: "run-child-suffix",
        threadId: fixture.childThread.id,
      },
      citations: [],
    };
    fixture.childResponse.messages.push(suffixUser, suffixAssistant);
    fixture.childResponse.turns.push(suffixTurn);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({
      status: "success",
      value: { turns: [{ id: suffixTurn.turnId }] },
    });
    unsubscribe();
  });

  it("rejects an edited child that exposes derived context as another actionable turn", async () => {
    const fixture = conversationForkFixture("edit_and_branch");
    const duplicate = structuredClone(fixture.startedTurn!);
    duplicate.turnId = "turn-forged-derived-context";
    duplicate.run.id = "run-forged-derived-context";
    duplicate.lineage = { origin: "original", retryDepth: 0 };
    fixture.childResponse.turns.push(duplicate);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.editAndBranchConversationTurn") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.editConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.userMessage.id,
      fixture.editedContent,
    )).rejects.toThrow("exposes inherited context as an actionable turn");
    unsubscribe();
  });

  it("retains the exact fork idempotency key after an ambiguous transport failure", async () => {
    const fixture = conversationForkFixture("branch");
    const mutationKeys: string[] = [];
    let attempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        mutationKeys.push(request.idempotencyKey);
        attempts += 1;
        if (attempts === 1) throw new Error("fork response lost");
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);
    const branch = () => client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    );

    await expect(branch()).rejects.toThrow("fork response lost");
    expect((client as unknown as { conversationForkMutations: Map<string, unknown> })
      .conversationForkMutations.size).toBe(1);
    await expect(branch()).resolves.toMatchObject({
      status: "success",
      value: { id: fixture.childThread.id },
    });

    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
    expect((client as unknown as { conversationForkMutations: Map<string, unknown> })
      .conversationForkMutations.size).toBe(0);
    unsubscribe();
  });

  it("acknowledges a fork only after canonical validation and installation", async () => {
    const fixture = conversationForkFixture("branch");
    const requests: BridgeRequest["kind"][] = [];
    let installedBeforeAcknowledgement = false;
    let client!: ElectronDesktopClient;
    const bridge = fakeBridge(async (request) => {
      requests.push(request.kind);
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      if (request.kind === "daemon.acknowledgeConversationForkDelivery") {
        installedBeforeAcknowledgement = (client as unknown as {
          conversations: Map<string, ConversationDetail>;
        }).conversations.has(fixture.childThread.id);
        return {
          kind: "daemon.conversationForkDelivery",
          delivery: {
            childThreadId: request.childThreadId,
            state: "acknowledged",
            revision: 1,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    }, { autoAcknowledgeForkDelivery: false });
    client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).resolves.toMatchObject({ status: "success", value: { id: fixture.childThread.id } });

    const branchIndex = requests.indexOf("daemon.branchConversationThread");
    const childLoadIndex = requests.lastIndexOf("daemon.getConversation");
    const acknowledgementIndex = requests.indexOf("daemon.acknowledgeConversationForkDelivery");
    expect(branchIndex).toBeGreaterThanOrEqual(0);
    expect(childLoadIndex).toBeGreaterThan(branchIndex);
    expect(acknowledgementIndex).toBeGreaterThan(childLoadIndex);
    expect(installedBeforeAcknowledgement).toBe(true);
    expect((client as unknown as {
      conversations: Map<string, ConversationDetail>;
    }).conversations.has(fixture.childThread.id)).toBe(false);
    unsubscribe();
  });

  it("resolves a lost acknowledgement from an acknowledged exact fork replay", async () => {
    const fixture = conversationForkFixture("regenerate");
    const forkKeys: string[] = [];
    let forkAttempts = 0;
    let acknowledgementAttempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.regenerateConversationTurn") {
        forkKeys.push(request.idempotencyKey);
        forkAttempts += 1;
        const fork = structuredClone(fixture.fork);
        if (forkAttempts > 1) {
          fork.delivery = {
            childThreadId: fixture.childThread.id,
            state: "acknowledged",
            revision: 1,
          };
        }
        return { kind: "daemon.conversationFork", fork };
      }
      if (request.kind === "daemon.acknowledgeConversationForkDelivery") {
        acknowledgementAttempts += 1;
        throw new Error("acknowledgement response lost after commit");
      }
      throw new Error(`unexpected request ${request.kind}`);
    }, { autoAcknowledgeForkDelivery: false });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);
    const regenerate = () => client.regenerateConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    );

    await expect(regenerate()).rejects.toThrow("acknowledgement response lost after commit");
    await expect(regenerate()).resolves.toMatchObject({
      status: "success",
      value: { id: fixture.childThread.id },
    });

    expect(forkKeys).toHaveLength(2);
    expect(forkKeys[1]).toBe(forkKeys[0]);
    expect(acknowledgementAttempts).toBe(1);
    expect((client as unknown as {
      conversationForkMutations: Map<string, unknown>;
      conversationForkDeliveryMutations: Map<string, unknown>;
    }).conversationForkMutations.size).toBe(0);
    expect((client as unknown as {
      conversationForkDeliveryMutations: Map<string, unknown>;
    }).conversationForkDeliveryMutations.size).toBe(0);
    unsubscribe();
  });

  it("reuses the exact acknowledgement key while delivery remains pending", async () => {
    const fixture = conversationForkFixture("branch");
    const forkKeys: string[] = [];
    const acknowledgementKeys: string[] = [];
    let acknowledgementAttempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        return request.threadId === fixture.parentThread.id
          ? fixture.parentResponse
          : fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        forkKeys.push(request.idempotencyKey);
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      if (request.kind === "daemon.acknowledgeConversationForkDelivery") {
        acknowledgementKeys.push(request.idempotencyKey);
        acknowledgementAttempts += 1;
        if (acknowledgementAttempts === 1) throw new Error("acknowledgement did not commit");
        return {
          kind: "daemon.conversationForkDelivery",
          delivery: {
            childThreadId: request.childThreadId,
            state: "acknowledged",
            revision: 1,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    }, { autoAcknowledgeForkDelivery: false });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);
    const branch = () => client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    );

    await expect(branch()).rejects.toThrow("acknowledgement did not commit");
    await expect(branch()).resolves.toMatchObject({
      status: "success",
      value: { id: fixture.childThread.id },
    });
    expect(forkKeys).toHaveLength(2);
    expect(forkKeys[1]).toBe(forkKeys[0]);
    expect(acknowledgementKeys).toHaveLength(2);
    expect(acknowledgementKeys[1]).toBe(acknowledgementKeys[0]);
    unsubscribe();
  });

  it("does not acknowledge a malformed fork delivery or an unvalidated child", async () => {
    const fixture = conversationForkFixture("branch");
    fixture.fork.delivery.childThreadId = "thread-forged-child";
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") return fixture.parentResponse;
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    }, { autoAcknowledgeForkDelivery: false });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow("invalid delivery state");
    expect(vi.mocked(bridge.request).mock.calls.some(([request]) => (
      request.kind === "daemon.acknowledgeConversationForkDelivery"
    ))).toBe(false);
    unsubscribe();
  });

  it("does not acknowledge when the canonical child reload fails", async () => {
    const fixture = conversationForkFixture("branch");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        throw new Error("canonical child reload unavailable");
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    }, { autoAcknowledgeForkDelivery: false });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow("canonical child reload unavailable");
    expect(vi.mocked(bridge.request).mock.calls.some(([request]) => (
      request.kind === "daemon.acknowledgeConversationForkDelivery"
    ))).toBe(false);
    unsubscribe();
  });

  it("bounds distinct ambiguous edited-branch intents without evicting retry keys", async () => {
    const fixture = conversationForkFixture("edit_and_branch");
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") return fixture.parentResponse;
      if (request.kind === "daemon.editAndBranchConversationTurn") throw new Error("response lost");
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);
    for (let index = 0; index < 64; index += 1) {
      await expect(client.editConversationMessage(
        fixture.parentThread.id,
        fixture.parentTurn.userMessage.id,
        `${fixture.editedContent} ${index}`,
      )).rejects.toThrow("response lost");
    }
    await expect(client.editConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.userMessage.id,
      `${fixture.editedContent} overflow`,
    )).rejects.toThrow("Too many conversation branch requests");
    expect((client as unknown as { conversationForkMutations: Map<string, unknown> })
      .conversationForkMutations.size).toBe(64);
    expect(vi.mocked(bridge.request).mock.calls.filter(([request]) => (
      request.kind === "daemon.editAndBranchConversationTurn"
    ))).toHaveLength(64);
    unsubscribe();
  });

  it("rejects a fork response whose child lineage does not identify the exact source", async () => {
    const fixture = conversationForkFixture("branch");
    const malformedFork = structuredClone(fixture.fork);
    if (malformedFork.childThread.lineage.origin !== "fork") throw new Error("expected fork lineage");
    malformedFork.childThread.lineage.sourceMessageId = fixture.parentTurn.userMessage.id;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") return fixture.parentResponse;
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: malformedFork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow("invalid thread lineage");
    unsubscribe();
  });

  it.each([
    {
      name: "changes frozen context",
      mutate(fixture: ConversationForkFixture) {
        fixture.childResponse.messages[0].content = "Mutated context";
      },
      error: "does not preserve the source response",
    },
    {
      name: "changes canonical lineage",
      mutate(fixture: ConversationForkFixture) {
        if (fixture.childResponse.thread.lineage.origin !== "fork") throw new Error("expected fork lineage");
        fixture.childResponse.thread.lineage.parentThreadId = "thread-unrelated";
        fixture.childResponse.forkMetadata.lineage = fixture.childResponse.thread.lineage;
      },
      error: "does not match the fork result",
    },
    {
      name: "reuses a parent-owned message identity",
      mutate(fixture: ConversationForkFixture) {
        fixture.childResponse.messages[1].id = fixture.parentTurn.assistantMessage!.id;
        fixture.childResponse.forkMetadata.inheritedAssistantOutcomes[0].childAssistantMessageId =
          fixture.parentTurn.assistantMessage!.id;
      },
      error: "reused parent-owned message identity",
    },
    {
      name: "changes inherited citations",
      mutate(fixture: ConversationForkFixture) {
        fixture.childResponse.forkMetadata.inheritedAssistantOutcomes[0].citations[0].title =
          "Unrelated source";
      },
      error: "changed inherited response citations",
    },
    {
      name: "changes immutable project ownership",
      mutate(fixture: ConversationForkFixture) {
        fixture.childResponse.thread.projectId = "unrelated-project";
        fixture.childResponse.forkMetadata.familyThreads[1].projectId = "unrelated-project";
      },
      error: "changed immutable fork ownership",
    },
  ])("rejects a canonical Branch that $name", async ({ mutate, error }) => {
    const fixture = conversationForkFixture("branch");
    mutate(fixture);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.branchConversationThread") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.branchConversation(
      fixture.parentThread.id,
      fixture.parentTurn.assistantMessage!.id,
    )).rejects.toThrow(error);
    unsubscribe();
  });

  it.each([
    {
      name: "changes the recorded model",
      mutate(turn: DaemonConversationTurn) {
        turn.modelId = "grok-unrelated";
      },
    },
    {
      name: "changes the child turn lineage",
      mutate(turn: DaemonConversationTurn) {
        turn.lineage = {
          origin: "regenerate",
          sourceTurnId: "turn-unrelated",
          retryDepth: 0,
        };
      },
    },
  ])("rejects a canonical edited child that $name", async ({ mutate }) => {
    const fixture = conversationForkFixture("edit_and_branch");
    mutate(fixture.childResponse.turns[0]);
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        if (request.threadId === fixture.parentThread.id) return fixture.parentResponse;
        if (request.threadId === fixture.childThread.id) return fixture.childResponse;
      }
      if (request.kind === "daemon.editAndBranchConversationTurn") {
        return { kind: "daemon.conversationFork", fork: fixture.fork };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const unsubscribe = client.subscribeConversation(fixture.parentThread.id, () => undefined);
    await client.getConversation(fixture.parentThread.id);

    await expect(client.editConversationMessage(
      fixture.parentThread.id,
      fixture.parentTurn.userMessage.id,
      fixture.editedContent,
    )).rejects.toThrow("canonical conversation omits the forked provider turn");
    unsubscribe();
  });

  it.each(["provider_started", "interrupted_needs_review"] as const)(
    "keeps Edit, Regenerate, and Branch unavailable for a %s source",
    async (state) => {
      const thread = conversationThread();
      const original = completedTurn(thread);
      const source: DaemonConversationTurn = {
        ...original,
        state,
        assistantMessage: undefined,
        run: {
          ...original.run,
          state: state === "provider_started" ? "running" : "interrupted_needs_review",
        },
        citations: [],
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        zeroDataRetention: undefined,
        retryEligibility: state === "provider_started"
          ? "source_in_progress"
          : "source_interrupted_needs_review",
      };
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return bootstrapResponse();
        if (request.kind === "daemon.getConversation") return conversationResponse(thread, source);
        throw new Error(`unexpected request ${request.kind}`);
      });
      const client = new ElectronDesktopClient(bridge);
      const unsubscribe = client.subscribeConversation(thread.id, () => undefined);
      await client.getConversation(thread.id);

      await expect(client.editConversationMessage(
        thread.id,
        source.userMessage.id,
        "Edited while unsafe",
      )).resolves.toMatchObject({ status: "unavailable" });
      await expect(client.regenerateConversationMessage(
        thread.id,
        original.assistantMessage!.id,
      )).resolves.toMatchObject({ status: "unavailable" });
      await expect(client.branchConversation(
        thread.id,
        original.assistantMessage!.id,
      )).resolves.toMatchObject({ status: "unavailable" });
      expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual([
        "daemon.bootstrap",
        "daemon.getConversation",
      ]);
      unsubscribe();
    },
  );

  it("uses exact turn revision for cancellation and accepts the interrupted race winner", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const interrupted: DaemonConversationTurn = {
      ...started,
      state: "interrupted_needs_review",
      revision: 2,
      run: { ...started.run, state: "interrupted_needs_review", revision: 3 },
      retryEligibility: "source_interrupted_needs_review",
    };
    let loads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return conversationResponse(thread, loads === 1 ? started : interrupted);
      }
      if (request.kind === "daemon.cancelConversationTurn") {
        expect(request).toMatchObject({ turnId: started.turnId, expectedRevision: 1 });
        expect(request.idempotencyKey).toBeTruthy();
        return { kind: "daemon.conversationTurn", turn: interrupted };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    await client.getConversation(thread.id);

    await expect(client.cancelConversationTurn({
      turnId: started.turnId,
      expectedRevision: 1,
    })).resolves.toMatchObject({
      status: "success",
      value: { state: "interrupted_needs_review", revision: 2 },
    });
  });

  it("returns a completed terminal winner when completion beats cancellation", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const completed = completedTurn(thread);
    let loads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return conversationResponse(thread, loads === 1 ? started : completed);
      }
      if (request.kind === "daemon.cancelConversationTurn") {
        return { kind: "daemon.conversationTurn", turn: completed };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    await client.getConversation(thread.id);

    await expect(client.cancelConversationTurn({
      turnId: started.turnId,
      expectedRevision: 1,
    })).resolves.toMatchObject({ status: "success", value: { state: "completed", revision: 2 } });
  });

  it("does not mask a stale cancellation conflict as an exact terminal winner", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const completed = completedTurn(thread);
    let loads = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        loads += 1;
        return conversationResponse(thread, loads === 1 ? started : completed);
      }
      if (request.kind === "daemon.cancelConversationTurn") {
        throw new Error("stale cancellation revision");
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    await client.getConversation(thread.id);

    await expect(client.cancelConversationTurn({
      turnId: started.turnId,
      expectedRevision: 0,
    })).rejects.toThrow("stale cancellation revision");
  });

  it("retains the exact Start key when both its response and refresh are ambiguous", async () => {
    const thread = conversationThread();
    const started = activeTurn(thread);
    const startKeys: string[] = [];
    let conversationLoads = 0;
    let startAttempts = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getConversation") {
        conversationLoads += 1;
        if (conversationLoads === 2) throw new Error("refresh unavailable");
        return {
          kind: "daemon.conversation",
          thread,
          messages: [],
          turns: [],
          forkMetadata: conversationForkMetadata(thread),
        };
      }
      if (request.kind === "daemon.startConversationTurn") {
        startKeys.push(request.idempotencyKey);
        startAttempts += 1;
        if (startAttempts === 1) throw new Error("start response lost");
        return { kind: "daemon.conversationTurn", turn: started };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    await client.getConversation(thread.id);

    await expect(client.sendConversationMessage(thread.id, "Retry exactly", []))
      .rejects.toThrow("start response lost");
    await expect(client.sendConversationMessage(thread.id, "Retry exactly", []))
      .resolves.toMatchObject({ status: "success", value: { turnId: started.turnId } });
    expect(startKeys).toHaveLength(2);
    expect(startKeys[1]).toBe(startKeys[0]);
  });

  it("rejects an unknown event owner before the renderer ACK deadline", async () => {
    vi.useFakeTimers();
    try {
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return bootstrapResponse();
        throw new Error(`unexpected request ${request.kind}`);
      });
      expect(new ElectronDesktopClient(bridge)).toBeInstanceOf(ElectronDesktopClient);
      const delivery = deliverConversationEvents(bridge, conversationNotification("turn-orphan", [
        { sequence: 1, turnId: "turn-orphan", kind: "created" },
      ], 1));
      const rejection = expect(delivery).rejects.toThrow("ownership was not established");

      await vi.advanceTimersByTimeAsync(2_999);
      await Promise.resolve();
      await vi.advanceTimersByTimeAsync(1);
      await rejection;
    } finally {
      vi.useRealTimers();
    }
  });

  it("fails closed with visible degraded capabilities when bootstrap fails", async () => {
    const bridge = fakeBridge(async () => {
      throw new Error("The local daemon could not be started.");
    });
    const client = new ElectronDesktopClient(bridge);

    const snapshot = await client.getSnapshot();

    expect(snapshot.connection).toMatchObject({ state: "degraded", plan: "Limited mode" });
    expect(snapshot.capabilities.find((item) => item.id === "work")).toMatchObject({
      available: false,
      reasonCode: "daemon_unavailable",
    });
  });

  it("persists project creation through the daemon bridge", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createProject") {
        return {
          kind: "daemon.project",
          project: {
            id: "project-launch",
            name: request.name,
            description: request.description,
            state: "active",
            revision: 0,
            createdAtUnixMs: Date.now(),
            updatedAtUnixMs: Date.now(),
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const result = await client.createProject({ name: "Launch", description: "Release planning" });

    expect(result).toMatchObject({ status: "success", value: { id: "project-launch", name: "Launch" } });
    expect((await client.getSnapshot()).projects.map((project) => project.id)).toContain("project-launch");
  });

  it("reports artifact metadata revisions without inventing content versions or renderer paths", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.workspace.artifacts = [{
      id: "artifact-release-plan",
      projectId: "inbox",
      name: "Release plan.md",
      mediaType: "text/markdown",
      byteSize: 1_024,
      state: "available",
      revision: 7,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 2,
    }];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      throw new Error(`unexpected request ${request.kind}`);
    });

    const snapshot = await new ElectronDesktopClient(bridge).getSnapshot();

    expect(snapshot.library).toEqual([expect.objectContaining({
      id: "artifact-release-plan",
      metadataRevision: 7,
    })]);
    expect(snapshot.library[0]).not.toHaveProperty("contentVersion");
    expect(snapshot.library[0]).not.toHaveProperty("relativePath");
  });

  it("imports through a pathless renderer request and updates the canonical Library snapshot", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.capabilities.push({
      id: "files",
      label: "Local artifact content",
      source: "desktop",
      authentication: "none",
      availability: "available",
      reasonCode: "ready",
      reason: "Available.",
    });
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.importArtifact") {
        expect(request).toEqual({
          kind: "daemon.importArtifact",
          projectId: "inbox",
          idempotencyKey: expect.any(String),
        });
        expect(request).not.toHaveProperty("sourcePath");
        return {
          kind: "daemon.artifactImported",
          artifact: {
            id: "artifact-imported",
            projectId: "inbox",
            name: "report.pdf",
            mediaType: "application/pdf",
            byteSize: 2_048,
            contentVersion: 1,
            state: "available",
            revision: 1,
            createdAtUnixMs: 10,
            updatedAtUnixMs: 11,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.importArtifact("inbox")).resolves.toMatchObject({
      status: "success",
      value: { id: "artifact-imported", contentVersion: 1, size: "2.0 KB" },
    });
    expect((await client.getSnapshot()).library).toEqual([
      expect.objectContaining({ id: "artifact-imported", contentVersion: 1 }),
    ]);
  });

  it("preserves native import cancellation without changing the snapshot", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.capabilities.push({
      id: "files",
      label: "Local artifact content",
      source: "desktop",
      authentication: "none",
      availability: "available",
      reasonCode: "ready",
      reason: "Available.",
    });
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.importArtifact") {
        return { kind: "daemon.artifactImportCancelled" };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.importArtifact("inbox")).resolves.toEqual({
      status: "cancelled",
      reason: "Import cancelled.",
    });
    expect((await client.getSnapshot()).library).toEqual([]);
  });

  it("opens only the current immutable artifact version and preserves closed receipt statuses", async () => {
    const receipts: DaemonArtifactOpenReceipt[] = [
      { artifactId: "artifact-1", contentVersion: 7, status: "opened" },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: "failed",
        failureCode: "content_unavailable",
      },
      { artifactId: "artifact-1", contentVersion: 7, status: "interrupted_needs_review" },
    ];
    for (const receipt of receipts) {
      const response = bootstrapResponse();
      if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
      response.capabilities.push({
        id: "files",
        label: "Local artifact content",
        source: "desktop",
        authentication: "none",
        availability: "available",
        reasonCode: "ready",
        reason: "Available.",
      });
      response.workspace.artifacts = [{
        id: "artifact-1",
        projectId: "inbox",
        name: "report.pdf",
        mediaType: "application/pdf",
        byteSize: 42,
        contentVersion: 7,
        state: "available",
        revision: 1,
        createdAtUnixMs: 1,
        updatedAtUnixMs: 2,
      }];
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return response;
        if (request.kind === "daemon.openArtifact") {
          expect(request).toEqual({
            kind: "daemon.openArtifact",
            artifactId: "artifact-1",
            contentVersion: 7,
            idempotencyKey: expect.any(String),
          });
          return {
            kind: "daemon.artifactOpened",
            receipt,
          };
        }
        throw new Error(`unexpected request ${request.kind}`);
      });
      const client = new ElectronDesktopClient(bridge);

      await expect(client.openArtifact("artifact-1", 7)).resolves.toEqual({
        status: "success",
        value: receipt,
      });
      await expect(client.openArtifact("artifact-1", 6)).resolves.toEqual({
        status: "unavailable",
        reason: "The selected artifact version is no longer available.",
      });
    }
  });

  it("rejects malformed, contradictory, and expanded artifact open receipts", async () => {
    const invalidReceipts = [
      { artifactId: "artifact-1", contentVersion: 7, status: "failed" },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: "opened",
        failureCode: "content_unavailable",
      },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: "failed",
        failureCode: "unknown_failure",
      },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: "failed",
        failureCode: "integrity_failure",
        storagePath: "/private/forged-object",
      },
    ];
    for (const receipt of invalidReceipts) {
      const response = bootstrapResponse();
      if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
      response.capabilities.push({
        id: "files",
        label: "Local artifact content",
        source: "desktop",
        authentication: "none",
        availability: "available",
        reasonCode: "ready",
        reason: "Available.",
      });
      response.workspace.artifacts = [{
        id: "artifact-1",
        projectId: "inbox",
        name: "report.pdf",
        mediaType: "application/pdf",
        byteSize: 42,
        contentVersion: 7,
        state: "available",
        revision: 1,
        createdAtUnixMs: 1,
        updatedAtUnixMs: 2,
      }];
      const client = new ElectronDesktopClient(fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return response;
        if (request.kind === "daemon.openArtifact") {
          return { kind: "daemon.artifactOpened", receipt } as unknown as BridgeResponse;
        }
        throw new Error(`unexpected request ${request.kind}`);
      }));

      await expect(client.openArtifact("artifact-1", 7))
        .rejects.toThrow("invalid artifact open bridge response");
    }
  });

  it("removes only the exact canonical artifact and reuses an ambiguous command key", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.capabilities.push({
      id: "files",
      label: "Local artifact content",
      source: "desktop",
      authentication: "none",
      availability: "available",
      reasonCode: "ready",
      reason: "Available.",
    });
    response.workspace.artifacts = [{
      id: "artifact-1",
      projectId: "inbox",
      name: "report.pdf",
      mediaType: "application/pdf",
      byteSize: 42,
      contentVersion: 7,
      state: "available",
      revision: 7,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 2,
    }];
    const mutationKeys: string[] = [];
    let failTransport = true;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        expect(request).toMatchObject({
          artifactId: "artifact-1",
          expectedRevision: 7,
          expectedContentVersion: 7,
        });
        expect(request).not.toHaveProperty("storagePath");
        expect(request).not.toHaveProperty("digest");
        mutationKeys.push(request.idempotencyKey);
        if (failTransport) {
          failTransport = false;
          throw new Error("daemon transport timed out");
        }
        return {
          kind: "daemon.artifactRemoved",
          artifact: {
            id: "artifact-1",
            projectId: "inbox",
            name: "report.pdf",
            state: "deleted",
            revision: 8,
            createdAtUnixMs: 1,
            updatedAtUnixMs: 3,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 7, 7)).rejects.toThrow("transport timed out");
    expect((await client.getSnapshot()).library).toHaveLength(1);
    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
    expect((await client.getSnapshot()).library).toEqual([]);
  });

  it.each(["internal", "integrity"])(
    "retains the exact removal key after an ambiguous %s daemon failure",
    async (failureKind) => {
      const response = artifactBootstrap([availableArtifact("artifact-1", 7)]);
      const mutationKeys: string[] = [];
      let attempt = 0;
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return response;
        if (request.kind === "daemon.removeArtifact") {
          mutationKeys.push(request.idempotencyKey);
          attempt += 1;
          if (attempt === 1) throw new Error(`${failureKind} daemon response remained ambiguous`);
          return {
            kind: "daemon.artifactRemoved",
            artifact: removedArtifact("artifact-1", 7),
          };
        }
        throw new Error(`unexpected request ${request.kind}`);
      });
      const client = new ElectronDesktopClient(bridge);

      await expect(client.removeArtifact("artifact-1", 7, 7)).rejects.toThrow(
        `${failureKind} daemon response remained ambiguous`,
      );
      await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toMatchObject({
        status: "success",
      });
      expect(mutationKeys).toHaveLength(2);
      expect(mutationKeys[1]).toBe(mutationKeys[0]);
    },
  );

  it("clears a definitive daemon rejection so an exact retry receives a new command key", async () => {
    const response = artifactBootstrap([availableArtifact("artifact-1", 7)]);
    const mutationKeys: string[] = [];
    let attempt = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        mutationKeys.push(request.idempotencyKey);
        attempt += 1;
        if (attempt === 1) {
          return { kind: "daemon.artifactRemovalRejected", reason: "conflict" };
        }
        return {
          kind: "daemon.artifactRemoved",
          artifact: removedArtifact("artifact-1", 7),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "unavailable",
      reason: "The selected artifact version is no longer available.",
    });
    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).not.toBe(mutationKeys[0]);
  });

  it("reconciles an ambiguous response from an exact canonical tombstone", async () => {
    const available = artifactBootstrap([availableArtifact("artifact-1", 7)]);
    const removed = artifactBootstrap([removedArtifact("artifact-1", 7)]);
    let bootstrapCount = 0;
    const mutationKeys: string[] = [];
    let removalCount = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        bootstrapCount += 1;
        return bootstrapCount === 1 ? available : removed;
      }
      if (request.kind === "daemon.removeArtifact") {
        mutationKeys.push(request.idempotencyKey);
        removalCount += 1;
        if (removalCount === 1) throw new Error("response lost after removal reservation");
        return {
          kind: "daemon.artifactRemoved",
          artifact: removedArtifact("artifact-1", 7),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "pending",
    });
    expect(mutationKeys).toHaveLength(1);
    expect((await client.getSnapshot()).library).toEqual([]);

    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
  });

  it("releases an ambiguous old tuple when canonical bootstrap advances the artifact", async () => {
    const initial = artifactBootstrap([availableArtifact("artifact-1", 7)]);
    const advancedArtifact = {
      ...availableArtifact("artifact-1", 8),
      updatedAtUnixMs: 4,
    };
    const advanced = artifactBootstrap([advancedArtifact]);
    let bootstrapCount = 0;
    const mutationKeys: string[] = [];
    let removalCount = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        bootstrapCount += 1;
        return bootstrapCount === 1 ? initial : advanced;
      }
      if (request.kind === "daemon.removeArtifact") {
        mutationKeys.push(request.idempotencyKey);
        removalCount += 1;
        if (removalCount === 1) throw new Error("response lost for old tuple");
        return {
          kind: "daemon.artifactRemoved",
          artifact: { ...removedArtifact("artifact-1", 8), updatedAtUnixMs: 5 },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 7, 7)).rejects.toThrow(
      "response lost for old tuple",
    );
    await expect(client.removeArtifact("artifact-1", 8, 8)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).not.toBe(mutationKeys[0]);
  });

  it("accepts a validated daemon-owned pending cleanup tombstone as canonical removal", async () => {
    const response = artifactBootstrap([availableArtifact("artifact-1", 7)]);
    const mutationKeys: string[] = [];
    let removalCount = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        mutationKeys.push(request.idempotencyKey);
        removalCount += 1;
        if (removalCount > 1) {
          return {
            kind: "daemon.artifactRemoved",
            artifact: removedArtifact(request.artifactId, request.expectedRevision),
          };
        }
        return {
          kind: "daemon.artifactRemovalPending",
          artifactId: request.artifactId,
          expectedRevision: request.expectedRevision,
          expectedContentVersion: request.expectedContentVersion,
          tombstone: removedArtifact(request.artifactId, request.expectedRevision),
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "pending",
    });
    expect((await client.getSnapshot()).library).toEqual([]);
    await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys).toHaveLength(2);
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
  });

  it("bounds presentation reconciliation and reuses the pending key until terminal removal", async () => {
    vi.useFakeTimers();
    try {
      const response = artifactBootstrap([availableArtifact("artifact-1", 7)]);
      const mutationKeys: string[] = [];
      const bridge = fakeBridge(async (request) => {
        if (request.kind === "daemon.bootstrap") return response;
        if (request.kind === "daemon.removeArtifact") {
          mutationKeys.push(request.idempotencyKey);
          if (mutationKeys.length === 1) {
            return {
              kind: "daemon.artifactRemovalPending",
              artifactId: request.artifactId,
              expectedRevision: request.expectedRevision,
              expectedContentVersion: request.expectedContentVersion,
              tombstone: removedArtifact(request.artifactId, request.expectedRevision),
            };
          }
          return {
            kind: "daemon.artifactRemoved",
            artifact: removedArtifact(request.artifactId, request.expectedRevision),
          };
        }
        throw new Error(`unexpected request ${request.kind}`);
      });
      const client = new ElectronDesktopClient(bridge);

      await expect(client.removeArtifact("artifact-1", 7, 7)).resolves.toEqual({
        status: "pending",
      });
      await vi.advanceTimersByTimeAsync(250);
      expect(mutationKeys).toHaveLength(2);
      expect(mutationKeys[1]).toBe(mutationKeys[0]);

      await vi.advanceTimersByTimeAsync(10_000);
      expect(mutationKeys).toHaveLength(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds genuinely ambiguous removals without leaking definitive entries or rotating retry keys", async () => {
    const artifacts = Array.from({ length: 65 }, (_, index) =>
      availableArtifact(`artifact-${index + 1}`, 1));
    const response = artifactBootstrap(artifacts);
    const mutationKeys = new Map<string, string[]>();
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        const keys = mutationKeys.get(request.artifactId) ?? [];
        keys.push(request.idempotencyKey);
        mutationKeys.set(request.artifactId, keys);
        if (request.artifactId === "artifact-1" && keys.length === 2) {
          return {
            kind: "daemon.artifactRemoved",
            artifact: removedArtifact("artifact-1", 1),
          };
        }
        throw new Error(`ambiguous ${request.artifactId}`);
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    for (let index = 1; index <= 64; index += 1) {
      await expect(client.removeArtifact(`artifact-${index}`, 1, 1))
        .rejects.toThrow(`ambiguous artifact-${index}`);
    }
    await expect(client.removeArtifact("artifact-65", 1, 1)).resolves.toEqual({
      status: "unavailable",
      reason: "Too many artifact removals have outcomes awaiting reconciliation.",
    });
    expect(mutationKeys.has("artifact-65")).toBe(false);

    await expect(client.removeArtifact("artifact-1", 1, 1)).resolves.toEqual({
      status: "success",
      value: undefined,
    });
    expect(mutationKeys.get("artifact-1")).toHaveLength(2);
    expect(mutationKeys.get("artifact-1")?.[1]).toBe(mutationKeys.get("artifact-1")?.[0]);

    await expect(client.removeArtifact("artifact-65", 1, 1))
      .rejects.toThrow("ambiguous artifact-65");
    expect(mutationKeys.get("artifact-65")).toHaveLength(1);
  });

  it("does not consume the ambiguity cap for terminal daemon rejections", async () => {
    const artifacts = Array.from({ length: 65 }, (_, index) =>
      availableArtifact(`definitive-${index + 1}`, 1));
    const response = artifactBootstrap(artifacts);
    let removalCalls = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        removalCalls += 1;
        return { kind: "daemon.artifactRemovalRejected", reason: "invalid_state" };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    for (let index = 1; index <= 65; index += 1) {
      await expect(client.removeArtifact(`definitive-${index}`, 1, 1)).resolves.toMatchObject({
        status: "unavailable",
      });
    }
    expect(removalCalls).toBe(65);
  });

  it("fails artifact removal closed for stale identity or a malformed tombstone", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.capabilities.push({
      id: "files",
      label: "Local artifact content",
      source: "desktop",
      authentication: "none",
      availability: "available",
      reasonCode: "ready",
      reason: "Available.",
    });
    response.workspace.artifacts = [{
      id: "artifact-1",
      projectId: "inbox",
      name: "report.pdf",
      mediaType: "application/pdf",
      byteSize: 42,
      contentVersion: 7,
      state: "available",
      revision: 7,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 2,
    }];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      if (request.kind === "daemon.removeArtifact") {
        return {
          kind: "daemon.artifactRemoved",
          artifact: {
            id: "artifact-1",
            projectId: "inbox",
            name: "substituted.pdf",
            state: "deleted",
            revision: 8,
            createdAtUnixMs: 1,
            updatedAtUnixMs: 3,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.removeArtifact("artifact-1", 6, 7)).resolves.toEqual({
      status: "unavailable",
      reason: "The selected artifact version is no longer available.",
    });
    await expect(client.removeArtifact("artifact-1", 7, 7))
      .rejects.toThrow("does not match the request");
    expect((await client.getSnapshot()).library).toHaveLength(1);
  });

  it("omits unavailable artifact reservations from the canonical Library", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.workspace.artifacts = [{
      id: "artifact-unavailable",
      projectId: "inbox",
      name: "pending.pdf",
      state: "unavailable",
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
    }];
    const client = new ElectronDesktopClient(fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      throw new Error(`unexpected request ${request.kind}`);
    }));

    expect((await client.getSnapshot()).library).toEqual([]);
  });

  it("searches canonical daemon workspace content with conversation routing", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.searchWorkspace") {
        expect(request).toEqual({
          kind: "daemon.searchWorkspace",
          projectId: undefined,
          query: "release evidence",
          offset: 0,
          limit: 8,
        });
        return {
          kind: "daemon.workspaceSearchResults",
          results: {
            hits: [{
              id: "message-1",
              projectId: "inbox",
              threadId: "thread-1",
              kind: "message",
              title: "Release review",
              snippet: "Evidence and next actions",
              updatedAtUnixMs: 10,
            }],
            hasMore: false,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.searchWorkspace({ query: "release evidence" })).resolves.toEqual({
      hits: [{
        id: "message-1",
        projectId: "inbox",
        threadId: "thread-1",
        kind: "message",
        title: "Release review",
        snippet: "Evidence and next actions",
        updatedAtUnixMs: 10,
      }],
      hasMore: false,
    });
  });

  it("does not dispatch Chat when the daemon reports it unavailable", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse("unavailable");
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const result = await client.sendConversationMessage("thread-1", "Send this to Grok", []);

    expect(result).toEqual({ status: "configuration_required", reason: "Configure a validated xAI API key." });
    expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual(["daemon.bootstrap"]);
  });

  it("saves automation definitions by project id and forces them disabled", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.createAutomation") {
        expect(request).toMatchObject({
          projectId: "inbox",
          schedule: "v1;weekdays;09:00",
          timezone: "UTC",
        });
        return {
          kind: "daemon.automation",
          automation: {
            id: "automation-1",
            projectId: request.projectId,
            title: request.title,
            prompt: request.prompt,
            schedule: request.schedule,
            timezone: request.timezone,
            missedRunPolicy: request.missedRunPolicy,
            overlapPolicy: request.overlapPolicy,
            state: "disabled",
            revision: 0,
            createdAtUnixMs: 1,
            updatedAtUnixMs: 1,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const result = await client.saveAutomation({
      name: "Release definition",
      projectId: "inbox",
      prompt: "Review release status",
      schedule: { frequency: "weekdays", localTime: "09:00", timeZoneIana: "UTC" },
      missedRunPolicy: "run_once",
      overlapPolicy: "queue_one",
    });

    expect(result).toMatchObject({
      status: "success",
      value: {
        projectId: "inbox",
        projectName: "Inbox",
        schedule: "Weekdays at 09:00",
        enabled: false,
        nextRun: "Not scheduled",
      },
    });
  });

  it("reads the exact daemon-canonical schedule without inventing execution state", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.workspace.automations = [{
      id: "automation-canonical",
      projectId: "inbox",
      title: "Canonical definition",
      prompt: "Review the release status.",
      schedule: "v1;weekly;5;16:00",
      timezone: "Europe/Paris",
      missedRunPolicy: "skip",
      overlapPolicy: "skip",
      state: "disabled",
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
    }];
    const client = new ElectronDesktopClient(fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      throw new Error(`unexpected request ${request.kind}`);
    }));

    const snapshot = await client.getSnapshot();

    expect(snapshot.automations).toEqual([
      expect.objectContaining({
        id: "automation-canonical",
        schedule: "Weekly at 16:00",
        scheduleConfig: {
          frequency: "weekly",
          localTime: "16:00",
          weekday: 5,
          timeZoneIana: "Europe/Paris",
        },
        nextRun: "Not scheduled",
        enabled: false,
      }),
    ]);
  });

  it("does not fabricate a schedule for malformed or legacy daemon definitions", async () => {
    const response = bootstrapResponse();
    if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
    response.workspace.automations = [{
      id: "automation-legacy",
      projectId: "inbox",
      title: "Legacy definition",
      prompt: "Review the release status.",
      schedule: "0 9 * * *",
      timezone: "UTC",
      missedRunPolicy: "run_once",
      overlapPolicy: "queue_one",
      state: "disabled",
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
    }];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return response;
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const snapshot = await client.getSnapshot();

    expect(snapshot.automations).toEqual([
      expect.objectContaining({
        id: "automation-legacy",
        schedule: "Schedule unavailable",
        scheduleConfig: undefined,
        nextRun: "Not scheduled",
        enabled: false,
      }),
    ]);
    expect(JSON.stringify(snapshot.automations)).not.toContain("09:00");
  });

  it("does not treat daemon-owned BYOK status as Grok Build subscription auth", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        return {
          ...bootstrapResponse(),
          accountState: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const result = await client.getAccountSetup();

    expect(result).toMatchObject({ xaiApiKey: "configured", grokBuild: "not_connected", limitedMode: true });
    expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual(["daemon.bootstrap"]);
  });

  it("does not treat any-model credential validation as selected-model readiness", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        return {
          ...bootstrapResponse("unavailable"),
          accountState: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    const setup = await client.getAccountSetup();

    expect(setup.xaiApiKey).toBe("configured");
    expect(setup.checks.find((check) => check.id === "xai_api")).toMatchObject({
      state: "action_required",
    });
  });

  it("enrolls BYOK without passing credential material through the renderer bridge", async () => {
    let configured = false;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        return {
          ...bootstrapResponse(configured ? "available" : "unavailable"),
          accountState: { xaiApiKeyConfigured: configured, xaiCapabilitiesResolved: configured },
        };
      }
      if (request.kind === "daemon.enrollXaiApiKey") {
        expect(Object.keys(request).toSorted()).toEqual(["idempotencyKey", "kind"]);
        expect(request.idempotencyKey).toBeTruthy();
        configured = true;
        return { kind: "daemon.accountState", accountState: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false } };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    expect((await client.getAccountSetup()).xaiApiKey).toBe("not_configured");
    const result = await client.enrollXaiApiKey();

    expect(result).toMatchObject({ status: "success", value: { xaiApiKey: "configured" } });
    expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual([
      "daemon.bootstrap",
      "daemon.enrollXaiApiKey",
      "daemon.bootstrap",
    ]);
  });

  it("returns native cancellation as a typed non-error outcome", async () => {
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse("unavailable");
      if (request.kind === "daemon.enrollXaiApiKey") {
        return { kind: "daemon.credentialEnrollmentFailure", reason: "cancelled" };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.enrollXaiApiKey()).resolves.toEqual({
      status: "cancelled",
      reason: "Credential entry was cancelled.",
    });
  });

  it("reuses the enrollment idempotency key after a transport failure and rotates it after success", async () => {
    const enrollmentKeys: string[] = [];
    let failTransport = true;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse("unavailable");
      if (request.kind === "daemon.enrollXaiApiKey") {
        enrollmentKeys.push(request.idempotencyKey);
        if (failTransport) {
          failTransport = false;
          throw new Error("daemon transport timed out");
        }
        return {
          kind: "daemon.accountState",
          accountState: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.enrollXaiApiKey()).rejects.toThrow("transport timed out");
    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({ status: "success" });
    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({ status: "success" });

    expect(enrollmentKeys[0]).toBeTruthy();
    expect(enrollmentKeys[1]).toBe(enrollmentKeys[0]);
    expect(enrollmentKeys[2]).not.toBe(enrollmentKeys[1]);
  });

  it("rotates the enrollment idempotency key after cancellation and integrity failure", async () => {
    const enrollmentKeys: string[] = [];
    const terminalReasons = ["cancelled", "integrity_failure"] as const;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse("unavailable");
      if (request.kind === "daemon.enrollXaiApiKey") {
        enrollmentKeys.push(request.idempotencyKey);
        const reason = terminalReasons[enrollmentKeys.length - 1];
        if (reason) return { kind: "daemon.credentialEnrollmentFailure", reason };
        return {
          kind: "daemon.accountState",
          accountState: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({ status: "cancelled" });
    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({ status: "unavailable" });
    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({ status: "success" });

    expect(new Set(enrollmentKeys).size).toBe(3);
  });

  it("deletes BYOK through the daemon and refreshes daemon-owned capabilities", async () => {
    let configured = true;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        return {
          ...bootstrapResponse(),
          accountState: { xaiApiKeyConfigured: configured, xaiCapabilitiesResolved: configured },
        };
      }
      if (request.kind === "daemon.deleteXaiApiKey") {
        expect(request.idempotencyKey).toBeTruthy();
        configured = false;
        return { kind: "daemon.accountState", accountState: { xaiApiKeyConfigured: false, xaiCapabilitiesResolved: false, grokBuildAuthenticated: false } };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    expect((await client.getAccountSetup()).xaiApiKey).toBe("configured");
    const result = await client.deleteXaiApiKey();

    expect(result).toMatchObject({ status: "success", value: { xaiApiKey: "not_configured" } });
    expect(vi.mocked(bridge.request).mock.calls.map(([request]) => request.kind)).toEqual([
      "daemon.bootstrap",
      "daemon.deleteXaiApiKey",
      "daemon.bootstrap",
    ]);
  });

  it("loads daemon-owned close behavior and reuses an ambiguous mutation key", async () => {
    const mutationKeys: string[] = [];
    let failTransport = true;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getDesktopPreferences") {
        return {
          kind: "daemon.desktopPreferences",
          preferences: { keepRunningInNotificationArea: true, revision: 0, updatedAtUnixMs: 1 },
        };
      }
      if (request.kind === "daemon.updateDesktopPreferences") {
        mutationKeys.push(request.idempotencyKey);
        if (failTransport) {
          failTransport = false;
          throw new Error("daemon transport timed out");
        }
        return {
          kind: "daemon.desktopPreferences",
          preferences: {
            keepRunningInNotificationArea: request.keepRunningInNotificationArea,
            revision: request.expectedRevision + 1,
            updatedAtUnixMs: 2,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.getDesktopPreferences()).resolves.toMatchObject({
      keepRunningInNotificationArea: true,
      revision: 0,
    });
    const update = { expectedRevision: 0, keepRunningInNotificationArea: false };
    await expect(client.updateDesktopPreferences(update)).rejects.toThrow("transport timed out");
    await expect(client.updateDesktopPreferences(update)).resolves.toMatchObject({
      keepRunningInNotificationArea: false,
      revision: 1,
    });

    expect(mutationKeys[0]).toBeTruthy();
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
  });

  it("loads live model discovery and reuses a selection key after an ambiguous transport failure", async () => {
    const mutationKeys: string[] = [];
    let failTransport = true;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") return bootstrapResponse();
      if (request.kind === "daemon.getChatModelCatalog") {
        expect(Object.keys(request)).toEqual(["kind"]);
        return {
          kind: "daemon.chatModelCatalog",
          catalog: {
            models: [{
              id: "grok-alternative",
              aliases: ["grok-current"],
              inputModalities: ["text"],
              outputModalities: ["text"],
              textConversationReady: true,
            }],
            preference: { selectedModelId: "grok-4.3", revision: 0, updatedAtUnixMs: 0 },
            defaultModelId: "grok-4.3",
            selectedModelReady: false,
            defaultModelReady: false,
          },
        };
      }
      if (request.kind === "daemon.selectChatModel") {
        expect(Object.keys(request).toSorted()).toEqual([
          "expectedRevision", "idempotencyKey", "kind", "modelId",
        ]);
        mutationKeys.push(request.idempotencyKey);
        if (failTransport) {
          failTransport = false;
          throw new Error("daemon transport timed out");
        }
        return {
          kind: "daemon.chatModelPreference",
          preference: {
            selectedModelId: "grok-alternative",
            revision: request.expectedRevision + 1,
            updatedAtUnixMs: 2,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    await expect(client.getChatModelCatalog()).resolves.toMatchObject({
      preference: { selectedModelId: "grok-4.3", revision: 0 },
      selectedModelReady: false,
    });
    const selection = { expectedRevision: 0, modelId: "grok-alternative" };
    await expect(client.selectChatModel(selection)).rejects.toThrow("transport timed out");
    await expect(client.selectChatModel(selection)).resolves.toMatchObject({
      selectedModelId: "grok-alternative",
      revision: 1,
    });

    expect(mutationKeys[0]).toBeTruthy();
    expect(mutationKeys[1]).toBe(mutationKeys[0]);
  });

  it("retains the selection key and fails Chat closed until readiness reconciliation succeeds", async () => {
    let bootstrapCalls = 0;
    const selectionKeys: string[] = [];
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        bootstrapCalls += 1;
        if (bootstrapCalls === 2) throw new Error("readiness refresh timed out");
        return bootstrapResponse();
      }
      if (request.kind === "daemon.selectChatModel") {
        selectionKeys.push(request.idempotencyKey);
        return {
          kind: "daemon.chatModelPreference",
          preference: {
            selectedModelId: "grok-alternative",
            revision: request.expectedRevision + 1,
            updatedAtUnixMs: 2,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);
    const selection = { expectedRevision: 0, modelId: "grok-alternative" };

    await expect(client.selectChatModel(selection)).rejects.toThrow(
      "model selection outcome could not be reconciled",
    );

    const failedClosed = await client.getSnapshot();
    expect(failedClosed.connection).toMatchObject({ state: "online" });
    expect(failedClosed.capabilities.find((capability) => capability.id === "chat")).toMatchObject({
      available: false,
      availability: "unavailable",
    });

    await expect(client.selectChatModel(selection)).resolves.toMatchObject({
      selectedModelId: "grok-alternative",
      revision: 1,
    });
    expect(selectionKeys[0]).toBeTruthy();
    expect(selectionKeys[1]).toBe(selectionKeys[0]);
    expect((await client.getSnapshot()).capabilities.find((capability) => capability.id === "chat"))
      .toMatchObject({ available: true });
  });

  it("fails cached Chat closed on catalog staleness and recovers only through daemon bootstrap", async () => {
    let bootstrapCalls = 0;
    let catalogCalls = 0;
    const bridge = fakeBridge(async (request) => {
      if (request.kind === "daemon.bootstrap") {
        bootstrapCalls += 1;
        return bootstrapResponse();
      }
      if (request.kind === "daemon.getChatModelCatalog") {
        catalogCalls += 1;
        if (catalogCalls === 1) throw new Error("catalog unavailable");
        const ready = catalogCalls === 2;
        return {
          kind: "daemon.chatModelCatalog",
          catalog: {
            models: [{
              id: ready ? "grok-4.3" : "grok-alternative",
              aliases: [],
              inputModalities: ["text"],
              outputModalities: ["text"],
              textConversationReady: true,
            }],
            preference: { selectedModelId: "grok-4.3", revision: 0, updatedAtUnixMs: 0 },
            defaultModelId: "grok-4.3",
            selectedModelReady: ready,
            defaultModelReady: ready,
          },
        };
      }
      throw new Error(`unexpected request ${request.kind}`);
    });
    const client = new ElectronDesktopClient(bridge);

    expect((await client.getSnapshot()).capabilities.find((item) => item.id === "chat")?.available).toBe(true);
    await expect(client.getChatModelCatalog()).rejects.toThrow("catalog unavailable");
    expect((await client.getSnapshot()).capabilities.find((item) => item.id === "chat")).toMatchObject({
      available: false,
      reasonCode: "xai_model_selection_unavailable",
    });

    await expect(client.getChatModelCatalog()).resolves.toMatchObject({ selectedModelReady: true });
    expect(bootstrapCalls).toBe(2);
    expect((await client.getSnapshot()).capabilities.find((item) => item.id === "chat")?.available).toBe(true);

    await expect(client.getChatModelCatalog()).resolves.toMatchObject({ selectedModelReady: false });
    expect(bootstrapCalls).toBe(2);
    expect((await client.getSnapshot()).capabilities.find((item) => item.id === "chat")?.available).toBe(false);
  });
});

type ConversationForkKind = "branch" | "edit_and_branch" | "regenerate";
type ConversationResponse = Extract<BridgeResponse, { kind: "daemon.conversation" }>;

interface ConversationForkFixture {
  kind: ConversationForkKind;
  editedContent: string;
  parentThread: DaemonThread;
  parentTurn: DaemonConversationTurn;
  childThread: DaemonThread;
  startedTurn?: DaemonConversationTurn;
  fork: DaemonConversationFork;
  parentResponse: ConversationResponse;
  childResponse: ConversationResponse;
}

function prependCompletedContext(fixture: ConversationForkFixture): void {
  const priorUser: DaemonMessage = {
    ...fixture.parentTurn.userMessage,
    id: "message-prior-user",
    sequence: 1,
    content: "List the release risks first.",
  };
  const priorAssistant: DaemonMessage = {
    ...fixture.parentTurn.assistantMessage!,
    id: "message-prior-assistant",
    sequence: 2,
    content: "The primary risk is rollout coordination.",
  };
  const priorTurn: DaemonConversationTurn = {
    ...fixture.parentTurn,
    turnId: "turn-prior-completed",
    userMessage: priorUser,
    assistantMessage: priorAssistant,
    run: { ...fixture.parentTurn.run, id: "run-prior-completed" },
  };
  fixture.parentTurn.userMessage.sequence = 3;
  fixture.parentTurn.assistantMessage!.sequence = 4;
  fixture.parentResponse.messages = [
    priorUser,
    priorAssistant,
    fixture.parentTurn.userMessage,
    fixture.parentTurn.assistantMessage!,
  ];
  fixture.parentResponse.turns = [priorTurn, fixture.parentTurn];
  const copiedPriorUser: DaemonMessage = {
    ...priorUser,
    id: "message-prior-user-copy",
    threadId: fixture.childThread.id,
    derivation: {
      origin: "fork",
      sourceMessageId: priorUser.id,
      sourceTurnId: fixture.parentTurn.turnId,
      contextPosition: 1,
      kind: "context_copy",
    },
  };
  const copiedPriorAssistant: DaemonMessage = {
    ...priorAssistant,
    id: "message-prior-assistant-copy",
    threadId: fixture.childThread.id,
    derivation: {
      origin: "fork",
      sourceMessageId: priorAssistant.id,
      sourceTurnId: fixture.parentTurn.turnId,
      contextPosition: 2,
      kind: "context_copy",
    },
  };
  const copiedSourceUser = fixture.childResponse.messages[0];
  copiedSourceUser.sequence = 3;
  if (copiedSourceUser.derivation.origin !== "fork") throw new Error("fork fixture is invalid");
  copiedSourceUser.derivation.contextPosition = 3;
  fixture.childResponse.messages[1].sequence = 4;
  fixture.childResponse.messages.unshift(copiedPriorUser, copiedPriorAssistant);
  fixture.childResponse.forkMetadata.inheritedAssistantOutcomes.unshift({
    childAssistantMessageId: copiedPriorAssistant.id,
    sourceTurnId: priorTurn.turnId,
    modelId: priorTurn.modelId,
    citations: structuredClone(priorTurn.citations),
    usage: structuredClone(priorTurn.usage),
    zeroDataRetention: priorTurn.zeroDataRetention,
  });
}

function conversationForkFixture(kind: ConversationForkKind): ConversationForkFixture {
  const parentThread = conversationThread();
  const parentTurn = completedTurn(parentThread);
  const parentAssistant = parentTurn.assistantMessage;
  if (!parentAssistant) throw new Error("fork fixture requires a completed response");
  const editedContent = "Rewrite the release plan as a launch checklist.";
  const sourceMessage = kind === "edit_and_branch" ? parentTurn.userMessage : parentAssistant;
  const childThread: DaemonThread = {
    id: `thread-${kind}`,
    projectId: parentThread.projectId,
    title: parentThread.title,
    state: "open",
    revision: 0,
    createdAtUnixMs: 20,
    updatedAtUnixMs: 20,
    lineage: {
      origin: "fork",
      rootThreadId: parentThread.id,
      parentThreadId: parentThread.id,
      sourceTurnId: parentTurn.turnId,
      sourceMessageId: sourceMessage.id,
      kind,
      forkDepth: 1,
    },
  };
  const childUser: DaemonMessage = {
    id: `message-user-${kind}`,
    threadId: childThread.id,
    sequence: 1,
    role: "user",
    content: kind === "edit_and_branch" ? editedContent : parentTurn.userMessage.content,
    state: "active",
    revision: 0,
    createdAtUnixMs: 21,
    updatedAtUnixMs: 21,
    derivation: {
      origin: "fork",
      sourceMessageId: parentTurn.userMessage.id,
      sourceTurnId: parentTurn.turnId,
      contextPosition: 1,
      kind: kind === "edit_and_branch" ? "edited_user" : "context_copy",
    },
  };
  const childAssistant: DaemonMessage = {
    id: "message-assistant-branch-copy",
    threadId: childThread.id,
    sequence: 2,
    role: "assistant",
    content: parentAssistant.content,
    state: "active",
    revision: 0,
    createdAtUnixMs: 21,
    updatedAtUnixMs: 21,
    derivation: {
      origin: "fork",
      sourceMessageId: parentAssistant.id,
      sourceTurnId: parentTurn.turnId,
      kind: "source_assistant_copy",
    },
  };
  const startedTurn: DaemonConversationTurn | undefined = kind === "branch"
    ? undefined
    : {
        turnId: `turn-${kind}`,
        state: "provider_started",
        revision: 1,
        modelId: parentTurn.modelId,
        userMessage: childUser,
        run: {
          id: `run-${kind}`,
          projectId: childThread.projectId,
          threadId: childThread.id,
          state: "running",
          revision: 2,
          createdAtUnixMs: 20,
          updatedAtUnixMs: 21,
        },
        citations: [],
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: {
          origin: kind,
          sourceTurnId: parentTurn.turnId,
          retryDepth: 0,
        },
        retryEligibility: "source_in_progress",
      };
  const fork: DaemonConversationFork = {
    childThread,
    ...(startedTurn ? { startedTurn } : {}),
    delivery: {
      childThreadId: childThread.id,
      state: "pending",
      revision: 0,
    },
  };
  const canonicalThread = structuredClone(childThread);
  const canonicalMessages = structuredClone(
    kind === "branch" ? [childUser, childAssistant] : [childUser],
  );
  const canonicalTurns = startedTurn ? [structuredClone(startedTurn)] : [];
  const forkMetadata: DaemonConversationForkMetadata = {
    lineage: canonicalThread.lineage,
    inheritedAssistantOutcomes: kind === "branch"
      ? [{
          childAssistantMessageId: canonicalMessages[1].id,
          sourceTurnId: parentTurn.turnId,
          modelId: parentTurn.modelId,
          citations: structuredClone(parentTurn.citations),
          usage: structuredClone(parentTurn.usage),
          zeroDataRetention: parentTurn.zeroDataRetention,
        }]
      : [],
    familyThreads: [structuredClone(parentThread), structuredClone(canonicalThread)],
  };
  return {
    kind,
    editedContent,
    parentThread,
    parentTurn,
    childThread,
    startedTurn,
    fork,
    parentResponse: conversationResponse(parentThread, parentTurn),
    childResponse: {
      kind: "daemon.conversation",
      thread: canonicalThread,
      messages: canonicalMessages,
      turns: canonicalTurns,
      forkMetadata,
    },
  };
}

function fakeBridge(
  handler: (request: BridgeRequest) => Promise<BridgeResponse>,
  options: { autoAcknowledgeForkDelivery?: boolean } = {},
): DesktopBridge {
  const bridge: DesktopBridge = {
    request: vi.fn((request: BridgeRequest) => {
      if (
        options.autoAcknowledgeForkDelivery !== false
        && request.kind === "daemon.acknowledgeConversationForkDelivery"
      ) {
        return Promise.resolve({
          kind: "daemon.conversationForkDelivery" as const,
          delivery: {
            childThreadId: request.childThreadId,
            state: "acknowledged" as const,
            revision: 1,
          },
        });
      }
      return handler(request);
    }),
    onDaemonStatus: vi.fn(() => () => undefined),
    onConversationTurnEvents: vi.fn((listener) => {
      conversationEventListeners.set(bridge, listener);
      return () => {
        if (conversationEventListeners.get(bridge) === listener) conversationEventListeners.delete(bridge);
      };
    }),
    onNavigationRoute: vi.fn(() => () => undefined),
  };
  return bridge;
}

const conversationEventListeners = new WeakMap<
  DesktopBridge,
  (notification: DesktopConversationTurnEventNotification) => void | Promise<void>
>();

async function deliverConversationEvents(
  bridge: DesktopBridge,
  notification: DesktopConversationTurnEventNotification,
): Promise<void> {
  const listener = conversationEventListeners.get(bridge);
  if (!listener) throw new Error("conversation event listener is not installed");
  await listener(notification);
}

function availableArtifact(id: string, version: number): DaemonArtifact {
  return {
    id,
    projectId: "inbox",
    name: `${id}.pdf`,
    mediaType: "application/pdf",
    byteSize: 42,
    contentVersion: version,
    state: "available",
    revision: version,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 2,
  };
}

function removedArtifact(id: string, previousVersion: number): DaemonArtifact {
  return {
    id,
    projectId: "inbox",
    name: `${id}.pdf`,
    state: "deleted",
    revision: previousVersion + 1,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 3,
  };
}

function artifactBootstrap(
  artifacts: DaemonArtifact[],
): Extract<BridgeResponse, { kind: "daemon.bootstrap" }> {
  const response = bootstrapResponse();
  if (response.kind !== "daemon.bootstrap") throw new Error("expected bootstrap response");
  response.capabilities.push({
    id: "files",
    label: "Local artifact content",
    source: "desktop",
    authentication: "none",
    availability: "available",
    reasonCode: "ready",
    reason: "Available.",
  });
  response.workspace.artifacts = artifacts;
  return response;
}

function bootstrapResponse(chatAvailability: "available" | "unavailable" = "available"): BridgeResponse {
  return {
    kind: "daemon.bootstrap",
    status: connected,
    accountState: {
      xaiApiKeyConfigured: chatAvailability === "available",
      xaiCapabilitiesResolved: chatAvailability === "available",
    },
    capabilities: [
      {
        id: "chat",
        label: "Grok chat",
        source: "xai_api",
        authentication: "xai_api_key",
        availability: chatAvailability,
        reasonCode: chatAvailability === "available" ? "ready" : "xai_api_key_required",
        reason: chatAvailability === "available" ? "Available." : "Configure a validated xAI API key.",
      },
      {
        id: "work",
        label: "Work runtime",
        source: "subscription_acp",
        authentication: "subscription_oauth",
        availability: "available",
        reasonCode: "ready",
        reason: "Available.",
      },
    ],
    workspace: {
      projects: [{
        id: "inbox", name: "Inbox", description: "Unsorted Grok conversations", state: "active",
        revision: 0, createdAtUnixMs: 1, updatedAtUnixMs: 1,
      }],
      threads: [],
      messages: [],
      artifacts: [],
      automations: [],
    },
  };
}

function conversationThread(): DaemonThread {
  return {
    id: "thread-chat-1",
    projectId: "inbox",
    title: "Summarize this release plan.",
    state: "open",
    revision: 0,
    createdAtUnixMs: 10,
    updatedAtUnixMs: 10,
    lineage: { origin: "original", rootThreadId: "thread-chat-1", forkDepth: 0 },
  };
}

function conversationResponse(
  thread: DaemonThread,
  turn: DaemonConversationTurn,
): Extract<BridgeResponse, { kind: "daemon.conversation" }> {
  return {
    kind: "daemon.conversation",
    thread,
    messages: [turn.userMessage, ...(turn.assistantMessage ? [turn.assistantMessage] : [])],
    turns: [turn],
    forkMetadata: conversationForkMetadata(thread),
  };
}

function conversationForkMetadata(
  thread: DaemonThread,
): Extract<BridgeResponse, { kind: "daemon.conversation" }>["forkMetadata"] {
  return {
    lineage: thread.lineage,
    inheritedAssistantOutcomes: [],
    familyThreads: [thread],
  };
}

function conversationNotification(
  turnId: string,
  events: DesktopConversationTurnEventNotification["batch"]["events"],
  nextSequence: number,
): DesktopConversationTurnEventNotification {
  return { turnId, batch: { events, nextSequence, hasMore: false } };
}

function completedTurn(thread: DaemonThread): DaemonConversationTurn {
  const userMessage: DaemonMessage = {
    id: "message-user-1",
    threadId: thread.id,
    sequence: 1,
    role: "user",
    content: "Summarize this release plan.",
    state: "active",
    revision: 0,
    createdAtUnixMs: 11,
    updatedAtUnixMs: 11,
    derivation: { origin: "original" },
  };
  const assistantMessage: DaemonMessage = {
    id: "message-assistant-1",
    threadId: thread.id,
    sequence: 2,
    role: "assistant",
    content: "The release plan is ready.",
    state: "active",
    revision: 0,
    createdAtUnixMs: 12,
    updatedAtUnixMs: 12,
    derivation: { origin: "original" },
  };
  return {
    turnId: "turn-chat-1",
    state: "completed",
    revision: 2,
    modelId: "grok-4.3",
    userMessage,
    assistantMessage,
    run: {
      id: "run-chat-1",
      projectId: "inbox",
      threadId: thread.id,
      state: "completed",
      revision: 3,
      createdAtUnixMs: 10,
      updatedAtUnixMs: 12,
    },
    citations: [{ title: "Release guide", url: "https://docs.x.ai/release" }],
    usage: { inputTokens: 20, outputTokens: 8, costInUsdTicks: 0 },
    zeroDataRetention: true,
    lineage: { origin: "original", retryDepth: 0 },
    retryEligibility: "source_completed",
  };
}

function activeTurn(thread: DaemonThread): DaemonConversationTurn {
  const completed = completedTurn(thread);
  return {
    ...completed,
    state: "provider_started",
    revision: 1,
    assistantMessage: undefined,
    citations: [],
    usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
    zeroDataRetention: undefined,
    retryEligibility: "source_in_progress",
    run: {
      ...completed.run,
      state: "running",
      revision: 2,
      updatedAtUnixMs: 11,
    },
  };
}

function retryableFailedTurn(thread: DaemonThread): DaemonConversationTurn {
  const completed = completedTurn(thread);
  return {
    ...completed,
    turnId: "turn-retry-source",
    state: "failed",
    assistantMessage: undefined,
    failure: { kind: "unavailable", message: "The provider is unavailable.", retryable: true },
    citations: [],
    usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
    zeroDataRetention: undefined,
    run: { ...completed.run, id: "run-retry-source", state: "failed", revision: 3 },
    retryEligibility: "allowed",
  };
}

function retriedActiveTurn(
  thread: DaemonThread,
  source: DaemonConversationTurn,
): DaemonConversationTurn {
  const active = activeTurn(thread);
  return {
    ...active,
    turnId: "turn-retried",
    userMessage: {
      ...source.userMessage,
      id: "message-user-retried",
      sequence: source.userMessage.sequence + 1,
    },
    run: { ...active.run, id: "run-retried" },
    lineage: {
      origin: "retry",
      sourceTurnId: source.turnId,
      retryDepth: source.lineage.retryDepth + 1,
    },
    retryEligibility: "source_in_progress",
  };
}

function retryConversationResponse(
  thread: DaemonThread,
  source: DaemonConversationTurn,
  retry?: DaemonConversationTurn,
): Extract<BridgeResponse, { kind: "daemon.conversation" }> {
  return {
    kind: "daemon.conversation",
    thread,
    messages: [source.userMessage, ...(retry ? [retry.userMessage] : [])],
    turns: [source, ...(retry ? [retry] : [])],
    forkMetadata: conversationForkMetadata(thread),
  };
}
