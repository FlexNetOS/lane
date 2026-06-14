//! Multi-hop proxy chain dialer for the tunnel client.
//!
//! Given an ordered chain of [`HopSpec`] hops and a final `host:port` target
//! (the tunnel server's authority), [`dial_through_hops`] opens a TCP connection
//! to the first hop and then, hop by hop, asks each proxy to CONNECT to the
//! next authority — SOCKS5 (RFC 1928 + 1929 user/pass auth) or HTTP `CONNECT` —
//! ending with the proxy connected to the tunnel server. The returned
//! [`TcpStream`] is a raw byte tunnel to the server, ready for the `wss`
//! TLS + WebSocket upgrade (`client_async_tls_with_config`), so the wire
//! protocol above it is untouched.
//!
//! The pure protocol encoders ([`socks5_greeting`], [`socks5_userpass_auth`],
//! [`socks5_connect_request`], [`http_connect_request`]) are unit-tested with no
//! network. The end-to-end chain across *real* intermediate proxies is
//! inherently un-CI-able (it needs live SOCKS5/HTTP egress hosts), exactly like
//! ACME's live Let's Encrypt round-trip — documented, not mocked.

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::hops::{HopAuth, HopScheme, HopSpec};

/// Open a TCP byte tunnel to `target` (`host:port`) routed through `hops` in
/// order. With an empty chain this is a plain `TcpStream::connect(target)`.
///
/// The live multi-hop path requires real intermediate proxies and is therefore
/// not exercised in CI; the per-hop protocol encoders it calls are unit-tested.
pub async fn dial_through_hops(hops: &[HopSpec], target: &str) -> Result<TcpStream> {
    // No hops: direct dial (current behavior).
    let first = match hops.first() {
        None => {
            return TcpStream::connect(target)
                .await
                .with_context(|| format!("connecting to {target}"));
        }
        Some(h) => h,
    };

    // Connect to the first hop's proxy.
    let mut stream = TcpStream::connect(first.authority())
        .await
        .with_context(|| format!("connecting to hop {}", first.authority()))?;

    // For each hop, ask it to CONNECT to the NEXT authority in the chain; the
    // last hop connects to the tunnel server `target`.
    for i in 0..hops.len() {
        let hop = &hops[i];
        let next_authority = hops
            .get(i + 1)
            .map(|h| h.authority())
            .unwrap_or_else(|| target.to_string());

        let (next_host, next_port) = split_authority(&next_authority)?;

        match hop.scheme {
            HopScheme::Socks5 => {
                socks5_handshake(&mut stream, hop, &next_host, next_port)
                    .await
                    .with_context(|| {
                        format!("socks5 hop {} → {next_authority}", hop.authority())
                    })?;
            }
            HopScheme::Http => {
                http_connect(&mut stream, hop, &next_authority)
                    .await
                    .with_context(|| {
                        format!("http CONNECT hop {} → {next_authority}", hop.authority())
                    })?;
            }
        }
    }

    Ok(stream)
}

/// Split a `host:port` authority into its parts (used for the SOCKS5 request,
/// which encodes host and port separately).
fn split_authority(authority: &str) -> Result<(String, u16)> {
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("invalid target authority {authority:?}: expected host:port"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| anyhow!("invalid target port in {authority:?}"))?;
    Ok((host.to_string(), port))
}

// ----------------------------------------------------------------------------
// SOCKS5 (RFC 1928, RFC 1929)
// ----------------------------------------------------------------------------

/// The SOCKS5 client greeting: version, advertised auth methods.
/// `no-auth` (0x00) is always offered; `user/pass` (0x02) is added when creds
/// are present.
fn socks5_greeting(has_auth: bool) -> Vec<u8> {
    if has_auth {
        // VER=5, NMETHODS=2, METHODS=[no-auth, user/pass]
        vec![0x05, 0x02, 0x00, 0x02]
    } else {
        // VER=5, NMETHODS=1, METHODS=[no-auth]
        vec![0x05, 0x01, 0x00]
    }
}

/// The RFC 1929 username/password sub-negotiation request.
fn socks5_userpass_auth(auth: &HopAuth) -> Result<Vec<u8>> {
    let user = auth.username.as_bytes();
    let pass = auth.password.as_bytes();
    if user.len() > 255 {
        bail!("socks5 username too long (max 255 bytes)");
    }
    if pass.len() > 255 {
        bail!("socks5 password too long (max 255 bytes)");
    }
    let mut buf = Vec::with_capacity(3 + user.len() + pass.len());
    buf.push(0x01); // auth sub-negotiation version
    buf.push(user.len() as u8);
    buf.extend_from_slice(user);
    buf.push(pass.len() as u8);
    buf.extend_from_slice(pass);
    Ok(buf)
}

