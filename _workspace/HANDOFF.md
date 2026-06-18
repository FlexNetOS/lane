# HANDOFF — lane-loop (W2 host network plane, ADR-0003)

```yaml
schema: lane-loop/packet/v1
packet_id: w2-p0p1p2-shipped-2026-06-17
session_id: w2-network-plane-2026-06-17
written_utc: 2026-06-18T01:55:00Z
backlog_status: ACTIVE — W2 P0+P1a+P2 shipped; only non-autonomous work remains (P1b human wall, P3 cross-repo)
in_flight:
  epic: Phase 8 (W2) host network plane — adopt-consume + Rust-native portability (ADR-0003)
  item: none autonomous — P1b (human wall) + P3 (cross-repo seam) remain
  pipeline_stage: milestone-complete (adopt + render + portability all landed/arming)
branch: main (per-feature worktrees+PRs; auto-merge on green)
worktree: ../.worktrees/<item>/lane per item
base_sha: ea58ff3   # origin/main when this packet written; #59 (P1) + #60 (P2) arming on top
drift_status: clean   # rust-native-guardian CLEAN on every slice (no language drift, no new deps)
cargo_gate:
  fmt: pass
  clippy: pass (default + --all-features)
  test: pass (458 default + 44 hostnet net tests at P2)
completed_items:
  - P0a — net::model lossless netplan-v2 superset + round-trip snapshot → PR #56 MERGED.
  - P0b — `lane net adopt` live nmcli reader (hostnet, secret-safe) → PR #57 MERGED.
  - P1a — `lane net apply` additive reconcile renderer (type-level no-flush, dry-run-default, both pre-decisions, live-idempotent) → PR #59 (auto-merge armed).
  - P2 — portability profiles (save/list/show + apply --host) + systemd-networkd renderer (structural no-silent-drop audit) → PR #60 (auto-merge armed).
decisions:
  - "Additive is enforced at the TYPE level: NmcliOp has only Add/Modify, no delete/flush variant exists — lane structurally cannot flush an address it doesn't own."
  - "Runtime-bridge exclusion must be SYMMETRIC (desired + current) or a self-diff spuriously Adds docker0/virbr0/br-* — caught in review, fixed, regression-tested."
  - "networkd no-downgrade is STRUCTURAL: exhaustive-destructure field audit (no `..`) → a new model field is a compile error until rendered-or-gapped. Caught+fixed silent drops of wakeonlan/nameservers/on_link."
  - "Deconfliction with network-control LOCKED by LAYER (lane=on-host plane single writer; network-control=off-host fabric). weave #120/#121; their PR #25."
blockers:
  - "P1b — live MUTATING `lane net apply --apply` + cognitum-seed durability across carrier-bounce + REBOOT. The --apply path is shipped + fail-closed; its EXECUTION needs sudo + a reboot = HUMAN WALL. When run: NEEDS-HUMAN, verify via REAL nmcli/ip/iptables, NEVER `lane doctor` (FlexNetOS/lane#5). Do NOT fake green."
  - "P3 — env-ctl seam is CROSS-REPO + staged. Kicked off via weave #127 (parity evidence + staged-migration proposal). Awaiting env-ctl's choice of seam (committed host-profile vs `lane net apply` call at unlock). Resume when env-ctl replies; keep env-ctl PR #115 working until byte-parity proven."
next_command: "git fetch && gh pr list --state open  # confirm #59/#60 merged; then watch weave inbox for env-ctl reply to #127 (P3). P1b is human-driven (sudo+reboot)."
open_prs:
  - "#59 lane-net-apply — P1 apply (auto-merge armed)"
  - "#60 lane-net-portability — P2 profiles+networkd (auto-merge armed; stacks on #59)"
```

## Resume in one paragraph
W2 (ADR-0003) had a big push this session (owner: "continue with P1a and batch the remaining task to
move faster. Push through"). The full autonomous arc is **shipped**: **adopt** (P0a model #56 + P0b
`lane net adopt` #57, both merged) → **render** (P1a `lane net apply`, additive + dry-run-default, #59
arming) → **portability** (P2 in-repo per-host profiles + systemd-networkd renderer, #60 arming). Every
slice was independently verified + rust-native-guard-clean; two real no-downgrade bugs were caught in
review and fixed (an asymmetric runtime-bridge exclusion that broke apply idempotence; silent drops of
`wakeonlan`/`nameservers`/`on_link` in the networkd renderer — now structurally impossible via an
exhaustive-destructure audit). The `network-control` overlap stays **locked by layer**.

## What remains (NOT single-session-autonomous — that's why the loop stops here, not faked)
- **P1b — HUMAN WALL.** The live *mutating* apply (`lane net apply --apply`, which shells `nmcli` via
  `run_privileged`) is shipped and fail-closed, but **executing** it + proving the cognitum-seed addr
  survives a carrier-bounce and a **reboot** needs sudo + a reboot. Run it human-driven; verify via real
  `nmcli`/`ip`/`iptables`, never `lane doctor`. Write `NEEDS-HUMAN` rather than fabricate a green.
- **P3 — CROSS-REPO, staged.** Migrating env-ctl's `cognitum-seed-net` rendering onto a lane unit is
  kicked off (weave **#127** to env-ctl, with parity evidence + a staged no-downgrade plan that keeps
  env-ctl PR #115 working until byte-parity is proven). It resumes when env-ctl replies. env-ctl keeps
  the trigger/USB-unlock ownership; lane owns the rendering.

## Hard rules carried forward
- NO-DOWNGRADE: lossless; a field lane can't express is a porter TASK, not a skip (enforced structurally now).
- Host-mutating phases: feature-gated (default-off), dry-run-default, additive, fail-closed.
- Human walls (sudo/reboot) STOP the loop → NEEDS-HUMAN, never a faked green.
- Owner blanket approval applies to lane workstream items — build straight through to merged.
- DECONFLICT locked; keep coordinating env-ctl (P3) + network-control via weave.
