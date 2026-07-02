# ADR 0023: Linux private artifact content and descriptor-based local open

- Status: Accepted, Linux implementation pending release qualification
- Date: 2026-07-12

## Context

Protocol epoch 13 removed daemon storage paths from artifact projections, and
epoch 14 adds one trusted native-selected file import plus exact-version local
open. The renderer must never receive or choose a host path. A source path is
ephemeral input from Electron main's native picker to the nonce-paired daemon;
it must not enter durable state, responses, logs, search, or model context.

Import is a local side effect even though it is idempotent. The daemon must
persist intent before copying, distinguish prepared bytes from published
bytes, recover without the original source, and never interpret a partial or
changed file as the selected content. Local open launches another application
and is non-idempotent: an interrupted dispatch is uncertain and must never be
replayed automatically.

## Decision

The application owns `Prepared -> ContentReady -> Committed/Failed` import
journals and `Prepared -> Dispatching -> Opened/Failed` open journals.
Restart changes an unfinished open dispatch to `InterruptedNeedsReview`.
Artifact versions are immutable and contain only artifact ID, version, SHA-256,
media type, byte count, and creation time. No source or storage locator is
stored.

On Linux, `grok-artifact-storage` opens a pre-qualified owner-private data
directory and retains descriptors for fixed `0700` object and staging
directories. Import:

1. opens the selected source read-only with `O_NOFOLLOW`, `O_CLOEXEC`, and
   `O_NONBLOCK`;
2. requires a regular file no larger than 64 MiB;
3. copies in 64 KiB chunks to a unique `0600`, single-link staging file while
   hashing SHA-256 and enforcing the application deadline;
4. rechecks source device, inode, size, modification time, and change time at
   EOF;
5. synchronizes the staged file and records its digest/size as ContentReady;
6. publishes with descriptor-relative `renameat2(RENAME_NOREPLACE)` into a
   deterministic name derived from artifact ID and content version, then
   requires the shard, staging, and newly-created parent entries to synchronize;
   and
7. commits version metadata and the Available artifact in one SQLCipher
   transaction.

Preparation and publication share one absolute 30-second I/O deadline. The IPC
boundary requires additional terminalization reserve and runs accepted artifact
operations in detached daemon tasks, so an outer response timeout cannot cancel
durable ownership mid-transition. Source copy, content status, publication, and
cleanup run on blocking workers; prepare, publish, and discard share one exact
artifact/version staging gate. Cancellation is atomically observed before a
namespace mutation, and digest validation checks cancellation/deadline between
every returned chunk. A caller timeout returns without waiting for cleanup and
retains Prepared or ContentReady ownership for exact recovery. A worker staging
guard unlinks only the exact private entry. A terminal failure is not persisted
until deterministic staging cleanup and a staging-directory sync succeed;
missing-entry retries still sync the directory. Quota rejection retains the
Prepared slot while cleanup is pending. Directory-sync uncertainty remains
ContentReady and resolves through exact publication replay rather than being
mislabeled as integrity failure.

Prepared restart recovery deletes the deterministic reserved staging entry;
ContentReady recovery verifies the full recorded digest and either publishes
and commits exactly once or fails without consulting the original source. A
corrupt published object that cannot yet be quarantined keeps Files degraded
and retains its journal ownership instead of releasing unaccounted bytes.
If the immutable destination is exact but deterministic staging is corrupt,
exact replay must remove and sync that staging entry before it may report
`AlreadyPublished`; cleanup uncertainty keeps ContentReady. Integrity as well
as transient/deadline recovery starts the rest of the daemon in Files-limited
mode.
Project/global byte quotas and the single active operation slots are rechecked
inside database transactions.

Before local open, the adapter reopens the deterministic object relative to a
held directory descriptor and validates owner, `0600` mode, regular-file type,
single-link identity, size, timestamps, and the full recorded SHA-256. It then
passes only that read-only descriptor to
[`org.freedesktop.portal.OpenURI.OpenFile`](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.OpenURI.html#org-freedesktop-portal-openuri-openfile)
with writable access disabled. The portal contract accepts an FD directly;
`file://`, `/proc/self/fd` launch paths, shell commands, and Electron
`shell.openPath` are not used.

Startup performs a bounded non-launching OpenURI interface/version probe and
advertises Files only when both private storage and `OpenFile` are reachable.
Epoch 15 separates those readiness facts internally so import/removal recovery
can use qualified storage while the portal is absent; it does not make local
open or the combined Files UI capability available without `OpenFile`.
Once portal request dispatch begins, any transport or response ambiguity maps
to `InterruptedNeedsReview`; only failures proven to precede the external side
effect may become stable `Failed` receipts. Transient content recovery starts
the rest of the daemon in Files-limited mode with the journal unchanged.

Other platforms and unsafe/unqualified data roots fail closed before a new
import/open command is reserved. Exact terminal commands are resolved before
that volatile readiness gate so a later platform outage cannot hide their
durable replay result. The Files capability becomes Available only when both
private content storage and native open are configured.

## Consequences

- Renderer compromise cannot select, learn, search, or reopen an arbitrary
  daemon path.
- Legacy schema-16 artifacts become Unavailable metadata; migration never
  follows or deletes their old path strings.
- Exact command replay returns the durable result and never rereads a selected
  source or relaunches an application.
- The selected source path and every deterministic derivative of it are omitted
  from durable command fingerprints. Reusing an exact key with the same logical
  destination metadata returns the original result without consuming a newly
  selected path.
- A 64 MiB full digest is intentionally paid during import and again before
  open. The bound keeps this verification finite and avoids trusting mutable
  filesystem metadata as content identity.
- Linux cannot portably preempt one regular-file syscall that is already
  executing inside the kernel. Such a call may outlive the application
  deadline on a broken filesystem, but it occupies only a blocking-pool thread;
  the caller returns on its Tokio deadline and durable journal ownership stays
  pending. No compatibility fallback performs the operation on a Tokio worker
  or releases the staging gate early.
- Private object names are infrastructure details derived at use time. They may
  change in a future storage adapter without changing domain or IPC contracts.
- Same-user process attacks remain outside the filesystem permission boundary;
  daemon non-dumpability, nonce-paired IPC, private directories, and
  descriptor-relative use reduce but do not claim protection from a fully
  compromised user session.
- Linux portal behavior still requires representative Wayland/X11, chooser,
  default-handler, cancellation, and packaged-build qualification. Windows and
  macOS remain unavailable until separately audited descriptor/handle brokers
  exist.
- Epoch 15 removal is specified separately in
  [ADR 0024](0024-daemon-owned-artifact-removal-and-retention.md). It does not
  weaken full-digest verification for import recovery or local open; only
  deletion of an exact qualified private entry is content-agnostic so corrupt
  owned bytes remain reclaimable.