/// The SOCKS5 CONNECT request to `host:port`, using a DOMAINNAME address (the
/// proxy resolves the name), which works for both hostnames and IP literals.
fn socks5_connect_request(host: &str, port: u16) -> Result<Vec<u8>> {
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        bail!("socks5 target host too long (max 255 bytes)");
    }
    let mut buf = Vec::with_capacity(7 + host_bytes.len());
    buf.push(0x05); // VER
    buf.push(0x01); // CMD = CONNECT
    buf.push(0x00); // RSV
    buf.push(0x03); // ATYP = DOMAINNAME
    buf.push(host_bytes.len() as u8);
    buf.extend_from_slice(host_bytes);
    buf.extend_from_slice(&port.to_be_bytes());
    Ok(buf)
}

/// Perform a full SOCKS5 handshake on `stream`, ending with a CONNECT to
/// `host:port`. Live network path (un-CI-able across real proxies).
async fn socks5_handshake(
    stream: &mut TcpStream,
    hop: &HopSpec,
    host: &str,
    port: u16,
) -> Result<()> {
    let has_auth = hop.auth.is_some();

    // 1. Greeting → method selection.
    stream.write_all(&socks5_greeting(has_auth)).await?;
    let mut method = [0u8; 2];
    stream.read_exact(&mut method).await?;
    if method[0] != 0x05 {
        bail!(
            "socks5: bad version byte {:#04x} in method reply",
            method[0]
        );
    }
    match method[1] {
        0x00 => {} // no auth required
        0x02 => {
            // 2. Username/password sub-negotiation (RFC 1929).
            let auth = hop.auth.as_ref().ok_or_else(|| {
                anyhow!("socks5: server requires auth but no credentials were given")
            })?;
            stream.write_all(&socks5_userpass_auth(auth)?).await?;
            let mut reply = [0u8; 2];
            stream.read_exact(&mut reply).await?;
            if reply[1] != 0x00 {
                bail!("socks5: authentication rejected by proxy");
            }
        }
        0xFF => bail!("socks5: no acceptable authentication method"),
        other => bail!("socks5: unexpected method {other:#04x}"),
    }

    // 3. CONNECT request → reply.
    stream
        .write_all(&socks5_connect_request(host, port)?)
        .await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        bail!("socks5: bad version byte {:#04x} in connect reply", head[0]);
    }
    if head[1] != 0x00 {
        bail!("socks5: connect failed ({})", socks5_reply_text(head[1]));
    }
    // Consume the bound address (ATYP-dependent) + 2-byte port.
    let bound_len = match head[3] {
        0x01 => 4,  // IPv4
        0x04 => 16, // IPv6
        0x03 => {
            let mut n = [0u8; 1];
            stream.read_exact(&mut n).await?;
            n[0] as usize
        }
        other => bail!("socks5: unknown bound address type {other:#04x}"),
    };
    let mut scratch = vec![0u8; bound_len + 2];
    stream.read_exact(&mut scratch).await?;
    Ok(())
}

/// Human-readable text for a SOCKS5 CONNECT reply code (RFC 1928 §6).
fn socks5_reply_text(code: u8) -> &'static str {
    match code {
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown error",
    }
}

// ----------------------------------------------------------------------------
// HTTP CONNECT (RFC 7231 §4.3.6)
// ----------------------------------------------------------------------------

/// Build an HTTP `CONNECT host:port HTTP/1.1` request, with an optional
/// `Proxy-Authorization: Basic` header when credentials are present.
fn http_connect_request(authority: &str, auth: Option<&HopAuth>) -> String {
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(a) = auth {
        let token = basic_auth_token(&a.username, &a.password);
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    req
}

/// Base64 (standard alphabet) of `user:pass` for HTTP Basic proxy auth.
/// Small self-contained encoder so no new dependency is added.
fn basic_auth_token(user: &str, pass: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{user}:{pass}");
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Perform an HTTP CONNECT on `stream` to `authority`. Live network path
/// (un-CI-able across real proxies).
async fn http_connect(stream: &mut TcpStream, hop: &HopSpec, authority: &str) -> Result<()> {
    let req = http_connect_request(authority, hop.auth.as_ref());
    stream.write_all(req.as_bytes()).await?;

    // Read until the end of the response headers (CRLFCRLF). CONNECT responses
    // have no body, so we stop at the header terminator.
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            bail!("http CONNECT: proxy closed connection before responding");
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            bail!("http CONNECT: response headers too large");
        }
    }

    let status = parse_http_connect_status(&buf)?;
    if !(200..300).contains(&status) {
        bail!("http CONNECT: proxy returned status {status}");
    }
    Ok(())
}

