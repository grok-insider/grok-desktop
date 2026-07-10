// @vitest-environment node
import { describe, expect, it } from "vitest";
import { DaemonProtocolError, DaemonResponseError, DaemonTransportError } from "./daemon/DaemonRpcClient.js";
import { ErrorCode } from "./generated/daemon/v1/daemon.js";
import { artifactRemovalFailure } from "./artifactRemovalFailure.js";

describe("artifactRemovalFailure", () => {
  it.each([
    [ErrorCode.ERROR_CODE_INVALID_ARGUMENT, "invalid_argument"],
    [ErrorCode.ERROR_CODE_NOT_FOUND, "not_found"],
    [ErrorCode.ERROR_CODE_CONFLICT, "conflict"],
    [ErrorCode.ERROR_CODE_INVALID_STATE, "invalid_state"],
  ] as const)("maps terminal daemon code %s without exposing its message", (code, reason) => {
    const failure = artifactRemovalFailure(new DaemonResponseError(
      "/private/source-canary must not cross IPC",
      code,
      false,
    ));

    expect(failure).toEqual({ kind: "daemon.artifactRemovalRejected", reason });
    expect(JSON.stringify(failure)).not.toContain("source-canary");
  });

  it("leaves every ambiguous failure thrown", () => {
    expect(artifactRemovalFailure(new DaemonResponseError(
      "retry later",
      ErrorCode.ERROR_CODE_UNAVAILABLE,
      true,
    ))).toBeUndefined();
    expect(artifactRemovalFailure(new DaemonResponseError(
      "outer deadline may follow reservation",
      ErrorCode.ERROR_CODE_DEADLINE_EXCEEDED,
      false,
    ))).toBeUndefined();
    for (const code of [
      ErrorCode.ERROR_CODE_INTERNAL,
      ErrorCode.ERROR_CODE_INTEGRITY_FAILURE,
      ErrorCode.ERROR_CODE_CANCELLED,
      ErrorCode.ERROR_CODE_UNAVAILABLE,
      ErrorCode.ERROR_CODE_UNAUTHORIZED,
    ]) {
      expect(artifactRemovalFailure(new DaemonResponseError(
        "outcome remains ambiguous",
        code,
        false,
      ))).toBeUndefined();
    }
    expect(artifactRemovalFailure(new DaemonTransportError("stream closed"))).toBeUndefined();
    expect(artifactRemovalFailure(new DaemonProtocolError("invalid response"))).toBeUndefined();
    expect(artifactRemovalFailure(new Error("unknown"))).toBeUndefined();
  });
});
