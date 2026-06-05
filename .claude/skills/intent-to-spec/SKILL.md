---
name: intent-to-spec
description: "Converts a raw feature intent for the lane project into a concrete, verifiable spec with goals, non-goals, and objectively-checkable acceptance criteria, recorded as a FlexNetOS KB task. Use whenever an intent/feature-request/upgrade for lane needs to be scoped before any design or code. Do NOT use for designing the implementation (that is lane-feature-design) or for trivial one-line fixes."
---

# Intent → Spec

Turn a human intent into a spec the crew can build and mechanically verify against. A good spec makes
the difference between building the right thing and building a plausible wrong thing.

## Why a spec first
`lane`'s process discipline (see `.kb/AGENTS.md`) is document-first: the task document IS the plan and
survives context resets. The acceptance criteria you write here are what the verification-engineer later
checks one by one — so they must be checkable by a different agent without your context.

## Procedure
1. **Restate** the intent in one sentence. If you cannot, the intent is too vague — ask 2–3 clarifying
   questions before continuing.
2. **Scope** it: list explicit non-goals. The cheapest way to ship is to agree on what you are *not* doing.
3. **Map** the intent to lane's surface area at a high level using the `ARCHITECTURE.md` module map
   (cli / config / proxy / cert / tunnel / daemon / system / doctor …). Name areas, not signatures.
4. **Write acceptance criteria** — each one objectively checkable (a command to run, a file/behavior to
   observe). See the rule below.
5. **Record** the spec to `_workspace/01_intent-analyst_spec.md` AND create a KB task.

## Acceptance-criterion rule
Each criterion must name *how it is verified*. Compare:

- Bad: "Password protection works on shared tunnels."
- Good: "`lane share --port 3000 --password secret` prints a URL; requesting it without the password
  returns 401; with `Authorization`/the password it returns the proxied 200."

- Bad: "Config supports a new `headers` option."
- Good: "A `.lane.yaml` with `headers: {X-Foo: bar}` round-trips through `config::load`/`save` (unit
  test), and a proxied response includes `X-Foo: bar` (behavioral)."

If a criterion is not checkable, it is a goal, not a criterion — move it.

## Spec template (write exactly this structure)
```markdown
# Spec: <feature title>

## Intent (one sentence)
<restated intent>

## Goals
- <what success looks like, bulleted>

## Non-goals / out of scope
- <explicitly excluded>

## Affected areas (high level)
- <module/command from ARCHITECTURE.md> — <why>

## Acceptance criteria
- [ ] <objectively checkable criterion, with how to verify>
- [ ] ...

## Open questions
- <only if genuinely blocking; otherwise omit>
```

## KB task creation
Record the spec as a durable task so it survives session resets and other agents can pick it up:
```bash
git kb create task --slug tasks/<kebab-feature> --title "<feature title>"
git kb checkout tasks/<kebab-feature>
# write the spec body into .kb/workspace/tasks/<kebab-feature>.md (goals + acceptance criteria)
git kb commit -m "Spec: <feature title>"
```
If `git kb` is unavailable, still write the `_workspace/01_*` file and note that the KB step was skipped.

## Sizing
If the intent has more than ~7 acceptance criteria or spans multiple distinct modules, split it: keep a
primary task and list child tasks. Tell the leader so the architect plans them in dependency order.
