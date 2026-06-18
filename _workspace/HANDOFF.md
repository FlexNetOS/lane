# HANDOFF — lane-loop (W2 host network plane, ADR-0003)

```yaml
schema: lane-loop/packet/v1
packet_id: w2-p0-complete-2026-06-17
session_id: w2-network-plane-2026-06-17
written_utc: 2026-06-18T00:40:00Z
backlog_status: ACTIVE — W2 P0 "Adopt" milestone COMPLETE; next item is P1a (render engine, autonomous)
in_flight:
  epic: Phase 8 (W2) host network plane — adopt-consume + Rust-native portability (ADR-0003)
  item: P1a — `lane net apply` render engine (additive reconcile DIFF + --dry-run default; NO host mutation)
  pipeline_stage: not-started (design fresh next session — safety-critical additive reconcile)
branch: lane-net-adopt-cli   # P0b; rebase-clean on origin/main. P0a already merged (#56).
worktree: ../.worktrees/lane-net-adopt-cli/lane
base_sha: de5cc4d            # origin/main at handoff (includes P0a #56)
drift_status: clean          # rust-native-guardian CLEAN on both P0a and P0b (no language drift, no new deps)
cargo_gate:
  fmt: pass
  clippy: pass (default + --all-features)
  test: pass (425 default + 13 hostnet)
completed_items:
  - P0a — net::model (lossless netplan-v2 superset) + round-trip committed snapshot → PR #56 MERGED to main. Includes ADR-0003 §Deconfliction lock.
  - P0b — `lane net adopt` live host reader (nmcli-sourced, `hostnet` feature default-off, secret-safe) → PR #57 (auto-merge armed; verified LIVE on box + guard-clean).
decisions:
  - "Adopt sources from nmcli (unprivileged, secret-safe), NOT /etc/netplan (root-only mode 600, may carry secrets) — keeps adopt autonomous + sanitizing."
  - "SecretRef driven by key-mgmt, never by nmcli's masked <hidden> value (nmcli masks ALL wifi secret slots regardless of existence; OWE APs must get no spurious ref)."
  - "Deconfliction with network-control LOCKED by LAYER not device (weave #120/#121; network-control PR #25): lane = single writer to on-host netplan-NM plane; network-control = off-host fabric. ADR §Deconfliction merged #56."
  - "Model always-compiled (pure data, runs in default cargo test); only effectful reader + CLI are hostnet-gated — mirrors obscura 'pure layer always built'."
blockers:
  - "P1b (live apply + cognitum-seed reboot/carrier-bounce durability proof) is a HUMAN WALL: needs sudo + a reboot. When reached → write _workspace/NEEDS-HUMAN, do NOT fake green. Verify via REAL nmcli/ip/iptables, NEVER `lane doctor` (FlexNetOS/lane#5)."
next_command: "git fetch && gh pr view 57 --json state  # if merged, branch P1a from origin/main (NOT stacked); else stack on lane-net-adopt-cli. Then drive crew on P1a."
open_prs:
  - "#57 lane-net-adopt-cli — P0b adopt (auto-merge armed, awaiting required CI)"
```

## Resume in one paragraph
The owner-directed W2 work (ADR-0003: lane adopt-consumes + rust-ports the host network plane so the
meta estate is portable) has its **read-only P0 "Adopt" half COMPLETE**. P0a (the lossless
netplan-v2-superset serde model that round-trips the committed host snapshot) is **merged to main (#56)**.
P0b (`lane net adopt` — a `hostnet`-feature-gated, nmcli-sourced, secret-safe live reader that round-trips
the real box's cognitum-seed link-local unit) is **PR #57 with auto-merge armed**. The
`network-control` ownership overlap is **reconciled and locked** in ADR-0003 §Deconfliction (by layer:
lane owns the on-host netplan-NM plane as single writer; network-control owns the off-host fabric).

## Next: P1 (Render) — split into autonomous P1a + human-wall P1b
- **P1a (do this next, fully autonomous):** `lane net apply` render engine — model → netplan/NM
  apply-plan + **additive reconcile DIFF** (computes add/modify; **NEVER** emits a flush of an
  address/route lane doesn't own) + `lane net apply --dry-run` (DEFAULT) printing the plan with **no
  host mutation**. Pure + fixture-tested; feature-gated under `hostnet`. **Land the two P0b
  pre-decisions (NOT skips):** (1) runtime-bridge **exclusion** (docker0/virbr0/br-*/veth* — so apply
  never fights Docker/libvirt); (2) passthrough **normalization** (decide which NM bookkeeping keys —
  `*.may-fail`, `*.dhcp-send-hostname-deprecated` — are carried vs normalized so diffs stay minimal,
  while staying lossless for semantically-significant keys: never-default, ipv6.method, addresses,
  routes, key-mgmt).
- **P1b (HUMAN WALL):** the live mutating apply (sudo `nmcli`/netplan write) + the cognitum-seed
  durability proof (static 169.254.42.2/24, never-default, durable across **carrier bounce + reboot**).
  Needs sudo + reboot → write `_workspace/NEEDS-HUMAN`, do **not** fake green.

## Hard rules carried forward (from KICKOFF + owner)
- NO-DOWNGRADE: adoption is lossless; a field lane can't express is a porter TASK, not a skip.
- Host-mutating phases (P1+): feature-gated (default-off), dry-run-default, additive, fail-closed.
- Owner blanket approval applies to lane workstream items — build straight through to merged.
- DECONFLICT: locked (above) — keep coordinating with network-control via weave as P1 lands.
