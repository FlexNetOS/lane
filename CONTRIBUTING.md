# Contributing to lane

Thanks for your interest in improving `lane` — clean, trusted HTTPS local domains
for dev servers (`myapp.test → localhost:3000`) plus one-command public sharing.

`lane` is a faithful Rust port of the Go tool [`slim`](https://github.com/kamranahmedse/slim),
rebuilt on `tokio` + `hyper` + `rustls`. The single most important rule for
contributors flows directly from that: **behavior must match `slim`**. Before you
change anything, read the rest of this guide and [`ARCHITECTURE.md`](./ARCHITECTURE.md),
which is the binding cross-module API and behavior contract.

---

## Development setup

### Toolchain

`lane` pins its toolchain with [`rust-toolchain.toml`](./rust-toolchain.toml):

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

If you have [`rustup`](https://rustup.rs) installed, the correct stable channel and
the `rustfmt` + `clippy` components are selected automatically the first time you run
a `cargo` command in this repo — you do not need to install anything by hand. The
crate's minimum supported Rust version (MSRV) is **1.82**, declared as
`rust-version = "1.82"` in [`Cargo.toml`](./Cargo.toml); please do not use language
or standard-library features newer than that.

### Clone and build

```bash
git clone https://github.com/lane-sh/lane.git
cd lane
cargo build
```

The first build compiles the full dependency tree (TLS, HTTP, websocket, and cert
crates), so it takes a few minutes. Subsequent builds are incremental.

---

## Build, test, lint

These are the four commands CI runs, and the four you should run locally before
opening a PR. They mirror the `## Development` section of the [`README`](./README.md):

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

- **`cargo build`** — compile the `lane` library and binary.
- **`cargo test`** — run the full suite. Many tests are ports of `slim`'s `*_test.go`
  files and assert exact error strings, log formats, and on-disk layout. See
  [Testing](#testing) below.
- **`cargo clippy --all-targets -- -D warnings`** — lint everything (lib, bin, tests,
  examples). Warnings are errors; the build is expected to be clippy-clean. (CI
  achieves the same `-D warnings` denial through `RUSTFLAGS: -D warnings` plus
  `cargo clippy --all-targets --all-features`; the explicit `-- -D warnings` form is
  the convenient local equivalent.)
- **`cargo fmt --all`** — format the whole workspace. CI enforces this with
  `cargo fmt --all --check`, so run it before committing or CI will fail on
  formatting alone.

A quick local pre-flight that matches CI:

```bash
cargo fmt --all --check && \
cargo clippy --all-targets -- -D warnings && \
cargo build && \
cargo test
```

---

## Module layout

The source is organized to mirror `slim`'s Go packages one-to-one, so the two trees
can be diffed against each other. [`ARCHITECTURE.md`](./ARCHITECTURE.md) is the
authoritative module map and public-signature contract — **read it before adding or
moving code**, and keep public signatures matching it so modules integrate without
churn. The high-level shape:

```
src/lib.rs        module declarations only (no logic)
src/main.rs       entrypoint: install rustls crypto provider; dispatch CLI vs daemon
src/config/       paths + config.yaml model, domain/route validation   ⇐ internal/config
src/project/      .lane.yaml discovery and parsing                       ⇐ internal/project
src/osutil/       privileged exec, PATH lookup, geteuid                  ⇐ internal/osutil
src/httperr/      HTTP/network error → human hint mapping                ⇐ internal/httperr
src/term/         colors, confirm prompts, run-steps, tables             ⇐ internal/term
src/log/          async access-log writer + info/error                  ⇐ internal/log
src/protocol/     tunnel wire format (frames + raw HTTP (de)serialize)   ⇐ protocol/
src/tunnel/       wss client, subdomain validation, error pages          ⇐ internal/tunnel
src/cert/         root CA + per-domain leaf certs + OS trust             ⇐ internal/cert
src/system/       /etc/hosts, port-forward (iptables/pf), elevated IO    ⇐ internal/system
src/auth/         device-OAuth login/logout token storage                ⇐ internal/auth
src/proxy/        hyper server, SNI resolver, reverse proxy, health      ⇐ internal/proxy
src/setup/        first-run provisioning, port availability              ⇐ internal/setup
src/doctor/       pass/warn/fail diagnostics                             ⇐ internal/doctor
src/daemon/       detached daemon, Unix-socket IPC                       ⇐ internal/daemon
src/cli/          one handler per command + clap root                    ⇐ cmd/
```

Unit tests live in-module under `#[cfg(test)] mod tests`; cross-cutting tests belong
in `tests/`.

The CLI command set is fixed and matches `slim` exactly. When touching `src/cli/`,
preserve these commands, flags, and aliases:

- `start <name> --port N [--route /path=port ...] [--cors] [--log-mode] [--wait] [--timeout]`
- `stop [name]` (omit name to stop everything)
- `up [--config path]` / `down` (discovered `.lane.yaml`)
- `list` (alias `ls`, `--json`)
- `logs [name] [-f|--follow] [--flush]`
- `share --port N [--subdomain] [--password] [--ttl] [--domain]`
- `login` / `logout`
- `domain add|list|verify|remove`
- `doctor`
- `upgrade` (alias `update`)
- `uninstall`
- `version`

---

## Coding conventions

### Be faithful to `slim`

This is a port, not a redesign. When `slim` and "the obviously nicer Rust way"
disagree, **`slim` wins**, and the Go source under
`/home/drdave/Downloads/slim-extract/slim-main` (read-only reference) is the source
of truth. In particular, preserve:

- **Error message text.** Tests assert on substrings; keep human-facing strings
  identical where practical.
- **Ordering and edge cases** — domain normalization, longest-prefix path routing,
  validation rules, and table/log column order.
- **On-disk layout** under `~/.lane` and **wire formats** (tunnel frames, IPC JSON
  field names, certificate parameters).
- **The `slim → lane` renames** in every new identifier, path, and string. The
  canonical rename table is in [`ARCHITECTURE.md`](./ARCHITECTURE.md): binary `lane`,
  base dir `~/.lane`, socket `lane.sock`, pid `lane.pid`, hosts marker `# lane`,
  iptables chain `LANE`, pf anchor `com.lane`, project file `.lane.yaml`, tunnel
  display domain `*.lane.show`, error header `X-Lane-Error`, console prefix `[lane]`,
  daemon env `_LANE_DAEMON`, tunnel env `LANE_TUNNEL_SERVER` /
  `LANE_TUNNEL_SERVER_API`, and proxy ports `10080` / `10443` (kept identical to slim).

### Errors

Functions return `anyhow::Result<T>` unless a more specific type is genuinely
warranted (`thiserror` is available for those cases). Reproduce Go's error wording;
do not "improve" messages that tests or `slim` parity depend on.

### Output

User-facing output goes through `crate::term` and `crate::log`, never raw
`eprintln!`/`println!` (except the narrow spots where Go used
`fmt.Fprintf(os.Stderr, ...)` directly, e.g. the top-level `Error:` print). The
console log prefix is `[lane]`.

### `unsafe`

Allowed only for libc calls (`geteuid`, `setsid`, `umask`) behind tiny wrappers.
Don't introduce `unsafe` elsewhere.

### Dependencies

**Do not add new dependencies without discussion first.** The dependency set in
[`Cargo.toml`](./Cargo.toml) is deliberate (it maps to `slim`'s Go dependencies and
keeps the release binary small — note the size-optimized `[profile.release]`). If you
think you need a new crate, open an issue or raise it in your PR description before
adding it, and prefer reusing what is already present.

### Formatting and lints

Run `cargo fmt --all` and keep `cargo clippy --all-targets -- -D warnings` clean. Do
not sprinkle `#[allow(...)]` to silence clippy; fix the underlying issue or justify
the allow in review.

---

## Running lane locally without trashing your system

`lane` provisions a CA into your OS trust store, edits `/etc/hosts`, and installs
port-forward rules (`iptables` on Linux, `pf` on macOS). You almost never want a
development build doing that to your real machine. Two facts make safe local testing
possible:

### 1. `~/.lane` isolation via `HOME`

Every path `lane` uses is derived from the home directory
(`config::dir()` resolves `~/.lane`). Point `HOME` at a throwaway directory and the
CA, certs, config, socket, pid, logs, and auth token all land there instead of your
real `~/.lane`:

```bash
export HOME=$(mktemp -d)
cargo run -- start myapp --port 3000
# ... CA / certs / config now live under $HOME/.lane, not your real home
```

The privileged, system-mutating steps (OS trust install, `/etc/hosts` edits, port
forwarding) are still real and still prompt for `sudo`. For routine development you
can simply avoid commands that trigger first-run provisioning, work against the
`HOME`-isolated tree, and exercise the proxy directly on its high ports
(`:10443` / `:10080`) without the privileged 80/443 redirect.

### 2. The daemon

The proxy runs as a **detached daemon**; the CLI talks to it over the Unix socket
`~/.lane/lane.sock`. The daemon is the same binary re-exec'd with `_LANE_DAEMON=1`.
To run the daemon body in the foreground for debugging (no detach, logs to your
terminal):

```bash
_LANE_DAEMON=1 cargo run
```

Stop a running daemon cleanly with `lane stop` (with no args, this stops all domains
and shuts the daemon down). If you isolated `HOME`, just delete the temp directory
when you're done; nothing persists outside it except the system-level changes you
explicitly allowed via `sudo`.

### 3. `LANE_TUNNEL_SERVER` for `share` / `login`

The hosted tunnel and control-plane service (`app.lane.sh`, `*.lane.show`) is **not**
part of this repository — `lane` ships only the client and wire protocol. To test
`share`, `login`, or the `domain` commands, point them at a compatible server:

```bash
export LANE_TUNNEL_SERVER=wss://localhost:9999/tunnel   # tunnel websocket endpoint
export LANE_TUNNEL_SERVER_API=https://localhost:9999    # API base (auth, domains, list)
```

Without these, the defaults (`wss://app.lane.sh/tunnel` and `https://app.lane.sh`)
apply. Never commit credentials or point CI at a production tunnel server.

---

## Testing

`lane` ports `slim`'s test suite and aims to preserve its assertions. Follow the same
patterns the existing tests use:

- **`HOME`-isolated tempdirs.** Tests that touch config, certs, or any `~/.lane` path
  create a `tempfile::TempDir` and set `HOME` to it, so they never read or write your
  real home directory. See `src/config/settings.rs`, `src/auth/mod.rs`, and
  `src/log/mod.rs` for the `with_temp_home()` / `isolate_home()` helpers.
- **`serial_test` for global state.** Anything that mutates process-global state —
  the `HOME` environment variable, or the global async access-log writer — must be
  annotated with `#[serial_test::serial]` so those tests do not race each other under
  parallel execution. `serial_test` is a dev-dependency for exactly this.
- **Pure-function extraction.** Where `slim` used package-level function-pointer seams
  for mocking (e.g. host-file IO), prefer extracting the pure logic (e.g.
  `compute_added(content, name) -> String`) and testing that directly, with the real
  IO wired separately. This keeps the bulk of the logic testable without touching the
  filesystem or requiring privileges.
- **Real listeners for network code.** Proxy and health-check tests spin up real
  `tokio` TCP listeners on ephemeral ports rather than mocking the network.

Run the suite with `cargo test`. If you add behavior, add or port the corresponding
test; if you change a ported behavior, update the assertions to keep them matching
`slim`.

---

## Pull requests and CI

### Before you open a PR

1. Run the four checks locally (`fmt --check`, `clippy -D warnings`, `build`, `test`)
   — see the pre-flight one-liner above.
2. Keep PRs focused. A behavior change, a refactor, and a new dependency are three
   separate conversations.
3. Describe what `slim` behavior your change corresponds to, and link the relevant
   Go source or `ARCHITECTURE.md` section when the mapping is non-obvious.
4. Don't commit or push unless your change is ready for review; branch off `main`
   rather than committing to it directly.

### CI must be green

CI ([`.github/workflows/ci.yml`](./.github/workflows/ci.yml)) runs on every push to
`main` and on every PR targeting `main`, and **must pass before a PR can merge**:

- **`fmt + clippy`** (Linux / `ubuntu-latest`): `cargo fmt --all --check` and
  `cargo clippy --all-targets --all-features`, with `RUSTFLAGS: -D warnings` so any
  lint fails the build.
- **`build + test`** on **both** `ubuntu-latest` **and** `macos-latest`:
  `cargo build --verbose` then `cargo test --verbose`. Because the cert/trust and
  port-forward layers are platform-specific (`iptables`/`update-ca-*` on Linux,
  `pf`/`security` on macOS), the matrix covers both OSes — make sure
  `#[cfg(...)]`-gated code compiles and its tests pass on each.

In short: **fmt + clippy + build + test, green on Linux and macOS.** Releases are cut
by [`.github/workflows/release.yml`](./.github/workflows/release.yml) on `v*` tags and
publish `lane_<version>_<os>_<arch>.tar.gz` artifacts plus `checksums.txt`; that
workflow is not part of normal PR review.

---

## License

By contributing you agree that your contributions are licensed under the
[PolyForm Shield License 1.0.0](./LICENSE), the same license as the project.
`lane` is a Rust port of `slim` by [Kamran Ahmed](https://github.com/kamranahmedse).
