# ADR 0002: Grok-only provider model

- Status: accepted
- Date: 2026-07-10

## Context

Subscription ACP and the direct xAI API expose different capabilities,
authentication, quotas, and data contracts. Modeling them as interchangeable
providers would either leak infrastructure details into domain code or discard
important behavior.

## Decision

Expose capability-focused application ports and implement two official
adapters: Grok Build ACP for subscription-backed agent sessions and xAI APIs for
BYOK direct features. Resolve capabilities at runtime and identify their source
in every UI state.

Run the official Grok Build component with an application-owned `GROK_HOME` and
pinned requirements. Do not inherit the standalone CLI's custom models,
compatible provider endpoints, API-key authentication, MCP servers, plugins,
hooks, or other-tool configuration. Native ACP is an authentication and control
surface; prompt/tool execution is admitted only in the qualified guest.
Because Grok honors `disable_bypass_permissions_mode` only from a root-owned
system requirements layer, the app-owned host file is defense in depth. The
host adapter rejects all session execution, while the guest image owns the
authoritative `/etc/grok/requirements.toml`.

Do not support non-xAI providers, arbitrary compatible base URLs, consumer web
scraping, cookie import, or an OAuth client that xAI has not approved. Consumer
surfaces without a supported contract use an explicit web handoff or import.

## Consequences

The application can evolve with official Grok contracts without maintaining a
generic provider marketplace. A feature may legitimately require an xAI API key
when ACP does not expose it; the UI must explain that requirement instead of
silently substituting behavior.
