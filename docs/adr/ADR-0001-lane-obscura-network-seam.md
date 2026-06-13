# ADR-0001 — The lane ↔ obscura network seam

- **Status:** Proposed (Draft) — W2 deliverable
- **Date:** 2026-06-13
- **Deciders:** FlexNetOS (owner) · lane maintainers
- **Workstream:** W2 (network) of the estate upgrade mission
- **Related:** [`docs/VISION.md`](../VISION.md) · meta census `ARCHITECTURE-TRUTH.md` (2026-06-12) ·
  `GAP-REGISTER.md` item 4 · obscura (`FlexNetOS/obscura`)

> This is the first network ADR in the estate. It exists to convert the census north-star —
> *"lane owns network engineering/control; obscura upgrades it with stealth agent web access"* —
> from a one-line aspiration into a concrete integration boundary that can be built and verified.

---

## Context

### What the estate has decided (the givens)
The meta-workspace census assigns lane and obscura to the same plane (`5-Feature:network`) with a
fixed relationship:

- **lane** = network engineering / control. Today: a TLS-terminating local-domain reverse proxy +
  tunnel client (the slim port), `tokio`/`hyper`/`rustls`, a daemon over a Unix socket, a local CA
  + OS trust, and `/etc/hosts` + `iptables`/`pf` port-forward management. It already *owns trust,
  certs, routing, proxying, and tunneling* on the local machine.
- **obscura** = a Rust headless-browser engine (dom/net/browser/cdp/js/mcp/cli; real V8; CDP;
  Puppeteer/Playwright drop-in; anti-detect/stealth). It exposes an **MCP** surface for agents.
  Today it is a pure mirror fork with zero org commits — a capability we *have* but have not *wired*.
- The vision phrase is **"agent web-access capability under lane's network control"** — i.e. obscura
  is the *engine*, lane is the *governor*.

### The problem this ADR must settle
obscura, used directly by an agent, is ungoverned web egress: arbitrary navigation, arbitrary
upstreams, no trust/policy/observability boundary, no relationship to the network lane already
controls. The estate does not want "an agent with a browser." It wants **a browser whose every
request is subject to lane's network policy, trust, and observability** — and, eventually, one that
works across the fleet's machines, not just localhost.

So the seam must answer three questions:

1. **Boundary** — what does lane own vs. what does obscura own, and how do they talk?
2. **Governance** — how does *every* obscura web request become subject to lane's control (allow/deny,
   trust, logging) rather than bypassing it?
3. **Reach** — how does this compose with the still-unbuilt **cross-machine "lane relay"** so an agent
   on one node can drive governed web access through another?

