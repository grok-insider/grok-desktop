# ADR 0015: Safe direct-Chat Retry lineage

- Status: Accepted
- Date: 2026-07-11

## Context

A cancelled direct-Chat turn or a known retryable provider failure may be safe
to attempt again, but the existing immutable-turn contract deliberately gives
the renderer no authority to reconstruct a provider request. Accepting prompt,
model, context, continuation, account, or credential fields from Retry would
let presentation state change what is billed or redisclose context that the
daemon did not select.

The source also cannot be treated as safe only because it exists locally. A
completed request has already produced a billable outcome, a
`provider_started` or `interrupted_needs_review` request may have produced one,
and a credential replacement can move later requests to a different local
enrollment generation. The official xAI API contract used here does not make a
stable account identity part of the turn contract, and deriving an identity
from key bytes would retain a credential fingerprint outside the vault.

Finally, Retry must not silently become editing or branching. Appending an
alternate response to historical canonical messages would make context order
ambiguous and weaken the one-linear-history invariants used by storage,
provider-context capture, and renderer replay.

## Decision

Protocol epoch 8 adds one narrow command:

- `RetryConversationTurn(source_turn_id, expected_revision)` uses request field
  49 and the existing envelope idempotency key. Epochs 1 through 7 are rejected
  before dispatch.
- The request contains no prompt, model, provider state, continuation, account,
  or credential field. The daemon canonically loads every one of those facts.
- `ConversationTurnResult.lineage` uses field 12 and exposes only `Original` or
  `Retry { source_turn_id }` plus a bounded depth. Field 13 exposes a closed,
  reasoned eligibility projection: `Allowed`, `NotNewest`,
  `SourceInProgress`, `SourceCompleted`, `SourceInterruptedNeedsReview`,
  `FailureNotRetryable`, `SourceAccountUnavailable`, `DepthExhausted`, or
  `SourceReadOnly`.
- Eligibility is explanatory, not authorization. The daemon revalidates the
  source atomically when handling the command. The local credential-generation
  binding never crosses Protobuf or renderer IPC.

A new retry is permitted only when all of these conditions still hold:

- the exact observed source revision matches;
- the source is `cancelled` before provider dispatch or `failed` with an
  explicit retryable known failure; completed, active, non-retryable, and
  uncertain `interrupted_needs_review` turns are excluded;
- the source user message is still the latest canonical message in its thread
  and the source has no existing retry child;
- the owning thread and project remain writable and the source depth is below
  the bounded maximum;
- source and child retain the same project, thread, recorded canonical model,
  local credential generation, and immutable provider context;
- the child receives a new canonical user-message identity and next sequence,
  but its prompt text is byte-for-byte equal to the source prompt; and
- lineage depth is exactly the source depth plus one and remains within 1–64.

The retry command fingerprint binds the source identity and revision, source
request fingerprint, recorded model, local credential generation, and exact
provider-context fingerprint. Reservation atomically creates the new user
message, queued run, turn, creation events, frozen context, and lineage before
the daemon-owned bounded task may dispatch. A source has at most one direct
retry child. A retry child that itself ends in a safe terminal state may be the
source of the next linear attempt, subject to the same checks and depth bound.

Schema 13 also seals every frozen context inside the reservation transaction:
context rows are written while the owner is a revision-zero reservation, then
the immutable lineage row closes further insertion. Later context insert,
update, and delete attempts are rejected. A failed seal rolls the entire
message/run/turn/context/lineage/event reservation back.

Replay does not broaden provider authority. A terminal or `provider_started`
retry replays its durable result without loading a current credential. A
reserved retry already owned by the daemon task registry also replays directly.
If a reserved retry is orphaned and must be reclaimed, the daemon first repeats
the original thread/current-generation and model preflight; the store's exact
reservation replay then returns the existing child instead of creating another.
Reusing the key with different canonical input conflicts.

### Local credential-generation binding

SQLCipher schema 13 adds immutable conversation-thread identity and turn-lineage
records. The only current source is the official xAI API. The first turn in an
empty thread binds that thread once to the daemon-local generation stored with
the validated credential in the operating-system vault. The binding is a
bounded non-secret local identifier derived from enrollment mutation identity,
not from credential bytes. It is not an official xAI account, user, tenant, or
subscription identity and must never be presented as one.

A distinct re-enrollment mutation creates a new local generation even when it
stores the same key. Exact idempotent replay of the original enrollment mutation
retains its generation. A bound thread does not switch generations; the user
must start a new thread. This conservative rule avoids claiming account
continuity that the available official contract does not prove.

The schema-12-to-13 migration backfills historical turns as original but
unbound. Their history and event journals remain readable. A thread that already
contains unbound legacy turns cannot claim a generation later, so new Start and
Retry commands fail closed. An empty migrated thread may bind normally on its
first turn. Lineage and thread bindings are immutable, and the migration is
transactional and restartable under the existing forward-only schema policy.

Credential reads and provider initiation use a daemon-internal read lease which
starts before the vault key and binding are loaded. Credential replacement and
deletion take the exclusive side of that gate. A provider dispatch rechecks its
lineage generation under the lease and retains it through durable
`provider_started` commit and network-stream initiation. If deletion wins, the
reservation is cancelled without provider I/O; if provider initiation wins,
deletion waits until that boundary is classified. Native enrollment pending
replay likewise reconciles only the exact already-installed generation and
never reopens entry under an existing generation identifier.

### Deferred edit, regenerate, and branch semantics

This command preserves one linear thread by appending the same failed or
undispatched prompt under frozen context. It does not authorize:

- editing a submitted prompt;
- regenerating an already completed assistant result;
- selecting a historical fork point; or
- placing multiple alternate children into one canonical message sequence.

Those operations need an explicit child-thread contract with durable parent
thread, fork point, and context-selection lineage. A child thread can preserve
the original immutable transcript while making the alternate history and its
credential/source policy unambiguous. Until that contract is designed,
edit/regenerate/branch controls remain unavailable rather than being aliases
for Retry.

## Consequences

- Retry can repeat only a known-safe attempt without granting the renderer
  provider, model, prompt, context, account, or credential authority.
- Changing the global model preference does not change a retry's recorded
  model. If that exact model or credential generation is no longer available,
  the retry fails before provider dispatch.
- Credential replacement intentionally ends write access to existing bound
  threads, even when a human believes the replacement represents the same xAI
  account.
- Completed and uncertain provider effects are never made replayable by this
  feature.
- Legacy schema-12 conversations remain inspectable but cannot acquire a
  retroactive credential identity.

## Rejected alternatives

### Let the renderer resubmit visible prompt and model fields

Visible text is not the immutable provider request. This would let stale or
compromised presentation state change canonical context and billing input.

### Bind threads to a hash of the API key

A key-derived identifier is credential-derived material, survives outside the
vault, and still does not prove an official xAI account identity.

### Treat every credential replacement as the same account

The current official contract does not prove that relationship. Assuming it
would allow an old thread to disclose context under a different credential.

### Retry completed or uncertain turns

Both may already have produced a billable provider outcome. Repeating either as
Retry would weaken the existing non-idempotent uncertainty boundary.

### Implement regenerate or branch as another child turn in the same thread

Multiple alternatives would make one chronological message sequence claim
several incompatible canonical contexts. Explicit child-thread lineage is
required first.
