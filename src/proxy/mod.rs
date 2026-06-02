//! proxy — the local HTTPS reverse proxy.
//!
//! Faithful port of `internal/proxy`. The TLS-terminating server resolves a
//! per-domain leaf cert on demand, routes by `Host` (with longest-prefix path
//! routes), reverse-proxies to `localhost:{port}`, bridges WebSocket upgrades,
//! and serves friendly "waiting for server" pages when an upstream is down.

mod handler;
mod health;
mod pages;
mod server;

pub use health::*;
pub use pages::*;
pub use server::*;
