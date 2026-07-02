# ADR 0008: Resumable long polling for durable run events

- Status: Accepted
- Date: 2026-07-11
- Current note: ADR 0013 adds a separate epoch-7 turn-local Chat stream; this
  ADR continues to govern only the durable run-audit channel.

## Context

The daemon already commits an append-only `RunEvent` audit stream in the same
transactions that create or change a run, request approval, prepare a side
effect, or mark an uncertain effect for review. Each stream has a contiguous
run-local sequence beginning at one, and `EventsSince` can replay a bounded
snapshot after a cursor. There was no bounded way for Electron to wait for new
durable events, however. Repeated renderer polling would duplicate lifecycle
and reconnect policy outside the daemon boundary, while unsolicited frames on
the shared unary connection would require a larger multiplexing protocol.

Token deltas and provider progress are not durable run events today. Treating
in-memory provider output as resumable would create a false recovery promise
and could encourage automatic replay of an uncertain non-idempotent request.

## Decision

Protocol epoch 4 adds the read-only `PollRunEvents` operation. A caller supplies
one run ID, the last completely consumed run-local sequence, a batch limit, and
a wait timeout. The daemon first validates that the run exists, returns
immediately when durable events are available, or waits for at most 20 seconds
before returning an empty batch that echoes the cursor. It fetches one extra
event to report `has_more` while returning at most 100 events. The operation's
wait must remain at least one second below its envelope deadline. Cursors are
also limited to the durable store's nonnegative signed 64-bit sequence range.

Electron runs each subscription on a dedicated nonce-paired connection, so an
idle long poll cannot block ordinary unary RPC. One supervisor permits at most
eight subscriptions; the daemon permits at most 64 concurrent IPC connections,
and every connection still uses the 4 MiB frame limit and sequential request
backpressure. Closing a subscription or stopping the supervisor aborts its
pending poll and destroys that exact socket.

The Electron client accepts a batch only when ownership, event shape, bounds,
and every sequence are valid and contiguous from the requested cursor. It
advances its in-memory cursor only after the listener has successfully consumed
the complete validated batch. A transport interruption retries this read-only
poll from the prior cursor with bounded exponential delay. Protocol corruption,
a non-retryable daemon response, listener failure, or a listener that does not
finish within five seconds closes the subscription without advancing the cursor.
This provides at-least-once delivery across an ambiguous response boundary;
consumers must treat sequence as the event identity.

Only the existing structured `RunEvent` variants cross this channel: lifecycle
edges and non-secret approval/effect identifiers. Credentials, provider text,
diagnostics, tool output, and arbitrary payloads are not event fields.
`Envelope.Event` remains unused. IPC v7 direct Chat uses the distinct durable
turn-local long poll from ADR 0013; it does not place provider text into
`RunEvent`. General background-operation events still require their own durable
producer contracts.

## Consequences

- A renderer or replacement Electron process can resume from a durable run
  cursor without asking the daemon to repeat an action.
- Poll retry is safe because it is read-only. It never starts a provider call,
  executes a tool, or replays a side effect. An uncertain non-idempotent effect
  remains `interrupted_needs_review`.
- A dedicated connection and bounded subscription count avoid head-of-line
  blocking and unbounded idle work at the cost of additional local sockets.
- Store polling currently checks at a bounded 100-millisecond interval. A
  daemon-internal notification optimization may replace that wake-up mechanism
  later without changing cursor or wire semantics.
- Epochs 1, 2, 3, and 4 are rejected before dispatch. Generated bindings remain in
  the canonical `grok.desktop.daemon.v1` schema family; the envelope epoch is
  the compatibility authority.
