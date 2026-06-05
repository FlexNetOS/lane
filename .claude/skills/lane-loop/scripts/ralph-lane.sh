#!/usr/bin/env bash
# ralph-lane.sh — external Ralph loop for lane: self-restarts the lane-loop with a FRESH context
# each iteration (each `claude -p` process is a clean session = the /new effect) until a terminal
# sentinel. Truth lives on disk (_workspace/backlog.md + loop_state.md + commits), so every restart
# resumes cold with zero loss.
#
# SAFE BY DEFAULT: destructive/unattended applies are refused unless LANE_APPLY=1 is set explicitly.
# Bounded by MAX_ITERS; `touch _workspace/STOP` halts at the next boundary.
set -euo pipefail

WORKTREE="${RALPH_WORKTREE:-$(pwd)}"
BUDGET="${RALPH_BUDGET:-3}"            # completed cycles per fresh agent before it hands off
MAX_ITERS="${RALPH_MAX_ITERS:-50}"    # hard backstop on respawns
SLEEP_BETWEEN="${RALPH_SLEEP:-5}"
MODEL="${RALPH_MODEL:-opus}"
WS="$WORKTREE/_workspace"; mkdir -p "$WS"

log(){ printf '[ralph-lane %s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
command -v claude >/dev/null || { log "FATAL: claude not on PATH"; exit 1; }

APPLY_ARGS=()
if [ "${LANE_APPLY:-0}" = "1" ]; then
  APPLY_ARGS=(--dangerously-skip-permissions)
  log "APPLY MODE — fresh agents may modify the live system unattended (LANE_APPLY=1)."
else
  log "SAFE mode (default): unattended destructive applies refused. Set LANE_APPLY=1 to opt in."
fi

read -r -d '' PROMPT <<EOF || true
/lane-loop resume (external ralph-lane runner, fresh context). Worktree: $WORKTREE.
1. If _workspace/HANDOFF.md exists, follow the session-relay RESUME entry point from it (the committed
   handoff is the authoritative signal): fetch origin, rebase the worktree if origin/main moved, run
   the verify-on-resume baseline (cargo fmt --check / clippy -D warnings / test + lane-verification),
   reset cycles_this_session=0. Otherwise run lane-loop DISCOVER and seed _workspace/backlog.md.
2. Run up to $BUDGET cycles via the lane-loop skill: ONE backlog item each, in its own worktree
   branched from origin/main; drive the crew (intent-to-spec -> lane-feature-design ->
   rust-native-implementation -> lane-verification + rust-native-guard); VERIFY across the boundary in
   a FRESH shell; commit per cycle. Stay 100% Rust-native. Fail-closed: dry-run before any destructive
   step, never weaken a guard, do NOT trust lane doctor's CA-trust/port-forward probes (FlexNetOS/lane#5).
3. Then write EXACTLY ONE sentinel under _workspace/ and stop (do NOT ScheduleWakeup):
   - _workspace/DONE (with evidence) if the backlog is clear AND the full DONE gate passes;
   - _workspace/NEEDS-HUMAN (with reason) at a sudo/interactive/hardware wall;
   - else _workspace/HANDOFF.md (spawn continuity-steward via session-relay HAND OFF).
EOF

cd "$WORKTREE"
i=0
while :; do
  i=$((i+1))
  [ "$i" -gt "$MAX_ITERS" ]   && { log "MAX_ITERS ($MAX_ITERS) hit — halting."; exit 3; }
  [ -f "$WS/STOP" ]           && { log "STOP present — halting."; exit 2; }
  [ -f "$WS/DONE" ]           && { log "DONE present — finished."; exit 0; }
  [ -f "$WS/NEEDS-HUMAN" ]    && { log "NEEDS-HUMAN: $(cat "$WS/NEEDS-HUMAN")"; exit 2; }

  log "iter $i/$MAX_ITERS — spawning fresh agent (budget=$BUDGET, model=$MODEL)"
  claude -p "$PROMPT" --model "$MODEL" --add-dir "$WORKTREE" "${APPLY_ARGS[@]}" \
    >>"$WS/ralph-run-$i.log" 2>&1 || log "iter $i exited nonzero (resuming from durable state)"

  # Re-check terminal sentinels written by the agent this iteration.
  [ -f "$WS/DONE" ]        && { log "DONE."; exit 0; }
  [ -f "$WS/NEEDS-HUMAN" ] && { log "NEEDS-HUMAN: $(cat "$WS/NEEDS-HUMAN")"; exit 2; }
  [ -f "$WS/STOP" ]        && { log "STOP — halting."; exit 2; }
  sleep "$SLEEP_BETWEEN"
done
