// @vitest-environment node
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  DaemonConversationForkMetadata,
  DaemonThread,
} from "../../src/contracts/bridge.js";
import {
  type Approval,
  ApprovalRisk,
  ApprovalScope,
  ApprovalStatus,
  ArtifactOpenFailureCode,
  ArtifactOpenReceiptStatus,
  ArtifactState,
  AutomationSchedulerHealth,
  AutomationState,
  type ChatModelCatalog,
  type ConversationForkDelivery,
  ConversationForkDeliveryState,
  ConversationForkKind,
  ConversationMessageDerivationKind,
  type ConversationForkMetadata,
  ConversationRetryEligibility,
  type ConversationTurnResult,
  ConversationTurnOrigin,
  ConversationTurnState,
  type Message,
  MessageRole,
  MessageState,
  MissedRunPolicy,
  OverlapPolicy,
  RunState,
  type Thread,
  ThreadState,
  type WorkspaceSearchResults,
  WorkspaceSearchKind,
} from "../generated/daemon/v1/daemon.js";
import { DaemonTransportError, PROTOCOL_VERSION } from "./DaemonRpcClient.js";
import {
  DaemonSupervisor,
  daemonBootstrapInput,
  daemonEnvironment,
  mapApprovalDecisionResponse,
  mapArtifact,
  mapArtifactRemovalOperation,
  mapArtifactOpenOperation,
  mapAutomationSchedulerHealth,
  mapChatModelCatalog,
  mapConversationFork,
  mapConversationForkMetadata,
  mapConversationTurn,
  mapMessage,
  mapImportedArtifactOperation,
  mapListedAutomation,
  mapRemovedArtifactOperation,
  mapRetryConversationTurnResponse,
  mapThread,
  mapWorkspaceSearchResults,
  validateConversationAggregate,
  type DaemonSupervisorOptions,
  resolveDaemonBinary,
} from "./DaemonSupervisor.js";

function pendingForkDelivery(childThreadId = "thread-child"): ConversationForkDelivery {
  return {
    childThreadId,
    state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_PENDING,
    revision: 0n,
  };
}

describe("daemon protocol compatibility", () => {
  it("matches the Rust grok-protocol PROTOCOL_VERSION so daemon startup cannot skew", () => {
    const rustSource = readFileSync(
      fileURLToPath(new URL("../../../../crates/grok-protocol/src/lib.rs", import.meta.url)),
      "utf8",
    );
    const match = rustSource.match(/pub const PROTOCOL_VERSION: u32 = (\d+);/);
    expect(match, "PROTOCOL_VERSION constant not found in grok-protocol").not.toBeNull();
    expect(PROTOCOL_VERSION).toBe(Number(match![1]));
  });
});

describe("automation scheduler lifecycle validation", () => {
  it.each([
    [
      AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_KERNEL_INITIALIZED_EXECUTION_DISABLED,
      "kernel_initialized_execution_disabled",
    ],
    [
      AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_RECOVERY_PENDING_EXECUTION_DISABLED,
      "recovery_pending_execution_disabled",
    ],
    [
      AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_DEGRADED_EXECUTION_DISABLED,
      "degraded_execution_disabled",
    ],
  ] as const)("maps closed health %s without enabling execution", (wire, state) => {
    expect(mapAutomationSchedulerHealth(wire)).toEqual({ state });
  });

  it("rejects unspecified and unknown scheduler health", () => {
    expect(() => mapAutomationSchedulerHealth(
      AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_UNSPECIFIED,
    )).toThrow("invalid automation scheduler health");
    expect(() => mapAutomationSchedulerHealth(AutomationSchedulerHealth.UNRECOGNIZED))
      .toThrow("invalid automation scheduler health");
  });
});

describe("automation projection validation", () => {
  const automation = {
    id: "automation-1",
    projectId: "project-1",
    title: "Release readiness",
    prompt: "Review current release blockers.",
    schedule: JSON.stringify({
      frequency: "weekdays",
      localTime: "09:00",
      timeZoneIana: "UTC",
    }),
    timezone: "UTC",
    missedRunPolicy: MissedRunPolicy.MISSED_RUN_POLICY_RUN_ONCE,
    overlapPolicy: OverlapPolicy.OVERLAP_POLICY_QUEUE_ONE,
    state: AutomationState.AUTOMATION_STATE_DISABLED,
    revision: 0n,
    createdAtUnixMs: 1n,
    updatedAtUnixMs: 1n,
  };

  it("accepts the domain prompt bound and rejects a larger automation prompt", () => {
    const maximumPrompt = "x".repeat(64 * 1024);
    expect(mapListedAutomation({ ...automation, prompt: maximumPrompt }, "project-1").prompt)
      .toHaveLength(maximumPrompt.length);
    expect(() => mapListedAutomation({ ...automation, prompt: `${maximumPrompt}x` }, "project-1"))
      .toThrow("automation prompt is invalid");
  });

  it("rejects an automation listed under a different project", () => {
    expect(() => mapListedAutomation(automation, "project-other"))
      .toThrow("daemon automation project does not match the requested project");
  });
});

