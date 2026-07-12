# ADR 0028: Durable per-conversation model binding

## Status

Accepted

## Context

The global chat-model preference is a composer default, not durable conversation
identity. Resolving every turn from that mutable preference can silently change
the model used by an existing thread and makes an idempotent replay depend on
live provider discovery.

## Decision

Protocol epoch 22 adds the optional `model_id` field to
`StartConversationTurnRequest`. The daemon validates an explicit override, or
the global default, against the live official model catalog before reserving a
new thread's first turn. The canonical model ID participates in the request
fingerprint and is atomically bound with that first reservation.

Later ordinary turns use the durable thread binding and reject a conflicting
override. Exact completed replays resolve their fingerprint from the binding
before credential or provider access. Retries preserve the source turn model.
Forks bind the child thread to the source turn model.

Schema 23 adds a nullable model binding to `conversation_thread_identity`.
Existing non-empty threads are deterministically backfilled from their most
recent durable turn. Older turns remain readable even when historical threads
contain multiple models; the binding governs only future ordinary turns.

## Consequences

- Changing the global default affects new, unbound threads only.
- A thread cannot silently switch models after its first reservation.
- Legacy protocol-21 payloads decode with no override and retain their prior
  behavior through the global default on new threads.
- Catalog removal can make a bound thread temporarily unavailable for a new
  ordinary turn, while exact terminal replay remains provider-free.
