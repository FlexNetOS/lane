# lane vs. slim

`lane` is a faithful Rust port of [`slim`](https://github.com/kamranahmedse/slim), the
Go tool by [Kamran Ahmed](https://github.com/kamranahmedse) that maps custom local
domains to dev-server ports over trusted HTTPS, with optional public tunnel sharing.

"Faithful port" is meant precisely here. `lane` is not a reimagining or a superset of
slim — it reproduces slim's command surface, flags, output, error strings, log-line
formats, path-routing semantics, certificate parameters, and on-disk layout, with only
the renames required to ship under a different name (`slim` → `lane`, `~/.slim` →
`~/.lane`, `.slim.yaml` → `.lane.yaml`, `*.slim.show` → `*.lane.show`, and so on). What
changes is the implementation language and its libraries: Go's runtime and standard
library give way to `tokio`, `hyper`, and `rustls`. Behavior does not change.

This document covers three things:

1. The **command parity table** — every command and its flags, identical between the two.
2. The **implementation mapping** — which Rust crate replaces each Go package.
3. The **rename table** and **what is intentionally not included** (the hosted control
   plane behind `*.lane.show`).

---

## Command parity

Every slim command is present in lane with the same arguments, flags, aliases, output,
and side effects. The only differences are the program name and the renamed paths,
markers, and domains listed in the [rename table](#renames-slim--lane) below.

| Command | Args / flags | Behavior (identical in both) |
|---|---|---|
| `start <name>` | `--port/-p` (required), `--route /path=port` (repeatable), `--log-mode full\|minimal\|off`, `--cors`, `--wait`, `--timeout <dur>` (default `30s`) | Normalize the name (`.test` appended when no dot), validate domain + port, warn if it ends in `.local`, run first-time setup if needed, upsert the domain into config under a file lock, add the `/etc/hosts` entry, ensure the leaf certificate, load port-forwarding, start or reload the daemon, optionally wait for the upstream(s), then print the mapped service(s). `--timeout` requires `--wait`. |
| `stop [name]` | — (`MaximumNArgs(1)`) | With a name: remove that domain (error `"<name> is not running"` if absent), remove its hosts entry, then reload the daemon — or shut it down if it was the last domain (`"Stopped <name> (daemon shut down)"`). With no name: remove every domain, clean all hosts entries, shut the daemon down; prints `"Nothing is running."` when there is nothing to stop, else `"Stopped all domains."`. |
| `up` | `--config/-c <path>` | Discover `.lane.yaml` (walk up from cwd) or load the given path, validate it, print `Using <path>`, run first-time setup, merge every service into config under lock, add hosts + leaf certs per service, load port-forwarding, start or reload the daemon, then print all services. |
| `down` | `--config/-c <path>` | Discover/load `.lane.yaml`, validate, remove only the services it defines (others keep running), clean their hosts entries, reload the daemon (or shut down if nothing remains), then print `"Stopped <n> project service(s)."`. |
| `list` (alias `ls`) | `--json` | Load config; if the daemon is running, reload port-forwarding when needed and probe ingress + per-upstream health concurrently. Render a borderless `DOMAIN / PORT / STATUS` table (statuses: `● reachable`, `● unreachable`, `● ingress down`, `-`) plus an active-tunnel table fetched from the API. `--json` emits `{ "domains": [...], "tunnels": [...] }`. Prints a guidance line when nothing exists or the proxy is not running. |
| `logs [name]` | `--follow/-f`, `--flush` | Tail `access.log`, optionally filtered by domain (substring match) and followed `tail -f`-style (100 ms poll). `--flush` truncates the log (`"Cleared access logs."`); it cannot combine with `--follow` or a name filter. Color-formats both 4-field (minimal) and 7-field (full) tab-separated lines, coloring status by class (5xx red, 4xx yellow, 3xx cyan, else green). |
| `share` | `--port/-p` (required), `--subdomain`, `--password`, `--ttl <dur>`, `--domain` | Validate port and subdomain, require login, open a `wss` tunnel and bridge HTTP to the local port. `--subdomain` and `--domain` are mutually exclusive. Prints the public URL (and custom-domain URL/password if set), streams a per-request access line, and disconnects on Ctrl-C. Pro-only features produce the same upsell text and exit cleanly. |
| `login` | — | Device-style OAuth against the API; prints `"Logged in as <name> (<email>)"`, or `"Already logged in as …"` when the token is unchanged. |
| `logout` | — | Best-effort token revocation against the API, then clears local auth; prints `"Logged out."`. |
| `domain add <domain>` | — | Requires login; POST the domain to the API, then print the DNS A-record instructions (Type/Name/Value), the Cloudflare grey-cloud note, and `Then run: lane domain verify <domain>`. |
| `domain list` | — | Requires login; fetch and render a `DOMAIN / STATUS / ADDED` table (`● active`, `● generating cert`, `● pending`, relative "added" time). Prints guidance when empty. |
| `domain verify <domain>` | — | Requires login; look up the domain ID, POST `…/verify`, and report `verified` / `issuing certificate …` / `done`. Errors `"domain <d> not found — use 'lane domain add' first"` when unknown. |
| `domain remove <domain>` | — | Requires login; DELETE the domain. On `409 Conflict` (active tunnel) prompt to continue, then retry with `?force=true`. Prints `"Removed <domain>"`. |
| `doctor` | — | Print a pass/warn/fail checklist: CA certificate, CA trust, port forwarding, per-domain hosts entries, daemon, and per-domain leaf certs. |
| `upgrade` (alias `update`) | — | Resolve the latest GitHub release, compare to the running version (`"Already up to date"` when equal), download the OS/arch archive, verify its SHA-256 against `checksums.txt`, extract the binary, and replace the running binary (falling back to `sudo install` on permission errors). |
| `uninstall` | — | Re-exec under `sudo --preserve-env=HOME` if not root, then step through: stop daemon, remove CA from the trust store, remove port-forward rules, clean `/etc/hosts`, remove `~/.lane`, remove the binary. Failures per step are reported as `skipped (...)` and do not abort. |
| `version` | — | Print `lane <version>` (slim prints `slim <version>`). |

Cross-cutting behaviors are preserved as well: top-level errors print as a red
`Error: <message>` to stderr with exit code 1; `--timeout requires --wait`,
`--flush cannot be used with --follow`, and the other validation messages are
reproduced verbatim; the daemon detaches and the CLI talks to it over a Unix-domain
socket; the proxy listens on the same high ports (`10080`/`10443`) with 80/443
redirected into them. Where the Go code matched on substrings of error text, the Rust
port keeps that text identical so the ported test suite still asserts the same strings.

---

## Implementation mapping

The port replaces each Go dependency with an idiomatic Rust equivalent. The contract is
behavioral, not structural: the wire format, certificate parameters, and output are
identical even when the underlying mechanics differ.

| Concern | slim (Go) | lane (Rust) | Notes |
|---|---|---|---|
| HTTP server + reverse proxy | `net/http` + `net/http/httputil` (`ReverseProxy`) | `hyper` + `hyper-util` | `hyper_util::server::conn::auto` serves HTTP/1.1 + HTTP/2; reverse-proxying and WebSocket/`Upgrade` passthrough are hand-built on `hyper::upgrade::on` to match `ReverseProxy` semantics. |
| TLS + certificates | `crypto/tls`, `crypto/x509` | `rustls` + `rcgen` (+ `rsa`, `x509-parser`, `rustls-pemfile`) | RSA-2048 root CA signs ECDSA-P256 leaf certs; a `rustls::server::ResolvesServerCert` picks per-domain certs by SNI and generates on demand. Persisted PEM encoding may differ (PKCS#8 vs PKCS#1) — by design, since `lane` owns its own `~/.lane` directory. |
| TLS acceptor glue | (stdlib) | `tokio-rustls` | Wraps accepted TCP connections in the rustls server config. |
| CLI / commands | `spf13/cobra` (+ `spf13/pflag`) | `clap` (derive) | One module per command; subcommands, aliases (`ls`, `update`), required flags, and `version` output map directly. |
| Terminal styling | `charm.land/lipgloss/v2` (+ `lipgloss/table`) | `owo-colors` | `lipgloss` ANSI colors map to `owo-colors` styles (green=2, red=1, yellow=3, cyan=6, magenta=5, faint=dim); the borderless table with a bold+faint header and 2-space right padding is reimplemented. |
| Spinners / step runner | `charmbracelet/huh/spinner` | `indicatif` | `RunSteps` drives an `indicatif` spinner for non-interactive steps and a plain `· name` line for interactive ones. |
| WebSocket tunnel client | `coder/websocket` | `tokio-tungstenite` (`connect_async`) | `wss` registration via a JSON text frame, then binary frames carrying length-prefixed request IDs; 20 s pings and exponential reconnect backoff preserved. |
| Daemonization | `sevlyar/go-daemon` | re-exec + `setsid` (`libc` via tiny `unsafe` wrappers) | The daemon re-execs the binary with `_LANE_DAEMON=1`, calls `setsid` in a `pre_exec` hook, and nulls stdio — replacing go-daemon's fork/reparent dance. |
| HTTP client (auth/domain/list/upgrade) | `net/http` client | `reqwest` (async) | Network-error hints (`is_timeout`, `is_connect`) reproduce the Go `net.Error`/`net.DNSError` hint strings. |
| YAML config | `gopkg.in/yaml.v3` | `serde` + `serde_yaml` | Same field names and `omitempty`/`skip_serializing_if` semantics. |
| File locking | `golang.org/x/sync` / flock | `fs2` (flock on `~/.lane/config.lock`) | Serializes config mutations across processes. |
| Browser open | (platform exec) | `open` crate | Used by the login flow. |
| Durations | Go `time.Duration` parsing | `humantime::parse_duration` | `--ttl` / `--timeout` accept Go-style strings (`30m`, `1h`, `500ms`). |
| Async runtime | goroutines + channels | `tokio` (multi-threaded) | The access-log writer and IPC server become tokio tasks; the CLI is `async`. |

Trust and port-forwarding shell out to the same OS tools as slim: on Linux,
`update-ca-certificates` / `update-ca-trust` for trust and `iptables` NAT `REDIRECT`
for forwarding (chain renamed `SLIM` → `LANE`); on macOS, `security add-trusted-cert`
and a `pf` anchor (renamed `com.slim` → `com.lane`). Like slim, `lane` supports macOS
and Linux only and degrades gracefully elsewhere.

---

## Renames (slim → lane)

These are the only intentional behavioral differences. Everything user-visible that
embedded the name `slim` becomes `lane`.

| Concept | slim | lane |
|---|---|---|
| Binary / command | `slim` | `lane` |
| Base directory | `~/.slim` | `~/.lane` |
| Socket | `~/.slim/slim.sock` | `~/.lane/lane.sock` |
| PID file | `~/.slim/slim.pid` | `~/.lane/lane.pid` |
| Hosts marker | `# slim` | `# lane` |
| iptables chain (Linux) | `SLIM` | `LANE` |
| pf anchor / file (macOS) | `com.slim` / `/etc/pf.anchors/com.slim` | `com.lane` / `/etc/pf.anchors/com.lane` |
| Linux CA anchor basename | `slim.crt` | `lane.crt` |
| CA subject | Org `slim`, CN `slim Root CA` | Org `lane`, CN `lane Root CA` |
| Daemon re-exec marker env | (go-daemon internal) | `_LANE_DAEMON=1` |
| Tunnel-server env vars | `SLIM_TUNNEL_SERVER` / `SLIM_TUNNEL_SERVER_API` | `LANE_TUNNEL_SERVER` / `LANE_TUNNEL_SERVER_API` |
| Default API base | `https://app.slim.sh` | `https://app.lane.sh` |
| Default tunnel server | `wss://app.slim.sh/tunnel` | `wss://app.lane.sh/tunnel` |
| Tunnel display domain | `*.slim.show` | `*.lane.show` |
| Error header | `X-Slim-Error` | `X-Lane-Error` |
| Console log prefix | `[slim]` | `[lane]` |
| Project file | `.slim.yaml` | `.lane.yaml` |
| Release repo | `kamranahmedse/slim` | `lane-sh/lane` (placeholder; set at release) |

The proxy ports are unchanged: `10080` (HTTP, 301 → HTTPS) and `10443` (HTTPS). The
release-artifact naming convention (`<name>_<version>_<os>_<arch>.tar.gz` plus a shared
`checksums.txt`, with `macos` mapped to `darwin`, `x86_64` to `amd64`, `aarch64` to
`arm64`) carries over for `upgrade`.

---

## What is intentionally not included

`lane`, exactly like slim, ships **only the tunnel client and the wire protocol** — not
the hosted control plane that terminates public traffic. The server side behind
`*.slim.show` / `app.slim.sh` (and, by extension, lane's `*.lane.show` / `app.lane.sh`)
is a separate hosted service and is **not part of this repository**.

Concretely, the following are out of scope:

- The **tunnel/edge server** that accepts `wss` registrations, allocates subdomains,
  terminates public TLS, and forwards inbound HTTP frames to connected clients.
- The **account/billing API** backing `lane login`, `lane logout`, the active-tunnel
  list shown by `lane list`, and the `lane domain add|list|verify|remove` commands.
- The **Pro-tier gating** for custom subdomains, custom domains, and password protection
  — `lane` reproduces the client-side upsell messaging slim shows, but the entitlement
  decisions live on the server.
- The **abuse-handling and DNS-provisioning** machinery that the hosted slim.show service
  operates for custom domains.

Because the client and protocol are self-contained, you can point `lane` at any
compatible server implementation through `LANE_TUNNEL_SERVER` (the `wss` endpoint) and
`LANE_TUNNEL_SERVER_API` (the HTTP API base). The defaults assume the hosted service,
but nothing in this repository requires it: the **local** features — trusted HTTPS
domains, path routing, the reverse proxy, certificates, and the daemon — work entirely
offline with no account.

---

## Credits and license

`lane` is a port of **slim** by [Kamran Ahmed](https://github.com/kamranahmedse)
(<https://github.com/kamranahmedse/slim>). All credit for the original design, command
surface, and behavior belongs to the slim authors.

slim is distributed under the
[PolyForm Shield License 1.0.0](https://polyformproject.org/licenses/shield/1.0.0), and
`lane` is released under the **same** license. The PolyForm Shield License permits use,
copying, modification, and distribution, but **prohibits using the software to build a
product or service that competes** with the licensor's offerings, and requires that
licensing/copyright notices be preserved and the license included with any distribution
or derivative work. Anyone shipping or modifying `lane` must keep these notices intact
and include a copy of the license — see [`LICENSE`](../LICENSE).