describe("artifact projection and operation validation", () => {
  const artifact = {
    id: "artifact-1",
    projectId: "project-1",
    threadId: "",
    name: "report.pdf",
    mediaType: "application/pdf",
    byteSize: 42n,
    state: ArtifactState.ARTIFACT_STATE_AVAILABLE,
    revision: 7n,
    createdAtUnixMs: 1n,
    updatedAtUnixMs: 2n,
    contentVersion: 7,
  };

  it("maps unavailable artifacts without inventing content metadata", () => {
    expect(mapArtifact({
      ...artifact,
      mediaType: "",
      byteSize: 0n,
      state: ArtifactState.ARTIFACT_STATE_UNAVAILABLE,
      contentVersion: undefined,
      revision: 0n,
      updatedAtUnixMs: 1n,
    })).toEqual(expect.objectContaining({
      state: "unavailable",
      mediaType: undefined,
      byteSize: undefined,
      contentVersion: undefined,
    }));
    expect(() => mapArtifact({
      ...artifact,
      state: ArtifactState.ARTIFACT_STATE_UNAVAILABLE,
      revision: 0n,
      updatedAtUnixMs: 1n,
    })).toThrow("non-available artifact exposes content metadata");
  });

  it("rejects inconsistent available, deleted, and oversized artifact projections", () => {
    expect(() => mapArtifact({ ...artifact, contentVersion: undefined }))
      .toThrow("available artifact content metadata is invalid");
    expect(() => mapArtifact({ ...artifact, revision: 1n }))
      .toThrow("available artifact content metadata is invalid");
    expect(() => mapArtifact({ ...artifact, byteSize: BigInt(64 * 1024 * 1024 + 1) }))
      .toThrow("artifact byte size is invalid");
    expect(() => mapArtifact({
      ...artifact,
      state: ArtifactState.ARTIFACT_STATE_DELETED,
      mediaType: "",
      byteSize: 0n,
      contentVersion: undefined,
      revision: 0n,
    })).toThrow("artifact lifecycle metadata is invalid");
  });

  it("accepts only the expected imported-artifact result", () => {
    const mapped = mapImportedArtifactOperation({
      result: { $case: "importedArtifact", value: artifact },
    }, "project-1", "report.pdf", "application/pdf");
    expect(mapped).toMatchObject({
      id: "artifact-1",
      state: "available",
      contentVersion: 7,
    });
    expect(JSON.stringify(mapped)).not.toContain("sourcePath");
    expect(() => mapImportedArtifactOperation({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-1",
          contentVersion: 7,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_OPENED,
        },
      },
    }, "project-1", "report.pdf", "application/pdf"))
      .toThrow("wrong result variant");
  });

  it("maps every closed exact-version open status and rejects malformed receipts", () => {
    expect(mapArtifactOpenOperation({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-1",
          contentVersion: 7,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_OPENED,
        },
      },
    }, "artifact-1", 7)).toEqual({
      artifactId: "artifact-1",
      contentVersion: 7,
      status: "opened",
    });
    expect(mapArtifactOpenOperation({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-1",
          contentVersion: 7,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_INTERRUPTED_NEEDS_REVIEW,
        },
      },
    }, "artifact-1", 7)).toEqual({
      artifactId: "artifact-1",
      contentVersion: 7,
      status: "interrupted_needs_review",
    });

    for (const [wireFailure, failureCode] of [
      [
        ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_CONTENT_UNAVAILABLE,
        "content_unavailable",
      ],
      [
        ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_PLATFORM_UNAVAILABLE,
        "platform_unavailable",
      ],
      [
        ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_DEADLINE_EXCEEDED,
        "deadline_exceeded",
      ],
      [
        ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTEGRITY_FAILURE,
        "integrity_failure",
      ],
      [
        ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTERRUPTED_BEFORE_DISPATCH,
        "interrupted_before_dispatch",
      ],
    ] as const) {
      expect(mapArtifactOpenOperation({
        result: {
          $case: "openReceipt",
          value: {
            artifactId: "artifact-1",
            contentVersion: 7,
            status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED,
            failureCode: wireFailure,
          },
        },
      }, "artifact-1", 7)).toEqual({
        artifactId: "artifact-1",
        contentVersion: 7,
        status: "failed",
        failureCode,
      });
    }
    expect(() => mapArtifactOpenOperation({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-other",
          contentVersion: 7,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_OPENED,
        },
      },
    }, "artifact-1", 7)).toThrow("does not match the request");
    expect(() => mapArtifactOpenOperation({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-1",
          contentVersion: 7,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_UNSPECIFIED,
        },
      },
    }, "artifact-1", 7)).toThrow("invalid daemon artifact open receipt status");

    for (const value of [
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED,
      },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED,
        failureCode: ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_UNSPECIFIED,
      },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_OPENED,
        failureCode: ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_CONTENT_UNAVAILABLE,
      },
      {
        artifactId: "artifact-1",
        contentVersion: 7,
        status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_INTERRUPTED_NEEDS_REVIEW,
        failureCode: ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_PLATFORM_UNAVAILABLE,
      },
    ]) {
      expect(() => mapArtifactOpenOperation({
        result: { $case: "openReceipt", value },
      }, "artifact-1", 7)).toThrow();
    }
  });

  it("accepts only the exact canonical removed-artifact tombstone", () => {
    const removed = {
      ...artifact,
      state: ArtifactState.ARTIFACT_STATE_DELETED,
      mediaType: "",
      byteSize: 0n,
      contentVersion: undefined,
      revision: 8n,
      updatedAtUnixMs: 3n,
    };
    expect(mapRemovedArtifactOperation({
      result: { $case: "removedArtifact", value: removed },
    }, "artifact-1", 7)).toMatchObject({
      id: "artifact-1",
      state: "deleted",
      revision: 8,
      contentVersion: undefined,
    });
    expect(() => mapRemovedArtifactOperation({
      result: { $case: "importedArtifact", value: artifact },
    }, "artifact-1", 7)).toThrow("wrong result variant");
    expect(() => mapRemovedArtifactOperation({
      result: { $case: "removedArtifact", value: { ...removed, revision: 9n } },
    }, "artifact-1", 7)).toThrow("does not match the request");
    expect(() => mapRemovedArtifactOperation({
      result: { $case: "removedArtifact", value: { ...removed, id: "artifact-other" } },
    }, "artifact-1", 7)).toThrow("does not match the request");

    expect(mapArtifactRemovalOperation({
      result: {
        $case: "removalPending",
        value: {
          artifactId: "artifact-1",
          expectedRevision: 7n,
          expectedContentVersion: 7,
          tombstone: removed,
        },
      },
    }, "artifact-1", 7, 7)).toEqual({
      status: "pending",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      tombstone: expect.objectContaining({
        id: "artifact-1",
        state: "deleted",
        revision: 8,
      }),
    });
    expect(mapArtifactRemovalOperation({
      result: {
        $case: "removedArtifact",
        value: removed,
      },
    }, "artifact-1", 7, 7)).toMatchObject({
      status: "removed",
      artifact: { id: "artifact-1", state: "deleted", revision: 8 },
    });
    for (const value of [
      {
        artifactId: "artifact-other",
        expectedRevision: 7n,
        expectedContentVersion: 7,
        tombstone: removed,
      },
      {
        artifactId: "artifact-1",
        expectedRevision: 8n,
        expectedContentVersion: 7,
        tombstone: removed,
      },
      {
        artifactId: "artifact-1",
        expectedRevision: 7n,
        expectedContentVersion: 8,
        tombstone: removed,
      },
      {
        artifactId: "artifact-1",
        expectedRevision: 7n,
        expectedContentVersion: 7,
        tombstone: undefined,
      },
      {
        artifactId: "artifact-1",
        expectedRevision: 7n,
        expectedContentVersion: 7,
        tombstone: { ...removed, revision: 9n },
      },
    ]) {
      expect(() => mapArtifactRemovalOperation({
        result: { $case: "removalPending", value },
      }, "artifact-1", 7, 7)).toThrow();
    }
  });
});

describe("conversation fork lineage validation", () => {
  it("maps closed original and forked thread ancestry", () => {
    expect(mapThread(validThread()).lineage).toEqual({
      origin: "original",
      rootThreadId: "thread-root",
      forkDepth: 0,
    });

    const forked = validThread();
    forked.id = "thread-child";
    forked.lineage = {
      rootThreadId: "thread-root",
      forkDepth: 2,
      origin: {
        $case: "fork",
        value: {
          parentThreadId: "thread-parent",
          sourceTurnId: "turn-source",
          sourceMessageId: "message-source",
          kind: ConversationForkKind.CONVERSATION_FORK_KIND_EDIT_AND_BRANCH,
        },
      },
    };

    expect(mapThread(forked).lineage).toEqual({
      origin: "fork",
      rootThreadId: "thread-root",
      parentThreadId: "thread-parent",
      sourceTurnId: "turn-source",
      sourceMessageId: "message-source",
      kind: "edit_and_branch",
      forkDepth: 2,
    });
  });

  it("rejects malformed, missing, and open-ended thread ancestry", () => {
    const missing = validThread();
    missing.lineage = undefined;
    expect(() => mapThread(missing)).toThrow("thread lineage is missing");

    const invalidDepth = validThread();
    invalidDepth.id = "thread-child";
    invalidDepth.lineage = {
      rootThreadId: "thread-root",
      forkDepth: 1,
      origin: {
        $case: "fork",
        value: {
          parentThreadId: "thread-other",
          sourceTurnId: "turn-source",
          sourceMessageId: "message-source",
          kind: ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH,
        },
      },
    };
    expect(() => mapThread(invalidDepth)).toThrow("forked thread lineage is invalid");

    if (invalidDepth.lineage.origin?.$case !== "fork") throw new Error("fork fixture is invalid");
    invalidDepth.lineage.origin.value.parentThreadId = "thread-root";
    invalidDepth.lineage.origin.value.kind = ConversationForkKind.UNRECOGNIZED;
    expect(() => mapThread(invalidDepth)).toThrow("invalid daemon conversation fork kind");
  });

  it("maps bounded derived-message ancestry and enforces role-specific forms", () => {
    const contextCopy = validMessage();
    contextCopy.id = "message-copy";
    contextCopy.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source",
          sourceTurnId: "turn-source",
          contextPosition: 1_000,
          kind: ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_CONTEXT_COPY,
        },
      },
    };
    expect(mapMessage(contextCopy).derivation).toEqual({
      origin: "fork",
      sourceMessageId: "message-source",
      sourceTurnId: "turn-source",
      contextPosition: 1_000,
      kind: "context_copy",
    });

    const assistantCopy = validMessage();
    assistantCopy.id = "message-assistant-copy";
    assistantCopy.role = MessageRole.MESSAGE_ROLE_ASSISTANT;
    assistantCopy.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-assistant-source",
          sourceTurnId: "turn-source",
          kind: ConversationMessageDerivationKind
            .CONVERSATION_MESSAGE_DERIVATION_KIND_SOURCE_ASSISTANT_COPY,
        },
      },
    };
    expect(mapMessage(assistantCopy).derivation).toMatchObject({ kind: "source_assistant_copy" });

    assistantCopy.role = MessageRole.MESSAGE_ROLE_USER;
    expect(() => mapMessage(assistantCopy)).toThrow("assistant-copy derivation is invalid");

    if (contextCopy.derivation.origin?.$case !== "fork") throw new Error("fork fixture is invalid");
    contextCopy.derivation.origin.value.contextPosition = 1_001;
    expect(() => mapMessage(contextCopy)).toThrow("forked message derivation is invalid");
    contextCopy.derivation.origin.value.contextPosition = 1_000;
    contextCopy.state = MessageState.MESSAGE_STATE_DELETED;
    expect(() => mapMessage(contextCopy)).toThrow("deleted message derivation is invalid");
  });

  it("maps edit-and-branch and regenerate turn origins as closed variants", () => {
    const turn = validConversationTurn();
    for (const [wireOrigin, expectedOrigin] of [
      [ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_EDIT_AND_BRANCH, "edit_and_branch"],
      [ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_REGENERATE, "regenerate"],
    ] as const) {
      turn.lineage = { origin: wireOrigin, sourceTurnId: "turn-source", retryDepth: 0 };
      expect(mapConversationTurn(turn).lineage).toEqual({
        origin: expectedOrigin,
        sourceTurnId: "turn-source",
        retryDepth: 0,
      });
    }

    turn.lineage!.retryDepth = 1;
    expect(() => mapConversationTurn(turn)).toThrow("fork conversation turn lineage is invalid");
  });
});

