//! lane's own **governed forward proxy** — the packet-level egress governor for
//! `lane web` (ADR-0001 §2/§4).
//!
//! This is the missing half of governed egress: the `lane web` seam ([`super`])
//! only gates the *entry* URL before spawning obscura. [`GovernedProxy`] is the
//! loopback HTTP forward proxy that lane RUNS and pins obscura's egress to (via
//! obscura's `--proxy`), so **every** connection obscura opens — not just the
//! entry navigation — is checked against the deny-by-default
//! [`crate::webpolicy`] gate and recorded in lane's access log. obscura cannot
//! reach the network except through this listener; lane is the egress governor.
//!
//! # Governance model (connection-level, no TLS MITM)
//!
//! A forward proxy sees two request shapes, and lane governs both at the
//! granularity webpolicy operates on (host + port + scheme):
//!
//! - **`CONNECT host:port`** (the HTTPS tunnel obscura opens for every `https://`
//!   origin): lane runs [`WebPolicy::check_addr`] on the host/port. ALLOW → reply
//!   `200 Connection Established` and [`tokio::io::copy_bidirectional`] the opaque
//!   TLS bytes between obscura and the origin. DENY → `403 Forbidden`, close,
//!   **never** connect upstream. lane does **not** terminate/inspect the TLS
//!   (no MITM): webpolicy is host/port-level, so CONNECT-level governance matches
//!   its granularity exactly. (The `--ca` capability the spawn plan emits is
//!   forward-compat for a future path-level MITM mode; this proxy does not build
//!   it.)
//! - **absolute-form plain HTTP** (`GET http://host/path HTTP/1.1`, as a forward
//!   proxy receives plain-HTTP requests): lane runs [`WebPolicy::check`] on the
//!   absolute URL. ALLOW → forward to the origin and stream the response back.
//!   DENY → `403 Forbidden`.
//!
//! Anything malformed or unrecognized fails **closed** (`403`).
//!
//! # Deny-by-default + observability
//!
//! Every request (ALLOW and DENY) is logged via [`crate::log::info`] — the single
//! place all governed agent web traffic is observable (ADR-0001 §4), e.g.
//! `web-egress ALLOW CONNECT example.com:443` /
//! `web-egress DENY GET http://10.0.0.1/ (blocked: private/internal network address)`.
//!
//! # Upstream chaining (`obscura_proxy` = upstream)
//!
//! When the caller supplies an `upstream` proxy (lane's `obscura_proxy` config),
//! it becomes the OPTIONAL upstream lane's governed proxy chains *allowed* traffic
//! through (so an org can still route egress via a corporate proxy **after** lane
//! has governed it). v1 implements the **direct** case fully; if an upstream is
//! configured the proxy still governs but returns a clear, fail-closed error
//! rather than silently dropping the upstream — see [`GovernedProxy::start_with_upstream`].

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::webpolicy::{Scheme, WebPolicy};

/// The maximum bytes lane reads while looking for the end of the request head
/// (`\r\n\r\n`). A forward-proxy request line + headers far exceeds nothing
/// reasonable below this; anything larger is treated as malformed (fail closed).
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// A running, lane-governed forward proxy bound to loopback. Hand its
/// [`addr`](GovernedProxy::addr) to obscura's `--proxy`; every connection obscura
/// opens is then policy-checked and logged. Egress is governed for the proxy's
/// whole lifetime; dropping the handle (or calling
/// [`shutdown`](GovernedProxy::shutdown)) aborts the accept loop and frees the
/// port.
#[derive(Debug)]
pub struct GovernedProxy {
    addr: SocketAddr,
    accept_task: JoinHandle<()>,
}

impl GovernedProxy {
    /// Start a governed forward proxy on an ephemeral loopback port, governing
    /// egress with `policy` and connecting to allowed origins **directly**.
    pub async fn start(policy: WebPolicy) -> Result<GovernedProxy> {
        Self::start_with_upstream(policy, None).await
    }

