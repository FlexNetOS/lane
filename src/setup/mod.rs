//! First-run setup and proxy-port preflight checks.
//!
//! Faithful port of `internal/setup/setup.go`. `ensure_first_run` walks the
//! interactive bootstrap (generate + trust the root CA, then wire up port
//! forwarding) using the same step sequence as the Go original, and
//! `ensure_proxy_ports_available` refuses to start when the proxy's listener
//! ports are already taken.

use anyhow::Result;

use crate::cert;
use crate::config;
use crate::system;
use crate::term::step::{run_steps, Step};

/// Run the one-time bootstrap if it has not been done yet.
///
/// Mirrors Go's `EnsureFirstRun`:
///
/// - If the root CA does not exist, run two steps: generate the CA, then trust
///   it (the trust step is interactive because it may prompt for a password).
/// - Then construct the port forwarder; if it is not already enabled, run a
///   single step that attempts to enable it, downgrading any failure to a
///   `skipped (…)` status rather than a hard error.
pub fn ensure_first_run() -> Result<()> {
    if !cert::ca_exists() {
        run_steps(vec![
            Step {
                name: "Generating root CA".to_string(),
                run: Box::new(|| cert::generate_ca().map(|()| "done".to_string())),
                interactive: false,
            },
            Step {
                name: "Trusting root CA (you may be prompted for your password)".to_string(),
                run: Box::new(|| cert::trust_ca().map(|()| "done".to_string())),
                interactive: true,
            },
        ])?;
    }

    let pf = system::new_port_forwarder();
    if !pf.is_enabled() {
        run_steps(vec![Step {
            name: format!(
                "Setting up port forwarding (80→{}, 443→{})",
                config::PROXY_HTTP_PORT,
                config::PROXY_HTTPS_PORT
            ),
            run: Box::new(move || match pf.enable() {
                Err(e) => Ok(format!("skipped ({e})")),
                Ok(()) => Ok("done".to_string()),
            }),
            interactive: false,
        }])?;
    }

    Ok(())
}

/// Verify that both proxy listener ports are free before starting.
///
/// Mirrors Go's `EnsureProxyPortsAvailable`: it tries to bind each of
/// `:PROXY_HTTP_PORT` and `:PROXY_HTTPS_PORT` and returns the first failure.
pub fn ensure_proxy_ports_available() -> Result<()> {
    let addrs = [
        format!(":{}", config::PROXY_HTTP_PORT),
        format!(":{}", config::PROXY_HTTPS_PORT),
    ];
    for addr in &addrs {
        ensure_port_available(addr)?;
    }
    Ok(())
}

/// Try to bind `addr` and immediately release it.
///
/// Mirrors Go's `ensurePortAvailable`. `addr` is a Go-style listen address
/// (`":10080"` binds all interfaces; `"127.0.0.1:0"` binds an ephemeral port on
/// loopback). On failure it returns the same error text as the Go original.
fn ensure_port_available(addr: &str) -> Result<()> {
    match std::net::TcpListener::bind(normalize_addr(addr)) {
        Ok(_listener) => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "proxy listener port {addr} is unavailable: {e} (another local proxy/old daemon may already be running)"
        )),
    }
}

/// Translate a Go-style listen address into one Rust's `TcpListener` accepts.
///
/// Go's `net.Listen("tcp", ":10080")` binds all interfaces; Rust requires an
/// explicit host, so a bare `:port` becomes `0.0.0.0:port`. Addresses that
/// already carry a host (e.g. `127.0.0.1:0`) are passed through unchanged.
fn normalize_addr(addr: &str) -> String {
    if let Some(port) = addr.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        addr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of `TestEnsureProxyPortsAvailableFailsWhenInUse`: occupy a port and
    /// confirm `ensure_port_available` rejects it with an "unavailable" error.
    #[test]
    fn ensure_port_available_fails_when_in_use() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("listen on ephemeral port");
        let addr = listener.local_addr().expect("local addr").to_string();

        let err = ensure_port_available(&addr)
            .expect_err("expected error for an in-use port")
            .to_string();
        assert!(
            err.contains("unavailable"),
            "expected unavailable error, got: {err}"
        );
    }

    /// Port of `TestEnsurePortAvailableSuccess`: bind, release, then confirm the
    /// now-free address binds cleanly.
    #[test]
    fn ensure_port_available_success() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("listen on ephemeral port");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        ensure_port_available(&addr).expect("unexpected error");
    }
}
