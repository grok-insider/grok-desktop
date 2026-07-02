# ADR 0010: Daemon-owned workspace search routing

- Status: Accepted
- Date: 2026-07-11

## Context

The daemon already owns bounded full-text search over canonical projects,
threads, messages, artifacts, and automation definitions. Electron did not use
that operation: the command palette filtered a partial renderer snapshot, so it
could miss persisted message content and could disagree with SQLCipher search
ordering. The existing wire result also lacked the owning thread for message
hits, which made a canonical result impossible to navigate without another
unbounded lookup or renderer inference.

Search content is untrusted local data. Queries and results therefore need
explicit byte, count, pagination, identifier, and text bounds at the renderer,
Electron, daemon, application, and store boundaries. Search must remain
read-only and must not expose paths, provider data, credentials, or a generic
database query surface.

## Decision

Protocol epoch 6 adds `thread_id` to `WorkspaceSearchHit` and rejects epochs 1
through 5 before dispatch. The generated package remains the canonical v1
schema family; the mandatory envelope epoch carries compatibility.

- `WorkspaceService::search` remains the only application search policy. It
  accepts a printable 1–256-byte query, an optional validated project scope,
  an offset no greater than 10,000, and a page size from 1 through 100.
- Stores return the canonical owning thread for thread and message hits and for
  thread-owned artifacts. SQLCipher derives that route from canonical entity
  tables rather than adding renderer-controlled index metadata.
- Electron validates result count, unique kind/id identity, identifiers, title
  and snippet bounds, timestamps, kind-specific thread routing, and the exact
  continuation cursor before returning a secret-free DTO to the renderer.
- The preload bridge exposes only the bounded read operation. Renderer input
  cannot select an endpoint, submit SQL/FTS syntax directly, or mutate the
  search index.
- The command palette debounces canonical searches, ignores stale responses,
  and presents loading, failure with retry, no-result guidance, and bounded
  more-result feedback. Empty-query recents remain a presentation-only view of
  the already-authorized workspace snapshot.

## Consequences

- Results now include persisted message content and share daemon/store ordering
  across restarts instead of depending on renderer caches.
- Thread and message results navigate to an exact conversation without exposing
  message storage details or requiring a renderer-owned lineage map.
- Artifact results may route to a conversation when one exists, but artifact
  import, versioning, export, and local-open remain unavailable until their
  separate bounded content and broker contracts exist.
- Epoch-5 Electron and daemon binaries fail closed instead of silently accepting
  search results without routing context.
