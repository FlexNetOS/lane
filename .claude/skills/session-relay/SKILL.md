---
name: session-relay
description: "Hands a lane-loop run off to a fresh session and resumes it cold. Two entry points: HAND OFF (at a cycle budget — checkpoint via continuity-steward + PreHandoff hook gate, commit, weave heartbeat) and RESUME (invoke the lane-session-resume skill to read _workspace/HANDOFF.md, validate against .lane-loop/schemas/packet.schema.json, run the verify-on-resume baseline, reset the per-session counter). Invoked BY the lane-loop skill at cycle-budget boundaries or during cold resume; not usually triggered directly. The committed HANDOFF.md — never the weave inbox — is the authoritative resume signal."
---

# Session Relay — durable hand-off / cold resume for lane-loop

A long session rots (context fills, quality drops) and burns tokens. The defense is a **chain of
short sessions**, each handing a durable checkpoint to the next. This skill is the hinge between
sessions. It has exactly two entry points. The `lane-loop` skill calls into it; you rarely trigger
it directly.

**The one load-bearing fact:** the **committed `_workspace/HANDOFF.md` is the authoritative resume
payload** — *not* weave, *not* cron. A weave message addressed to your own identity does **not** land
in your own inbox, and a same-machine successor inherits the same identity, so weave can only ever be
a *cross-identity heartbeat*, never the handoff payload. cron's `durable:true` is **not** honored in
this runtime (session-only), so cron is best-effort. Truth lives on disk + in git; that is what
survives a restart with zero loss.

---

## Entry point 1 — HAND OFF (called at the cycle budget)

Preconditions: the current cycle is fully committed (clean tree); `cycles_this_session >=
cycle_budget`; there is still at least one `- [ ]` item left in `_workspace/backlog.md`.

1. **Fire PreHandoff hook gate.** Load `.lane-loop/hooks/lane-events.toml` → `PreHandoff` event. Gates:
   `drift_pass_or_soft_fail`, `handoff_file_integrity`, `no_dirty_tree`. If drift is `hard_fail`, stop —
   fix drift before handoff. Never weaken this gate.
2. **Spawn the `continuity-steward` agent** (Agent tool, `subagent_type: continuity-steward`,
   `model: opus`) to write `_workspace/HANDOFF.md` from real ground truth, validated against
   `.lane-loop/schemas/packet.schema.json`. Pass it the worktree path.
3. **Validate the checkpoint packet** before committing — verify all required fields (schema, in_flight,
   branch, worktree, base_sha, drift_status, cargo_gate) are present and non-empty. If validation fails,
   write the steward back with feedback.
2. **Commit the checkpoint.** Stage and commit `_workspace/HANDOFF.md` + `_workspace/backlog.md` +
   `_workspace/loop_state.md`:
   ```bash
   git -C <worktree> add _workspace/HANDOFF.md _workspace/backlog.md _workspace/loop_state.md
   git -C <worktree> commit -m "chore(lane-loop): handoff @ cycle <n> — <in-flight item>"
   ```
   The committed checkpoint is the real handoff. Everything below is observability, not payload.
3. **weave heartbeat (cross-identity, best-effort).** Broadcast `to:"all"` so other identities can
   observe the relay — `relay:handoff` with the resume command + branch. If weave is unavailable,
   skip silently; it is not the payload.
4. **Best-effort cron successor.** `CronCreate {recurring:false}` whose **prompt self-describes the
   resume** (`/lane-loop resume from _workspace/HANDOFF.md` in the worktree). Treat as best-effort —
   if it doesn't fire, a human, the external `ralph-lane.sh` runner, or `RemoteTrigger` resumes from
   the committed `HANDOFF.md`.
5. **Write the `HANDOFF.md` sentinel and STOP.** Do **not** `ScheduleWakeup` — the whole point is to
   yield this context. In the in-session loop this means: stop here. Under the external runner, the
   presence of `HANDOFF.md` (and absence of `DONE`/`NEEDS-HUMAN`) tells `ralph-lane.sh` to spawn the
   next fresh process.

> Hand off only at a clean cycle boundary. Never hand off mid-edit — the successor would inherit a
> dirty tree and a broken verify-on-resume baseline.

---

## Entry point 2 — RESUME (a fresh session picks up the loop)

Triggered by `/lane-loop resume from _workspace/HANDOFF.md`, "pick up the loop", "continue in a new
session", or the external runner's prompt.

1. **Invoke the `lane-session-resume` skill** — it reads the committed `_workspace/HANDOFF.md`, validates
   it against `.lane-loop/schemas/packet.schema.json`, verifies the cargo gate baseline, re-establishes
   origin/main, and returns exact next commands to continue at the recorded backlog item and pipeline stage.
   If `HANDOFF.md` is absent or invalid, tell `lane-loop` to run DISCOVER instead.
2. **Re-establish the base.** `git fetch origin`; if `origin/main` moved past the recorded base,
   rebase the worktree onto it before doing anything (a stale base causes duplicate/colliding work).
3. **Run the Verify-on-resume baseline** from `HANDOFF.md` verbatim (the cargo gate + the in-flight
   item's lane-verification checks). **If it is red, STOP and fix the regression first** — a red
   baseline means the prior cycle left the tree broken; building on it compounds the break. Never
   proceed on a red baseline, and never paper over it.
4. **Broadcast `relay:resumed`** `to:"all"` (best-effort heartbeat).
5. **Reset `cycles_this_session = 0`** in `_workspace/loop_state.md` (carry `cycles_total` forward).
6. **Hand control back to `lane-loop`** at the backlog's current in-flight item (the one marked
   IN FLIGHT in `HANDOFF.md`), at the exact pipeline stage recorded.

---

## What this skill must never do

- Never treat the weave inbox as the handoff payload (self-addressed messages aren't there).
- Never claim a successor will definitely run (cron is best-effort) — the committed checkpoint is the
  guarantee, the rest is heartbeat.
- Never hand off or resume across a dirty tree or a red baseline.
- Never weaken a guard or fake a green to make the relay "succeed".

## Data flow

```
┌─────────────┐   PreHandoff     ┌──────────────────┐   CHECKPOINT    ┌───────────────┐
│ lane-loop   │ ──────────────→  │ continuity-steward │ ──────────────→ │ HANDOFF.md    │
│ (orchestrat) │                  │ (agent)            │   committed     │ (.git tracked)│
└─────────────┘                  └──────────────────┘                  └───────┬───────┘
                                                                                 │ resume invoked
                                                                                 ▼
┌─────────────┐   lane-session-resume      ┌──────────────┐
│ fresh session │ ←──────────────────────  │ HANDOFF.md   │
│ (zero ctx)    │   validate + verify     │ → resume cmd  │
└─────────────┘                           └──────────────┘

Heartbeat: weave `to:"all"` relay:handoff / relay:resumed (best-effort, not payload).
Successor trigger: cron (best-effort) + ralph-lane.sh runner + human/RemoteTrigger.
Policy: .lane-loop/policies/rules.toml — enforced by lane-loop and rust-native-guard.
Hooks: .lane-loop/hooks/lane-events.toml — gates at phase transitions.
```

## References

- Schema validation: `.lane-loop/schemas/packet.schema.json` (handoff packet) + `session.schema.json` (state machine).
- Session resume implementation: `lane-session-resume` skill.
