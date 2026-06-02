//! `lane list` — show configured domains and active tunnels.
//!
//! Faithful port of `cmd/list.go`. Loads the config, probes upstream health
//! when the daemon is running, fetches active tunnels from the API, and renders
//! either JSON or two borderless tables (domains and tunnels).

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::config;
use crate::daemon;
use crate::proxy;
use crate::system;
use crate::term;

use super::{ingress_ports_reachable, should_reload_port_forwarding};

/// One active tunnel returned by `GET {api}/api/tunnels/active`.
///
/// JSON tags match the Go `activeTunnel` struct exactly.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ActiveTunnel {
    #[serde(default)]
    subdomain: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    has_password: bool,
    #[serde(default)]
    connected_at: String,
    #[serde(default)]
    expires_at: String,
    #[serde(default)]
    request_count: u64,
}

/// The envelope `{ "tunnels": [...] }` returned by the active-tunnels endpoint.
#[derive(Default, Deserialize)]
struct ActiveTunnelsBody {
    #[serde(default)]
    tunnels: Vec<ActiveTunnel>,
}

/// One route entry within a domain, for JSON/table output.
///
/// `healthy` is `Option<bool>` to mirror Go's `*bool` with `omitempty`.
#[derive(Clone, Debug, Default, Serialize)]
struct RouteEntry {
    path: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthy: Option<bool>,
}

/// One domain entry, for JSON/table output.
#[derive(Clone, Debug, Default, Serialize)]
struct DomainEntry {
    domain: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthy: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    routes: Vec<RouteEntry>,
}

