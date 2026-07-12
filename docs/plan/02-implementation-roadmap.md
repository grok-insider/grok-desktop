# Implementation roadmap

Phases are dependency ordered. A later capability must not be advertised while
an earlier safety gate is open.

## Phase 0 — Preserve and classify

- Keep existing commits; add forward fixes only.
- Split dirty work by subsystem and remove credential import, recursive
  deletion, and host-dependent normal tests.
- Correct the epoch/status documentation after source and generated protocol
  output agree.
- Acceptance: clean ownership of every dirty file and no mixed catch-all commit.

## Phase 1 — Fail-closed corrective epoch

- Publish a new protocol epoch through the documented generator.
- Return scheduling to definition-only: stop automatic thread/run creation,
  revoke execution-ready facts, preserve definitions and journals.
- Withdraw Wisp install/update/remove until trust binding and durable lifecycle
  state are repaired; preserve existing records as unqualified.
- Acceptance: old epochs rejected, retained data readable, UI reports explicit
  unavailability, and no scheduler/Wisp side effect can start.

## Phase 2 — Safe ACP and Linux foundations

- Keep official ACP authentication inside the official client. Do not import
  or project its credential files.
- Make development component discovery debug-only and bind version/digest to
  one canonical opened executable identity.
- Verify signed catalog, publisher, version, architecture, and executable
  digest before and after Linux staging.
- Harden Linux broker peer identity, private socket ownership/mode, framing,
  deadlines, concurrency, negotiation, and stale-socket recovery.
- Acceptance: deterministic fake-component tests pass; packaged builds reject
  debug descriptors; Work remains unavailable without qualified isolation.

## Phase 3 — Atomic scheduled execution

- Add one store operation that atomically validates the occurrence and fence,
  claims it, creates its dedicated thread/run, records the immutable prompt,
  and binds exact idempotency.
- Implement memory/SQLCipher parity in one SQL transaction.
- Recover unbound pending work without I/O; resume only atomically bound queued
  runs; mark interrupted non-idempotent effects for review.
- Dispatch only through a qualified isolated guest port with no inherited Chat
  or workspace capabilities.
- Give the daemon scheduler a cancellation token and joined shutdown.
- Acceptance: crash injection at every boundary cannot create duplicate
  threads/runs; Limited Mode never dispatches.

## Phase 4 — Durable signed Wisp lifecycle

- Derive canonical signing bytes from the parsed manifest and verify only those
  bytes. Fixture signing bytes are comparison evidence, never authority.
- Bound manifest/file sizes and counts; reject links/reparse points; hash and
  revalidate the opened file identity at use.
- Add forward-only SQLCipher lifecycle state and recovery journals with exact
  idempotency, optimistic revision, and fail-closed overflow.
- Stage and publish atomically in a private daemon namespace; integrations stay
  out of process and cannot inject renderer code.
- Acceptance: tamper, link, race, crash, restart, rollback, and memory/SQL parity
  tests pass before IPC mutations are re-enabled.

## Phase 5 — Product completion and regression

- Regress completed tray assets, close-to-tray preference, explicit Quit, and
  normal close when the preference is disabled.
- Finish Setup through official ACP authentication and existing daemon-owned
  xAI-key enrollment.
- Keep Imagine, Voice, Work, managed browser, and other unfinished surfaces
  unavailable until each has a daemon-owned policy, persistence, and recovery
  path.
- Keep Rust as Chat provider/durable-state authority. Any AI SDK experiment is
  renderer-only, ADR-gated, and must preserve the daemon event protocol.

## Phase 6 — SuperGrok API Chat

- Use the owner-authorized, source-pinned public xAI OAuth flow recorded in ADR
  0026 and implement `SuperGrokApi` separately from `XaiApiKey`.
- Keep enrollment/tokens in the daemon vault and persist immutable per-turn
  rail lineage. Never silently switch rails.
- Send provider traffic only to `api.x.ai`; never import CLI tokens or use the
  CLI chat proxy.
- Acceptance: a redacted real-account qualification proves endpoint, scope,
  model access, revocation, and which usage product changes. Until then UI must
  not claim that Home Chat consumes `Api` or `GrokChat`.
