// @vitest-environment node
import { describe, expect, it } from "vitest";
import { DaemonResponseError } from "./daemon/DaemonRpcClient.js";
import { ErrorCode } from "./generated/daemon/v1/daemon.js";
import { credentialEnrollmentFailure } from "./credentialEnrollmentFailure.js";

describe("credentialEnrollmentFailure", () => {
  it("maps only cancellation and integrity failures", () => {
    expect(credentialEnrollmentFailure(new DaemonResponseError(
      "cancelled",
      ErrorCode.ERROR_CODE_CANCELLED,
      false,
    ))).toEqual({ kind: "daemon.credentialEnrollmentFailure", reason: "cancelled" });
    expect(credentialEnrollmentFailure(new DaemonResponseError(
      "integrity",
      ErrorCode.ERROR_CODE_INTEGRITY_FAILURE,
      false,
    ))).toEqual({ kind: "daemon.credentialEnrollmentFailure", reason: "integrity_failure" });
    expect(credentialEnrollmentFailure(new DaemonResponseError(
      "unavailable",
      ErrorCode.ERROR_CODE_UNAVAILABLE,
      true,
    ))).toBeUndefined();
    expect(credentialEnrollmentFailure(new Error("transport"))).toBeUndefined();
  });
});