    /// Start a governed forward proxy, optionally chaining allowed traffic
    /// through an `upstream` proxy after governance.
    ///
    /// `upstream` is lane's `obscura_proxy` config repurposed: it is **not** the
    /// proxy obscura points at (obscura points at *this* governed proxy); it is
    /// the proxy lane's governed proxy itself chains *allowed* traffic through.
    ///
    /// v1 implements the **direct** case (`upstream == None`) fully. If an
    /// upstream IS supplied, the proxy is **not** started and a clear,
    /// fail-closed error is returned (rather than silently ignoring the upstream
    /// and leaking traffic direct): upstream-proxy chaining is not yet
    /// implemented. This is documented and deliberate — never a silent drop.
    pub async fn start_with_upstream(
        policy: WebPolicy,
        upstream: Option<String>,
    ) -> Result<GovernedProxy> {
        if let Some(up) = upstream {
            anyhow::bail!(
                "upstream proxy chaining not yet supported; unset `obscura_proxy` \
                 (got upstream {up:?}). lane's governed proxy connects directly in v1; \
                 chaining allowed egress through a downstream proxy is future work"
            );
        }

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding lane governed proxy on loopback")?;
        let addr = listener
            .local_addr()
            .context("reading governed proxy local addr")?;

        let policy = Arc::new(policy);
        let accept_task = tokio::spawn(async move {
            loop {
                let (client, _peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    // Accept errors are transient (e.g. fd pressure); keep serving.
                    Err(_) => continue,
                };
                let policy = Arc::clone(&policy);
                tokio::spawn(async move {
                    // A failed connection is logged inside; the error is opaque
                    // to the accept loop (one bad client never stops the proxy).
                    let _ = handle_conn(client, policy).await;
                });
            }
        });

        Ok(GovernedProxy { addr, accept_task })
    }

    /// The `http://127.0.0.1:<port>` URL to hand obscura's `--proxy`. Every
    /// connection obscura opens through this is governed.
    pub fn addr(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The raw socket address the proxy is listening on (for tests / diagnostics).
    pub fn socket_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop the proxy: abort the accept loop and free the port. Idempotent; also
    /// runs on [`Drop`].
    pub fn shutdown(&self) {
        self.accept_task.abort();
    }
}

impl Drop for GovernedProxy {
    fn drop(&mut self) {
        // RAII: when the handle goes out of scope (e.g. obscura has exited),
        // governance ends and the loopback port is released.
        self.accept_task.abort();
    }
}

/// The parsed first line of a forward-proxy request: either a CONNECT tunnel or
/// an absolute-form plain-HTTP request.
enum ProxyRequest {
    /// `CONNECT host:port HTTP/1.1` — open an opaque TLS tunnel.
    Connect { host: String, port: u16 },
    /// `METHOD http://host/path HTTP/1.1` — forward a plain-HTTP request. Carries
    /// the absolute target URL (for policy + forwarding).
    Http { url: String },
}

/// Handle one accepted connection: read the request head, govern it, and either
/// tunnel/forward (ALLOW) or reject with `403` (DENY / malformed).
async fn handle_conn(mut client: TcpStream, policy: Arc<WebPolicy>) -> Result<()> {
    // Read until the end of the request head (`\r\n\r\n`) or the cap.
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find_head_end(&head) {
            break pos;
        }
        if head.len() >= MAX_HEAD_BYTES {
            // Oversized / never-terminating head: fail closed.
            return deny(&mut client, "request head too large").await;
        }
        let n = client.read(&mut buf).await?;
        if n == 0 {
            // Client closed before sending a full head.
            return deny(&mut client, "incomplete request").await;
        }
        head.extend_from_slice(&buf[..n]);
    };

    let request_line = match first_line(&head) {
        Some(line) => line,
        None => return deny(&mut client, "malformed request line").await,
    };

    match parse_request_line(&request_line) {
        Some(ProxyRequest::Connect { host, port }) => {
            handle_connect(&mut client, &policy, &host, port).await
        }
        Some(ProxyRequest::Http { url }) => {
            // Pass the full head (request line + headers) so the forwarder can
            // replay the method + headers to the origin.
            handle_http(&mut client, &policy, &url, &head, head_end).await
        }
        None => {
            crate::log::info(&format!("web-egress DENY (unparsable) {request_line}"));
            deny(&mut client, "unsupported proxy request").await
        }
    }
}

