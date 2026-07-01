# ADR 0002: Service-mediated authenticated guest channel

- Status: accepted; proxy and rekey implemented behind caller-authorization gate
- Date: 2026-07-10
- Extends: `docs/platform/adr/0001-privileged-guest-contract.md`
- Protocol: `proto/guest/v2/channel.proto`

## Context

An AF_VSOCK peer address identifies the Windows host, not the desktop daemon.
The current HCS socket ACL admits the tenant SID, so another process running as
that user can reach the guest control listener. Guest control can select a
signed integration, supply its declared grants, and request computer-use
actions. A host-CID check therefore is not an authorization boundary.

A shared token in the image, registry, HCS document, command line, environment,
or guest disk would become a long-lived credential and is rejected. A raw guest
socket returned to the daemon would also leave the privileged service unable to
bind frames to the qualified desktop process or enforce replay policy.

## Decision

Guest channel v2 will be owned end to end by the Windows VM service:

1. Production HCS socket ACLs for provisioning and control admit LocalSystem
   only. They do not admit the tenant SID or Builtin Administrators. The service
   does not return a raw VSOCK endpoint or handle.
2. At every VM start, the service creates a random 128-bit boot ID, 256-bit
   channel key, and 256-bit host nonce with the Windows CSPRNG. These values live
   only in the tenant runtime and are zeroed when the VM stops or the service
   loses ownership.
3. The service provisions the key over the LocalSystem-only socket. The guest
   replies with an independently generated nonce and an HMAC proof. The guest
   does not report control readiness until this exchange succeeds.
4. The Rust daemon reaches guest operations through a revised narrow service
   proxy. The service must verify the named-pipe client SID, MSIX package family,
   publisher, executable identity, and process identity at the moment the
   session is opened. A production grant-bearing session is unavailable to an
   unpackaged or development client.
5. Every subsequent frame uses `AuthenticatedFrame`. HMAC-SHA-256 covers the
   ASCII domain `grok.desktop.guest-channel.v2`, a zero byte, and deterministic
   serialization of `AuthenticatedPayload`. Boot ID, direction, sequence,
   request ID, deadline, and the exact bounded control payload are therefore
   authenticated together.
6. Host-to-guest and guest-to-host sequences start at one and advance exactly
   once. A duplicate sequence with identical authenticated bytes may return the
   cached response. Reuse with different bytes, gaps, stale boot IDs, invalid
   MACs, and expired deadlines close the channel.

The provisioning acknowledgment uses the separate ASCII domain
`grok.desktop.guest-channel.v2.provision-ack`, a zero byte, and deterministic
serialization of `ProvisionChannelProof`. The proof contains protocol version,
boot ID, host nonce, and guest nonce; it contains neither the channel key nor
the MAC. Wire messages use a four-byte big-endian length prefix and reject zero,
oversized, malformed, or unknown-field protobuf messages before dispatch.
These details are part of v2 and require a new version to change.

The guest runner never receives the key through a manifest, integration config,
adapter environment, inherited descriptor, log, crash field, or durable state.
Adapters cannot create AF_VSOCK sockets; a capability-specific broker passes an
already connected descriptor only when an approved manifest and durable daemon
grant require one.

## Side-effect recovery

Channel authentication is not durable idempotency. The Rust daemon persists
intent before every side effect. If the service, VM, channel, or adapter is lost
after dispatch but before a correlated result, the operation becomes
`interrupted_needs_review`. Neither the service nor the guest automatically
replays it on a new boot ID. Read-only observations may be issued again as a new
operation.

The in-boot replay cache has both entry and byte limits. It cannot evict an
unexpired non-idempotent result while the boot remains active. Response overflow
or protocol ambiguity poisons the adapter stream and returns an indeterminate
outcome rather than a retryable failure.

## Rollout and qualification

- v1 is compiled only with the `guest_control_v1_dev` Go build tag. Release
  binaries contain only v2, with no environment, policy, command-line, or
  negotiation fallback that can select v1.
- Windows tests must verify the HVSock security descriptor, package identity
  checks, service restart, VM restart, replay, key zeroization, and rejection of
  tenant-SID direct connections.
- Guest tests must verify MAC-before-decode, strict sequence handling, bounded
  replay memory, bootstrap timeout, and that adapter seccomp denies AF_VSOCK.
- Protocol or cryptographic changes require a new version and threat-model
  review; an environment-variable compatibility switch is not permitted.

## Consequences

The VM service contract grows a small typed guest proxy rather than exposing a
generic stream. This adds implementation and Windows qualification work, but it
keeps guest authority bound to the signed desktop process and makes channel
reset behavior explicit. Development images remain Limited Mode for privileged
guest actions until the complete boundary is available.

## Implementation state

The generated Go messages and shared v2 codec live in
`native/windows-vm-service/guestchannel/v2`. Boot material and both handshake
nonces use the OS CSPRNG; both channel directions use deterministic protobuf
MACs, strict metadata validation, bounded replay, and key poisoning on close.
The production guest runner authenticates the channel before decoding control
JSON or reporting systemd readiness.

HCS configuration restricts each configured socket service to LocalSystem. The
legacy `OpenSocket` operation remains only for v1 wire compatibility and the
Windows backend returns `unavailable`; it never returns a raw HVSock endpoint.
The service now owns the native Windows HVSock connector, provisions a channel
during VM start, rekeys adopted runtimes after service restart, and retires
channels on VM lifecycle or protocol failure. The contract 1.1
`guest_control` operation constructs the guest envelope from the authenticated
idempotency key and deadline and never returns a raw endpoint. Fake-backed race
tests cover restart, rekey, cancellation, and corrupt responses; Windows x64
and ARM64 builds compile.

Named-pipe authentication now binds every connection to SID, logon session,
client PID, process creation time, the exact packaged daemon path, and the
broker's own package full name/family. That process qualification deliberately
does not set the guest-control grant. A read-only Rust client now validates the
static capability document after independently qualifying the SCM service,
package identity, and fixed executable paths; it exposes no lifecycle or
guest-control method. The per-install daemon proof-of-possession protocol, a
proof-bearing control client, durable privileged replay journal, and real
Windows HVSock/package qualification remain required before production guest
control can leave Limited Mode.
