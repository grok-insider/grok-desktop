# ADR 0034: Unsigned Windows core installer

- Status: accepted
- Date: 2026-07-16
- Supersedes: the Windows packaging decision in ADR 0030

## Context

The first public Windows release was blocked by certificate provisioning,
SignTool, timestamp compatibility, and MSIX identity requirements. The active
core product contains Chat and explicitly enrolled Host Tools but not the
privileged service or isolated guest that motivated the enterprise MSIX design.
Requiring those deferred distribution inputs prevents public feedback without
improving the active package's capability boundary.

An unsigned installer will trigger Windows publisher-reputation warnings.
Source availability helps users audit intent, but it does not authenticate
downloaded bytes, so update authorization and release provenance must remain
independent security boundaries.

## Decision

- The active Windows x64 core release is an intentionally unsigned, per-user
  NSIS installer named `GrokDesktop-<channel>-x64.exe`, with application ID
  `com.grokinsider.grokdesktop`.
- Its build requires no PFX, certificate-store identity, SignTool, signer
  thumbprint, timestamp endpoint, or MSIX identity. Ambient Electron signing
  inputs are rejected rather than used implicitly.
- Update-manifest schema 3 identifies the artifact as `nsis-installer` and
  binds its canonical GitHub release URL, semantic version, platform,
  architecture, byte size, and SHA-256 under an offline Ed25519 key. The updater
  revalidates the file immediately before launching it directly without a shell.
- Every release publishes `SHA256SUMS`, a GitHub build attestation, release
  evidence declaring `windowsCodeSigning: unsigned`, and a link to the immutable
  source tag. Release notes explain the expected Unknown Publisher or Microsoft
  Defender SmartScreen warning before installation.
- The signed MSIX identity, Authenticode, packaged service, and guest-image
  contract remains deferred to the isolated/enterprise train. It is not an
  active core release prerequisite.

## Consequences

Windows will not identify Grok Desktop as a trusted publisher, and reputation
warnings may recur after releases. Users must consciously accept that warning
and can verify the exact artifact against GitHub-hosted provenance. Automatic
updates remain fail-closed under signed metadata; source audit alone is never
treated as artifact authentication.

The updater rehashes an open file immediately before a direct `CreateProcess`
launch and uses a dedicated, bounded staging directory. Windows still launches
executables by path rather than by the verified Node file handle. A malicious
process already running as the same user can race that final path lookup and
can also replace the per-user installed application; defending a compromised
user session is outside this update channel's threat boundary. Network and
cross-user substitution remain fail-closed under the signed manifest, private
profile permissions, exact size, and SHA-256 checks.

The NSIS installer owns only current-user application and protocol state,
requires no elevation, preserves application data during uninstall, and can be
replaced later by a separately qualified stable distribution without weakening
the deferred isolated package's trust contract.

## Rejected alternatives

- A self-signed preview certificate: it still requires manual trust-store
  enrollment and retains the signing/timestamp machinery that blocked release.
- An unsigned MSIX: Windows deployment and identity rules make it a poor fit
  for the intended public, per-user install and update flow.
- Removing signed update metadata: checksums and auditable source do not stop a
  compromised or substituted update feed from authorizing arbitrary bytes.
