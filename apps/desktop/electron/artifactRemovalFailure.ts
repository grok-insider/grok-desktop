import type { BridgeResponse } from "../src/contracts/bridge.js";
import { DaemonResponseError } from "./daemon/DaemonRpcClient.js";
import { ErrorCode } from "./generated/daemon/v1/daemon.js";

type ArtifactRemovalRejection = Extract<
  BridgeResponse,
  { kind: "daemon.artifactRemovalRejected" }
>;

/**
 * Converts only a terminal daemon rejection into a closed renderer response.
 * Retryable daemon replies and every transport/protocol failure remain thrown
 * because their post-reservation outcome can be ambiguous.
 */
export function artifactRemovalFailure(error: unknown): ArtifactRemovalRejection | undefined {
  if (!(error instanceof DaemonResponseError) || error.retryable) return undefined;
  const reason = artifactRemovalRejectionReason(error.code);
  if (!reason) return undefined;
  return {
    kind: "daemon.artifactRemovalRejected",
    reason,
  };
}

function artifactRemovalRejectionReason(
  code: number,
): ArtifactRemovalRejection["reason"] | undefined {
  switch (code) {
    case ErrorCode.ERROR_CODE_INVALID_ARGUMENT:
      return "invalid_argument";
    case ErrorCode.ERROR_CODE_NOT_FOUND:
      return "not_found";
    case ErrorCode.ERROR_CODE_CONFLICT:
      return "conflict";
    case ErrorCode.ERROR_CODE_INVALID_STATE:
      return "invalid_state";
    default:
      return undefined;
  }
}
