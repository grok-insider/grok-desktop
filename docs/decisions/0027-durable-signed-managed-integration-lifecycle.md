# ADR 0027: Durable signed managed-integration lifecycle

- Status: Accepted; IPC mutations remain unavailable pending release qualification
- Date: 2026-07-12

## Context

The earlier Wisp prototype trusted comparison signing bytes supplied beside a
fixture and stored lifecycle authority in JSON. That could not prove that every
field and file later used was signed, could not make publication and recovery
transactional, and could not safely survive crashes or concurrent retries.
Wisp is separately versioned and out of process; it must never become renderer
code or inherit Chat/Work authority.

## Decision

Schema 22 makes the daemon's SQLCipher store the sole lifecycle authority. The
service derives canonical signing bytes only from a deny-unknown-fields parsed
manifest and catalog. Catalog trust is independently bound at build time;
release bundles cannot nominate their own root. The signed inventory binds the
exact normalized path, executable bit, file count, aggregate size, and digest
of every published file. Links, identity swaps, extra files, unsupported
protocol versions, oversized inputs, and non-canonical manifests fail closed.

Publication uses a private daemon namespace and retained file identities. The
daemon stages an exact release, writes the durable intent, publishes atomically,
then acknowledges the result. Public state projects only the last acknowledged
release. Startup recovery is bounded and idempotency-linked: it resumes the
exact known stage, removes only owned residue, and refuses unknown/foreign
stages. Update and rollback preserve exact digest lineage and optimistic
revision checks. Durable mode neither reads nor writes the legacy JSON state.

The lifecycle port exposes only install, update, and rollback for the fixed
first-party integration identity. It grants no tool, workspace, shell, provider,
or renderer authority. Linux publication is implemented with private-directory,
`O_NOFOLLOW`, identity revalidation, and atomic no-replace semantics. Windows
remains explicitly unqualified until an equivalent audited retained-handle
publication boundary exists.

IPC epoch 21 continues to return mutation unavailability. Re-enabling the
product controls requires independently pinned production catalog roots,
packaged signed bundles, platform publication qualification, compatibility
tests, and isolated native UI qualification. Fixture keys and debug bundle
overrides are test evidence only.

## Consequences

- A valid signature covers exactly the manifest and files the daemon uses.
- A crash cannot make an unacknowledged release appear installed.
- Repeated commands cannot publish a different release under the same
  idempotency identity.
- Renderer compromise cannot install arbitrary integrations or inject code.
- Local tests can qualify recovery mechanics, but cannot manufacture release
  publisher trust or the missing Windows publication primitive.

## Rejected alternatives

- Bundle-provided or self-signed catalog roots.
- JSON as authoritative lifecycle state.
- Best-effort copy/rename without retained identity checks.
- Loading integration code into Electron or the renderer.
