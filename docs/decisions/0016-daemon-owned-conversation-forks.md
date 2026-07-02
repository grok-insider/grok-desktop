# ADR 0016: Daemon-owned conversation forks

- Status: Accepted
- Date: 2026-07-11

## Context

Editing a submitted prompt, generating another answer to a completed prompt,
and branching from historical context are not mutations of an existing linear
conversation. An in-place renderer edit would change canonical provider input
after a request was billed, while appending an alternate answer to the same
thread would make message order and future context selection ambiguous.

The epoch-8 Retry command is deliberately narrower. It may create one new
attempt only for the latest cancelled or explicitly retryable known failure,
using the exact frozen source request. A completed request has a known billable
outcome and therefore requires explicit Regenerate semantics. A different
prompt requires Edit-and-branch semantics. Neither operation may be disguised
as Retry.

The official xAI API surface used by this application does not expose a stable
account identifier suitable for durable thread ownership. The daemon already
stores the fixed official-xAI source and an opaque local credential-generation
binding. The latter is not an account identity, is not derived from key bytes,
and never crosses renderer IPC. This decision does not invent or claim an
official account identity.

## Decision

Protocol epoch 9 adds three explicit commands:

- `BranchConversationThread(source_turn_id, expected_revision)` creates a
  readable child thread after a completed source response and performs no
  provider request.
- `EditAndBranchConversationTurn(source_turn_id, expected_revision, content)`
  creates a child thread, replaces only the source turn's final user content,
  and reserves a new billable turn using the source turn's recorded model.
- `RegenerateConversationTurn(source_turn_id, expected_revision)` creates a
  child thread and reserves another billable attempt for the exact source
  prompt and frozen context using the source turn's recorded model.

The requests do not accept a target thread, project, model, context, account,
credential, continuation identifier, provider state, tool configuration, or
lineage supplied by the renderer. The daemon loads and revalidates all of those
facts from canonical storage.

Request fields 50, 51, and 52 and response field 27 are fresh. Request field 53
loads bounded fork metadata at response field 28 so reload can correlate copied
assistant outcomes and the root family without synthesizing turns. Epochs 1
through 8 are rejected before dispatch. `ConversationForkResult` contains the
canonical child thread and an optional started turn. Branch requires the turn
to be absent; Edit-and-branch and Regenerate require it to be present and owned
by the child thread.

### Immutable thread lineage

Every thread has validated lineage:

```text
ConversationThreadLineage
  root_thread_id
  origin:
    Original
    Fork {
      parent_thread_id
      source_turn_id
      source_message_id
      kind: Branch | EditAndBranch | Regenerate
    }
  fork_depth
```

An original's root is itself and its depth is zero. A fork has the same project
and root as its parent, a distinct child identity, a depth exactly one greater
than its parent, and an exact source turn and message in the immediate parent.
Fork depth is at most 64. One parent may have at most 64 direct children, and a
root family may contain at most 256 threads. These bounds are revalidated in
the atomic store transaction rather than trusted from a prior UI projection.

All fork kinds require an active source project. An open or archived source
thread may be forked because the source is not mutated; the newly created child
is open. The child inherits the parent title and remains independently
renameable or archivable afterward. Historical, non-latest completed turns may
be forked; forking is an explicit divergence and does not alter the parent.

The child inherits the parent's fixed official-xAI source and exact local
credential-generation binding. Legacy unbound threads cannot fork. Pure Branch
does not require the generation to remain currently installed because it has
no provider side effect; a resulting child remains readable and ordinary Chat
continues to fail closed if its inherited generation is unavailable.

### Child-owned message copies

A fork never shares canonical message identity with its parent. Every copied
message receives a new globally unique ID, belongs to the child thread, and is
assigned a contiguous child-local sequence beginning at one. Immutable
derivation records identify the source message, source turn, context position,
and whether the child message is a context copy, copied source assistant, or
edited user prompt.

Branch requires a completed source. It copies the source turn's exact immutable
provider context and then its completed assistant response. The child has no
new run, effect, turn, or provider call.

Edit-and-branch accepts a completed, cancelled, or known failed source. It
excludes reserved, provider-started, and `interrupted_needs_review` sources. It
copies the immutable source provider context except its final user entry,
appends the validated edited user content, and rejects byte-identical content.
The edited message becomes the new child turn's canonical user message.

Regenerate requires a completed source. It copies the exact immutable provider
context, including a child-owned copy of the final user message. That copy
becomes the new child turn's canonical user message. The completed source
assistant is not copied because it is the divergence point.

Copied assistant messages retain presentation outcomes through immutable
inherited-outcome records pointing at their canonical completed source turns.
The bounded fork-metadata projection includes citations, usage, observed
zero-data-retention state, recorded model, validated thread lineage, and at most
the 256 members of the root family. It does so without cloning a run, effect,
provider response identity, or billing record. Search indexes each child-owned
message normally, so a copy may appear once per child thread and always routes
unambiguously to its owner.

