# ADR 0025: Daemon-owned automation scheduler journal

- Status: Accepted, journal kernel and lifecycle health implemented; execution disabled
- Date: 2026-07-12

## Context

Grok Desktop can store inactive automation definitions and terminal history, but
neither is a scheduler. Terminal history cannot prove that one due occurrence
was claimed exactly once, bind work to the definition that was current when it
became due, enforce missed-run and overlap policy atomically, or distinguish a
safe pre-dispatch retry from an interrupted side effect.

The existing schedule field is also not an executable contract. Renderer saves
currently use JSON while older tests and records use cron-like strings. Treating
either opaque string as authority in Electron would move time-zone policy out
of the daemon and could silently reinterpret malformed records. Starting runs
before a durable occurrence journal and qualified isolated executor exist would
violate the fail-closed execution and interrupted-side-effect invariants.

## Decision

Schema 19 introduces a daemon-private, journal-only scheduler kernel. Public
creation and update continue to reject `enabled=true`; Automations remains a
Limited capability; and the kernel has no Run, Chat, tool, privileged-operation,
or provider dispatcher. A stored occurrence is not authority to execute it.

Protocol epoch 16 removes and permanently reserves
`CreateAutomationRequest.enabled` field 8 and
`UpdateAutomationRequest.enabled` field 9. The daemon always supplies
`enabled=false` to the inward-facing definition use cases, including when an
old peer sends either removed field as an unknown protobuf tag. Epoch 16 also
adds required closed scheduler lifecycle health with only
`kernel_initialized_execution_disabled`,
`recovery_pending_execution_disabled`, and
`degraded_execution_disabled` as valid renderer states. An absent or
unspecified value is a protocol failure, not an inferred healthy state.

After schema and store construction, production creates one journal service
and performs exactly one bounded expired-claim recovery pass using the same
new process identity exposed by daemon health. A complete pass reports kernel
initialized; a truncated pass or a live prior-process lease reports recovery
pending; storage, integrity, or invalid-state failures report degraded. These
outcomes do not block Chat or general app startup and never start a timer,
evaluate a schedule, claim an occurrence, or dispatch work.

Rust owns a closed versioned schedule grammar for daily, weekdays, weekly, and
monthly local-wall-clock schedules. New and updated definitions are normalized
to canonical `v1` text before persistence. The schema-18-to-19 migration accepts
only the previously supported renderer JSON and narrow five-field cron forms,
normalizes them transactionally, and rejects malformed or already-enabled
legacy rows. The separately stored IANA time zone is authoritative and is part
of a versioned SHA-256 schedule fingerprint.

The calendar engine freezes every real due instant as UTC milliseconds. A
nonexistent spring-forward local time is represented only by its logical local
slot and is never shifted to a synthetic instant. A fall-back fold resolves to
the earlier UTC occurrence exactly once. A missing monthly date produces no
slot for that month. Calculator version and fingerprint are retained with each
cursor and occurrence so later time-zone database or algorithm changes cannot
rewrite materialized history.

The durable journal separates:

1. a fenced, expiring scheduler lease;
2. an optimistic per-definition evaluation cursor;
3. exact evaluation command evidence;
4. immutable occurrences containing the definition revision and complete
   execution snapshot; and
5. bounded occurrence-claim attempts.

Occurrence identity is the automation, definition revision, and nominal local
slot, not merely a UTC timestamp. At most one active occurrence and one queued
overlap may exist per automation. Missed `RunOnce` windows coalesce to one exact
latest candidate with a count; `Skip` records a bounded terminal decision.
Overlap `QueueOne` retains at most one pending successor; `Skip` terminalizes
the overlap. Cursor advance and all resulting occurrence decisions commit in
one transaction after rechecking the enabled definition revision, canonical
fingerprint, lease fence, and expected cursor revision.

Claims persist before any future dispatch and carry an owner, fence, deadline,
and bounded attempt count. An expired claim with no run link may be reclaimed
without inventing a second occurrence. Once a future implementation durably
links an occurrence to a run or side effect, expiry can only become
`interrupted_needs_review`; it must never trigger automatic replay. Claim
exhaustion also requires review. Terminal occurrence and public history writes
must be atomic and exactly replayable.

Wall time determines due instants and persisted deadlines. Monotonic time is
used only for in-process sleeping. A backward wall-clock correction produces no
new cursor or occurrence write until time reaches the durable floor. A forward
jump beyond lease continuity is a missed window and applies the stored policy.
Every scan, evaluation, recovery pass, and claim history is bounded and resumes
through stable keys rather than enumerating an unbounded outage in memory.

Protocol epoch 16 exposes only closed lifecycle health. Occurrences, prompts,
schedule fingerprints, lease owners, fences, wakeups, ticks, claims, and
recovery controls remain daemon-private. Enabling schedules requires a new
qualified command and cannot reuse the removed definition boolean once
isolated execution, atomic run binding, restart recovery, and soak
qualification exist.

## Consequences

- Renderer compromise cannot enable a definition, choose due times, claim an
  occurrence, or manufacture scheduler recovery state.
- Restart, suspend, overlap, and clock-correction behavior can be tested against
  durable evidence before any billable or side-effecting execution is wired.
- Definition edits cannot change the prompt, schedule, policies, or time zone of
  an already materialized occurrence.
- Existing terminal automation history remains a projection, not the source of
  scheduling truth.
- The migration is intentionally strict: unsupported legacy schedule strings,
  corrupt history, or pre-contract enabled rows block schema advancement rather
  than being guessed into executable meaning.
- An initialized journal does not make Automations active. Qualification still
  requires a daemon-lifetime scheduler loop, atomic isolated run production,
  sleep/reboot/DST/clock-correction recovery, and the documented multi-day soak.
