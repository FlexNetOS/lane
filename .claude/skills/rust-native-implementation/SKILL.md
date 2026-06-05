---
name: rust-native-implementation
description: "How to implement lane features in idiomatic, warning-clean Rust that matches lane's conventions and module patterns — error style, user output, port typing, async, module layout, and the test-isolation recipe. ALWAYS use when writing or editing Rust code in the lane crate. Do NOT use for non-Rust files or for design decisions (lane-feature-design)."
---

# Rust-Native Implementation

Write Rust that compiles clean, passes `clippy -D warnings`, and reads like the module around it. Follow
the architect's plan; encode lane's conventions so every implementer is consistent.

## The Rust-native rule (non-negotiable)
The shipped crate is 100% Rust. Never add a `.js/.mjs/.ts/.py/.go/.omc/.ecc` source or a shell build
step to make something work, and never pull a tool that auto-generates a non-Rust package into the
build. If a task seems to require it, stop and raise it with the guardian/architect — there is always a
Rust way here. (`.workflows/*.mjs` are dev-only orchestration scripts, never product code — not a precedent.)

## Conventions (match these exactly)
- **Errors:** return `anyhow::Result<T>` unless the contract names a specific type; add context with
  `.context("...")`. Reproduce existing error-message text where tests assert substrings.
- **User output:** go through `crate::term` (styled) and `crate::log` (access log / `[lane]` info/error).
  Never raw `eprintln!` except where the contract explicitly mirrors a Go `Fprintf(os.Stderr,...)`.
- **Ports:** validate as `i64` so out-of-range CLI input yields the exact expected error, then store `u16`.
- **Async:** the CLI is `#[tokio::main]`; honor async signatures where the contract is async
  (proxy/daemon/health/doctor/send_ipc). Don't block the runtime — use `tokio` primitives.
- **unsafe:** only tiny libc wrappers (`geteuid`, `setsid`, `umask`). Nothing else.
- **rustls:** call `lane::install_crypto_provider()` before building a rustls config in any new entrypoint.
- **Renames:** apply the `slim → lane` table from `ARCHITECTURE.md` (paths, env, markers, chains,
  anchors). No `slim` or old-org strings in new code.

## Module layout
- Extend an existing module before creating one. A new module is `src/<name>/mod.rs` (+ submodules),
  declared in `src/lib.rs`, mirroring the `mod.rs` + submodule pattern of its neighbors.
- Keep public surface minimal; match the signatures the architect/`ARCHITECTURE.md` specify so other
  modules integrate without churn.

## Before changing a shared symbol
Re-check callers with code intelligence and update every call site in the same change:
```bash
git-kb code callers <symbol> --json
```

## Tests (write alongside the code)
- Place unit tests in-module: `#[cfg(test)] mod tests { ... }`.
- Isolate filesystem/config/cert state with `tempfile::TempDir` and an overridden `HOME` (config and
  cert paths resolve via `HOME`/`dirs::home_dir()`).
- Serialize anything touching process-global state — the access-log writer or `HOME` — with
  `#[serial_test::serial]` (already a dev-dependency).
- Spin real `tokio` TCP listeners for proxy/health tests rather than mocking sockets.
- Assert the *new behavior*, not just that code runs. A test that can't fail has no value.

Example skeleton:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[serial_test::serial]
    fn round_trips_new_field() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        // arrange → act → assert the specific new behavior
    }
}
```

## Self-check before handing off (each module)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings` — zero warnings.
3. The new tests pass and assert the spec'd behavior.
4. Update `ARCHITECTURE.md` if you added/changed a documented public signature.
5. Append to `_workspace/03_rust-implementer_log.md`: files touched, tests added, any open risk.
Then SendMessage the verifier "module <name> ready" — verification is incremental.
