//! lane's **host network plane** (ADR-0003 — adopt-consume the host network).
//!
//! ADR-0003 makes lane the named owner of the host's *base* network
//! configuration — interfaces, addresses, routes, link-local special cases —
//! today owned per-box by `NetworkManager`/`netplan` files in `/etc`, which do
//! not travel with the repo (so the estate is not portable). lane **adopts** that
//! host state into a Rust-native, declarative model and **consumes** it (diff,
//! render, reconcile) so a box is reproducible from the repo.
//!
//! # What is always compiled (and tested)
//!
//! - [`model`] — the pure serde data model for the host plane. It is a **lossless
//!   superset of netplan v2** (per the ADR's no-downgrade contract): every
//!   observable in a host snapshot — address, route, match rule, never-default,
//!   autoconnect, wifi key-mgmt, link-local mode — round-trips adopt→render
//!   unchanged. The model carries no host access, so it compiles and its
//!   round-trip tests run in the default build (no feature gate), mirroring how
//!   [`crate::relay::allowlist`] keeps its pure security core always-built.
//!
//! # What will be feature-gated (later slices, not here)
//!
//! Only the *effectful* paths take a gate: the adopter that reads the live host
//! (`nmcli`/`/etc/netplan`/`ip`) and the renderer that mutates it
//! (`lane net apply`). This slice (P0a) ships **only** the pure model — no host
//! reader, no CLI, no feature gate — exactly the "pure layer always built,
//! effectful path gated" precedent the relay module established.

pub mod model;
