# ADR 0006: Linux virtio guest image and signed catalog

- Status: accepted (implementation pending)
- Date: 2026-07-12
- Extends: `docs/platform/adr/0001-privileged-guest-contract.md`,
  `docs/platform/adr/0004-linux-qemu-kvm-managed-execution.md`

## Context

The repository builds a reproducible NixOS utility guest as a **Hyper-V VHDX**
(`hypervImage`) for Windows HCS. The guest baseline already disables DHCP,
general NIC, SSH, sudo, and console login, and hosts the integration runner over
AF_VSOCK.

Linux QEMU/KVM cannot boot that Hyper-V-first configuration as the sole image
flavor. A second, virtio-oriented image product is required while preserving the
same security baseline and runner contracts.

## Decision

1. **Two release image families** share the `grok-guest` module baseline:
   - **Windows:** Hyper-V guest modules + VHDX (existing).
   - **Linux host:** virtio (virtio-blk, virtio-vsock, virtio-9p and/or
     virtio-fs as selected in ADR 0007) + **qcow2 or raw** disk published for
     `EnsureImage`.
2. **`EnsureImage`** on the Linux broker:
   - stages under a service-owned root only;
   - verifies size and SHA-256 before an image becomes selectable;
   - accepts only **signed catalog** metadata with anti-rollback sequence
     watermark (same trust posture as Windows guest catalogs);
   - never trusts a caller-supplied absolute path as provenance.
3. **Architecture pins:** x86_64 and aarch64 Linux images are independent
   artifacts with matching catalog entries when both are claimed.
4. **Guest control readiness** still requires guest channel v2 provisioning
   (fresh boot ID, channel key, nonces, HMAC frames) mediated by the broker.
   Image presence alone does not authorize control.
5. **Development images** may be built from the flake for engineering; release
   builds require clean locked inputs, published digests, and channel keys.
   Unsigned or `simulated` backends never qualify Work Available.

## Consequences

- Flake packaging gains a virtio image output alongside `hypervImage`.
- Download size and first-run image ensure become Linux Work prerequisites.
- Catalog sequence and signing keys must be recorded in the Linux release
  evidence pack (mirroring Windows release notes requirements).
- Guest module changes must be regression-tested on **both** image flavors when
  shared.

## Non-goals

- Shipping a general-purpose desktop VM image with login or package manager UX.
- Allowing users to point EnsureImage at arbitrary qcow2 files without catalog
  verification.
- Embedding secrets or long-lived channel keys in the image.
