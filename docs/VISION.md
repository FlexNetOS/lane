# lane — Vision, North-Star & Intent

> **Status:** living document · **Last traced:** 2026-06-13 · **Sources:** the
> meta-workspace census (`ARCHITECTURE-TRUTH.md`, `NORTH-STAR.md`,
> `UPGRADE-MISSION-PROMPT.md`, `GAP-REGISTER.md`, dated 2026-06-12) and lane's own
> `.handoff/context/capsule.json`.

This document exists because lane's *true* role was, until now, recorded **only** in
the workspace census and the handoff capsule — never in lane's own product docs. lane's
README/PRD still describe it as "a faithful Rust port of slim." That is accurate, but it
is the *floor*, not the *ceiling*. This file records the ceiling.

---

## The two altitudes

lane has to be read at two altitudes at once, and they have not been reconciled until now:

| Altitude | What lane is | Where it's written | State |
|---|---|---|---|
| **Product** | A faithful, full-parity Rust port of [`slim`](https://github.com/kamranahmedse/slim): trusted local HTTPS domains (`myapp.test → :3000`) + one-command public tunneling (`lane share`), plus an additive `--json` automation layer and Phase-7 cert features. | `README.md`, `PRD.md`, `docs/comparison-with-slim.md` | **TERMINAL DONE** (slim parity + JSON surface, 2026-06-05) and **Phase-7 Round A shipped** (PR #26/#27). |
| **Fleet** | **The network plane (Tier B) of the FlexNetOS estate** — the first-party layer that *owns network engineering and control*, upgraded by **obscura** (a stealth headless browser) into *governed web access for AI agents*. | the meta census + `.handoff/context/capsule.json` | **Chartered, largely unbuilt** — the seam is greenfield. |

The product altitude is essentially complete. The fleet altitude is the work that remains.

---

## North-star (verbatim)

From `.handoff/context/capsule.json` (`source: ARCHITECTURE-TRUTH.md census 2026-06-12`):

> **"lane owns network engineering/control; obscura upgrades it with stealth agent web access."**
>
> plane: `network` · tier: `B` · next_command: **"W2: lane+obscura seam ADR"**

And the estate-level destination statement (`NORTH-STAR.md`):

> "`RuVector` is the agentic OS this rides on; teri+shimmy give it a swarm-prediction
> engine; **lane+obscura give it the network and the web**; kasetto+envctl give every
> agent its environment and its model credentials."

So lane is not "a dev-server HTTPS helper." It is the estate's **network-and-web-access
substrate for an agent fleet** — and `lane share` / `myapp.test → localhost` are the
first, smallest expression of that, not the whole of it.

---

## The network plane

The FlexNetOS estate is organized into five planes (census `ARCHITECTURE-TRUTH.md`). lane
sits in plane **5-Feature:network**, whose members are:

| Member | Tier | What it is today | Role in the plane |
|---|---|---|---|
| **lane** | B | TLS-terminating local-domain reverse proxy + tunnel client (the slim port). | **Network control** — trust, certs, routing, proxying, tunneling. The plane's spine. |
| **obscura** | C→B | A **real, built** Rust headless-browser engine — **8 crates** (`obscura-browser/cdp/dom/js/mcp/net/cli` + core), 188 commits; real V8; CDP; Puppeteer/Playwright drop-in; anti-detect/stealth. A fork that **exists and builds** but is **not yet integrated/verified as a FlexNetOS tool** (Phase A1). *(Earlier "zero-commit empty mirror" framing was inaccurate.)* | **Web egress** — "agent web-access capability under lane's network control" (`GAP-REGISTER.md`). lane's *upgrade*. |
| **network_hub** | D | Network-topology catalog scaffold; README prose ahead of an empty `registry.json`. | **Catalog** of the plane's tools + the native-Rust reference set. |

The plane's job, as a whole: **give the agent fleet a network it controls and a web it can
reach — safely, observably, and across machines.**

---

## The strategic frontier: cross-machine "lane relay"

The single most load-bearing piece of the fleet vision that lane does **not** yet do is
**cross-machine networking** — and it is flagged as a standing wall in multiple census docs:

- `NEEDS-HUMAN.md`: *"lane relay unfinished → cross-machine paths unreliable."*
- `RUVECTOR-RESEARCH.md`: *"Network issue (**lane will fix**) blocks reliable cross-machine reach."*
- `GAP-REGISTER.md`: *"lane relay (cross-machine) still unfinished — **standing wall**."*

The RuVector edge fleet spans `cloud → desktop → browser → P2P → ESP32`. lane is the named
owner of making that reachable. `myapp.test → localhost:3000` is the loopback case of a much
larger reachability problem: **trusted, controlled connectivity between fleet nodes.** This is
the real headline feature behind the "network engineering/control" mandate.

---

## The W-series: where lane's next work fits

The estate upgrade is organized into eight **parallel** workstreams (`UPGRADE-MISSION-PROMPT.md`,
"each = verify → gap → upgrade → ship"). lane owns **W2**:

> **W2 network (item 4)** — lane = network engineering and control; obscura = lane's upgrade
> (both currently under-triaged). Inventory both from code, **deliver the merged lane+obscura
> vision ADR**, re-tag, fix lane's broken loop harness/stale backlog.

(Siblings, for context: W1 env-control = kasetto+envctl · W3 comms = weave · W4 = teri/shimmy
Rust port · W5 harness = Archon · W6 = rusty-idd · W7 front-door = prompt_hub · W8 = RuVector
integration audit.)

W2's first deliverable — the **lane↔obscura seam ADR** — is drafted at
[`docs/adr/ADR-0001-lane-obscura-network-seam.md`](adr/ADR-0001-lane-obscura-network-seam.md).
As of this writing it is the *only* network ADR in the estate (the handoff `decisions/`
directory holds ADR-0001…0010 with no network entry).

---

## Roadmap (current truth)

**✅ Done**
- Full slim parity (12 PRD goals).
- `--json` automation surface across every command, documented (PRs #15–#25).
- TERMINAL DONE gate green (223/0), 2026-06-05.
- Phase-7 **Round A** (PR #26/#27): `cert key-type` (RSA/ECDSA-P256/P384), wildcard certs,
  `doctor --fix`, `start --san`.
- `.handoff` continuity kernel + P7 ledger-residency guard (PR #28/#29).

**◻️ Near-term — Phase-7 remainder** (each ~1 crate; see `_workspace/backlog.md`)
- ACME / Let's Encrypt (`--acme`) via a Rust-native ACME crate.
- Service-file generation (`lane install --service`; systemd unit / launchd plist).
- Template-driven config (`lane config template`).
- Reverse-tunnel syntax (`lane share R:3000:localhost:8080`, chisel-style).
- Request-inspection TUI (`lane inspect`, ngrok-web-UI pattern over the daemon socket).
- Multi-hop tunnel proxy chains (gost-style).

**🎯 Strategic — the W2 network mandate** (owner sequencing 2026-06-13: **Phase A gates Phase B**)

*Phase A (prerequisites, NOW): **A1** obscura built+integrated as a FlexNetOS tool (real 8-crate engine, not empty — needs estate integration/verify); **A2** lane Phase 7 Round B finished. Only then →*
1. **lane↔obscura seam ADR** (drafted here) → decide the integration boundary.
2. **Governed agent web-access**: obscura as the egress engine invoked under lane's network
   policy/trust.
3. **Cross-machine lane relay**: close the standing wall — reliable, trusted connectivity
   across the edge fleet. Architecture settled in
   [`ADR-0002`](adr/ADR-0002-cross-machine-lane-relay.md) (Proposed; iroh p2p + relay fallback,
   per-node `webpolicy` governance; implementation owner-gated, Phase C).
4. Re-tag obscura B (owned) once it carries real integration, and back the empty
   `network_hub` registry with real entries.

---

## Reference repositories

lane is built by studying the best of the field. There are **two** reference sets:

### 1. lane's own survey — `docs/reference/repositories.md`
~16 named tools across tunneling (ngrok, cloudflared, localtunnel, chisel, frp, gost),
local-CA (mkcert, smallstep, acme-lib, localhost-tls), dev-server HTTPS (vite-plugin-mkcert),
reverse proxy (traefik, caddy), and daemon lifecycle (consul-template, systemd, launchd).
The Go **slim** source is the behavioral source of truth.

> **On-disk note:** the slim Go source actually lives at
> `/home/drdave/Downloads/tmp/router-lane/slim-extract/slim-main` (the path previously cited
> in `CLAUDE.md`/`ARCHITECTURE.md`/`CONTRIBUTING.md` — `…/slim-extract/slim-main` without the
> `tmp/router-lane/` prefix — does not exist). All other repos are named-only (not cloned).

### 2. The workspace-level native-Rust network set — `network_hub/README.md` ("Project Referances")
The estate-level reference list the network plane is meant to mine and wire in
(*"Run deep-research on these sources | if possible - extract crates and wire in"*). The
ones most relevant to lane's mandate:

- **cloudflare/pingora** — fast/reliable network-service library (a candidate proxy core).
- **hyperium/hyper** — lane's actual HTTP dependency.
- **n0-computer/iroh, dumbpipe, net-tools** — Rust QUIC/p2p stack (the cross-machine relay).
- **rustdesk/rustdesk(+server)** — self-hosted remote-desktop/relay pattern.
- **h4ckf0r0day/obscura**, **vercel-labs/agent-browser** — stealth web access / browser
  automation for AI agents (the obscura upgrade).
- **FoxIO-LLC/ja4**, **biandratti/huginn-net** — TLS/HTTP fingerprinting (JA4).
- **bee-san/RustScan, Chleba/netscanner, CramBL/mdns-scanner** — port/network discovery.

This second list is the concrete answer to "what does lane's network plane draw on" — it is
where the cross-machine + agent-web-access capabilities come from, beyond the slim feature set.

---

## TL;DR

lane *is* a finished, faithful slim port — and that is the **starting line**. Its charter is
to be the **network-and-web-access plane** for the FlexNetOS agent fleet: own network
engineering/control, gain stealth governed web egress through obscura, and close the
cross-machine "lane relay" wall. The next concrete step is the **W2 lane↔obscura seam ADR**.
