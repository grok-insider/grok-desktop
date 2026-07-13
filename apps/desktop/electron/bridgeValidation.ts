import type {
  BridgeRequest,
  DaemonAutomationInput,
} from "../src/contracts/bridge.js";
import { parseExternalHttpsUrl } from "./externalUrlPolicy.js";

const missedRunPolicies = new Set<DaemonAutomationInput["missedRunPolicy"]>(["run_once", "skip"]);
const overlapPolicies = new Set<DaemonAutomationInput["overlapPolicy"]>(["queue_one", "skip"]);
const MAX_ARTIFACT_CONTENT_VERSION = 1_000_000;

/** Validates the untrusted renderer payload before privileged dispatch. */
export function parseBridgeRequest(value: unknown): BridgeRequest {
  const input = object(value, "bridge request");
  const kind = string(input.kind, "request kind", 64);
  if (kind === "daemon.getDesktopPreferences") {
    exactKeys(input, ["kind"], "desktop preference request");
    return { kind };
  }
  if (
    kind === "daemon.selectHostWorkFolder"
    || kind === "daemon.getHostExecutionPolicy"
  ) {
    exactKeys(input, ["kind"], `${kind} request`);
    return { kind };
  }
  if (kind === "daemon.enrollHostExecution") {
    exactKeys(input, [
      "kind", "expectedRevision", "acknowledgmentVersion", "typedAcknowledgment",
      "filesystemRead", "filesystemWrite", "processExecute", "pathRoots",
      "broadScopeAcknowledged", "idempotencyKey",
    ], "Host Tools enrollment request");
    if (!Array.isArray(input.pathRoots) || input.pathRoots.length < 1 || input.pathRoots.length > 8) {
      throw new TypeError("Host Tools roots must contain between 1 and 8 paths");
    }
    const pathRoots = input.pathRoots.map((root) => string(root, "Host Tools root", 4096));
    if (new Set(pathRoots).size !== pathRoots.length) {
      throw new TypeError("Host Tools roots must be unique");
    }
    return {
      kind,
      expectedRevision: unsignedInteger(input.expectedRevision, "Host Tools policy revision"),
      acknowledgmentVersion: unsignedInteger(input.acknowledgmentVersion, "Host Tools acknowledgment version"),
      typedAcknowledgment: string(input.typedAcknowledgment, "Host Tools acknowledgment", 128),
      filesystemRead: booleanValue(input.filesystemRead, "Host Tools filesystem read"),
      filesystemWrite: booleanValue(input.filesystemWrite, "Host Tools filesystem write"),
      processExecute: booleanValue(input.processExecute, "Host Tools process execution"),
      pathRoots,
      broadScopeAcknowledged: booleanValue(input.broadScopeAcknowledged, "Host Tools broad scope acknowledgment"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.revokeHostExecution") {
    exactKeys(input, ["kind", "expectedRevision", "idempotencyKey"], "Host Tools revocation request");
    return {
      kind,
      expectedRevision: unsignedInteger(input.expectedRevision, "Host Tools policy revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.prepareHostWorkRuntime" || kind === "daemon.deactivateHostWorkRuntime") {
    exactKeys(input, ["kind", "idempotencyKey"], "Host Tools runtime request");
    return { kind, idempotencyKey: identifier(input.idempotencyKey, "idempotency key") };
  }
  if (kind === "daemon.startHostWork") {
    exactKeys(input, ["kind", "projectId", "threadId", "prompt", "idempotencyKey"], "Host Work start request");
    return {
      kind,
      projectId: identifier(input.projectId, "project id"),
      threadId: identifier(input.threadId, "thread id"),
      prompt: string(input.prompt, "Host Work prompt", 1024 * 1024),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.cancelHostWork") {
    exactKeys(input, ["kind", "runId", "idempotencyKey"], "Host Work cancel request");
    return {
      kind,
      runId: identifier(input.runId, "run id"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.listHostWorkRuns") {
    exactKeys(input, ["kind", "limit"], "Host Work list request");
    const limit = unsignedInteger(input.limit, "Host Work list limit");
    if (limit < 1 || limit > 100) throw new TypeError("Host Work list limit is outside the supported bounds");
    return { kind, limit };
  }
  if (kind === "daemon.getChatModelCatalog") {
    exactKeys(input, ["kind"], "chat model catalog request");
    return { kind };
  }
  if (kind === "desktop.openExternalUrl") {
    exactKeys(input, ["kind", "url"], "external URL request");
    return { kind, url: parseExternalHttpsUrl(input.url) };
  }
  if (
    kind === "runtime.info"
    || kind === "desktop.getUpdateState"
    || kind === "desktop.checkForUpdates"
    || kind === "desktop.installUpdate"
    || kind === "window.minimize"
    || kind === "window.maximize"
    || kind === "window.close"
    || kind === "daemon.bootstrap"
    || kind === "daemon.getAccountState"
    || kind === "daemon.getGrokBuildAuthStatus"
  ) {
    exactKeys(input, ["kind"], `${kind} request`);
    return { kind };
  }
  if (kind === "daemon.startGrokBuildAuth") {
    exactKeys(input, ["kind", "idempotencyKey"], "grok build auth request");
    return {
      kind,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.getManagedIntegration") {
    exactKeys(input, ["kind", "integrationId"], "managed integration request");
    return {
      kind,
      integrationId: identifier(input.integrationId, "integration id"),
    };
  }
  if (kind === "daemon.changeManagedIntegration") {
    exactKeys(
      input,
      ["kind", "integrationId", "action", "expectedRevision", "idempotencyKey"],
      "managed integration change request",
    );
    const action = string(input.action, "managed integration action", 32);
    if (action !== "install" && action !== "update" && action !== "rollback") {
      throw new TypeError("invalid managed integration action");
    }
    return {
      kind,
      integrationId: identifier(input.integrationId, "integration id"),
      action,
      expectedRevision: unsignedInteger(input.expectedRevision, "integration revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.updateDesktopPreferences") {
    exactKeys(input, ["kind", "expectedRevision", "keepRunningInNotificationArea", "updateChannel", "idempotencyKey"], "desktop preference request");
    const updateChannel = string(input.updateChannel, "desktop update channel", 16);
    if (updateChannel !== "stable" && updateChannel !== "beta") {
      throw new TypeError("invalid desktop update channel");
    }
    return {
      kind,
      expectedRevision: unsignedInteger(input.expectedRevision, "desktop preference revision"),
      keepRunningInNotificationArea: booleanValue(input.keepRunningInNotificationArea, "desktop close behavior"),
      updateChannel,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.getUsageSummary") {
    exactKeys(input, ["kind", "scopeKind", "scopeId", "window"], "usage summary request");
    const scopeKind = string(input.scopeKind, "usage scope kind", 32);
    if (scopeKind !== "workspace" && scopeKind !== "project" && scopeKind !== "thread") {
      throw new TypeError("usage scope kind is invalid");
    }
    const window = string(input.window, "usage window", 32);
    if (window !== "last_7_days" && window !== "last_30_days" && window !== "all_time") {
      throw new TypeError("usage window is invalid");
    }
    const scopeId = input.scopeId === undefined ? undefined : identifier(input.scopeId, "usage scope id");
    if (scopeKind === "workspace") {
      if (scopeId !== undefined) throw new TypeError("workspace usage summary must not include a scope id");
      return { kind, scopeKind, window };
    }
    if (scopeId === undefined) throw new TypeError("usage scope id is required");
    return { kind, scopeKind, scopeId, window };
  }
  if (kind === "daemon.selectChatModel") {
    exactKeys(input, ["kind", "expectedRevision", "modelId", "idempotencyKey"], "chat model selection request");
    return {
      kind,
      expectedRevision: unsignedInteger(input.expectedRevision, "chat model preference revision"),
      modelId: modelIdentifier(input.modelId),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.enrollXaiApiKey") {
    exactKeys(input, ["kind", "idempotencyKey"], "credential enrollment request");
    return {
      kind,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (
    kind === "daemon.beginSuperGrokDeviceEnrollment" ||
    kind === "daemon.cancelSuperGrokEnrollment" ||
    kind === "daemon.disconnectSuperGrok"
  ) {
    exactKeys(input, ["kind", "idempotencyKey"], "SuperGrok enrollment request");
    return { kind, idempotencyKey: identifier(input.idempotencyKey, "idempotency key") };
  }
  if (kind === "daemon.getSuperGrokEnrollmentStatus") {
    exactKeys(input, ["kind"], "SuperGrok enrollment status request");
    return { kind };
  }
  if (kind === "daemon.deleteXaiApiKey") {
    exactKeys(input, ["kind", "idempotencyKey"], "credential deletion request");
    return {
      kind,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.createProject") {
    exactKeys(input, ["kind", "name", "description", "idempotencyKey"], "project creation request");
    return {
      kind,
      name: string(input.name, "project name", 200),
      description: string(input.description, "project description", 4_096, true),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.createThread") {
    exactKeys(input, ["kind", "projectId", "title", "idempotencyKey"], "thread creation request");
    return {
      kind,
      projectId: identifier(input.projectId, "project id"),
      title: string(input.title, "thread title", 200),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.importArtifact") {
    exactKeys(input, ["kind", "projectId", "idempotencyKey"], "artifact import request");
    return {
      kind,
      projectId: identifier(input.projectId, "project id"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.openArtifact") {
    exactKeys(
      input,
      ["kind", "artifactId", "contentVersion", "idempotencyKey"],
      "artifact open request",
    );
    const contentVersion = unsignedInteger(input.contentVersion, "artifact content version");
    if (contentVersion < 1 || contentVersion > MAX_ARTIFACT_CONTENT_VERSION) {
      throw new TypeError("artifact content version is outside the supported bounds");
    }
    return {
      kind,
      artifactId: identifier(input.artifactId, "artifact id"),
      contentVersion,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.removeArtifact") {
    exactKeys(
      input,
      ["kind", "artifactId", "expectedRevision", "expectedContentVersion", "idempotencyKey"],
      "artifact removal request",
    );
    const expectedContentVersion = unsignedInteger(
      input.expectedContentVersion,
      "artifact removal content version",
    );
    if (expectedContentVersion < 1 || expectedContentVersion > MAX_ARTIFACT_CONTENT_VERSION) {
      throw new TypeError("artifact removal content version is outside the supported bounds");
    }
    const expectedRevision = unsignedInteger(
      input.expectedRevision,
      "artifact removal revision",
    );
    if (expectedRevision !== expectedContentVersion) {
      throw new TypeError("artifact removal revision does not match its content version");
    }
    return {
      kind,
      artifactId: identifier(input.artifactId, "artifact id"),
      expectedRevision,
      expectedContentVersion,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.getConversation") {
    exactKeys(input, ["kind", "threadId"], "conversation request");
    return { kind, threadId: identifier(input.threadId, "thread id") };
  }
  if (kind === "daemon.searchWorkspace") {
    exactKeys(input, ["kind", "projectId", "query", "offset", "limit"], "workspace search request");
    const offset = unsignedInteger(input.offset, "workspace search offset");
    const limit = unsignedInteger(input.limit, "workspace search limit");
    if (offset > 10_000) throw new TypeError("workspace search offset is outside the supported bounds");
    if (limit < 1 || limit > 100) throw new TypeError("workspace search limit is outside the supported bounds");
    return {
      kind,
      projectId: input.projectId === undefined ? undefined : identifier(input.projectId, "project id"),
      query: printableString(input.query, "workspace search query", 256),
      offset,
      limit,
    };
  }
  if (kind === "daemon.startConversationTurn") {
    exactKeys(input, ["kind", "threadId", "content", "modelId", "searchEnabled", "idempotencyKey"], "conversation start request");
    return {
      kind,
      threadId: identifier(input.threadId, "thread id"),
      content: string(input.content, "message content", 1024 * 1024),
      ...(input.modelId === undefined ? {} : { modelId: modelIdentifier(input.modelId) }),
      searchEnabled: booleanValue(input.searchEnabled, "search enabled"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.cancelConversationTurn") {
    exactKeys(input, ["kind", "turnId", "expectedRevision", "idempotencyKey"], "conversation cancellation request");
    return {
      kind,
      turnId: identifier(input.turnId, "turn id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected turn revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.retryConversationTurn") {
    exactKeys(
      input,
      ["kind", "sourceTurnId", "expectedRevision", "idempotencyKey"],
      "conversation retry request",
    );
    return {
      kind,
      sourceTurnId: identifier(input.sourceTurnId, "source turn id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected source turn revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.branchConversationThread") {
    exactKeys(
      input,
      ["kind", "sourceTurnId", "expectedRevision", "idempotencyKey"],
      "conversation branch request",
    );
    return {
      kind,
      sourceTurnId: identifier(input.sourceTurnId, "source turn id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected source turn revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.editAndBranchConversationTurn") {
    exactKeys(
      input,
      ["kind", "sourceTurnId", "expectedRevision", "content", "idempotencyKey"],
      "conversation edit-and-branch request",
    );
    return {
      kind,
      sourceTurnId: identifier(input.sourceTurnId, "source turn id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected source turn revision"),
      content: string(input.content, "message content", 1024 * 1024),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.regenerateConversationTurn") {
    exactKeys(
      input,
      ["kind", "sourceTurnId", "expectedRevision", "idempotencyKey"],
      "conversation regeneration request",
    );
    return {
      kind,
      sourceTurnId: identifier(input.sourceTurnId, "source turn id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected source turn revision"),
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.getConversationForkMetadata") {
    exactKeys(input, ["kind", "threadId"], "conversation fork metadata request");
    return { kind, threadId: identifier(input.threadId, "thread id") };
  }
  if (kind === "daemon.acknowledgeConversationForkDelivery") {
    exactKeys(
      input,
      ["kind", "childThreadId", "expectedRevision", "idempotencyKey"],
      "conversation fork delivery acknowledgement request",
    );
    const expectedRevision = unsignedInteger(
      input.expectedRevision,
      "expected fork delivery revision",
    );
    if (expectedRevision !== 0) {
      throw new TypeError("conversation fork delivery acknowledgement requires pending revision zero");
    }
    return {
      kind,
      childThreadId: identifier(input.childThreadId, "fork child thread id"),
      expectedRevision,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  if (kind === "daemon.createAutomation" || kind === "daemon.updateAutomation") {
    const automationKeys = [
      "kind", "projectId", "title", "prompt", "schedule", "timezone", "missedRunPolicy",
      "overlapPolicy", "idempotencyKey", "scheduleActive",
    ];
    if (kind === "daemon.updateAutomation") automationKeys.push("automationId", "expectedRevision");
    exactKeys(input, automationKeys, "automation request");
    const automation = parseAutomationInput(input);
    const common = {
      ...automation,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
    if (kind === "daemon.createAutomation") return { kind, ...common };
    return {
      kind,
      ...common,
      automationId: identifier(input.automationId, "automation id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected automation revision"),
    };
  }
  if (kind === "daemon.decideApproval") {
    exactKeys(input, ["kind", "approvalId", "expectedRevision", "approved", "idempotencyKey"], "approval decision request");
    if (typeof input.approved !== "boolean") throw new TypeError("approval decision must be boolean");
    return {
      kind,
      approvalId: identifier(input.approvalId, "approval id"),
      expectedRevision: unsignedInteger(input.expectedRevision, "expected approval revision"),
      approved: input.approved,
      idempotencyKey: identifier(input.idempotencyKey, "idempotency key"),
    };
  }
  throw new TypeError("unsupported bridge request");
}

function parseAutomationInput(input: Record<string, unknown>): DaemonAutomationInput {
  const missedRunPolicy = string(input.missedRunPolicy, "missed-run policy", 32) as DaemonAutomationInput["missedRunPolicy"];
  const overlapPolicy = string(input.overlapPolicy, "overlap policy", 32) as DaemonAutomationInput["overlapPolicy"];
  if (!missedRunPolicies.has(missedRunPolicy)) throw new TypeError("invalid missed-run policy");
  if (!overlapPolicies.has(overlapPolicy)) throw new TypeError("invalid overlap policy");
  if (input.scheduleActive !== undefined && typeof input.scheduleActive !== "boolean") {
    throw new TypeError("scheduleActive must be a boolean");
  }
  return {
    projectId: identifier(input.projectId, "project id"),
    title: string(input.title, "automation title", 200),
    prompt: string(input.prompt, "automation prompt", 64 * 1024),
    schedule: string(input.schedule, "automation schedule", 256),
    timezone: string(input.timezone, "automation timezone", 128),
    missedRunPolicy,
    overlapPolicy,
    ...(input.scheduleActive === true ? { scheduleActive: true } : {}),
  };
}

function object(value: unknown, field: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError(`${field} must be an object`);
  return value as Record<string, unknown>;
}

function exactKeys(input: Record<string, unknown>, allowed: readonly string[], field: string): void {
  const allowedKeys = new Set(allowed);
  if (Object.keys(input).some((key) => !allowedKeys.has(key))) {
    throw new TypeError(`${field} contains unsupported fields`);
  }
}

function identifier(value: unknown, field: string): string {
  const result = string(value, field, 128);
  if (Array.from(result).some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
  })) {
    throw new TypeError(`${field} is invalid`);
  }
  return result;
}

function modelIdentifier(value: unknown): string {
  const result = string(value, "chat model identifier", 512);
  if (result.trim() !== result || Array.from(result).some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
  })) {
    throw new TypeError("chat model identifier is invalid");
  }
  return result;
}

function printableString(value: unknown, field: string, maximum: number): string {
  const result = string(value, field, maximum);
  if (result.trim().length === 0 || Array.from(result).some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
  })) {
    throw new TypeError(`${field} is invalid`);
  }
  return result;
}

function string(value: unknown, field: string, maximum: number, allowEmpty = false): string {
  if (
    typeof value !== "string"
    || (!allowEmpty && value.length === 0)
    || (typeof value === "string" && Buffer.byteLength(value, "utf8") > maximum)
  ) {
    throw new TypeError(`${field} is invalid`);
  }
  return value;
}

function unsignedInteger(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${field} must be a non-negative safe integer`);
  }
  return value;
}

function booleanValue(value: unknown, field: string): boolean {
  if (typeof value !== "boolean") throw new TypeError(`${field} must be boolean`);
  return value;
}
