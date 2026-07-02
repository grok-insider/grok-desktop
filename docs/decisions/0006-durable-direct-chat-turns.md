# ADR 0006: Durable direct Chat turns

- Status: Accepted
- Date: 2026-07-10
- Current note: the durable aggregate and uncertainty rules remain in force;
  ADR 0013 supersedes only this ADR's epoch-6 unary transport decision.

## Context

A direct xAI Chat request is billable and may cross the network boundary before
the desktop observes a response. Persisting only user and assistant messages
cannot distinguish a request that never started from one whose provider result
is uncertain. Retrying either case automatically could duplicate a provider
call, while reporting success before local persistence could lose canonical
history after a crash.

The renderer, Electron main process, provider adapter, and SQL store also have
different responsibilities. Provider DTOs and credentials must not become the
desktop application model, and a UI restart must not erase failure or citation
state.

## Decision

Each direct Chat submission is one daemon-owned `ConversationTurn` aggregate.
Its idempotency key and immutable request fingerprint are reserved atomically
with the canonical user message, run, creation event, and exact provider
context. The selected official xAI model is discovered before reservation and
recorded on the turn.

Before network dispatch, the daemon atomically moves the turn to
`provider_started`, advances the run to `running`, and records a non-idempotent
provider effect plus the provider-request fingerprint. Completion atomically
stores the assistant message, citations, usage, response identifier, observed
ZDR header, terminal effect, run, turn, and audit event. Known provider failures
are committed without an assistant message.

A reserved turn left by a crash becomes `cancelled` during daemon startup. A
provider-started turn left by a crash becomes `interrupted_needs_review` and is
never dispatched automatically again. The same rule applies when transport
failure makes the provider outcome uncertain.

IPC v2 introduced typed execution and bounded chronological history operations.
IPC v6 retained a unary execution request that returned only after a terminal
transaction was durable. ADR 0013 replaces that transport in IPC v7 with a
durable asynchronous start/cancel/text-event contract; it does not change this
ADR's provider-dispatch or uncertainty boundary. The resumable long poll
introduced in IPC v4 still carries durable run audit events only.

IPC v5 replaces the process-static model policy with the daemon-owned,
live-validated preference from ADR 0009. A new turn fingerprints and records the
current canonical selection. A retry after that global preference changes
conflicts before provider dispatch rather than executing the same key under a
different model.

Only the official xAI adapter may implement this direct path. Subscription Chat
and Work remain separate Grok Build ACP lifecycles and cannot reuse BYOK keys or
turn authority implicitly.

## Consequences

- UI success always refers to a committed terminal turn.
- Restarted renderers restore canonical output, citations, usage metadata, and
  explicit failed or uncertain states from the daemon.
- Submitted turn-linked messages are immutable. Edit, regenerate, retry, and
  branch features require new commands with their own fingerprints and lineage;
  they cannot mutate an executed request in place.
- Progressive text is permitted only through the durable schema-12 event
  protocol in ADR 0013, never through renderer-local optimistic provider text.
- At most one nonterminal turn may exist per thread, bounding ambiguous
  concurrency and preserving provider context order.
