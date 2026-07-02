# ADR 0021: Remove artifact storage paths from projections and search

- Status: Accepted
- Date: 2026-07-11

## Context

Artifact metadata retains a daemon-relative storage path for the current
metadata-only implementation. Electron already omitted that field from its
renderer-facing object, but protocol `Artifact` responses still carried it.
Artifact search also indexed the path as its full-text body and returned it as
the result snippet. Consequently a renderer-connected client could observe the
path directly over raw daemon IPC or infer it with path-token search queries.

The relative path is an implementation detail owned by the daemon. A display
name, media type, byte count, lifecycle, and metadata revision are sufficient
for the current read-only Library projection. Storage identity must not become
a renderer capability or a searchable disclosure channel.

## Decision

Protocol epoch 13 permanently reserves `Artifact` field 5 and the name
`relative_path`. Canonical artifacts retain the path internally, but
`artifact_to_wire` intentionally discards it. Epochs 0 through 12 are rejected
by epoch-13 peers, so an older daemon or renderer cannot silently retain the
previous response shape.

SQLCipher schema 16 replaces artifact search triggers with metadata-only
versions, deletes and canonically rebuilds existing artifact search-cache rows
with an empty body, and then rebuilds the FTS5 index from that external-content
table so stale internal postings are removed. Runtime SQL artifact matching is
independently restricted to the FTS title column, requires the external body to
equal an empty canonical body, and returns an empty snippet. Thus a later
desynchronized body posting cannot become a path-token oracle. The memory
adapter has the same title-only contract. This removes both direct snippets and
the query oracle over rows written before the upgrade.

The migration is forward-only and transactional. If cache reconstruction
fails, the trigger replacement, cache deletion, migration-history row, and
`user_version` change all roll back. Restart retries the same migration.

This decision does not claim that existing artifact metadata proves local file
identity or content availability. Import, immutable content versions, quotas,
digest verification, and safe local opening require a separate typed producer,
store, filesystem adapter, protocol epoch, and schema migration.

## Consequences

- Get/List artifact projections contain no storage path at the Protobuf,
  Electron-main, preload, or renderer boundary.
- Artifact names remain searchable; storage-directory and path-only tokens do
  not match, and artifact result snippets are empty.
- Existing schema-15 search rows are scrubbed during migration rather than
  merely hidden at presentation time.
- A stale or damaged FTS posting cannot make an artifact match a body-only
  token because runtime artifact queries are title-column-only.
- Field 5 can never be reused for a different `Artifact` meaning.
- A future ingestion implementation must not reintroduce arbitrary paths into
  renderer state, search documents, logs, or public artifact responses.

## Rejected alternatives

### Continue dropping the path only in Electron main

The daemon protocol is itself a security boundary. A trusted consumer choosing
not to copy a field does not justify sending the field to every authenticated
local IPC client.

### Return an empty snippet but retain path indexing

That leaves a query oracle: a client can probe candidate path tokens and learn
whether they match an artifact even if the matched text is not displayed.

### Delete only current cache rows

The old insert and update triggers would immediately reintroduce path bodies.
Schema 16 replaces both triggers and rebuilds from canonical ownership-checked
artifact rows in one transaction.
