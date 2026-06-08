# Harness Upgrade Progress — 2026-06-08 (Updated)

## COMPLETED ✅ (build + test green, 223/0)

### Session handoff / continuity layer
- `.lane-loop/` — schemas (packet/session/task JSON/TOML), policies, lifecycle hooks
- `docs/reference/` — repositories.md (16 external repos, 4 categories), README.md
- Phase 7 backlog items appended to _workspace/backlog.md (10 feature ideas)

### Round A: Zero-dep features from Phase 7 (all implemented)
1. **Key-type selection** (`KeyType::Rsa2048 | EcdsaP256 | EcdsaP384`) — cert module enum + CLI
2. **Wildcard cert generation** — `generate_wildcard_cert()` + `lane cert wildcard <domain>` subcommand
3. **Doctor --fix auto-heal** — auto-regenerate CA/trust/hosts/leaf on Fail checks
4. **Custom SAN on start** (`--san`) — parse_extra_sans() with IP/DNS auto-detection

## In-progress / Not yet started (Phase 7 remaining)

### Round B: One-crate features (deferred to later cycle)
- `ACME integration` — needs `instant-acme` crate + new src/acme.rs module (~80 lines)
- `Service file generation` — needs `daemon-kit` or `qsu` crate + new src/service.rs (~40 lines)
- `Template-driven config` — needs `askama` crate + new src/template.rs (~30 lines)
- `Request inspection TUI` — needs `crossterm` + `ratatui` crates + new src/inspect.rs (~100 lines)

### Deferred (greenfield, larger scope)
- Multi-hop tunnel proxy chains

## Git status
Changes ready to commit: all Round A features + session handoff infrastructure.
Tests: 223 passed / 0 failed (9.77s). Build: 0 errors, 0 warnings.

## Next action
Commit current changes, then optionally continue with Round B items one at a time.
