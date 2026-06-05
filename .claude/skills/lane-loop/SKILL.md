---
name: lane-loop
description: "Autonomous, resumable, self-restarting backlog runner for lane. Works a durable on-disk backlog ONE item per cycle, commits every cycle, and hands off to a fresh session at a cycle budget so truth lives on disk and any restart resumes cold with zero loss. Per cycle it DRIVES the existing crew skills (lane-feature-design / intent-to-spec → rust-native-implementation → lane-verification + rust-native-guard) — it does not reimplement them. USE THIS when asked to: run lane autonomously / unattended / 'ralph' lane / work the lane backlog ACROSS SESSIONS until done (NOT a one-shot in-session 'work the backlog' — that is intent-driven-development) / resume the loop / 'pick up the loop' / 'continue in a new session' / 'resume from _workspace/HANDOFF.md' / kick off the self-restarting runner. For a SINGLE in-session feature with no cross-session continuity, use intent-driven-development instead; lane-loop is the durability + continuity + /new self-restart layer on top of it."
---

# lane-loop — autonomous, resumable, self-restarting operation

Run lane's feature work as a **chain of short sessions** instead of one long one. A single session
rots (context fills, quality drops) and burns tokens; the defense is: write state to disk every
cycle, do one backlog item per cycle, commit, and hand off to a fresh context at a budget. Because
truth lives on disk (backlog + ledger + commits), any restart — same session, cron successor, the
external `ralph-lane.sh` runner, or a human — resumes **cold with zero loss**.

This skill owns the **loop, durability, and continuity**. It does **not** reimplement design,
implementation, or verification — each cycle it drives the existing lane crew skills. Read the
`intent-driven-development` orchestrator for the crew's internals; this skill is the layer that
sequences it across a durable backlog and across sessions.

## Non-negotiable principles (the loop is only as trustworthy as these)

1. **Write state down every cycle.** Never hold the plan only in context.
2. **Durable state on disk** under `_workspace/` (`backlog.md` + `loop_state.md` + `HANDOFF.md`).
3. **One item per cycle; commit per cycle.** A fresh process must resume from committed state alone.
4. **The committed `HANDOFF.md` is the authoritative resume signal** — not the weave inbox.
5. **Fail-closed.** Destructive/irreversible ops are dry-run first + opt-in; never weaken a guard to
   make a step pass.
6. **Human walls STOP the loop** (sudo / interactive auth / reboot / hardware you can't drive) —
   write `NEEDS-HUMAN` with the reason and halt; don't spin or force.
7. **Safe by default.** Unattended *apply* is a deliberate opt-in (`LANE_APPLY=1` on the runner),
   never the default.
8. **Bounded.** The external runner has a `MAX_ITERS` backstop and an always-checked `STOP` switch.
9. **Stay 100% Rust-native.** rust-native-guard blocks drift; `.workflows/*.mjs` is the only exception.

## Phase 0: Context check (initial vs resume vs new)

Decide the mode before acting:

- **`_workspace/HANDOFF.md` exists** and the request is "resume / pick up / continue" →
  **RESUME**: invoke the `session-relay` skill's RESUME entry point (it reads the committed handoff,
  rebases if the base moved, runs the verify-on-resume baseline, resets `cycles_this_session=0`),
  then continue this loop at the recorded in-flight item.
