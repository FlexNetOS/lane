# Cross-machine relay validation runbook

The cross-machine relay (`lane relay`, ADR-0002) is proven **hermetically** by the in-process
two-endpoint test in `src/relay/live.rs` — that test exercises the transport and the
governance-across-the-link logic (deny-by-default node trust + the same webpolicy as local traffic)
with `RelayMode::Disabled` and direct loopback addressing, so it needs no network.

What the hermetic test **cannot** cover is real NAT traversal: two hosts on *different* networks
hole-punching (or falling back to a DERP relay) to reach each other. That is hardware- and
network-dependent and therefore not CI-able. This runbook is the **manual operator procedure** for
validating that hardware-dependent path on real machines.

> TL;DR — the automated hermetic test covers the transport + governance logic; this runbook covers
> the real two-host NAT-traversal case.

---

## Prerequisites

- Two hosts, **host A** and **host B**, ideally on different networks (e.g. one behind home NAT,
  one on a cloud VM) so you actually exercise NAT traversal rather than LAN direct addressing.
- A lane binary built **with the relay feature** on *both* hosts:

  ```bash
  cargo build --release --features relay
  ```

  Without `--features relay` the `lane relay up` / `lane relay connect` actions fail closed with
  `rebuild with --features relay`.

- A real service to reach on host B — e.g. a dev server on `127.0.0.1:3000`.
- Each host has a persistent node identity (created automatically on first `lane relay up` at
  `~/.lane/relay/node.key`, mode `0600`). You read a host's **NodeId** from `lane relay up` /
  `lane relay status`.

### Get each host's NodeId

```bash
# On each host:
lane relay status --json     # {"node_id":"<…64-hex…>", …}  (empty node_id until identity exists)
# …or it is printed when the node starts:
lane relay up --json         # {"node_id":"<…>","listening":true,"trusted_count":N}
```

---

## Step-by-step: reach host B's service from host A

The roles: **host B is the service side** (it runs the governed accept loop and exposes a local
service); **host A is the client side** (it bridges a local port to B's service).

### 1. On host B — trust host A and start the node

```bash
# Trust A's NodeId (deny-by-default: B accepts inbound relay connections ONLY from trusted nodes).
lane relay trust <hostA-NodeId>

# Make the local service reachable through B's webpolicy (deny-by-default, same keys as `lane web`):
#   ~/.lane/config.yaml
#     web_allow_hosts: [127.0.0.1]
#     web_allow_ports: [3000]

# Start the node and run the governed accept loop. Note the printed NodeId.
lane relay up --json         # {"node_id":"<hostB-NodeId>","listening":true,"trusted_count":1}
```

Leave `lane relay up` running on host B for the duration of the test.

### 2. On host A — bridge a local port to B's service

`lane relay connect` dials host B by NodeId (iroh discovery + relay locate it across networks) and
bridges a loopback port on A to `host:port` on B. The `host:port` is **on B's side** and is governed
by **B's** webpolicy.

```bash
lane relay connect <hostB-NodeId>/127.0.0.1:3000 --local-port 8080
#   → 127.0.0.1:8080 on host A now bridges to 127.0.0.1:3000 on host B, if B allows it.
```

> If you also want B to be able to reach a service on A (bidirectional), repeat the symmetric setup:
> on host A `lane relay trust <hostB-NodeId>` + `lane relay up`, and on host B
> `lane relay connect <hostA-NodeId>/<host:port>`. For the one-directional A→B case above, A does not
> need to trust B to *initiate* the outbound connect.

### 3. On host A — confirm it reaches the service on host B

```bash
curl -v http://127.0.0.1:8080/
# Expect the response served by 127.0.0.1:3000 on host B.
```

A successful response that originated on B confirms the end-to-end cross-machine relay.

---

## What to verify

### Direct vs relayed path

iroh first attempts **direct** connectivity via UDP hole-punching; if that fails (symmetric NAT,
firewalls), it falls back to a **DERP relay** so the connection still succeeds — at the cost of
routing bytes through the relay. Either way the bridge works; the difference is latency/throughput.

- With the default config (`relay_servers` empty) the fallback uses iroh's **public n0 relays**.
- With `relay_servers` set, the fallback uses **your own self-hosted DERP relays** (see below).

To exercise the relayed path specifically, put the two hosts behind NATs that block direct
hole-punching, or block UDP between them; the connect should still succeed via the relay.

### Governance refusal at the target (host B)

Prove that B's webpolicy actually gates relayed traffic, exactly like local traffic:

```bash
# On host A, ask for a target B does NOT allow (e.g. a port not in B's web_allow_ports):
lane relay connect <hostB-NodeId>/127.0.0.1:9999 --local-port 8081
curl http://127.0.0.1:8081/
```

Expect the bridge to be **refused by the remote node** (the connect errors with
`relay denied by remote node: …`), and on **host B** a log line `relay DENY … (reason)` — with **no
upstream connect** to the denied service. This is the same deny-by-default webpolicy that governs
local traffic, applied across the link at the destination.

### Deny-by-default trust

Prove an untrusted node cannot connect:

```bash
# On host B, remove A from the allowlist:
lane relay untrust <hostA-NodeId>
# On host A, retry the (previously working) connect:
lane relay connect <hostB-NodeId>/127.0.0.1:3000 --local-port 8082
curl http://127.0.0.1:8082/
```

Expect B to reject the connection (it is no longer trusted) and the bridge to fail. With an **empty**
allowlist B trusts nothing — there is no "trust all" and no implicit self-trust.

---

## Self-hosted DERP relays (`relay_servers`)

By default lane uses iroh's public n0 relays for NAT-traversal fallback. To pin **your own** DERP
relay(s) instead — e.g. for a private fleet that must not route through public infrastructure — set
`relay_servers` in `~/.lane/config.yaml` on each host:

```yaml
relay_servers:
  - https://derp.example.test
  - https://derp2.example.test   # optional additional relays
```

This wiring (`relay_mode_from_config` in `src/relay/live.rs`) maps the config to iroh's relay setting:

- **empty** (default) ⇒ public n0 relays (`RelayMode::Default`).
- **one or more URLs** ⇒ pin those self-hosted relays (`RelayMode::Custom`). Invalid entries are
  logged and skipped; if **every** entry is invalid lane **falls back to the public relays** rather
  than dropping NAT traversal — relay connectivity is *availability*, not a security boundary, so it
  is fail-safe. (Security is the deny-by-default trusted-node allowlist + the webpolicy, which are
  unaffected.)
- **`[disabled]`** ⇒ relaying off entirely (direct-only; for direct-reachable deployments).

> Note: `relay_servers` (the DERP relay setting) is **distinct** from `relay_mode` (`peer`|`relay`,
> the node ROLE). They are independent keys.

After editing `relay_servers`, restart `lane relay up` so the new relay mode takes effect, then
re-run the steps above and confirm the relayed path now uses your relay.

---

## Why this is a manual runbook

True NAT traversal needs ≥2 real hosts on different networks with real NAT/firewall behavior between
them — it cannot be reproduced deterministically in CI. The automated hermetic test
(`src/relay/live.rs`) proves the transport and governance logic in-process; this runbook is the
operator validation for the remaining hardware-dependent NAT case.
