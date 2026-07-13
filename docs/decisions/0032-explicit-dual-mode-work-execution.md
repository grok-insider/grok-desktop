# ADR 0032: Explicit dual-mode Work execution

- Status: accepted
- Date: 2026-07-13

## Context

Requiring a qualified signed guest for every Work operation leaves the product
without useful local tools while guest distribution is unfinished. Silently
falling back to the host would nevertheless widen authority without consent.

## Decision

Work runs bind immutably to one concrete backend: `HostDirect` or
`IsolatedGuest`. Limited Mode is capability state, not a run backend. HostDirect
requires a versioned, revocable risk enrollment and remains unavailable to Chat
and scheduled runs. IsolatedGuest is preferred for new runs when qualified.

Host filesystem roots constrain only daemon-native filesystem tools. Every
process invocation requires an exact, one-time approval and runs with the
desktop user's authority; it may access resources beyond those roots. This is
disclosed as full-machine execution, not sandboxing.

The daemon remains the authority for policy, approvals, effect journaling, path
validation, process lifecycle, and recovery. The renderer and ACP client cannot
create grants. Backend failure never enables or changes a grant.

The pinned official Grok Build ACP feasibility gate is satisfied by a
daemon-created, per-run authenticated loopback HTTP MCP bridge, bounded
additional directories, exclusive home-role switching, and a narrow ACP
allow-once envelope for only the daemon MCP namespace. The official process
keeps its strict sandbox; on Unix the daemon places the public platform CA
bundle inside the managed private Grok home because the official HTTP client
constructs a TLS-capable client even for loopback HTTP. Every other native tool
permission remains cancelled. Failure stops the feature rather than weakening
the sandbox.

## Consequences

- HostDirect deliberately accepts user-account compromise risk.
- Guest isolation remains the recommended backend and future qualification
  target.
- Non-idempotent effects persist intent before execution and uncertain outcomes
  require review instead of replay.
- Production enablement is daemon/release-policy owned; normal environment
  variables cannot silently enable HostDirect in packaged builds.
