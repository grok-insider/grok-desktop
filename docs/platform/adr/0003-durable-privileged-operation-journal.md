# ADR 0003: Durable privileged-operation journal

- Status: accepted; internal journal and bounded startup recovery implemented;
  privileged dispatch remains unavailable
- Date: 2026-07-10
- Extends: `docs/platform/adr/0001-privileged-guest-contract.md`
  and `docs/platform/adr/0002-authenticated-guest-channel.md`

## Context

Guest control and managed computer use can cross a privileged boundary before
the daemon receives a result. A process, service, guest, or integration crash
can therefore leave the external outcome uncertain. Retrying an integration
start, stop, catalog application, or computer input could duplicate or
contradict a user-visible side effect.

The authenticated guest channel prevents forged or reordered frames within one
guest boot, but it is not durable authority or durable idempotency. Its replay
cache is intentionally memory-only and disappears when the service or guest
restarts. Package identity, a Windows SID, and a stable local operation ID also
do not prove that a new process may replay an earlier request.

Review and diagnostics must remain useful after bounded request and result
payloads are pruned. They cannot depend on reparsing an old opaque payload to
discover which VM, integration instance, application, or observation the user
authorized.

## Decision

The daemon will own a durable privileged-operation journal. The pure domain
aggregate and SQLCipher schema migration 7 establish its closed vocabulary and
storage invariants. Future application use cases must use capability-focused
store and gateway ports; they must not expose a generic method-plus-JSON
privileged interface.

### Closed operations and targets

The journal admits exactly these operation kinds and non-secret typed targets:

| Operation | Audit target | Retry class |
| --- | --- | --- |
| `runner_health` | VM | retry-safe |
| `catalog_apply` | VM | non-idempotent |
| `integration_start` | VM and integration | non-idempotent |
| `integration_stop` | VM, integration, and runtime instance | non-idempotent |
| `computer_observe` | VM and integration | retry-safe |
| `computer_act` | VM, integration, runtime instance, application, and positive observation revision | non-idempotent |

Kind and target shape must agree. Resource IDs are bounded safe ASCII values.
The typed target is sufficient for review and audit but does not replace the
request or payload digests. The canonical request digest binds semantic intent
and rejects conflicting reuse of an idempotency key. The payload digest binds
the exact bounded bytes retained for dispatch. Catalog application remains
conservatively non-idempotent until the complete broker and guest path has a
qualified convergent replay contract.

An authority grant ID, authority expiry, and idempotency key are immutable
operation metadata. Run-owned effect or approval links require the matching run
link. A manually created replacement may link to the reviewed operation through
`supersedes_id`, but it is a new operation with a new key and freshly evaluated
authority and approval.

### Persist before I/O

Preparation commits the operation intent and bounded request payload before any
privileged I/O. Immediately before each dispatch, one transaction must advance
the aggregate to `dispatching` and append an immutable attempt containing:

- a monotonically increasing attempt sequence;
- a fresh, globally unique transport operation ID;
- the digest of the exact wire request;
- the current broker boot ID and guest boot ID;
- the dispatch timestamp and absolute bounded deadline; and
- initial `dispatching` outcome certainty.

The stable journal ID is not a guest transport ID. A retry-safe redispatch keeps
the journal identity and idempotency metadata but creates a new attempt with a
new transport ID, current boot IDs, new deadline, and new wire digest. A service
or guest boot ID change invalidates in-memory replay state; it never authorizes
reuse of an old wire request. The migration bounds deadlines to 30 seconds and
makes attempt identity, epoch, deadline, and wire digest immutable after insert.

Only after the intent and exact attempt are committed may the gateway perform
I/O. A correlated result then atomically records outcome certainty, result
digest or bounded failure code, and the aggregate transition before success is
reported to a caller.

### Lifecycle and crash recovery

The lifecycle is:

```text
prepared -> dispatching -> succeeded
                        -> failed
                        -> retry_pending -> dispatching
                        -> interrupted_needs_review -> reviewed
prepared -> cancelled
```

`retry_pending` is reachable only for `runner_health` and
`computer_observe`. Terminal and reviewed operations never dispatch again.
Checked revisions, attempt counts, monotonic timestamps, authority expiry, and
kind/target validation fail without partially mutating the aggregate.

