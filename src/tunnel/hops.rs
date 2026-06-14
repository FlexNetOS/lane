//! Multi-hop proxy chain spec for `lane share` (gost / chisel-style).
//!
//! For developers behind a NAT/firewall who can only reach the public internet
//! through one or more intermediate proxy hops (e.g. a company VPN's SOCKS5 or
//! HTTP-CONNECT egress), `--hop` lets `lane share` route the WebSocket dial to
//! the hosted tunnel server *through* those hops, in order, before the `wss`
//! upgrade. This is purely a **client-side dialing** decision — the wire
//! protocol between the client and the tunnel server is unchanged, so a chain
//! never affects how requests are framed or forwarded.
//!
//! Each hop is `[scheme://][user:pass@]host:port`:
//! - `scheme` ∈ `socks5` | `http` (default `socks5`)
//! - `user:pass@` optional proxy credentials
//! - `host:port` required; `host` must be non-empty, `port` in 1..=65535
//!
//! Accepted forms:
//! - `proxy.corp:1080`                       → socks5, no auth
//! - `socks5://proxy.corp:1080`              → socks5, no auth
//! - `http://gw.corp:8080`                   → http CONNECT, no auth
//! - `socks5://alice:s3cret@proxy.corp:1080` → socks5 with credentials
//! - `http://bob:pw@gw.corp:8080`            → http CONNECT with credentials
//!
//! Multiple `--hop` flags chain in order: the client dials hop 1, then asks
//! hop 1 to connect to hop 2, and so on, with the final hop connecting to the
//! tunnel server's host:port. An empty chain (no `--hop`) is the current
//! direct-dial behavior.

use std::str::FromStr;

use anyhow::{anyhow, bail, Result};

/// The proxy scheme of a single hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopScheme {
    /// SOCKS5 proxy (RFC 1928 / 1929 for username-password auth).
    Socks5,
    /// HTTP proxy using the `CONNECT` method to tunnel a raw TCP stream.
    Http,
}

impl HopScheme {
    /// The canonical lowercase scheme name, as it appears in a hop spec.
    pub fn as_str(self) -> &'static str {
        match self {
            HopScheme::Socks5 => "socks5",
            HopScheme::Http => "http",
        }
    }
}

/// Optional proxy credentials for a hop (`user:pass@`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopAuth {
    pub username: String,
    pub password: String,
}

/// A parsed multi-hop proxy spec: one intermediate proxy in the dial chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopSpec {
    /// The proxy protocol to speak to this hop.
    pub scheme: HopScheme,
    /// The proxy host (non-empty).
    pub host: String,
    /// The proxy port (1..=65535).
    pub port: u16,
    /// Optional proxy credentials.
    pub auth: Option<HopAuth>,
}

impl HopSpec {
    /// The `host:port` authority of this hop, for connecting / CONNECT targets.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl FromStr for HopSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let raw = s.trim();
        if raw.is_empty() {
            bail!("invalid hop {s:?}: hop spec must not be empty");
        }

        // Split off an optional `scheme://` prefix.
        let (scheme, rest) = match raw.split_once("://") {
            Some((sch, rest)) => (parse_scheme(sch, s)?, rest),
            None => (HopScheme::Socks5, raw),
        };

        // Split off optional `user:pass@` credentials. Use rsplit_once on '@'
        // so a password may itself contain '@' (the host/port never does).
        let (auth, hostport) = match rest.rsplit_once('@') {
            Some((creds, hostport)) => (Some(parse_auth(creds, s)?), hostport),
            None => (None, rest),
        };

        let (host, port) = parse_host_port(hostport, s)?;

        Ok(HopSpec {
            scheme,
            host,
            port,
            auth,
        })
    }
}

/// Parse the `scheme` token, defaulting nothing (caller supplies the default).
fn parse_scheme(tok: &str, full: &str) -> Result<HopScheme> {
    match tok.trim().to_ascii_lowercase().as_str() {
        "socks5" => Ok(HopScheme::Socks5),
        "http" => Ok(HopScheme::Http),
        other => Err(anyhow!(
            "invalid hop {full:?}: unknown proxy scheme {other:?} (want socks5 or http)"
        )),
    }
}

/// Parse a `user:pass` credentials token (both parts may be empty strings only
/// if the whole token is non-empty; an empty username is rejected).
fn parse_auth(tok: &str, full: &str) -> Result<HopAuth> {
    let (user, pass) = tok
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid hop {full:?}: proxy credentials must be user:pass"))?;
    if user.is_empty() {
        bail!("invalid hop {full:?}: proxy username must not be empty");
    }
    Ok(HopAuth {
        username: user.to_string(),
        password: pass.to_string(),
    })
}

/// Parse and validate the `host:port` authority of a hop.
fn parse_host_port(tok: &str, full: &str) -> Result<(String, u16)> {
    let (host, port) = tok.rsplit_once(':').ok_or_else(|| {
        anyhow!("invalid hop {full:?}: expected [scheme://][user:pass@]host:port")
    })?;
    let host = host.trim();
    if host.is_empty() {
        bail!("invalid hop {full:?}: proxy host must not be empty");
    }
    Ok((host.to_string(), parse_port(port)?))
}

