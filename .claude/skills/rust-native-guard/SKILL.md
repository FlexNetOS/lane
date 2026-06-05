---
name: rust-native-guard
description: "How to enforce lane's invariants on a change: scan for language drift (any non-Rust source/.omc/.ecc/build step pulled into the crate), verify ARCHITECTURE.md contract conformance, convention adherence, and slim→lane rename consistency — then drive Rust-native remediation. ALWAYS use when auditing a lane change for drift/conformance or when a non-Rust file appears. Do NOT use for functional QA (lane-verification)."
---

# Rust-Native Guard

Enforce lane's identity. The prime directive: the shipped crate stays **100% Rust**. Be adversarial —
assume drift is present until the scan proves otherwise.

## Drift scan (run first, every time)
Detect non-Rust source or build coupling introduced by the change:
```bash
# Files changed in the working tree / branch:
git status --short
git diff --name-only        # (or: git diff --name-only main...HEAD)

# Any non-Rust, non-doc source added under the crate? (sanctioned exception: *.mjs workflow scripts)
git diff --name-only | grep -Ev '\.(rs|md|toml|lock|html|json|yaml|yml|gitignore)$|^\.workflows/|^docs/reference/wf-'

# Build coupling smells — a build.rs or non-Rust codegen step:
ls build.rs 2>/dev/null; grep -rn "build = \|Command::new(\"node\"\|\.py\"\|\.mjs\"" Cargo.toml src/ 2>/dev/null
```
Any hit from the second command is **drift** — a non-Rust, build-coupled file. Treat `.omc`, `.ecc`, and
unknown extensions as drift by default.

### Sanctioned exception (do not flag)
`.workflows/*.mjs` and `docs/reference/wf-*.mjs` are Claude Code orchestration scripts (the Workflow
tool requires JavaScript). They never ship in the binary and are never `cargo`-built. They are the *only*
sanctioned non-Rust files. They never justify other non-Rust code under `src/`.

## Remediation (when drift is found)
1. **Stop the loop** — drift is a hard block, not a nit.
2. If the change is small and clearly portable: port the logic into the correct `src/` module per
   `ARCHITECTURE.md`, wire it through `cargo`, delete the foreign artifact.
3. If the port is non-trivial or the drift looks intentional: escalate to the leader/user with the
   file, what it does, and the proposed Rust home — do not silently leave it or silently rip it out.
4. Re-run the gate after remediation: `cargo fmt --all --check && cargo clippy --all-targets -- -D
   warnings && cargo test`.

## Conformance checklist (beyond drift)
| Invariant | How to check | Hard? |
|-----------|--------------|-------|
| Public signatures match `ARCHITECTURE.md` | compare new `pub` items to the contract section | yes |
| New public API documented | new `pub fn`/struct reflected in `ARCHITECTURE.md` | yes |
| Errors return `anyhow::Result` (unless contract says otherwise) | scan new fn signatures | yes |
| User output via `crate::term`/`crate::log` | `grep -rn 'eprintln!\|println!' <changed files>` — flag raw prints | yes |
| Ports `u16`, validated via `i64` | inspect new port params | yes |
| `unsafe` only for tiny libc wrappers | `grep -rn 'unsafe' <changed files>` | yes |
| slim→lane renames; no stray old refs | `grep -rniI 'slim\|drdave-flexnetos' <changed files>` | yes |
| rustls provider installed in new entrypoints | check for `install_crypto_provider()` | advisory |

The slim→lane rename table (verbatim, from `ARCHITECTURE.md`): `~/.lane`, `# lane` hosts marker,
iptables chain `LANE`, pf anchor `com.lane`, env `LANE_*`, CN `lane Root CA`, `[lane]` log prefix,
`.lane.yaml`, repo `FlexNetOS/lane`, ports `10080`/`10443`.

## Report format (`_workspace/05_rust-native-guardian_report.md`)
```markdown
# Conformance: <feature title>

## Rust-native invariant
- Drift scan: CLEAN / DRIFT FOUND
- <if drift> file:<path> — <what it is> — required fix: <Rust home + cargo wiring>

## Conformance checklist
| Invariant | Status | Evidence / file:line | Required fix |
|-----------|--------|----------------------|--------------|

## Verdict
- BLOCK (hard invariant red) / PASS (all green)
```

## Rule
Be specific and remediable: every finding cites file:line, names the violated invariant, and states the
Rust-native fix. A change is not acceptable while any hard invariant is red.
