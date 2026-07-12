# ADR 0004: Linux QEMU/KVM managed execution backend

- Status: accepted (implementation pending)
- Date: 2026-07-12
- Extends: `docs/decisions/0003-managed-execution.md`,
  `docs/platform/adr/0001-privileged-guest-contract.md`
- Product contract: `docs/quality/linux-ga.md`

## Context

Strong Work on Windows uses HCS/VirtualMachinePlatform and a LocalSystem
service. Linux is already a first-class host for the daemon, BYOK Chat, pinentry,
and private artifacts, but the only non-Windows VM service implementation is a
**contract simulator** that reports `simulated: true` and is not an isolation
boundary.

External research notes that other products use QEMU on Linux. This repository
had no accepted Linux hypervisor path. Host-exec sandboxes (bubblewrap/seccomp
on the desktop host) are rejected as a Work substitute by product invariants.

## Decision

1. On Linux, strong Work uses a **privileged host broker** (working name
   `linux-vm-service`) that drives a **closed QEMU/KVM** machine template.
   Prefer direct QEMU with a fixed argv/config surface over unrestricted libvirt
   domain XML from callers. Libvirt may wrap QEMU later only if the same closed
   template is enforced server-side.
2. The broker exposes the **same narrow operation set** as the Windows service:
   `GetCapabilities`, `EnsureImage`, `CreateVm`, `StartVm`, `StopVm`,
   `DeleteVm`, `AttachWorkspace`, and authorization-gated `GuestControl`.
   There is no generic QEMU monitor, shell, or arbitrary device API.
3. Machine policy:
   - no general-purpose guest NIC;
   - host-brokered egress only when a purpose-specific proxy is later approved;
   - virtio-vsock for control (and later purpose sockets);
   - read-only workspace share (see ADR 0007);
   - bounded vCPU and memory;
   - fail closed when `/dev/kvm` is absent, blocked by policy, or nested
     virtualization is unavailable.
4. The in-guest stack reuses the NixOS utility guest baseline, integration
   runner, and guest channel v2 concepts. Image **flavor** becomes virtio-first
   (see ADR 0005); Hyper-V VHDX remains the Windows release image.
5. Missing or unhealthy isolation keeps Work, Shell, MCP, managed browser, and
   computer use **unavailable**. The product must never fall back to host tool
   execution.

## Consequences

- Full Linux GA requires a real privileged service install (e.g. systemd system
  unit), not only an unpackaged developer binary.
- Desktop packaging must eventually ship or download the signed guest image and
  depend on KVM-capable hosts for Work Available.
- Limited Mode remains the correct product state on machines without KVM.
- Implementation order: broker capabilities + EnsureImage + lifecycle before
  enabling `GuestControl` grants (see ADR 0005 identity and platform ADR 0003
  journal gateway).

## Non-goals

- Replacing Windows HCS with QEMU on Windows.
- Running Work tools under the user session with bubblewrap alone.
- Returning raw vsock endpoints or QMP sockets to Electron or the renderer.
