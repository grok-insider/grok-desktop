# hcsshim provenance

The Windows VM service uses a narrowly adapted subset of the MIT-licensed
Microsoft `hcsshim` project as an audited reference for public Host Compute
System ABI declarations and schema constants. It does not vendor or link the
container runtime, command execution, network, layer, or guest process APIs.

- Upstream: <https://github.com/microsoft/hcsshim>
- Commit: `aaa13778d1cfad8ffc6547110048c37b5bae6f27`
- Retrieved: 2026-07-10
- License: MIT, reproduced in `LICENSE`

Adapted references:

- `internal/computecore/computecore.go`: documented `computecore.dll` lifecycle,
  operation, enumeration, service probe, and VM file-access signatures.
- `internal/hcs/schema2/{compute_system,virtual_machine,devices,plan9,
  plan9_share,hv_socket_2,hv_socket_system_config,hv_socket_service_config}.go`:
  fixed schema 2.1 JSON field names used by `hcs_document.go`.
- `internal/hcs/schema2/plan9_share_flags.go`: `Plan9ShareFlagsReadOnly = 0x1`.
- `internal/vm/vmutils/constants.go`: the standard LCOW Plan9 port `564`.

The local implementation was rewritten around the service's smaller
`internal/hcsapi.Client` interface. Only fixed-schema Linux utility VM creation,
start, shutdown, terminate, enumeration, capability probing, and VM disk access
are callable.

The native declarations were also checked against the Microsoft Compute Core
API reference. In particular, HCS result `PWSTR` values are released with
`LocalFree`, as required by `HcsWaitForOperationResult` and used by Microsoft's
official HCS samples; there is no generalized allocator or runtime dependency.

- <https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcswaitforoperationresult>
- <https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcscreatecomputesystem>
- <https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/tutorial>
