# NixOS utility guest

The root flake builds the x86_64 or ARM64 VHDX used by the Windows HCS/
VirtualMachinePlatform backend. The image contains the managed integration
runner; release trust keys and signed integration bundles are supplied by the
release image configuration.

The baseline has no DHCP, general-purpose guest NIC, SSH, sudo, console login,
or mutable user passwords. Host communication is reserved for allowlisted
AF_VSOCK purposes. Workspaces live below `/run/grok-desktop/workspaces` and are
read-only; integration writes remain below `/var/lib/grok-integrations`.

Release runner builds accept only authenticated guest channel v2. The runner
does not report systemd readiness until the LocalSystem-owned host service
completes per-boot protobuf provisioning and proves the ephemeral channel key.
The unauthenticated v1 transport exists only behind the
`guest_control_v1_dev` Go build tag and is never selected at runtime.

The reusable module requires a concrete runner package whenever its service is
enabled. A release image also configures trusted manifest keys and publisher
classes:

```nix
grok.guest = {
  integrationRunner.enable = true;
  integrationRunner.package = pkgs.grok-integration-runner;
  trustedManifestKeyFiles = [ ./release-key.pub ];
};
```

Build or evaluate from the repository root:

```sh
nix build path:.#hypervImage
nix build path:.#guestModuleEvaluation
nix flake check path:. --no-build
```

Release images must be built from a clean, locked tree and published with a
SHA-256 digest consumed by `EnsureImage`.
