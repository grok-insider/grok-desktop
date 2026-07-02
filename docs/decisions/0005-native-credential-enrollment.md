# ADR 0005: Native credential enrollment boundary

- Status: Accepted, Windows and Linux implementations pending release qualification
- Date: 2026-07-10

## Context

An xAI API key must be entered by a person before the daemon can validate it
against the fixed official xAI origin and place it in the operating-system
credential vault. Passing the key through the React renderer, preload bridge,
or Electron main process would expose it to a broader UI attack surface.
Clearing component state after submission would not remove that exposure.

A separately launchable credential helper was evaluated and rejected. Any
prompt-capable executable that returns the entered value to its launcher needs
a second mutual-authentication and executable-identity protocol; a signed but
insufficiently authenticated helper becomes a convincing local phishing
surface. It also adds path replacement, spawn TOCTOU, orphan-process, and
cross-process plaintext-buffer failure modes.

## Decision

Credential enrollment uses a narrow platform adapter inside the trusted Rust
daemon. Electron may submit a non-secret enrollment intent, but neither
Electron main nor the renderer receives the entered value.

On Windows, Electron main derives the foreground native window handle and the
daemon invokes the audited Win32 credential UI adapter.

Before showing native credential UI, the adapter requires all of the following:

- the current executable is the exact packaged
  `resources/bin/grok-daemon.exe` layout;
- the daemon has an MSIX package full name and family;
- the HWND belongs to the exact packaged `Grok Desktop.exe` owner; and
- the owner process has the same package full name and family as the daemon.

The adapter keeps the qualified owner process open across the modal prompt and
rechecks HWND ownership immediately before the Win32 call. Entry is bounded to
printable ASCII, the password allocation is locked where Windows permits, and
volatile cleanup zeroes temporary buffers on every return path. The resulting
`SecretValue` remains inside application and adapter code for fixed-origin
validation and OS-vault persistence.

On Linux, the daemon launches a local pinentry process and exchanges bounded
Assuan commands over private child pipes. The secret is percent-decoded into a
zeroizing `SecretValue`; it never passes through Electron or a command-line
argument. The conversation has byte and time limits, cancellation is mapped to
a non-secret result, and the child is terminated and reaped after every
outcome. The default executable must canonicalize to a protected, root-owned
`pinentry` or `pinentry-*` binary beneath protected canonical ancestors. An
absolute operator override is accepted only in explicit debug/development
launches and is stripped by packaged Electron launches. Before spawn, the
adapter clears the child environment and reconstructs a bounded, validated
allowlist containing only local display, session-bus, runtime, Xauthority, and
locale values. Pinentry runs in its own process group; normal completion and
caller cancellation kill the group, and the direct child is reaped. The Linux
adapter deliberately does not use the Electron window handle because portable
Wayland/X11 parenting is not available through this boundary.

The supervised startup nonce is also excluded from the daemon environment.
Electron writes exactly 32 raw bytes to a one-shot stdin pipe and closes it;
the daemon consumes that payload before creating the Tokio runtime. The legacy
hex environment variable is rejected. On Linux the daemon becomes non-dumpable
before reading configuration or starting worker threads, preventing an
untrusted same-user child from recovering bearer material through the daemon's
`/proc` environment or attaching for process-memory inspection. The local
socket path alone is not sufficient to authenticate IPC.

A dedicated application service validates and durably reserves the
`enroll_xai_api_key` idempotency command before native UI. A completed command
replays canonical non-secret account state without another prompt. Prompt
execution is serialized and cooldown is recorded before dispatch; if an IPC
future is cancelled, the detached native UI thread retains the serialization
permit and zeroizes any unclaimed result.

The mutation journal distinguishes the call which first creates a pending
reservation from a later pending replay. A later replay never reopens native
entry under the same local credential generation. If that command's coherent
key and generation are already installed, the daemon completes the missing
journal acknowledgement without prompting. Missing, partial, or mismatched
vault material fails closed with an integrity result and requires a fresh
idempotency key. This prevents a vault-success/journal-failure ambiguity from
associating a different subsequently entered key with an existing generation.

The key is never placed in argv, environment variables, files, application
clipboard operations, renderer state, IPC responses, diagnostics, crash
metadata, or reusable protocol payloads. A platform may not fall back to a web
form or command-line secret argument, and sharing the Unix adapter does not by
itself qualify another operating system for release.

This breaking local-IPC change advanced the envelope compatibility epoch from
1 to 2. The removed field number and name remain reserved. Later desktop
preferences, event polling, model selection, search, asynchronous Chat, safe
Retry, and conversation-fork changes advanced the current epoch to 10; epochs
1 through 9 are
rejected before dispatch. Those later bumps do not change this enrollment
boundary. The generated Protobuf
package name remains the canonical v1 schema family; the mandatory envelope
version governs compatibility.

## Consequences

BYOK status and deletion remain available through the narrow renderer bridge
because they carry no secret. Adding or replacing a key remains unavailable in
a qualified release until Windows 11 x64 and ARM64 coverage includes
accessibility, foreground ownership, HWND destruction/reuse, package update,
cancellation, daemon shutdown, low-memory locking failure, provider failure,
vault failure, and plaintext-residue inspection. Linux promotion separately
requires representative Wayland/X11 desktops and pinentry implementations,
missing-helper and cancellation behavior, child cleanup, Secret Service
availability/failure, and plaintext-residue inspection.

The local Wayland acceptance pass proves the protected Nix pinentry target,
closed spawn environment, process-group identity, Escape cancellation, direct
child reaping, unchanged unconfigured account state, and post-cancel renderer
health. It does not replace representative X11, alternate pinentry, Secret
Service, packaged Linux, or Windows x64/ARM64 qualification.

Release qualification must also prove that renderer compromise cannot select
an arbitrary HWND or observe entered bytes, and that incomplete enrollment
commands recover without a partial credential mutation or automatic external
side-effect replay.
