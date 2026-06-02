//! Tunnel client.
//!
//! Faithful port of `internal/tunnel/client.go`. The Go version used
//! `coder/websocket` for the transport and `net/http` to forward requests to
//! the local server. Here we use `tokio-tungstenite` for the WebSocket and
//! `reqwest` for the local HTTP calls, preserving the same control flow:
//!
//! 1. [`Client::connect`] dials the tunnel server, sends a JSON registration
//!    message, reads the JSON registration response, then spawns a background
//!    read loop and returns the public URL.
//! 2. The read loop receives binary frames, each carrying a `request_id` and the
//!    raw HTTP request bytes; it decodes them, forwards the request to
//!    `http://localhost:{local_port}`, serializes the response, and writes it
//!    back as a binary frame guarded by a shared write mutex.
//! 3. A 20s ping keeps the connection alive; lost connections reconnect with
//!    exponential backoff (1s..30s). Close codes 4000 (TTL reached) and 4001
//!    (dropped) stop the loop, as does a registration failure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::httperr;
use crate::log;
use crate::protocol as proto;

/// 10 MiB read limit, mirroring Go's `conn.SetReadLimit(10 << 20)`.
const MAX_MESSAGE_SIZE: usize = 10 << 20;

/// Type alias for the duplex WebSocket stream over a (maybe-TLS) TCP socket.
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// Write half of the split WebSocket, shared across forwarding tasks.
type WsSink = SplitSink<WsStream, Message>;
/// Read half of the split WebSocket.
type WsSource = SplitStream<WsStream>;

/// An observed proxied request, surfaced to the optional `on_request` callback.
pub struct RequestEvent {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration: Duration,
}

/// Options controlling a tunnel [`Client`].
pub struct ClientOptions {
    pub server_url: String,
    pub token: String,
    pub subdomain: String,
    pub domain: String,
    pub local_port: u16,
    pub password: String,
    pub ttl: Option<Duration>,
    pub on_request: Option<Box<dyn Fn(RequestEvent) + Send + Sync>>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            server_url: String::new(),
            token: String::new(),
            subdomain: String::new(),
            domain: String::new(),
            local_port: 0,
            password: String::new(),
            ttl: None,
            on_request: None,
        }
    }
}

/// Shared, immutable-after-connect dial configuration used by the read loop's
/// reconnect path. Mirrors the fields of `ClientOptions` that `dial` reads.
struct DialConfig {
    server_url: String,
    token: String,
    domain: String,
    password: String,
    ttl: Option<Duration>,
    /// The subdomain to (re)register with. Updated to the server-assigned value
    /// after the first successful registration, mirroring Go's
    /// `c.opts.Subdomain = resp.Subdomain`.
    subdomain: Mutex<String>,
    /// Public domain URL (`https://<domain>` or empty), updated on each dial.
    domain_url: Mutex<String>,
}

/// A tunnel client. Construct with [`Client::new`], then [`Client::connect`].
pub struct Client {
    opts: ClientOptions,
    domain_url: String,
    /// Live write half of the current connection, set on connect; used by
    /// [`Client::close`] to send a normal-closure frame.
    sink: Option<Arc<Mutex<WsSink>>>,
    /// Shared dial config handed to the spawned read loop.
    dial_cfg: Arc<DialConfig>,
}

impl Client {
    /// Construct a new client from the given options.
    pub fn new(opts: ClientOptions) -> Self {
        let dial_cfg = Arc::new(DialConfig {
            server_url: opts.server_url.clone(),
            token: opts.token.clone(),
            domain: opts.domain.clone(),
            password: opts.password.clone(),
            ttl: opts.ttl,
            subdomain: Mutex::new(opts.subdomain.clone()),
            domain_url: Mutex::new(String::new()),
        });
        Client {
            opts,
            domain_url: String::new(),
            sink: None,
            dial_cfg,
        }
    }

