# AUDIT KICKOFF — W2 host network plane (ADR-0003) full verification audit

> Paste-ready kickoff for a FRESH lane session. Queued 2026-06-17 at the end of the W2 build
> session. Purpose: independently AUDIT that the W2 host-network-plane work is implemented and
> done **properly** — not re-build it, not trust the prior session's self-reports.

You are auditing, with fresh eyes and an adversarial stance, the W2 host network plane that the
previous lane-loop session shipped to `main` (ADR-0003). **Assume nothing is correct until you have
read the code and run it.** The prior session reported all green — your job is to try to prove it
wrong. Per the owner's standing rules: a silently-dropped portable field is a **no-downgrade
violation** (bad behavior), and you must **never fabricate a green** — verify against real ground
truth (the actual binary / nmcli / the committed snapshot), never against a reread of our own claims.

## Start (cold)
```bash
git fetch && git checkout main && git pull --ff-only origin main   # latest; develop audits in a fresh worktree
git worktree add ../.worktrees/w2-audit/lane -b w2-audit origin/main
cargo build --features hostnet                                     # confirm it builds
```
Read first: `docs/adr/ADR-0003-host-network-adopt-consume.md`, `docs/adopt/host-nm-snapshot-2026-06-17.md`,
`ARCHITECTURE.md` (`## src/net`), `_workspace/backlog.md` (Phase 8 W2 item), `_workspace/HANDOFF.md`.

## Scope — what shipped (the audit targets, all merged on main)
- **P0a #56** `src/net/model.rs` — lossless netplan-v2 superset model + round-trip the snapshot.
- **P0b #57** `src/net/adopt.rs` — `lane net adopt` (nmcli-sourced, secret-safe, `hostnet`-gated).
- **P1 #59** `src/net/apply.rs` — `lane net apply` additive reconcile (type-level no-flush), dry-run-default.
- **P2 #60** `src/net/profile.rs` + `src/net/networkd.rs` — per-host profiles + systemd-networkd renderer.

## The audit (drive the crew; one finding-class per pass; verify across the boundary)
Run `verification-engineer` + `rust-native-guardian` (and optionally `/harness:code-research` for a
deep architecture pass). Produce a written audit report under `_workspace/` with PASS/FAIL + evidence
(file:line / command output) per item. **Do not mark anything PASS without running it.**

1. **Gate is real:** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`;
   `cargo test`; `cargo test --features hostnet`. Confirm the test COUNTS match the claims (≈458 default /
   ≈455 hostnet) and that no test is `#[ignore]`-hidden or trivially asserting.
2. **No stubs / no skips:** grep the whole `src/net/` for `todo!`, `unimplemented!`, `unreachable!`,
   `// TODO`, `// FIXME`, `simplified`, `for now`, dropped match arms. A portable feature parked as
   "optional" is a no-downgrade violation — flag it.
3. **Lossless adopt (no-downgrade):** run `lane net adopt` on the live box; diff the emitted model
   against the committed snapshot + the raw `nmcli`/`netplan` truth. Confirm EVERY observable
   (address/route/match/never-default/autoconnect/wifi-key-mgmt/link-local) round-trips. Any field the
   model can't express is a porter TASK to file, not an accepted skip.
4. **Additive safety is structural (not just tested):** confirm `NmcliOp` has NO delete/flush variant
   and `reconcile` can never emit one; confirm key-by-match-not-UUID; re-run the live whole-host
   `adopt | apply --dry-run` and confirm an EMPTY plan (idempotent). Try to construct an input that
   makes apply plan a flush/delete — if you can, that's a critical finding.
5. **networkd no-silent-drop is structural:** confirm the exhaustive-destructure field audit actually
   makes a new model field a compile error until rendered-or-gapped (add a scratch field and confirm
   it fails to compile, then revert). Render a unit exercising wakeonlan/nameservers/on_link/routes and
   confirm each appears or is a documented gap.
6. **Secret-safety everywhere:** prove a literal secret is structurally unrepresentable (model) AND
   cannot appear in adopt output, a saved profile, an apply plan, or a networkd file. Feed a raw PSK
   fixture through each path and grep the output.
7. **Fail-closed + dry-run-default + feature gate:** confirm `--apply` is explicit/conflicting, the
   mutating loop stops on first error, refuses unresolved secret placeholders, and `lane net` fails
   closed without `--features hostnet`. Confirm NO host mutation happens on any default/dry-run path.
8. **Path-safety:** confirm `profile save`/`--host`/`profile show` reject `..`/path-separator names.
9. **Backlog ↔ reality cross-check:** for every `[x]` item in `_workspace/backlog.md` Phase-8 W2,
   confirm the claim matches the merged code. For every remaining item (P1b, P3) confirm it is
   genuinely blocked (human wall / cross-repo) and was NOT silently skipped.
10. **Continuity docs accurate:** confirm `.handoff/context/capsule.json` + `_workspace/HANDOFF.md` +
    `loop_state.md` reflect the merged truth; `hf drift` clean; `hf doctor` health OK.
11. **Rust-native invariant:** no `.js/.mjs/.ts/.py/.go/.omc/.ecc`/shell build step entered the crate;
    no new dependency beyond what's justified; nmcli/networkd are runtime exec, not build steps.

## Done definition for the audit
Write `_workspace/AUDIT-REPORT-<date>.md` with: per-item PASS/FAIL + evidence, every finding (with
severity), and a final verdict. If the audit finds a real defect, **fix it through the crew** (spec →
implement → verify → guard → PR → auto-merge), don't just log it. If everything passes, record the
audit as the DONE-evidence for the W2 epic. Surface (don't bury) the two genuinely-remaining items:
P1b (live-apply + reboot durability — human wall) and P3 (env-ctl seam — cross-repo, weave #127).

## Hard rules (carried)
- NO-DOWNGRADE: a field lane can't express is a porter TASK, not a skip. Verify BOTH directions.
- NEVER fake a green — verify against the real binary / nmcli / committed snapshot, never a reread of our own claims.
- Owner blanket approval applies to lane workstream items — fix straight through to merged.
- Host mutation stays a human wall (sudo/reboot) → NEEDS-HUMAN if reached.
