# ADR-0002 — The cross-machine lane relay

- **Status:** **Accepted + IMPLEMENTED** (owner-ratified + shipped). The feature-gated `relay`
  surface (iroh peer, persistent node identity, deny-by-default trusted-node allowlist, governance-
  across-the-link, `lane relay up/connect/trust/untrust/status`) landed in **PR #47**; configurable
  DERP/relay-server selection (`relay_servers`) landed alongside. The only remaining work is the
  hardware-dependent real-fleet (≥2-host NAT) validation, covered by `docs/relay-validation.md`.
  Phase **C** in ADR-0001's sequencing (A → B → C).
- **Date:** 2026-06-13
- **Deciders:** FlexNetOS (owner) · lane maintainers
- **Workstream:** W2 (network) of the estate upgrade mission
- **Related:** [`docs/VISION.md`](../VISION.md) ("The strategic frontier: cross-machine lane relay") ·
  [`ADR-0001`](ADR-0001-lane-obscura-network-seam.md) (the lane↔obscura seam; §5 "fleet reach is the
  relay's job") · census `NEEDS-HUMAN.md` / `GAP-REGISTER.md` / `RUVECTOR-RESEARCH.md` · the
  `network_hub` "Project Referances" (iroh, dumbpipe, pingora, rustdesk)

> This is the second network ADR in the estate. ADR-0001 made obscura's web egress governable by lane
> on **one machine**. This ADR settles how that same trust/policy/observability contract reaches
> **across machines** — the load-bearing piece of the fleet vision lane is the named owner of and does
> not yet do.

---

## Context

### The standing wall (the givens)
lane is the estate's network plane and the named owner of fleet reachability. The single most
load-bearing thing it does **not** yet do is cross-machine networking, flagged in three census docs:

- `NEEDS-HUMAN.md`: *"lane relay unfinished → cross-machine paths unreliable."*
- `RUVECTOR-RESEARCH.md`: *"Network issue (lane will fix) blocks reliable cross-machine reach."*
- `GAP-REGISTER.md`: *"lane relay (cross-machine) still unfinished — standing wall."*

The RuVector edge fleet spans **`cloud → desktop → browser → P2P → ESP32`**. `myapp.test →
localhost:3000` is just the **loopback case** of a much larger problem: **trusted, controlled
connectivity between fleet nodes** that sit behind different NATs, firewalls, and trust domains.

### What lane has today (and why it isn't the relay)
- **Loopback HTTPS** — local-domain → local-port reverse proxy with a local CA + OS trust.
- **A tunnel _client_** (`lane share`) — `tunnel::Client` dials `wss` to a **hosted tunnel server that
  is NOT in this repo**, registers, and relays one public endpoint to one local port. It is
  **single-hub, ingress-only (public → one local service), and server-dependent.** It is not
  node-to-node fleet connectivity.
- **Trust + system mutation** — CA issuance/trust, `/etc/hosts`, `iptables`/`pf`.

None of this gives **any fleet node reachability to a service on any other node**, across NAT, under
lane's trust + policy + observability. That is the relay.

### The problem this ADR must settle
1. **Transport** — how do two lane nodes establish a connection when both may be behind NAT/firewalls,
   across cloud/desktop/edge, with no assumption of a public IP on either side?
2. **Identity & trust** — how does a node prove who it is, how is the set of trusted fleet nodes
   bounded (deny-by-default), and how does lane's existing CA/TLS trust model extend across the link
   **without weakening** it?
3. **Topology & governance** — mesh vs hub-relay vs hybrid; how does it compose with the existing
   `lane share` tunnel and, critically, with the **ADR-0001 seam** so that "local obscura under local
   lane" and "remote obscura under remote lane, reached via the relay" are the **same** policy/trust
   contract?

### Constraints (non-negotiable)
- **100% Rust-native** in the shipped crate (lane's ⚠️ invariant). Orchestration `.mjs` exempt.
- **lane stays the network authority.** Trust, policy (the pure deny-by-default `webpolicy`), and the
  access-log apply to cross-machine traffic exactly as they do locally.
- **Deny-by-default, SSRF-safe.** A node accepts inbound connections only from an explicit trusted-node
  allowlist; every relayed request is still subject to `webpolicy` at the destination node.
- **NAT/firewall-traversing.** The fleet has no public-IP assumption; the relay must hole-punch and
  fall back to a relay node when direct fails.
- **Composes with ADR-0001 (transport ≠ governance).** Per ADR-0001 §5, the relay only changes the
  transport; governance stays per-node. The seam contract must be identical local vs remote.
- **Optional, feature-gated, no downgrade.** A lane build without the relay compiles and behaves exactly
  as today; the relay is additive to the finished slim-parity tool.

---

## Decision

> **NOTE (proposed):** the architecture below is the **recommended** relay. Options A–D are recorded so
> the owner can ratify or redirect. The recommendation is **Option A (iroh QUIC p2p + relay-node
> fallback), governed per-node by webpolicy, with `lane share` kept as the public-ingress special case.**

### The relay (recommended)

**Every lane node is a relay-capable peer in a trusted fleet mesh. Connectivity is direct p2p when
possible and falls back to a relay node when not; governance (trust, policy, logging) is enforced by
the lane at the _destination_ node, identically to the local case.**

1. **Transport — iroh (QUIC) with built-in NAT traversal + relay fallback.**
   lane gains a relay peer built on **`iroh`** (n0-computer): QUIC streams between nodes addressed by a
   cryptographic **NodeId** (ed25519). iroh does **hole-punching for direct connections** and falls
   back to **relay servers (DERP-style)** when a direct path can't be established — exactly the
   mixed-reachability the cloud↔desktop↔edge fleet needs. **`dumbpipe`** (iroh-based) is the minimal
   pipe primitive to prototype a single relayed stream before the full surface.

2. **Identity & trust — node identity over the link, lane's CA for services.**
   Each lane node has a stable iroh **NodeId** (its fleet identity). A node accepts connections only
   from NodeIds on an explicit **trusted-node allowlist** (deny-by-default; config + `lane relay`
   commands to manage it). **Service TLS is unchanged:** a service reached across the relay is still
   terminated by the **destination node's lane CA** — so the ADR-0001 `--ca` trust model (obscura on
   node Y trusts node-Y-lane's CA) works across the relay with **no new trust assumption**. The relay
   carries bytes between authenticated nodes; it does not become a new CA.

3. **Topology — hybrid (direct-preferred, relay-node fallback).**
   Direct iroh p2p is preferred. When direct fails (symmetric NAT, locked-down firewall) or for
   always-on rendezvous, traffic falls back through a **relay node** — a lane node running in relay
   mode (the rendezvous/relay role, informed by the **rustdesk** self-hosted relay pattern, and
   optionally implemented on the **`pingora`** proxy core for the high-throughput relay-server case).
   The existing **`lane share`** (hosted-server, public ingress) is retained as the **public-ingress
   special case**; the relay is the **general fleet-mesh case**. Where the wire shapes overlap
   (4-byte request id + framed HTTP, per `protocol`), unify; do not fork gratuitously.

4. **Governance composition — per-node, identical contract.**
   A cross-machine request is governed by the **lane at the destination node**: `webpolicy`
   deny-by-default + the access-log, exactly as a local request (and, for `lane web`, through that
   node's GovernedProxy from ADR-0001's live wiring). This makes the seam contract **identical local
   vs remote** — an agent on node X driving obscura governed by lane on node Y over the relay is the
   same policy/trust/observability path as running it locally; only the transport differs.

### Surface (IMPLEMENTED, feature-gated `relay`)
- `lane relay up` — join the fleet mesh as a node (start the iroh peer; print this node's NodeId).
- `lane relay connect <node>/<service>` — open a governed stream to a service on a trusted node.
- `lane relay trust <NodeId>` / `lane relay untrust <NodeId>` / `lane relay status` — manage the
  deny-by-default trusted-node allowlist and show mesh/relay state.
- Config: `relay_node_id`, `relay_trusted_nodes[]`, `relay_mode` (peer|relay), optional
  `relay_servers[]` (DERP fallback). All inert without the `relay` feature.
- Default build compiles none of it; `lane share` and the local proxy are unchanged.

### Options considered

| | **A — iroh QUIC p2p + relay fallback (recommended)** | **B — pingora proxy core** | **C — rustdesk relay pattern** | **D — extend the `lane share` wss tunnel to a mesh** |
|---|---|---|---|---|
| NAT traversal | **Built-in (hole-punch + DERP relay fallback)** | None (it's a proxy framework, not a transport) | Rendezvous + relay (proven) | No — single hub, ingress-only |
| p2p / node-to-node | **Yes (NodeId-addressed)** | No | Yes (desktop-oriented) | No (one public hub) |
| Rust-native | **Yes (n0-computer crates)** | Yes | Yes (heavier, RD-oriented) | Yes (reuses tunnel::Client) |
| Composes with ADR-0001 seam | **Yes — transport-only, CA model unchanged** | As a relay-node impl only | As a pattern | Partially (ingress only) |
| Reuses lane today | New peer; `lane share` kept as ingress case | Could implement the relay node | Pattern reference | **Most reuse**, but wrong shape |
| Role | **Transport + traversal (the core)** | The relay-**server** impl (complement to A) | Architecture **reference** | Public-ingress special case (keep) |

- **D** reuses the most code but is single-hub, ingress-only, and depends on a hosted server not in this
  repo — it does not solve general NAT mesh. Keep it as the public-ingress case, don't stretch it into
  the relay.
- **C** is a proven hole-punch+relay architecture but remote-desktop-oriented and heavy — adopt it as a
  **pattern** for the relay-node role, not a drop-in.
- **B** is an excellent **relay-server** core (high-throughput) but is not a transport and does no NAT
  traversal — it **complements** A (build the relay-node on pingora) rather than replacing it.
- **A** gives direct-when-possible / relay-when-not, NodeId identity, and Rust-native NAT traversal out
  of the box, and keeps lane's CA/TLS model intact across the link. **A is recommended**, with the
  relay-node role informed by C and optionally built on B.

---

## Consequences

**Positive**
- Closes the census standing wall: trusted, controlled connectivity across the cloud↔desktop↔edge fleet.
- The ADR-0001 seam becomes **fleet-wide for free**: governed agent web access on any node, reachable
  from any node, under the same policy/trust/observability contract.
- Rust-native (iroh/dumbpipe); additive and feature-gated — zero impact on the slim-parity tool.

**Negative / risks (and mitigations)**
- **New, heavyweight dependency surface** (iroh QUIC stack) — mitigate: behind the `relay` feature; the
  default build never compiles it; prototype with `dumbpipe` first.
- **Inbound connection security** — accepting node connections is new attack surface — mitigate:
  deny-by-default trusted-node allowlist (NodeId) + `webpolicy` at the destination + lane CA for
  service TLS; never an open relay.
- **Relay-node operations** (a fallback relay must run somewhere) — mitigate: any lane node can be the
  relay; DERP fallback is optional and configurable; document the ops model when implemented.
- **ESP32 / constrained edge** — full QUIC may not fit the smallest nodes — mitigate: those reach the
  fleet **through** a nearby full lane node (gateway pattern), not as first-class iroh peers; scope this
  explicitly in the implementation ADR.

**Sequencing**
- This is **Phase C** (after Phase B, the full seam). **Implementation is owner-gated**; this ADR only
  settles the architecture. A follow-on implementation ADR (or this one, once ratified) will specify the
  wire protocol unification with `lane share`, the iroh integration plan, the relay-node role, and the
  differential/integration test strategy (two-node reachability + governance-across-the-link proofs).

## Status log
| Date | Change |
|------|--------|
| 2026-06-13 | Proposed. Architecture recommendation: Option A (iroh p2p + relay fallback), per-node webpolicy governance, `lane share` kept as the public-ingress case. Awaiting owner ratification; implementation owner-gated (Phase C). |
| 2026-06-13 | **Accepted** — owner ratified Option A. Implementation begun: feature-gated `relay` (iroh peer + NodeId identity + deny-by-default trusted-node allowlist + governed cross-machine streams + two-node reachability/governance tests). |
| 2026-06-14 | **IMPLEMENTED + shipped (PR #47).** Feature-gated `relay`: iroh 0.98 QUIC p2p, persistent NodeId identity, deny-by-default trusted-node allowlist, governance-across-the-link (per-node webpolicy + access-log), `lane relay up/connect/trust/untrust/status`, hermetic two-node + governance tests. Configurable DERP (`relay_servers` → `RelayMode::Custom`) + cross-machine validation runbook (`docs/relay-validation.md`) landed alongside. Remaining: hardware-dependent ≥2-host NAT validation (manual, per the runbook). |
