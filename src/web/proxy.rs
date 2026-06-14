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
//! has governed it). Governance is unchanged and still happens **first**: a denied
//! target is rejected before any upstream connect. ALLOWED traffic then egresses
//! via the upstream:
//!
//! - **plain HTTP** is forwarded by a reqwest client built with
//!   [`reqwest::Proxy::all`] pointed at the upstream.
//! - **CONNECT** is tunneled through a nested CONNECT to the upstream: lane opens a
//!   TCP connection to the upstream proxy, sends its own `CONNECT host:port`, and —
//!   on a `200` from the upstream — splices the client to that tunnel. A non-`200`
//!   upstream reply yields a `502` to the client (fail closed, never a direct
//!   connect). See [`GovernedProxy::start_with_upstream`].
//!
//! # Hardening (Phase B)
//!
//! - a [`Semaphore`] caps concurrent in-flight proxied connections
//!   ([`MAX_CONCURRENT_CONNS`]);
//! - the request-head read is bounded by both a byte cap ([`MAX_HEAD_BYTES`]) and a
//!   wall-clock timeout ([`HEAD_READ_TIMEOUT`]) — slowloris defense;
//! - every upstream connect / CONNECT handshake is wrapped in [`CONNECT_TIMEOUT`];
//! - inbound proxy-specific headers (`Proxy-Connection`, `Proxy-Authorization`, …)
//!   are dropped and never forwarded; hop-by-hop headers are stripped;
//! - every parse / timeout / error path **denies or closes** — it never falls
//!   through to an ungoverned connect.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::webpolicy::{Scheme, WebPolicy};

/// The maximum bytes lane reads while looking for the end of the request head
/// (`\r\n\r\n`). A forward-proxy request line + headers far exceeds nothing
/// reasonable below this; anything larger is treated as malformed (fail closed).
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// The maximum number of in-flight proxied connections served concurrently. Over
/// this cap, new connections wait for a permit (back-pressure) rather than being
/// spawned unbounded — a single misbehaving client cannot exhaust fds/memory.
const MAX_CONCURRENT_CONNS: usize = 256;

