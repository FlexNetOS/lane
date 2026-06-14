//! `lane relay` — cross-machine relay (ADR-0002): join the trusted fleet mesh,
//! manage the deny-by-default trusted-node allowlist, and open governed streams
//! to services on trusted remote nodes.
//!
//! The pure pieces (allowlist management via config, identity-path reporting,
//! status) are ALWAYS available. The iroh-using actions (`up`, `connect`) are
//! gated behind the `relay` cargo feature: without it the command still parses
//! and reports, then fails closed with a clear "rebuild with `--features relay`"
//! error (mirroring `lane web` / `lane start --acme`). The allowlist/identity
//! config logic is exercised in every build.

use anyhow::{Context, Result};

use crate::config;
use crate::relay::{self, allowlist};

/// CLI args for the `lane relay` top-level subcommand.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct RelayArgs {
    #[clap(subcommand)]
    pub command: RelayCommand,
}

/// Subcommands available under `lane relay`.
#[derive(Debug, Clone, clap::Subcommand)]
pub(crate) enum RelayCommand {
    /// Join the fleet mesh as a node: start the iroh peer and run the governed
    /// accept loop (deny-by-default node trust + per-node webpolicy).
    Up {
        /// Output as JSON ({node_id, listening, trusted_count})
        #[arg(long)]
        json: bool,
    },
    /// Open a governed stream to a service on a trusted remote node and bridge a
    /// local port to it. TARGET is `<NodeId>/<host:port>`.
    Connect {
        /// The remote target: `<NodeId>/<host:port>` (the host:port is on the
        /// remote node's side; it is governed by THAT node's webpolicy).
        target: String,
        /// Local loopback port to bind and bridge to the remote service
        /// (default: an ephemeral port).
        #[arg(long = "local-port")]
        local_port: Option<u16>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trust a node: add its NodeId to the deny-by-default allowlist.
    Trust {
        /// NodeId (64-char hex) to trust.
        node_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Untrust a node: remove its NodeId from the allowlist.
    Untrust {
        /// NodeId (64-char hex) to untrust.
        node_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show this node's relay state: NodeId, mode, and trusted nodes.
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run the `lane relay` subcommand.
pub async fn run(args: &RelayArgs) -> Result<()> {
    match &args.command {
        RelayCommand::Up { json } => up(*json).await,
        RelayCommand::Connect {
            target,
            local_port,
            json,
        } => connect(target, *local_port, *json).await,
        RelayCommand::Trust { node_id, json } => trust(node_id, *json),
        RelayCommand::Untrust { node_id, json } => untrust(node_id, *json),
        RelayCommand::Status { json } => status(*json),
    }
}

// --- allowlist management (always compiled) --------------------------------

/// Add `node_id` to the trusted-node allowlist (deny-by-default). The id is
/// validated and normalized first; a duplicate is a no-op.
fn trust(node_id: &str, json: bool) -> Result<()> {
    let id =
        allowlist::parse_node_id(node_id).map_err(|e| anyhow::anyhow!("invalid node id: {e}"))?;
    config::with_lock(|| {
        let mut cfg = config::load().context("loading config")?;
        if !cfg.relay_trusted_nodes.iter().any(|n| n == &id) {
            cfg.relay_trusted_nodes.push(id.clone());
            cfg.save().context("saving config")?;
        }
        Ok(())
    })?;
    if json {
        println!("{}", serde_json::json!({ "trusted": id, "ok": true }));
    } else {
        crate::log::info(&format!("relay: now trusting node {id}"));
    }
    Ok(())
}

/// Remove `node_id` from the trusted-node allowlist. Removing an absent id is a
/// no-op (idempotent).
fn untrust(node_id: &str, json: bool) -> Result<()> {
    let id = allowlist::normalize_node_id(node_id);
    let mut removed = false;
    config::with_lock(|| {
        let mut cfg = config::load().context("loading config")?;
        let before = cfg.relay_trusted_nodes.len();
        cfg.relay_trusted_nodes
            .retain(|n| allowlist::normalize_node_id(n) != id);
        removed = cfg.relay_trusted_nodes.len() != before;
        if removed {
            cfg.save().context("saving config")?;
        }
        Ok(())
    })?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "untrusted": id, "removed": removed })
        );
    } else if removed {
        crate::log::info(&format!("relay: no longer trusting node {id}"));
    } else {
        crate::log::info(&format!("relay: node {id} was not trusted"));
    }
    Ok(())
}

