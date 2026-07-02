# ADR 0009: Daemon-owned xAI Chat model discovery and selection

- Status: Accepted
- Date: 2026-07-11

## Context

Direct Chat previously used one process-compiled xAI model identifier. The
official xAI model catalog is credential-scoped and can change independently of
the desktop release, but neither the credential nor provider policy may move to
Electron or the renderer. A selection must also remain deterministic across
restarts and retries without causing a second billable Chat request.

Provider model data is untrusted input. Aliases can collide with another alias
or canonical identifier, modalities can contradict Chat support, and a model
that was selected earlier can disappear for a later key or catalog. Silently
choosing another model would change command meaning and provider cost.

## Decision

Protocol epoch 5 added `GetChatModelCatalog` and `SelectChatModel`; current
epoch 6 retains both operations unchanged. Epochs 1 through 5 are rejected
before dispatch.

- The daemon loads the user-owned key from its vault and calls only the fixed
  official xAI model adapter. Keys, endpoints, request headers, and provider
  bodies never cross IPC. Discovery has a 12-second provider bound inside a
  15-second daemon operation bound; Electron uses a 16-second correlated RPC
  deadline so local commit and response handling retain reserve.
- Catalog descriptors retain the adapter's count and field bounds. Canonical
  IDs and aliases share one uniqueness namespace: duplicate aliases,
  alias-to-alias collisions, and alias-to-canonical collisions reject the
  complete catalog. Explicit input or output modalities without `text` make a
  descriptor unavailable for Chat. Empty modality lists mean the provider did
  not advertise a contradiction; every Chat request still undergoes live
  discovery before dispatch.
- The selection use case accepts only an exact canonical ID or advertised
  alias. A unique alias is canonicalized to its owning model ID before durable
  persistence and before any later turn fingerprint. Ambiguous aliases are
  never resolved by ordering. The revision-zero compiled product default may
  itself be advertised as an alias; it is a bootstrap policy rather than a
  completed mutation. Every revision greater than zero is canonical whenever
  that selected descriptor is present in the live catalog.
- SQLCipher schema 9 stores a singleton `ChatModelPreference` with optimistic
  revision and timestamp, plus an exact idempotency journal. A completed exact
  selection replay resolves before vault access or another provider call. A
  crash before local commit may repeat only the read-only catalog request.
  Validation, timeout, or revision conflict leaves the previous preference
  unchanged. Persisted rows are revalidated through the domain constructor on
  read.
- `ConversationService` reads the committed preference for each new turn and
  includes its canonical model ID in the immutable command fingerprint and
  `ConversationTurn`. Existing turns never change. Reusing a turn idempotency
  key after the global preference changes deterministically conflicts before a
  second provider lookup or effect; it is never dispatched under the new model.
- Credential validation now records that at least one text-capable official
  model was discovered, rather than requiring the compiled default. Capability
  resolution separately performs bounded live discovery for the persisted
  selection. A missing selection is reported not ready; there is no fallback to
  the product default or first catalog entry.
- Electron independently validates model counts, string bounds, alias
  uniqueness, readiness consistency, and safe integer conversion before
  exposing descriptors to the renderer. The renderer can submit only a model
  ID and optimistic revision through an exact-key bridge request. It retains a
  selection idempotency key across an ambiguous transport failure.

## Consequences

Settings can show a live, credential-scoped list and safely change the default
for future Chat turns. A valid key without `grok-4.3` can select another
advertised text model and become Chat-ready.

Catalog or network failure is an honest unavailable state. If a persisted model
disappears, Chat remains fail-closed until the person explicitly selects a new
ready model. Effort, Research, arbitrary compatible endpoints, provider token
streaming, and subscription-model selection are not enabled by this decision.