/// Wall-clock budget for reading a full request head. A client that dribbles bytes
/// (or never sends `\r\n\r\n`) is closed once this elapses — slowloris defense.
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Wall-clock budget for opening an upstream/origin TCP connection (and, when
/// chaining, for the upstream CONNECT handshake). On timeout the client gets a
/// `502`; lane never hangs waiting on a dead upstream.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

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

/// A validated upstream proxy lane chains *allowed* egress through. Holds both the
/// authority lane dials for the nested CONNECT (`host:port`) and the original URL
/// the reqwest HTTP forwarder uses for [`reqwest::Proxy::all`].
#[derive(Debug, Clone)]
struct Upstream {
    /// Host of the upstream proxy (for the nested-CONNECT TCP dial).
    host: String,
    /// Port of the upstream proxy (for the nested-CONNECT TCP dial).
    port: u16,
    /// The full proxy URL, handed to `reqwest::Proxy::all` for HTTP forwarding.
    url: String,
}

/// Per-connection state shared with both handlers: the policy gate, the optional
/// upstream to chain through, and the concurrency permit pool.
#[derive(Debug)]
struct ProxyState {
    policy: WebPolicy,
    upstream: Option<Upstream>,
    semaphore: Semaphore,
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
    /// Governance is unchanged and always runs **first** — a denied target is
    /// rejected before any upstream connect. When `upstream` is `Some`, ALLOWED
    /// HTTP egresses via a reqwest client pointed at the upstream, and ALLOWED
    /// CONNECT tunnels are established by a nested CONNECT through the upstream.
    /// A malformed upstream URL is rejected here at `start` (fail closed).
    pub async fn start_with_upstream(
        policy: WebPolicy,
        upstream: Option<String>,
    ) -> Result<GovernedProxy> {
        let upstream = match upstream {
            Some(up) => Some(parse_upstream(&up)?),
            None => None,
        };

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding lane governed proxy on loopback")?;
        let addr = listener
            .local_addr()
            .context("reading governed proxy local addr")?;

        let state = Arc::new(ProxyState {
            policy,
            upstream,
            semaphore: Semaphore::new(MAX_CONCURRENT_CONNS),
        });
        let accept_task = tokio::spawn(async move {
            loop {
                let (client, _peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    // Accept errors are transient (e.g. fd pressure); keep serving.
                    Err(_) => continue,
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    // Concurrency cap: wait for a permit before doing any work, so
                    // over-cap connections back-pressure instead of spawning
                    // unbounded. The permit is held for the connection's lifetime.
                    let _permit = match state.semaphore.acquire().await {
                        Ok(p) => p,
                        // Only errors if the semaphore is closed (never here).
                        Err(_) => return,
                    };
                    // A failed connection is logged inside; the error is opaque
                    // to the accept loop (one bad client never stops the proxy).
                    let _ = handle_conn(client, &state).await;
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
async fn handle_conn(mut client: TcpStream, state: &ProxyState) -> Result<()> {
    // Read the request head under both a byte cap and a wall-clock timeout
    // (slowloris defense). On timeout the connection is closed, never read
    // unboundedly and never connected upstream.
    let (head, head_end) = match timeout(HEAD_READ_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(parsed)) => parsed,
        Ok(Err(reason)) => return deny(&mut client, reason).await,
        Err(_) => return deny(&mut client, "request head read timed out").await,
    };

    let request_line = match first_line(&head) {
        Some(line) => line,
        None => return deny(&mut client, "malformed request line").await,
    };

    match parse_request_line(&request_line) {
        Some(ProxyRequest::Connect { host, port }) => {
            handle_connect(&mut client, state, &host, port).await
        }
        Some(ProxyRequest::Http { url }) => {
            // Pass the full head (request line + headers) so the forwarder can
            // replay the method + headers to the origin.
            handle_http(&mut client, state, &url, &head, head_end).await
        }
        None => {
            crate::log::info(&format!("web-egress DENY (unparsable) {request_line}"));
            deny(&mut client, "unsupported proxy request").await
        }
    }
}

/// Read the request head up to `\r\n\r\n` or the [`MAX_HEAD_BYTES`] cap. Returns
/// the raw head bytes and the offset just past the terminator. On a too-large or
/// truncated head it returns the deny reason (caller fails closed). The byte cap
/// is enforced **before** each read, so an oversized head is never buffered
/// unboundedly.
async fn read_head(client: &mut TcpStream) -> std::result::Result<(Vec<u8>, usize), &'static str> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 4096];
    loop {
        if let Some(pos) = find_head_end(&head) {
            return Ok((head, pos));
        }
        if head.len() >= MAX_HEAD_BYTES {
            // Oversized / never-terminating head: fail closed.
            return Err("request head too large");
        }
        let n = match client.read(&mut buf).await {
            Ok(n) => n,
            Err(_) => return Err("read error"),
        };
        if n == 0 {
            // Client closed before sending a full head.
            return Err("incomplete request");
        }
        head.extend_from_slice(&buf[..n]);
    }
}

/// Govern + service a `CONNECT host:port` tunnel.
///
/// Governance runs **first**: a denied target is rejected before any connect. On
/// ALLOW, lane opens the origin either directly or — when an upstream proxy is
/// configured — by a nested CONNECT through that upstream, then splices opaque
/// bytes (no TLS termination / no MITM).
async fn handle_connect(
    client: &mut TcpStream,
    state: &ProxyState,
    host: &str,
    port: u16,
) -> Result<()> {
    // CONNECT is always for TLS origins (https): check at https granularity.
    let decision = state.policy.check_addr(host, port, Scheme::Https);
    if let crate::webpolicy::PolicyDecision::Deny(reason) = decision {
        crate::log::info(&format!("web-egress DENY CONNECT {host}:{port} ({reason})"));
        return deny(client, &reason.to_string()).await;
    }

    // ALLOW: open the origin. Either directly, or — when chaining — via a nested
    // CONNECT through the upstream proxy. Connecting can still fail (origin/
    // upstream down or refusing) — that is a 502-class failure, not a policy
    // denial. Every connect is bounded by CONNECT_TIMEOUT (no hang).
    // `leftover` holds any tunnel bytes already read from the upstream past its
    // CONNECT response head (only possible on the chained path); they are replayed
    // to the client before splicing so no origin bytes are lost.
    let (upstream, leftover) = if let Some(up) = &state.upstream {
        match connect_via_upstream(up, host, port).await {
            Ok(pair) => pair,
            Err(e) => {
                crate::log::info(&format!(
                    "web-egress ALLOW CONNECT {host}:{port} — upstream proxy {}:{} failed: {e}",
                    up.host, up.port
                ));
                client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .ok();
                return Ok(());
            }
        }
    } else {
        match timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port))).await {
            Ok(Ok(s)) => (s, Vec::new()),
            Ok(Err(e)) => {
                crate::log::info(&format!(
                    "web-egress ALLOW CONNECT {host}:{port} — upstream unreachable: {e}"
                ));
                client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .ok();
                return Ok(());
            }
            Err(_) => {
                crate::log::info(&format!(
                    "web-egress ALLOW CONNECT {host}:{port} — upstream connect timed out"
                ));
                client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .ok();
                return Ok(());
            }
        }
    };

    crate::log::info(&format!("web-egress ALLOW CONNECT {host}:{port}"));
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("acking CONNECT")?;
    if !leftover.is_empty() {
        // Replay tunnel bytes the upstream sent before we started splicing.
        client
            .write_all(&leftover)
            .await
            .context("replaying upstream tunnel prefix")?;
    }

    let mut upstream = upstream;
    // Opaque byte splice — no TLS termination (no MITM); webpolicy is
    // host/port-level so CONNECT-level governance already matches its grain.
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
    Ok(())
}

/// Open a tunnel to `host:port` **through** the configured upstream proxy: dial
/// the upstream's TCP socket, send a nested `CONNECT host:port`, and on a `2xx`
/// status line return the live socket (now a tunnel to the origin) **plus** any
/// tunnel bytes the upstream already sent past its response head (so they are not
/// lost — `read_head` may buffer beyond `\r\n\r\n`). The dial and the
/// handshake-read are each bounded by [`CONNECT_TIMEOUT`]. A non-2xx reply, a
/// malformed status, or a timeout is an error (caller maps it to `502` — never a
/// direct connect).
async fn connect_via_upstream(
    up: &Upstream,
    host: &str,
    port: u16,
) -> Result<(TcpStream, Vec<u8>)> {
    let mut sock = match timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((up.host.as_str(), up.port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e).context("connecting to upstream proxy"),
        Err(_) => anyhow::bail!("connecting to upstream proxy timed out"),
    };

    // Forward-proxy nested CONNECT request to the upstream.
    let req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
    sock.write_all(req.as_bytes())
        .await
        .context("sending CONNECT to upstream proxy")?;

    // Read the upstream's response head (status line + headers) under a timeout,
    // bounded by MAX_HEAD_BYTES (never read unboundedly).
    let (head, head_end) = match timeout(CONNECT_TIMEOUT, read_head(&mut sock)).await {
        Ok(Ok(parsed)) => parsed,
        Ok(Err(reason)) => anyhow::bail!("reading upstream CONNECT response: {reason}"),
        Err(_) => anyhow::bail!("upstream CONNECT handshake timed out"),
    };

    let status = first_line(&head).context("upstream CONNECT response had no status line")?;
    // Expect `HTTP/1.1 200 ...` (any 2xx is treated as success for tolerance).
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if !code.starts_with('2') {
        anyhow::bail!("upstream proxy refused CONNECT (status: {status})");
    }
    // Any bytes the upstream sent after its response head are the start of the
    // tunneled stream — hand them back so the caller can replay them to the
    // client before splicing (otherwise the first origin bytes are dropped).
    let leftover = head.get(head_end..).unwrap_or(&[]).to_vec();
    Ok((sock, leftover))
}

/// Govern + service an absolute-form plain-HTTP forward-proxy request.
async fn handle_http(
    client: &mut TcpStream,
    state: &ProxyState,
    url: &str,
    head: &[u8],
    head_end: usize,
) -> Result<()> {
    let decision = state.policy.check(url);
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

    // When chaining, build the client with the upstream proxy so allowed egress
    // routes THROUGH it (after governance); otherwise the client connects direct.
    let mut client_builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
    if let Some(up) = &state.upstream {
        match reqwest::Proxy::all(&up.url) {
            Ok(proxy) => client_builder = client_builder.proxy(proxy),
            Err(e) => {
                client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .ok();
                return Err(e).context("configuring upstream proxy for forward-proxy http client");
            }
        }
    }

    let mut builder = match client_builder.build() {
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

/// Parse and validate an upstream proxy URL (`http://host:port`). lane chains
/// allowed egress through this. Requires the `http` scheme, a host, and an
/// explicit port; anything malformed is rejected here at `start` (fail closed).
/// `https`/SOCKS upstreams are intentionally rejected: lane's nested-CONNECT
/// tunnel speaks plain HTTP to the upstream proxy (TLS-to-proxy / SOCKS is future
/// work, not silently downgraded).
fn parse_upstream(url: &str) -> Result<Upstream> {
    let rest = url
        .strip_prefix("http://")
        .with_context(|| format!("upstream proxy URL must start with `http://` (got {url:?})"))?;
    // Strip any path/query: a proxy URL's authority is all we dial.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())
        .with_context(|| format!("upstream proxy URL has no host:port (got {url:?})"))?;
    // Reject embedded credentials for now (we never forward proxy-auth upstream).
    if authority.contains('@') {
        anyhow::bail!("upstream proxy URL must not contain credentials (got {url:?})");
    }
    let (host, port) = split_authority(authority)
        .with_context(|| format!("upstream proxy URL needs an explicit host:port (got {url:?})"))?;
    Ok(Upstream {
        host,
        port,
        url: url.to_string(),
    })
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
    async fn start_with_malformed_upstream_fails_closed() {
        // A malformed upstream proxy URL must be rejected at start (fail closed),
        // never start a proxy that would silently connect direct.
        for bad in [
            "corp-proxy:8080",           // no scheme
            "https://corp-proxy:8080",   // wrong scheme (TLS-to-proxy is future work)
            "http://corp-proxy",         // no explicit port
            "http://user:pw@proxy:8080", // embedded creds rejected
            "http://",                   // no authority
        ] {
            let err = GovernedProxy::start_with_upstream(
                allow_only("example.com"),
                Some(bad.to_string()),
            )
            .await
            .expect_err("malformed upstream must fail closed");
            let msg = err.to_string();
            assert!(
                msg.contains("upstream proxy URL"),
                "expected upstream-URL error for {bad:?}, got {msg}"
            );
        }
    }

    #[tokio::test]
    async fn start_with_valid_upstream_succeeds() {
        // A well-formed upstream URL starts the governed proxy (chaining armed).
        let proxy = GovernedProxy::start_with_upstream(
            allow_only("example.com"),
            Some("http://127.0.0.1:8080".to_string()),
        )
        .await
        .expect("valid upstream should start");
        assert!(proxy.addr().starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn parse_upstream_extracts_authority() {
        let up = parse_upstream("http://proxy.local:3128").unwrap();
        assert_eq!(up.host, "proxy.local");
        assert_eq!(up.port, 3128);
        assert_eq!(up.url, "http://proxy.local:3128");
        // Path/query are stripped from the dialled authority but the full URL is
        // preserved for reqwest::Proxy::all.
        let up2 = parse_upstream("http://proxy.local:3128/pac?x=1").unwrap();
        assert_eq!(up2.host, "proxy.local");
        assert_eq!(up2.port, 3128);
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

    // --- upstream-chaining tests (hermetic fake upstream) -------------------

    /// A tiny in-process fake upstream proxy. It accepts ONE connection, reads a
    /// nested `CONNECT host:port` request, replies `200 Connection Established`,
    /// then bridges the tunnel to a local echo (writes a marker, drains input).
    /// Sets `seen` true once it accepts, so a denied target can be proven to never
    /// reach it. Returns the upstream's listen address.
    async fn spawn_fake_connect_upstream(seen: Arc<std::sync::atomic::AtomicBool>) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                // Read the nested CONNECT head.
                let (_head, _end) = read_head(&mut sock).await.unwrap();
                sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .ok();
                // Now act as the tunneled origin: emit a marker, drain input.
                sock.write_all(b"VIA-UPSTREAM").await.ok();
                let mut buf = [0u8; 64];
                let _ = sock.read(&mut buf).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn connect_chains_through_upstream() {
        // An ALLOWED CONNECT must flow THROUGH the fake upstream (nested CONNECT),
        // and the bytes the upstream tunnel emits must reach the client.
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let up_addr = spawn_fake_connect_upstream(Arc::clone(&seen)).await;

        // Allow the (loopback) target host; disable the IP-literal guard for this
        // wiring test (SSRF guard proven in the webpolicy suite). The target port
        // is arbitrary — the upstream tunnels regardless.
        let mut policy = WebPolicy::default().allow_host("example.com");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(443);
        let proxy = GovernedProxy::start_with_upstream(
            policy,
            Some(format!("http://127.0.0.1:{}", up_addr.port())),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();

        let status = read_status_line(&mut client).await;
        assert!(status.contains("200"), "expected 200, got {status:?}");
        // Consume the trailing CRLF of the ack, then read the upstream marker.
        let mut rest = [0u8; 2];
        client.read_exact(&mut rest).await.unwrap();
        let mut marker = [0u8; 12];
        client.read_exact(&mut marker).await.unwrap();
        assert_eq!(&marker, b"VIA-UPSTREAM");
        assert!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            "allowed CONNECT must reach the upstream"
        );
    }

    #[tokio::test]
    async fn denied_connect_never_reaches_upstream() {
        // Governance is FIRST: a denied target must NOT touch the upstream proxy.
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let up_addr = spawn_fake_connect_upstream(Arc::clone(&seen)).await;

        // Policy allows only example.com; we CONNECT to a denied host.
        let proxy = GovernedProxy::start_with_upstream(
            allow_only("example.com"),
            Some(format!("http://127.0.0.1:{}", up_addr.port())),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(b"CONNECT blocked.test:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(resp.contains("403"), "expected 403, got {resp:?}");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !seen.load(std::sync::atomic::Ordering::SeqCst),
            "denied CONNECT must never reach the upstream proxy"
        );
    }

    #[tokio::test]
    async fn http_chains_through_upstream() {
        // An ALLOWED plain-HTTP request must egress via the fake HTTP upstream.
        // The fake upstream answers absolute-form proxy requests directly with a
        // marker body and records that it was hit.
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen2 = Arc::clone(&seen);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                seen2.store(true, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                sock.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\nconnection: close\r\n\r\nVIA-PROXY",
                )
                .await
                .ok();
            }
        });

        // Allow the origin host; the request egresses via the upstream proxy.
        let mut policy = WebPolicy::default().allow_host("origin.test");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(80);
        let proxy = GovernedProxy::start_with_upstream(
            policy,
            Some(format!("http://127.0.0.1:{}", up_addr.port())),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(b"GET http://origin.test/ HTTP/1.1\r\nHost: origin.test\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(
            resp.contains("200"),
            "expected 200 via upstream, got {resp:?}"
        );
        assert!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            "allowed HTTP must egress via the upstream proxy"
        );
    }

    #[tokio::test]
    async fn upstream_refusal_yields_502() {
        // If the upstream proxy refuses the nested CONNECT (non-2xx), the client
        // gets a 502 — never a fallback direct connect.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let (_head, _end) = read_head(&mut sock).await.unwrap();
                sock.write_all(b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\n\r\n")
                    .await
                    .ok();
            }
        });

        let mut policy = WebPolicy::default().allow_host("example.com");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(443);
        let proxy = GovernedProxy::start_with_upstream(
            policy,
            Some(format!("http://127.0.0.1:{}", up_addr.port())),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let resp = read_to_string(client).await;
        assert!(
            resp.contains("502"),
            "upstream refusal must yield 502, got {resp:?}"
        );
    }

    // --- hardening tests ----------------------------------------------------

    #[tokio::test]
    async fn oversized_head_is_rejected_without_hang() {
        // A request head that never terminates and exceeds the byte cap must be
        // rejected cleanly (no unbounded read, no hang).
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();
        let mut client = TcpStream::connect(proxy.socket_addr()).await.unwrap();
        // Send a valid-looking request line, then a flood of header bytes with no
        // terminating blank line, well over MAX_HEAD_BYTES.
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\n")
            .await
            .unwrap();
        let filler = vec![b'x'; MAX_HEAD_BYTES + 8 * 1024];
        // Best-effort: the proxy may close mid-write once the cap trips.
        let _ = client.write_all(&filler).await;

        // The proxy must respond (403) and close — bounded by the test's own
        // wall clock far below HEAD_READ_TIMEOUT since the byte cap trips first.
        let resp = tokio::time::timeout(std::time::Duration::from_secs(5), read_to_string(client))
            .await
            .expect("oversized head must not hang");
        assert!(
            resp.contains("403"),
            "oversized head must be denied, got {resp:?}"
        );
    }

    #[tokio::test]
    async fn concurrency_cap_serves_many_connections() {
        // The semaphore caps in-flight work; N+1 simultaneous connections must
        // still all be served (queued, not dropped/panicked). Keep it light.
        let proxy = GovernedProxy::start(allow_only("example.com"))
            .await
            .unwrap();
        let addr = proxy.socket_addr();
        let mut handles = Vec::new();
        for _ in 0..(MAX_CONCURRENT_CONNS + 4) {
            handles.push(tokio::spawn(async move {
                let mut client = TcpStream::connect(addr).await.unwrap();
                // Denied target → fast 403 path (no real upstream needed).
                client
                    .write_all(b"GET http://blocked.test/ HTTP/1.1\r\nHost: blocked.test\r\n\r\n")
                    .await
                    .unwrap();
                let resp = read_to_string(client).await;
                assert!(resp.contains("403"), "got {resp:?}");
            }));
        }
        for h in handles {
            h.await.expect("connection task must not panic");
        }
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
