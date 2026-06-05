<h1 align="center">🛣️ lane</h1>

<p align="center">
  Clean, trusted HTTPS local domains for your dev servers — and one-command public sharing.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.82%2B-CE412B?style=flat-square&logo=rust&logoColor=white" alt="Rust 1.82+">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-111827?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/license-PolyForm%20Shield-0f172a?style=flat-square" alt="License">
</p>

```
myapp.test        → localhost:3000
myapp.test/api    → localhost:8080
dashboard.test    → localhost:5173
app.loc           → localhost:4000
```

`lane` maps custom local domains to your dev-server ports with real HTTPS (via a
locally-trusted CA) and full WebSocket passthrough for HMR. It's a complete Rust port of
[`slim`](https://github.com/kamranahmedse/slim), rebuilt on `tokio` + `hyper` + `rustls`.

## Install

Build from source (requires Rust 1.82+):

```bash
git clone https://github.com/FlexNetOS/lane.git
cd lane
cargo build --release
install -m0755 target/release/lane /usr/local/bin/lane
```

Or, once releases are published:

```bash
curl -sL https://lane.sh/install.sh | sh
```

## Quick start

```bash
lane start myapp --port 3000
# https://myapp.test → localhost:3000
```

First run provisions a local CA, adds it to your OS trust store, and sets up port
forwarding (80→10080, 443→10443). You may be prompted for your password once.

To share a local port on a public URL:

```bash
lane share --port 3000
# https://cheeky-panda.lane.show
```

## Local usage

Start or stop domains with `lane start` / `lane stop`:

```bash
lane start myapp --port 3000
lane start api --port 8080
lane stop myapp                  # stop one domain
lane stop                        # stop all domains
```

If you don't specify a TLD you get a `.test` domain. Specify a full domain for any TLD:

```bash
lane start app.loc --port 3000   # https://app.loc → localhost:3000
lane start my.demo --port 4000   # https://my.demo → localhost:4000
```

> **Note:** Avoid `.local` — it's reserved for mDNS and can cause slow DNS resolution.

Route different URL paths to different upstream ports on a single domain:

```bash
lane start myapp --port 3000 --route /api=8080 --route /ws=9000
```

Define all services for a project in a `.lane.yaml` at the project root:

```yaml
services:
  - domain: myapp
    port: 3000
    routes:
      - path: /api
        port: 8080
  - domain: dashboard
    port: 5173
  - domain: app.loc
    port: 4000
log_mode: minimal  # full | minimal | off
cors: true         # enable CORS headers on proxied responses
```

```bash
lane up                              # start all services
lane up --config /path/to/.lane.yaml # specify a config path
lane down                            # stop all project services
```

## Internet sharing

Expose a local server to the internet with a public `lane.show` URL. Requires `lane login`.

```bash
lane share --port 3000                              # random subdomain
lane share --port 3000 --subdomain demo             # https://demo.lane.show
lane share --port 3000 --password secret            # password protected
lane share --port 3000 --ttl 30m                    # auto-expires after 30 minutes
lane share --port 3000 --domain myapp.example.com   # custom domain
```

> The hosted tunnel service is not part of this repository — `lane` ships the client and wire
> protocol only. Point it at a compatible server with `LANE_TUNNEL_SERVER` /
> `LANE_TUNNEL_SERVER_API`.

## Logs and diagnostics

```bash
lane list                # inspect running domains and tunnels
lane list --json

lane logs                # view access logs
lane logs --follow myapp # tail logs for a domain
lane logs --flush        # clear the log file

lane doctor              # run diagnostic checks
```

```
$ lane doctor
  ✓  CA certificate        valid, expires 2036-06-02
  ✓  CA trust              trusted by OS
  ✓  Port forwarding       active (80→10080, 443→10443)
  ✓  Hosts: myapp.test    present in /etc/hosts
  !  Daemon                not running
  ✓  Cert: myapp.test     valid, expires 2028-09-05
```

## Updating

Run `lane upgrade` to update to the latest release.

## Uninstall

Remove everything — CA, certs, hosts entries, port-forward rules, and config:

```bash
lane uninstall
```

## How it works

- A `hyper` server listens on `:10080` (redirects to HTTPS) and `:10443` (TLS via `rustls`,
  HTTP/1.1 + HTTP/2). Per-domain certificates are selected by SNI and generated on demand.
- A locally-generated RSA root CA (added to your OS trust store) signs short-lived ECDSA leaf
  certificates for each domain, so browsers trust `https://*.test` with no warnings.
- `iptables` (Linux) or `pf` (macOS) redirect ports 80/443 to the proxy's high ports, so no
  process needs to bind privileged ports long-term.
- The proxy runs as a detached daemon; the CLI talks to it over a Unix-domain socket.
- `lane share` opens a `wss` tunnel and bridges HTTP requests to your local port.

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the full design and module map.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

## License

[PolyForm Shield License 1.0.0](./LICENSE). A Rust port of `slim` by
[Kamran Ahmed](https://github.com/kamranahmedse).

## References and Acknowledgments

- https://github.com/nilbuild/slim
- https://github.com/rtk-ai/rtk
