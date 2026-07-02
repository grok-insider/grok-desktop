# ADR 0020: Remove public generic message mutation authority

- Status: Accepted
- Date: 2026-07-11

## Context

The desktop protocol still exposed generic Create, Update, and Delete Message
requests even though the production renderer uses only typed daemon-owned
conversation commands: start, cancel, retry, branch, edit-and-branch, and
regenerate. The dormant generic bridge could create arbitrary system, user, or
assistant history outside turn reservation, provider dispatch evidence,
credential/model binding, immutable context, fork provenance, and normalized
event delivery. It could also rewrite or delete a message before a later turn
captured that history.

Durable message state is part of the daemon-owned conversation aggregate. A
renderer-connected client must not manufacture provider history or bypass the
typed commands that persist intent and provenance.

## Decision

Protocol epoch 12 removes the public CreateMessage, UpdateMessage, and
DeleteMessage operations and their request messages. Request fields 18, 19,
and 20 and the names `create_message`, `update_message`, and `delete_message`
are permanently reserved. Legacy encodings of those fields decode to no
operation and are rejected before any mutation handler can run.

Electron removes the corresponding RPC client, daemon-supervisor, preload
bridge, validation, and main-process handlers. The daemon removes their public
dispatch and wire-to-domain role conversion. GetMessage and ListMessages remain
read-only projections.

Application/store message primitives remain inward-facing. They are required
by the daemon's typed conversation-turn and fork producers and by adapter
conformance tests; their presence does not grant renderer authority. The
SQLCipher schema remains version 15 because this change removes a wire path and
does not change durable representation.

Epochs 0 through 11 are rejected by epoch-12 peers. This is an intentional
local compatibility break; the Protobuf package remains the canonical v1
schema family while the envelope epoch identifies the accepted operation set.

## Consequences

- A renderer-connected client cannot inject arbitrary system/user/assistant
  history or rewrite/delete canonical messages through generic desktop IPC.
- Typed start/retry/fork commands remain the only public message-producing
  paths and retain their existing intent, provider, lineage, recovery, and
  presentation-delivery contracts.
- Get/List projections and daemon-internal producer APIs remain available.
- Reintroducing a public message producer requires a new protocol epoch and an
  independently reviewed typed contract; the reserved tags and names are never
  reused.

## Rejected alternatives

### Keep the operations because the renderer does not currently call them

The isolated preload exposes a generic request bridge, so dormant handlers are
still reachable authority. Lack of a current UI button is not a security
boundary.

### Restrict CreateMessage to the user role

That would still bypass durable turn reservation, selected-model and credential
binding, dispatch evidence, event delivery, cancellation, retry, and fork
lineage.

### Make submitted messages mutable with revision checks

Revision checks prevent stale writes but do not preserve immutable provider
context or turn/fork provenance. Edit-and-branch is the explicit supported
operation and never rewrites the parent thread.
