# Security Policy

## Reporting

Do not open a public issue for a suspected vulnerability. Send a concise report
to `admin@grokinsider.net` containing affected versions, prerequisites,
reproduction steps, impact, and any proposed mitigation. Remove credentials,
personal content, and unrelated user data before sending evidence.

Receipt will be acknowledged within three business days. Triage targets are
seven days for critical/high severity and fourteen days for other supported
reports. Coordinated disclosure timing is agreed after a fix and release path
exist.

## Security boundaries

The Electron renderer, model output, files, web content, MCP servers, managed
add-ons, browser pages, and VM workers are untrusted. The Rust daemon, platform
vault, signed updater, and minimal Windows VM service are trusted components.
Approvals improve user control but are not a substitute for deterministic
containment.

Reports that depend on deliberately enabling an unsafe developer option remain
useful, but should clearly identify that prerequisite. Never test against an
account, machine, or service you do not own or have permission to assess.

