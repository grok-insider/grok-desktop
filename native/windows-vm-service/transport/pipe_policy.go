package transport

const (
	// ServicePipeName is versioned and fixed so the MSIX declaration, the
	// daemon, and the service cannot be redirected to an attacker-controlled
	// named-pipe namespace.
	ServicePipeName = `\\.\pipe\GrokDesktop.VMService.v1`

	// Authenticated local users may exchange frames. SYSTEM and Administrators
	// own the pipe. Network and anonymous tokens are denied in the DACL and are
	// independently rejected after per-frame impersonation.
	servicePipeSDDL = "D:P(D;;GA;;;AN)(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)"
)
