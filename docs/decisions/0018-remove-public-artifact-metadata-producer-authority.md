# ADR 0018: Remove public artifact-metadata producer authority

- Status: Accepted
- Date: 2026-07-11

## Context

Protocol epoch 10 still exposed `CreateArtifact`, `UpdateArtifact`, and
`DeleteArtifact` requests on the authenticated desktop IPC connection. Those
requests let a caller assert a relative path, media type, byte size, and
available artifact state even though Grok Desktop does not yet have a trusted
ingestion producer, content-version contract, file-identity check, or safe
local-open/delete broker.

The production Electron client did not call these mutations; it reads bounded
artifact projections for Library, Projects, and workspace search. Keeping
unused producer operations public nevertheless enlarged renderer authority and
could create metadata that claimed content existed without a daemon-owned
content operation.

## Decision

Protocol epoch 11 removes the three public mutation operations. Request field
numbers 23, 24, and 25 and their names `create_artifact`, `update_artifact`, and
`delete_artifact` are reserved permanently. Legacy payloads therefore decode as
unknown fields and cannot become dispatchable operations. Epochs 1 through 10
are rejected before dispatch.

`GetArtifact` and `ListArtifacts` remain public read operations. Artifact
entities, `WorkspaceService` mutations, and memory/SQLCipher store operations
remain inward-facing primitives so a future trusted daemon producer can use
them without moving authority into Electron. SQLCipher remains at schema 15;
this wire-only removal does not rewrite or delete existing artifact rows.

A future public content workflow requires a separately reviewed contract for
bounded ingestion, content identity and versions, atomic publication, safe
open/delete, disclosure policy, recovery, and platform reparse behavior. It
must expose intent appropriate to that workflow rather than restoring generic
metadata CRUD.

## Consequences

- Renderer-connected clients cannot manufacture, rewrite, or delete artifact
  metadata.
- Existing artifact projections remain readable and searchable.
- Legacy tags cannot be reused for another operation, and older clients fail
  at the protocol-version boundary.
- A trusted future ingestion producer can retain the tested application and
  persistence primitives behind a narrower port.

## Rejected alternatives

### Keep the operations because Electron does not currently call them

An authenticated but compromised renderer-facing client must not receive
authority merely because the first-party client happens not to exercise it.

### Treat artifact metadata as harmless presentation state

Available state, path, type, and size are claims about daemon-owned content.
Accepting those claims without content validation would make metadata an
untrusted substitute for a content contract.

### Remove all artifact storage and read projections

Existing durable rows and read-only Library/search projections remain useful.
The unsafe boundary is public production, not the inward-facing domain and
store primitives.
