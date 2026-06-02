//! Path and URL helpers for the `~/.lane` data directory.
//!
//! Faithful port of `internal/config/paths.go`. The Go version cached the
//! home directory via `Init()`; here we resolve it lazily on each call (cheap)
//! through `dirs::home_dir()`.

use std::path::PathBuf;

/// HTTP port the proxy listens on for plaintext (redirected to HTTPS).
pub const PROXY_HTTP_PORT: u16 = 10080;
/// HTTPS port the proxy listens on.
pub const PROXY_HTTPS_PORT: u16 = 10443;

const DEFAULT_API_BASE: &str = "https://app.lane.sh";
const DEFAULT_TUNNEL_SERVER: &str = "wss://app.lane.sh/tunnel";

/// API base URL: `LANE_TUNNEL_SERVER_API` env override, else the default.
pub fn api_base_url() -> String {
    match std::env::var("LANE_TUNNEL_SERVER_API") {
        Ok(v) if !v.is_empty() => v,
        _ => DEFAULT_API_BASE.to_string(),
    }
}

/// Tunnel server URL: `LANE_TUNNEL_SERVER` env override, else the default.
pub fn tunnel_server_url() -> String {
    match std::env::var("LANE_TUNNEL_SERVER") {
        Ok(v) if !v.is_empty() => v,
        _ => DEFAULT_TUNNEL_SERVER.to_string(),
    }
}

/// Base directory `~/.lane`.
///
/// The Go original cached the home directory at startup (`config.Init()`) and
/// errored there if it could not be determined; in practice the home directory
/// always resolves. We resolve lazily via `dirs::home_dir()` and panic with a
/// clear message in the impossible no-home case, keeping the contract's
/// infallible `PathBuf` return so downstream modules can chain `.join(...)`.
pub fn dir() -> PathBuf {
    let home = dirs::home_dir().expect("cannot determine home directory");
    home.join(".lane")
}

/// `~/.lane/config.yaml`.
pub fn config_path() -> PathBuf {
    dir().join("config.yaml")
}

/// `~/.lane/access.log`.
pub fn log_path() -> PathBuf {
    dir().join("access.log")
}

/// `~/.lane/lane.sock`.
pub fn socket_path() -> PathBuf {
    dir().join("lane.sock")
}

/// `~/.lane/lane.pid`.
pub fn pid_path() -> PathBuf {
    dir().join("lane.pid")
}

/// `~/.lane/tunnel-token`.
pub fn tunnel_token_path() -> PathBuf {
    dir().join("tunnel-token")
}

/// `~/.lane/auth.json`.
pub fn auth_path() -> PathBuf {
    dir().join("auth.json")
}

/// `~/.lane/pf.token`.
pub fn pf_token_path() -> PathBuf {
    dir().join("pf.token")
}
