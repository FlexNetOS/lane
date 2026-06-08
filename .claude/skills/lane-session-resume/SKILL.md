---
name: lane-session-resume
description: "Cold-start session resume for the lane-loop autonomous runner. Reads a committed _workspace/HANDOFF.md checkpoint, validates against .lane-loop/schemas/packet.schema.json, verifies the cargo gate baseline is green, re-establishes origin/main, and returns exact next commands to continue at the recorded backlog item and pipeline stage. Use only for the RESUME operation (reading HANDOFF.md after a session boundary): 'resume', 'continue in a new session', 'resume from _workspace/HANDOFF.md'. NOT for starting a fresh loop — use lane-loop for that. Invoked by session-relay during handoff protocol; the lane-loop skill invokes it internally at Phase 0 RESUME path."
---

# lane-session-resume — cold-start resume

A fresh `claude -p` process with **zero prior context** reads a committed `_workspace/HANDOFF.md` and continues the lane-loop at the exact pipeline stage recorded. No chat archaeology, no guessing.

## Trigger phrases

- "resume from _workspace/HANDOFF.md"
- "pick up the loop" / "continue in a new session" / "resume the lane-loop"
- `/lane-loop resume`
- External runner prompt containing "resume" and `_workspace/HANDOFF.md`

## Resume steps

### 1. Read the committed checkpoint (authoritative — not weave, not memory)

```bash
cat _workspace/HANDOFF.md          # read full checkpoint
cat _workspace/loop_state.md       # read counters
cat _workspace/backlog.md           # read backlog state
```

Validate the packet against `.lane-loop/schemas/packet.schema.json`:
- `schema` must be `"lane.loop.packet.v1"`
- All required fields present: `in_flight`, `branch`, `worktree`, `base_sha`, `drift_status`, `cargo_gate`
- `drift_status` is not `"hard_fail"` (if it is, STOP and fix drift before resuming)

### 2. Re-establish the base

```bash
git fetch origin && git rev-parse origin/main    # compare against HANDOFF.md base_sha
```

If `origin/main` moved past `HANDOFF.md`'s `base_sha`, rebase the worktree:

```bash
git -C <worktree> rebase origin/main
```

### 3. Run the verify-on-resume baseline (from HANDOFF.md verbatim)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

**CRITICAL:** If this baseline is red, **STOP and fix the regression first**. Never proceed on a red baseline.

### 4. Determine next action from HANDOFF.md `in_flight` pipeline_stage

| Stage | Next action |
|-------|-------------|
| `spec` | Run `intent-to-spec` to write spec; mark stage → `design` |
| `design` | Run `lane-feature-design` on the item; mark → `implement` |
| `implement` | Run `rust-native-implementation` on the spec+plan; mark → `verify` |
| `verify` | Run `lane-verification` + `rust-native-guard`; if green → mark → `done`, else → `implement` |
| `test` | Run verify gate (same as CI); if green → mark → `done`, else → fix and retry |
| `done` | Mark backlog item `- [x]`, bump counters, write state, continue next item |

### 5. Reset per-session counter and hand back to lane-loop

```bash
# In _workspace/loop_state.md:
cycles_this_session = 0   # carry cycles_total forward
status = "cycling"        # resume into the loop
last_update = <UTC now>
```

Broadcast `relay:resumed` via weave (best-effort heartbeat). Then hand control back to the `lane-loop` orchestrator at the recorded in-flight item.

## Hard rules

- **Never** trust weave inbox as handoff payload (self-addressed messages don't land there).
- **Never** proceed on a red cargo gate baseline — fix the break first.
- **Never** skip drift audit on resume — rust-native-guard must pass.
- **Never** use a stale packet that contradicts `origin/main` — rebase first.
- This skill is **read + execute**, not advisory. If HANDOFF.md says "resume here", that's where you resume.

## What this skill does NOT do

- It does NOT orchestrate the handoff (that's `session-relay`).
- It does NOT write checkpoints (that's `continuity-steward`).
- It does NOT reimplement crew skills (design, implement, verify).
