# ADR 0007: Linux workspace share and reviewed host commit

- Status: accepted (implementation pending)
- Date: 2026-07-12
- Extends: `docs/platform/adr/0001-privileged-guest-contract.md`,
  `docs/platform/adr/0004-linux-qemu-kvm-managed-execution.md`,
  `docs/platform/adr/0006-linux-virtio-guest-image-and-catalog.md`

## Context

Managed execution requires host content in the guest **read-only**, guest writes
in an isolated overlay, and host mutation only through a **daemon-reviewed**
commit. On Windows, HCS Plan9 shares implement the read-only attach path.

Linux QEMU needs an explicit share technology and the same commit authority
split. Writable host mounts from the guest are rejected by the threat model.

## Decision

1. **AttachWorkspace** on the Linux broker attaches a host directory into the
   guest as **read-only** using a closed technology choice:
   - preferred initial implementation: **virtio-9p** with mapped security model
     appropriate to RO export; or
   - **virtio-fs** if qualification shows clearer ownership and performance,
   chosen once in the broker template (not caller-selectable per request).
2. Paths are relative to fixed, service-owned workspace roots. The broker
   rejects traversal, absolute escapes, and unsafe reparse/symlink patterns at
   attach time and revalidates identity at use.
3. A running VM cannot change attachments. Detach requires stop/recreate policy
   consistent with Windows.
4. Guest writes land only under guest-owned state/overlay paths (existing guest
   policy under `/var/lib/grok-integrations` and workspace overlays). They never
   write through the RO share.
5. **Host commit** is a separate daemon application operation outside the VM
   service:
   - export a reviewed diff/artifact from the guest via service-mediated
     control;
   - present it in the desktop for explicit approval;
   - apply to the host only after durable approval and path revalidation;
   - deny leaves the host unchanged.
6. The VM service does **not** expose a “write host file” method. Ambient
   bidirectional mounts are forbidden.

## Consequences

- Product UX must include review-and-apply for Work file mutations.
- Share technology is part of the signed machine template and guest image
  modules (9p/fs drivers must match).
- Daemon, not QEMU user networking tricks, owns any future egress proxy.

## Non-goals

- SMB/NFS host mounts into the guest for Work.
- Letting integrations open host paths by absolute string.
- Auto-committing guest overlays without user/daemon approval.
