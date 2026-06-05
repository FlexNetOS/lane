---
name: intent-driven-development
description: "Orchestrates the lane construction crew (intent-analyst, solution-architect, rust-implementer, verification-engineer, rust-native-guardian) to take a feature intent from idea to verified, Rust-native code. Use for ANY request to add a feature, upgrade, enhance, extend, or change lane, or to run the crew autonomously on a backlog/loop. Also use for follow-up: re-run, run again, continue, update, revise, refine, 'redo the X part', 'based on the previous result', 'keep going', 'next intent', 'work the backlog'. Simple questions about lane may be answered directly without the crew."
---

# Intent-Driven Development Orchestrator — Lane Construction Crew

Coordinate a 5-member agent team that turns a feature intent into verified, convention-clean,
**Rust-native** code for `lane`. The crew designs, implements, and verifies in a tight feedback loop, and
can run autonomously across a backlog of intents.

## Execution Mode: Agent Team (with sub-agent fallback)
The Design → Implement → Verify cycle and its feedback loop are the core; team members coordinate
directly via `SendMessage` and a shared task list. The leader (this session) assembles the team, drives
the loop, and synthesizes results.

**Fallback:** if `TeamCreate`/`SendMessage` are unavailable in the session, run the same pipeline in
**sub-agent mode** — invoke each member with the `Agent` tool (one stage at a time; verifier + guardian
in parallel after implementation), hand artifacts off through `_workspace/` files, and route the
implement↔verify fix loop through the leader instead of peer messages. Because sub-agents may not see
agents/skills created earlier in the same session via the registry, have each agent **Read its
`.claude/agents/<name>.md` and `.claude/skills/<skill>/SKILL.md` directly** and operate on absolute
paths. Everything else (phases, bounded loop, gates) is identical.

## Agent Composition
| Member | Agent Type | Role | Skill | Output |
|--------|-----------|------|-------|--------|
| intent-analyst | intent-analyst (custom) | Intent → verifiable spec + KB task | intent-to-spec | `_workspace/01_intent-analyst_spec.md` |
| solution-architect | solution-architect (custom) | Design + blast radius + plan | lane-feature-design | `_workspace/02_solution-architect_design.md` |
| rust-implementer | rust-implementer (custom) | Idiomatic Rust + unit tests | rust-native-implementation | `src/**` + `_workspace/03_rust-implementer_log.md` |
| verification-engineer | general-purpose | Cross-boundary QA + CI gate | lane-verification | `_workspace/04_verification-engineer_report.md` |
| rust-native-guardian | rust-native-guardian (custom) | Invariant + drift enforcement | rust-native-guard | `_workspace/05_rust-native-guardian_report.md` |

All `Agent`/`TeamCreate` member specs use `model: "opus"`.

## Workflow

