# ADR 0026: Daemon-owned SuperGrok API Chat rail

- Status: Accepted
- Date: 2026-07-12

## Context

SuperGrok exposes one weekly allowance with separate `GrokBuild`, `Api`, and
`GrokChat` product accounting. Upstream OpenCode commit
`34e58090595d44e3e7cc37498f16753a98627456` implements a fresh xAI OAuth grant
using the public desktop/CLI client, the scopes `openid profile email
offline_access grok-cli:access api:access`, and the normal `api.x.ai` provider
origin. The repository owner explicitly authorized Grok Desktop to implement
this contract.

Importing `~/.grok/auth.json` or calling the CLI chat proxy is not acceptable:
it would import another client's credential authority and misidentify the
calling product.

## Decision

Grok Desktop may offer a distinct `SuperGrokApi` Home Chat rail alongside the
existing `XaiApiKey` rail.

- Enrollment is a fresh authorization-code/PKCE or RFC 8628 device grant using
  fixed xAI authorization endpoints and public client ID
  `b1a00492-073a-47ea-816f-4c329264a828`.
- OAuth requests include `plan=generic`, `referrer=grok-desktop`, and the exact
  approved scopes. Provider requests go only to `https://api.x.ai`.
- The daemon owns pending state, tokens, refresh rotation, vault persistence,
  model discovery, provider calls, durable events, cancellation, and recovery.
- The renderer receives only URLs, a short device user code, expiry, readiness,
  and sanitized reason codes.
- Home Chat is unprivileged: no tools, workspace roots, shell, MCP, browser, or
  Work authority.
- Every turn records an immutable rail and local credential generation. Retry,
  regenerate, and forks never switch rails silently.
- UI copy says “SuperGrok plan · API”. It does not claim `GrokChat` attribution
  until a redacted qualification measures that product counter.
- Grok Build ACP remains the exclusive subscription execution/Work boundary.

## Consequences

Credential and model services become rail-aware. Token rotation is
single-flight and atomically persisted; rejected refresh fails closed into
reauthentication. Interrupted provider requests retain the existing
outcome-unknown semantics and are never replayed automatically.

The Vercel AI SDK is not provider authority. Existing Rust Responses parsing
and durable turn machinery are reused.

## Compatibility and qualification

Public wire changes require a new protocol epoch. Before default enablement,
one authorized real-account test must prove OAuth enrollment, model discovery,
one bounded text response, disconnect/revocation, and redacted product-accounting
attribution. Tokens, JWT claims, account identifiers, exact usage values, and
raw billing responses are never committed.
