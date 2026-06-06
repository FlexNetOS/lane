# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>

- [x] Add `--json` to `lane domain list` — emit a stable machine-readable array of custom domains (parity with `lane list --json`), pretty-printed, deserializable; human table unchanged without the flag. — PR #15, green local gate (212 tests +2, clippy clean, fmt clean, `--json` in help, Rust-native only-.rs). Auto-merge ARMED (`gh pr merge 15 --auto --merge`) → lands hands-free on green CI.
- [x] Make doctor `run()` an `async fn run() -> Report` per ARCHITECTURE.md:411 `(preferred)` note. — ALREADY SHIPPED (dedup-drop, no PR). Top-of-cycle dedup found `doctor::run()` is already `pub async fn run() -> Report` (src/doctor/mod.rs:66), `cli/doctor.rs` already `doctor::run().await`, and there is NO block_on/Handle bridge to collapse. Satisfied by the earlier doctor --json work (#3). No fabricated no-op change.
- [x] Add `--json` to `lane domain verify <domain>` — emit `{domain, verified, status?, error?}` for CI/scripting. — PR #16, green local gate (215 tests +3, clippy/fmt clean, `--json` in help, Rust-native only-.rs). Auto-merge ARMED → lands hands-free on green CI.

## Batch 2 (re-DISCOVER 2026-06-05 — complete domain-subcommand JSON coverage; depends on #16 merging since same file `domain.rs`)
- [x] Add `--json` to `lane domain add <domain>` — emit `{domain, target_ip, dns:{type,name,value}}`. — PR #17, green local gate (216 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.
- [x] Add `--json` to `lane domain remove <domain>` — emit `{domain, removed, error?}`; un-forced 409 → `{removed:false,error}` (no prompt). — PR #18 MERGED, green local gate (217 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Completes domain-subcommand JSON coverage (#15/#16/#17/#18 all MERGED).

## Batch 3 (re-DISCOVER 2026-06-05 — scriptable project orchestration; independent of domain.rs)
- [x] Add `--json` to `lane up` — emit `{config, started:[{name,port,routes?}]}`. — PR #19, green local gate (218 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.
- [x] Add `--json` to `lane down` — emit `{stopped:[domain…], remaining, daemon, warnings?}`. — PR #20, green local gate (219 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.
- [x] (stretch) `lane logs --follow`/`-f` — ALREADY SHIPPED (dedup-drop). `LogsArgs.follow` exists (src/cli/mod.rs:166); logs.rs already tails like `tail -f` and honors `--json` (NDJSON) in both the tail and stream loops. No work needed.

## Batch 4 (re-DISCOVER 2026-06-05 — programmatic URL capture for automation; NOT churn — scripts need the URL)
- [x] Add `--json` to `lane start` — emit `{domain, port, url, routes?}`; --wait progress → stderr so stdout is pure JSON. — PR #21, green local gate (220 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.
- [x] Add `--json` to `lane share` — NDJSON event stream (connected{url,…}/request*/disconnected, error for Pro path). — PR #22, green local gate (222 tests +2, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.
- [x] Add `--json` to `lane stop` — emit `{stopped:[domain…], daemon, warnings?}`. — PR #23, green local gate (223 tests +1, clippy/fmt clean, `--json` in help, Rust-native). Auto-merge ARMED.

## Batch 5 (DEEPER re-DISCOVER 2026-06-05 — docs sync; the --json surface grew but docs lag)
- [x] Document `--json` across `docs/commands.md` — added a `--json` row+shape+example to start/stop/up/down/list/logs/share/doctor/version and all 4 domain subcommands (+ logs `-n/--lines`). — PR #24, docs-only, 222 tests unaffected. Auto-merge ARMED.

## Batch 6 (CONVERGING — substantive backlog nearly exhausted; verify before declaring DONE)
- [x] (thin) Note lane's `--json` enhancements over slim in `docs/comparison-with-slim.md`. — PR #25, docs-only ('Additions beyond slim' subsection + intro qualifier). Auto-merge ARMED.

CONVERGENCE NOTE (2026-06-05): The real enhancement backlog is essentially exhausted — PRD all 12
goals shipped (full slim parity), `--json` complete + documented across every value-adding command,
no code TODOs except deferred `TODO(test-phase)` integration tests that need a live daemon socket /
privileged /etc/hosts+iptables (NOT runnable unattended → a genuine integration-env wall, not loop
work). After the thin Batch-6 doc note, the next session should run the **DONE gate** (Phase 3:
build+release+test+fmt+clippy green on integrated main, backlog clear, no `- [!]`) and write
`_workspace/DONE` with evidence — a LEGITIMATE terminal now (everything merged+green), distinct from
the earlier premature stop. Do NOT manufacture `--json`-on-action-command churn to keep the loop alive.

NOTE: --json read/orchestration coverage is otherwise COMPLETE (list/doctor/logs/version/domain×4/up/down). lane is at full slim parity (PRD all 12 goals shipped), no code TODOs, the one ARCHITECTURE `(preferred)` note (doctor-async) already satisfied. After Batch 4, re-DISCOVER must mine DEEPER (test gaps, docs accuracy, edge cases) or run the DONE gate — do NOT churn --json onto action commands beyond start/share/stop.

<!--
DISCOVER baseline (re-seed, 2026-06-05, fresh session after prior backlog cleared+merged):
branched from origin/main @ 8209c86 (local main = 8209c86 + unpushed _workspace bookkeeping).
Tree clean. NO open PRs, NO open issues, NO claimed worktrees (pruned the two stale already-merged
ones: feat-completions, fix-doctor-probes). lane is at full slim command parity
(docs/comparison-with-slim.md), so backlog = enhancements + ARCHITECTURE (preferred)/TODO notes,
matching the shipped pattern. Recently shipped (do NOT re-propose): doctor --json (#3), logs --json
(#6), logs -n/--lines (#7), version --json (#8), restart (#9), completions (#13), doctor#5 probe
fix (#14). `lane list --json` already exists; `domain list`/`domain verify` lack --json (gap).

POLICY (this session, no-human-in-loop): every cycle MUST open a PR and arm `gh pr merge --auto
--merge`. main is protected with required checks (fmt+clippy, build+test ubuntu, build+test macos)
+ delete_branch_on_merge, so --auto lands it hands-free on green — NO "await human merge", NO
premature DONE. Leaving PRs uncreated/open is what caused cross-session conflicts; do not repeat it.
Re-dedup against origin/main + open PRs at the top of EACH cycle.
-->
