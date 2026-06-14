# Phase A1 — obscura estate integration (the gate that un-gates lane's live `lane web`)

DISCOVER 2026-06-13. Repo: `FlexNetOS/obscura` at `~/Desktop/meta/obscura` (8-crate Rust workspace:
obscura-dom/net/browser/cdp/js[real V8]/mcp/cli + obscura). main @ 5a8c0db. No prior loop state.
Stale-but-leave (per owner "carry forward, never discard"): `harness-upgrade` (behind main),
`chore/cargo-fmt` (its work already landed as #1 on main).

Legend: [ ] todo · [~] in-flight · [x] done+verified · [!] blocked: <reason>

## The driving finding (why A1 is real work, not a rubber-stamp)
lane's shipped `lane web` seam (`ObscuraSpawn::plan`, lane `src/web/mod.rs`) emits a CLI contract that
does NOT match real obscura (verified vs `crates/obscura-cli/src/main.rs`). The seam is feature-gated +
inert, so nothing is broken in prod — but it would not work live until reconciled:
- lane emits subcommands `open`/`run`; obscura has `fetch`/`serve`/`mcp` (no `open`/`run`).
- lane emits `--ca <pem>`; obscura has **NO** custom-CA trust (only `danger_accept_invalid_certs(false)`,
  default rustls roots) — **the linchpin gap**: lane terminates TLS with its own CA, so obscura behind
  lane's proxy can't validate without trusting lane's CA.
- lane puts `--stealth` pre-subcommand; obscura's `--stealth` is per-subcommand + a build feature.
- obscura blocks loopback/RFC1918 by default (its SSRF fix #4); lane's proxy is on loopback ⇒ the
  governed spawn needs `--allow_private_network` to reach it.

## Backlog (dependency-ordered)
- [x] **A1-1: green build/test/clippy/fmt baseline** — DONE, obscura PR #2 MERGED (obscura main @ 07d621b).
  Was RED (65 obscura-js test failures + 38 clippy lints + 14 fmt diffs) → now **271 passed / 0 failed**,
  clippy `-D warnings` clean (all 8 crates), fmt clean, build green. Root cause of the 65: test helpers
  never called `run_page_init()` (globalThis.document null); + 1 real engine gap fixed
  (navigator.connection NetworkInformation made an EventTarget — addEventListener, goofish.com regression).
  panic="unwind" V8 protocol preserved. obscura has NO committed PR-time CI (only branch-protection
  "Format"); PR was CLEAN/MERGEABLE so merged directly. Estate convention gate now green.
- [x] **A1-2: obscura gains custom-CA trust** (THE LINCHPIN) — DONE, obscura PR #3 MERGED (main @ 6552a66).
  Global `--ca <PATH>` + env fallback `--ca → LANE_CA → SSL_CERT_FILE`, wired into EVERY egress reqwest
  site (obscura-net client, obscura-js ops/module-loader, stealth wreq CertStore) via
  `add_root_certificate`; `get_or_try_init` fails-closed on bad CA; `load_ca_certs` multi-cert bundle.
  Validation preserved (danger_accept_invalid_certs stays false). Real localhost HTTPS round-trip test
  (rcgen CA + hyper/tokio-rustls) proves fetch SUCCEEDS with CA, FAILS without. 271→282 tests; new crates
  dev-deps only. panic=unwind preserved. obscura CLI now exposes globals: --proxy, --ca, --user-agent,
  --allow-private-network; subcommands fetch/serve/mcp (--stealth is per-subcommand + build feature).
- [x] **A1-3: reconcile lane `ObscuraSpawn::plan()` to obscura's real CLI** — DONE, lane PR #42 MERGED
  (lane main @ fca98b9). Open→`fetch <url>`; Run→`fetch <url> --eval <SCRIPT-CONTENTS>` (obscura `--eval`
  takes a JS string → lane reads the script file at plan time; unreadable → new
  `SpawnPlanError::ScriptUnreadable`, fail-closed); globals `--proxy`/`--ca`/`--allow-private-network`
  (needed: obscura blocks loopback by default, lane's proxy is loopback)/`--user-agent`; `--stealth`
  moved after `fetch` (per-subcommand). web tests + ARCHITECTURE.md + docs/commands.md updated. 352 tests
  default / 351 --features obscura, clippy+fmt clean both. Live path now needs obscura ≥ A1-2's `--ca`.
- [~] **A1-4: estate registration** — owner approved all 3 parts (fork-identity lowest priority):
  - [x] `.meta.yaml` triage: obscura `[untriaged]` → `[network, ai, browser]` — meta PR #35 OPEN,
    auto-merge ARMED, **BLOCKED by a pre-existing meta-main `Format` (cargo fmt) failure** unrelated to
    this one-line YAML change (lands when meta-main goes green; not an A1 blocker — separate meta-repo
    health issue).
  - [x] fork identity → FlexNetOS — obscura PR #4 MERGED. `[workspace.package].repository` →
    github.com/FlexNetOS/obscura + README logo; upstream attribution preserved (Cargo.toml comment +
    network_hub `upstream` field). Release-download URLs left on upstream until FlexNetOS publishes
    releases (flipping now would 404 installs); MangoProxy sponsor links untouched.
  - [x] network_hub registry entry — network_hub PR #1 MERGED. obscura = first catalog entry
    (id=obscura, category=service-endpoint, protocol=ws [CDP :9222], status=beta, upstream
    h4ckf0r0day; MCP stdio/--http :3000 in notes) + entries/obscura.md + README row; version 0.1.0→0.2.0.
    BONUS Rust-native migration (estate standard): scripts/validate.sh + scripts/hub-validate/ crate
    (ported from harness_hub), removed scripts/validate.py, CI validate.yml → Rust validator. Validator
    green. CLOSES the lane Phase-8 "back the empty network_hub registry" item.

## PHASE A1 COMPLETE (2026-06-13) — obscura is a green, CA-capable, estate-registered FlexNetOS tool
All A1 items done. obscura main green (271→282 tests, clippy/fmt clean); custom-CA trust shipped; lane's
`lane web` seam reconciled to obscura's real CLI; MCP surface verified; estate-registered. Only loose end:
meta PR #35 (.meta.yaml triage) auto-merge ARMED but blocked by a PRE-EXISTING meta-main `Format` (cargo
fmt) failure unrelated to the one-line YAML change — lands when meta-main goes green (separate meta health).

NEXT (owner sequence 1→4→3, step 3): return to lane → run the lane DONE-gate (build+release+test+fmt+
clippy green, backlog clear). The live `lane web` path CAN now be un-gated (obscura has --ca + lane emits
the right CLI) — but that's a follow-on feature, not required for the lane DONE-gate. lane Phase-8 relay +
remaining items stay owner-gated (own ADRs).
- [x] **A1-5: exercise the MCP surface** — DONE (verified via existing coverage). obscura's
  `crates/obscura-cli/tests/mcp_client.rs` (16 e2e tests, GREEN in the A1-1 baseline) spawns the real
  `obscura mcp` subprocess and exercises initialize handshake → tools/list (asserts `browser_navigate`
  +others) → tools/call → resources/list → prompts/list → error handling → notifications. The MCP
  surface works as a FlexNetOS tool; skill at obscura `skills/obscura/SKILL.md`.

## DONE-gate for A1 (then return to lane step 3)
obscura green (build/test/clippy/fmt) + A1-2 CA-trust verified + A1-3 lane seam reconciled & green +
A1-4 registered + A1-5 MCP exercised. Then lane's live `lane web` path can be wired (un-gated) and
lane runs its own DONE-gate.
