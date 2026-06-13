//! Chisel-style reverse-tunnel forward spec for `lane share`.
//!
//! `R:<remotePort>:<localHost>:<localPort>` (chisel's `R:` remote forward) lets
//! `lane share` forward public traffic to an arbitrary **local upstream**, not
//! just `localhost:<--port>`. lane's hosted tunnel assigns the PUBLIC endpoint by
//! URL / `--subdomain` / `--domain`, so the `remotePort` number is **advisory**
//! (lane maps it to the assigned URL); the honored part is the local upstream
//! `localHost:localPort` the client forwards decoded requests to.
//!
//! Accepted forms:
//! - `R:8080`                  → `localhost:8080`
//! - `R:localhost:8080`        → `localhost:8080`
//! - `R:3000:localhost:8080`   → `localhost:8080`   (remote 3000 advisory)
//! - `R:3000:127.0.0.1:8080`   → `127.0.0.1:8080`   (remote 3000 advisory)

use std::str::FromStr;

use anyhow::{anyhow, bail, Result};

/// A parsed reverse-tunnel forward spec: where the tunnel forwards locally, plus
/// the advisory remote port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSpec {
    /// The remote/public port requested (chisel-style). Advisory in lane's
    /// URL-based model; `None` when the short form omits it.
    pub remote_port: Option<u16>,
    /// The local upstream host the tunnel forwards to (e.g. `localhost`).
    pub local_host: String,
    /// The local upstream port the tunnel forwards to.
    pub local_port: u16,
}

impl FromStr for ForwardSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let body = s
            .strip_prefix("R:")
            .or_else(|| s.strip_prefix("r:"))
            .ok_or_else(|| {
                anyhow!(
                    "invalid forward {s:?}: reverse-tunnel spec must start with \"R:\" \
                     (e.g. R:3000:localhost:8080)"
                )
            })?;

        let parts: Vec<&str> = body.split(':').collect();
        let spec = match parts.as_slice() {
            [port] => ForwardSpec {
                remote_port: None,
                local_host: "localhost".to_string(),
                local_port: parse_port(port)?,
            },
            [host, port] => ForwardSpec {
                remote_port: None,
                local_host: parse_host(host)?,
                local_port: parse_port(port)?,
            },
            [remote, host, port] => ForwardSpec {
                remote_port: Some(parse_port(remote)?),
                local_host: parse_host(host)?,
                local_port: parse_port(port)?,
            },
            _ => bail!(
                "invalid forward {s:?}: expected R:[remotePort:][localHost:]localPort \
                 (e.g. R:3000:localhost:8080)"
            ),
        };
        Ok(spec)
    }
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

/// Validate a non-empty local host token.
fn parse_host(tok: &str) -> Result<String> {
    let host = tok.trim();
    if host.is_empty() {
        bail!("invalid forward: local host must not be empty");
    }
    Ok(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_chisel_form() {
        let f: ForwardSpec = "R:3000:localhost:8080".parse().unwrap();
        assert_eq!(f.remote_port, Some(3000));
        assert_eq!(f.local_host, "localhost");
        assert_eq!(f.local_port, 8080);
    }

    #[test]
    fn parses_host_port_form() {
        let f: ForwardSpec = "R:127.0.0.1:9000".parse().unwrap();
        assert_eq!(f.remote_port, None);
        assert_eq!(f.local_host, "127.0.0.1");
        assert_eq!(f.local_port, 9000);
    }

    #[test]
    fn parses_port_only_form() {
        let f: ForwardSpec = "R:8080".parse().unwrap();
        assert_eq!(f.remote_port, None);
        assert_eq!(f.local_host, "localhost");
        assert_eq!(f.local_port, 8080);
    }

    #[test]
    fn lowercase_r_prefix_accepted() {
        let f: ForwardSpec = "r:8080".parse().unwrap();
        assert_eq!(f.local_port, 8080);
    }

    #[test]
    fn rejects_missing_r_prefix() {
        let err = "3000:localhost:8080".parse::<ForwardSpec>().unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
    }

    #[test]
    fn rejects_too_many_segments() {
        let err = "R:1:2:3:4".parse::<ForwardSpec>().unwrap_err();
        assert!(err.to_string().contains("expected R:"), "{err}");
    }

    #[test]
    fn rejects_zero_and_out_of_range_ports() {
        assert!("R:0".parse::<ForwardSpec>().is_err());
        assert!("R:99999".parse::<ForwardSpec>().is_err());
        assert!("R:notaport".parse::<ForwardSpec>().is_err());
    }

    #[test]
    fn rejects_empty_host() {
        let err = "R::8080".parse::<ForwardSpec>().unwrap_err();
        assert!(err.to_string().contains("host"), "{err}");
    }
}