describe("conversation fork response validation", () => {
  it("maps valid Branch, Edit-and-branch, and Regenerate results", () => {
    const branch = mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: pendingForkDelivery(),
      },
      "branch",
      "turn-source",
    );
    expect(branch).toMatchObject({
      childThread: { id: "thread-child", lineage: { kind: "branch", sourceTurnId: "turn-source" } },
      startedTurn: undefined,
    });

    for (const [kind, expectedKind] of [
      [ConversationForkKind.CONVERSATION_FORK_KIND_EDIT_AND_BRANCH, "edit_and_branch"],
      [ConversationForkKind.CONVERSATION_FORK_KIND_REGENERATE, "regenerate"],
    ] as const) {
      const mapped = mapConversationFork(
        {
          childThread: forkedThread(kind),
          startedTurn: forkedTurn(expectedKind),
          delivery: pendingForkDelivery(),
        },
        expectedKind,
        "turn-source",
      );
      expect(mapped.startedTurn).toMatchObject({
        lineage: { origin: expectedKind, sourceTurnId: "turn-source" },
        run: { threadId: "thread-child" },
      });
    }
  });

  it("rejects missing children, lineage, required turns, and mismatched fork turns", () => {
    expect(() => mapConversationFork(
      { childThread: undefined, startedTurn: undefined, delivery: pendingForkDelivery() },
      "branch",
      "turn-source",
    )).toThrow("missing its child thread");

    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: undefined,
      },
      "branch",
      "turn-source",
    )).toThrow("missing its delivery state");

    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: pendingForkDelivery("thread-other"),
      },
      "branch",
      "turn-source",
    )).toThrow("belongs to another child");

    const invalidDelivery = pendingForkDelivery();
    invalidDelivery.revision = 1n;
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: invalidDelivery,
      },
      "branch",
      "turn-source",
    )).toThrow("delivery state is inconsistent");

    const acknowledgedAtZero = pendingForkDelivery();
    acknowledgedAtZero.state =
      ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_ACKNOWLEDGED;
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: acknowledgedAtZero,
      },
      "branch",
      "turn-source",
    )).toThrow("delivery state is inconsistent");

    const unspecified = pendingForkDelivery();
    unspecified.state =
      ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_UNSPECIFIED;
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: undefined,
        delivery: unspecified,
      },
      "branch",
      "turn-source",
    )).toThrow("invalid daemon conversation fork delivery state");

    const missingLineage = forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH);
    missingLineage.lineage = undefined;
    expect(() => mapConversationFork(
      { childThread: missingLineage, startedTurn: undefined, delivery: pendingForkDelivery() },
      "branch",
      "turn-source",
    )).toThrow("thread lineage is missing");

    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH),
        startedTurn: forkedTurn("edit_and_branch"),
        delivery: pendingForkDelivery(),
      },
      "branch",
      "turn-source",
    )).toThrow("invalid turn presence");
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_EDIT_AND_BRANCH),
        startedTurn: undefined,
        delivery: pendingForkDelivery(),
      },
      "edit_and_branch",
      "turn-source",
    )).toThrow("invalid turn presence");

    const wrongTurn = forkedTurn("regenerate");
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_EDIT_AND_BRANCH),
        startedTurn: wrongTurn,
        delivery: pendingForkDelivery(),
      },
      "edit_and_branch",
      "turn-source",
    )).toThrow("turn is not owned by its child");

    wrongTurn.lineage = undefined;
    expect(() => mapConversationFork(
      {
        childThread: forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_REGENERATE),
        startedTurn: wrongTurn,
        delivery: pendingForkDelivery(),
      },
      "regenerate",
      "turn-source",
    )).toThrow("conversation turn is incomplete");
  });

  it("maps bounded metadata and rejects incomplete outcomes and malformed families", () => {
    const valid = validForkMetadata();
    expect(mapConversationForkMetadata(valid, "thread-child")).toMatchObject({
      lineage: { origin: "fork", kind: "branch", sourceTurnId: "turn-source" },
      inheritedAssistantOutcomes: [{
        childAssistantMessageId: "message-child-assistant",
        sourceTurnId: "turn-source",
        usage: { inputTokens: 7, outputTokens: 11, costInUsdTicks: 13 },
      }],
      familyThreads: [{ id: "thread-root" }, { id: "thread-child" }],
    });

    const missingLineage = validForkMetadata();
    missingLineage.lineage = undefined;
    expect(() => mapConversationForkMetadata(missingLineage, "thread-child"))
      .toThrow("missing lineage");

    const missingUsage = validForkMetadata();
    missingUsage.inheritedAssistantOutcomes[0].usage = undefined;
    expect(() => mapConversationForkMetadata(missingUsage, "thread-child"))
      .toThrow("outcome is incomplete");

    const duplicateOutcome = validForkMetadata();
    duplicateOutcome.inheritedAssistantOutcomes.push({
      ...duplicateOutcome.inheritedAssistantOutcomes[0],
    });
    expect(() => mapConversationForkMetadata(duplicateOutcome, "thread-child"))
      .toThrow("outcome is duplicated");

    const emptyFamily = validForkMetadata();
    emptyFamily.familyThreads = [];
    expect(() => mapConversationForkMetadata(emptyFamily, "thread-child"))
      .toThrow("metadata exceeds its bounds");

    const duplicateFamily = validForkMetadata();
    duplicateFamily.familyThreads.push({ ...duplicateFamily.familyThreads[0] });
    expect(() => mapConversationForkMetadata(duplicateFamily, "thread-child"))
      .toThrow("family contains duplicate threads");

    const malformedFamily = validForkMetadata();
    malformedFamily.familyThreads[0].lineage = undefined;
    expect(() => mapConversationForkMetadata(malformedFamily, "thread-child"))
      .toThrow("thread lineage is missing");

    const tooManyOutcomes = validForkMetadata();
    tooManyOutcomes.inheritedAssistantOutcomes = Array.from({ length: 257 }, (_entry, index) => ({
      ...structuredClone(tooManyOutcomes.inheritedAssistantOutcomes[0]),
      childAssistantMessageId: `message-child-${index}`,
    }));
    expect(() => mapConversationForkMetadata(tooManyOutcomes, "thread-child"))
      .toThrow("metadata exceeds its bounds");

    const oversizedMetadata = validForkMetadata();
    const longUrl = `https://example.test/${"x".repeat(7_400)}`;
    oversizedMetadata.inheritedAssistantOutcomes = Array.from({ length: 5 }, (_outcome, outcomeIndex) => ({
      ...structuredClone(oversizedMetadata.inheritedAssistantOutcomes[0]),
      childAssistantMessageId: `message-large-${outcomeIndex}`,
      citations: Array.from({ length: 100 }, (_citation, citationIndex) => ({
        title: `Source ${citationIndex}`,
        url: longUrl,
      })),
    }));
    expect(() => mapConversationForkMetadata(oversizedMetadata, "thread-child"))
      .toThrow("metadata exceeds its byte bound");
  });

  it("accepts inherited assistant context and source copies only with matching outcome ancestry", () => {
    const wireThread = forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH);
    if (wireThread.lineage?.origin?.$case !== "fork") throw new Error("fork fixture is invalid");
    wireThread.lineage.origin.value.sourceMessageId = "message-source-assistant";
    const thread = mapThread(wireThread);
    const wireUser = validMessage();
    wireUser.id = "message-child-user";
    wireUser.threadId = thread.id;
    wireUser.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source-user",
          sourceTurnId: "turn-source",
          contextPosition: 1,
          kind: ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_CONTEXT_COPY,
        },
      },
    };
    const user = mapMessage(wireUser);
    const wireAssistant = validMessage();
    wireAssistant.id = "message-child-assistant";
    wireAssistant.threadId = thread.id;
    wireAssistant.role = MessageRole.MESSAGE_ROLE_ASSISTANT;
    wireAssistant.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source-assistant",
          sourceTurnId: "turn-source",
          kind: ConversationMessageDerivationKind
            .CONVERSATION_MESSAGE_DERIVATION_KIND_SOURCE_ASSISTANT_COPY,
        },
      },
    };
    wireAssistant.sequence = 2n;
    const assistant = mapMessage(wireAssistant);
    const wireMetadata = validForkMetadata();
    wireMetadata.lineage = wireThread.lineage;
    wireMetadata.familyThreads = wireMetadata.familyThreads.map((familyThread) => (
      familyThread.id === wireThread.id ? wireThread : familyThread
    ));
    const metadata = mapConversationForkMetadata(wireMetadata, thread.id);
    expect(() => validateConversationAggregate(thread, [user, assistant], [], metadata)).not.toThrow();

    if (assistant.derivation.origin !== "fork") throw new Error("fork fixture is invalid");
    const contextCopy = {
      ...assistant,
      sequence: 1,
      derivation: {
        ...assistant.derivation,
        kind: "context_copy" as const,
        sourceTurnId: "turn-newer-fork-source",
        contextPosition: 1,
      },
    };
    const contextThread = mapThread(forkedThread(
      ConversationForkKind.CONVERSATION_FORK_KIND_REGENERATE,
      "turn-newer-fork-source",
    ));
    const finalContextUser = {
      ...user,
      sequence: 2,
      derivation: {
        origin: "fork" as const,
        sourceMessageId: "message-source-user",
        sourceTurnId: "turn-newer-fork-source",
        contextPosition: 2,
        kind: "context_copy" as const,
      },
    };
    const contextMetadata = structuredClone(metadata);
    contextMetadata.lineage = contextThread.lineage;
    contextMetadata.familyThreads = contextMetadata.familyThreads.map((familyThread) => (
      familyThread.id === contextThread.id ? contextThread : familyThread
    ));
    expect(() => validateConversationAggregate(
      contextThread,
      [contextCopy, finalContextUser],
      [],
      contextMetadata,
    )).not.toThrow();

    const wrongKind = {
      ...assistant,
      derivation: {
        ...assistant.derivation,
        kind: "edited_user" as const,
        contextPosition: 1,
      },
    };
    expect(() => validateConversationAggregate(thread, [user, wrongKind], [], metadata))
      .toThrow("fork message prefix is invalid");

    const wrongSource = {
      ...metadata,
      inheritedAssistantOutcomes: metadata.inheritedAssistantOutcomes.map((outcome) => ({
        ...outcome,
        sourceTurnId: "turn-other",
      })),
    };
    expect(() => validateConversationAggregate(thread, [user, assistant], [], wrongSource))
      .toThrow("unlinked assistant message");
  });

  it("rejects a fork family whose parent is absent or at the wrong depth", () => {
    const childWire = forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH);
    if (childWire.lineage?.origin?.$case !== "fork") throw new Error("fork fixture is invalid");
    childWire.lineage.forkDepth = 2;
    childWire.lineage.origin.value.parentThreadId = "thread-missing-parent";
    const child = mapThread(childWire);
    const metadata = mapConversationForkMetadata({
      lineage: childWire.lineage,
      inheritedAssistantOutcomes: [],
      familyThreads: [validThread(), childWire],
    }, child.id);

    expect(() => validateConversationAggregate(child, [], [], metadata))
      .toThrow("fork family has invalid ancestry");
  });
});

