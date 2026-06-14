//! tunnel — client that bridges a public tunnel server to a local dev server.
//!
//! Faithful port of `internal/tunnel` from the Go tool `slim`. The [`Client`]
//! dials the tunnel server over a WebSocket, registers, and then forwards each
//! inbound request (carried as raw HTTP/1.x bytes inside binary frames) to a
//! local port via `reqwest`, streaming the local response back over the same
//! WebSocket. [`validate_subdomain`] rejects subdomains that resemble protected
//! brand names, and [`pages`] renders the 502 "server down" page shown when the
//! local server is unreachable.

pub mod client;
pub mod dialer;
pub mod forward;
pub mod hops;
pub mod pages;
pub mod subdomain;

pub use client::{Client, ClientOptions, RequestEvent};
pub use forward::ForwardSpec;
pub use hops::{HopAuth, HopScheme, HopSpec};
pub use subdomain::validate_subdomain;
