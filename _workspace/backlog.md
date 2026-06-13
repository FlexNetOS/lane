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

## Phase 7: Feature Inspiration (from external tool survey)

Derived from study of ngrok, cloudflared, chisel, frp, mkcert, acme-lib, caddy, traefik
and others in `docs/reference/repositories.md`. Each is a discrete backlog item for the
lane-loop crew.

### High priority — Round A ✅ ALL SHIPPED (PR #26, fmt-merge #27; verified in code 2026-06-13)
- [x] Add `lane cert key-type` — choose RSA vs ECDSA-P256/P384 (like mkcert `-key-type`). SHIPPED: `KeyType::{Rsa2048,EcdsaP256,EcdsaP384}` enum in `src/cert/mod.rs` + CLI `src/cli/cert.rs`.
- [x] Add `lane cert wildcard` — generate `*.test` wildcard certs (like mkcert `-hostname "*.test"`). SHIPPED: `generate_wildcard_cert()` + `lane cert wildcard <domain>` subcommand.
- [x] Add `lane doctor --fix` — auto-heal CA trust / stale leaves / orphan hosts / stale socket. SHIPPED: auto-regenerate CA/trust/hosts/leaf on Fail checks (`src/doctor/mod.rs`, `src/cli/doctor.rs`).
- [x] Custom SAN support (`--san IP,...`) on start. SHIPPED: `parse_extra_sans()` with IP/DNS auto-detection (`src/cli/start.rs`, `src/cert/mod.rs`).

### Medium priority — Round B (NOT started; each ~1 crate)
- [ ] ACME integration (`--acme` flag on `start`) — use acme-lib to obtain a real public cert from Let's Encrypt for the local domain if DNS is configured. Would need DNS-01 or HTTP-01 challenge support. Affects: new `src/acme.rs` module, CLI `start` args, certificate generation flow.
- [x] Reverse tunnel syntax (`lane share R:3000:localhost:8080` — chisel-style) — SHIPPED: new `src/tunnel/forward.rs` (`ForwardSpec` parser + FromStr, 8 tests) + threaded `local_host` through the tunnel client forward path (`ClientOptions.local_host`, empty⇒localhost) + `lane share [FORWARD]` positional arg with exactly-one-of `--port`/spec resolution. NO wire-format change needed (forward target is a client-side decision — `remotePort` is advisory since lane assigns the public URL). ARCHITECTURE.md + docs/commands.md updated; +12 tests (240 green). Second Phase-7 Round B item (Phase A2).
- [x] Service file generation (`lane install --service`) — SHIPPED: new `src/service.rs` (pure systemd-unit/launchd-plist renderers + user-level `install()`) + `src/cli/install.rs` (`--service`/`--enable`/`--print`/`--json`); ExecStart re-execs the binary with `_LANE_DAEMON=1`; ARCHITECTURE.md + docs/commands.md updated; +5 tests (228 total, green). First Phase-7 Round B item (Phase A2).
- [ ] `lane config template` — generate project configs from templates (inspired by consul-template), useful for teams sharing `.lane.yaml` patterns. Affects: `src/config/project`, new `template` subcommand, template engine integration.

### Lower priority (nice-to-have, larger scope)
- [ ] Multi-hop tunnel support — proxy chains through intermediate hosts (gost-style). For developers behind NAT/firewall who need to share a local port through their company's VPN. Affects: tunnel wire protocol, client/server state machine.
- [ ] Request inspection TUI (`lane inspect` — ngrok web UI pattern via IPC) — connect to running proxy daemon and view/modify request/response payloads. Affects: `src/daemon/socket`, new `inspect.rs` CLI module, TUI rendering (comfy-table or crossterm).

<!-- Discovered 2026-06-08 from external tool survey in docs/reference/repositories.md -->

## SEQUENCING (owner directive 2026-06-13): Phase A → Phase B → Phase C

The Option-B seam is **accepted design** but is **GATED**. Build order is fixed:
- **Phase A (NOW — prerequisites, parallel):** **A1** obscura implementation/integration (separate
  repo; build+verify its 8 crates + MCP/CDP surface — it's a real engine, NOT empty) · **A2** finish
  lane **Phase 7 Round B** (this repo). ← *all current lane implementation work lives here.*
- **Phase B (after A1 ∧ A2):** the `lane web` governed-egress seam (Option B). Design done
  (`docs/adr/ADR-0001-lane-obscura-network-seam.md`); do **not** start until A is complete.
- **Phase C (after B, separate ADR):** cross-machine **lane relay** (the standing wall).

## Phase 8: The W2 network mandate (STRATEGIC — the "bigger than the slim port" vision)

Surfaced 2026-06-13 by tracing lane's intent/vision across the meta census
(`ARCHITECTURE-TRUTH.md`, `NORTH-STAR.md`, `UPGRADE-MISSION-PROMPT.md`, `GAP-REGISTER.md`,
2026-06-12) + lane's `.handoff/context/capsule.json`. lane is the estate's **network plane
(Tier B)**; north-star = *"lane owns network engineering/control; obscura upgrades it with
stealth agent web access."* Full trace: [`docs/VISION.md`](../docs/VISION.md). This is
**Phase-7's ceiling, not churn** — it is the chartered W2 workstream.

- [~] **lane↔obscura seam ADR (W2 deliverable)** — DRAFTED 2026-06-13 at
  `docs/adr/ADR-0001-lane-obscura-network-seam.md` (Proposed; recommends Option B = governed-egress
  proxy seam). Awaiting owner ratification. → then implement.
- [ ] **Governed `lane web` surface** (feature-gated `obscura`) — spawn obscura as a managed child,
  pin its egress through lane, gate every op via a pure deny-by-default `webpolicy` (SSRF/loopback
  validator), log via lane's access-log. Mirrors weave's WL-049/ADR-0002 seam shape. Affects: new
  `src/web/` (+ `webpolicy`), CLI `lane web`, daemon dispatcher, config (`obscura_*`).
- [ ] **lane relay — cross-machine networking (the STANDING WALL)** — close the census-flagged gap
  (`NEEDS-HUMAN.md` "lane relay unfinished → cross-machine paths unreliable"; `GAP-REGISTER.md`
  "lane relay (cross-machine) still unfinished — standing wall"). Reliable trusted connectivity across
  the RuVector edge fleet (cloud→desktop→browser→P2P→ESP32). Needs its own ADR. Reference crates:
  n0-computer/iroh + dumbpipe (QUIC/p2p), cloudflare/pingora (proxy core), rustdesk relay pattern —
  see `network_hub/README.md` "Project Referances" + `docs/VISION.md`.
- [ ] **Back the empty `network_hub` registry** + re-tag obscura C→B once it carries real integration.

NOTE: Phase 8 items are NOT autonomous-loop churn candidates — the seam ADR needs owner ratification
and the relay needs a design ADR first. Round B (Phase 7 medium/lower) remains the unattended-loop
backlog; Phase 8 is owner-gated strategic work.
