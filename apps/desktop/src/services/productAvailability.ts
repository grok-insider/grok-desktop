export const GROK_EXECUTION_UNAVAILABLE_REASON =
  "Grok execution is not connected to the desktop daemon yet. No request was sent.";

export const AUTOMATION_DEFINITION_ONLY_REASON =
  "Automation scheduling is not connected yet. Definitions are saved disabled and never run automatically.";

export const GROK_BUILD_AUTH_UNAVAILABLE_REASON =
  "Grok Build host authentication requires a healthy official agent runtime. Connect through the managed Grok Build component — not an unofficial OAuth client.";

export const SETTINGS_PERSISTENCE_UNAVAILABLE_REASON =
  "This preference is unavailable until daemon-owned settings persistence is connected.";

/** Maps daemon agent-runtime reason codes to Setup-facing copy. */
export function grokBuildAgentRuntimeDetail(runtime?: {
  healthy?: boolean;
  configured?: boolean;
  name?: string;
  version?: string;
  reasonCode?: string;
}): string {
  if (runtime?.healthy) {
    const label = [runtime.name, runtime.version].filter(Boolean).join(" ").trim();
    return label
      ? `${label} is ready for host authentication`
      : "Official Grok Build agent is ready for host authentication";
  }

  switch (runtime?.reasonCode) {
    case "not_configured":
      return "Official Grok Build agent is not configured. Development launches need a debug daemon (`cargo build -p grok-daemon --features debug-acp-descriptor`) with the official `grok` CLI on PATH, or a packaged signed components/grok-acp catalog.";
    case "configuration_invalid":
      return "Grok Build agent configuration is invalid. Check development GROK_ACP_* overrides (executable, version, sha256) or rebuild with the signed managed catalog trust keys.";
    case "component_verification_failed":
      return "Official Grok Build component verification failed. Reinstall the signed component or refresh the development grok binary descriptor.";
    case "agent_process_unavailable":
      return "The official Grok Build agent process could not be started or exited early.";
    case "agent_protocol_unavailable":
      return "The official Grok Build agent did not complete ACP negotiation.";
    case "agent_authentication_failed":
      return "Grok Build host authentication failed inside the official agent.";
    case "agent_runtime_unavailable":
      return "The official Grok Build agent runtime is unavailable.";
    default:
      if (runtime?.reasonCode) {
        return `Grok Build runtime is unavailable (${runtime.reasonCode}).`;
      }
      return "Grok Build runtime is not connected.";
  }
}
