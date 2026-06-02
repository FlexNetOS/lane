//! Network error classification.
//!
//! Faithful port of `internal/httperr/network.go`. Go inspected `net.DNSError`
//! and `net.Error.Timeout()`; here callers pass `reqwest` errors (or any
//! `std::error::Error`), so we detect network conditions via
//! `reqwest::Error::is_timeout()` / `is_connect()` where the concrete type is
//! available, and otherwise fall back to the same Display-string substring
//! checks Go used. The human-facing strings are byte-identical to Go.

use anyhow::anyhow;

/// Wrapper that reproduces Go's `fmt.Errorf("%s: %w", context, err)`: its
/// `Display` is `"{context}: {source}"` and `source()` exposes the inner error
/// so callers can still walk / downcast the chain (matching `errors.Is`).
#[derive(Debug)]
struct WrappedError {
    context: String,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl std::fmt::Display for WrappedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for WrappedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Classify an error into a network hint, or echo its message when it is not a
/// recognized network condition. Mirrors Go's `NetworkHint`.
///
/// (Go returned `""` for a nil error; in Rust a `nil` error is unrepresentable,
/// so the no-error case is handled in [`wrap`].)
pub fn network_hint(err: &(dyn std::error::Error + 'static)) -> String {
    // Where the concrete error is a reqwest error we can ask it directly,
    // matching Go's typed checks against net.Error.
    if let Some(re) = err.downcast_ref::<reqwest::Error>() {
        // net.Error{}.Timeout()
        if re.is_timeout() {
            return "connection timed out — check your internet connection".to_string();
        }
    }

    let msg = err.to_string();

    // net.DNSError / "no such host" -> resolution failure.
    if msg.contains("no such host") || msg.contains("failed to lookup address") {
        return "could not resolve host — check your internet connection".to_string();
    }

    // net.Error.Timeout() via message, for non-reqwest errors.
    if msg.contains("timed out") || msg.contains("i/o timeout") {
        return "connection timed out — check your internet connection".to_string();
    }

    if msg.contains("connection refused") {
        return "connection refused — the server may be down".to_string();
    }
    if msg.contains("network is unreachable") || msg.contains("no route to host") {
        return "network is unreachable — check your internet connection".to_string();
    }

    msg
}

/// Returns `true` when the error looks like a network-level failure (so that
/// [`wrap`] should substitute a hint instead of surfacing the raw message).
///
/// Mirrors Go's `errors.As(err, &netErr)` test against `net.Error`.
fn is_network_error(err: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(re) = err.downcast_ref::<reqwest::Error>() {
        if re.is_timeout() || re.is_connect() {
            return true;
        }
    }

    let msg = err.to_string();
    msg.contains("no such host")
        || msg.contains("failed to lookup address")
        || msg.contains("timed out")
        || msg.contains("i/o timeout")
        || msg.contains("connection refused")
        || msg.contains("network is unreachable")
        || msg.contains("no route to host")
}

/// Wrap an error with a context prefix, substituting a friendly hint for
/// recognized network failures. Mirrors Go's `Wrap`.
///
/// For network errors the resulting message is `"{context}: {hint}"` (the raw
/// error is not chained, matching Go's `%s`); for everything else the source is
/// preserved so `anyhow`'s chain / downcasting still works (matching Go's `%w`).
pub fn wrap(context: &str, err: impl std::error::Error + Send + Sync + 'static) -> anyhow::Error {
    if is_network_error(&err) {
        anyhow!("{}: {}", context, network_hint(&err))
    } else {
        anyhow::Error::new(WrappedError {
            context: context.to_string(),
            source: Box::new(err),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    /// Minimal error type so we can exercise the Display-string code paths the
    /// same way Go's `fmt.Errorf` strings did.
    #[derive(Debug)]
    struct StrErr(String);
    impl fmt::Display for StrErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for StrErr {}

    fn e(s: &str) -> StrErr {
        StrErr(s.to_string())
    }

    // Port of TestNetworkHint_DNS
    #[test]
    fn network_hint_dns() {
        let err = e("dial tcp: lookup app.lane.sh: no such host");
        let hint = network_hint(&err);
        assert!(
            hint.contains("check your internet connection"),
            "got {hint:?}"
        );
    }

    // Port of TestNetworkHint_Timeout (custom timeoutErr whose message is
    // "i/o timeout"; adapted to the Display-string seam).
    #[test]
    fn network_hint_timeout() {
        let err = e("dial tcp: i/o timeout");
        let hint = network_hint(&err);
        assert!(hint.contains("timed out"), "got {hint:?}");
    }

    // Port of TestNetworkHint_ConnectionRefused
    #[test]
    fn network_hint_connection_refused() {
        let err = e("dial tcp 127.0.0.1:443: connection refused");
        let hint = network_hint(&err);
        assert!(hint.contains("server may be down"), "got {hint:?}");
    }

    // Port of TestNetworkHint_Unreachable
    #[test]
    fn network_hint_unreachable() {
        let err = e("dial tcp: network is unreachable");
        let hint = network_hint(&err);
        assert!(
            hint.contains("check your internet connection"),
            "got {hint:?}"
        );
    }

    // Port of TestNetworkHint_GenericError
    #[test]
    fn network_hint_generic_error() {
        let err = e("something unexpected");
        let hint = network_hint(&err);
        assert_eq!(hint, "something unexpected");
    }

    // Port of TestWrap_NetworkError
    #[test]
    fn wrap_network_error() {
        let inner = e("dial tcp: lookup app.lane.sh: no such host");
        let err = wrap("login failed", inner);
        let s = err.to_string();
        assert!(s.contains("login failed"), "missing context: {s:?}");
        assert!(
            s.contains("check your internet connection"),
            "missing hint: {s:?}"
        );
    }

    // Port of TestWrap_NonNetworkError
    #[test]
    fn wrap_non_network_error() {
        let inner = e("parse error");
        let err = wrap("login failed", inner);
        // Matches Go's fmt.Errorf("%s: %w", ...).Error().
        assert_eq!(err.to_string(), "login failed: parse error");
        // Go asserted errors.Is(err, inner): the source is preserved in the chain.
        assert!(err.chain().any(|c| c.to_string() == "parse error"));
    }
}
