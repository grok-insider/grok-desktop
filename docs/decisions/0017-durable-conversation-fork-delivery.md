# ADR 0017: Durable conversation-fork result delivery

- Status: Accepted
- Date: 2026-07-11

## Context

Protocol epoch 9 makes Branch, Edit-and-branch, and Regenerate exact daemon
mutations. Their scoped idempotency key is deliberately renderer-ephemeral.
Electron retains a key while one process is alive, but a renderer restart after
the daemon committed a child and before the response was accepted loses that
key. Repeating the same Edit or Regenerate with a new key can then reserve a
second billable turn even though the first child already exists.

Persisting mutation keys or business intent in renderer storage would move
durable authority across the daemon boundary. Making keys deterministic forever
would prevent a user from deliberately performing the same operation again.
The daemon therefore needs a bounded at-least-once presentation-delivery
contract which distinguishes an unresolved response from a later deliberate
fork.

## Decision

Protocol epoch 10 adds a required `ConversationForkDelivery` to every
`ConversationForkResult` and adds
`AcknowledgeConversationForkDelivery(child_thread_id, expected_revision)`.
The projection contains only the canonical child identity, `Pending` or
`Acknowledged` state, and its optimistic revision. Mutation keys and request
fingerprints never cross the wire.

Every newly committed fork creates a pending delivery at revision zero in the
same transaction as the child, copied messages, optional turn, and canonical
fork command. A fork request resolves in this order:

1. An exact scoped key replays its previously bound child after fingerprint
   validation.
2. A new key whose scope and request fingerprint match one pending delivery is
   atomically bound as an immutable alias and returns that child without a new
   provider dispatch.
3. An acknowledged or absent matching delivery does not coalesce the request;
   the normal eligibility and reservation path may create a deliberate new
   child.

The pending `(command_scope, request_fingerprint)` relation is unique. A child
accepts at most 64 immutable reconciliation aliases, and canonical and alias
key namespaces reject cross-table collisions. The bound prevents an untrusted
renderer from growing the journal without limit. Reservation repeats the
pending lookup transactionally to close concurrent-key races. A narrow race may
perform redundant official model discovery, but it cannot create a second
child, turn, or provider generation call.

The renderer acknowledges only after it has strictly validated the fork wire
projection, fetched and validated the canonical child aggregate, and installed
that aggregate in its in-memory state. Acknowledgement uses a separately scoped
idempotency key. Pending revision zero moves exactly once to Acknowledged
revision one. Replaying the same acknowledgement key and fingerprint returns
revision one; a new key after acknowledgement, key reuse with different input,
or any other revision conflicts. The renderer retains both uncertain fork and
acknowledgement keys until the validated acknowledgement response arrives, or
until an exact fork replay proves that delivery is already Acknowledged.

SQLCipher schema 15 stores delivery rows, bounded immutable fork-key aliases,
and exact acknowledgement commands. Schema-14 fork commands are migrated to
Acknowledged revision one without synthetic acknowledgement commands. This
avoids presenting historical forks as unresolved and permits multiple legacy
rows with the same fingerprint because the uniqueness constraint covers only
pending rows. It cannot recover a response that was already lost while epoch 9
was running, and no such upgrade claim is made.

Acknowledgement is a presentation handoff marker, not proof that a person read
the response. There is an unavoidable process-crash window after the daemon
commits the acknowledgement and before Electron observes its response. At that
point the renderer had already validated and installed the child, and the
canonical child remains discoverable through its family. This protocol does not
claim distributed exactly-once user observation.

## Consequences

- A renderer restart before acknowledgement can safely repeat an identical
  Branch, Edit-and-branch, or Regenerate with a new key and recover the existing
  child without another billable provider request.
- The daemon remains the only durable owner of intent, child identity, replay,
  and delivery state; no renderer database or credential-derived identifier is
  introduced.
- Acknowledged intent fingerprints are released so a later explicit identical
  operation can create a new child.
- Fork delivery recovery does not weaken provider-start uncertainty: an
  interrupted non-idempotent turn remains `interrupted_needs_review` and is
  never automatically redispatched.
- The wire and schema changes require protocol epoch 10 and forward-only schema
  15 compatibility evidence.

## Rejected alternatives

### Persist fork mutation keys in the renderer

That would make renderer storage authoritative for durable business intent and
would still require integrity, rollback, and multi-process reconciliation.

### Coalesce every identical fork forever

That would prevent a user from deliberately requesting another answer or
creating another same-content branch after the first result was accepted.

### Acknowledge immediately in Electron main

Main cannot prove the renderer validated the child-owned lineage, canonical
message prefix, inherited outcomes, and optional turn. Acknowledging before
those checks would discard the recovery signal too early.

### Automatically replay provider work during reconciliation

The existing child and its durable turn are the result. Provider-started or
uncertain work is never repeated; normal turn events and restart recovery expose
its canonical state.