describe("conversation numeric response validation", () => {
  it("accepts the exact JavaScript-safe usage bound and rejects one unit over", () => {
    const turn = validConversationTurn();
    const maximum = BigInt(Number.MAX_SAFE_INTEGER);
    turn.usage = {
      inputTokens: maximum,
      outputTokens: maximum,
      costInUsdTicks: maximum,
    };

    expect(mapConversationTurn(turn).usage).toEqual({
      inputTokens: Number.MAX_SAFE_INTEGER,
      outputTokens: Number.MAX_SAFE_INTEGER,
      costInUsdTicks: Number.MAX_SAFE_INTEGER,
    });

    turn.usage.inputTokens = maximum + 1n;
    expect(() => mapConversationTurn(turn)).toThrow("conversation input tokens is outside the safe integer range");
  });

  it("rejects cross-read snapshots with missing or unlinked assistant messages", () => {
    const wireTurn = validConversationTurn();
    const turn = mapConversationTurn(wireTurn);
    const thread = {
      id: "thread-1",
      projectId: "project-1",
      title: "Conversation",
      state: "open" as const,
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
      lineage: { origin: "original" as const, rootThreadId: "thread-1", forkDepth: 0 as const },
    };
    const forkMetadata = originalForkMetadata(thread);
    expect(() => validateConversationAggregate(thread, [turn.userMessage], [turn], forkMetadata))
      .toThrow("references invalid history");
    expect(() => validateConversationAggregate(
      thread,
      [
        turn.userMessage,
        turn.assistantMessage!,
        { ...turn.assistantMessage!, id: "assistant-raced", sequence: 3 },
      ],
      [turn],
      forkMetadata,
    )).toThrow("unlinked assistant message");
  });

  it("rejects duplicate turn identities and more than one active turn", () => {
    const completed = mapConversationTurn(validConversationTurn());
    const thread = {
      id: "thread-1",
      projectId: "project-1",
      title: "Conversation",
      state: "open" as const,
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
      lineage: { origin: "original" as const, rootThreadId: "thread-1", forkDepth: 0 as const },
    };
    const messages = [completed.userMessage, completed.assistantMessage!];
    expect(() => validateConversationAggregate(
      thread,
      messages,
      [completed, structuredClone(completed)],
      originalForkMetadata(thread),
    )).toThrow("duplicate turns");

    const first = {
      ...completed,
      turnId: "turn-active-1",
      state: "provider_started" as const,
      assistantMessage: undefined,
      failure: undefined,
      citations: [],
      usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
      zeroDataRetention: undefined,
      retryEligibility: "source_in_progress" as const,
    };
    const second = {
      ...first,
      turnId: "turn-active-2",
      userMessage: { ...first.userMessage, id: "message-user-active-2", sequence: 3 },
      run: { ...first.run, id: "run-active-2" },
    };
    expect(() => validateConversationAggregate(
      thread,
      [first.userMessage, { ...completed.assistantMessage!, sequence: 2 }, second.userMessage],
      [first, second],
      originalForkMetadata(thread),
    )).toThrow("multiple active turns");
  });

  it("rejects lifecycle revisions that the canonical turn and run cannot reach", () => {
    const invalidTurnRevision = validConversationTurn();
    invalidTurnRevision.revision = 1n;
    expect(() => mapConversationTurn(invalidTurnRevision)).toThrow("conversation turn revision is invalid");

    const invalidRunRevision = validConversationTurn();
    invalidRunRevision.run!.revision = 2n;
    expect(() => mapConversationTurn(invalidRunRevision)).toThrow("conversation turn revision is invalid");
  });

  it("enforces parsed HTTPS citation and aggregate UTF-8 byte bounds", () => {
    const turn = validConversationTurn();
    const prefix = "https://example.test/";
    const url = `${prefix}${"x".repeat(8_000 - prefix.length)}`;
    turn.citations = Array.from({ length: 125 }, () => ({ title: "", url }));
    expect(mapConversationTurn(turn).citations).toHaveLength(125);

    turn.citations[0].url += "x";
    expect(() => mapConversationTurn(turn)).toThrow("citations exceeded the byte limit");

    for (const invalid of [
      "https://",
      "https:// example.test/source",
      "https://#fragment",
      "https://example.test:99999/source",
      "https://user@example.test/source",
    ]) {
      const invalidTurn = validConversationTurn();
      invalidTurn.citations = [{ title: "Source", url: invalid }];
      expect(() => mapConversationTurn(invalidTurn), invalid).toThrow("conversation citation url is invalid");
    }

    const oversizedUtf8Title = validConversationTurn();
    oversizedUtf8Title.citations = [{ title: "é".repeat(251), url: "https://example.test/source" }];
    expect(() => mapConversationTurn(oversizedUtf8Title)).toThrow("conversation citation title is invalid");
  });

  it("maps bounded retry lineage and reasoned eligibility", () => {
    const retry = retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n));
    retry.retryEligibility = ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS;

    expect(mapConversationTurn(retry)).toMatchObject({
      lineage: { origin: "retry", sourceTurnId: "turn-source", retryDepth: 1 },
      retryEligibility: "source_in_progress",
    });

    const depthExhausted = cancelledTurn("turn-depth", "message-depth", 1n);
    depthExhausted.lineage = {
      origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_RETRY,
      sourceTurnId: "turn-depth-parent",
      retryDepth: 64,
    };
    depthExhausted.retryEligibility =
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_DEPTH_EXHAUSTED;
    expect(mapConversationTurn(depthExhausted).retryEligibility).toBe("depth_exhausted");
    depthExhausted.retryEligibility =
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_ALLOWED;
    expect(() => mapConversationTurn(depthExhausted)).toThrow(
      "retry eligibility conflicts with turn state",
    );

    const readOnly = cancelledTurn("turn-read-only", "message-read-only", 1n);
    readOnly.retryEligibility =
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_READ_ONLY;
    expect(mapConversationTurn(readOnly).retryEligibility).toBe("source_read_only");
  });

  it("binds retry responses to the exact requested source turn", () => {
    const retry = retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n));

    expect(mapRetryConversationTurnResponse(retry, "turn-source").lineage).toEqual({
      origin: "retry",
      sourceTurnId: "turn-source",
      retryDepth: 1,
    });
    expect(() => mapRetryConversationTurnResponse(retry, "turn-other")).toThrow(
      "daemon retry response does not match the requested source turn",
    );
  });

  it("rejects missing, self-referential, oversized, or unknown lineage", () => {
    const missing = validConversationTurn();
    missing.lineage = undefined;
    expect(() => mapConversationTurn(missing)).toThrow("conversation turn is incomplete");

    const originalWithSource = validConversationTurn();
    originalWithSource.lineage!.sourceTurnId = "turn-source";
    expect(() => mapConversationTurn(originalWithSource)).toThrow("original conversation lineage is invalid");

    for (const retryDepth of [0, 65]) {
      const retry = retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n));
      retry.lineage!.retryDepth = retryDepth;
      expect(() => mapConversationTurn(retry)).toThrow("retry conversation lineage is invalid");
    }

    const selfRetry = retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n));
    selfRetry.lineage!.sourceTurnId = selfRetry.turnId;
    expect(() => mapConversationTurn(selfRetry)).toThrow("retry conversation lineage is invalid");

    const unknown = validConversationTurn();
    unknown.lineage!.origin = ConversationTurnOrigin.UNRECOGNIZED;
    expect(() => mapConversationTurn(unknown)).toThrow("invalid daemon conversation turn origin");
  });

  it("rejects eligibility that conflicts with state and retry position", () => {
    const completed = validConversationTurn();
    completed.retryEligibility = ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_ALLOWED;
    expect(() => mapConversationTurn(completed)).toThrow("retry eligibility conflicts with turn state");

    const completedAsSuperseded = validConversationTurn();
    completedAsSuperseded.retryEligibility =
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_NOT_NEWEST;
    expect(() => mapConversationTurn(completedAsSuperseded)).toThrow(
      "retry eligibility conflicts with turn state",
    );

    const unknown = validConversationTurn();
    unknown.retryEligibility = ConversationRetryEligibility.UNRECOGNIZED;
    expect(() => mapConversationTurn(unknown)).toThrow("invalid daemon conversation retry eligibility");

    const source = mapConversationTurn(cancelledTurn("turn-source", "message-source", 1n));
    const retry = mapConversationTurn(retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n)));
    const thread = {
      id: "thread-1",
      projectId: "project-1",
      title: "Conversation",
      state: "open" as const,
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
      lineage: { origin: "original" as const, rootThreadId: "thread-1", forkDepth: 0 as const },
    };
    expect(() => validateConversationAggregate(
      thread,
      [source.userMessage, retry.userMessage],
      [{ ...source, retryEligibility: "allowed" }, retry],
      originalForkMetadata(thread),
    )).toThrow("retry eligibility has an invalid position");

    const laterUnlinkedUser = {
      ...source.userMessage,
      id: "message-later",
      sequence: 2,
      content: "A later local user message",
    };
    expect(() => validateConversationAggregate(
      thread,
      [source.userMessage, laterUnlinkedUser],
      [{ ...source, retryEligibility: "not_newest" }],
      originalForkMetadata(thread),
    )).not.toThrow();
  });

  it("validates retry ancestry against canonical source history", () => {
    const source = mapConversationTurn(cancelledTurn("turn-source", "message-source", 1n));
    const retry = mapConversationTurn(retryReservedTurn(cancelledTurn("turn-source", "message-source", 1n)));
    const thread = {
      id: "thread-1",
      projectId: "project-1",
      title: "Conversation",
      state: "open" as const,
      revision: 0,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 1,
      lineage: { origin: "original" as const, rootThreadId: "thread-1", forkDepth: 0 as const },
    };
    expect(() => validateConversationAggregate(
      thread,
      [source.userMessage, retry.userMessage],
      [source, retry],
      originalForkMetadata(thread),
    )).not.toThrow();

    const forged = { ...retry, userMessage: { ...retry.userMessage, content: "forged prompt" } };
    expect(() => validateConversationAggregate(
      thread,
      [source.userMessage, forged.userMessage],
      [source, forged],
      originalForkMetadata(thread),
    )).toThrow("retry lineage references invalid history");

    const insertedMessage = {
      ...source.userMessage,
      id: "message-inserted",
      sequence: 2,
      content: "An unrelated local message",
    };
    const nonAdjacentRetry = {
      ...retry,
      userMessage: { ...retry.userMessage, sequence: 3 },
    };
    expect(() => validateConversationAggregate(
      thread,
      [source.userMessage, insertedMessage, nonAdjacentRetry.userMessage],
      [{ ...source, retryEligibility: "not_newest" }, nonAdjacentRetry],
      originalForkMetadata(thread),
    )).toThrow("retry lineage references invalid history");
  });
});