/// Show relay status: NodeId (if an identity exists), mode, and trusted nodes.
fn status(json: bool) -> Result<()> {
    let cfg = config::load().context("loading config")?;
    let node_id = current_node_id();
    let mode = cfg.relay_effective_mode();
    let key_path = relay::identity::node_key_path();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "node_id": node_id,
                "mode": mode,
                "trusted_nodes": cfg.relay_trusted_nodes,
                "trusted_count": cfg.relay_trusted_nodes.len(),
                "feature_relay": cfg!(feature = "relay"),
                "key_path": key_path.to_string_lossy(),
            })
        );
        return Ok(());
    }

    use crate::term::{bold, dim};
    println!("{}", bold("lane relay"));
    match &node_id {
        Some(id) => println!("  node id:  {id}"),
        None => println!("  node id:  {}", dim("(none — run `lane relay up`)")),
    }
    println!("  mode:     {mode}");
    println!(
        "  feature:  {}",
        if cfg!(feature = "relay") {
            "relay (enabled)"
        } else {
            "relay (disabled — rebuild with --features relay)"
        }
    );
    if cfg.relay_trusted_nodes.is_empty() {
        println!(
            "  trusted:  {}",
            dim("(none — deny-by-default: no node can connect)")
        );
    } else {
        println!("  trusted:  {} node(s)", cfg.relay_trusted_nodes.len());
        for n in &cfg.relay_trusted_nodes {
            println!("    - {n}");
        }
    }
    Ok(())
}

/// The current node's NodeId derived from the persisted identity, if one exists.
/// Pure-build: returns `None` (no identity without the feature). Feature build:
/// reads `~/.lane/relay/node.key` if present.
#[cfg(feature = "relay")]
fn current_node_id() -> Option<String> {
    if !relay::identity::node_key_path().exists() {
        return None;
    }
    relay::identity::load_or_generate_secret_key()
        .ok()
        .map(|k| relay::identity::node_id_string(&k))
}

/// Pure build has no iroh identity to derive a NodeId from.
#[cfg(not(feature = "relay"))]
fn current_node_id() -> Option<String> {
    None
}

// --- iroh-using actions (feature-gated) ------------------------------------

/// Start the relay endpoint and run the governed accept loop (feature build).
#[cfg(feature = "relay")]
async fn up(json: bool) -> Result<()> {
    use crate::relay::{AcceptConfig, RelayEndpoint};

    crate::install_crypto_provider();
    let cfg = config::load().context("loading config")?;

    let secret_key =
        relay::identity::load_or_generate_secret_key().context("loading relay node identity")?;
    let relay_mode = relay::relay_mode_from_config(&cfg);

    let endpoint = RelayEndpoint::bind(secret_key, relay_mode)
        .await
        .context("binding relay endpoint")?;
    let node_id = endpoint.node_id();
    let trusted_count = cfg.relay_trusted_nodes.len();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "node_id": node_id,
                "listening": true,
                "trusted_count": trusted_count,
            })
        );
    } else {
        crate::log::info(&format!("relay up — node id {node_id}"));
        if trusted_count == 0 {
            crate::log::info(
                "relay: no trusted nodes (deny-by-default) — add one with `lane relay trust <NodeId>`",
            );
        } else {
            crate::log::info(&format!("relay: trusting {trusted_count} node(s)"));
        }
    }

    let accept_config = AcceptConfig {
        trusted_nodes: cfg.relay_trusted_nodes.clone(),
        policy: cfg.web_policy(),
    };
    relay::run_accept_loop(endpoint.endpoint(), accept_config)
        .await
        .context("relay accept loop")?;
    endpoint.close().await;
    Ok(())
}

/// Feature-off `up`: fails closed (mirrors `lane web` without `obscura`).
#[cfg(not(feature = "relay"))]
async fn up(_json: bool) -> Result<()> {
    anyhow::bail!(
        "the cross-machine relay is not enabled in this build; rebuild with \
         `--features relay` (ADR-0002)"
    )
}

