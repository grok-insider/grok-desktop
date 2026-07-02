# Claude Cowork on Windows: verified reference points

- Research date: 2026-07-10
- Purpose: record externally verifiable design inputs, not reverse-engineer or
  reproduce Anthropic implementation details

## Confidence labels

- **Verified** means the behavior is stated in current Anthropic or Microsoft
  documentation.
- **Observed** means it can be established from a released package or runtime
  without bypassing access controls.
- **Decision** means it is a Grok Desktop design choice. It must not be
  presented as an Anthropic implementation detail.

## Verified Cowork architecture

Anthropic documents two local execution environments. The agent loop runs on
the device and owns conversation handling, connected-folder file operations,
web fetches, and local plugin MCP servers. Shell commands and generated code run
inside a dedicated Linux VM. Windows uses Hyper-V, while macOS uses
Virtualization.framework and Linux uses QEMU. The VM adds network egress
filtering, syscall restrictions, and per-session user isolation.

Sources:

- [Claude Cowork desktop architecture overview](https://support.claude.com/en/articles/14479288-claude-cowork-desktop-architecture-overview)
- [Get started with Claude Cowork](https://support.claude.com/en/articles/13345190-get-started-with-claude-cowork)

The Windows distribution uses MSIX for enterprise deployment, supports x64 and
ARM64 packages, requires the Windows Virtual Machine Platform feature for
Cowork, and requires elevation for the complete feature set. The product has a
separate VM service; Anthropic's troubleshooting material names
`CoworkVMService` and `CoworkVMServiceStore`.

Source: [Deploy Claude Desktop for Windows](https://support.claude.com/en/articles/12622703-deploy-claude-desktop-for-windows)

Enterprise policy can control updates, workspace folders, local MCP servers,
desktop extensions, and secure VM features. Machine policy overrides user
policy. Anthropic also documents OpenTelemetry export for prompts, tool/MCP
calls, file access, approvals, and model request metrics, with explicit warnings
that event payloads can be sensitive.

Sources:

- [Enterprise configuration for Claude Desktop](https://support.claude.com/en/articles/12622667-enterprise-configuration-for-claude-desktop)
- [Monitor Cowork with OpenTelemetry](https://support.claude.com/en/articles/14477985-monitor-claude-cowork-activity-with-opentelemetry)

## What is not verified

Anthropic's public documentation does not specify the complete desktop UI
framework, internal state model, IPC protocol, database, update implementation,
or source layout. Grok Desktop therefore does not claim that its Electron,
React, Rust, Protobuf, SQLCipher, NixOS, or HCS choices match private Anthropic
implementation details. Those are independent decisions evaluated against our
requirements and maintained behind replaceable boundaries.

## Grok Desktop decisions informed by the reference

1. Use a sandboxed Electron renderer only as presentation. Keep durable state,
   secrets, policy, provider calls, and execution in a separately supervised
   Rust daemon.
2. Use a narrow privileged Windows service for HCS lifecycle operations. Do not
   expose generic HCS documents, command execution, PowerShell, registry, or
   arbitrary filesystem methods.
3. Use a reproducible Linux utility VM for strong Work execution. Share host
   workspaces read-only and stage guest writes for explicit review and commit.
   This is intentionally stricter than the minimum behavior described in the
   Cowork overview.
4. Keep Chat unprivileged. Never make VM failure silently expand host
   authority. Strong local tools become unavailable in Limited Mode.
5. Ship a signed MSIX family for Windows x64 and ARM64, with one declared update
   owner at a time: either the application channel or enterprise management.
6. Treat telemetry as a separate, opt-in export boundary with redaction and
   field-level sensitivity classifications. Local audit history is not the
   same thing as remote telemetry.

## Windows caveats carried into qualification

- Virtual Machine Platform may be disabled by firmware, Windows features, or
  enterprise policy. Readiness must be checked before offering strong Work.
- Service installation and feature enablement require elevation. A
  non-elevated installation must remain useful but must not pretend to provide
  VM isolation.
- MSIX package identity, publisher, and version continuity determine whether
  updates are accepted. x64 and ARM64 release families need the same identity
  and independent qualification.
- Anthropic documents cross-volume failures when package/AppData redirection
  makes an image move cross devices. Grok Desktop writes downloads directly to
  service-owned staging on the final volume and uses copy, verify, fsync, and
  atomic publish rather than relying on cross-volume rename.
- Enterprise deployment must choose between MDM-owned versions and app-owned
  updates. Running both creates conflicting registrations and unpredictable
  rollback behavior.
- VM isolation reduces host visibility. Enterprise diagnostics need explicit
  guest lifecycle and broker audit events without recording prompts, file
  contents, typed text, screenshots, tokens, or credentials by default.
- HCS compute-system handles are ephemeral and are invalid after the VM stops.
  Service restart and VM recreation therefore require an explicit ownership
  recovery and guest-channel rekey protocol; persisting an old session key is
  not an acceptable shortcut.
- Hyper-V sockets are raw streams identified by VM and service GUIDs, require a
  registered integration service, and require Linux guest support for
  `CONFIG_VSOCKET` and `CONFIG_HYPERV_VSOCKETS`. Framing, authentication,
  replay defense, authorization, and lifecycle recovery remain application
  responsibilities.

Microsoft references for the packaging decisions:

- [MSIX overview](https://learn.microsoft.com/en-us/windows/msix/overview)
- [MSIX automatic update and repair](https://learn.microsoft.com/en-us/windows/msix/app-installer/auto-update-and-repair--overview)
- [MSIX package update constraints](https://learn.microsoft.com/en-us/windows/msix/app-package-updates)
- [Host Compute System overview](https://learn.microsoft.com/en-us/virtualization/api/hcs/overview)
- [Build a Hyper-V socket integration service](https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/make-integration-service)
