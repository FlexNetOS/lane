# `.lane-loop/` — Session handoff schemas, policies, and lifecycle hooks

**Purpose:** Durable, schema-defined state for lane-loop's autonomous cycle runner. Enables
cold-start resume across sessions with zero context loss.

**Origin:** Adapted from `weave/sessions-handoff/` (Handoff Ledger PRD V2). Lane-specific tailoring:
no Rust CLI dependency, no lease engine, focused on the cargo gate + backlog handoff pattern that
already powers lane-loop.

## Structure

```
.lane-loop/
├── README.md                 ← you are here
├── schemas/                  → machine-readable state contracts
│   ├── packet.schema.json    → HANDOFF.md checkpoint schema (lane loop packet)
│   ├── session.schema.json   → lane-loop session state machine
│   └── task.schema.json      → backlog item schema
├── policies/                 → hard gates for the harness
│   └── rules.toml            → policy rules enforced by lane-loop and rust-native-guard
└── hooks/                    → lifecycle events at phase boundaries
    └── lane-events.toml      → gates that fire before/after each loop phase
```

## Usage

**Schema validation:** Run with `jsonschema` or a JSON Schema validator to check HANDOFF.md packets.

**Policy enforcement:** The `lane-loop` skill reads `.lane-loop/policies/rules.toml` and enforces
the hard gates during handoff and completion phases. Do not edit manually; update the TOML file.

**Hook wiring:** The lane-loop orchestrator fires hooks at phase transitions (see `hooks/lane-events.toml`). Each hook defines its name, action, and required gates. Gates are checked before the action runs.

## Relationship to existing harness

| Existing artifact | Purpose | How `.lane-loop/` relates |
|-------------------|---------|--------------------------|
| `_workspace/HANDOFF.md` | Committed checkpoint payload | Validated against `schemas/packet.schema.json` |
| `_workspace/loop_state.md` | Per-session counters + state | Maps to `schemas/session.schema.json` state machine |
| `_workspace/backlog.md` | Backlog list | Items conform to `schemas/task.schema.json` |
| `continuity-steward` agent | Writes HANDOFF.md | Uses schemas for ground-truth capture |
| `session-relay` skill | HAND OFF / RESUME protocol | Reads policies, fires hooks at transitions |
| `rust-native-guardian` | Language drift detection | Enforced by policy `block_rust_native_drift` |

## Design principles

1. **Lean, not heavy.** These schemas describe lane's handoff packet — a compact resume payload,
   not a full ledger or lease engine. Lane already has rust-native-guard for drift.
2. **File-backed truth.** The committed `_workspace/HANDOFF.md` is authoritative; schemas just
   validate its structure. No external binary required.
3. **Policy over process.** Gates in `rules.toml` define what MUST be true before handoff/done.
   Hooks fire at phase boundaries to check those gates.
