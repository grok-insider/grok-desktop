# ADR 0022: Daemon-owned artifact import and exact-version open

- Status: Accepted
- Date: 2026-07-11

## Context

Artifact reads expose bounded metadata, but epoch 13 deliberately removed the
daemon's storage path and offered no public artifact producer. Importing a
person-selected file and opening stored content therefore need explicit
operations without restoring generic metadata mutation or turning a storage
locator into renderer authority.

An import necessarily begins with an operating-system path selected by trusted
Electron main. That path is ephemeral input, not artifact identity. Once the
request crosses into the daemon, durable state and later operations must use a
canonical artifact ID and immutable content version instead.

## Decision

Protocol epoch 14 adds two typed request operations:

- `ImportArtifact` carries a project ID, optional thread ID, portable display
  name, media type, and one ephemeral source path.
- `OpenArtifact` carries only a canonical artifact ID and exact content
  version.

The envelope idempotency key remains the sole command key for both operations.
The request messages do not duplicate it.

Both operations return `ArtifactOperationResult`. Import returns a canonical
`Artifact`; open returns a bounded `ArtifactOpenReceipt` recording the artifact
ID, exact content version, and the closed status `Opened`, `Failed`, or
`InterruptedNeedsReview`. `Failed` additionally carries exactly one closed,
path-free failure code; opened and uncertain receipts carry none. A replayed
failure or uncertain open cannot silently launch another side effect.

`Artifact` field 12 optionally projects the current content version. Existing
`Available = 1` and `Deleted = 2` state numbers remain stable, while
`Unavailable = 3` represents metadata for which no content may be consumed.

The source path is accepted only in `ImportArtifactRequest`. No response or
canonical artifact contains it. Storage paths, object locators, and content
digests remain daemon-private and are absent from every request result and
read projection. Import fields are revalidated as untrusted boundary input
despite the trusted Electron-main selection step.

The selected path is also excluded from the durable request fingerprint: an
unkeyed path hash would remain a guessable persistence leak. Exact replay is
bound to the idempotency key plus logical destination metadata and never
consumes a newly supplied path. Volatile Files readiness is evaluated only
after exact journal resolution, so a later platform outage cannot hide an
already durable terminal result.

Epochs 0 through 13 are rejected so an older peer cannot silently interpret a
different operation or projection set.

## Consequences

- The renderer can request import and open without choosing durable storage
  identity or receiving filesystem authority.
- Import success is represented by the same canonical artifact shape used by
  Get/List; open success and exact replay use a path-free receipt.
- Unavailable, deleted, stale-version, and invalid requests fail closed.
- Diagnostics, durable commands, search records, responses, and renderer state
  must never retain the ephemeral source path or a deterministic derivative.
- Database and filesystem adapters remain responsible for durable byte
  publication, digest verification, quotas, and interruption recovery.
- Epoch 15 adds exact local-copy removal and retention recovery without changing
  these import/open contracts; see
  [ADR 0024](0024-daemon-owned-artifact-removal-and-retention.md).

## Rejected alternatives

### Reintroduce a public storage path

A path is an implementation detail and ambient filesystem capability. It also
breaks relocation and immutable-version semantics.

### Reuse generic artifact metadata mutation

Import is a bounded file-ingestion side effect, not permission to author
canonical size, availability, version, digest, or storage identity.

### Put a second command key inside each request

The authenticated envelope already supplies the idempotency key. Two keys
create conflicting replay identities and ambiguous recovery behavior.