/// Fetch the caller's active tunnels. Any error (transport, non-200, or decode)
/// yields an empty list, exactly like Go's `fetchActiveTunnels`.
async fn fetch_active_tunnels(token: &str) -> Vec<ActiveTunnel> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let resp = match client
        .get(format!("{}/api/tunnels/active", config::api_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    if resp.status().as_u16() != 200 {
        return Vec::new();
    }

    match resp.json::<ActiveTunnelsBody>().await {
        Ok(body) => body.tunnels,
        Err(_) => Vec::new(),
    }
}

/// Run `lane list`.
pub async fn run(args: &super::ListArgs) -> Result<()> {
    let cfg = config::load()?;

    let running = daemon::is_running().await;
    let mut ingress_ok = true;
    let mut pf_reload_err: Option<anyhow::Error> = None;
    if running {
        let pf = system::new_port_forwarder();
        if should_reload_port_forwarding(pf.as_ref(), true) {
            if let Err(e) = pf.ensure_loaded() {
                pf_reload_err = Some(e);
            }
        }
        ingress_ok = ingress_ports_reachable();
    }

    let mut domains: Vec<DomainEntry> = Vec::new();
    for d in &cfg.domains {
        let mut entry = DomainEntry {
            domain: d.name.clone(),
            port: d.port,
            healthy: None,
            routes: Vec::new(),
        };
        for r in &d.routes {
            entry.routes.push(RouteEntry {
                path: r.path.clone(),
                port: r.port,
                healthy: None,
            });
        }
        domains.push(entry);
    }

    if running && !domains.is_empty() {
        let mut all_ports: Vec<u16> = Vec::new();
        for d in &cfg.domains {
            all_ports.push(d.port);
            for r in &d.routes {
                all_ports.push(r.port);
            }
        }
        let health = proxy::check_upstreams(&all_ports).await;
        let mut idx = 0;
        for entry in &mut domains {
            entry.healthy = Some(health[idx]);
            idx += 1;
            for r in &mut entry.routes {
                r.healthy = Some(health[idx]);
                idx += 1;
            }
        }
        if !ingress_ok {
            for entry in &mut domains {
                entry.healthy = Some(false);
                for r in &mut entry.routes {
                    r.healthy = Some(false);
                }
            }
        }
    }

    let info = auth::load_auth().ok().flatten();
    let tunnels = match &info {
        Some(info) => fetch_active_tunnels(&info.token).await,
        None => Vec::new(),
    };

    if domains.is_empty() && tunnels.is_empty() {
        println!("No domains or tunnels. Use 'lane start' or 'lane share' to create one.");
        return Ok(());
    }

    if args.json {
        let payload = serde_json::json!({
            "domains": domains,
            "tunnels": tunnels,
        });
        let data = serde_json::to_string_pretty(&payload).context("marshaling JSON")?;
        println!("{data}");
        return Ok(());
    }

    if !domains.is_empty() {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for e in &domains {
            let status = status_cell(running, ingress_ok, e.healthy);
            rows.push(vec![e.domain.clone(), e.port.to_string(), status]);
            for r in &e.routes {
                let r_status = status_cell(running, ingress_ok, r.healthy);
                rows.push(vec![format!("  {}", r.path), r.port.to_string(), r_status]);
            }
        }
        println!(
            "{}",
            term::table::render_table(&["DOMAIN", "PORT", "STATUS"], &rows)
        );
    }

    if let Some(err) = &pf_reload_err {
        println!(
            "\n{} {}",
            term::yellow("Port forwarding reload failed:"),
            err
        );
    }

    if !tunnels.is_empty() {
        if !domains.is_empty() {
            println!();
        }
        let mut rows: Vec<Vec<String>> = Vec::new();
        for t in &tunnels {
            rows.push(vec![
                format!("{}.lane.show", t.subdomain),
                t.url.clone(),
                t.request_count.to_string(),
            ]);
        }
        println!(
            "{}",
            term::table::render_table(&["TUNNEL", "URL", "REQUESTS"], &rows)
        );
    }

    if !domains.is_empty() && !running {
        println!("\nProxy is not running. Use 'lane start' to start it.");
    }

    Ok(())
}

/// Render the colored status cell for a domain or route. Mirrors the inline
/// status logic in `cmd/list.go`:
/// - no health probe -> dim `-`
/// - daemon running but ingress down -> red `● ingress down`
/// - healthy -> green `● reachable`
/// - otherwise -> red `● unreachable`
fn status_cell(running: bool, ingress_ok: bool, healthy: Option<bool>) -> String {
    match healthy {
        None => term::dim("-"),
        Some(_) if running && !ingress_ok => term::red("● ingress down"),
        Some(true) => term::green("● reachable"),
        Some(false) => term::red("● unreachable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_cell_no_health_is_dim_dash() {
        let cell = status_cell(false, true, None);
        assert!(cell.contains('-'), "expected a dash, got {cell:?}");
        assert!(cell.contains('\u{1b}'), "expected styling escape");
    }

    #[test]
    fn status_cell_ingress_down_overrides() {
        // Running daemon + ingress unreachable: even a "healthy" upstream shows
        // ingress down.
        let cell = status_cell(true, false, Some(true));
        assert!(cell.contains("ingress down"), "got {cell:?}");
        assert_eq!(cell, term::red("● ingress down"));
    }

    #[test]
    fn status_cell_reachable_and_unreachable() {
        assert_eq!(
            status_cell(true, true, Some(true)),
            term::green("● reachable")
        );
        assert_eq!(
            status_cell(true, true, Some(false)),
            term::red("● unreachable")
        );
    }

    #[test]
    fn active_tunnel_json_tags() {
        // The JSON tags must match the Go activeTunnel struct exactly.
        let raw = r#"{
            "subdomain": "demo",
            "url": "https://demo.lane.show",
            "has_password": true,
            "connected_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T01:00:00Z",
            "request_count": 42
        }"#;
        let t: ActiveTunnel = serde_json::from_str(raw).expect("parse");
        assert_eq!(t.subdomain, "demo");
        assert_eq!(t.url, "https://demo.lane.show");
        assert!(t.has_password);
        assert_eq!(t.request_count, 42);
    }

    #[test]
    fn active_tunnels_body_extracts_list() {
        let raw = r#"{"tunnels":[{"subdomain":"a"},{"subdomain":"b"}]}"#;
        let body: ActiveTunnelsBody = serde_json::from_str(raw).expect("parse");
        assert_eq!(body.tunnels.len(), 2);
        assert_eq!(body.tunnels[0].subdomain, "a");
    }

    #[test]
    fn domain_entry_omits_empty_healthy_and_routes() {
        let entry = DomainEntry {
            domain: "myapp.test".into(),
            port: 3000,
            healthy: None,
            routes: Vec::new(),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"domain\":\"myapp.test\""));
        assert!(json.contains("\"port\":3000"));
        assert!(!json.contains("healthy"), "healthy omitted when None");
        assert!(!json.contains("routes"), "routes omitted when empty");
    }

    #[test]
    fn domain_entry_includes_healthy_when_present() {
        let entry = DomainEntry {
            domain: "myapp.test".into(),
            port: 3000,
            healthy: Some(true),
            routes: vec![RouteEntry {
                path: "/api".into(),
                port: 8080,
                healthy: Some(false),
            }],
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"healthy\":true"));
        assert!(json.contains("\"path\":\"/api\""));
        assert!(json.contains("\"healthy\":false"));
    }
}