Startup recovery must inspect the durable operation and its last attempt, not
infer an outcome from process exit alone:

- `prepared` proves that no attempt was persisted. It remains undispatched and
  may be cancelled or dispatched only after authority and policy revalidation.
- An interrupted retry-safe `dispatching` attempt or known retryable failure
  becomes `retry_pending` after its outcome certainty is recorded. A later
  dispatch is a new attempt.
- An interrupted non-idempotent `dispatching` attempt becomes
  `interrupted_needs_review`, even when repeating it appears convenient.
- Known success and known terminal failure become durable outcomes. No service,
  guest, renderer, or scheduled run may independently replay them.

Recovery after broker restart, guest restart, deadline expiry, cancellation, or
transport loss follows the same rules. There is no host-execution compatibility
fallback.

### Human review

An uncertain non-idempotent outcome remains blocked until a human records one
of `confirmed_succeeded`, `confirmed_failed`, or `abandoned`. The review record
is immutable and binds the operation revision, timestamp, bounded actor ID,
bounded rationale, and optional replacement operation. Moving to `reviewed`
does not grant redispatch authority. Any manual retry is a separate operation
with a new idempotency key, exact target, current authority, and approval; it
may reference the reviewed record for lineage.

### Tombstones and pruning

The pair of authority grant ID and idempotency key is unique. Its operation row,
canonical request digest, payload digest, terminal result digest when present,
attempt metadata, and review metadata form an immutable idempotency tombstone.
A replay with the same key must match the original request digest; a mismatch is
a conflict. Tombstones remain after user-visible payload retention expires so a
pruned operation cannot later be executed as if it were new.

Full request payloads are retained while an operation is prepared,
dispatching, retry-pending, or awaiting review. They may be removed after a
terminal or reviewed transition, while their digest and typed target remain.
Bounded terminal result payloads may also be pruned, but a result digest and
explicit pruned marker remain. Attempt identity, epoch, wire, and deadline
fields are immutable; a completed attempt and every review record cannot be
rewritten or deleted. Pruning is a storage operation, never a lifecycle
transition or permission to reuse an idempotency key.

## Current boundary

Implemented now:

- the `PrivilegedOperation` domain aggregate, typed IDs, digests, targets,
  retry policy, lifecycle, review dispositions, validated durable rehydration,
  and exhaustive transition tests;
- forward-only SQLCipher migration 7 with strict operation, payload, attempt,
  review, identity, deadline, retention, and recovery constraints;
- a capability-focused application store and coordinator that validates exact
  payload digests and bounds, provides digest-conflicting idempotency replay,
  and exposes only atomic prepare-plus-payload and
  dispatching-plus-attempt persistence boundaries;
- in-memory and SQLCipher implementations with restart-durable exact replay,
  optimistic revision checks, validated row reconstruction, and atomic unknown
  outcome recovery; and
- a bounded daemon startup pass that moves interrupted retry-safe attempts to
  `retry_pending` and non-idempotent attempts to
  `interrupted_needs_review` without performing I/O. If more rows exist than
  the bounded pass can process, startup fails closed; a subsequent startup can
  continue the durable recovery pass.

Not implemented now:

- a typed guest-control gateway;
- the per-install proof-of-possession and proof-bearing daemon session;
- approval/review use cases or any renderer/public IPC surface for this
  journal; and
- signed Windows qualification of the complete path.

Therefore the internal store and recovery coordinator create no executable
Work path. There is no guest gateway, the Windows service continues to deny
`guest_control`, Work remains unavailable in Limited Mode, capability
resolution keeps `guestGrant=false`, and package/process qualification alone
grants no tool authority.

## Consequences

- Durable recovery decisions have a closed, inspectable model independent of
  renderer and transport DTOs.
- Every retry creates auditable transport identity and epoch metadata rather
  than reusing a stale frame.
- Payload retention can be bounded without erasing deduplication or review
  evidence.
- Adding a privileged operation or changing retry classification requires a
  domain change, forward-only schema migration, threat-model review, and native
  qualification.
- The schema and aggregate deliberately do not make Work partially available;
  enablement waits for the proof-bearing session, application ports, daemon
  recovery orchestration, and release evidence.
