# ADR 0011: Remove renderer execution-producer authority

- Status: Accepted
- Date: 2026-07-11

## Context

The local daemon protocol and Electron preload bridge exposed generic
`CreateRun`, `TransitionRun`, and `RequestApproval` mutations. They were not used
by the production renderer, but their shapes let renderer input choose terminal
or review lifecycle states and manufacture the action, target, disclosure, risk,
scope, and expiry of an approval. A compromised renderer must not be able to
produce execution audit records or describe the action it is asking the user to
approve.

Run creation, lifecycle transitions, approval requests, completion, failure,
and interruption review are producer decisions. Only the daemon use case that
owns an operation has enough trusted context to create them. A user may decide
an exact pending approval or request a typed operation-specific cancellation;
neither intent grants authority to select an arbitrary `RunState`.

## Decision

Protocol epoch 6 removes request fields 3 (`create_run`), 4
(`transition_run`), and 6 (`request_approval`) and response field 3 (`run`). The
field numbers and names remain reserved. Epochs 1 through 5 are rejected before
dispatch.

- `RunService` and `ApprovalService` remain inward-facing application APIs for
  trusted daemon producers and recovery coordinators.
- The public daemon transport retains read-only durable run-event polling and
  the exact revisioned `DecideApproval` user intent.
- The preload bridge rejects producer mutations and accepts only an approval ID,
  observed revision, Grant/Deny choice, and idempotency key for a decision. It
  cannot carry an action description, risk, scope, progress, target state, or
  executable data.
- Pause/resume remains unavailable until a specific producer implements a
  cooperative contract. Cancellation will be operation-specific and must define
  pre-dispatch, in-flight, ambiguity, and restart behavior before it is exposed.

## Consequences

- Renderer compromise cannot fabricate successful, failed, completed, or
  review-required runs or social-engineer approval disclosures through the
  generic bridge.
- Existing application and persistence tests continue to cover run and approval
  state machines without making those producer operations public IPC.
- Future Activity UI requires a canonical daemon read model containing the exact
  approval and daemon-derived allowed actions; it cannot infer controls from a
  run state alone.
