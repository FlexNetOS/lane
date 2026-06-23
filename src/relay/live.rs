//! Live iroh transport for the relay (compiled only with `--features relay`).
//!
//! Three pieces, all governed:
//! - [`RelayEndpoint`] — binds an iroh [`Endpoint`](iroh::Endpoint) keyed by the
//!   node's persistent identity, advertising the [`RELAY_ALPN`].
//! - [`run_accept_loop`] — the *governance-across-the-link* heart: it accepts
//!   inbound connections, **rejects any whose remote NodeId is not on the
//!   deny-by-default trusted-node allowlist**, and for a trusted connection runs
//!   the **same** [`crate::webpolicy`] deny-by-default check + access-log lane runs
//!   for local traffic before bridging the stream to a local `TcpStream`.
//! - [`connect_and_bridge`] — the connect side: dial a trusted node, send the
//!   target request frame, and bridge a local listener to the governed remote
//!   stream.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, TransportAddr};
use tokio::net::{TcpListener, TcpStream};

use crate::webpolicy::{PolicyDecision, Scheme, WebPolicy};

use super::{encode_denied, TargetRequest, RELAY_ALPN, RESP_OK};

/// A bound relay endpoint and the identity it speaks as. Hold this for the life
/// of `lane relay up`; dropping it (via [`Endpoint::close`]) tears the peer down.
pub struct RelayEndpoint {
    endpoint: Endpoint,
}

impl RelayEndpoint {
    /// Bind a relay endpoint with `secret_key` as the node identity, accepting
    /// the relay ALPN. `relay_mode` selects iroh's relay/NAT-traversal behavior
    /// ([`RelayMode::Default`] for the real fleet; [`RelayMode::Disabled`] for
    /// hermetic, direct-only use such as tests).
    pub async fn bind(secret_key: SecretKey, relay_mode: RelayMode) -> Result<RelayEndpoint> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![RELAY_ALPN.to_vec()])
            .relay_mode(relay_mode)
            .bind()
            .await
            .map_err(|e| anyhow::anyhow!("binding relay endpoint: {e}"))?;
        Ok(RelayEndpoint { endpoint })
    }

    /// Bind a relay endpoint with a **fresh, ephemeral, in-memory** identity (a
    /// freshly generated [`SecretKey`] that is never persisted). For hermetic
    /// two-endpoint tests that just need two distinct nodes on `RelayMode::Disabled`
    /// — and lets a downstream crate spin up a throwaway endpoint **without naming
    /// any iroh type** (no `SecretKey` in scope), keeping iroh transitive-only
    /// behind this boundary.
    pub async fn bind_ephemeral(relay_mode: RelayMode) -> Result<RelayEndpoint> {
        RelayEndpoint::bind(SecretKey::generate(), relay_mode).await
    }

    /// This node's stable NodeId (its fleet identity), as 64-char lowercase hex.
    pub fn node_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// The endpoint's current [`EndpointAddr`] (NodeId + direct addrs + relay
    /// url). With a relay disabled this still carries the bound direct addresses,
    /// which is what hermetic direct addressing uses.
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// This endpoint's [`EndpointAddr`] restricted to its bound **direct** IP
    /// addresses (NodeId + direct socket addrs only, no relay url). This is the
    /// hermetic, discovery-free address a second in-process endpoint dials in
    /// `RelayMode::Disabled` tests — and lets a downstream crate (e.g. netengine)
    /// build a loopback peer address **without naming any iroh type**, so iroh can
    /// stay a transitive-only dependency behind this boundary.
    pub fn direct_endpoint_addr(&self) -> EndpointAddr {
        let full = self.endpoint.addr();
        let id = self.endpoint.id();
        let addrs: Vec<SocketAddr> = full.ip_addrs().copied().collect();
        endpoint_addr_from_parts(id, addrs)
    }

    /// The underlying iroh endpoint (for the accept loop / shutdown).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Close the endpoint, flushing queued close frames.
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