    /// Dial the tunnel server, register, spawn the background read loop, and
    /// return the public URL. Mirrors Go's `Connect`.
    pub async fn connect(&mut self) -> Result<String> {
        let (stream, url) = dial(&self.dial_cfg).await?;

        self.domain_url = self.dial_cfg.domain_url.lock().await.clone();

        // Split into write/read halves. The write half is shared (Arc<Mutex>)
        // so concurrent request handlers can serialize their frame writes,
        // mirroring Go's `wsMu`.
        let (sink, source) = stream.split();
        let sink = Arc::new(Mutex::new(sink));
        self.sink = Some(sink.clone());

        let dial_cfg = self.dial_cfg.clone();
        let on_request = self.opts.on_request.take();
        let local_port = self.opts.local_port;

        tokio::spawn(async move {
            read_loop(dial_cfg, local_port, on_request, sink, source).await;
        });

        Ok(url)
    }

    /// The public `https://<domain>` URL, or empty if none was assigned.
    /// Mirrors Go's `DomainURL`.
    pub fn domain_url(&self) -> String {
        self.domain_url.clone()
    }

    /// Close the connection with a normal-closure frame. Mirrors Go's `Close`,
    /// which sends `StatusNormalClosure` with reason "client disconnected".
    pub async fn close(&self) {
        if let Some(sink) = &self.sink {
            let mut guard = sink.lock().await;
            let _ = guard
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Normal,
                    reason: "client disconnected".into(),
                })))
                .await;
        }
    }
}

/// Dial the tunnel server and register. Returns the open stream and the public
/// URL on success. Mirrors Go's `Client.dial`.
async fn dial(cfg: &DialConfig) -> Result<(WsStream, String)> {
    crate::install_crypto_provider();

    let ws_config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE_SIZE),
        ..Default::default()
    };

    let (mut stream, _resp) =
        match connect_async_with_config(&cfg.server_url, Some(ws_config), false).await {
            Ok(ok) => ok,
            Err(e) => return Err(httperr::wrap("dialing tunnel server", e)),
        };

    let mut reg = proto::RegistrationRequest {
        token: cfg.token.clone(),
        subdomain: cfg.subdomain.lock().await.clone(),
        domain: cfg.domain.clone(),
        password: cfg.password.clone(),
        ttl: String::new(),
    };
    if let Some(ttl) = cfg.ttl {
        if ttl > Duration::ZERO {
            reg.ttl = format_go_duration(ttl);
        }
    }

    let reg_json = serde_json::to_string(&reg).map_err(|e| anyhow!("sending registration: {e}"))?;
    if let Err(e) = stream.send(Message::Text(reg_json)).await {
        let _ = stream
            .close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: "registration write failed".into(),
            }))
            .await;
        bail!("sending registration: {e}");
    }

    // Read the next text message as the registration response, skipping any
    // non-text control traffic (matching wsjson.Read which reads one JSON value).
    let resp: proto::RegistrationResponse = loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                break serde_json::from_str(&text)
                    .map_err(|e| anyhow!("reading registration response: {e}"))?;
            }
            Some(Ok(Message::Binary(bin))) => {
                break serde_json::from_slice(&bin)
                    .map_err(|e| anyhow!("reading registration response: {e}"))?;
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => {
                let _ = stream
                    .close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: "registration read failed".into(),
                    }))
                    .await;
                bail!("reading registration response: {e}");
            }
            None => {
                bail!("reading registration response: connection closed");
            }
        }
    };

    if !resp.ok {
        let _ = stream
            .close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "registration rejected".into(),
            }))
            .await;
        bail!("registration failed: {}", resp.error);
    }

    if !resp.subdomain.is_empty() {
        *cfg.subdomain.lock().await = resp.subdomain.clone();
    }

    let domain_url = if !resp.domain.is_empty() {
        format!("https://{}", resp.domain)
    } else {
        String::new()
    };
    *cfg.domain_url.lock().await = domain_url;

    Ok((stream, resp.url))
}