describe("workspace search response validation", () => {
  it("retains the canonical conversation route for message hits", () => {
    const results = validWorkspaceSearchResults();
    expect(mapWorkspaceSearchResults(results, 0, 8)).toEqual({
      hits: [{
        id: "message-1",
        projectId: "project-1",
        threadId: "thread-1",
        kind: "message",
        title: "Release review",
        snippet: "Evidence and next actions",
        updatedAtUnixMs: 10,
      }],
      hasMore: false,
    });
  });

  it("rejects missing routes, duplicate identities, and forged cursors", () => {
    const missingRoute = validWorkspaceSearchResults();
    missingRoute.hits[0].threadId = "";
    expect(() => mapWorkspaceSearchResults(missingRoute, 0, 8)).toThrow("missing its conversation route");

    const duplicate = validWorkspaceSearchResults();
    duplicate.hits.push({ ...duplicate.hits[0] });
    expect(() => mapWorkspaceSearchResults(duplicate, 0, 8)).toThrow("repeated a result");

    const cursor = validWorkspaceSearchResults();
    cursor.hasMore = true;
    cursor.nextOffset = 8;
    expect(() => mapWorkspaceSearchResults(cursor, 0, 8)).toThrow("cursor is invalid");

    const oversizedUtf8 = validWorkspaceSearchResults();
    oversizedUtf8.hits[0].title = "é".repeat(257);
    expect(() => mapWorkspaceSearchResults(oversizedUtf8, 0, 8)).toThrow("title is invalid");
  });

  it("validates every kind-specific route and canonical project identity", () => {
    const base = validWorkspaceSearchResults().hits[0];
    const results: WorkspaceSearchResults = {
      hits: [
        { ...base, id: "project-1", projectId: "project-1", threadId: "", kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_PROJECT },
        { ...base, id: "thread-1", threadId: "thread-1", kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_THREAD },
        { ...base, id: "message-1", threadId: "thread-1", kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_MESSAGE },
        { ...base, id: "artifact-1", threadId: "", kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_ARTIFACT },
        { ...base, id: "automation-1", threadId: "", kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_AUTOMATION },
      ],
      nextOffset: 0,
      hasMore: false,
    };
    expect(mapWorkspaceSearchResults(results, 0, 8).hits.map(({ kind, threadId }) => ({ kind, threadId }))).toEqual([
      { kind: "project", threadId: undefined },
      { kind: "thread", threadId: "thread-1" },
      { kind: "message", threadId: "thread-1" },
      { kind: "artifact", threadId: undefined },
      { kind: "automation", threadId: undefined },
    ]);

    results.hits[0].projectId = "project-other";
    expect(() => mapWorkspaceSearchResults(results, 0, 8)).toThrow("inconsistent ownership");
  });

  it("advances only from a full page and accepts the exact bounded cursor", () => {
    const fullPage = validWorkspaceSearchResults();
    fullPage.hits = Array.from({ length: 8 }, (_, index) => ({
      ...fullPage.hits[0],
      id: `message-${index}`,
    }));
    fullPage.hasMore = true;
    fullPage.nextOffset = 8;
    expect(mapWorkspaceSearchResults(fullPage, 0, 8)).toMatchObject({ nextOffset: 8, hasMore: true });

    fullPage.hits.pop();
    expect(() => mapWorkspaceSearchResults(fullPage, 0, 8)).toThrow("cursor is invalid");
  });
});

