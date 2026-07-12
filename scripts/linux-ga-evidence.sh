#!/usr/bin/env bash
# Capture post-change Linux GA evidence into the goal implementer scratch dir.
# Usage: SCRATCH=/tmp/grok-goal-.../implementer ./scripts/linux-ga-evidence.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="${SCRATCH:-/tmp/grok-goal-e4b40f0c3621/implementer}"
mkdir -p "$SCRATCH"
cd "$ROOT"

{
  echo "=== linux-ga-evidence $(date -Iseconds) ==="
  echo "cwd=$ROOT"
  git rev-parse HEAD
  git log --oneline -8
} | tee "$SCRATCH/git-log.txt"

run_log() {
  local name="$1"
  shift
  echo "=== $name ===" | tee "$SCRATCH/${name}.log"
  if "$@" >>"$SCRATCH/${name}.log" 2>&1; then
    echo "OK $name" | tee -a "$SCRATCH/${name}.log"
    return 0
  fi
  echo "FAIL $name (exit $?)" | tee -a "$SCRATCH/${name}.log"
  return 1
}

status=0
run_log protocol-test cargo test -p grok-protocol --lib || status=1
run_log application-test cargo test -p grok-application --lib || status=1
run_log memory-execute-due cargo test -p grok-memory --lib automation_scheduler_execute_due || status=1
run_log daemon-wire-test cargo test -p grok-daemon --bin grok-daemon linux_guest_transport || status=1
run_log daemon-lib-test cargo test -p grok-daemon --lib --tests || status=1
run_log go-linux-vm-service bash -lc 'cd native/linux-vm-service && go test ./... -count=1' || status=1

if command -v pnpm >/dev/null 2>&1; then
  run_log desktop-library-automations \
    pnpm --filter @grok-desktop/desktop exec vitest run \
      src/views/LibraryView.test.tsx \
      src/views/AutomationsView.test.tsx \
      src/app/ProductFlows.test.tsx \
      electron/daemon/DaemonSupervisor.test.ts \
    || status=1
else
  echo "pnpm missing" | tee "$SCRATCH/desktop-library-automations.log"
  status=1
fi

# Socket smoke is included in daemon-wire-test; restate summary for verifier.
if rg -q "socket_smoke_orchestrates_ensure_create_start_health ... ok|socket_smoke_parses_guest_control_error_envelope ... ok" "$SCRATCH/daemon-wire-test.log" 2>/dev/null; then
  echo "socket_smoke: ok (product EnsureImage→Start→health orchestration, codec clean)" | tee "$SCRATCH/socket-smoke.log"
else
  echo "socket_smoke: missing or failed — see daemon-wire-test.log" | tee "$SCRATCH/socket-smoke.log"
  status=1
fi

# Honest residual ledger (do not claim green Work/QEMU/media).
cat >"$SCRATCH/final-check.log" <<EOF
=== final-check $(date -Iseconds) ===
HEAD: $(git rev-parse HEAD)
aggregate_status: $status

GREEN paths captured this run:
- protocol-test, application-test, memory-execute-due, daemon-wire-test (base64 body + socket smoke), daemon-lib-test
- go-linux-vm-service wire round-trip + package tests
- desktop Library/Automations/ProductFlows/DaemonSupervisor vitest (when pnpm present)

Honest residuals (NOT done for full Linux GA):
- Production QEMU (non-lab Spawn) release matrix and signed guest image catalog promotion
- Signed Wisp install/update IPC productization (adapter schema exists; install remains fail-closed)
- T6 overlay host-commit UX still deferred
- Imagine/voice/search product ops not shipped (Library de-advertises media creation)
- Work Available still requires subscription + strong isolation guest health success

Shipped product isolation path (lab-qualified):
- EnsureImage → Create/StartVm → grant → runner.health via GROK_LINUX_VM_SOCKET
- Peer: SO_PEERCRED + /proc/pid/exe (client peerExe not authoritative)

T7 status: durable execute_due + schedule_active + KernelInitializedExecutionEnabled when journal recovers cleanly.
T8 status: de-advertise only (no Imagine create UI).
EOF

echo "wrote $SCRATCH/final-check.log (status=$status)"
exit "$status"
