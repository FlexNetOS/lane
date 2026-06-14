//! The cross-machine **lane relay** (ADR-0002 Option A): every lane node is a
//! relay-capable iroh peer in a trusted fleet mesh. A node accepts inbound
//! connections **only** from NodeIds on a deny-by-default trusted-node allowlist,
//! and — before bridging a relayed request to a local service — runs the **same**
//! deny-by-default [`crate::webpolicy`] gate and access-log it runs for local
//! traffic. That is *governance-across-the-link*: a cross-machine request is
//! trust-checked, webpolicy-checked, and logged at the destination node exactly
//! like a local one (ADR-0002 §"governance composition").
//!
//! # What is always compiled (and tested)
//!
//! - [`allowlist`] — the pure, deny-by-default trusted-node check ([`is_trusted`]).
//!   This is the security core and is compiled + exhaustively tested in **every**
//!   build, including the default no-`relay` build.
//! - [`identity`] path helpers — where the node identity key lives.
//! - [`TargetRequest`] framing — the tiny, explicit wire frame a connecting node
//!   sends to name the service it wants on the accepting node, plus the response
//!   frame. Pure encode/decode, unit-tested without iroh.
//!
//! # What is feature-gated (`relay`)
//!
//! Only the iroh-using transport: binding the [`Endpoint`], the governed accept
//! loop, and the connect-and-bridge side. Without the feature the `lane relay`
//! CLI still parses but every action fails closed with a clear "rebuild with
//! `--features relay`" error (mirroring `lane web` / `lane start --acme`).
//!
//! # Wire protocol
//!
//! - **ALPN:** `lane/relay/1`.
//! - **Request frame** (connector → acceptor, on a fresh bi-stream): a 2-byte
//!   big-endian length `N` followed by `N` UTF-8 bytes of the target string
//!   `host:port` naming the service on the *accepting* node. See
//!   [`TargetRequest`].
//! - **Response frame** (acceptor → connector): 1 status byte —
//!   [`RESP_OK`] (the acceptor allowed + connected; raw bytes are bridged after)
//!   or [`RESP_DENIED`] (governance refused), and on denial a 2-byte big-endian
//!   length + that many UTF-8 bytes of the human reason. After [`RESP_OK`] the
//!   stream carries the opaque bridged bytes of the local service.

pub mod allowlist;
pub mod identity;

pub use allowlist::{is_trusted, normalize_node_id, parse_node_id};

/// The relay ALPN — the application protocol identifier both peers must agree on.
pub const RELAY_ALPN: &[u8] = b"lane/relay/1";

/// Response status: the acceptor governed the request, ALLOWED it, and connected
/// to the local service; bridged bytes follow on the stream.
pub const RESP_OK: u8 = 0;
/// Response status: the acceptor DENIED the request (trust or webpolicy); a
/// length-prefixed reason follows and no upstream was connected.
pub const RESP_DENIED: u8 = 1;

/// The maximum target-string length accepted on the wire (a `host:port` far
/// exceeds nothing reasonable below this; larger is treated as malformed).
pub const MAX_TARGET_LEN: usize = 1024;

/// A decoded target request: the `host:port` a connecting node wants to reach on
/// the accepting node. Built by [`TargetRequest::parse`] from the frame payload;
/// the host/port are what the acceptor feeds to [`crate::webpolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRequest {
    /// The target host on the accepting node's side (a hostname or IP literal).
    pub host: String,
    /// The target port on the accepting node's side.
    pub port: u16,
}

impl TargetRequest {
    /// Build a request for `host:port`.
    pub fn new(host: impl Into<String>, port: u16) -> TargetRequest {
        TargetRequest {
            host: host.into(),
            port,
        }
    }

