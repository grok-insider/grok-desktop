# ADR 0024: Daemon-owned artifact removal and retention

- Status: Accepted, Linux implementation pending release qualification
- Date: 2026-07-12

## Context

Epoch 14 and schema 17 introduced daemon-owned immutable artifact content, but
offered no way to reclaim it. Hiding metadata without a durable content
retention journal would either leak quota forever or risk reporting success
before private namespace deletion became durable. Reusing the retired generic
artifact delete operation would also restore renderer authority that epoch 11
permanently removed.

Removal must stay exact and path-free, survive cancellation and restart, remove
corrupt daemon-owned content safely, and never replay a different artifact or
version. It must also distinguish a canonical tombstone from completion of the
daemon's local cleanup. Linux namespace deletion cannot revoke a descriptor
already handed to another application and must not be presented as physical
media erasure.

## Decision

Protocol epoch 15 adds the closed `RemoveArtifact` operation. It does not reuse
the permanently reserved generic `delete_artifact` producer. The caller supplies
only an artifact ID, expected revision, expected current content version, and
the envelope idempotency key. Revision and content version must be the same
nonzero exact value. No path, digest, storage name, retention policy, or target
version is caller-controlled.

Schema 18 adds immutable per-version retention rows and an exact removal command
journal. Reservation is one SQL transaction which:

1. persists the normalized command fingerprint;
2. verifies the exact Available artifact and every immutable version;
3. changes the current projection to a Deleted tombstone;
4. moves every `Retained` version to `PurgePending`; and
5. takes the single global active-removal slot.

The tombstone immediately prevents new opens and releases artifact-count quota.
Byte quota remains charged for both `Retained` and `PurgePending` rows. A version
releases its bytes only after the storage adapter proves durable namespace
absence and the daemon commits `Purged`. The removal command becomes
`Committed` only when every version is `Purged`; version metadata remains
immutable for audit and exact replay.

Pending work is live-recoverable. Exact same-key retry resumes purge, retention
marking, and commit under an application single-flight guard. Versions are read
and processed in bounded pages of 100, so a valid long history is never
permanently rejected for exceeding one pass. A timeout or failure after unlink
may re-prove absence and continue; a commit failure after all rows are Purged
does not repeat storage I/O. When a request has a durable tombstone but cannot
finish cleanup in its response budget, the daemon returns a path-free
`ArtifactRemovalPendingReceipt` containing the exact tuple and canonical
tombstone. The response envelope echoes the command key, and one daemon-owned
background task continues recovery with bounded exponential retry. Terminal
replay returns the canonical removed artifact without storage I/O.

Electron and the renderer preserve that distinction. A pending receipt removes
the tombstoned card from the active Library but announces that private cleanup
is still continuing, retains the exact command key, and performs only three
bounded same-key presentation reconciliations. A Deleted workspace projection
alone is not proof of committed cleanup and cannot clear the key or be reported
as terminal success. Only an exact `removed_artifact` replay is terminal.

On Linux, purge derives the fixed shard and object name from canonical artifact
identity, opens it relative to retained private directory descriptors with
`O_NOFOLLOW`, and requires a current-user-owned regular `0600` single-link
entry. It compares the open descriptor's device/inode identity with the named
entry immediately before descriptor-relative unlink, verifies link count zero
on the retained descriptor, and synchronizes the shard and objects directories.
Content digest and size are deliberately not preconditions for deletion: a
bit-flipped daemon-owned object must remain removable. A symlink, hard link,
wrong owner, wrong mode, non-regular entry, identity substitution, or unreadable
namespace fails closed and remains pending.

`Purged` means the deterministic entry is durably absent from Grok Desktop's
private namespace. A descriptor previously transferred to a portal or another
application can keep the unlinked inode readable and allocated until that
application closes it. The product does not promise descriptor revocation,
secure overwrite, or immediate physical-block reclamation.

Startup recovers incomplete imports, open certainty, and removals before Files
becomes available. Private storage readiness is independent from OpenURI portal
readiness: import/removal recovery can run when local open is unavailable, while
new opens still fail closed. Unsupported platforms and unqualified roots expose
no new removal authority; exact known terminal or pending commands remain
path-free and queryable across a volatile readiness outage.

## Consequences

- Renderer compromise cannot choose a filesystem target or retarget a stale
  command. Main/preload accept only the closed exact tuple.
- A corrupt private object no longer wedges the global removal slot merely
  because its bytes differ from the immutable digest.
- A definitive pre-reservation rejection can be discarded by the renderer;
  transport uncertainty retains the same key, while a typed pending receipt
  transfers cleanup ownership to the daemon.
- Original selected source files are never modified. Files already opened in
  another application may remain accessible there after Grok's copy disappears.
- Windows and macOS removal remain unavailable until their private content
  adapters satisfy equivalent identity, durability, cancellation, and recovery
  requirements.
- Automatic retention schedules, grace periods, secure erase, export, provider
  upload lifecycle, malware policy, and multi-device deletion are outside this
  decision.