/// Govern + service a `CONNECT host:port` tunnel.
async fn handle_connect(
    client: &mut TcpStream,
    policy: &WebPolicy,
    host: &str,
    port: u16,
) -> Result<()> {
    // CONNECT is always for TLS origins (https): check at https granularity.
    let decision = policy.check_addr(host, port, Scheme::Https);
    if let crate::webpolicy::PolicyDecision::Deny(reason) = decision {
        crate::log::info(&format!("web-egress DENY CONNECT {host}:{port} ({reason})"));
        return deny(client, &reason.to_string()).await;
    }

    // ALLOW: open the upstream and splice. Connecting can still fail (origin
    // down) — that is a 502-class failure, not a policy denial.
    let upstream = match TcpStream::connect((host, port)).await {
        Ok(s) => s,
        Err(e) => {
            crate::log::info(&format!(
                "web-egress ALLOW CONNECT {host}:{port} — upstream unreachable: {e}"
            ));
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await
                .ok();
            return Ok(());
        }
    };

    crate::log::info(&format!("web-egress ALLOW CONNECT {host}:{port}"));
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("acking CONNECT")?;

    let mut upstream = upstream;
    // Opaque byte splice — no TLS termination (no MITM); webpolicy is
    // host/port-level so CONNECT-level governance already matches its grain.
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
    Ok(())
}

/// Govern + service an absolute-form plain-HTTP forward-proxy request.
async fn handle_http(
    client: &mut TcpStream,
    policy: &WebPolicy,
    url: &str,
    head: &[u8],
    head_end: usize,
) -> Result<()> {
    let decision = policy.check(url);
    if let crate::webpolicy::PolicyDecision::Deny(reason) = decision {
        let method = first_token(head).unwrap_or_else(|| "?".to_string());
        crate::log::info(&format!("web-egress DENY {method} {url} ({reason})"));
        return deny(client, &reason.to_string()).await;
    }

    let method = first_token(head).unwrap_or_else(|| "GET".to_string());
    crate::log::info(&format!("web-egress ALLOW {method} {url}"));

    // Forward via the in-tree reqwest client. Replay the method and the
    // forwardable request headers; the body (if any) follows the head in the
    // buffer we already read.
    let reqwest_method = match reqwest::Method::from_bytes(method.as_bytes()) {
        Ok(m) => m,
        Err(_) => return deny(client, "unsupported method").await,
    };

    let headers = parse_headers(head, head_end);
    let body = head.get(head_end..).unwrap_or(&[]).to_vec();

    let mut builder = match reqwest::Client::builder().build() {
        Ok(c) => c.request(reqwest_method, url),
        Err(e) => {
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await
                .ok();
            return Err(e).context("building forward-proxy http client");
        }
    };
    for (name, value) in &headers {
        // Hop-by-hop and proxy-specific headers are dropped when forwarding.
        if is_hop_by_hop(name) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if !body.is_empty() {
        builder = builder.body(body);
    }

    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            crate::log::info(&format!(
                "web-egress ALLOW {method} {url} — upstream error: {e}"
            ));
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await
                .ok();
            return Ok(());
        }
    };

    write_http_response(client, resp).await
}

/// Serialize a reqwest response back to the proxy client (status line, headers,
/// body).
async fn write_http_response(client: &mut TcpStream, resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    let reason = status.canonical_reason().unwrap_or("");
    let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), reason).into_bytes();

    // Snapshot the forwardable headers before consuming the body (`bytes()`
    // takes ownership of the response). Emit a normalized framing: drop the
    // origin's transfer-encoding / content-length and set our own from the
    // fully-read body.
    let headers: Vec<(String, Vec<u8>)> = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let n = name.as_str();
            if n.eq_ignore_ascii_case("transfer-encoding")
                || n.eq_ignore_ascii_case("content-length")
                || n.eq_ignore_ascii_case("connection")
            {
                None
            } else {
                Some((n.to_string(), value.as_bytes().to_vec()))
            }
        })
        .collect();

    let body = resp.bytes().await.unwrap_or_default();
    for (name, value) in &headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(&body);

    client
        .write_all(&out)
        .await
        .context("writing proxied response")?;
    Ok(())
}

/// Reply `403 Forbidden` with a short reason body and close. Errors writing are
/// swallowed (the client may already be gone); the caller fails closed anyway.
async fn deny(client: &mut TcpStream, reason: &str) -> Result<()> {
    let body = format!("lane governed proxy: denied ({reason})\n");
    let resp = format!(
        "HTTP/1.1 403 Forbidden\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    client.write_all(resp.as_bytes()).await.ok();
    Ok(())
}

/// Find the byte offset just past the `\r\n\r\n` head terminator, if present.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// The first request line (up to the first CRLF), as a UTF-8 string.
fn first_line(head: &[u8]) -> Option<String> {
    let end = head.windows(2).position(|w| w == b"\r\n")?;
    std::str::from_utf8(&head[..end]).ok().map(str::to_string)
}

/// The first whitespace-delimited token of the head (the HTTP method).
fn first_token(head: &[u8]) -> Option<String> {
    let line = first_line(head)?;
    line.split_whitespace().next().map(str::to_string)
}

/// Parse a forward-proxy request line into [`ProxyRequest`], or `None` if it is
/// malformed / unsupported (caller fails closed).
fn parse_request_line(line: &str) -> Option<ProxyRequest> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    // The HTTP version is required but not otherwise used.
    let _version = parts.next()?;

    if method.eq_ignore_ascii_case("CONNECT") {
        // authority-form: host:port
        let (host, port) = split_authority(target)?;
        return Some(ProxyRequest::Connect { host, port });
    }

    // A forward proxy receives plain-HTTP in absolute-form: the target MUST be
    // an absolute URL (`http://...`). Origin-form (`/path`) is not a proxy
    // request and is rejected (fail closed).
    if target.starts_with("http://") || target.starts_with("https://") {
        return Some(ProxyRequest::Http {
            url: target.to_string(),
        });
    }

    None
}