/// Parse the status code from an HTTP CONNECT response's status line.
fn parse_http_connect_status(resp: &[u8]) -> Result<u16> {
    let text = String::from_utf8_lossy(resp);
    let line = text.lines().next().unwrap_or("");
    // Expect: "HTTP/1.1 200 Connection established"
    let mut parts = line.split_whitespace();
    let _version = parts
        .next()
        .filter(|v| v.starts_with("HTTP/"))
        .ok_or_else(|| anyhow!("http CONNECT: malformed response status line {line:?}"))?;
    let code = parts
        .next()
        .ok_or_else(|| anyhow!("http CONNECT: missing status code in {line:?}"))?;
    code.parse::<u16>()
        .map_err(|_| anyhow!("http CONNECT: non-numeric status code in {line:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks5_greeting_no_auth() {
        assert_eq!(socks5_greeting(false), vec![0x05, 0x01, 0x00]);
    }

    #[test]
    fn socks5_greeting_with_auth_offers_userpass() {
        assert_eq!(socks5_greeting(true), vec![0x05, 0x02, 0x00, 0x02]);
    }

    #[test]
    fn socks5_userpass_auth_encoding() {
        let auth = HopAuth {
            username: "user".to_string(),
            password: "pw".to_string(),
        };
        let bytes = socks5_userpass_auth(&auth).unwrap();
        assert_eq!(bytes, vec![0x01, 4, b'u', b's', b'e', b'r', 2, b'p', b'w']);
    }

    #[test]
    fn socks5_userpass_rejects_overlong_fields() {
        let long = "x".repeat(256);
        let auth = HopAuth {
            username: long.clone(),
            password: "pw".to_string(),
        };
        assert!(socks5_userpass_auth(&auth).is_err());
        let auth = HopAuth {
            username: "u".to_string(),
            password: long,
        };
        assert!(socks5_userpass_auth(&auth).is_err());
    }

    #[test]
    fn socks5_connect_request_domainname() {
        let bytes = socks5_connect_request("example.com", 443).unwrap();
        // VER, CMD=CONNECT, RSV, ATYP=DOMAINNAME, LEN
        assert_eq!(&bytes[0..5], &[0x05, 0x01, 0x00, 0x03, 11]);
        assert_eq!(&bytes[5..16], b"example.com");
        // port 443 = 0x01BB big-endian
        assert_eq!(&bytes[16..18], &[0x01, 0xBB]);
    }

    #[test]
    fn socks5_connect_request_rejects_overlong_host() {
        let host = "h".repeat(256);
        assert!(socks5_connect_request(&host, 80).is_err());
    }

    #[test]
    fn socks5_reply_text_known_codes() {
        assert_eq!(socks5_reply_text(0x05), "connection refused");
        assert_eq!(socks5_reply_text(0x03), "network unreachable");
        assert_eq!(socks5_reply_text(0xAA), "unknown error");
    }

    #[test]
    fn http_connect_request_no_auth() {
        let req = http_connect_request("tunnel.lane.show:443", None);
        assert_eq!(
            req,
            "CONNECT tunnel.lane.show:443 HTTP/1.1\r\nHost: tunnel.lane.show:443\r\n\r\n"
        );
    }

    #[test]
    fn http_connect_request_with_basic_auth() {
        let auth = HopAuth {
            username: "aladdin".to_string(),
            password: "opensesame".to_string(),
        };
        let req = http_connect_request("gw:8080", Some(&auth));
        // RFC 7617 canonical example.
        assert!(
            req.contains("Proxy-Authorization: Basic YWxhZGRpbjpvcGVuc2VzYW1l\r\n"),
            "{req}"
        );
        assert!(req.starts_with("CONNECT gw:8080 HTTP/1.1\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn basic_auth_token_padding() {
        // RFC 4648 vectors.
        assert_eq!(basic_auth_token("", ""), "Og=="); // ":" → "Og=="
                                                      // "aladdin:opensesame" canonical.
        assert_eq!(
            basic_auth_token("aladdin", "opensesame"),
            "YWxhZGRpbjpvcGVuc2VzYW1l"
        );
    }

    #[test]
    fn parse_http_connect_status_ok() {
        let resp = b"HTTP/1.1 200 Connection established\r\n\r\n";
        assert_eq!(parse_http_connect_status(resp).unwrap(), 200);
    }

    #[test]
    fn parse_http_connect_status_error() {
        let resp = b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n";
        assert_eq!(parse_http_connect_status(resp).unwrap(), 407);
    }

    #[test]
    fn parse_http_connect_status_malformed() {
        assert!(parse_http_connect_status(b"garbage\r\n").is_err());
        assert!(parse_http_connect_status(b"HTTP/1.1\r\n").is_err());
        assert!(parse_http_connect_status(b"HTTP/1.1 abc\r\n").is_err());
    }

    #[test]
    fn split_authority_parses_host_port() {
        assert_eq!(
            split_authority("tunnel.lane.show:443").unwrap(),
            ("tunnel.lane.show".to_string(), 443)
        );
        assert!(split_authority("noport").is_err());
        assert!(split_authority("host:notaport").is_err());
    }

    // Empty-chain direct dial: connects to a real local listener with no hops.
    #[tokio::test]
    async fn dial_through_hops_empty_chain_is_direct() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let target = format!("127.0.0.1:{}", addr.port());
        let stream = dial_through_hops(&[], &target).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap().port(), addr.port());
        accept.await.unwrap();
    }
}
