# Reference Documents

This subdirectory contains reference documents and tooling for lane development.

## Purpose

- **Workflow scripts** (`.mjs`) — Agent-driven automation templates for code generation,
  implementation playbooks, and parallel fan-out work. Consumed by the `intent-driven-development`
  skill during feature implementation. Not user-facing; not designed for manual editing.
- **`repositories.md`** — Curated list of external GitHub repositories worth studying for future
  lane features. Grouped by domain (tunneling, certificates, dev-server HTTPS, infrastructure).

## Documents

| Document | Purpose |
|---|---|
| [`repositories.md`](./repositories.md) | External tools to study for feature inspiration and backlog seeding |
| `wf-reference-playbooks.mjs` | Agent prompt for authoring authoritative implementation playbooks |
| `wf-port-layer.mjs` | Single-module Go-to-Rust porting workflow |
| `wf-fanout-write.mjs` | Multi-agent fan-out template for parallel file writes |

## Using the References

The `.mjs` workflows are consumed by the lane-loop crew during feature development (via
`intent-driven-development`). They are not designed for manual execution.

The `repositories.md` list is updated periodically as new tools emerge in the local-HTTPS,
tunneling, and dev-server space. When considering a new feature, review the relevant category to
see what patterns already exist before designing from scratch.
