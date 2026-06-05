---
name: lane-feature-design
description: "Designs how a lane feature is implemented: maps the spec onto the ARCHITECTURE.md module contract, runs code-intelligence blast-radius analysis (kb_callers/kb_impact), and produces a file-by-file implementation plan with risk-ranked steps. Use after a spec exists and before implementation. Do NOT use to scope intent (intent-to-spec) or to write code (rust-native-implementation)."
---

# Lane Feature Design

Produce a precise, low-risk plan that respects lane's binding contract (`ARCHITECTURE.md`) and its
Rust-native conventions, so the implementer never has to guess where code goes or what it breaks.

## Why blast radius first
lane is a tightly-integrated single crate. Changing a shared signature/type without knowing its callers
is the main source of churn. Code intelligence understands the call graph (AST), not just text — use it
before proposing any signature or struct-field change.

## Procedure
1. **Read** the spec (`_workspace/01_*`) and the relevant `ARCHITECTURE.md` section(s) for the modules
   you expect to touch. The doc's signatures are the contract — match them or propose an explicit addition.
2. **Locate** existing symbols and structure with code intelligence (prefer MCP `kb_*`; CLI fallback shown):
   ```bash
   git-kb code symbols --json --file src/<module>/mod.rs   # what's there
   git-kb code callers <symbol> --json                     # who calls it
   git-kb code callees <symbol> --json                     # what it calls
   git-kb code impact src/<module>/<file>.rs --json        # transitive dependents
   ```
   If a path is unindexed, run `git kb index <path>` once, then retry.
3. **Decide placement** — extend an existing module when possible; a new module must fit the
   `ARCHITECTURE.md` map and mirror the `mod.rs` + submodule pattern of its neighbors.
4. **Specify new/changed signatures** in lane's style: `anyhow::Result<T>`, async where the contract is
   async, `u16` ports validated via `i64`, output via `crate::term`/`crate::log`.
5. **Rank risk** per step by caller count and write an ordered plan (leaf callers first).
6. **Plan tests** — name the unit tests and any behavioral check the verifier will run.
7. **Pre-clear drift** with the guardian: confirm the approach needs zero non-Rust code.

## Risk rubric (from caller analysis)
| Callers | Risk | Action |
|---------|------|--------|
| 0–2, same module | Low | proceed |
| 3–10, multiple modules | Medium | update all call sites carefully, ensure tests |
| 10+ or public API | High | require leader/user confirmation before implementing |

## Design template (write to `_workspace/02_solution-architect_design.md`)
```markdown
# Design: <feature title>

## Approach (2–4 sentences)
<the chosen approach and why, vs. alternatives rejected>

## Module changes
| Module / file | Change | New/changed signatures |
|---------------|--------|------------------------|
| src/<...> | <add/modify> | `pub fn ...` |

## Blast radius
| Symbol | Callers | Risk | Notes |
|--------|---------|------|-------|
| <symbol> | <n, where> | low/med/high | <call sites to update> |

## Step plan (ordered, leaf-first)
1. [low] <step> — files: <...>
2. [med] <step> — files: <...>

## Test strategy
- Unit: <module::tests asserts ...>
- Behavioral: <command the verifier runs>

## Contract notes
- ARCHITECTURE.md additions implied: <new public API to document, or "none">
- Drift check: <confirmed zero non-Rust code needed>
```

## Anti-patterns
- Don't propose a new dependency when an already-present crate (see `Cargo.toml`) does the job.
- Don't design across module boundaries you didn't blast-radius — surprise callers cause rework.