/// Background loop driving the connection: process messages until the
/// connection drops, then reconnect with exponential backoff. Mirrors Go's
/// `Client.readLoop`.
async fn read_loop(
    cfg: Arc<DialConfig>,
    local_port: u16,
    on_request: Option<Box<dyn Fn(RequestEvent) + Send + Sync>>,
    mut sink: Arc<Mutex<WsSink>>,
    mut source: WsSource,
) {
    let on_request = on_request.map(Arc::new);
    let mut backoff = Duration::from_secs(1);

    loop {
        let close_code = read_messages(
            &cfg,
            local_port,
            on_request.clone(),
            sink.clone(),
            &mut source,
        )
        .await;

        let reason = match close_code {
            // The connection closed cleanly with no error condition.
            ReadOutcome::Closed => return,
            ReadOutcome::CloseCode(4000) => {
                log::info("tunnel expired (TTL reached)");
                return;
            }
            ReadOutcome::CloseCode(4001) => {
                log::info("tunnel was dropped");
                return;
            }
            ReadOutcome::CloseCode(c) => format!("close code {c}"),
            ReadOutcome::Lost(msg) => msg,
        };

        log::error(&format!("tunnel connection lost: {reason}"));

        // Reconnect with exponential backoff (1s..30s).
        loop {
            log::info(&format!(
                "reconnecting in {}...",
                format_go_duration(backoff)
            ));
            tokio::time::sleep(backoff).await;

            backoff *= 2;
            if backoff > Duration::from_secs(30) {
                backoff = Duration::from_secs(30);
            }

            match dial(&cfg).await {
                Ok((stream, _url)) => {
                    log::info("reconnected to tunnel server");
                    let (new_sink, new_source) = stream.split();
                    sink = Arc::new(Mutex::new(new_sink));
                    source = new_source;
                    backoff = Duration::from_secs(1);
                    break;
                }
                Err(dial_err) => {
                    let msg = dial_err.to_string();
                    if msg.contains("registration failed:") {
                        log::error(&msg);
                        return;
                    }
                    log::error(&format!("reconnect failed: {msg}"));
                    continue;
                }
            }
        }
    }
}

/// Result of one [`read_messages`] session.
enum ReadOutcome {
    /// The connection closed normally (or the stream ended) — stop the loop.
    Closed,
    /// The peer closed with the given status code.
    CloseCode(u16),
    /// The connection was lost with a transport error described by the message.
    Lost(String),
}

/// Read and dispatch binary frames until the connection drops. Spawns a 20s
/// ping task for the lifetime of the session. Mirrors Go's `readMessages`.
async fn read_messages(
    cfg: &Arc<DialConfig>,
    local_port: u16,
    on_request: Option<Arc<Box<dyn Fn(RequestEvent) + Send + Sync>>>,
    sink: Arc<Mutex<WsSink>>,
    source: &mut WsSource,
) -> ReadOutcome {
    let http_client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ReadOutcome::Lost(e.to_string()),
    };

    // Ping task: every 20s, send a ping. Stops when its handle is dropped at the
    // end of this session. Mirrors Go's ticker goroutine.
    let ping_sink = sink.clone();
    let ping_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(20));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick so the first ping is at +20s.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let mut guard = ping_sink.lock().await;
            if guard.send(Message::Ping(Vec::new())).await.is_err() {
                return;
            }
        }
    });

    let outcome = loop {
        match source.next().await {
            Some(Ok(Message::Binary(frame))) => {
                let (request_id, data) = match proto::decode_frame(&frame) {
                    Ok(v) => v,
                    Err(e) => {
                        log::error(&format!("decoding frame: {e}"));
                        continue;
                    }
                };
                let req = match proto::deserialize_request(&data) {
                    Ok(r) => r,
                    Err(e) => {
                        log::error(&format!("deserializing request: {e}"));
                        continue;
                    }
                };

                let client = http_client.clone();
                let sink = sink.clone();
                let on_request = on_request.clone();
                tokio::spawn(async move {
                    handle_request(client, local_port, on_request, sink, request_id, req).await;
                });
            }
            Some(Ok(Message::Close(frame))) => {
                break match frame {
                    Some(f) => ReadOutcome::CloseCode(u16::from(f.code)),
                    None => ReadOutcome::Closed,
                };
            }
            // Other message types (Text, Ping, Pong, Frame) are ignored —
            // matching Go's `if msgType != MessageBinary { continue }`.
            Some(Ok(_)) => continue,
            Some(Err(e)) => break classify_error(e),
            None => break ReadOutcome::Closed,
        }
    };

    ping_task.abort();
    outcome
}

