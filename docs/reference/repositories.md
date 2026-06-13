# External Repository References — lane Feature Inspiration

Curated list of external GitHub repositories worth studying for future lane features. Grouped by domain. Each entry includes concrete "features worth stealing" that could become backlog items for the `lane-loop` crew.

**Intentionally selective** — only the most relevant repos are listed, not an exhaustive survey.

> **Two reference sets.** This file is lane's *product-feature* survey (tunneling / local-CA /
> reverse-proxy / daemon UX). There is a second, **workspace-level** reference set for lane's
> *fleet* mandate (the network plane: cross-machine relay + governed agent web access) in
> [`network_hub/README.md`](../../../network_hub/README.md) under **"Project Referances: Network
> tools for Native Rust Crates"** — pingora (proxy core), iroh/dumbpipe (QUIC/p2p relay), obscura
> + agent-browser (stealth agent web access), ja4 (TLS fingerprinting), rustdesk (relay). See
> [`docs/VISION.md`](../VISION.md) for how the two sets map to lane's two altitudes.
>
> **On-disk:** only the Go **slim** source is cloned locally, at
> `/home/drdave/Downloads/tmp/router-lane/slim-extract/slim-main`. Every repo below is named-only
> (inspiration targets, not vendored).

## Tunneling & Port Forwarding

| Name | URL | Relevance to lane | Features Worth Stealing |
|---|---|---|---|
| **ngrok** | <https://github.com/ngrok/ngrok> | Dominant tunneling tool; lane's `share` is a local-first competitor. Study NAT traversal, edge routing, session resumption patterns. | Custom domains on free tier, per-request auth, request inspection/payload modification, webhook replay, IP allowlists, session token persistence |
| **cloudflared** | <https://github.com/cloudflare/cloudflared> | Cloudflare's open-source tunnel agent. Study auto-configuration and zero-config public HTTPS as a UX reference for lane's share flow. | Automatic TLS provisioning (no user CA), `tunnel create` workflow, health checks on tunnels, local edge routing |
| **localtunnel/server** | <https://github.com/localtunnel/server> | The original open-source CLI tunnel. Minimal architecture — good reference for the simplest possible tunnel protocol. | Simple `lt --port 3000` one-liner UX, configurable remote server URL, auto-detect host IP |
| **chisel** | <https://github.com/jpillora/chisel> | Client + server in one binary. Study its multiplexing and bidirectional tunnel mode. | Bidirectional tunnel (server+client in one process), SOCKS5 proxy, fast reconnect with session token, reverse tunnel `R:port:host:port` syntax |
| **frp** | <https://github.com/fatedier/frp> | Enterprise-grade port forwarding with K8s integrations. Relevant if lane ever needs self-hosted tunnel server with load balancing. | Multi-port multiplexing over single connection, token-based client auth, TLS full chain (not just SNI), web dashboard |
| **gost** | <https://github.com/go-gost/gost> | Multi-protocol proxy (HTTP/HTTPS/SOCKS/SSH). Study its proxy-chain pattern for multi-hop tunnels. | Proxy chains (hop-to-hop), SSH-based tunneling, WebSocket transport fallback, SNI forwarding |

## Local Certificate Authorities

| Name | URL | Relevance to lane | Features Worth Stealing |
|---|---|---|---|
| **mkcert** | <https://github.com/FiloSottile/mkcert> | Gold standard for local HTTPS certs. lane's cert system is the most likely candidate for mkcert-inspired upgrades. | Custom SAN/CN (`-hostname`, `-key-type`), wildcard certs (`*.test`), auto-detect OS trust API (more platforms), `install-root-ca` non-sudo setup, Windows CA trust |
| **smallstep/certificates** | <https://github.com/smallstep/certificates> | Production-grade PKI (mini Let's Encrypt). Study ACME support for future lane ACME integration with auto-renewal. | ACME/LE client built-in, STAC API, `step certificate` CLI with full key-type/SAN options |
| **acme-lib** | <https://github.com/rustls/acme-lib> | Rust-native ACME client library. Directly relevant — could become the basis for lane's own ACME integration via Let's Encrypt. | Rust-native ACME implementation, HTTP-01/DNS-01/TLS-ALPN-01 challenges, renewal workflow, fits tokio/hyper stack |
| **localhost-tls** | <https://github.com/Cali0707/localhost-tls> | Lightweight shell-based localhost TLS cert generator. Compare its openssl-based approach with lane's rcgen system for edge cases. | Zero-dependency (just openssl), simple CLI workflow, auto-generate root + leaf in one command |

## Dev Server HTTPS & Proxy Tools

| Name | URL | Relevance to lane | Features Worth Stealing |
|---|---|---|---|
| **vite-plugin-mkcert** | <https://github.com/liuweiGL/vite-plugin-mkcert> | Study how mkcert integrates into dev server tooling. lane could ship IDE/dev-tool integration for zero-config HTTPS in Vite/webpack. | Dev-server auto-configuration (no manual config), transparent cert injection, framework-agnostic plugin API |
| **traefik** | <https://github.com/traefik/traefik> | Production reverse proxy with auto-cert. Study middleware chain and entry-point separation as inspiration for lane's future route-level features. | Middleware chain per-route, Docker/K8s auto-discovery, dashboard UI, access logs per-service |
| **caddy** | <https://github.com/caddyserver/caddy> | HTTP server that "just works" with HTTPS by default. Study its auto-HTTPS heuristic and Caddyfile syntax for future lane config improvements. | Zero-config TLS (auto-provisions via Let's Encrypt), Caddyfile domain-first syntax, automatic cert renewal loop, reverse proxy with health checks |

## Infrastructure & Daemon Lifecycle

| Name | URL | Relevance to lane | Features Worth Stealing |
|---|---|---|---|
| **consul-template** | <https://github.com/hashicorp/consul-template> | Config-file templating + file-watch + reload pattern. lane's `.lane.yaml` orchestration could benefit from template-driven config generation. | Config templating with file watches, atomic config replacement, trigger-based reload on cert change, variable interpolation |
| **systemd (socket activation)** | Reference only (built-in) | Study how lane can integrate with systemd for daemon lifecycle (`systemctl enable lane-proxy`). Current: re-exec + setsid; could be more robust. | `Type=exec` with `ExecReload`, watchdog integration, `ListenStream=` socket activation |
| **launchd (plist)** | Reference only (macOS built-in) | macOS daemon management. lane could offer `lane install --service` that generates a `~/Library/LaunchAgents/sh.com.lane.plist`. | `KeepAlive`, `ProgramArguments`, `EnvironmentVariables` for `LANE_TUNNEL_SERVER` |

## Upstream (lane's Original)

| Name | URL | Relevance to lane |
|---|---|---|
| **slim** (Go upstream) | <https://github.com/kamranahmedse/slim> | The original Go tool that lane faithfully ports. Command parity target. See `docs/comparison-with-slim.md` for the full parity table. |
| **k8s.io/client-go** | <https://github.com/kubernetes/client-go> (reference) | If lane ever needs K8s service discovery for auto-routing, this is the gold-standard Go HTTP client + config loading pattern. Not yet actionable. |

## Notes

- When studying an external tool, focus on its **CLI UX patterns** and **protocol choices** rather than implementation details — lane's Rust port should not copy Go idioms or library preferences.
- Any feature inspired by an external tool should be implemented per lane's existing conventions: `ARCHITECTURE.md` module contract, 100% Rust-native (rust-native-guard), slim parity where applicable, and the same error-string style for user-facing messages.
