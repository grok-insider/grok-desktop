# ADR 0004: Daemon-owned credentials and capability truth

> Amended by [ADR 0032](0032-explicit-dual-mode-work-execution.md): daemon-owned
> capability truth may include an independently enrolled Host Tools backend;
> guest failure itself never enables it.

- Status: Accepted
- Date: 2026-07-10

## Context

Grok Desktop supports two official access paths: subscription access through
the official Grok Build ACP client and user-owned xAI API keys for documented
xAI APIs. Renderer state, environment flags, and client assertions are not
trusted evidence that either path is ready.

Credentials must not be persisted in renderer storage, application settings,
logs, artifacts, crash reports, tool environments, or the guest. Retried
credential mutations also need the same conflict semantics as other daemon
commands without storing the credential in an idempotency journal.

## Decision

The Rust daemon is the sole owner of credential state and capability facts.

- xAI API keys are accepted only through a bounded local IPC mutation, validated
  against the fixed official xAI origin, and stored in the operating-system
  credential vault. IPC returns configuration state only; no operation reads a
  key back to Electron. The secret-bearing mutation is restricted to the native
  enrollment adapter defined by ADR 0005 and is not exposed to the general
  renderer bridge.
- Credential command fingerprints are derived from secret bytes, while the
  durable command journal stores only the fingerprint and canonical non-secret
  outcome. Reusing a key for different input conflicts.
- Subscription authentication remains owned by the verified official Grok
  Build ACP component. Grok Desktop does not import cookies or implement an
  unofficial OAuth client.
- Capability resolution ignores deprecated caller-supplied readiness facts.
  Vault presence and trusted runtime adapters supply facts inside the daemon.
  The credential marker means official discovery found at least one text-capable
  model; current IPC v13 separately resolves the persisted Chat selection against
  a live catalog, as recorded by ADR 0009.
  Provider network availability is optimistic and every provider request still
  enforces its own timeout, authentication, and transport result.
- Isolation, managed-browser, and computer-use capabilities remain unavailable
  until their daemon-owned service probes are implemented and qualified. No
  renderer flag or development fallback may enable them in a release build.

## Consequences

The desktop can safely display whether BYOK is configured and can replace or
delete it without ever receiving stored secret material. A locked vault or
failed official validation fails closed with a sanitized error.

Capability availability may change after a credential mutation or service
health transition, so Electron must refresh the daemon snapshot rather than
patching capability booleans locally. Work remains in Limited Mode until the
authenticated Windows broker and guest channel are connected.
