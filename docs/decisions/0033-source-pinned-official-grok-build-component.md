# ADR 0033: Source-pinned official Grok Build component

- Status: accepted
- Date: 2026-07-13

## Context

Grok Desktop delegates subscription execution to the official Grok Build ACP
client. The existing release path accepts only an independently signed private
component catalog. That catalog infrastructure is not publicly available, so a
clean public build cannot assemble a working package even when it downloads the
exact official artifact published by xAI.

The desktop release must remain reproducible and fail closed. A package must
not select a mutable channel at runtime, accept an arbitrary executable, or
trust metadata supplied beside the installed files.

## Decision

Public beta packages may use a tracked, platform-specific source pin for the
official `https://x.ai/cli/` artifact. The pin records the exact source URL,
semantic version, target, install-relative executable path, byte size, and
SHA-256 digest. Release assembly downloads or receives that exact artifact and
verifies every field before staging it.

The SHA-256 digest of the exact manifest bytes is embedded in `grok-daemon` at
compile time under a domain-separated binding. At startup the daemon accepts
the installed pinned manifest only when it matches that binding, matches the
running platform, names the fixed xAI origin and official identity, and the
local executable passes canonical path, link, size, digest, executable-mode,
and filesystem-identity checks. The same executable is reverified immediately
before every spawn.

Source-pinned and signed-catalog trust modes are mutually exclusive in one
daemon build. The existing signed catalog, key rotation, expiry, and rollback
watermark route remains available and is preferred once public catalog
operations exist. Development-only caller descriptors remain unavailable in
release builds.

This decision replaces only the unavailable private ACP catalog signature for
the beta component. It does not replace package signing, update-manifest
signing, MSIX identity, guest-image signing, or integration signing.

## Consequences

- The exact official artifact is auditable from source and immutable within a
  built daemon/package pair.
- Updating Grok Build requires a reviewed manifest change and a daemon rebuild.
- A compromised xAI distribution origin before pin review remains a supply
  chain risk; published hashes and release evidence make that selection
  observable but do not create an independent xAI signature.
- A package assembled with a different manifest or executable fails closed.

## Rejected alternatives

- Runtime resolution of a `stable` URL is mutable and non-reproducible.
- Trusting an adjacent hash file lets package contents redefine their own
  trust root.
- Importing another Grok client's installed executable or credentials violates
  component ownership and credential boundaries.
- Removing verification to make beta packaging convenient silently widens the
  execution trust boundary.