/// Configuration for the governed accept loop: the deny-by-default trusted-node
/// allowlist and the per-node [`WebPolicy`] applied to every relayed target.
#[derive(Clone)]
pub struct AcceptConfig {
    /// Trusted-node allowlist (NodeIds). Empty ⇒ trust nothing (deny-by-default).
    pub trusted_nodes: Vec<String>,
    /// This node's deny-by-default web policy — identical contract to local
    /// governance (built from THIS node's allow-list config).
    pub policy: WebPolicy,
}

/// Run the governed accept loop until the endpoint is closed.
///
/// For each incoming connection: reject (deny-by-default) any remote NodeId not
/// on `config.trusted_nodes`; for a trusted one, read the target request frame,
/// run `config.policy` (the SAME webpolicy as local traffic) on the target,
/// log via [`crate::log`], and only on ALLOW connect to the local service and
/// bridge bytes. DENY sends an error frame and never connects upstream.
pub async fn run_accept_loop(endpoint: &Endpoint, config: AcceptConfig) -> Result<()> {
    let config = Arc::new(config);
    while let Some(incoming) = endpoint.accept().await {
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            // One bad peer must never stop the loop; failures are logged inside.
            let _ = handle_incoming(incoming, config).await;
        });
    }
    Ok(())
}

/// Handle a single inbound connection end to end.
async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    config: Arc<AcceptConfig>,
) -> Result<()> {
    let conn = incoming
        .await
        .map_err(|e| anyhow::anyhow!("accepting relay connection: {e}"))?;
    let remote = conn.remote_id().to_string();

    // GOVERNANCE-ACROSS-THE-LINK, layer 1: deny-by-default node trust.
    if !super::is_trusted(&config.trusted_nodes, &remote) {
        crate::log::info(&format!(
            "relay DENY connection from untrusted node {remote}"
        ));
        conn.close(0u32.into(), b"untrusted node");
        return Ok(());
    }

    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|e| anyhow::anyhow!("accepting bi-stream: {e}"))?;

    // Read the request frame: 2-byte BE length + UTF-8 host:port.
    let target = match read_target_request(&mut recv).await {
        Ok(t) => t,
        Err(e) => {
            crate::log::info(&format!("relay DENY {remote} — malformed request: {e}"));
            deny_and_close(&conn, &mut send, &format!("malformed request: {e}")).await;
            return Ok(());
        }
    };

    // GOVERNANCE-ACROSS-THE-LINK, layer 2: the SAME deny-by-default webpolicy as
    // local traffic, applied at the destination node. host/port granularity
    // (Scheme::Https is the conservative choice; webpolicy ignores it on the
    // decomposed check_addr path).
    let decision = config
        .policy
        .check_addr(&target.host, target.port, Scheme::Https);
    if let PolicyDecision::Deny(reason) = decision {
        crate::log::info(&format!(
            "relay DENY {remote} {}:{} ({reason})",
            target.host, target.port
        ));
        deny_and_close(&conn, &mut send, &reason.to_string()).await;
        return Ok(());
    }

    // ALLOW: connect to the local service. A failed connect is an upstream error,
    // not a policy denial.
    let upstream = match TcpStream::connect((target.host.as_str(), target.port)).await {
        Ok(s) => s,
        Err(e) => {
            crate::log::info(&format!(
                "relay ALLOW {remote} {}:{} — upstream unreachable: {e}",
                target.host, target.port
            ));
            deny_and_close(&conn, &mut send, &format!("upstream unreachable: {e}")).await;
            return Ok(());
        }
    };

    crate::log::info(&format!(
        "relay ALLOW {remote} {}:{}",
        target.host, target.port
    ));

    // Ack OK, then splice the iroh stream and the local TCP stream.
    send.write_all(&[RESP_OK])
        .await
        .context("writing relay ok ack")?;

    let mut upstream = upstream;
    let mut iroh_stream = tokio::io::join(recv, send);
    let _ = tokio::io::copy_bidirectional(&mut iroh_stream, &mut upstream).await;
    Ok(())
}

