# ADR 0013: Durable asynchronous Chat events and exact cancellation

- Status: Accepted
- Date: 2026-07-11

## Context

The direct xAI Chat operation in protocol epoch 6 is unary: one
`ExecuteConversationTurn` request remains open until the provider outcome and
terminal aggregate are durable. A renderer or Electron restart cannot resume
provider text, distinguish an accepted turn from a lost response, or express a
turn-specific cancellation with an observed revision. The durable run-event
stream is an audit stream and must not be repurposed for provider text.

Provider requests are non-idempotent once dispatch may have crossed the network
boundary. Cancellation therefore cannot mean "report cancelled" for every
state. In particular, aborting a provider-started request does not prove that
xAI did not complete it.

## Decision

Protocol epoch 7 replaces the unary Chat operation with a durable asynchronous
contract. Epochs 1 through 6 are rejected before dispatch. Generated bindings
remain in the `grok.desktop.daemon.v1` schema family; the envelope epoch is the
compatibility authority.

Request field 38 (`execute_conversation_turn`) is removed, and both its number
and name remain reserved permanently. Epoch 7 adds:

- `StartConversationTurn(thread_id, content)` at request field 46. It durably
  reserves or exactly replays a turn and returns the current
  `ConversationTurnResult` without keeping the request open for provider
  completion.
- `CancelConversationTurn(turn_id, expected_revision)` at request field 47. It
  is an exact optimistic intent, not permission for a caller to choose a turn
  state.
- `PollConversationTurnEvents(turn_id, after_sequence, limit,
  wait_timeout_ms)` at request field 48. It is a read-only correlated long poll
  and returns `ConversationTurnEventBatch` at response field 26.

`StartConversationTurn` and `CancelConversationTurn` use the existing
`ConversationTurnResult` response field. Epoch 7 adds its previously absent
`revision` at fresh field 11 so cancellation can name the exact observed
aggregate revision without inferring it from renderer state. Mutation replay remains tied to the
bounded envelope idempotency key and canonical request fingerprint.

### Epoch 7 wire compatibility

| Shape | Field | Epoch 7 treatment |
| --- | ---: | --- |
| `Request.execute_conversation_turn` | 38 | Removed; number and name permanently reserved |
| `Request.start_conversation_turn` | 46 | Added |
| `Request.cancel_conversation_turn` | 47 | Added |
| `Request.poll_conversation_turn_events` | 48 | Added |
| `Response.conversation_turn_event_batch` | 26 | Added |
| `ConversationTurnResult.revision` | 11 | Added for exact optimistic cancellation |

Cancellation has state-specific semantics:

- `reserved -> cancelled` commits before returning and proves that no provider
  dispatch occurred.
- `provider_started -> interrupted_needs_review` commits before the daemon asks
  the provider task to abort. The turn is never described as safely cancelled
  and is never replayed automatically.
- If completion, failure, interruption recovery, or another cancellation wins
  the optimistic race, the command returns that durable winner. It does not
  overwrite a terminal revision or synthesize a second outcome.

Each turn owns an append-only durable event stream with contiguous sequence
numbers beginning at one. Epoch 7 exposes only three normalized event kinds:

- `Created`, establishing the initial `reserved` projection with no provider
  text;
- `StateChanged`, with exact previous and next `ConversationTurnState` values;
- `TextAppended`, containing a non-empty canonical UTF-8 assistant-text chunk.

Provider transport delta boundaries are not durable API boundaries. The daemon
coalesces or splits them at UTF-8 scalar boundaries so each `TextAppended`
payload is at most 16 KiB and concatenating events in sequence reconstructs the
canonical assistant prefix. Each text event carries its exact starting UTF-8
byte offset so overlap, gaps, and ambiguous replay fail validation. A turn may
persist at most 1 MiB of appended text and at most 4,097 text events, preventing
a corrupted one-byte-event flood. Empty, oversized, out-of-order, or
post-terminal text is invalid; an uncertain post-dispatch outcome retains the
existing `interrupted_needs_review` policy. Event payloads contain no
credentials, wall-clock metadata, provider diagnostics, tool output, or
arbitrary metadata.

