# Linux full product GA contract

- Status: accepted target (not yet qualified)
- Date: 2026-07-12
- Product bar: full README surface on Linux (not Limited Mode only)
- Platform series: [platform/adr/0004](../platform/adr/0004-linux-qemu-kvm-managed-execution.md)
  through [0007](../platform/adr/0007-linux-workspace-share-and-host-commit.md)

This document freezes what “Linux GA” means for Grok Desktop. It does not claim
that the bar is met. Engineering progress is tracked in
[implementation-status.md](implementation-status.md). Release promotion still
requires [release-qualification.md](release-qualification.md) evidence for the
exact signed artifacts.

## Non-negotiable invariants

1. Strong Isolated Work, managed browser automation, and computer use run only
   inside a **qualified utility VM**. There is no automatic host-exec fallback.
   Separately enrolled Host Tools is an explicit risk-accepted backend and is
   not evidence of isolation qualification.
2. The ordinary HostControl ACP role is authentication and control only.
   Isolated Work prompts/tools use `isolated_guest`; independently enrolled
   Host Tools uses the constrained HostWorkTools role and daemon MCP bridge.
3. Subscription OAuth is delegated to the official Grok Build component. No
   cookie import, scraped web OAuth, or unapproved OAuth client.
4. BYOK xAI keys remain a separate billing and capability path from SuperGrok
   subscription. Configuring a key never implies subscription readiness.
5. An interrupted non-idempotent side effect becomes
   `interrupted_needs_review` and is never automatically replayed.
6. When KVM, the privileged broker, signed guest image, proof-of-possession, or
   guest health is missing, Isolated Work is unavailable. The product remains
   Limited unless Host Tools was independently enrolled and prepared.

## Full GA surface bar

Every advertised surface is either **Available with a real backend** or removed
from marketing. Silent permanent stubs are not GA.

| Surface | Linux full GA requirement |
| --- | --- |
| Packaged desktop + daemon | Installable artifact embeds `grok-daemon`; fuses inspected post-package |
| Updater or explicit manual channel | Signed update path **or** Settings permanently document manual-only updates |
| BYOK enroll / delete / status | Pinentry + Secret Service; key never in renderer/IPC/logs |
| Direct Chat | Durable turns, Stop, Retry, Branch/Edit/Regenerate, model selection, restart recovery |
| Grok Build sign-in | Daemon-owned host ACP authenticate lifecycle; non-secret account projection |
| Work / Shell / MCP | Guest ACP sessions after isolation + subscription facts; Host Tools may ship separately with explicit risk enrollment and does not satisfy this isolation bar |
| Files Library | Import, exact-version open, local-copy remove on private Linux storage |
| Export / diagnostics | Daemon-owned, pathless, secret-canary clean if advertised |
| Automations | Enabled schedules with lease/overlap/missed-run policy and soak evidence |
| Imagine / voice / search | Real daemon/provider paths if UI advertises them |
| Computer use / managed browser | Signed guest integrations (e.g. Wisp); no ambient host desktop control |
| Settings shown as toggles | Daemon policy store + enforcement |
| Deep links | `grok-desktop://open/v1/` registered for the package |
| Limited Mode | Only degradation when isolation/provider facts fail |

## Milestone map

| Milestone | Ships | User value |
| --- | --- | --- |
| **M0** | Platform ADRs + this contract | Architecture locked |
| **M1** | Packaging + BYOK Chat + Files qualification | Usable Limited Mode product |
| **M2** | Linux VM broker + daemon gateway | Isolation readiness without Work tools |
| **M3** | Subscription auth + guest Work/Shell/MCP | Work beta |
| **M4** | Schedules, media, Wisp/browser, policy settings | Full feature beta |
| **M5** | Complete Linux matrix + release record | **Linux full GA** |

Jumping to M5 without M1–M3 violates fail-closed isolation.

## Explicit out of scope for Linux host

- Windows MSIX, Win32 credential UI, HCS, HVSock, and Windows ACL matrices as
  Linux GA gates (they remain Windows release gates).
- Treating the non-Windows VM service **simulator** (`simulated: true`) as an
  isolation boundary.
- Host-side Wisp nested QA as a substitute for guest computer-use qualification.

## Related documents

- Threat model: [platform/threat-model.md](../platform/threat-model.md)
- Windows managed execution: [decisions/0003-managed-execution.md](../decisions/0003-managed-execution.md)
- Privileged guest contract: [platform/adr/0001-privileged-guest-contract.md](../platform/adr/0001-privileged-guest-contract.md)
- Privileged journal: [platform/adr/0003-durable-privileged-operation-journal.md](../platform/adr/0003-durable-privileged-operation-journal.md)
- Official surfaces: [research/official-grok-surfaces.md](../research/official-grok-surfaces.md)
