import type { BridgeResponse } from "../src/contracts/bridge.js";
import { DaemonResponseError } from "./daemon/DaemonRpcClient.js";
import { ErrorCode } from "./generated/daemon/v1/daemon.js";

type EnrollmentFailure = Extract<BridgeResponse, { kind: "daemon.credentialEnrollmentFailure" }>;

/** Maps only expected native-enrollment outcomes onto the narrow renderer contract. */
export function credentialEnrollmentFailure(error: unknown): EnrollmentFailure | undefined {
  if (!(error instanceof DaemonResponseError)) return undefined;
  if (error.code === ErrorCode.ERROR_CODE_CANCELLED) {
    return { kind: "daemon.credentialEnrollmentFailure", reason: "cancelled" };
  }
  if (error.code === ErrorCode.ERROR_CODE_INTEGRITY_FAILURE) {
    return { kind: "daemon.credentialEnrollmentFailure", reason: "integrity_failure" };
  }
  return undefined;
}