/// Parse + validate a port token (1..=65535), reproducing lane's port-error text.
fn parse_port(tok: &str) -> Result<u16> {
    let n: i64 = tok
        .parse()
        .map_err(|_| anyhow!("invalid port {tok:?}: must be between 1 and 65535"))?;
    if !(1..=65535).contains(&n) {
        bail!("invalid port {n}: must be between 1 and 65535");
    }
    Ok(n as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_host_port_defaults_to_socks5() {
        let h: HopSpec = "proxy.corp:1080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Socks5);
        assert_eq!(h.host, "proxy.corp");
        assert_eq!(h.port, 1080);
        assert_eq!(h.auth, None);
        assert_eq!(h.authority(), "proxy.corp:1080");
    }

    #[test]
    fn parses_explicit_socks5_scheme() {
        let h: HopSpec = "socks5://proxy.corp:1080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Socks5);
        assert_eq!(h.host, "proxy.corp");
        assert_eq!(h.port, 1080);
        assert_eq!(h.auth, None);
    }

    #[test]
    fn parses_http_scheme() {
        let h: HopSpec = "http://gw.corp:8080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Http);
        assert_eq!(h.host, "gw.corp");
        assert_eq!(h.port, 8080);
    }

    #[test]
    fn scheme_is_case_insensitive() {
        let h: HopSpec = "SOCKS5://proxy.corp:1080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Socks5);
        let h: HopSpec = "HTTP://gw.corp:8080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Http);
    }

    #[test]
    fn parses_socks5_with_credentials() {
        let h: HopSpec = "socks5://alice:s3cret@proxy.corp:1080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Socks5);
        assert_eq!(h.host, "proxy.corp");
        assert_eq!(h.port, 1080);
        assert_eq!(
            h.auth,
            Some(HopAuth {
                username: "alice".to_string(),
                password: "s3cret".to_string(),
            })
        );
    }

    #[test]
    fn parses_http_with_credentials() {
        let h: HopSpec = "http://bob:pw@gw.corp:8080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Http);
        assert_eq!(
            h.auth,
            Some(HopAuth {
                username: "bob".to_string(),
                password: "pw".to_string(),
            })
        );
    }

    #[test]
    fn credentials_without_scheme_default_socks5() {
        let h: HopSpec = "alice:s3cret@proxy.corp:1080".parse().unwrap();
        assert_eq!(h.scheme, HopScheme::Socks5);
        assert_eq!(h.auth.as_ref().unwrap().username, "alice");
    }

    #[test]
    fn password_may_contain_at_sign() {
        // rsplit on '@' keeps the host:port; the password keeps the earlier '@'.
        let h: HopSpec = "socks5://alice:p@ss@proxy.corp:1080".parse().unwrap();
        assert_eq!(h.host, "proxy.corp");
        assert_eq!(h.port, 1080);
        let auth = h.auth.unwrap();
        assert_eq!(auth.username, "alice");
        assert_eq!(auth.password, "p@ss");
    }

    #[test]
    fn empty_password_is_allowed() {
        let h: HopSpec = "socks5://alice:@proxy.corp:1080".parse().unwrap();
        let auth = h.auth.unwrap();
        assert_eq!(auth.username, "alice");
        assert_eq!(auth.password, "");
    }

    #[test]
    fn rejects_empty_spec() {
        let err = "".parse::<HopSpec>().unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
        let err = "   ".parse::<HopSpec>().unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = "ftp://proxy.corp:1080".parse::<HopSpec>().unwrap_err();
        assert!(err.to_string().contains("unknown proxy scheme"), "{err}");
        assert!(err.to_string().contains("socks5 or http"), "{err}");
    }

    #[test]
    fn rejects_missing_port() {
        let err = "socks5://proxy.corp".parse::<HopSpec>().unwrap_err();
        assert!(err.to_string().contains("expected [scheme://]"), "{err}");
    }

    #[test]
    fn rejects_empty_host() {
        let err = "socks5://:1080".parse::<HopSpec>().unwrap_err();
        assert!(err.to_string().contains("host must not be empty"), "{err}");
    }

    #[test]
    fn rejects_empty_username() {
        let err = "socks5://:pw@proxy.corp:1080"
            .parse::<HopSpec>()
            .unwrap_err();
        assert!(
            err.to_string().contains("username must not be empty"),
            "{err}"
        );
    }

    #[test]
    fn rejects_out_of_range_and_nonnumeric_ports() {
        let err = "proxy.corp:0".parse::<HopSpec>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid port 0: must be between 1 and 65535"
        );
        assert!("proxy.corp:99999".parse::<HopSpec>().is_err());
        let err = "proxy.corp:notaport".parse::<HopSpec>().unwrap_err();
        assert!(
            err.to_string().contains("must be between 1 and 65535"),
            "{err}"
        );
    }

    #[test]
    fn authority_round_trips() {
        let h: HopSpec = "http://bob:pw@gw.corp:8080".parse().unwrap();
        assert_eq!(h.authority(), "gw.corp:8080");
        assert_eq!(h.scheme.as_str(), "http");
    }
}
