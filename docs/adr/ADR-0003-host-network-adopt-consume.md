# ADR-0003 — lane adopts & consumes the host network plane (portable, Rust-native)

- **Status:** **Accepted (design)** — owner-directed 2026-06-17 ("direct lane to adopt-consume and
  rust port the current box's NM so meta is truly portable"); covered by the owner's standing
  blanket approval for lane workstream items. Implementation is the lane-loop's to sequence; this
  ADR sets the contract and the first-cut design. Supersedes nothing; extends the W2 network charter.
- **Date:** 2026-06-17
- **Deciders:** FlexNetOS (owner) · lane maintainers
- **Workstream:** W2 (network) of the estate upgrade mission
- **Related:** [`docs/VISION.md`](../VISION.md) (network plane) · [`ADR-0001`](ADR-0001-lane-obscura-network-seam.md)
  (one-machine web egress governance) · [`ADR-0002`](ADR-0002-cross-machine-lane-relay.md) (cross-machine
  reach) · adoption input [`docs/adopt/host-nm-snapshot-2026-06-17.md`](../adopt/host-nm-snapshot-2026-06-17.md) ·
  env-ctl `manifest/cognitum-seed-net.toml` (the proof-case host link the meta tree currently configures
  out-of-band) · the meta "network governance overlap" note (envctl/lane/network-control/NetworkManager/netplan)

> ADR-0001 governs web egress on one box; ADR-0002 carries trust across boxes. This ADR closes the
> remaining gap **beneath** both: the host's *base* network configuration itself — interfaces,
> addresses, routes, link-local special cases — is today owned by per-box `NetworkManager`/`netplan`
> files in `/etc`, so the meta estate is **not portable**: re-creating a box means hand-rebuilding its
> network. lane is the named network-plane owner; it should **adopt-consume** that host config and
> re-express it as a Rust-native, declarative, version-controlled model that any box can render.

---

## Context

### The portability gap (the givens)
- The box's network is configured from several owners — `envctl` (manifest components), `lane`,
  `network-control`, `NetworkManager`, `netplan`, and raw host settings. No single source of truth.
- The durable config lives in **per-box `/etc/netplan/90-NM-<uuid>.yaml`** (netplan = source of truth,
  NM = renderer; `/run` keyfiles are generated). It does **not** travel with the repo → not portable.
- The trigger case: env-ctl's `cognitum-seed-net` component must keep a static link-local address on a
  USB NIC for the secrets USB-unlock factor. It works, but env-ctl is reaching *around* the network
  plane (`nmcli`/`netplan` directly) because lane offers no host-network model to render into. That is
  the overlap the owner wants resolved at the right layer.

### What "adopt-consume" means here
1. **Adopt** — ingest the existing host network state (NM connections + netplan YAMLs + interface
   inventory) into a lane-owned model, losslessly (no downgrade: every address, route, match rule,
   never-default flag, autoconnect, link-local mode preserved). See the captured input snapshot.
2. **Consume** — make lane the manager: a Rust-native declarative representation lane can diff, render,
   and reconcile, so the config is reproducible from the repo on a fresh box.

## Decision

lane gains a **host network-plane** capability (feature-gated, default-off, like `obscura`/`relay`):

1. **Model (Rust-native, no-downgrade).** A serde model for the host plane —
   `ethernet`/`wifi`/`link-local`/`bridge?` units with `match {name|mac}`, `addresses`, `routes`,
   `dhcp4/6`, `never-default`, `wakeonlan`, and a `renderer` (`networkmanager` | `networkd`) — a strict
   **superset** of what netplan v2 expresses (so adoption is lossless). Secrets are **references to
   `secretd`**, never inline (PSK/802.1x resolved at render time via env-ctl).
2. **Adopter (`lane net adopt`).** Reads the live host (`nmcli`/`/etc/netplan`/`ip`) → emits the lane
   model. Idempotent; sanitizing (no secret material written). Round-trips the snapshot format.
3. **Renderer (`lane net apply`).** Renders the model to the box's actual manager
   (netplan-NM first, since that is what the estate runs; networkd as a portability target) — keyed by
   **stable `match`+name, never the regenerated UUID** — and reconciles (add/modify/delete) without
   flushing addresses it does not own (the additive discipline env-ctl already follows).
4. **Portability.** The model lives in-repo (per-host profile under `network_hub` or a lane host
   profile); `lane net apply --host <name>` reproduces a box. This is the "meta is truly portable" win.
5. **Seam with env-ctl.** env-ctl stops reaching around the plane: `cognitum-seed-net` becomes a lane
   network unit (special-purpose link-local: additive, never-default, match-by-name). env-ctl keeps the
   *trigger/ownership* (the secrets USB-unlock dependency); lane owns the *config rendering*. Migration
   is staged so env-ctl PR #115 keeps working until the lane unit lands (no regression).

## Sequencing (lane-loop owns execution)
- **P0 — Adopt (read-only):** model + `lane net adopt` + round-trip the snapshot. No host mutation.
- **P1 — Render (netplan-NM):** `lane net apply` for the netplan-NM renderer; reconcile additively;
  prove on the `cognitum-seed` link-local case (bounce/reboot durable, never-default).
- **P2 — Portability profiles:** per-host model in-repo; `--host` reproduction; networkd renderer for
  non-NM boxes.
- **P3 — env-ctl seam migration:** move `cognitum-seed-net` rendering to lane; retire the direct
  `nmcli`/`netplan` reach once parity is proven. Coordinate via weave; no-downgrade gate.

## Consequences
- **+** Single Rust-native source of truth for the host plane; estate becomes reproducible/portable.
- **+** Resolves the multi-manager overlap at the right layer (lane), additively (no manager war).
- **+** Secrets stay in `secretd`; the model is safe to commit.
- **−** lane takes on host-mutating scope (root/`nmcli`/`netplan`) — must be fail-closed, additive,
  dry-run-by-default, and never flush addresses it does not own. Default-off feature gate until proven.
- **−** Cross-repo coordination (lane ↔ env-ctl) for the P3 seam; staged to avoid USB-unlock regression.

## No-downgrade contract
Adoption is **lossless**: every observable in the host snapshot (address, route, match, never-default,
autoconnect, wifi key-mgmt, link-local mode) MUST round-trip adopt→render unchanged before P3 retires
any existing path. A field that lane cannot yet express is a **porter task, not a skip**.
