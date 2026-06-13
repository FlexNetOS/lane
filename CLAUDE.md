# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`lane` maps custom local domains to dev-server ports with real, OS-trusted HTTPS
(`myapp.test → localhost:3000`) plus one-command public tunneling (`lane share`). It is a
**faithful Rust port of the Go tool [`slim`](https://github.com/kamranahmedse/slim)**, rebuilt on
`tokio` + `hyper` + `rustls`. The original Go source is the behavioral source of truth (read-only
reference, when present, at `/home/drdave/Downloads/tmp/router-lane/slim-extract/slim-main`).

`ARCHITECTURE.md` is the **binding cross-module API contract** — public signatures and behavior are
specified there so modules integrate without churn. Read it before changing any module's public API.
When Go behavior is ambiguous, port it exactly: same error-string substrings (tests assert them),
same ordering, same edge cases.

---

## Harness: Intent-Driven Development

**Goal:** Take a feature intent for `lane` from idea to verified, 100% Rust-native code via a 5-member
construction crew (analyst → architect → implementer ⇄ verifier + Rust-native guardian), with bounded
build–verify–guard loops and an autonomous backlog mode.

**Trigger:** For any request to add a feature, upgrade, enhance, extend, or change `lane` — or to run the
crew autonomously on a backlog/loop within one session — use the `intent-driven-development` skill. It
also handles follow-ups (re-run, refine, "next intent", "work the backlog"). For **durable, resumable,
or unattended/self-restarting** operation across sessions ("resume", "pick up the loop", "ralph", "run
unattended until done") use the `lane-loop` skill — it owns the loop + continuity + external `/new`
runner layer and drives the same crew one backlog item per cycle. Simple questions about lane may be
answered directly without either.

**Change history:**
| Date | Change | Target | Reason |
|------|--------|--------|--------|
| 2026-06-04 | Initial setup | All (5 agents, 6 skills) | Construction crew for intent-driven, agentic feature development |
| 2026-06-04 | Add sub-agent fallback note | skills/intent-driven-development | Shakedown found TeamCreate/SendMessage unavailable; documented Agent-tool fallback + read-def/skill-by-path |
| 2026-06-04 | Sync-to-latest + backlog dedup guards | skills/intent-driven-development | Parallel work off a stale base caused a duplicate Cargo.toml [workspace] and a near-miss cli/mod.rs collision; mandate fetch/pull before branching and diff backlog vs origin/main each iteration |
| 2026-06-05 | Add autonomous loop + continuity + self-restart layer | skills/lane-loop, skills/session-relay, agents/continuity-steward, ralph-lane.sh; + trigger de-confliction in skills/intent-driven-development | Harness upgrade kit (`harness_hub/upgrade-kits/lane.md`): chain short sessions over a durable on-disk backlog so any restart resumes cold with zero loss; runner safe-by-default, `LANE_APPLY=1` opt-in for unattended apply (user-authorized) |
| 2026-06-08 | Session handoff upgrade: copy schemas/policies/hooks from weave/sessions-handoff into .lane-loop/; create lane-session-resume skill; strengthen continuity-steward checkpoint format (drift_status + cargo_gate fields); wire lane-loop auto-handoff at cycle end with PreCycle/PreHandoff/PostCycle hook gates; update session-relay to reference new schemas | .lane-loop/, skills/lane-session-resume, agents/continuity-steward, skills/lane-loop, skills/session-relay | weave/sessions-handoff provides schema-defined state contracts (packet/session/task schemas), policy rules, and lifecycle hooks. Adapted for lane's simpler model: no Rust CLI dependency, focused on cargo gate + backlog handoff pattern already in use. Every cycle now produces a durable checkpoint; resume validates against packet schema before proceeding. |

---

## ⚠️ Critical invariant: stay Rust-native

The shipped crate (`src/`, `Cargo.toml`, `Cargo.lock`) MUST remain 100% Rust. There is no
sanctioned second language in the product. Treat any of the following as drift to fix, not accept:

- A new source file in `src/` (or a new tracked, build-coupled file anywhere) in another language —
  `.js`/`.mjs`/`.ts`, `.py`, `.go`, `.omc`, `.ecc`, shell as a build step, etc.
- A build/codegen step that emits or compiles a non-Rust artifact into the binary.
- A dependency or tool that "auto-pushes a new package" in a non-Rust language to make the build work.

**When you find drift:** stop, then transform it to Rust-native and re-sync with the codebase —
port the logic into the appropriate `src/` module per `ARCHITECTURE.md`, wire it through `cargo`,
remove the foreign artifact, and run `cargo fmt && cargo clippy && cargo test`. If the drift looks
intentional or large enough that porting is non-trivial, flag it to the user before proceeding rather
than silently leaving non-Rust code in the product.

**The one sanctioned exception:** `.workflows/*.mjs` are Claude Code *orchestration* scripts (the
`Workflow` tool requires JavaScript). They drove the original Go→Rust port and are **dev tooling, not
product code** — they never ship in the binary and never get `cargo`-built. Do not "port" them to
Rust, and do not let their existence justify any other non-Rust code under `src/`.

---

## ⚠️ Session start: work in a fresh git worktree

Do not develop directly on the primary `main` checkout. Each session, create/enter an isolated
worktree so concurrent work and the meta-workspace stay clean. This repo is a member of the parent
`meta` workspace, whose worktrees live under `~/Desktop/meta/.worktrees/<name>/lane`.

```bash
# Verify in-sync first (this session began clean & up to date with origin/main):
git fetch && git status

# Create an isolated worktree for the session's task (preferred: meta tooling):
meta worktree create <task-name>        # manages the worktree across the whole meta workspace
# …or plain git from the lane repo root:
git worktree add ../.worktrees/<task-name>/lane -b <task-name>

git worktree list                        # confirm where you are
```

There is already a `harness-upgrade` worktree at `~/Desktop/meta/.worktrees/harness-upgrade/lane`.
Branch from `main`, do the work in the worktree, open a PR; never force-push `main`.

---

## Build, test, lint

MSRV is **1.82**; toolchain is pinned (`rust-toolchain.toml`, stable + rustfmt + clippy). CI runs
fmt-check, clippy (with `-D warnings`), and build+test on ubuntu + macos.

```bash
cargo build                                   # debug build
cargo build --release                         # optimized (opt-level=z, LTO, panic=abort, stripped)
cargo test                                     # all tests (37 in-module #[cfg(test)] suites)
cargo test <name>                              # tests whose name matches <name>
cargo test --lib config::tests                 # one module's tests
cargo test -- --exact path::to::test_fn         # a single exact test
cargo clippy --all-targets -- -D warnings      # lint as CI does
cargo fmt --all                                # format (CI checks --check)
```

Tests that mutate global state — the access-log writer or the `HOME` env var (config/cert paths
resolve via `HOME`/`dirs::home_dir()`) — are marked `#[serial_test::serial]`. Follow that pattern:
isolate filesystem state with `tempfile::TempDir` + an overridden `HOME`, and serialize anything
touching process-global state. Tests live in-module under `#[cfg(test)] mod tests`; there is no
top-level `tests/` directory yet.

### Running the tool

```bash
cargo run -- start myapp --port 3000          # https://myapp.test → :3000
cargo run -- doctor                            # diagnostics
cargo run -- list --json
```

First run provisions a local CA, adds it to the OS trust store, and sets up 80→10080 / 443→10443
port forwarding — it may prompt for a password and touch `/etc/hosts`, the OS trust store, and
`iptables`/`pf`. Prefer an isolated `HOME` and the tunnel env overrides when exercising paths
locally (see `CONTRIBUTING.md`):

```bash
export HOME=$(mktemp -d)
export LANE_TUNNEL_SERVER=wss://localhost:9999/tunnel
export LANE_TUNNEL_SERVER_API=https://localhost:9999
```

---

## Architecture (big picture)

Single crate, `lib` (`src/lib.rs`) + `bin` (`src/main.rs`), `#[tokio::main]`. The CLI is fully async.

**Two process roles in one binary.** `main.rs` installs the rustls ring crypto provider once, then
branches: if `_LANE_DAEMON=1` is set it runs `daemon::run_foreground()` (the long-lived proxy);
otherwise it runs `cli::run()`. `cli` talks to the daemon over a **Unix-domain socket**
(`~/.lane/lane.sock`) using JSON IPC (`daemon::protocol`: shutdown / status / reload). `run_detached`
re-execs the binary with `_LANE_DAEMON=1`, `setsid`, and null stdio to spawn the detached daemon.

**Request path (proxy).** `proxy::Server` (state behind `Arc` + `tokio::sync::RwLock`) binds two
ports: `:10080` 301-redirects everything to HTTPS; `:10443` terminates TLS via `tokio-rustls` and
serves HTTP/1.1 + HTTP/2 (`hyper_util` auto builder, ALPN `h2,http/1.1`). A custom
`ResolvesServerCert` picks/generates a per-domain leaf cert by SNI (cached, single-flight on first
miss). `handler.rs` normalizes the Host, matches the domain's longest-prefix path route, reverse-
proxies to `http://localhost:{port}`, and does bidirectional WebSocket/Upgrade passthrough via
`hyper::upgrade::on` for HMR. Upstream-down renders a styled 502.

**Certificates / trust.** `cert` generates an RSA-2048 root CA (`rsa` → `rcgen`), signs short-lived
ECDSA P-256 leaves (SAN per domain + loopback IPs), and installs/removes the CA in the OS trust store
(`cert::trust`, cfg-gated: `update-ca-*` on Linux, `security add-trusted-cert` on macOS).

**System mutation.** `system` edits `/etc/hosts` (entries marked `# lane`), writes files with a
`sudo tee` fallback, and manages port forwarding behind a `PortForwarder` trait (Linux `iptables` nat
chain `LANE`; macOS `pf` anchor `com.lane`). `osutil::run_privileged` runs direct as root else via
`sudo`. These are the privileged, hard-to-reverse operations — change them carefully.

**Tunnel (`lane share`).** `tunnel::Client` dials `wss` (`tokio-tungstenite`), registers via JSON,
then receives binary frames. `protocol` defines the wire format: a 4-byte BE request id + raw HTTP
bytes. The client decodes a frame → parses the request (`httparse`) → forwards to localhost
(`reqwest`) → serializes the response → re-frames it back. Pings every 20s; reconnects with
exponential backoff; honors close codes 4000 (TTL) / 4001 (dropped). The hosted tunnel server is
**not** in this repo — only the client + wire protocol ship here.

**Module map** (`Rust module ⇐ Go package`): `config` (+`paths`, `project`), `osutil`, `httperr`,
`term` (owo-colors + comfy-table UI), `log` (async access-log writer), `protocol`, `tunnel`, `cert`
(+`trust`), `system` (`hostfile`/`portfwd`/`elevated`), `auth`, `proxy` (`server`/`handler`/
`health`/`pages`), `setup`, `doctor`, `daemon` (`socket`/`protocol`), `cli` (one file per
subcommand). `ARCHITECTURE.md` has the full signature-level contract for each.

**Conventions baked into the port:**
- Functions return `anyhow::Result<T>` unless a more specific type is noted; reproduce Go error text.
- Ports are validated as `i64` (so out-of-range CLI input yields the exact Go error) then stored as `u16`.
- User-facing output goes through `crate::term` / `crate::log`, not raw `eprintln!`.
- `unsafe` only for tiny libc wrappers (`geteuid`, `setsid`, `umask`).
- The `slim → lane` rename table in `ARCHITECTURE.md` (paths, sockets, env vars, markers, chains,
  anchors, the `.lane.yaml` project file) must be applied consistently — keep proxy ports at
  10080/10443.

---

## Meta-workspace note

`lane` is its own independent git repo (`git@github.com:FlexNetOS/lane.git`) that happens to live
inside the parent `meta` workspace. It is **not** itself a meta-repo (no `.meta.yaml` here), so for
work scoped to lane, plain `cargo`/`git` in this directory is correct. Use `meta git` / `meta exec`
only when you intend to act across the whole multi-repo workspace.

---

## Harness: autonomous / resumable operation

lane's harness **already runs** autonomous, resumable, self-restarting operation via the `lane-loop`
skill (shipped): a durable on-disk backlog → one item per cycle → hand off to a fresh session at a
cycle budget → optional fully-unattended self-restart with a clean context each cycle (the "/new"
effect, via `.claude/skills/lane-loop/scripts/ralph-lane.sh`, `LANE_APPLY=1` to opt into unattended
apply). Truth lives on disk (`_workspace/` backlog + checkpoints + commits), so any restart resumes
cold with zero loss. It drives the existing crew per backlog item — it does not reimplement it.

The source pattern and tailoring sheet that produced this layer (consult when evolving the harness):

- Generic pattern + templates: `~/Desktop/meta/HARNESS-UPGRADE-KIT.md`
- Tailored kit for THIS repo:  `~/Desktop/meta/harness_hub/upgrade-kits/lane.md`
- Built on the existing crew skills: `intent-driven-development`, `lane-verification`,
  `rust-native-guard` (don't duplicate them).