### New provider turns

Edit-and-branch and Regenerate are explicit new billable attempts. Their turn
lineage records the fork operation and source turn with retry depth zero. A
later safe Retry may reference that new attempt under the existing epoch-8
rules.

Before the atomic reservation, the application requires the currently
installed credential generation to match the inherited thread binding and
discovers the source turn's recorded canonical model through the official xAI
adapter. It ignores the current global model preference. The store then
revalidates the source revision, lifecycle, project/thread state, lineage,
binding, context, copy derivations, family bounds, and idempotency command in
one transaction.

Provider dispatch uses the existing bounded daemon task registry, durable
`reserved -> provider_started` transition, coherent credential-use lease,
normalized event stream, exact Stop semantics, terminal validation, and startup
recovery. If the daemon exits before dispatch, startup recovery cancels the
reservation. If dispatch may have crossed the network boundary, uncertainty
remains `interrupted_needs_review` and is never regenerated or retried
automatically.

### Persistence and replay

SQLCipher schema 14 adds immutable thread-fork, message-derivation, inherited-
outcome, and fork-command records. It expands turn lineage for Edit-and-branch
and Regenerate sources while preserving all schema-13 rows byte-for-byte in
meaning. Absence of a thread-fork row means Original, so no synthetic fork
backfill is required.

One transaction creates the child thread, inherits and validates its identity,
copies messages, records derivations and inherited outcomes, optionally creates
the run/turn/context/events, seals lineage, and records the exact command
result. A fault at any point rolls back every child row and any binding change.
Fork, derivation, inherited-outcome, and command rows cannot be updated or
deleted through generic workspace operations.

The transaction also projects the child's inherited metadata before writing
anything. At most 256 inherited assistant outcomes and a conservative 3 MiB
metadata estimate are allowed. Memory, SQLCipher, the application use case, and
Electron reapply the same bounds on load. SQLCipher validates inherited
assistant derivation chains iteratively with a visited set and a depth bound;
it does not recursively materialize prior fork snapshots while holding the
store connection.

An exact idempotency replay returns the original child and optional turn. Reuse
of a scoped key with different source revision, kind, or edited content is a
conflict. Electron retains an ambiguous mutation key until a canonical child
matching the expected parent, source, kind, and optional turn is observed for
the lifetime of that renderer process. The renderer does not durably persist
business intent. Epoch 9 alone therefore could not recover a lost key after a
renderer restart. Protocol epoch 10 and SQLCipher schema 15 now add the
daemon-owned bounded pending-delivery alias and acknowledgement contract
specified by [ADR 0017](0017-durable-conversation-fork-delivery.md); that later
decision does not alter epoch 9's immutable fork semantics.

### Renderer behavior

The renderer exposes Branch only for eligible completed assistant messages.
Edit opens a labeled dialog containing the prior user content and dispatches
only after explicit confirmation. Regenerate opens an explicit confirmation
stating that it sends another billable request. Successful operations navigate
to the canonical child route; parent content is never edited in place.

The branch switcher is backed by daemon thread lineage rather than synthetic
`Main` labels or renderer-maintained counts. It groups threads by root and
navigates through the existing canonical conversation route. Active,
uncertain, and inherited copied messages render actionless explanations.
Store-only rejections such as a legacy missing binding, inactive project, or
exhausted depth/family bound return a fixed error without creating a child;
they are not guessed from renderer state. Archived source threads remain
eligible as described above when their owning project is active.

## Consequences

- Parent history and already-billed outcomes remain immutable.
- Every future provider request has one unambiguous child-thread context.
- Search, deep links, and message ownership retain globally unique canonical
  routes.
- Regenerate and Edit-and-branch reuse the hardened asynchronous Chat machinery
  without granting the renderer provider authority.
- Pure Branch remains useful without initiating a provider call.
- An official account identity remains unavailable until an official Grok/xAI
  contract exposes one; the local generation binding must not be relabeled as
  that identity.

## Rejected alternatives

### Mutate the parent message and delete later output

That would rewrite canonical history after provider dispatch and invalidate
stored request fingerprints, citations, usage, and audit evidence.

### Append alternate assistant responses to one thread

Future context would have no canonical answer ordering, and search/deep-link
ownership could not identify which branch the user intended.

### Implement Regenerate as Retry

A completed source already has a known outcome. Retry intentionally excludes
it; Regenerate is a separately confirmed billable request in a new thread.

### Let the renderer submit copied context or a model

Renderer state is untrusted and may be stale, incomplete, or attacker
controlled. Only immutable daemon-loaded context and the source turn's recorded
model define the fork.

### Derive an account identity from the API key

That would retain a credential fingerprint outside the vault without becoming
an official account contract. Credential bytes and their derivatives remain
secret.
