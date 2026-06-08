---
schema: lane.backlog.item.v1
id: PLACEHOLDER
title: "Backlog item title"
status: todo
priority: P1
description: |
  One-line description of the intent for lane.
acceptance_criteria:
  - Verifiable criterion 1
  - Verifiable criterion 2
pipeline_stages: [spec, design, implement, verify]
cargo_commands:
  - cargo fmt --all -- --check
  - cargo clippy --all-targets -- -D warnings
  - cargo test
dependencies: []
drift_check: true        # lane invariant: must be 100% Rust-native
rust_native_only: true
---

<!-- Usage: copy this to _workspace/backlog items. Lane uses markdown backlog format, not YAML. -->
<!-- This frontmatter defines the structured fields; the body is free text for notes/dead-ends. -->

## Context
<!-- Why does this item exist? What's the background? -->

## Implementation notes
<!-- Ideas, references to ARCHITECTURE.md modules, known constraints. -->

## Dead ends
<!-- Things tried that didn't work — so the next cycle doesn't relitigate them. -->
