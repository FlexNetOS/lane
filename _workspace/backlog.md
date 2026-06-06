# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>

- [ ] Add `--json` to `lane domain list` — emit a stable machine-readable array of custom domains (parity with `lane list --json`), pretty-printed, deserializable; human table unchanged without the flag. Fully verifiable in an isolated HOME, no privilege.
- [ ] Make doctor `run()` an `async fn run() -> Report` per ARCHITECTURE.md:411 `(preferred)` note ("Mark in cli") — the IPC + health checks are already async; collapse the internal block_on/bridge, CLI awaits it. Behavior-preserving refactor; proven by unchanged doctor output + green tests.
- [ ] Add `--json` to `lane domain verify <domain>` — emit `{domain, verified, error?}` for CI/scripting; the arg-parse + error-JSON-shape paths are unit-testable without network. Human output unchanged without the flag.

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
