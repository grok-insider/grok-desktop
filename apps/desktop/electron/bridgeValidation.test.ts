// @vitest-environment node
import { describe, expect, it } from "vitest";
import { parseBridgeRequest } from "./bridgeValidation.js";

describe("parseBridgeRequest", () => {
  it("accepts only fieldless main-process update commands", () => {
    for (const kind of ["desktop.getUpdateState", "desktop.checkForUpdates", "desktop.installUpdate"] as const) {
      expect(parseBridgeRequest({ kind })).toEqual({ kind });
      expect(() => parseBridgeRequest({ kind, url: "https://attacker.example/update" })).toThrow("unsupported fields");
    }
  });

  it("rejects producer-only execution mutations from the renderer", () => {
    for (const request of [
      { kind: "daemon.createRun", projectId: "project-1", threadId: "thread-1", idempotencyKey: "run-1" },
      { kind: "daemon.transitionRun", runId: "run-1", expectedRevision: 2, nextState: "completed", idempotencyKey: "transition-1" },
      {
        kind: "daemon.requestApproval",
        runId: "run-1",
        expectedRunRevision: 2,
        action: { action: "write", target: "report.md", dataSummary: "final report", risk: "low" },
        scope: "once",
        expiresAtUnixMs: Date.now() + 60_000,
        idempotencyKey: "approval-command-1",
      },
    ]) {
      expect(() => parseBridgeRequest(request)).toThrow("unsupported bridge request");
    }
  });

  it("accepts bounded project creation", () => {
    expect(parseBridgeRequest({
      kind: "daemon.createProject",
      name: "Release planning",
      description: "Durable project",
      idempotencyKey: "project-command-1",
    })).toMatchObject({ kind: "daemon.createProject", name: "Release planning" });
  });

  it("accepts only pathless artifact import and exact-version open/removal intents", () => {
    expect(parseBridgeRequest({
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "artifact-import-1",
    })).toEqual({
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "artifact-import-1",
    });
    for (const forgedField of [
      { sourcePath: "/renderer/private.txt" },
      { displayName: "renderer-name.txt" },
      { mediaType: "text/plain" },
      { threadId: "thread-1" },
    ]) {
      expect(() => parseBridgeRequest({
        kind: "daemon.importArtifact",
        projectId: "project-1",
        idempotencyKey: "artifact-import-forged",
        ...forgedField,
      })).toThrow("artifact import request contains unsupported fields");
    }

    expect(parseBridgeRequest({
      kind: "daemon.openArtifact",
      artifactId: "artifact-1",
      contentVersion: 7,
      idempotencyKey: "artifact-open-1",
    })).toEqual({
      kind: "daemon.openArtifact",
      artifactId: "artifact-1",
      contentVersion: 7,
      idempotencyKey: "artifact-open-1",
    });
    for (const contentVersion of [0, 1_000_001, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => parseBridgeRequest({
        kind: "daemon.openArtifact",
        artifactId: "artifact-1",
        contentVersion,
        idempotencyKey: "artifact-open-invalid",
      })).toThrow();
    }
    expect(() => parseBridgeRequest({
      kind: "daemon.openArtifact",
      artifactId: "artifact-1",
      contentVersion: 7,
      storagePath: "/renderer/private.txt",
      idempotencyKey: "artifact-open-forged",
    })).toThrow("artifact open request contains unsupported fields");

    expect(parseBridgeRequest({
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "artifact-remove-1",
    })).toEqual({
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "artifact-remove-1",
    });
    for (const request of [
      {
        kind: "daemon.removeArtifact",
        artifactId: "artifact-1",
        expectedRevision: -1,
        expectedContentVersion: 7,
        idempotencyKey: "artifact-remove-invalid-revision",
      },
      {
        kind: "daemon.removeArtifact",
        artifactId: "artifact-1",
        expectedRevision: 8,
        expectedContentVersion: 7,
        idempotencyKey: "artifact-remove-mismatched-version",
      },
      {
        kind: "daemon.removeArtifact",
        artifactId: "artifact-1",
        expectedRevision: 7,
        expectedContentVersion: 0,
        idempotencyKey: "artifact-remove-invalid-version",
      },
      {
        kind: "daemon.removeArtifact",
        artifactId: "artifact-1",
        expectedRevision: 7,
        expectedContentVersion: 7,
        storagePath: "/renderer/private.txt",
        idempotencyKey: "artifact-remove-forged",
      },
    ]) {
      expect(() => parseBridgeRequest(request)).toThrow();
    }
  });

  it("accepts only bounded read-only workspace searches", () => {
    expect(parseBridgeRequest({
      kind: "daemon.searchWorkspace",
      projectId: "project-1",
      query: "release evidence",
      offset: 0,
      limit: 8,
    })).toEqual({
      kind: "daemon.searchWorkspace",
      projectId: "project-1",
      query: "release evidence",
      offset: 0,
      limit: 8,
    });
    for (const request of [
      { kind: "daemon.searchWorkspace", query: " ", offset: 0, limit: 8 },
      { kind: "daemon.searchWorkspace", query: "release\nunsafe", offset: 0, limit: 8 },
      { kind: "daemon.searchWorkspace", query: "release", offset: 10_001, limit: 8 },
      { kind: "daemon.searchWorkspace", query: "release", offset: 0, limit: 101 },
      { kind: "daemon.searchWorkspace", query: "release", offset: 0, limit: 8, sql: "SELECT *" },
    ]) {
      expect(() => parseBridgeRequest(request)).toThrow();
    }
  });

  it("accepts only an exact revisioned approval decision from the renderer", () => {
    expect(parseBridgeRequest({
      kind: "daemon.decideApproval",
      approvalId: "approval-1",
      expectedRevision: 0,
      approved: false,
      idempotencyKey: "approval-decision-1",
    })).toEqual({
      kind: "daemon.decideApproval",
      approvalId: "approval-1",
      expectedRevision: 0,
      approved: false,
      idempotencyKey: "approval-decision-1",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.decideApproval",
      approvalId: "approval-1",
      expectedRevision: 0,
      approved: true,
      action: { action: "write", target: "anything" },
      idempotencyKey: "approval-decision-2",
    })).toThrow("approval decision request contains unsupported fields");
  });

  it("rejects generic message mutations from the renderer", () => {
    for (const request of [
      {
        kind: "daemon.createMessage",
        threadId: "thread-1",
        role: "user",
        content: "Untracked user input",
        idempotencyKey: "message-create-1",
      },
      {
        kind: "daemon.updateMessage",
        messageId: "message-1",
        expectedRevision: 0,
        content: "Mutated history",
        idempotencyKey: "message-update-1",
      },
      {
        kind: "daemon.deleteMessage",
        messageId: "message-1",
        expectedRevision: 0,
        idempotencyKey: "message-delete-1",
      },
    ]) {
      expect(() => parseBridgeRequest(request)).toThrow("unsupported bridge request");
    }
  });

  it("accepts only exact usage summary scopes and windows", () => {
    expect(parseBridgeRequest({
      kind: "daemon.getUsageSummary",
      scopeKind: "workspace",
      window: "last_7_days",
    })).toEqual({
      kind: "daemon.getUsageSummary",
      scopeKind: "workspace",
      window: "last_7_days",
    });
    expect(parseBridgeRequest({
      kind: "daemon.getUsageSummary",
      scopeKind: "thread",
      scopeId: "thread-1",
      window: "last_30_days",
    })).toEqual({
      kind: "daemon.getUsageSummary",
      scopeKind: "thread",
      scopeId: "thread-1",
      window: "last_30_days",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.getUsageSummary",
      scopeKind: "workspace",
      scopeId: "extra",
      window: "last_7_days",
    })).toThrow("workspace usage summary must not include a scope id");
    expect(() => parseBridgeRequest({
      kind: "daemon.getUsageSummary",
      scopeKind: "project",
      window: "last_7_days",
    })).toThrow("usage scope id is required");
    expect(() => parseBridgeRequest({
      kind: "daemon.getUsageSummary",
      scopeKind: "thread",
      scopeId: "thread-1",
      window: "weekly",
    })).toThrow("usage window is invalid");
  });

  it("accepts only bounded idempotent conversation start requests", () => {
    expect(parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      idempotencyKey: "turn-command-1",
    })).toEqual({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      idempotencyKey: "turn-command-1",
    });
    expect(parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      modelId: "grok-4.3",
      idempotencyKey: "turn-command-model-1",
    })).toEqual({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      modelId: "grok-4.3",
      idempotencyKey: "turn-command-model-1",
    });
    expect(parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      modelId: undefined,
      idempotencyKey: "turn-command-model-2",
    })).toEqual({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      idempotencyKey: "turn-command-model-2",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      idempotencyKey: "turn-command-1",
      role: "assistant",
      model: "untrusted-model",
    })).toThrow("conversation start request contains unsupported fields");
    expect(() => parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      modelId: "",
      idempotencyKey: "turn-command-bad-model-1",
    })).toThrow("chat model identifier is invalid");
    expect(() => parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "Ask official Grok",
      modelId: "grok\nunsafe",
      idempotencyKey: "turn-command-bad-model-2",
    })).toThrow("chat model identifier is invalid");
    expect(() => parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "",
      idempotencyKey: "turn-command-2",
    })).toThrow("message content is invalid");
    expect(() => parseBridgeRequest({
      kind: "daemon.startConversationTurn",
      threadId: "thread-1",
      content: "x".repeat(1024 * 1024 + 1),
      idempotencyKey: "turn-command-3",
    })).toThrow("message content is invalid");
  });

  it("accepts only exact idempotent conversation cancellation requests", () => {
    expect(parseBridgeRequest({
      kind: "daemon.cancelConversationTurn",
      turnId: "turn-1",
      expectedRevision: 2,
      idempotencyKey: "cancel-turn-command-1",
    })).toEqual({
      kind: "daemon.cancelConversationTurn",
      turnId: "turn-1",
      expectedRevision: 2,
      idempotencyKey: "cancel-turn-command-1",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.cancelConversationTurn",
      turnId: "turn-1",
      expectedRevision: Number.MAX_SAFE_INTEGER + 1,
      idempotencyKey: "cancel-turn-command-2",
    })).toThrow("expected turn revision must be a non-negative safe integer");
    expect(() => parseBridgeRequest({
      kind: "daemon.cancelConversationTurn",
      turnId: "turn-1",
      expectedRevision: 2,
      reason: "forged",
      idempotencyKey: "cancel-turn-command-3",
    })).toThrow("conversation cancellation request contains unsupported fields");
  });

  it("accepts only exact content-free conversation retry requests", () => {
    expect(parseBridgeRequest({
      kind: "daemon.retryConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "retry-turn-command-1",
    })).toEqual({
      kind: "daemon.retryConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "retry-turn-command-1",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.retryConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: Number.MAX_SAFE_INTEGER + 1,
      idempotencyKey: "retry-turn-command-2",
    })).toThrow("expected source turn revision must be a non-negative safe integer");
    for (const forged of [
      { content: "renderer-selected prompt" },
      { modelId: "renderer-selected-model" },
      { accountId: "renderer-selected-account" },
      { continuation: "provider-response-id" },
      { desiredState: "completed" },
    ]) {
      expect(() => parseBridgeRequest({
        kind: "daemon.retryConversationTurn",
        sourceTurnId: "turn-source",
        expectedRevision: 2,
        idempotencyKey: "retry-turn-command-3",
        ...forged,
      })).toThrow("conversation retry request contains unsupported fields");
    }
  });

  it("accepts only the canonical fork request surfaces", () => {
    expect(parseBridgeRequest({
      kind: "daemon.branchConversationThread",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "branch-command-1",
    })).toEqual({
      kind: "daemon.branchConversationThread",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "branch-command-1",
    });
    expect(parseBridgeRequest({
      kind: "daemon.editAndBranchConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      content: "Edited question",
      idempotencyKey: "edit-branch-command-1",
    })).toEqual({
      kind: "daemon.editAndBranchConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      content: "Edited question",
      idempotencyKey: "edit-branch-command-1",
    });
    expect(parseBridgeRequest({
      kind: "daemon.regenerateConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "regenerate-command-1",
    })).toEqual({
      kind: "daemon.regenerateConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      idempotencyKey: "regenerate-command-1",
    });
    expect(parseBridgeRequest({
      kind: "daemon.getConversationForkMetadata",
      threadId: "thread-child",
    })).toEqual({
      kind: "daemon.getConversationForkMetadata",
      threadId: "thread-child",
    });
    expect(parseBridgeRequest({
      kind: "daemon.acknowledgeConversationForkDelivery",
      childThreadId: "thread-child",
      expectedRevision: 0,
      idempotencyKey: "fork-delivery-ack-1",
    })).toEqual({
      kind: "daemon.acknowledgeConversationForkDelivery",
      childThreadId: "thread-child",
      expectedRevision: 0,
      idempotencyKey: "fork-delivery-ack-1",
    });

    expect(() => parseBridgeRequest({
      kind: "daemon.branchConversationThread",
      sourceTurnId: "turn-source",
      expectedRevision: Number.MAX_SAFE_INTEGER + 1,
      idempotencyKey: "branch-command-invalid",
    })).toThrow("expected source turn revision must be a non-negative safe integer");
    expect(() => parseBridgeRequest({
      kind: "daemon.editAndBranchConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 2,
      content: "",
      idempotencyKey: "edit-branch-command-invalid",
    })).toThrow("message content is invalid");
    expect(() => parseBridgeRequest({
      kind: "daemon.acknowledgeConversationForkDelivery",
      childThreadId: "thread-child",
      expectedRevision: 1,
      idempotencyKey: "fork-delivery-ack-invalid",
    })).toThrow("requires pending revision zero");

    for (const request of [
      {
        kind: "daemon.branchConversationThread",
        sourceTurnId: "turn-source",
        expectedRevision: 2,
        idempotencyKey: "branch-command-2",
        title: "Renderer-selected title",
      },
      {
        kind: "daemon.editAndBranchConversationTurn",
        sourceTurnId: "turn-source",
        expectedRevision: 2,
        content: "Edited question",
        idempotencyKey: "edit-branch-command-2",
        modelId: "renderer-selected-model",
      },
      {
        kind: "daemon.regenerateConversationTurn",
        sourceTurnId: "turn-source",
        expectedRevision: 2,
        idempotencyKey: "regenerate-command-2",
        content: "renderer-selected prompt",
      },
      {
        kind: "daemon.getConversationForkMetadata",
        threadId: "thread-child",
        rootThreadId: "renderer-selected-root",
      },
      {
        kind: "daemon.acknowledgeConversationForkDelivery",
        childThreadId: "thread-child",
        expectedRevision: 0,
        idempotencyKey: "fork-delivery-ack-2",
        acknowledged: true,
      },
    ]) {
      expect(() => parseBridgeRequest(request)).toThrow("contains unsupported fields");
    }
  });

  it("validates automation policies and bounds", () => {
    expect(() => parseBridgeRequest({
      kind: "daemon.createAutomation",
      projectId: "project-1",
      title: "Daily brief",
      prompt: "Summarize changes",
      schedule: "daily",
      timezone: "Europe/Paris",
      missedRunPolicy: "replay_everything",
      overlapPolicy: "queue_one",
      idempotencyKey: "automation-command-1",
    })).toThrow("invalid missed-run policy");
    expect(() => parseBridgeRequest({
      kind: "daemon.createAutomation",
      projectId: "project-1",
      title: "Daily brief",
      prompt: "Summarize changes",
      schedule: "daily",
      timezone: "Europe/Paris",
      missedRunPolicy: "skip",
      overlapPolicy: "queue_one",
      idempotencyKey: "automation-command-2",
      localExecutable: "/tmp/untrusted",
    })).toThrow("automation request contains unsupported fields");
    expect(() => parseBridgeRequest({
      kind: "daemon.createAutomation",
      projectId: "project-1",
      title: "Daily brief",
      prompt: "Summarize changes",
      schedule: "v1;daily;09:00",
      timezone: "Europe/Paris",
      missedRunPolicy: "skip",
      overlapPolicy: "queue_one",
      enabled: true,
      idempotencyKey: "automation-command-3",
    })).toThrow("automation request contains unsupported fields");
  });

  it("allows secret-free enrollment and deletion but rejects credential material", () => {
    expect(parseBridgeRequest({
      kind: "daemon.enrollXaiApiKey",
      idempotencyKey: "credential-command-1",
    })).toEqual({ kind: "daemon.enrollXaiApiKey", idempotencyKey: "credential-command-1" });
    expect(parseBridgeRequest({
      kind: "daemon.deleteXaiApiKey",
      idempotencyKey: "credential-command-2",
    })).toEqual({ kind: "daemon.deleteXaiApiKey", idempotencyKey: "credential-command-2" });
    expect(() => parseBridgeRequest({
      kind: "daemon.enrollXaiApiKey",
      apiKey: "fixture-value",
      idempotencyKey: "credential-command-3",
    })).toThrow("credential enrollment request contains unsupported fields");
    expect(() => parseBridgeRequest({
      kind: "daemon.deleteXaiApiKey",
      idempotencyKey: "credential-command-4",
      apiKey: "fixture-value",
    })).toThrow("credential deletion request contains unsupported fields");
  });

  it("rejects undeclared fields on parameterless requests", () => {
    expect(parseBridgeRequest({ kind: "daemon.bootstrap" })).toEqual({ kind: "daemon.bootstrap" });
    expect(() => parseBridgeRequest({
      kind: "daemon.bootstrap",
      localPath: "/tmp/bootstrap.json",
    })).toThrow("daemon.bootstrap request contains unsupported fields");
  });

  it("accepts only an exact canonical external HTTPS navigation request", () => {
    expect(parseBridgeRequest({
      kind: "desktop.openExternalUrl",
      url: "https://docs.x.ai/docs/guides#sources",
    })).toEqual({
      kind: "desktop.openExternalUrl",
      url: "https://docs.x.ai/docs/guides#sources",
    });
    expect(() => parseBridgeRequest({
      kind: "desktop.openExternalUrl",
      url: "https://docs.x.ai/",
      disposition: "in_app",
    })).toThrow("external URL request contains unsupported fields");
    expect(() => parseBridgeRequest({
      kind: "desktop.openExternalUrl",
      url: "file:///tmp/source.html",
    })).toThrow();
  });

  it("accepts only bounded, secret-free desktop preference mutations", () => {
    expect(parseBridgeRequest({ kind: "daemon.getDesktopPreferences" })).toEqual({
      kind: "daemon.getDesktopPreferences",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.getDesktopPreferences",
      localPath: "/tmp/preferences.json",
    })).toThrow("desktop preference request contains unsupported fields");
    expect(parseBridgeRequest({
      kind: "daemon.updateDesktopPreferences",
      expectedRevision: 2,
      keepRunningInNotificationArea: false,
      idempotencyKey: "desktop-preference-1",
    })).toEqual({
      kind: "daemon.updateDesktopPreferences",
      expectedRevision: 2,
      keepRunningInNotificationArea: false,
      idempotencyKey: "desktop-preference-1",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.updateDesktopPreferences",
      expectedRevision: -1,
      keepRunningInNotificationArea: false,
      idempotencyKey: "desktop-preference-2",
    })).toThrow("desktop preference revision must be a non-negative safe integer");
    expect(() => parseBridgeRequest({
      kind: "daemon.updateDesktopPreferences",
      expectedRevision: 0,
      keepRunningInNotificationArea: false,
      localPath: "/tmp/preferences.json",
      idempotencyKey: "desktop-preference-3",
    })).toThrow("desktop preference request contains unsupported fields");
  });

  it("accepts exact bounded model operations and rejects endpoint or policy injection", () => {
    expect(parseBridgeRequest({ kind: "daemon.getChatModelCatalog" })).toEqual({
      kind: "daemon.getChatModelCatalog",
    });
    expect(parseBridgeRequest({
      kind: "daemon.selectChatModel",
      expectedRevision: 2,
      modelId: "grok-4.3",
      idempotencyKey: "chat-model-1",
    })).toEqual({
      kind: "daemon.selectChatModel",
      expectedRevision: 2,
      modelId: "grok-4.3",
      idempotencyKey: "chat-model-1",
    });
    expect(() => parseBridgeRequest({
      kind: "daemon.selectChatModel",
      expectedRevision: 2,
      modelId: "grok-4.3",
      endpoint: "https://compatible.example.test/v1",
      idempotencyKey: "chat-model-2",
    })).toThrow("chat model selection request contains unsupported fields");
    expect(() => parseBridgeRequest({
      kind: "daemon.selectChatModel",
      expectedRevision: 2,
      modelId: " padded ",
      idempotencyKey: "chat-model-3",
    })).toThrow("chat model identifier is invalid");
    for (const modelId of ["x".repeat(513), "grok\nunsafe"]) {
      expect(() => parseBridgeRequest({
        kind: "daemon.selectChatModel",
        expectedRevision: 2,
        modelId,
        idempotencyKey: "chat-model-4",
      })).toThrow("chat model identifier is invalid");
    }
    expect(() => parseBridgeRequest({
      kind: "daemon.selectChatModel",
      expectedRevision: Number.MAX_SAFE_INTEGER + 1,
      modelId: "grok-4.3",
      idempotencyKey: "chat-model-5",
    })).toThrow("chat model preference revision must be a non-negative safe integer");
    expect(() => parseBridgeRequest({
      kind: "daemon.getChatModelCatalog",
      apiKey: "fixture-value",
    })).toThrow("chat model catalog request contains unsupported fields");
  });
});