### Phase 0: Context Check (follow-up support)
Decide the run mode before doing anything:
1. Check whether `_workspace/` exists.
2. Branch:
   - **`_workspace/` absent** → Initial run. Proceed to Phase 1.
   - **`_workspace/` present + partial-modification request** ("redo the design", "fix the failing
     criterion") → Partial re-run. Keep the workspace; re-invoke only the affected member(s); they read
     their prior artifact and amend it.
   - **`_workspace/` present + new unrelated intent** → Fresh run. Move `_workspace/` to
     `_workspace_<YYYYMMDD_HHMMSS>/` (ask the user for the timestamp or read it from a shell `date`
     call), then Phase 1.
   - **Autonomous / "work the backlog" / "next intent"** → go to Phase 5 (Autonomous Loop).

### Phase 1: Preparation
1. **Enforce the worktree mandate.** lane's CLAUDE.md requires each session's work to happen in an
   isolated git worktree, not the primary `main` checkout. Verify in-sync and ensure a worktree:
   ```bash
   git fetch && git status
   git worktree list
   # if developing directly on the primary main checkout, create one:
   #   meta worktree create <task-name>      (preferred, manages the whole meta workspace)
   #   git worktree add ../.worktrees/<task-name>/lane -b <task-name>
   ```
   Branch from `main`; never force-push `main`.
2. Read the intent. If multiple intents were given, queue them (Phase 5).
3. Create `_workspace/` and save the raw intent to `_workspace/00_input/intent.md`.

### Phase 2: Team Assembly
```
TeamCreate(team_name: "lane-crew", members: [
  { name: "intent-analyst",        agent_type: "intent-analyst",        model: "opus", prompt: "Use the intent-to-spec skill. Turn the intent in _workspace/00_input/intent.md into _workspace/01_intent-analyst_spec.md + a KB task. Acceptance criteria must be mechanically checkable." },
  { name: "solution-architect",    agent_type: "solution-architect",    model: "opus", prompt: "Use the lane-feature-design skill. Read the spec, run blast-radius analysis, write _workspace/02_solution-architect_design.md. Pre-clear zero language drift with the guardian." },
  { name: "rust-implementer",      agent_type: "rust-implementer",      model: "opus", prompt: "Use the rust-native-implementation skill. Implement the design step-by-step in src/, write unit tests, run fmt+clippy, log to _workspace/03. Stay 100% Rust. Report each module ready to the verifier." },
  { name: "verification-engineer", agent_type: "general-purpose",       model: "opus", prompt: "Use the lane-verification skill. Run the CI gate and verify every acceptance criterion incrementally; do cross-boundary checks. Write _workspace/04. Never edit production code." },
  { name: "rust-native-guardian",  agent_type: "rust-native-guardian",  model: "opus", prompt: "Use the rust-native-guard skill. Scan every change for language drift and contract/convention violations. Write _workspace/05. Block on any hard invariant." }
])
```
Register tasks with dependencies:
```
TaskCreate(tasks: [
  { title: "Write spec",        assignee: "intent-analyst" },
  { title: "Design + blast radius", assignee: "solution-architect", depends_on: ["Write spec"] },
  { title: "Pre-clear drift",   assignee: "rust-native-guardian",   depends_on: ["Design + blast radius"] },
  { title: "Implement feature", assignee: "rust-implementer",       depends_on: ["Pre-clear drift"] },
  { title: "Verify (incremental)", assignee: "verification-engineer", depends_on: ["Implement feature"] },
  { title: "Conformance audit", assignee: "rust-native-guardian",   depends_on: ["Implement feature"] },
])
```

### Phase 3: Pipeline (Intent → Spec → Design)
Sequential, with direct clarification:
1. intent-analyst writes the spec, creates the KB task, SendMessages the architect the spec path.
2. solution-architect designs against `ARCHITECTURE.md`, runs blast radius, SendMessages the guardian to
   pre-clear drift, then SendMessages the implementer the ordered step plan.
3. Leader monitors via TaskGet; intervenes only if a member stalls or asks for a decision.

### Phase 4: Build–Verify–Guard Loop (Producer-Reviewer, bounded)
This is the heart. **Max 3 iterations** (prevent infinite loops):
1. rust-implementer implements the next module → fmt+clippy clean → SendMessage verifier + guardian "module <name> ready".
2. verification-engineer runs the gate + checks that module's criteria incrementally; SendMessages
   concrete failures (criterion, file:line, expected vs actual) to the implementer.
3. rust-native-guardian scans the new code for drift/conformance; SendMessages hard violations to the
   implementer immediately. **Drift is an absolute block** — the loop cannot pass while any non-Rust,
   build-coupled file or contract violation remains.
4. implementer fixes reported issues → re-notifies. Repeat per module until all modules done.
5. **Exit condition:** verifier report shows every acceptance criterion PASS + green CI gate, AND
   guardian report shows Rust-native invariant + conformance fully green.
6. If 3 iterations pass without converging: stop, summarize the remaining red items for the user, and
   ask whether to continue, re-scope (back to analyst/architect), or hand off.

### Phase 5: Autonomous Loop (run the crew on a backlog)
For "be agentic / run autonomously / work the backlog / keep adding features":
1. **Source the backlog** — read pending intents from the KB board (`git kb board`, tasks with
   `status: backlog/active`) or `_workspace/00_input/backlog.md` (one intent per line/section).
2. **For each intent**, run Phases 1–4 to completion (each intent gets its own KB task and a green
   gate). Commit the result on the worktree branch with a `[[tasks/<slug>]]` wikilink in the message.
3. **Pace the loop** with the `/loop` skill so it self-drives without burning the session:
   - `/loop /intent-driven-development work the backlog` — self-paced; the model picks the next intent
     each turn until the backlog is dry.
   - Or interval form `/loop 30m /intent-driven-development next intent` for a heartbeat cadence.
   Stop when the backlog is empty or the user interrupts. Report progress after each intent (which
   criteria passed, commit hash) so the loop is observable.
4. **Never let the loop bypass the gate.** An intent is only "done" when its verifier + guardian reports
   are green; a red intent is reported and skipped (left active in the KB), not silently marked done.

### Phase 6: Cleanup & Report
1. Ensure each completed intent's KB task is updated with completion evidence (commit hashes, gate result).
2. SendMessage members to stand down; `TeamDelete`.
3. Preserve `_workspace/` (audit trail). Report per-intent: criteria status, files changed, commits,
   anything left red.
4. Offer feedback collection (Phase 7 of the harness): ask if the result or the crew workflow should improve.

## Data Flow
```
intent → [intent-analyst] → 01_spec ─┐
                                      ↓
                            [solution-architect] → 02_design ──→ (guardian pre-clear)
                                      ↓
                            [rust-implementer] → src/** + 03_log
                                 ↑        │ "module ready"
                  fixes ─────────┘        ├───────────────→ [verification-engineer] → 04_report
                                          └───────────────→ [rust-native-guardian]  → 05_report
                                      ↓ (all green)
                              [Leader] → commit (+KB evidence) → next intent / done
```
Transfer: **Task-based** (coordination/dependencies) + **Message-based** (real-time fix loop) +
**File-based** (`_workspace/` artifacts + the actual `src/` edits). Always use paths rooted at `_workspace/`.

## Error Handling
| Situation | Strategy |
|-----------|----------|
| A member stalls/errors | Leader detects idle → SendMessage to check → restart or replace the member; reassign its open tasks |
| Implement↔verify won't converge in 3 iterations | Stop, summarize red items, ask user: continue / re-scope / hand off |
| Guardian finds drift needing a non-trivial port | Block the loop; escalate to user with the file + proposed Rust home before remediating |
| CI gate red for reasons outside the change (flaky/env) | Verifier retries once; if still red, report with output, don't mark done |
| Behavioral check needs privileges unavailable in sandbox | Verify via tests + inspection; explicitly note what wasn't run live (never fake a pass) |
| Conflicting findings (verifier vs guardian) | Keep both with sources; leader adjudicates — functional vs conformance are different axes |
| `git kb` unavailable | Proceed with `_workspace/` files; note KB step skipped |

## Test Scenarios
### Happy path
1. User: "Add a `--header` flag to `lane start` that injects a response header."
2. analyst writes spec + criteria (flag parses; header appears on proxied response; round-trips in config).
3. architect maps it to `cli/start.rs` + `config` + `proxy/handler.rs`, runs blast radius, plans steps.
4. guardian pre-clears (no non-Rust needed). implementer codes it module-by-module, fmt+clippy clean.
5. verifier confirms each criterion + green gate incrementally; guardian confirms zero drift + conformance.
6. Leader commits with `[[tasks/...]]`, updates KB, reports all criteria PASS.

### Error path (drift caught)
1. During implementation a `.mjs` codegen helper gets added under `src/` to generate a lookup table.
2. guardian's drift scan flags it as a hard block and SendMessages the implementer + leader.
3. Loop pauses; the table is ported to a Rust `const`/`build`-free module; foreign file deleted.
4. Gate re-run green; guardian flips to PASS; loop resumes. Report notes the caught drift + fix.

## Notes
- Team size is 5 (large scope: full feature lifecycle). Keep each member focused; the leader does not implement.
- Only one team is active per session — if a later phase needs a different mix, `TeamDelete` then re-`TeamCreate`.
- This crew **adds new features** beyond the original slim port; `ARCHITECTURE.md` remains the contract
  for ported modules and the style guide for new public APIs.
