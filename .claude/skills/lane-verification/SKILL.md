---
name: lane-verification
description: "How to verify a lane change: run the full CI gate (cargo fmt --check / clippy -D warnings / test), check each acceptance criterion against the real code, perform cross-boundary shape comparisons, and exercise behavior via the binary in an isolated HOME. ALWAYS use when verifying/QA-ing a lane feature or confirming a change works. Do NOT use to write features (rust-native-implementation)."
---

# Lane Verification

Prove the change satisfies the spec. QA is **cross-boundary comparison**, not existence checking — read
the producing side and the consuming side together and compare shapes. "It compiles" hides the bugs that
matter; your job is to find them.

## CI gate (run exactly as CI does)
```bash
cargo fmt --all --check                      # formatting
cargo clippy --all-targets -- -D warnings    # lint, warnings are errors
cargo test                                   # full suite
```
All three must be green. Capture command output excerpts as evidence in the report. (CI runs these on
ubuntu + macos; you run locally — note any OS-specific path you could not exercise.)

## Per-criterion verification
Walk the spec's acceptance criteria one at a time. For each, record verdict + evidence:
- **Logic criterion** → point to the code that satisfies it AND the test that asserts it (file:line).
- **Behavioral criterion** → run it. Use an isolated HOME so you never touch the real machine:
  ```bash
  export HOME=$(mktemp -d)
  export LANE_TUNNEL_SERVER=wss://localhost:9999/tunnel
  export LANE_TUNNEL_SERVER_API=https://localhost:9999
  cargo run -- <command> ...
  ```
  Some paths need privileged setup (OS trust store, port forwarding, `/etc/hosts`) unavailable in a
  sandbox. Verify those via unit/integration tests + code inspection and **explicitly state what could
  not be run live** — never claim a behavioral pass you didn't observe.

## Cross-boundary checklist (where the real bugs live)
Read both sides and compare shapes/field names/types/serde tags:

| Boundary | Producing side | Consuming side | Compare |
|----------|----------------|----------------|---------|
| Config field | `config::Config` + serde tags | every reader + `.lane.yaml` example | name, type, default, skip_serializing |
| Daemon IPC | `daemon::protocol` message struct | CLI sender/parser | variant rename, fields, optionality |
| CLI flag | `clap` derive in `cli/<cmd>.rs` | the handler that uses it | flag name, required, parse type |
| Tunnel wire | `protocol` (de)serialize | client forward/serialize | frame layout, JSON tags |
| New public fn | the definition | every caller (`git-kb code callers`) | arity, types, async |

A mismatch here (e.g. a serde tag that doesn't match the field a reader expects) compiles fine and fails
at runtime — these are the highest-value findings.

## Report format (`_workspace/04_verification-engineer_report.md`)
```markdown
# Verification: <feature title>

## CI gate
- fmt:    PASS/FAIL  <excerpt>
- clippy: PASS/FAIL  <excerpt>
- test:   PASS/FAIL  <N passed / failures>

## Acceptance criteria
| # | Criterion | Verdict | Evidence |
|---|-----------|---------|----------|
| 1 | <criterion> | PASS/FAIL | <command+result or file:line compared> |

## Boundary checks
| Boundary | Verdict | Evidence |

## Blocking issues (for implementer)
- <file:line> — <mismatch> — expected <shape>
## Non-blocking notes
- <nit>
```

## Rules
- Never edit production code to make a check pass — report the gap; the implementer fixes it.
- Verify **incrementally** as each module is reported ready; re-run the cheap full gate each round to
  catch regressions.
- The crew is done only when every criterion is PASS and the gate is green.