/// Split a CONNECT authority `host:port` (bracketed IPv6 supported). Returns
/// `None` if no valid port is present (CONNECT requires an explicit port).
fn split_authority(authority: &str) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        // [ipv6]:port
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = after.strip_prefix(':')?.parse::<u16>().ok()?;
        if port == 0 {
            return None;
        }
        return Some((host.to_string(), port));
    }
    let (host, port_str) = authority.rsplit_once(':')?;
    if host.is_empty() || host.contains(':') {
        return None;
    }
    let port = port_str.parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_string(), port))
}

/// Parse the `Name: value` header lines from the head (between the request line
/// and `head_end`).
fn parse_headers(head: &[u8], head_end: usize) -> Vec<(String, String)> {
    let head_str = match std::str::from_utf8(head.get(..head_end).unwrap_or(head)) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    // Skip the request line; parse the rest until the blank line.
    for line in head_str.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

/// `true` for hop-by-hop / proxy-specific headers that must not be forwarded to
/// the origin.
fn is_hop_by_hop(name: &str) -> bool {
    const HOP: &[&str] = &[
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "host",
    ];
    HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A policy allowing exactly one host on default ports, denying all else.
    fn allow_only(host: &str) -> WebPolicy {
        WebPolicy::default().allow_host(host)
    }

    /// Read a full HTTP-ish response (until EOF) from a stream as a String.
    async fn read_to_string(mut s: TcpStream) -> String {
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Read just the status line (up to the first CRLF) from a stream.
    async fn read_status_line(s: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut one = [0u8; 1];
        loop {
            let n = s.read(&mut one).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.push(one[0]);
            if buf.ends_with(b"\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&buf).trim_end().to_string()
    }

    #[tokio::test]
    async fn start_with_upstream_fails_closed() {
        // An upstream proxy is documented-but-unsupported in v1: it must error,
        // never silently start a direct (ungoverned-upstream) proxy.
        let err = GovernedProxy::start_with_upstream(
            allow_only("example.com"),
            Some("http://corp-proxy:8080".to_string()),
        )
        .await
        .expect_err("upstream chaining must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("upstream proxy chaining not yet supported"),
            "{msg}"
        );
        assert!(msg.contains("unset"), "{msg}");
    }

    #[tokio::test]
    async fn addr_is_loopback_http_url() {
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();
        let addr = proxy.addr();
        assert!(addr.starts_with("http://127.0.0.1:"), "{addr}");
    }

    #[tokio::test]
    async fn connect_to_allowed_host_gets_200_and_tunnels() {
        // Spin a local "upstream" TCP listener that echoes a marker, allowlist
        // its host (127.0.0.1 is loopback so we allowlist via a hostname that
        // we... can't resolve). Instead: allow the literal and disable the IP
        // guard for THIS test only (the guard is webpolicy's job, exercised in
        // its own suite); here we prove the proxy's CONNECT splice + gate wiring.
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = upstream.accept().await {
                // Echo a fixed marker once the tunnel is established.
                sock.write_all(b"UPSTREAM-OK").await.ok();
                // Drain anything the client sends, then close.
                let mut buf = [0u8; 64];
                let _ = sock.read(&mut buf).await;
            }
        });

        // Allow the loopback literal AND turn off the IP-literal guard so the
        // CONNECT reaches our local upstream (the SSRF guard is proven in the
        // webpolicy suite; here we test the proxy's tunnel behavior).
        let mut policy = WebPolicy::default().allow_host("127.0.0.1");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(up_addr.port());
        let proxy = GovernedProxy::start(policy).await.unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", up_addr.port()).as_bytes())
            .await
            .unwrap();

        let status = read_status_line(&mut client).await;
        assert!(status.contains("200"), "expected 200, got {status:?}");

        // Consume the rest of the CONNECT ack (the second CRLF) then read the
        // tunneled upstream marker.
        let mut rest = [0u8; 2];
        client.read_exact(&mut rest).await.unwrap();
        let mut marker = [0u8; 11];
        client.read_exact(&mut marker).await.unwrap();
        assert_eq!(&marker, b"UPSTREAM-OK");
    }

    #[tokio::test]
    async fn connect_to_denied_host_gets_403_and_no_upstream() {
        // A denied CONNECT must NOT open the upstream. We prove "no upstream" by
        // pointing at a local listener and asserting it never accepts.
        let canary = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let canary_addr = canary.local_addr().unwrap();
        let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accepted2 = Arc::clone(&accepted);
        tokio::spawn(async move {
            if canary.accept().await.is_ok() {
                accepted2.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        // Policy allows only example.com — the canary's loopback host is denied.
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(
                format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", canary_addr.port()).as_bytes(),
            )
            .await
            .unwrap();

        let resp = read_to_string(client).await;
        assert!(resp.contains("403"), "expected 403, got {resp:?}");

        // Give any (erroneous) upstream connect a moment; assert it never happened.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !accepted.load(std::sync::atomic::Ordering::SeqCst),
            "denied CONNECT must not reach the upstream"
        );
    }

    #[tokio::test]
    async fn plain_http_to_denied_url_gets_403() {
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();
        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        // Absolute-form forward-proxy request to a denied host.
        client
            .write_all(b"GET http://blocked.test/ HTTP/1.1\r\nHost: blocked.test\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(resp.contains("403"), "expected 403, got {resp:?}");
    }

    #[tokio::test]
    async fn plain_http_to_private_ip_gets_403() {
        // SSRF: a private-IP absolute URL is denied even with a broad allowlist.
        let proxy = GovernedProxy::start(WebPolicy::default().allow_domain("example.com"))
            .await
            .unwrap();
        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(b"GET http://10.0.0.1/ HTTP/1.1\r\nHost: 10.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(resp.contains("403"), "expected 403, got {resp:?}");
    }

    #[tokio::test]
    async fn malformed_request_fails_closed_with_403() {
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();
        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        // Origin-form (not a proxy request) — fail closed.
        client
            .write_all(b"GET /local/path HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(resp.contains("403"), "expected 403, got {resp:?}");
    }

    #[tokio::test]
    async fn plain_http_to_allowed_url_is_forwarded() {
        // Stand up a tiny origin HTTP server on loopback. Allow its host literal
        // and (for this test) disable the IP guard, then prove the proxy
        // forwards and relays the body.
        let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = origin.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                sock.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nHELLO",
                )
                .await
                .ok();
            }
        });

        let mut policy = WebPolicy::default().allow_host("127.0.0.1");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(origin_addr.port());
        let proxy = GovernedProxy::start(policy).await.unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        let req = format!(
            "GET http://127.0.0.1:{}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            origin_addr.port()
        );
        client.write_all(req.as_bytes()).await.unwrap();
        let resp = read_to_string(client).await;
        assert!(resp.contains("200"), "expected 200, got {resp:?}");
        assert!(
            resp.contains("HELLO"),
            "expected relayed body, got {resp:?}"
        );
    }

    // --- parser unit tests --------------------------------------------------

    #[test]
    fn parse_connect_line() {
        match parse_request_line("CONNECT example.com:443 HTTP/1.1") {
            Some(ProxyRequest::Connect { host, port }) => {
                assert_eq!(host, "example.com");
                assert_eq!(port, 443);
            }
            other => panic!("expected Connect, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn parse_absolute_http_line() {
        match parse_request_line("GET http://example.com/path HTTP/1.1") {
            Some(ProxyRequest::Http { url }) => assert_eq!(url, "http://example.com/path"),
            other => panic!("expected Http, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn parse_origin_form_is_rejected() {
        // origin-form is not a forward-proxy request → None (fail closed).
        assert!(parse_request_line("GET /path HTTP/1.1").is_none());
    }

    #[test]
    fn parse_connect_requires_port() {
        assert!(parse_request_line("CONNECT example.com HTTP/1.1").is_none());
    }

    #[test]
    fn split_authority_ipv6() {
        assert_eq!(split_authority("[::1]:443"), Some(("::1".to_string(), 443)));
    }
}
