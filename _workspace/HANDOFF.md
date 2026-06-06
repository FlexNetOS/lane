# lane-loop HAND OFF — 2026-06-05 (cold-resume signal)

This committed file is the AUTHORITATIVE resume signal. A fresh session re-invoked with
`/lane-loop resume` reads this, rebases, runs the verify-on-resume baseline, resets
`cycles_this_session=0`, and continues at the in-flight item below.

## Where we are
- main is clean; local main carries only unpushed `_workspace/` bookkeeping commits.
- One PR in flight: **#21 `lane start --json`** (auto-merge ARMED — lands on green CI).
- No active worktrees (pruned). Re-dedup vs `origin/main` + `gh pr list` at the top of each cycle.

## Shipped (all auto-merged hands-free; nothing awaiting human merge)
Session A: #15 `domain list --json`, #16 `domain verify --json`, #17 `domain add --json`,
  #18 `domain remove --json`; doctor-async dedup-drop.
Session B (this one): #19 `up --json`, #20 `down --json`, #21 `start --json` (auto-merge armed).
Dedup-dropped (already shipped): doctor async `run()`; `logs --follow`.
→ `--json` now covers every read/orchestration/start command: list, doctor, logs, version,
  domain×4, up, down, start.

## In-flight item (resume HERE)
No half-done code. Next top `- [ ]` = **`lane share --json`** (Batch 4): emit `{url, port, …}` for
the created tunnel — the public URL is the key automation value. Then `lane stop --json`.
NOTE: `share --json` and `stop --json` edit `mod.rs` (ShareArgs/StopArgs) like #21 did StartArgs —
branch each from a main that has #21 merged (rebase first), and ship one at a time (poll-merge
between them since they share mod.rs).

## Next-session recipe
1. `git fetch origin && git checkout main && git rebase origin/main` (integrate #21; `_workspace`
   commits rebase cleanly — they only touch `_workspace/*.md`).
2. Branch `lane-share-json` from origin/main; add `--json` to ShareArgs (mod.rs) + share.rs.
   Mirror the pattern: pretty JSON to stdout, human output unchanged without the flag, progress to
   stderr if any, unit test on the payload shape.
3. Gate: fmt --check · clippy -D warnings · cargo test (220+) · `lane share --help` shows `--json`.
4. Commit · push · `gh pr create` · `gh pr merge <n> --auto --merge`. Mark `- [x]` only on green
   LOCAL gate. Then `lane stop --json` likewise. At cycle_budget(3) → HAND OFF again.
5. AFTER Batch 4: the obvious `--json` space is exhausted. Re-DISCOVER must mine DEEPER (integration
   test gaps, docs accuracy, edge-case hardening) — do NOT churn `--json` onto remaining action
   commands (login/logout/restart/uninstall/upgrade have no meaningful structured output). If a real
   DISCOVER turns up nothing shippable AND everything is merged+green, THEN run the DONE gate
   (Phase 3) and write DONE with evidence — that is a legitimate terminal, unlike a premature stop.

## Standing guardrails
- NO-HUMAN-IN-LOOP: every cycle opens a PR + arms auto-merge. Never leave PRs uncreated/open.
- NO premature DONE while `- [ ]` items remain. 100% Rust-native (only `.rs`/`Cargo.*`).
- Don't trust `lane doctor`'s CA-trust/portfwd probes beyond what #14 fixed.
- Real human walls (sudo/interactive/branch-protection-review) → NEEDS-HUMAN, never a fake green.
  Transient CI/network failures are retryable, not walls.
