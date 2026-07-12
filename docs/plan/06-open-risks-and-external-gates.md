# Open risks and external gates

| Gate | Why local tests are insufficient | Completion evidence |
|---|---|---|
| SuperGrok API Chat contract | Contract drift after the approved source-pinned flow | ADR 0026 compatibility tests and fixed-origin enforcement |
| SuperGrok billing attribution | Counter names do not prove which request increments which product | Redacted before/after bounded real-account qualification |
| Real xAI Chat | Mocks cannot prove account/model/stream behavior | User-approved bounded run with a user-owned key; no secret retained in evidence |
| Windows HCS | Linux/local mocks cannot qualify Windows isolation | Documented Windows qualification-worker results |
| Linux QEMU/KVM | Contract tests do not prove VM isolation or PoP | Signed image plus broker/guest qualification on supported hardware |
| Release component trust | Fixture keys do not establish production publisher trust | Production trust roots, signed catalogs, provenance and packaged identity verification |
| Native tray | Headless web preview cannot inspect OS tray/menu/window lifecycle | Isolated native Electron qualification when the desktop is available |
| Signing/updater | Local packages are not distributable releases | Platform signatures, updater/channel evidence, fuse/inventory checks |
| Multi-day recovery/soak | Unit fault injection does not prove long-lived stability | Bounded-resource soak with restart, sleep/resume, network loss and recovery evidence |

## Security risks requiring forward fixes

- Scheduler partial persistence can duplicate durable runs after crash.
- Wisp signatures may not bind the manifest fields later trusted.
- Wisp JSON state is not transactional, encrypted canonical daemon state.
- Dirty ACP credential copying crosses the official-client secret boundary.
- Dirty recursive cleanup can delete identity-swapped or unexpected trees.
- Linux socket/package work needs peer, identity, framing, and signed-staging
  qualification before Work can be available.

## Defaults while gates remain open

- Chat remains unprivileged and uses only a user-owned xAI API key.
- Subscription access remains inside official Grok Build ACP.
- Scheduler definitions remain inactive.
- Wisp lifecycle mutations remain unavailable.
- Work remains Limited Mode without qualified isolation.
- Unfinished product surfaces remain visibly unavailable.
