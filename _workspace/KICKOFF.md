# KICKOFF — W2 host network plane (adopt-consume + portability)

> Paste-ready kickoff for a fresh lane session. Owner-directed 2026-06-17 (relayed via envctl):
> *"adopt-consume and rust port the current box's NM so meta is truly portable."*

Resume the lane-loop on the W2 network-plane work the owner directed: adopt-consume +
rust-port this box's host network config so the meta estate is truly portable.

Everything is already specified and committed on main (merged via PR #54):
- ADR: `docs/adr/ADR-0003-host-network-adopt-consume.md`  (Accepted-design, owner blanket-approved)
- Adoption input (sanitized snapshot of this box): `docs/adopt/host-nm-snapshot-2026-06-17.md`
- Backlog item: `_workspace/backlog.md` → "Phase 8 (W2) — Host network plane: adopt-consume + Rust-native portability"

Read those three first, then run the lane-loop on this item with your normal discipline
(per-feature worktrees under `../.worktrees/`, one PR per slice, auto-merge on green, update
`loop_state.md`). Sequence:

- **P0 — Adopt (READ-ONLY, no host mutation):** a serde model that is a LOSSLESS superset of
  netplan v2 (ethernet/wifi/link-local/bridge units with `match{name|mac}`, addresses, routes,
  dhcp4/6, never-default, wakeonlan, renderer), plus `lane net adopt` that reads the live host
  (`nmcli` / `/etc/netplan` / `ip`) and emits the model. It MUST round-trip the committed snapshot.
  Secrets are REFERENCES to secretd (env-ctl), never inline.
- **P1 — Render:** `lane net apply` for the netplan-NM renderer; additive reconcile (NEVER flush
  addresses lane doesn't own); key by stable match+name, NOT the regenerated NM UUID. Prove it on
  the cognitum-seed link-local case: static `169.254.42.2/24`, never-default, durable across
  carrier bounce + reboot.
- **P2 — Portability:** in-repo per-host profile; `lane net apply --host <name>` reproduces a box;
  add a systemd-networkd renderer for non-NM boxes.
- **P3 — env-ctl seam:** migrate env-ctl's `cognitum-seed-net` rendering onto a lane network unit,
  no-downgrade and STAGED — keep env-ctl (develop, PRs #115/#118) working until parity is proven;
  coordinate over weave.

## Hard rules
- **NO-DOWNGRADE:** adoption is lossless — every address/route/match/never-default/autoconnect/
  wifi-key-mgmt/link-local mode must round-trip adopt→render unchanged before P3 retires any
  existing path. A field lane can't yet express is a porter TASK, not a skip.
- Host-mutating phases (P1+) are feature-gated (default-off, like obscura/relay), fail-closed,
  dry-run by default, additive only.
- **DECONFLICT:** the `network-control` repo is on branch `omada-migration-records` doing network
  work concurrently — reconcile ownership of the host plane with it in P0; don't both claim it.
- Owner blanket approval applies to lane workstream items — build straight through to merged,
  don't gate on approvals.

## Trigger context
env-ctl currently renders the Seed link-local addr via `nmcli`/`netplan` directly
(env-ctl `manifest/cognitum-seed-net.toml`) because lane offers no host-network model to render
into. ADR-0003 moves that to the right layer. **Start at P0.**
