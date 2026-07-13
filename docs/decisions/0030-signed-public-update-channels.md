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
  public trust roots. Manifest schema 2 binds both the semantic application
  version and the platform-native package version.
- Stable manifests reject prerelease versions. Unknown fields, unknown keys,
  invalid signatures, unsupported channels, noncanonical URLs, and invalid
  bounds fail closed before download.
- Windows retains one signed MSIX identity and publisher. Linux downloads the
  exact AppImage URL authorized by the signed manifest and verifies its size and
  digest before atomic replacement. Platform installation remains Electron-main
  owned; the renderer receives bounded state and commands only.
- Stable is the default. A user may opt into beta in Settings; this preference
  is revisioned and persisted by the daemon. Stable discovery uses the latest
  release, while beta discovery considers bounded GitHub prereleases and still
  requires an exact signed beta manifest. Renderers cannot provide feed URLs.
- Downgrades and remotely initiated channel switches are unsupported. A future key rotation
  requires an application release that trusts the new public key before the old
  key is removed.

## Consequences

The first public installer must already contain an update trust root. Signing
keys and platform certificates are external release inputs and are never stored
in this repository or ordinary CI logs. Local tests use ephemeral keys only.