/// Map a tungstenite error to a [`ReadOutcome`], extracting any close code the
/// way Go's `websocket.CloseStatus(err)` did.
fn classify_error(err: tokio_tungstenite::tungstenite::Error) -> ReadOutcome {
    use tokio_tungstenite::tungstenite::Error as WsErr;
    match err {
        // A clean close with no further data — Go treats `err == nil` paths and
        // explicit close frames; a bare ConnectionClosed has no code, so we stop.
        WsErr::ConnectionClosed | WsErr::AlreadyClosed => ReadOutcome::Closed,
        WsErr::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => ReadOutcome::Lost("connection reset".to_string()),
        other => ReadOutcome::Lost(other.to_string()),
    }
}

/// Forward one decoded request to the local server and write the response back.
/// Mirrors Go's `handleRequest`.
async fn handle_request(
    http_client: reqwest::Client,
    local_port: u16,
    on_request: Option<Arc<Box<dyn Fn(RequestEvent) + Send + Sync>>>,
    sink: Arc<Mutex<WsSink>>,
    request_id: u32,
    req: proto::WireRequest,
) {
    let start = Instant::now();

    let path_for_event = req.uri.split('?').next().unwrap_or(&req.uri).to_string();
    let method_for_event = req.method.clone();

    let local_url = format!("http://localhost:{}{}", local_port, req.uri);

    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            log::error(&format!("creating local request: {e}"));
            return;
        }
    };

    let mut builder = http_client.request(method, &local_url);
    for (name, value) in &req.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !req.body.is_empty() {
        builder = builder.body(req.body.clone());
    }

    // Issue the request to the local server. A transport-level failure yields
    // the 502 "server down" page (mirroring Go's `httpClient.Do` error path).
    let (status, reason, headers, body) = match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let reason = resp.status().canonical_reason().unwrap_or("").to_string();
            let headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect();
            match resp.bytes().await {
                Ok(b) => (status, reason, headers, b.to_vec()),
                Err(e) => {
                    // Reading the upstream body failed; Go logs around the
                    // serialize step and returns without writing a frame.
                    log::error(&format!("serializing response: {e}"));
                    return;
                }
            }
        }
        Err(e) => {
            log::error(&format!("forwarding to localhost:{local_port}: {e}"));
            error_response(local_port, &e.to_string())
        }
    };

    finish_and_send(
        sink,
        request_id,
        status,
        &reason,
        &headers,
        &body,
        on_request,
        method_for_event,
        path_for_event,
        start,
    )
    .await;
}

/// Serialize the response, frame it, write it under the shared write lock, and
/// fire the `on_request` callback. Mirrors the tail of Go's `handleRequest`.
#[allow(clippy::too_many_arguments)]
async fn finish_and_send(
    sink: Arc<Mutex<WsSink>>,
    request_id: u32,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
    body: &[u8],
    on_request: Option<Arc<Box<dyn Fn(RequestEvent) + Send + Sync>>>,
    method: String,
    path: String,
    start: Instant,
) {
    let resp_bytes = proto::serialize_response(status, reason, headers, body);
    let frame_out = proto::encode_frame(request_id, &resp_bytes);

    let write_err = {
        let mut guard = sink.lock().await;
        guard.send(Message::Binary(frame_out)).await.err()
    };

    if let Some(e) = write_err {
        log::error(&format!("writing response frame: {e}"));
        return;
    }

    if let Some(cb) = on_request {
        cb(RequestEvent {
            method,
            path,
            status,
            duration: start.elapsed(),
        });
    }
}

