# ADR 0005: Linux broker identity and daemon proof-of-possession

- Status: accepted (implementation pending)
- Date: 2026-07-12
- Extends: `docs/platform/adr/0001-privileged-guest-contract.md`,
  `docs/platform/adr/0002-authenticated-guest-channel.md`,
  `docs/platform/adr/0004-linux-qemu-kvm-managed-execution.md`

## Context

On Windows the VM service authenticates named-pipe clients with SID, logon
session, PID, creation time, and packaged MSIX identity. Clients request
identification-only tokens. Guest control further requires daemon
proof-of-possession (PoP), which is still incomplete on Windows.

Linux has no MSIX package family. A unix domain socket alone is not enough: any
process running as the same UID can connect unless the broker proves the peer
is the **exact packaged daemon binary** and a **proof-bearing session**.

## Decision

1. **Transport:** the privileged Linux broker listens on a fixed path under a
   service-owned directory (not a world-writable abstract name). Mode and
   ownership admit only the service and the intended desktop user class as
   defined by packaging (typically root-owned service, group or ACL for the
   installing user).
2. **Peer authentication at accept:** use `SO_PEERCRED` (uid, pid, gid). Reject
   root-as-client shortcuts that would let arbitrary root helpers impersonate
   the product without path proof.
3. **Binary identity:** resolve the peer executable through
   `/proc/<pid>/exe` (or equivalent) at accept and again at sensitive ops.
   Require an exact match to the installed `grok-daemon` path shipped by the
   package (or a channel-scoped allowlist of digests). PID reuse races fail
   closed when start-time or inode identity diverges.
4. **Lifecycle ops** (`EnsureImage`, VM CRUD, `AttachWorkspace`) may proceed for
   an identity-qualified peer under rate limits. They never grant ambient guest
   control.
5. **GuestControl grant:** requires an additional **per-install
   proof-of-possession** key held only by the daemon (outside renderer,
   integrations, and broad child environments). Challenge-bound requests cover
   operation, payload digest, idempotency key, and deadline. SID/UID-only
   authority is never enough for guest control.
6. **No raw guest endpoints:** the broker never returns a vsock fd, QMP socket,
   or guest shell to the client. All guest control is service-mediated, matching
   ADR 0002’s intent on Windows.
7. Development unpackaged clients fail closed for production grant-bearing
   sessions. Explicit debug paths, if any, must be compile-time gated and never
   ship in release builds.

## Consequences

- Packaging must install a stable daemon path and service unit layout before
  Work can be Available.
- PoP material is daemon-local durable state under OS vault or service-assisted
  storage with the same secrecy rules as other credentials.
- Same-UID malware can still attempt DoS against the socket; residual risk is
  bounded availability, not guest authority, when PoP and path checks hold.
- Windows and Linux share the **grant model** (identity + PoP + journal) even
  though transports differ.

## Non-goals

- Trusting D-Bus caller metadata alone as peer proof.
- Allowing the renderer or Electron main to hold PoP keys.
- Auto-enabling GuestControl because KVM is present.