describe("approval decision response validation", () => {
  it("accepts only the exact revisioned Grant or Deny outcome", () => {
    const granted = validApproval();
    expect(mapApprovalDecisionResponse(granted, "approval-1", 2, true)).toMatchObject({
      id: "approval-1",
      revision: 3,
      status: "granted",
      scope: "once",
    });

    const denied = { ...validApproval(), status: ApprovalStatus.APPROVAL_STATUS_DENIED };
    expect(mapApprovalDecisionResponse(denied, "approval-1", 2, false)).toMatchObject({ status: "denied" });
  });

  it("rejects mismatched identity, revision, status, and resource scope", () => {
    expect(() => mapApprovalDecisionResponse(validApproval(), "approval-other", 2, true)).toThrow("inconsistent");
    expect(() => mapApprovalDecisionResponse(validApproval(), "approval-1", 1, true)).toThrow("inconsistent");
    expect(() => mapApprovalDecisionResponse(validApproval(), "approval-1", 2, false)).toThrow("inconsistent");

    const missingResource = {
      ...validApproval(),
      scope: ApprovalScope.APPROVAL_SCOPE_RESOURCE,
    };
    expect(() => mapApprovalDecisionResponse(missingResource, "approval-1", 2, true)).toThrow("resource identity");

    const unexpectedResource = { ...validApproval(), resourceId: "resource-1" };
    expect(() => mapApprovalDecisionResponse(unexpectedResource, "approval-1", 2, true)).toThrow("resource identity");

    const invalidResource = {
      ...validApproval(),
      scope: ApprovalScope.APPROVAL_SCOPE_RESOURCE,
      resourceId: `resource-${"é".repeat(65)}`,
    };
    expect(() => mapApprovalDecisionResponse(invalidResource, "approval-1", 2, true)).toThrow("resource id is invalid");
  });
});

describe("chat model response validation", () => {
  it("accepts a revision-zero product default advertised as a unique alias", () => {
    const catalog = validModelCatalog();
    catalog.preference = { selectedModelId: "grok-4.3", revision: 0n, updatedAtUnixMs: 0n };
    catalog.models[0].id = "grok-4.3-2026";
    catalog.models[0].aliases = ["grok-4.3"];

    expect(mapChatModelCatalog(catalog)).toMatchObject({
      preference: { selectedModelId: "grok-4.3", revision: 0 },
      selectedModelReady: true,
    });
  });

  it("rejects oversized descriptors and identifier fields", () => {
    const tooMany = validModelCatalog();
    tooMany.models = Array.from({ length: 257 }, (_, index) => ({
      ...tooMany.models[0],
      id: `grok-${index}`,
      aliases: [],
    }));
    expect(() => mapChatModelCatalog(tooMany)).toThrow("incomplete or oversized");

    const oversizedId = validModelCatalog();
    oversizedId.models[0].id = "x".repeat(513);
    expect(() => mapChatModelCatalog(oversizedId)).toThrow("chat model id is invalid");
  });

  it("rejects alias collisions and inconsistent readiness", () => {
    const aliasCollision = validModelCatalog();
    aliasCollision.models.push({
      ...aliasCollision.models[0],
      id: "grok-other",
      aliases: [aliasCollision.models[0].id],
    });
    expect(() => mapChatModelCatalog(aliasCollision)).toThrow("ambiguous alias");

    const descriptorMismatch = validModelCatalog();
    descriptorMismatch.models[0].outputModalities = ["image"];
    expect(() => mapChatModelCatalog(descriptorMismatch)).toThrow("descriptor readiness is inconsistent");

    const catalogMismatch = validModelCatalog();
    catalogMismatch.selectedModelReady = false;
    expect(() => mapChatModelCatalog(catalogMismatch)).toThrow("readiness does not match");
  });

  it("accepts a stale revisioned selection now advertised only as an alias for recovery", () => {
    const catalog = validModelCatalog();
    catalog.models[0].id = "grok-canonical";
    catalog.models[0].aliases = ["grok-alias"];
    catalog.preference = { selectedModelId: "grok-alias", revision: 1n, updatedAtUnixMs: 2n };
    catalog.defaultModelId = "grok-canonical";
    catalog.selectedModelReady = false;

    expect(mapChatModelCatalog(catalog)).toMatchObject({
      preference: { selectedModelId: "grok-alias", revision: 1 },
      selectedModelReady: false,
      defaultModelReady: true,
    });
  });
});

