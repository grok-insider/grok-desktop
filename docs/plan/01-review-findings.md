# Review findings

## Commit verdicts

| Commit | Verdict | Finding |
|---|---|---|
| `2264846` | keep | Linux GA contract and QEMU/KVM ADRs correctly preserve fail-closed isolation as the release bar. |
| `531b662` | keep with forward fix | Useful package and broker contracts; packaging must verify the signed component/catalog identity rather than only stage files. |
| `7cb95a6` | keep with forward fix | Adds narrow application ports and capability facts; release claims still depend on authenticated broker and signed component qualification. |
| `68cfd2b` | superseded docs | Status refresh was accurate for that point but later epochs and implementations made the ledger stale. |
| `5d3c316` | keep with forward fix | Strengthens gates and wires isolation, but does not by itself qualify Work or Linux GA. |
| `6f4342a` | keep | Correctly removes Settings surfaces without daemon support. |
| `b2f02a1` | keep | Correctly routes subscription setup to official ACP host authentication. |
| `a71d7ef` | unsafe; forward-disable | Non-atomic occurrence/thread/run binding permits duplicate durable objects after crash; no qualified isolated executor consumes queued prompts. |
| `70838a4` | keep with forward fix | Unix-socket transport is useful; peer identity, ownership, bounds, deadlines, concurrency, negotiation, and stale-socket recovery need qualification. |
| `ddaaf51` | keep | Correctly removes unimplemented Imagine advertising. |
| `38b53b8` | keep | Corrects JSON base64 wire compatibility and adds cross-language fixtures. |
| `97c1b13` | keep with forward fix | Adds EnsureImage/StartVm/health sequence; signed image and real QEMU/KVM qualification remain external gates. |
| `2c20128` | keep | Correctly removes Wisp install advertising before lifecycle readiness. |
| `69ffe8d` | unsafe; forward-disable | Signature authority is not necessarily bound to the parsed manifest; lifecycle state is mutable JSON outside canonical encrypted durable storage. |
| `66787d5` | keep | Narrow evidence-parser correction. |

No reviewed commit will be rewritten or reverted. Corrective commits must
preserve compatible durable data and fail closed.

## Dirty worktree inventory

The following is not part of HEAD and must be reviewed as independent changes:

- Electron supervision and a debug ACP descriptor: potentially useful local
  discovery, but it must be unpackaged/debug-only, identity-stable, and must
  not claim publisher authenticity from a self-computed digest.
- Linux packaging script changes: retain only after signed-catalog validation,
  post-copy hashing, architecture checks, and deterministic tests.
- Product availability changes: retain only if they report daemon facts and do
  not advertise locally inferred readiness.
- ACP isolation/runtime changes: remove copying of `~/.grok/auth.json`, remove
  recursive residue deletion, and restore fail-closed handling of unexpected
  or mutated trees. Tightening a verified application-owned directory may be
  retained after identity and ownership tests.
- `auth_seed.rs` and `real_grok_verify.rs`: exclude from normal tests. Ordinary
  tests must not inspect real credentials or require a locally installed CLI.
- `docs/platform/linux-release.md`: reconcile only after the corresponding
  implementation survives its gates.
- `ai-continue-plan.md` and `ui-plan.md`: historical inputs only.

## SuperGrok proposal review and authorization

`open-usage` at `6764adc6fa729337d15bab90c9d8e8e28b28646c`
does verify that the official billing response contains one weekly pool and
separate `GrokBuild`, `Api`, and `GrokChat` product rows. It does **not** prove
that those rows are interchangeable transports or that a third-party desktop
may enroll the public OAuth client for direct API Chat.

The repository owner subsequently authorized the source-pinned public xAI
OAuth/API contract. ADR 0026 and the amended product invariant make that
authorization explicit while preserving Grok Build ACP ownership and banning
credential import. See the research note for the locked contract and remaining
real-account attribution gate.