    /// The canonical wire string, `host:port`.
    pub fn wire_string(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Encode to the request frame: 2-byte BE length + UTF-8 `host:port`.
    pub fn encode(&self) -> Vec<u8> {
        let s = self.wire_string();
        let bytes = s.as_bytes();
        let len = bytes.len().min(u16::MAX as usize) as u16;
        let mut out = Vec::with_capacity(2 + bytes.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&bytes[..len as usize]);
        out
    }

    /// Parse a target string `host:port` (bracketed IPv6 `[::1]:443` supported)
    /// into a [`TargetRequest`]. Returns an error string for malformed input.
    pub fn parse(target: &str) -> Result<TargetRequest, String> {
        let t = target.trim();
        if t.is_empty() {
            return Err("empty target".to_string());
        }
        if t.len() > MAX_TARGET_LEN {
            return Err("target too long".to_string());
        }

        // Bracketed IPv6 literal: [addr]:port.
        if let Some(rest) = t.strip_prefix('[') {
            let close = rest.find(']').ok_or("unterminated IPv6 literal")?;
            let host = &rest[..close];
            let after = &rest[close + 1..];
            let port_str = after
                .strip_prefix(':')
                .ok_or("missing port after IPv6 literal")?;
            let port = parse_port(port_str)?;
            if host.is_empty() {
                return Err("empty host".to_string());
            }
            return Ok(TargetRequest::new(host, port));
        }

        // host:port — the host must not itself contain a colon (unbracketed IPv6
        // is ambiguous and rejected).
        match t.rsplit_once(':') {
            Some((host, port_str)) if !host.contains(':') => {
                if host.is_empty() {
                    return Err("empty host".to_string());
                }
                let port = parse_port(port_str)?;
                Ok(TargetRequest::new(host, port))
            }
            Some(_) => Err("ambiguous host (unbracketed IPv6 must be in [..])".to_string()),
            None => Err("missing port (expected host:port)".to_string()),
        }
    }
}

/// Encode the acceptor's deny response: [`RESP_DENIED`] + 2-byte BE reason length
/// + UTF-8 reason.
pub fn encode_denied(reason: &str) -> Vec<u8> {
    let bytes = reason.as_bytes();
    let len = bytes.len().min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(3 + bytes.len());
    out.push(RESP_DENIED);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes[..len as usize]);
    out
}

/// Parse a decimal port in `1..=65535`.
fn parse_port(s: &str) -> Result<u16, String> {
    let n: u32 = s.parse().map_err(|_| format!("invalid port: {s}"))?;
    if n == 0 || n > u16::MAX as u32 {
        return Err(format!("port out of range: {s}"));
    }
    Ok(n as u16)
}

/// Live, iroh-backed relay transport (feature-gated): the [`Endpoint`], the
/// governed accept loop, and the connect-and-bridge side.
#[cfg(feature = "relay")]
mod live;

#[cfg(feature = "relay")]
pub use live::{
    connect_and_bridge, endpoint_addr_from_parts, relay_mode_from_config, run_accept_loop,
    serve_local_bridge, AcceptConfig, RelayEndpoint,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_request_round_trips_through_the_frame() {
        let req = TargetRequest::new("example.test", 8080);
        let frame = req.encode();
        // 2-byte length prefix.
        let len = u16::from_be_bytes([frame[0], frame[1]]) as usize;
        assert_eq!(len, frame.len() - 2);
        let payload = std::str::from_utf8(&frame[2..]).unwrap();
        let parsed = TargetRequest::parse(payload).unwrap();
        assert_eq!(parsed, req);
        assert_eq!(parsed.wire_string(), "example.test:8080");
    }

    #[test]
    fn parse_accepts_host_port() {
        let r = TargetRequest::parse("localhost:3000").unwrap();
        assert_eq!(r.host, "localhost");
        assert_eq!(r.port, 3000);
    }

    #[test]
    fn parse_accepts_bracketed_ipv6() {
        let r = TargetRequest::parse("[::1]:443").unwrap();
        assert_eq!(r.host, "::1");
        assert_eq!(r.port, 443);
    }

    #[test]
    fn parse_rejects_malformed_targets() {
        for bad in [
            "",
            "   ",
            "no-port",
            "host:",
            "host:0",
            "host:99999",
            "::1:443",  // unbracketed IPv6 is ambiguous
            "[::1]443", // missing colon after literal
            "[::1",     // unterminated literal
            ":3000",    // empty host
        ] {
            assert!(
                TargetRequest::parse(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn parse_rejects_oversized_target() {
        let huge = format!("{}:80", "a".repeat(MAX_TARGET_LEN));
        assert!(TargetRequest::parse(&huge).is_err());
    }

    #[test]
    fn encode_denied_frame_shape() {
        let frame = encode_denied("blocked: private/internal network address");
        assert_eq!(frame[0], RESP_DENIED);
        let len = u16::from_be_bytes([frame[1], frame[2]]) as usize;
        assert_eq!(len, frame.len() - 3);
        let reason = std::str::from_utf8(&frame[3..]).unwrap();
        assert_eq!(reason, "blocked: private/internal network address");
    }

    #[test]
    fn alpn_is_versioned() {
        assert_eq!(RELAY_ALPN, b"lane/relay/1");
    }
}