### Constraints (non-negotiable)
- **100% Rust-native** in both shipped crates (lane's `⚠️ Critical invariant`). No second language in
  the product. (Orchestration `.mjs` are dev tooling, exempt.)
- **lane stays the network authority.** obscura must not open its own ungoverned egress when run under
  lane.
- **Deny-by-default**, SSRF-safe. (Compare: weave's WL-049/ADR-0002 web-access seam already uses a
  pure deny-by-default + SSRF/loopback validator; lane's seam should not be *weaker*.)
- **Optional + feature-gated.** A lane build without the integration must compile and behave exactly as
  today (dependency-light invariant; mirrors obscura being compiled out).
- **No downgrade.** This integration is *additive* to the finished slim-parity tool.

---

## Decision

> **NOTE (draft):** the boundary below is the *recommended* seam. Options A–C are recorded so the
> owner can ratify or redirect. The recommendation is **Option B (governed-egress proxy seam)**.

### The seam (recommended — Option B)

**lane is the network control plane; obscura is a managed web-egress engine that lane spawns and
forces through lane's own proxy + policy.** Concretely:

1. **lane owns the obscura process.** A new feature-gated lane surface (`lane web …` CLI + a
   `web`/`browser` module behind the daemon) is the *only* sanctioned way to invoke obscura inside the
   estate. lane spawns the obscura binary (path/flags from lane config, never ambient `$PATH`) — the
   same governed-spawn pattern weave uses for obscura today.
2. **All obscura egress is pinned to lane.** lane launches obscura configured to route its HTTP(S)
   through lane (proxy/upstream pointed at a lane-controlled listener) and to trust **lane's CA**.
   obscura never talks to the open internet directly when run under lane — it talks *through* lane.
   This is what makes "under lane's network control" literally true at the packet level.
3. **A pure policy gate decides every request.** A new pure, I/O-free `webpolicy` module in lane
   (deny-by-default allow-list of `browser_*` operations + an SSRF/loopback/RFC1918/`*.local`/bare-IP
   validator + optional allowed-domains) answers "may this navigation proceed?" *before* obscura acts.
   Exhaustively unit-testable; reused by both the CLI and any MCP/daemon dispatcher. (Architecturally
   parallel to weave's `webpolicy.rs`, so the two seams share a shape.)
4. **lane observes and logs.** Because egress flows through lane, web requests land in lane's existing
   async access-log path — one observable place for all agent web traffic.
5. **Fleet reach is the relay's job, layered on top.** Cross-machine governed web access = an agent on
   node X drives an obscura governed by lane on node Y **over the lane relay** (the standing-wall
   cross-machine transport). The seam is defined so that "local obscura under local lane" and "remote
   obscura under remote lane, reached via relay" are the *same* policy/trust contract — the relay only
   changes the transport, not the governance.

### Surface (proposed, feature-gated `obscura`)
- `lane web open <url>` / `lane web run <script>` — governed navigation/automation (deny-by-default).
- Config: `obscura_bin`, `obscura_stealth`, `obscura_proxy`, `obscura_user_agent` (+ `LANE_OBSCURA_*`
  env overlays); all inert without the feature.
- MCP/daemon dispatcher (`lane_web` op) so agents reach it the same way they reach obscura's MCP —
  but through lane's gate.
- Default build compiles none of it.

### Options considered

| | **A — Library link** | **B — Governed-egress proxy seam (recommended)** | **C — Loose coexistence** |
|---|---|---|---|
| Shape | lane depends on obscura crates directly; calls in-process. | lane spawns obscura as a managed child, pins its egress through lane, gates every op. | Document that "agents may use obscura"; no enforced boundary. |
| "Under lane's control"? | Partial (API-level) | **Yes — packet-level + policy-level** | No |
| Rust-native | Yes (heavy dep coupling) | Yes (process boundary; light coupling) | Yes |
| Cross-machine ready | Hard (in-process can't span nodes) | **Yes — relay swaps transport, contract unchanged** | No |
| Matches existing weave seam | No | **Yes (same governed-spawn + webpolicy shape)** | No |
| Risk | Tight version lock-step; obscura is a fast-moving fork | Process mgmt + proxy plumbing | Fails the vision (ungoverned egress) |

Option **C** fails the mandate (ungoverned egress). Option **A** couples lane to a fast-moving mirror
fork and can't reach across machines. Option **B** keeps a clean process boundary, makes "under lane's
network control" true at the packet level, mirrors the already-proven weave seam, and is the only one
that composes with the lane relay. **B is recommended.**

---

## Consequences

**Positive**
- The census north-star becomes buildable and *verifiable* (you can prove an obscura request was
  policy-gated and flowed through lane).
- One governance/trust/observability contract for all agent web access, local or cross-machine.
- obscura earns its re-tag from C (mirror) to B (owned) once it carries the integration.
- Shares shape with weave's web-access seam → estate-wide consistency, shared review intuition.

**Negative / costs**
- lane gains a process-management + egress-proxy responsibility (more surface, more failure modes).
- Version coupling to obscura's CLI/MCP contract (a fast-moving fork) — mitigated by the process
  boundary and feature-gating.
- The cross-machine story depends on the **lane relay**, which does not exist yet (this ADR scopes the
  *seam*, not the relay).

**Neutral**
- No change to lane's default build or existing behavior — the entire seam is feature-gated.

---

## Open questions (to resolve before Accepted)

1. **Egress mechanism** — HTTP(S)_PROXY pointed at a lane listener, a CONNECT/MITM path (cf. envctl's
   MITM CA stack), or obscura-native `--proxy`? Each has different trust/observability depth.
2. **Trust** — does obscura validate lane's CA (clean), or does lane terminate+re-originate (deeper
   inspection, heavier)? Tie-in with envctl's vault-backed CA.
3. **Relay dependency** — should this ADR block on a `lane relay` ADR, or ship the local seam first and
   layer relay later? (Recommendation: ship local seam first; relay is a separate ADR.)
4. **MCP ownership** — does lane re-expose obscura's MCP ops behind its gate, or proxy obscura's MCP
   server wholesale with a policy shim?
5. **Re-tag timing** — what concrete integration milestone flips obscura C→B in the census?

---

## Status / next steps

This is a **draft for owner ratification**. On acceptance, W2 proceeds: (1) inventory obscura from code
(its MCP/CLI contract), (2) implement the feature-gated `lane web` surface + `webpolicy` gate +
governed spawn, (3) differential-test against weave's seam shape, (4) open the separate **lane relay**
ADR for the cross-machine frontier.