describe.sequential("DaemonSupervisor security boundaries", () => {
  const temporaryDirectories: string[] = [];

  afterEach(() => {
    vi.unstubAllEnvs();
    for (const directory of temporaryDirectories.splice(0)) rmSync(directory, { recursive: true, force: true });
  });

  it("uses only the packaged resources binary when development paths are disabled", () => {
    const root = temporaryRoot();
    const options = supervisorOptions(root, false);
    const packaged = executable(path.join(options.resourcesPath, "bin", "grok-daemon"));
    executable(path.join(root, "target", "debug", "grok-daemon"));
    const override = executable(path.join(root, "override", "grok-daemon"));
    vi.stubEnv("GROK_DAEMON_BINARY", override);

    expect(resolveDaemonBinary(options, "linux")).toBe(packaged);
  });

  it("rejects development and environment override binaries in packaged mode", () => {
    const root = temporaryRoot();
    const options = supervisorOptions(root, false);
    executable(path.join(root, "target", "debug", "grok-daemon"));
    vi.stubEnv("GROK_DAEMON_BINARY", executable(path.join(root, "override", "grok-daemon")));

    expect(() => resolveDaemonBinary(options, "linux")).toThrow("grok-daemon binary is not available");
  });

  it("allows an explicit development binary only when the development gate is enabled", () => {
    const root = temporaryRoot();
    const override = executable(path.join(root, "override", "grok-daemon"));
    const options = { ...supervisorOptions(root, true), daemonBinary: override };

    expect(resolveDaemonBinary(options, "linux")).toBe(override);
  });

  it("constructs a credential-free child environment", () => {
    vi.stubEnv("XAI_API_KEY", "secret-xai");
    vi.stubEnv("OPENAI_API_KEY", "secret-other-provider");
    vi.stubEnv("GROK_OAUTH_REFRESH_TOKEN", "secret-oauth");
    vi.stubEnv("GROK_PINENTRY", "/operator/pinentry");
    vi.stubEnv("PATH", "/safe/bin");
    vi.stubEnv("WAYLAND_DISPLAY", "wayland-7");

    const environment = daemonEnvironment("/tmp/daemon.sock", "linux");

    expect(environment.PATH).toBe("/safe/bin");
    expect(environment.WAYLAND_DISPLAY).toBe("wayland-7");
    expect(environment.GROK_DAEMON_STARTUP_NONCE_STDIN).toBe("1");
    expect(environment).not.toHaveProperty("GROK_DAEMON_STARTUP_NONCE_HEX");
    expect(environment.GROK_DAEMON_SOCKET).toBe("/tmp/daemon.sock");
    expect(environment).not.toHaveProperty("XAI_API_KEY");
    expect(environment).not.toHaveProperty("OPENAI_API_KEY");
    expect(environment).not.toHaveProperty("GROK_OAUTH_REFRESH_TOKEN");
    expect(environment).not.toHaveProperty("GROK_PINENTRY");
  });

  it("forwards an absolute pinentry override only for an explicit unix development launch", () => {
    vi.stubEnv("GROK_PINENTRY", "/nix/store/pinentry-qt/bin/pinentry-qt");

    const development = daemonEnvironment(
      "/tmp/development-daemon.sock",
      "linux",
      true,
    );
    const packaged = daemonEnvironment(
      "/tmp/packaged-daemon.sock",
      "linux",
      false,
    );
    const windows = daemonEnvironment(
      String.raw`\\.\pipe\grok-daemon`,
      "win32",
      true,
    );

    expect(development.GROK_PINENTRY).toBe("/nix/store/pinentry-qt/bin/pinentry-qt");
    expect(packaged).not.toHaveProperty("GROK_PINENTRY");
    expect(windows).not.toHaveProperty("GROK_PINENTRY");
  });

  it.each([
    ["relative override", "pinentry-qt"],
    ["empty override", ""],
    ["control-character override", "/tmp/pinentry\nqt"],
    ["oversized override", `/${"p".repeat(4_096)}`],
  ])("strips a malformed development pinentry %s", (_case, override) => {
    vi.stubEnv("GROK_PINENTRY", override);

    const environment = daemonEnvironment(
      "/tmp/development-daemon.sock",
      "linux",
      true,
    );

    expect(environment).not.toHaveProperty("GROK_PINENTRY");
  });

  it("keeps the startup bearer value in an exact one-shot binary payload", () => {
    const nonce = Buffer.alloc(32, 0xa7);
    const payload = daemonBootstrapInput(nonce);

    expect(payload).toEqual(nonce);
    expect(payload).not.toBe(nonce);
    nonce.fill(0);
    expect(payload).toEqual(Buffer.alloc(32, 0xa7));
    expect(() => daemonBootstrapInput(Buffer.alloc(31))).toThrow(
      "daemon startup nonce must contain exactly 32 bytes",
    );
    expect(() => daemonBootstrapInput(Buffer.alloc(33))).toThrow(
      "daemon startup nonce must contain exactly 32 bytes",
    );
  });

  it("does not let automatic event subscriptions bypass the restart budget", async () => {
    const root = temporaryRoot();
    const supervisor = new DaemonSupervisor(supervisorOptions(root, false));
    const internal = supervisor as unknown as {
      unexpectedRestartRequiresManual: boolean;
      unexpectedRestartTimes: number[];
      scheduleUnexpectedRestart(): void;
    };
    const now = Date.now();
    internal.unexpectedRestartTimes = [now, now, now];
    internal.scheduleUnexpectedRestart();

    expect(internal.unexpectedRestartRequiresManual).toBe(true);
    await expect(supervisor.subscribeConversationTurnEvents("turn-1", vi.fn())).rejects.toThrow(
      "automatic restart requires a manual retry",
    );
    expect(supervisor.getStatus()).toMatchObject({
      state: "degraded",
      reason: "The local daemon stopped repeatedly and requires a manual retry.",
    });

    // An ordinary request is the explicit retry path and clears the latch,
    // even though this fixture intentionally has no packaged daemon binary.
    await expect(supervisor.start()).rejects.toThrow("grok-daemon binary is not available");
    expect(internal.unexpectedRestartRequiresManual).toBe(false);
  });

  it("loads fork metadata with inherited assistant copies as one validated conversation", async () => {
    const supervisor = new DaemonSupervisor(supervisorOptions(temporaryRoot(), false));
    vi.spyOn(supervisor, "start").mockResolvedValue();
    const child = forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH);
    if (child.lineage?.origin?.$case !== "fork") throw new Error("fork fixture is invalid");
    child.lineage.origin.value.sourceMessageId = "message-source-assistant";
    const metadata = validForkMetadata();
    metadata.lineage = child.lineage;
    metadata.familyThreads = metadata.familyThreads.map((familyThread) => (
      familyThread.id === child.id ? child : familyThread
    ));
    const copiedUser = validMessage();
    copiedUser.id = "message-child-user";
    copiedUser.threadId = "thread-child";
    copiedUser.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source-user",
          sourceTurnId: "turn-source",
          contextPosition: 1,
          kind: ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_CONTEXT_COPY,
        },
      },
    };
    const copiedAssistant = validMessage();
    copiedAssistant.id = "message-child-assistant";
    copiedAssistant.threadId = "thread-child";
    copiedAssistant.sequence = 2n;
    copiedAssistant.role = MessageRole.MESSAGE_ROLE_ASSISTANT;
    copiedAssistant.content = "Inherited answer";
    copiedAssistant.derivation = {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source-assistant",
          sourceTurnId: "turn-source",
          kind: ConversationMessageDerivationKind
            .CONVERSATION_MESSAGE_DERIVATION_KIND_SOURCE_ASSISTANT_COPY,
        },
      },
    };
    const protocol = {
      getThread: vi.fn().mockResolvedValue(child),
      listMessages: vi.fn().mockResolvedValue({
        messages: [copiedUser, copiedAssistant],
        nextCursor: "",
      }),
      listConversationTurns: vi.fn().mockResolvedValue({ turns: [], nextCursor: "" }),
      getConversationForkMetadata: vi.fn().mockResolvedValue(metadata),
    };
    const internal = supervisor as unknown as { requireProtocol(): typeof protocol };
    internal.requireProtocol = () => protocol;

    await expect(supervisor.getConversation("thread-child")).resolves.toMatchObject({
      thread: { id: "thread-child", lineage: { kind: "branch" } },
      messages: [
        { id: "message-child-user", derivation: { kind: "context_copy" } },
        { id: "message-child-assistant", derivation: { kind: "source_assistant_copy" } },
      ],
      turns: [],
      forkMetadata: {
        inheritedAssistantOutcomes: [{ childAssistantMessageId: "message-child-assistant" }],
      },
    });
    expect(protocol.getConversationForkMetadata).toHaveBeenCalledWith("thread-child");
  });

  it("does not reinterpret an ambiguous fork transport failure", async () => {
    const supervisor = new DaemonSupervisor(supervisorOptions(temporaryRoot(), false));
    vi.spyOn(supervisor, "start").mockResolvedValue();
    const ambiguity = new DaemonTransportError("daemon stream closed after mutation dispatch");
    const protocol = {
      regenerateConversationTurn: vi.fn().mockRejectedValue(ambiguity),
    };
    const internal = supervisor as unknown as { requireProtocol(): typeof protocol };
    internal.requireProtocol = () => protocol;

    await expect(supervisor.regenerateConversationTurn(
      "turn-source",
      7,
      "regenerate-ambiguous-1",
    )).rejects.toBe(ambiguity);
    expect(protocol.regenerateConversationTurn).toHaveBeenCalledWith(
      "turn-source",
      7n,
      "regenerate-ambiguous-1",
    );
  });

  it("maps only the exact acknowledged fork-delivery transition", async () => {
    const supervisor = new DaemonSupervisor(supervisorOptions(temporaryRoot(), false));
    vi.spyOn(supervisor, "start").mockResolvedValue();
    const protocol = {
      acknowledgeConversationForkDelivery: vi.fn().mockResolvedValue({
        childThreadId: "thread-child",
        state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_ACKNOWLEDGED,
        revision: 1n,
      }),
    };
    const internal = supervisor as unknown as { requireProtocol(): typeof protocol };
    internal.requireProtocol = () => protocol;

    await expect(supervisor.acknowledgeConversationForkDelivery(
      "thread-child",
      0,
      "fork-delivery-ack-1",
    )).resolves.toEqual({
      childThreadId: "thread-child",
      state: "acknowledged",
      revision: 1,
    });
    expect(protocol.acknowledgeConversationForkDelivery).toHaveBeenCalledWith(
      "thread-child",
      0n,
      "fork-delivery-ack-1",
    );
    await expect(supervisor.acknowledgeConversationForkDelivery(
      "thread-child",
      1,
      "fork-delivery-ack-invalid",
    )).rejects.toThrow("requires revision zero");
    expect(protocol.acknowledgeConversationForkDelivery).toHaveBeenCalledTimes(1);
  });

  it("keeps stop terminal when a later request races application shutdown", async () => {
    const root = temporaryRoot();
    const supervisor = new DaemonSupervisor(supervisorOptions(root, false));

    await supervisor.stop();

    await expect(supervisor.start()).rejects.toThrow("daemon supervisor is stopping");
    expect(supervisor.getStatus()).toMatchObject({ state: "stopped" });
  });

  function temporaryRoot(): string {
    const directory = mkdtempSync(path.join(os.tmpdir(), "grok-supervisor-test-"));
    temporaryDirectories.push(directory);
    return directory;
  }
});

function supervisorOptions(root: string, allowDevelopmentBinary: boolean): DaemonSupervisorOptions {
  return {
    appPath: path.join(root, "apps", "desktop"),
    resourcesPath: path.join(root, "resources"),
    runtimeDirectory: path.join(root, "runtime"),
    allowDevelopmentBinary,
  };
}

function executable(file: string): string {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, "test executable");
  chmodSync(file, 0o700);
  return file;
}

function originalForkMetadata(thread: DaemonThread): DaemonConversationForkMetadata {
  return {
    lineage: thread.lineage,
    inheritedAssistantOutcomes: [],
    familyThreads: [thread],
  };
}