Polling uses a dedicated nonce-paired connection so an idle wait cannot block
ordinary unary commands. `after_sequence` is the last event completely
consumed by the caller. A batch returns contiguous events, `next_sequence`, and
`has_more`; callers advance only after consuming the whole validated batch.
Retrying the read-only poll from the prior cursor provides at-least-once
delivery. `Envelope.Event` remains reserved and unused.

SQLCipher schema 12 is the persistence boundary for the turn-local stream. Its
forward migration must be transactional and restartable from schema 11.
Reservation, state transition, terminal assistant content, and their matching
events commit atomically. The schema and stores must enforce turn ownership,
contiguous positive sequence, the 16 KiB event bound, the 1 MiB aggregate bound,
the text-event-count bound, and stable cursor replay. No renderer-owned or
in-memory-only stream satisfies this contract.

## Consequences

- Electron can reconnect to an accepted turn without asking the daemon to
  repeat a provider request, and text already committed before a disconnect is
  replayable by sequence.
- Cancellation cannot weaken the uncertain non-idempotent boundary. A
  provider-started turn remains review-required even if task abortion appears
  locally successful.
- Chat event polling is separate from run audit polling. Neither channel uses
  unsolicited envelope payloads, and neither claims general background-task
  progress support.
- Epoch 6 clients and daemons fail closed instead of silently interpreting the
  removed unary operation or the new asynchronous response shapes.
- The wire decision alone does not enable asynchronous Chat. The accepted
  implementation includes daemon handlers, schema-12 memory/SQLCipher stores,
  atomic cancellation-command replay, bounded task ownership, acknowledged
  Electron delivery, independent renderer projection validation, and exact Stop
  UX before the UI exposes the contract.

## Rejected alternatives

### Keep the unary request open and reconnect by retrying it

An ambiguous disconnect could duplicate a billable non-idempotent provider
request and offers no durable text cursor.

### Put provider text into `RunEvent`

Run events are bounded audit facts shared by non-Chat workflows. Provider text
has different ownership, volume, retention, and terminal consistency rules.

### Use unsolicited `Envelope.Event` frames

That would require multiplexing and backpressure on the shared unary transport.
Dedicated correlated long polls retain the existing request/response pairing
and make reconnect cursors explicit.

### Stream renderer-local provider deltas

In-memory deltas disappear on restart and would falsely imply resumability.
Only schema-12 events committed by the daemon are part of the asynchronous Chat
contract.

### Move direct Chat to the TypeScript AI SDK

Using the AI SDK's xAI provider in Electron or the renderer would move provider
dispatch and credential access out of the Rust daemon, violating the product's
single-authority boundary. Retaining the daemon while adapting its durable
events to an AI SDK UI transport would add a second stream state machine without
removing the schema-12 journal, exact cancellation, canonical terminal reload,
or acknowledged cursor logic. The daemon-owned contract therefore remains the
core Chat implementation; isolated presentation helpers may be reconsidered
only if they do not acquire provider, secret, persistence, or retry authority.

This remains true with AI SDK 7. Its TypeScript agent stack, official xAI
Responses adapter, durable `WorkflowAgent`, and React transports are useful in
other architectures, but adopting them here would add a second runtime and
durable state machine beside SQLCipher. The generic `streamText` surface
defaults to two retries, while an accepted billable xAI request must never be
replayed after an uncertain outcome. The xAI Responses adapter also defaults
`store` to `true`; Grok Desktop deliberately sends `store: false`. Message
persistence remains an application responsibility, and the documented
resumable-stream design is incompatible with abort. A future UI-only spike must
use a daemon-backed custom transport, set no provider/secret/tool/retry or
persistence authority in JavaScript, and demonstrably delete meaningful
projection code; otherwise it is a second state machine and should not ship.
See the official [AI SDK 7 release](https://vercel.com/changelog/ai-sdk-7),
[xAI provider](https://ai-sdk.dev/providers/ai-sdk-providers/xai),
[custom transport](https://ai-sdk.dev/docs/ai-sdk-ui/transport),
[persistence](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-message-persistence),
[resume](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-resume-streams), and
[`streamText`](https://ai-sdk.dev/docs/reference/ai-sdk-core/stream-text)
documentation.