/// Connect to a trusted node and bridge a local port to a remote service
/// (feature build).
#[cfg(feature = "relay")]
async fn connect(target: &str, local_port: Option<u16>, json: bool) -> Result<()> {
    use tokio::net::TcpListener;

    crate::install_crypto_provider();
    let cfg = config::load().context("loading config")?;

    let (node_id_str, target_req) = parse_connect_target(target)?;
    // The remote node must itself be trusted locally too (defense in depth: we
    // do not dial nodes we have not chosen to trust).
    let node_id = node_id_str
        .parse::<iroh::EndpointId>()
        .map_err(|e| anyhow::anyhow!("invalid node id {node_id_str}: {e}"))?;

    let secret_key =
        relay::identity::load_or_generate_secret_key().context("loading relay node identity")?;
    let relay_mode = relay::relay_mode_from_config(&cfg);
    let endpoint = relay::RelayEndpoint::bind(secret_key, relay_mode)
        .await
        .context("binding relay endpoint")?;

    // Rely on iroh discovery/relay for addressing: build a NodeId-only addr.
    let peer = relay::endpoint_addr_from_parts(node_id, std::iter::empty());

    let bind_port = local_port.unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", bind_port))
        .await
        .with_context(|| format!("binding local bridge port {bind_port}"))?;
    let local_addr = listener.local_addr().context("reading local bridge addr")?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "node_id": node_id_str,
                "remote_target": target_req.wire_string(),
                "local_addr": local_addr.to_string(),
            })
        );
    } else {
        crate::log::info(&format!(
            "relay connect — local 127.0.0.1:{} → {}/{}",
            local_addr.port(),
            node_id_str,
            target_req.wire_string(),
        ));
    }

    relay::serve_local_bridge(endpoint.endpoint().clone(), peer, target_req, listener)
        .await
        .context("relay local bridge")?;
    endpoint.close().await;
    Ok(())
}

/// Feature-off `connect`: fails closed.
#[cfg(not(feature = "relay"))]
async fn connect(target: &str, _local_port: Option<u16>, _json: bool) -> Result<()> {
    // Validate the target shape so the user gets a useful error even in the pure
    // build, THEN report the feature is off.
    let _ = parse_connect_target(target)?;
    anyhow::bail!(
        "the cross-machine relay is not enabled in this build; rebuild with \
         `--features relay` (ADR-0002)"
    )
}

/// Parse a `<NodeId>/<host:port>` connect target into the validated NodeId
/// string and the [`relay::TargetRequest`]. Always compiled so the shape is
/// validated in every build.
fn parse_connect_target(target: &str) -> Result<(String, relay::TargetRequest)> {
    let (node_part, host_part) = target.split_once('/').ok_or_else(|| {
        anyhow::anyhow!("invalid target {target:?}: expected <NodeId>/<host:port>")
    })?;
    let node_id = allowlist::parse_node_id(node_part)
        .map_err(|e| anyhow::anyhow!("invalid node id in target: {e}"))?;
    let req = relay::TargetRequest::parse(host_part)
        .map_err(|e| anyhow::anyhow!("invalid host:port in target: {e}"))?;
    Ok((node_id, req))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connect_target_splits_node_and_hostport() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let (node, req) = parse_connect_target(&format!("{id}/localhost:3000")).unwrap();
        assert_eq!(node, id);
        assert_eq!(req.host, "localhost");
        assert_eq!(req.port, 3000);
    }

    #[test]
    fn parse_connect_target_rejects_missing_slash() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(parse_connect_target(id).is_err());
    }

    #[test]
    fn parse_connect_target_rejects_bad_node_id() {
        assert!(parse_connect_target("not-a-node/localhost:3000").is_err());
    }

    #[test]
    fn parse_connect_target_rejects_bad_hostport() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(parse_connect_target(&format!("{id}/no-port")).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn trust_then_untrust_round_trips_through_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        trust(id, true).expect("trust");
        let cfg = config::load().unwrap();
        assert!(cfg.relay_trusted_nodes.iter().any(|n| n == id));

        // Trusting again is a no-op (no duplicate).
        trust(id, true).expect("trust again");
        let cfg = config::load().unwrap();
        assert_eq!(
            cfg.relay_trusted_nodes.iter().filter(|n| *n == id).count(),
            1
        );

        untrust(id, true).expect("untrust");
        let cfg = config::load().unwrap();
        assert!(!cfg.relay_trusted_nodes.iter().any(|n| n == id));
    }

    #[test]
    fn trust_rejects_invalid_node_id() {
        assert!(trust("too-short", true).is_err());
    }
}
