# lane — Product Requirements

## What it is

`lane` gives you clean, trusted **HTTPS** local domains for your dev servers, and
one-command public sharing of a local port. It is a faithful Rust port of the Go
tool **slim**, re-implemented on `tokio` + `hyper` + `rustls`.

```
myapp.test        → localhost:3000
myapp.test/api    → localhost:8080
dashboard.test    → localhost:5173
app.loc           → localhost:4000
```

## Why

Local dev over `http://localhost:PORT` is noisy and breaks anything that needs a
real domain or HTTPS (cookies, OAuth callbacks, secure-context web APIs, HMR over
WSS). `lane` fronts your apps with a locally-trusted CA so `https://myapp.test`
Just Works — no browser warnings, no per-project nginx, no editing certs by hand.

## Goals (functional parity with slim)

1. **`lane start <name> --port N`** — map `name.test` (or any TLD) to `localhost:N`,
   with optional `--route /path=port` path routing, `--cors`, `--log-mode`, and
   `--wait`/`--timeout` for upstream readiness. First run auto-provisions the CA,
   OS trust, and port-forwarding.
2. **`lane stop [name]`** — stop one domain or everything (shuts the daemon when empty).
3. **`lane up` / `lane down`** — start/stop all services from a discovered `.lane.yaml`.
4. **`lane list [--json]`** — table of domains (with reachability) and active tunnels.
5. **`lane logs [name] [-f] [--flush]`** — colorized access logs, follow, filter, clear.
6. **`lane share --port N [--subdomain|--domain|--password|--ttl]`** — expose a local
   port via a `*.lane.show` tunnel (requires `lane login`).
7. **`lane login` / `lane logout`** — device-style OAuth against the lane API.
8. **`lane domain add|list|verify|remove`** — manage custom tunnel domains.
9. **`lane doctor`** — pass/warn/fail diagnostics (CA, trust, port-forward, hosts,
   daemon, leaf certs).
10. **`lane upgrade`** — self-update from GitHub releases with checksum verification.
11. **`lane uninstall`** — remove CA trust, port-forward rules, `/etc/hosts` entries,
    `~/.lane`, and the binary.
12. **`lane version`**.

## Non-goals

- The hosted tunnel/control-plane service (`app.lane.sh`, `*.lane.show`) is **not**
  in this repo — only the client + wire protocol, exactly as in slim. Endpoints are
  configurable via `LANE_TUNNEL_SERVER` / `LANE_TUNNEL_SERVER_API`.
- Windows trust/port-forward (slim supports macOS + Linux only; lane matches that,
  with graceful "unsupported" messages elsewhere).

## Platforms

- **Linux**: CA trust via `update-ca-certificates` / `update-ca-trust`; port-forward
  via `iptables` nat REDIRECT (chain `LANE`).
- **macOS**: CA trust via `security add-trusted-cert`; port-forward via `pf` anchor
  `com.lane`.

## Architecture (summary)

- **Proxy**: `hyper` server on `:10080` (301→HTTPS) and `:10443` (rustls, HTTP/1.1+H2,
  SNI-based per-domain certs), reverse-proxying to `localhost:PORT` with full
  WebSocket/Upgrade passthrough for HMR. Port-forward maps 80→10080 / 443→10443.
- **Certs**: an RSA-2048 root CA (10y) signs ECDSA-P256 leaf certs (825d, auto-renew
  <30d) per domain; CA installed into the OS trust store.
- **Daemon**: the proxy runs detached (re-exec + `setsid`); the CLI talks to it over a
  Unix-domain socket (`~/.lane/lane.sock`) with a small JSON IPC protocol
  (status / reload / shutdown).
- **Tunnel**: a `wss` client registers with the control plane, then bridges raw HTTP
  request/response frames to the local port.

See `ARCHITECTURE.md` for the binding module-by-module API contract.

## Quality bar

- `cargo build`, `cargo clippy -D warnings`, and `cargo fmt --check` all clean.
- The full Go test suite (~4.3k LOC) is ported; `cargo test` green.
- Behavior matches slim down to error strings, log line formats, path-route matching,
  certificate parameters, and on-disk layout (under `~/.lane`).
- README + CI (build/test/clippy/fmt) + release workflow + install script.