/// Write the deny frame, finish the send side, and drain the connection so the
/// frame reliably reaches the peer before teardown. QUIC `finish()` only *queues*
/// the FIN; if we returned immediately and dropped `conn` the peer could observe
/// "connection lost" before reading the deny bytes (which is what made the
/// connect side fail instead of seeing the denial). We wait (bounded) for the
/// peer to read + close, then close the connection ourselves.
async fn deny_and_close(
    conn: &iroh::endpoint::Connection,
    send: &mut iroh::endpoint::SendStream,
    reason: &str,
) {
    let _ = send.write_all(&encode_denied(reason)).await;
    let _ = send.finish();
    // Give the peer a bounded window to read the deny frame and close.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), conn.closed()).await;
    conn.close(0u32.into(), b"denied");
}

/// Read and decode the request frame (2-byte BE length + UTF-8 host:port) from
/// the recv stream.
async fn read_target_request(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<TargetRequest, String> {
    let mut len_buf = [0u8; 2];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("reading length: {e}"))?;
    let len = u16::from_be_bytes(len_buf) as usize;
    if len == 0 || len > super::MAX_TARGET_LEN {
        return Err(format!("invalid target length {len}"));
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| format!("reading target: {e}"))?;
    let s = std::str::from_utf8(&payload).map_err(|e| format!("non-utf8 target: {e}"))?;
    TargetRequest::parse(s)
}

/// Connect to a trusted node and bridge a single local connection to a service on
/// it, governed at the destination. This is the building block for both a
/// one-shot stdio bridge and a local-listener bridge.
///
/// Opens a bi-stream, sends the [`TargetRequest`] frame, reads the 1-byte
/// response status: on [`RESP_OK`] it bridges `local` ⇄ the remote stream; on a
/// denial it returns the carried reason as an error (never bridges).
pub async fn connect_and_bridge(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    target: &TargetRequest,
    mut local: TcpStream,
) -> Result<()> {
    let conn = endpoint
        .connect(peer, RELAY_ALPN)
        .await
        .map_err(|e| anyhow::anyhow!("connecting to relay node: {e}"))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("opening relay stream: {e}"))?;

    // Send the target request frame.
    send.write_all(&target.encode())
        .await
        .context("sending relay target request")?;

    // Read the 1-byte response status.
    let mut status = [0u8; 1];
    recv.read_exact(&mut status)
        .await
        .map_err(|e| anyhow::anyhow!("reading relay response: {e}"))?;
    if status[0] != RESP_OK {
        let reason = read_denied_reason(&mut recv).await;
        anyhow::bail!("relay denied by remote node: {reason}");
    }

    // ALLOW: splice the local TCP stream and the iroh stream.
    let mut iroh_stream = tokio::io::join(recv, send);
    let _ = tokio::io::copy_bidirectional(&mut iroh_stream, &mut local).await;
    Ok(())
}

