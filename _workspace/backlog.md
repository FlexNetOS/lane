# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>

- [x] Add `lane completions <shell>` subcommand (bash/zsh/fish/powershell) emitting a shell completion script via clap_complete; fully verifiable in an isolated HOME with no privilege. — PR #13, green local gate (206 tests, clippy clean, behavior verified live, guard PASS). Auto-merge NOT armed: SAFE-mode session (no LANE_APPLY opt-in) → awaiting human merge.
- [x] Fix `lane doctor` false-negative CA-trust + port-forwarding probes (FlexNetOS/lane#5): trust check looks for basename `rootCA.pem` but the installer writes `lane.crt` (cert/trust.rs); `portfwd` `is_enabled()` runs `iptables -t nat -C` without privilege (portfwd.rs) so exit-4 perm-denied reads as "not configured". Both report ✗ while the layer actually works. — PR #14, green local gate (208 tests, clippy clean, guard PASS). VERIFIED LIVE against #5's exact condition on this host: doctor now shows ✓ CA trust + ! "cannot verify without root" (Warn, no sudo prompt), was ✗/✗. The anticipated sudo "human wall" was AVOIDED by design — the diagnostic reports Warn instead of escalating, so no privileged verify was needed. Auto-merge NOT armed: SAFE-mode session (no LANE_APPLY) → awaiting human merge.

<!--
DISCOVER baseline (cycle seed, 2026-06-05): branched from origin/main @ b8636e3, tree clean,
no open PRs, only the primary worktree. Recently shipped (do NOT re-propose): doctor --json (#3),
logs --json (#6), logs -n/--lines (#7), version --json (#8), restart (#9), harness loop upgrade
(#10/#11/#12). The prior "completions (feat-completions worktree)" pointer has NO surviving
branch/worktree/PR — treated as unclaimed and re-seeded above. slim Go reference NOT present
locally (/home/drdave/Downloads/slim-extract/slim-main absent), so completions is a standard CLI
enhancement rather than a confirmed Go-parity port. Re-dedup against origin/main + open PRs at the
top of EACH cycle.
-->
