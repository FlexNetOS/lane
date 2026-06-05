# lane-loop templates & layout

Concrete templates the `lane-loop` skill references. Kept here so the SKILL.md body stays lean.

## `_workspace/` layout

```
_workspace/
├── backlog.md       # single source of truth — ordered checklist, committed every cycle
├── loop_state.md    # the ledger (schema below), committed every cycle
├── HANDOFF.md       # rolling cold-start checkpoint (written by continuity-steward), committed
├── DONE             # terminal sentinel: finished + verified (evidence inside)
├── NEEDS-HUMAN      # terminal sentinel: a human wall (reason inside)
├── STOP             # kill switch (a human touches it)
└── ralph-run-*.log  # per-iteration runner logs — gitignored, NOT committed
```

Commit `backlog.md` + `loop_state.md` + `HANDOFF.md` every cycle. The `*.log` files and the bare
sentinel files (`DONE`/`NEEDS-HUMAN`/`STOP`) are runtime artifacts — gitignore them (see the
`_workspace/.gitignore` shipped in this upgrade).

## `_workspace/backlog.md` template

```markdown
# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>

- [ ] <intent — the smallest correct shippable unit, one line>
- [ ] <next intent>
```

## `_workspace/loop_state.md` template

```markdown
# Loop state — lane-loop
session_started: <UTC e.g. 2026-06-05T02:52:37Z>   # you supply it; scripts can't read the clock
loop: lane-loop
branch: <branch>
worktree: <abs path>
cycle_budget: 3            # completed cycles per session before handoff (override via arg)
cycles_this_session: 0     # reset to 0 on RESUME
cycles_total: 0            # carried across sessions
last_item: (none — discovery only)
status: DISCOVER complete — backlog seeded
last_update: <UTC>
```

## Per-cycle verify gate (the lane verbs)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```
Plus the `lane-verification` skill's acceptance checks for the in-flight item, exercised against the
real binary in an isolated `HOME` where behavior is claimed.

## DONE gate (terminal — only with evidence inside `_workspace/DONE`)

```bash
cargo build && cargo build --release    # release: opt-level=z, LTO, panic=abort, stripped
cargo test
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings
```
Plus: backlog clear; `lane-verification` green on every shipped item; blocked items surfaced.

## Repo guardrails (always in force)

- **Rust-native by mandate.** No non-Rust source/build step pulled into the crate; `rust-native-guard`
  blocks drift. The only sanctioned non-Rust is `.workflows/*.mjs` (dev orchestration, never shipped).
- **`lane doctor` lies (for now).** Its CA-trust + port-forwarding probes false-negative
  (`FlexNetOS/lane#5`). Do not let a verify cycle trust `lane doctor` for those two checks until #5
  lands — verify them via the real `curl https://<domain>/…` + `iptables -t nat -L` ground truth.
- **Never develop on `main`.** One worktree per item, branched from a freshly-pulled `origin/main`;
  rebase before each PR.

## External runner — launch incantations

The runner is **safe by default**: it self-restarts fresh-context agents and commits non-destructive
progress, but refuses unattended destructive apply unless you explicitly opt in. Opting in is a
deliberate human act — the loop never enables it on its own.

```bash
# SAFE (default): plans/dry-runs, commits non-destructive progress, every destructive step still gated
bash .claude/skills/lane-loop/scripts/ralph-lane.sh

# UNATTENDED APPLY: opt in deliberately (passes --dangerously-skip-permissions to each fresh agent)
LANE_APPLY=1 bash .claude/skills/lane-loop/scripts/ralph-lane.sh

# Tunables (env): RALPH_WORKTREE, RALPH_BUDGET (cycles/agent), RALPH_MAX_ITERS, RALPH_SLEEP, RALPH_MODEL
RALPH_BUDGET=2 RALPH_MAX_ITERS=20 bash .claude/skills/lane-loop/scripts/ralph-lane.sh

# Kill switch, any time:
touch _workspace/STOP
```

## Sentinel contract

| Sentinel | Meaning | Runner action |
|----------|---------|---------------|
| `HANDOFF.md`  | more work remains | spawn next fresh process |
| `DONE`        | finished + verified | exit 0 |
| `NEEDS-HUMAN` | human wall (reason inside) | halt for human |
| `STOP`        | kill switch | halt |