/// Read the denial reason after a non-OK status byte (2-byte BE length + UTF-8).
async fn read_denied_reason(recv: &mut iroh::endpoint::RecvStream) -> String {
    let mut len_buf = [0u8; 2];
    if recv.read_exact(&mut len_buf).await.is_err() {
        return "(no reason)".to_string();
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len == 0 || len > super::MAX_TARGET_LEN {
        return "(no reason)".to_string();
    }
    let mut buf = vec![0u8; len];
    if recv.read_exact(&mut buf).await.is_err() {
        return "(truncated reason)".to_string();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Bridge a local TCP listener to a service on a trusted remote node: for each
/// local accept, open a governed relay stream to `peer`'s `target` and splice.
/// Returns when the listener errors irrecoverably. `local_addr` is the bound
/// loopback address the user connects to.
pub async fn serve_local_bridge(
    endpoint: Endpoint,
    peer: EndpointAddr,
    target: TargetRequest,
    listener: TcpListener,
) -> Result<()> {
    loop {
        let (local, _peer) = listener
            .accept()
            .await
            .context("accepting local bridge connection")?;
        let endpoint = endpoint.clone();
        let peer = peer.clone();
        let target = target.clone();
        tokio::spawn(async move {
            if let Err(e) = connect_and_bridge(&endpoint, peer, &target, local).await {
                crate::log::error(&format!("relay bridge: {e}"));
            }
        });
    }
}

/// Sentinel value in `relay_servers` that explicitly disables DERP/relaying:
/// `relay_servers: ["disabled"]` ⇒ [`RelayMode::Disabled`] (direct-only). Any
/// other non-empty list is treated as custom self-hosted relay URLs.
pub const RELAY_SERVERS_DISABLED: &str = "disabled";

/// Map lane's config to iroh's DERP [`RelayMode`] (the relay-server / NAT-traversal
/// setting — **distinct** from lane's node-role `relay_mode` (`peer`|`relay`),
/// which is handled separately by [`Config::relay_effective_mode`] and does not
/// affect the DERP `RelayMode` chosen here).
///
/// The choice is driven entirely by [`Config::relay_servers`]:
/// - **empty** ⇒ [`RelayMode::Default`] — n0's public relays; iroh's
///   hole-punching + DERP fallback work out of the box with no configuration.
/// - **`["disabled"]`** ([`RELAY_SERVERS_DISABLED`]) ⇒ [`RelayMode::Disabled`] —
///   direct-only, no relaying (advanced/direct-reachable deployments).
/// - **non-empty URLs** ⇒ [`RelayMode::Custom`] pinning your own self-hosted DERP
///   relays. Each entry is parsed as a [`RelayUrl`]; invalid entries are logged
///   and skipped. If **every** entry is invalid the function **falls back to
///   [`RelayMode::Default`]** with a warning — relay connectivity is an
///   *availability* concern (NAT traversal), not a security boundary, so it is
///   fail-safe here, not fail-closed. Security stays deny-by-default in the
///   trusted-node allowlist and the webpolicy, which are unaffected by this.
///
/// To pin self-hosted relays, set `relay_servers` in `.lane.yaml`:
/// ```yaml
/// relay_servers:
///   - https://derp.example.test
/// ```
pub fn relay_mode_from_config(cfg: &crate::config::Config) -> RelayMode {
    if cfg.relay_servers.is_empty() {
        return RelayMode::Default;
    }

    // Explicit opt-out: a single "disabled" sentinel turns relaying off entirely.
    if cfg.relay_servers.len() == 1
        && cfg.relay_servers[0]
            .trim()
            .eq_ignore_ascii_case(RELAY_SERVERS_DISABLED)
    {
        return RelayMode::Disabled;
    }

    let mut urls: Vec<RelayUrl> = Vec::with_capacity(cfg.relay_servers.len());
    for entry in &cfg.relay_servers {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.parse::<RelayUrl>() {
            Ok(url) => urls.push(url),
            Err(e) => crate::log::error(&format!(
                "relay: ignoring invalid relay_servers entry {entry:?}: {e}"
            )),
        }
    }

    if urls.is_empty() {
        // Fail-safe (availability, not security): no usable custom relay ⇒ fall
        // back to the public default relays rather than disabling NAT traversal.
        crate::log::error(
            "relay: all relay_servers entries were invalid — falling back to default relays",
        );
        return RelayMode::Default;
    }

    RelayMode::custom(urls)
}

/// Build an [`EndpointAddr`] for a NodeId plus optional direct socket addresses.
/// Used by the connect side: the NodeId is required; direct addrs (when known,
/// e.g. hermetic tests) let iroh skip discovery. With no direct addrs iroh relies
/// on discovery/relay to locate the peer.
pub fn endpoint_addr_from_parts(
    node_id: iroh::EndpointId,
    direct: impl IntoIterator<Item = SocketAddr>,
) -> EndpointAddr {
    EndpointAddr::from_parts(node_id, direct.into_iter().map(TransportAddr::Ip))
}

/// Parse a 64-char hex NodeId string into an [`EndpointAddr`] with **no** direct
/// addresses — iroh locates the peer via discovery/relay. This is the connect-side
/// entry point for "dial a peer by its NodeId alone" (the `relay ping <node-id>`
/// shape), and lets a downstream crate build a dialable peer address from a string
/// **without naming any iroh type**, keeping iroh transitive-only behind this
/// boundary.
///
/// Returns an error string for any input that is not a valid NodeId (the same
/// validation [`parse_node_id`](super::parse_node_id) performs, then the iroh
/// public-key decode).
pub fn endpoint_addr_from_node_id(node_id: &str) -> Result<EndpointAddr, String> {
    // Validate shape (64 lowercase hex) via the always-compiled allowlist parser,
    // then decode to the iroh public key (EndpointId).
    let normalized = super::parse_node_id(node_id)?;
    let id: iroh::EndpointId = normalized
        .parse()
        .map_err(|e| format!("invalid node id {normalized:?}: {e}"))?;
    Ok(EndpointAddr::new(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webpolicy::WebPolicy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Bind a hermetic, relay-disabled endpoint on loopback with a fresh
    /// identity. Returns the endpoint wrapper.
    async fn hermetic_node() -> RelayEndpoint {
        crate::install_crypto_provider();
        RelayEndpoint::bind(SecretKey::generate(), RelayMode::Disabled)
            .await
            .expect("bind hermetic endpoint")
    }

    /// The direct EndpointAddr for a node, restricted to loopback addrs so the
    /// two in-process endpoints find each other without any relay/discovery.
    fn direct_addr(node: &RelayEndpoint) -> EndpointAddr {
        let full = node.endpoint_addr();
        let id = node.endpoint().id();
        let addrs: Vec<SocketAddr> = full.ip_addrs().copied().collect();
        endpoint_addr_from_parts(id, addrs)
    }

    /// Spawn a local TCP echo server; return its loopback addr.
    async fn spawn_echo() -> SocketAddr {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if sock.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        addr
    }

    /// REQUIRED PROOF 1: two in-process iroh endpoints find each other
    /// HERMETICALLY (RelayMode::Disabled + direct addrs) and round-trip bytes
    /// through a governed bridge to a local service.
    #[tokio::test]
    async fn two_node_reachability_round_trips_bytes() {
        let node_a = hermetic_node().await;
        let node_b = hermetic_node().await;

        // A local echo service on node B's machine (this process).
        let echo = spawn_echo().await;

        // B trusts A and allows the echo target (loopback, IP guard off for the
        // test — the SSRF guard is proven in the webpolicy suite).
        let mut policy = WebPolicy::default().allow_host("127.0.0.1");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(echo.port());
        let config = AcceptConfig {
            trusted_nodes: vec![node_a.node_id()],
            policy,
        };

        let b_endpoint = node_b.endpoint().clone();
        let accept = tokio::spawn(async move {
            let _ = run_accept_loop(&b_endpoint, config).await;
        });

        // A connects to B and bridges a local pair of pipes via a TcpStream.
        // Use a loopback socketpair: a listener A writes into, bridged to B.
        let local_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_addr = local_listener.local_addr().unwrap();

        let a_endpoint = node_a.endpoint().clone();
        let peer = direct_addr(&node_b);
        let target = TargetRequest::new("127.0.0.1", echo.port());
        let bridge = tokio::spawn(async move {
            let (local, _) = local_listener.accept().await.unwrap();
            connect_and_bridge(&a_endpoint, peer, &target, local).await
        });

        // The user side: connect to A's local listener, send, expect the echo.
        let mut client = TcpStream::connect(local_addr).await.unwrap();
        client.write_all(b"PING-OVER-RELAY").await.unwrap();
        let mut buf = [0u8; 15];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"PING-OVER-RELAY");

        drop(client);
        let _ = bridge.await.unwrap();
        node_a.close().await;
        node_b.close().await;
        accept.abort();
    }

    /// REQUIRED PROOF 2a: an ALLOWED target bridges; a DENIED target is refused
    /// with an error frame and no upstream connect.
    #[tokio::test]
    async fn governance_denies_target_not_in_policy() {
        let node_a = hermetic_node().await;
        let node_b = hermetic_node().await;

        // A canary "upstream" that must NEVER be connected to on a denial.
        let canary = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let canary_addr = canary.local_addr().unwrap();
        let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accepted2 = Arc::clone(&accepted);
        tokio::spawn(async move {
            if canary.accept().await.is_ok() {
                accepted2.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        // B trusts A but its policy allows NOTHING (deny-by-default web policy).
        let config = AcceptConfig {
            trusted_nodes: vec![node_a.node_id()],
            policy: WebPolicy::default(),
        };
        let b_endpoint = node_b.endpoint().clone();
        let accept = tokio::spawn(async move {
            let _ = run_accept_loop(&b_endpoint, config).await;
        });

        let local_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_addr = local_listener.local_addr().unwrap();
        let a_endpoint = node_a.endpoint().clone();
        let peer = direct_addr(&node_b);
        let target = TargetRequest::new("127.0.0.1", canary_addr.port());
        let bridge = tokio::spawn(async move {
            let (local, _) = local_listener.accept().await.unwrap();
            connect_and_bridge(&a_endpoint, peer, &target, local).await
        });

        let _client = TcpStream::connect(local_addr).await.unwrap();
        // The bridge must error (denied by remote), and the canary must never be
        // connected to.
        let res = bridge.await.unwrap();
        assert!(res.is_err(), "denied target must bridge-error");
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("relay denied by remote node"), "{msg}");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !accepted.load(std::sync::atomic::Ordering::SeqCst),
            "denied target must NOT reach the upstream"
        );
        node_a.close().await;
        node_b.close().await;
        accept.abort();
    }

    /// REQUIRED PROOF 2b: an UNTRUSTED node's connection is rejected
    /// (deny-by-default node trust), even if the target would be allowed.
    #[tokio::test]
    async fn governance_rejects_untrusted_node() {
        let node_a = hermetic_node().await;
        let node_b = hermetic_node().await;
        let echo = spawn_echo().await;

        // B's policy WOULD allow the echo, but B does NOT trust A (empty list).
        let mut policy = WebPolicy::default().allow_host("127.0.0.1");
        policy.guard_ip_literals = false;
        policy = policy.allow_port(echo.port());
        let config = AcceptConfig {
            trusted_nodes: Vec::new(), // trust nothing
            policy,
        };
        let b_endpoint = node_b.endpoint().clone();
        let accept = tokio::spawn(async move {
            let _ = run_accept_loop(&b_endpoint, config).await;
        });

        let local_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_addr = local_listener.local_addr().unwrap();
        let a_endpoint = node_a.endpoint().clone();
        let peer = direct_addr(&node_b);
        let target = TargetRequest::new("127.0.0.1", echo.port());
        let bridge = tokio::spawn(async move {
            let (local, _) = local_listener.accept().await.unwrap();
            connect_and_bridge(&a_endpoint, peer, &target, local).await
        });

        let _client = TcpStream::connect(local_addr).await.unwrap();
        // B closes the connection for an untrusted node; the connect/bridge fails.
        let res = bridge.await.unwrap();
        assert!(
            res.is_err(),
            "an untrusted node must not be able to bridge a stream"
        );
        node_a.close().await;
        node_b.close().await;
        accept.abort();
    }

    /// Build a `Config` with the given `relay_servers` list (all else default).
    fn cfg_with_relays(servers: &[&str]) -> crate::config::Config {
        crate::config::Config {
            relay_servers: servers.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// Empty `relay_servers` ⇒ public default relays. This is the no-config path:
    /// NAT traversal works out of the box.
    #[test]
    fn relay_mode_empty_is_default() {
        let cfg = cfg_with_relays(&[]);
        assert!(matches!(relay_mode_from_config(&cfg), RelayMode::Default));
    }

    /// One valid URL ⇒ Custom relay map pinning exactly that self-hosted relay.
    #[test]
    fn relay_mode_one_url_is_custom() {
        let cfg = cfg_with_relays(&["https://derp.example.test"]);
        match relay_mode_from_config(&cfg) {
            RelayMode::Custom(map) => assert_eq!(map.len(), 1, "exactly one relay pinned"),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    /// Multiple valid URLs ⇒ Custom relay map carrying all of them.
    #[test]
    fn relay_mode_multiple_urls_is_custom() {
        let cfg = cfg_with_relays(&[
            "https://derp1.example.test",
            "https://derp2.example.test",
            "https://derp3.example.test",
        ]);
        match relay_mode_from_config(&cfg) {
            RelayMode::Custom(map) => assert_eq!(map.len(), 3, "all three relays pinned"),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    /// An invalid entry among valid ones is skipped (fail-safe); the valid relays
    /// still form a Custom map.
    #[test]
    fn relay_mode_skips_invalid_among_valid() {
        let cfg = cfg_with_relays(&[
            "https://derp1.example.test",
            "not a url",
            "https://derp2.example.test",
        ]);
        match relay_mode_from_config(&cfg) {
            RelayMode::Custom(map) => assert_eq!(map.len(), 2, "only the two valid relays survive"),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    /// ALL entries invalid ⇒ fail-safe fallback to Default (not Disabled): relay
    /// connectivity is availability, not security.
    #[test]
    fn relay_mode_all_invalid_falls_back_to_default() {
        let cfg = cfg_with_relays(&["not a url", "also bad", ""]);
        assert!(matches!(relay_mode_from_config(&cfg), RelayMode::Default));
    }

    /// `direct_endpoint_addr` returns the same NodeId + loopback direct addrs the
    /// test helper builds by hand — proving a downstream crate can dial a hermetic
    /// peer without naming any iroh type.
    #[tokio::test]
    async fn direct_endpoint_addr_matches_manual_direct_addr() {
        let node = hermetic_node().await;
        let via_method = node.direct_endpoint_addr();
        let via_helper = direct_addr(&node);
        assert_eq!(via_method.id, via_helper.id, "same NodeId");
        let m: Vec<SocketAddr> = via_method.ip_addrs().copied().collect();
        let h: Vec<SocketAddr> = via_helper.ip_addrs().copied().collect();
        assert_eq!(m, h, "same direct ip addrs");
        node.close().await;
    }

    /// `endpoint_addr_from_node_id` parses a node's own id string back into an
    /// EndpointAddr carrying that exact NodeId (discovery-based: no direct addrs).
    #[tokio::test]
    async fn endpoint_addr_from_node_id_round_trips_the_id() {
        let node = hermetic_node().await;
        let id_str = node.node_id();
        let addr = endpoint_addr_from_node_id(&id_str).expect("parse node id");
        assert_eq!(addr.id.to_string(), id_str, "NodeId round-trips");
        assert_eq!(
            addr.ip_addrs().count(),
            0,
            "discovery-based addr carries no direct ips"
        );
        node.close().await;
    }

    /// `endpoint_addr_from_node_id` rejects malformed ids (deny-by-default shape
    /// validation before the iroh decode).
    #[test]
    fn endpoint_addr_from_node_id_rejects_malformed() {
        assert!(endpoint_addr_from_node_id("").is_err());
        assert!(endpoint_addr_from_node_id("abc123").is_err());
        assert!(endpoint_addr_from_node_id(&"z".repeat(64)).is_err());
    }

    /// The explicit `["disabled"]` sentinel turns relaying off (direct-only).
    #[test]
    fn relay_mode_disabled_sentinel() {
        let cfg = cfg_with_relays(&["disabled"]);
        assert!(matches!(relay_mode_from_config(&cfg), RelayMode::Disabled));
        // case-insensitive
        let cfg = cfg_with_relays(&["DISABLED"]);
        assert!(matches!(relay_mode_from_config(&cfg), RelayMode::Disabled));
    }
}
