# ADR 0030: Signed public update channels

- Status: accepted
- Date: 2026-07-12

## Context

Grok Desktop needs public beta and stable updates on Windows and Linux. Release
metadata is a security boundary: an update replaces Electron, the daemon, and
privileged integration inputs. Renderer-owned feeds, unsigned JSON, mutable
asset URLs, or silent channel changes would give untrusted content execution
authority.

## Decision

- GitHub Releases under `grok-insider/grok-desktop` is the only initial update
  artifact origin.
- Every platform, architecture, and channel has a canonical schema-versioned
  manifest signed with an offline Ed25519 release key. Applications embed only
  public trust roots.
- Stable manifests reject prerelease versions. Unknown fields, unknown keys,
  invalid signatures, unsupported channels, noncanonical URLs, and invalid
  bounds fail closed before download.
- Windows retains one signed MSIX identity and publisher. Linux publishes a
  signed AppImage update path. Platform installation remains Electron-main
  owned; the renderer receives bounded state and commands only.
- Downgrades and remote channel switches are unsupported. A future key rotation
  requires an application release that trusts the new public key before the old
  key is removed.

## Consequences

The first public installer must already contain an update trust root. Signing
keys and platform certificates are external release inputs and are never stored
in this repository or ordinary CI logs. Local tests use ephemeral keys only.