- **`_workspace/backlog.md` exists, no resume requested** → continue the existing backlog from its
  current `- [ ]` item (don't re-DISCOVER and clobber it).
- **No `_workspace/` state** → **DISCOVER** (below), then start cycling.

## Phase 1: DISCOVER (seed the backlog from real state, don't hallucinate)

Build `_workspace/backlog.md` from ground truth, deduped against what's already shipped/in-flight:

```bash
git fetch origin && git checkout main && git pull --ff-only origin main   # branch only from real latest
git worktree list ; git branch -a ; gh pr list --state open               # treat all as CLAIMED
```

Source candidate items from: open intents, the `docs/` roadmap, `PRD.md` parity gaps, `ARCHITECTURE.md`
`(preferred)`/TODO contract notes, the CLI surface, and open issues. Run each candidate through the
`intent-to-spec` skill's lens to confirm it's real and shippable. **Dedup against reality:** drop any
item already on `origin/main` or in flight in another worktree/branch/PR. Re-check this dedup at the
top of *each* iteration — the backlog goes stale as other sessions merge.

Write `_workspace/backlog.md` (ordered, dependency-respecting):

```markdown
# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>
- [ ] <intent, one line — the smallest correct shippable unit>
- [ ] <next intent>
```

Seed `_workspace/loop_state.md` from the §template (see `references/loop-templates.md`). Commit both.

## Phase 2: One iteration (the cycle body)

1. **Read state** — `loop_state.md` + `backlog.md`.
2. **Stop-checks (in order):**
   - No `- [ ]` item left → run the **DONE gate** (Phase 3). If it passes, write `_workspace/DONE`
     and stop. If not, the unmet check becomes the next backlog item.
   - `cycles_this_session >= cycle_budget` → invoke `session-relay` **HAND OFF**, then stop (no
     wakeup).
   - A human wall is unavoidable for the top item → write `_workspace/NEEDS-HUMAN` (reason) and stop.
3. **Pick the top `- [ ]` item** (respect dependency order). Mark it IN FLIGHT in `loop_state.md`.
4. **Fresh worktree per item.** Re-sync and branch from `origin/main` (never develop on main):
   ```bash
   git fetch origin && git checkout main && git pull --ff-only origin main
   git worktree add ../.worktrees/<item-slug>/lane -b <item-slug> origin/main
   ```
5. **Drive the crew on the item** (this is the work — delegate, don't reimplement). The standard
   pipeline, via sub-agents reading their `.claude/agents/*.md` + skills directly:
   - `intent-to-spec` (intent-analyst) → verifiable spec + acceptance criteria.
   - `lane-feature-design` (solution-architect) → blast-radius + file-by-file plan; pre-clear drift.
   - `rust-native-implementation` (rust-implementer) → idiomatic Rust + unit tests.
   - `lane-verification` (verification-engineer) **and** `rust-native-guard` (rust-native-guardian),
     in parallel after implementation, in a bounded build→verify→guard loop (max 3 iterations).
   > For a heavyweight item you may invoke the whole `intent-driven-development` orchestrator on it;
   > for a small item, drive the four skills directly. Either way the loop owns only sequencing +
   > durability, never the crew's internals.
6. **VERIFY across the boundary** (not existence-only — confirm it actually works). The per-cycle gate:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test                       # 37+ in-module #[cfg(test)] suites
   ```
   plus the `lane-verification` skill's acceptance checks for *this* item, exercised against the real
   binary in an isolated `HOME` where behavior is claimed. **Guardrail:** do NOT trust `lane doctor`'s
   CA-trust / port-forwarding probes until `FlexNetOS/lane#5` lands — verify those two via the real
   curl / iptables ground truth, never via `doctor`.
7. **Write state back:** mark the item `- [x]` (or `- [!] blocked: <reason>` with the reason),
   bump `cycles_this_session` and `cycles_total`, update `last_item` / `status` / `last_update`.
8. **Commit per cycle** on the item branch, area-prefixed subject, with a `[[tasks/<slug>]]` KB
   wikilink when KB is available. Rebase onto `origin/main` before opening the item's PR so it can't
   collide; open one PR per feature; confirm CI green.
9. **Self-pace.** `ScheduleWakeup` to re-enter the next cycle. Use a long delay only when genuinely
   waiting on a slow external step (e.g. CI); otherwise re-enter promptly. At the budget, hand off
   instead of waking.

## Phase 3: DONE gate (terminal — only with evidence)

Write `_workspace/DONE` **only** when the backlog has no `- [ ]` left AND every check below passes;
put the evidence (commands + results) inside the file. Never write DONE on an unproven green.

```bash
cargo build && cargo build --release   # release profile: opt-level=z, LTO, panic=abort, stripped
cargo test                             # green
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings
```
Plus: backlog clear, the `lane-verification` skill green on every shipped item, and any `- [!]`
blocked items surfaced (not silently dropped). If a blocked item remains, the terminal sentinel is
`NEEDS-HUMAN` (or the loop stays open), not `DONE`.

## Sentinel contract (the external runner reads exactly one per process)

| Sentinel (`_workspace/…`) | Meaning | Runner action |
|---------------------------|---------|---------------|
| `HANDOFF.md`  | more work remains | spawn the next fresh process |
| `DONE`        | finished + verified (evidence inside) | exit 0 |
| `NEEDS-HUMAN` | sudo / reboot / interactive / hardware wall (reason inside) | halt for human |
| `STOP`        | kill switch (a human `touch`es it) | halt |

Write **exactly one** terminal sentinel per process and then stop. The in-session loop self-paces via
`ScheduleWakeup`; the external `ralph-lane.sh` runner self-restarts a fresh context per iteration.

## The external self-restart runner (`/new` in executable form)

An agent cannot type `/new` (it's a REPL command, not a tool) — but **a new process is a clean
context**. `scripts/ralph-lane.sh` is a bounded `while` loop that spawns one fresh
`claude -p "/lane-loop resume …"` per iteration, reads the single sentinel it wrote, and respawns
until a terminal sentinel. Safe by default; `LANE_APPLY=1` opts into unattended apply; `MAX_ITERS`
backstops; `touch _workspace/STOP` kills it. See `scripts/ralph-lane.sh` and
`references/loop-templates.md` for the launch incantations.

## Test Scenarios

### Happy path (autonomous, multi-session, terminates on DONE)
1. `bash .claude/skills/lane-loop/scripts/ralph-lane.sh` (SAFE mode). No `_workspace/HANDOFF.md`, so
   the fresh agent runs DISCOVER → seeds `backlog.md` with 2 deduped intents, commits.
2. Cycle 1: picks item 1, branches a worktree from `origin/main`, drives the crew, the cargo gate +
   lane-verification go green, marks `- [x]`, commits, opens a PR. Cycle 2: same for item 2. At
   `cycle_budget=3` not yet reached but the backlog is now empty.
3. Backlog has no `- [ ]` left → DONE gate runs: `cargo build && cargo build --release`, `cargo test`,
   fmt+clippy all green → writes `_workspace/DONE` with the evidence and stops. The runner sees `DONE`
   and `exit 0`. No spin past the empty backlog.

### Error path 1 (cold resume across a session boundary)
1. Cycle reaches `cycle_budget` mid-backlog → `session-relay` HAND OFF: `continuity-steward` writes
   `_workspace/HANDOFF.md` (in-flight item = "item 3, spec written, not yet implemented"), it's
   committed, `HANDOFF.md` sentinel set, process stops.
2. The runner spawns a fresh `claude -p`. With zero context it reads the committed `HANDOFF.md`,
   `git fetch` shows `origin/main` moved, so it rebases the worktree first, runs the verify-on-resume
   baseline (green), resets `cycles_this_session=0`, and continues at item 3's *implement* stage — no
   re-doing the spec, no lost work.

### Error path 2 (human wall → fail-closed, no false green)
1. An item needs `sudo` to exercise port-forwarding behavior in verification, unavailable unattended.
   The loop does NOT weaken the check or fake a pass. It verifies what it can (tests + inspection),
   marks the item `- [!] blocked: needs sudo to verify portfwd live`, writes `_workspace/NEEDS-HUMAN`
   with the reason, and stops. The runner halts for a human. (And per the standing guardrail, it never
   trusted `lane doctor`'s CA-trust/port-forward probes to manufacture a green — `FlexNetOS/lane#5`.)

## References

- `_workspace/loop_state.md` schema + the `_workspace/` layout: `references/loop-templates.md`.
- The crew internals this loop drives: the `intent-driven-development` skill.
- The hand-off / cold-resume protocol: the `session-relay` skill.