/// Build the 502 "server down" wire response shown when the local server is
/// unreachable. Mirrors Go's `Client.errorResponse`.
fn error_response(port: u16, err: &str) -> (u16, String, Vec<(String, String)>, Vec<u8>) {
    let body = crate::tunnel::pages::render_server_down(port, err);
    let body_bytes = body.into_bytes();
    let headers = vec![
        (
            "Content-Type".to_string(),
            "text/html; charset=utf-8".to_string(),
        ),
        ("X-Lane-Error".to_string(), "connection-failed".to_string()),
        ("Content-Length".to_string(), body_bytes.len().to_string()),
    ];
    (502, "Bad Gateway".to_string(), headers, body_bytes)
}

/// Format a [`Duration`] the way Go's `time.Duration.String()` does, since the
/// registration `ttl` field and reconnect log lines use that representation
/// (e.g. `30m0s`, `1h0m0s`, `1s`, `500ms`).
fn format_go_duration(d: Duration) -> String {
    let nanos = d.as_nanos();
    if nanos == 0 {
        return "0s".to_string();
    }

    // Sub-second durations: ns / µs / ms with a fractional component.
    if nanos < 1_000_000_000 {
        if nanos < 1_000 {
            return format!("{nanos}ns");
        }
        if nanos < 1_000_000 {
            return format!("{}µs", trim_float(nanos as f64 / 1_000.0));
        }
        return format!("{}ms", trim_float(nanos as f64 / 1_000_000.0));
    }

    let total_secs = d.as_secs();
    let frac_nanos = nanos % 1_000_000_000;

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    let mut out = String::new();
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if hours > 0 || minutes > 0 {
        out.push_str(&format!("{minutes}m"));
    }

    // Seconds (with any fractional part).
    if frac_nanos == 0 {
        out.push_str(&format!("{secs}s"));
    } else {
        let secs_f = secs as f64 + frac_nanos as f64 / 1_000_000_000.0;
        out.push_str(&format!("{}s", trim_float(secs_f)));
    }
    out
}

/// Render a float without trailing zeros (mirroring Go's duration formatting,
/// which trims insignificant fractional digits).
fn trim_float(v: f64) -> String {
    let s = format!("{v:.9}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_is_502_with_lane_header() {
        let (status, reason, headers, body) = error_response(3000, "boom");
        assert_eq!(status, 502);
        assert_eq!(reason, "Bad Gateway");

        let has_lane_err = headers
            .iter()
            .any(|(k, v)| k == "X-Lane-Error" && v == "connection-failed");
        assert!(has_lane_err, "missing X-Lane-Error header: {headers:?}");

        let ct = headers
            .iter()
            .find(|(k, _)| k == "Content-Type")
            .map(|(_, v)| v.as_str());
        assert_eq!(ct, Some("text/html; charset=utf-8"));

        // Content-Length matches the rendered body length.
        let cl: usize = headers
            .iter()
            .find(|(k, _)| k == "Content-Length")
            .map(|(_, v)| v.parse().unwrap())
            .expect("Content-Length present");
        assert_eq!(cl, body.len());

        let body_str = String::from_utf8(body).unwrap();
        assert!(body_str.contains("port 3000"));
        assert!(body_str.contains("boom"));
    }

    #[test]
    fn go_duration_formatting() {
        assert_eq!(format_go_duration(Duration::ZERO), "0s");
        assert_eq!(format_go_duration(Duration::from_secs(1)), "1s");
        assert_eq!(format_go_duration(Duration::from_secs(30 * 60)), "30m0s");
        assert_eq!(format_go_duration(Duration::from_secs(3600)), "1h0m0s");
        assert_eq!(
            format_go_duration(Duration::from_secs(2 * 3600 + 90)),
            "2h1m30s"
        );
        assert_eq!(format_go_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_go_duration(Duration::from_millis(1500)), "1.5s");
    }

    #[test]
    fn client_new_seeds_dial_config() {
        let client = Client::new(ClientOptions {
            server_url: "wss://example/tunnel".to_string(),
            token: "tok".to_string(),
            subdomain: "myapp".to_string(),
            local_port: 3000,
            ..Default::default()
        });
        // domain_url is empty until connect populates it.
        assert_eq!(client.domain_url(), "");
        assert_eq!(client.dial_cfg.server_url, "wss://example/tunnel");
        assert_eq!(client.dial_cfg.token, "tok");
    }
}
