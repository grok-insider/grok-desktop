# ADR 0019: Private atomic backup publication

- Status: Accepted
- Date: 2026-07-11

## Context

The SQLCipher online-backup path previously checked whether a destination
existed and then used a replacing filesystem rename. A competing destination
created between those operations could be overwritten. Staging directly in a
shared export directory would also let another principal swap pathname-opened
SQLite material before publication.

Publication must preserve an existing target, expose only a complete encrypted
snapshot, and fail closed when the operating system cannot provide the required
identity and no-replace guarantees. The live database and its backups are
owned by the same operating-system account, so this contract protects against
other users and cooperative concurrent publishers. It does not claim that an
application can defend its user-owned files from a malicious process already
running as that same user.

## Decision

On Linux, backup creation requires an existing target parent that is a
non-symlink directory owned by the effective user with exact mode `0700`. Its
canonical ancestor chain must be root- or current-user-owned and not
group/other writable, except for a root-owned sticky ancestor such as `/tmp`.

The adapter opens and retains the validated parent descriptor, then creates a
unique `0700` staging directory and `0600` snapshot relative to it and retains
those descriptors too. The backup connection uses an in-memory rollback
journal during the copy and is finalized to exact `DELETE` journal mode. A
separate read-only keyed connection performs cipher and structural integrity
checks without invoking the normal WAL-enabling database opener. Immediately
before publication, descriptor and directory-entry identities, ownership,
permissions, link count, the canonical parent binding, and absence of the
destination plus its `-wal`, `-shm`, and `-journal` sidecars are rechecked.

Publication uses Linux `renameat2(RENAME_NOREPLACE)` between the retained
staging and parent descriptors. There is no replacing-rename, hard-link, or
copy fallback. Retained descriptors and identity rechecks detect namespace
changes observed before commit; this is not a claim of same-user race proof.

The exclusive rename is the commit point. Failures before it remove the
snapshot and staging directory only when their entries still match the retained
identities; known SQLite sidecar names are unlinked relative to the retained
private staging descriptor and symlink targets are never followed. The target
remains absent or untouched.

A reported rename error is reconciled from the retained snapshot identity at
both source and target. A matching target with a missing source is committed,
even if the filesystem reported an error. A matching source without a matching
target proves no commit; an outcome that cannot be proven either way returns the explicit
`BackupPublicationUncertain` result and requires inspection rather than blind
retry. Once commit is established, parent synchronization and removal of the
now-empty staging directory are best-effort; their failure means the backup is
committed with durability unknown and must not produce an apparently retryable
error after a backup is already visible.

Apple, Windows, and other platforms fail with a fixed unsafe-target result
before key or filesystem work. Apple remains unavailable pending a native
descriptor/SQLite/path-binding and crash qualification. Windows remains
unavailable until `grok-windows-acl` provides and Windows workers qualify a
narrow handle-relative `SetFileInformationByHandle(FileRenameInfo)` operation
with `ReplaceIfExists=false`, verified parent and source handles, private ACLs,
reparse rejection, and filesystem/crash behavior. Ordinary path-based rename
is not an accepted compatibility fallback.

## Consequences

- A destination created at the publication race wins and its bytes are never
  replaced.
- Concurrent publishers produce exactly one complete winner.
- This API is a private backup publication contract, not a general export to a
  shared directory.
- A malicious same-user process can change any user-owned database or backup;
  preventing that requires a stronger account/isolation boundary and, for the
  pre-publication SQLite path, a qualified descriptor-aware VFS.
- Linux behavior is locally exercised. Apple, Windows, and other platforms
  fail closed pending native implementation and qualification.

## Rejected alternatives

### Check existence immediately before ordinary rename

The check and replacing rename remain separate operations, so the overwrite
race is unchanged.

### Publish with a hard link and then unlink staging

Although link creation is no-replace on supported filesystems, a crash can
leave two names and the path-based Windows operation does not satisfy the
reparse-at-use boundary.

### Copy when exclusive rename is unsupported

A copy can expose partial data and changes the durability and identity
contract. Unsupported platforms and filesystems fail closed instead.