function forkedThread(
  kind: ConversationForkKind,
  sourceTurnId = "turn-source",
): Thread {
  return {
    id: "thread-child",
    projectId: "project-1",
    title: "Conversation",
    state: ThreadState.THREAD_STATE_OPEN,
    revision: 0n,
    createdAtUnixMs: 2n,
    updatedAtUnixMs: 2n,
    lineage: {
      rootThreadId: "thread-root",
      forkDepth: 1,
      origin: {
        $case: "fork",
        value: {
          parentThreadId: "thread-root",
          sourceTurnId,
          sourceMessageId: "message-source",
          kind,
        },
      },
    },
  };
}

function forkedTurn(kind: "edit_and_branch" | "regenerate"): ConversationTurnResult {
  const turn = validConversationTurn();
  turn.turnId = `turn-${kind}`;
  turn.state = ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED;
  turn.revision = 0n;
  turn.userMessage = {
    ...turn.userMessage!,
    id: `message-${kind}`,
    threadId: "thread-child",
    sequence: 2n,
    derivation: {
      origin: {
        $case: "fork",
        value: {
          sourceMessageId: "message-source-user",
          sourceTurnId: "turn-source",
          contextPosition: 1,
          kind: kind === "edit_and_branch"
            ? ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_EDITED_USER
            : ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_CONTEXT_COPY,
        },
      },
    },
  };
  turn.assistantMessage = undefined;
  turn.run = {
    ...turn.run!,
    id: `run-${kind}`,
    threadId: "thread-child",
    state: RunState.RUN_STATE_QUEUED,
    revision: 0n,
  };
  turn.failure = undefined;
  turn.citations = [];
  turn.usage = { inputTokens: 0n, outputTokens: 0n, costInUsdTicks: 0n };
  turn.zeroDataRetention = undefined;
  turn.lineage = {
    origin: kind === "edit_and_branch"
      ? ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_EDIT_AND_BRANCH
      : ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_REGENERATE,
    sourceTurnId: "turn-source",
    retryDepth: 0,
  };
  turn.retryEligibility =
    ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS;
  return turn;
}

function validForkMetadata(): ConversationForkMetadata {
  const child = forkedThread(ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH);
  return {
    lineage: child.lineage,
    inheritedAssistantOutcomes: [{
      childAssistantMessageId: "message-child-assistant",
      sourceTurnId: "turn-source",
      modelId: "grok-4.3",
      citations: [{ title: "Source", url: "https://example.test/source" }],
      usage: { inputTokens: 7n, outputTokens: 11n, costInUsdTicks: 13n },
      zeroDataRetention: true,
    }],
    familyThreads: [validThread(), child],
  };
}

function validThread(): Thread {
  return {
    id: "thread-root",
    projectId: "project-1",
    title: "Conversation",
    state: ThreadState.THREAD_STATE_OPEN,
    revision: 0n,
    createdAtUnixMs: 1n,
    updatedAtUnixMs: 1n,
    lineage: {
      rootThreadId: "thread-root",
      forkDepth: 0,
      origin: { $case: "original", value: {} },
    },
  };
}

function validMessage(): Message {
  return {
    id: "message-user",
    threadId: "thread-root",
    sequence: 1n,
    role: MessageRole.MESSAGE_ROLE_USER,
    content: "Question",
    state: MessageState.MESSAGE_STATE_ACTIVE,
    revision: 0n,
    createdAtUnixMs: 1n,
    updatedAtUnixMs: 1n,
    derivation: { origin: { $case: "original", value: {} } },
  };
}

function validConversationTurn(): ConversationTurnResult {
  return {
    turnId: "turn-1",
    state: ConversationTurnState.CONVERSATION_TURN_STATE_COMPLETED,
    revision: 2n,
    modelId: "grok-4.3",
    userMessage: {
      id: "message-user",
      threadId: "thread-1",
      sequence: 1n,
      role: MessageRole.MESSAGE_ROLE_USER,
      content: "Question",
      state: MessageState.MESSAGE_STATE_ACTIVE,
      revision: 0n,
      createdAtUnixMs: 1n,
      updatedAtUnixMs: 1n,
      derivation: { origin: { $case: "original", value: {} } },
    },
    assistantMessage: {
      id: "message-assistant",
      threadId: "thread-1",
      sequence: 2n,
      role: MessageRole.MESSAGE_ROLE_ASSISTANT,
      content: "Answer",
      state: MessageState.MESSAGE_STATE_ACTIVE,
      revision: 0n,
      createdAtUnixMs: 2n,
      updatedAtUnixMs: 2n,
      derivation: { origin: { $case: "original", value: {} } },
    },
    run: {
      id: "run-1",
      projectId: "project-1",
      threadId: "thread-1",
      state: RunState.RUN_STATE_COMPLETED,
      revision: 3n,
      createdAtUnixMs: 1n,
      updatedAtUnixMs: 2n,
    },
    failure: undefined,
    citations: [],
    usage: { inputTokens: 1n, outputTokens: 1n, costInUsdTicks: 0n },
    zeroDataRetention: true,
    lineage: {
      origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_ORIGINAL,
      sourceTurnId: "",
      retryDepth: 0,
    },
    retryEligibility:
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_COMPLETED,
  };
}

function cancelledTurn(turnId: string, messageId: string, sequence: bigint): ConversationTurnResult {
  const turn = validConversationTurn();
  turn.turnId = turnId;
  turn.state = ConversationTurnState.CONVERSATION_TURN_STATE_CANCELLED;
  turn.revision = 1n;
  turn.userMessage = {
    ...turn.userMessage!,
    id: messageId,
    sequence,
  };
  turn.assistantMessage = undefined;
  turn.run = {
    ...turn.run!,
    id: `run-${turnId}`,
    state: RunState.RUN_STATE_CANCELLED,
    revision: 1n,
  };
  turn.usage = { inputTokens: 0n, outputTokens: 0n, costInUsdTicks: 0n };
  turn.zeroDataRetention = undefined;
  turn.retryEligibility = ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_NOT_NEWEST;
  return turn;
}

function retryReservedTurn(source: ConversationTurnResult): ConversationTurnResult {
  return {
    turnId: "turn-retry",
    state: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
    revision: 0n,
    modelId: source.modelId,
    userMessage: {
      ...source.userMessage!,
      id: "message-retry",
      sequence: source.userMessage!.sequence + 1n,
      createdAtUnixMs: source.userMessage!.updatedAtUnixMs + 1n,
      updatedAtUnixMs: source.userMessage!.updatedAtUnixMs + 1n,
    },
    assistantMessage: undefined,
    run: {
      ...source.run!,
      id: "run-retry",
      state: RunState.RUN_STATE_QUEUED,
      revision: 0n,
      createdAtUnixMs: source.run!.updatedAtUnixMs + 1n,
      updatedAtUnixMs: source.run!.updatedAtUnixMs + 1n,
    },
    failure: undefined,
    citations: [],
    usage: { inputTokens: 0n, outputTokens: 0n, costInUsdTicks: 0n },
    zeroDataRetention: undefined,
    lineage: {
      origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_RETRY,
      sourceTurnId: source.turnId,
      retryDepth: (source.lineage?.retryDepth ?? 0) + 1,
    },
    retryEligibility:
      ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS,
  };
}

function validModelCatalog(): ChatModelCatalog {
  return {
    models: [{
      id: "grok-4.3",
      aliases: [],
      inputModalities: ["text"],
      outputModalities: ["text"],
      textConversationReady: true,
    }],
    preference: { selectedModelId: "grok-4.3", revision: 0n, updatedAtUnixMs: 0n },
    defaultModelId: "grok-4.3",
    selectedModelReady: true,
    defaultModelReady: true,
  };
}

function validWorkspaceSearchResults(): WorkspaceSearchResults {
  return {
    hits: [{
      id: "message-1",
      projectId: "project-1",
      threadId: "thread-1",
      kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_MESSAGE,
      title: "Release review",
      snippet: "Evidence and next actions",
      updatedAtUnixMs: 10n,
    }],
    nextOffset: 0,
    hasMore: false,
  };
}

function validApproval(): Approval {
  return {
    id: "approval-1",
    runId: "run-1",
    action: {
      action: "publish_release",
      target: "Release notes",
      dataSummary: "Publishes the reviewed release notes.",
      risk: ApprovalRisk.APPROVAL_RISK_HIGH,
    },
    scope: ApprovalScope.APPROVAL_SCOPE_ONCE,
    resourceId: "",
    status: ApprovalStatus.APPROVAL_STATUS_GRANTED,
    revision: 3n,
    createdAtUnixMs: 1n,
    expiresAtUnixMs: 10_000n,
    decidedAtUnixMs: 2n,
  };
}
