//! lane — clean HTTPS local domains for your dev servers.
//!
//! A faithful Rust port of the Go tool `slim`. See `ARCHITECTURE.md` for the
//! cross-module API contract and the original-source mapping.

pub mod acme;
pub mod auth;
pub mod cert;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod httperr;
pub mod inspect;
pub mod log;
pub mod osutil;
pub mod project;
pub mod protocol;
pub mod proxy;
pub mod service;
pub mod setup;
pub mod system;
pub mod term;
pub mod tunnel;
pub mod web;
pub mod webpolicy;

/// Build version. Overridable at build time via the `LANE_VERSION` env var
/// (set by CI/release); falls back to the crate version.
pub const VERSION: &str = match option_env!("LANE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Install the process-wide rustls crypto provider exactly once. Safe to call
/// from multiple entrypoints (`main`, daemon, tests).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
