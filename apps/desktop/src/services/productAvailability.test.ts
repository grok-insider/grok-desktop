import { describe, expect, it } from "vitest";
import {
  GROK_BUILD_AUTH_UNAVAILABLE_REASON,
  grokBuildAgentRuntimeDetail,
} from "./productAvailability";

describe("productAvailability", () => {
  it("explains not_configured without claiming the feature is unimplemented", () => {
    const detail = grokBuildAgentRuntimeDetail({
      healthy: false,
      configured: false,
      reasonCode: "not_configured",
    });
    expect(detail).toMatch(/not configured/i);
    expect(detail).toMatch(/debug-acp-descriptor/);
    expect(detail).not.toMatch(/not exposed by the desktop daemon yet/i);
    expect(GROK_BUILD_AUTH_UNAVAILABLE_REASON).toMatch(/official agent runtime/i);
  });

  it("surfaces healthy runtime readiness", () => {
    expect(
      grokBuildAgentRuntimeDetail({
        healthy: true,
        name: "Grok",
        version: "0.2.97",
      }),
    ).toBe("Grok 0.2.97 is ready for host authentication");
  });

  it("maps known failure codes", () => {
    expect(
      grokBuildAgentRuntimeDetail({ reasonCode: "configuration_invalid" }),
    ).toMatch(/configuration is invalid/i);
    expect(
      grokBuildAgentRuntimeDetail({ reasonCode: "component_verification_failed" }),
    ).toMatch(/verification failed/i);
    expect(
      grokBuildAgentRuntimeDetail({ reasonCode: "agent_configuration_isolation_failed" }),
    ).toMatch(/configuration was changed or could not be secured/i);
  });
});
