//! `lane start` (⇐ `cmd/start.go`).
//!
//! Map a local domain to a port and start proxying, running first-time setup
//! automatically if needed. Faithful port of Go's `startCmd.RunE`.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::{self, Domain};
use crate::daemon::{self, MessageType, Request};
use crate::{cert, proxy, setup, system, term};

/// Run `lane start`. Mirrors Go's `startCmd.RunE`.
pub async fn run(args: &super::StartArgs) -> Result<()> {
    let name = super::normalize_name(&args.name);

    config::validate_domain(&name, args.port as i64)?;

    if name.ends_with(".local") {
        eprintln!(
            "{} .local is reserved for mDNS and may cause slow DNS resolution on macOS/Linux",
            term::yellow("Warning:")
        );
    }

    // `--timeout` requires `--wait`; with `--wait`, the (defaulted) timeout must
    // be positive. `args.timeout.is_some()` mirrors Go's `Flags().Changed("timeout")`.
    let timeout = validate_start_wait_flags(args.timeout.is_some(), args.wait, args.timeout)?;

    if let Some(mode) = &args.log_mode {
        config::validate_log_mode(mode)?;
    }

    let routes = super::parse_route_flags(&args.routes)?;

    setup::ensure_first_run()?;

    let name_for_lock = name.clone();
    let routes_for_lock = routes.clone();
    config::with_lock(|| {
        let mut cfg = config::load()?;
        if args.cors {
            cfg.cors = true;
        }
        if let Some(mode) = &args.log_mode {
            cfg.log_mode = mode.trim().to_lowercase();
        }
        cfg.set_domain(&name_for_lock, args.port, routes_for_lock.clone())
    })?;

    system::add_host(&name).context("updating /etc/hosts")?;

    cert::ensure_leaf_cert(&name).context("generating certificate")?;

    if !daemon::is_child() {
        let pf = system::new_port_forwarder();
        if super::should_reload_port_forwarding(&*pf, daemon::is_running().await) {
            pf.ensure_loaded()
                .context("loading port forwarding rules")?;
        }
    }

    if !daemon::is_running().await {
        setup::ensure_proxy_ports_available()?;
        daemon::run_detached().context("starting daemon")?;
        daemon::wait_for_daemon().await?;
    } else {
        daemon::send_ipc(Request {
            msg_type: MessageType::Reload,
            data: None,
        })
        .await
        .context("reloading daemon")?;
    }

    if !daemon::is_child() {
        let pf = system::new_port_forwarder();
        if super::should_reload_port_forwarding(&*pf, true) {
            pf.ensure_loaded()
                .context("loading port forwarding rules")?;
        }
    }

    if args.wait {
        let mut wait_ports = vec![args.port];
        for r in &routes {
            wait_ports.push(r.port);
        }
        for p in wait_ports {
            // In --json mode the wait progress goes to stderr so stdout stays
            // pure JSON; the human path keeps printing to stdout as before.
            if args.json {
                eprint!(
                    "Waiting for localhost:{p} (timeout {})... ",
                    humantime::format_duration(timeout)
                );
            } else {
                print!(
                    "Waiting for localhost:{p} (timeout {})... ",
                    humantime::format_duration(timeout)
                );
                // Flush so the in-progress line shows before the upstream probe.
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            if let Err(e) = proxy::wait_for_upstream(p, timeout).await {
                if args.json {
                    eprintln!("timed out");
                } else {
                    println!("timed out");
                }
                return Err(e);
            }
            if args.json {
                eprintln!("ready");
            } else {
                println!("ready");
            }
        }
    }

    let domain = Domain {
        name,
        port: args.port,
        routes,
    };
    if args.json {
        let payload = start_json_payload(&domain);
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("marshaling JSON")?
        );
    } else {
        super::print_services(std::slice::from_ref(&domain));
    }
    Ok(())
}

/// Build the `lane start --json` payload: the mapped domain, its port, the
/// resulting `https://<domain>` URL, and any path routes (omitted when empty).
fn start_json_payload(domain: &Domain) -> serde_json::Value {
    let mut v = serde_json::json!({
        "domain": domain.name,
        "port": domain.port,
        "url": format!("https://{}", domain.name),
    });
    if !domain.routes.is_empty() {
        v["routes"] = serde_json::json!(domain.routes);
    }
    v
}

/// Validate the `--wait`/`--timeout` flag combination and resolve the effective
/// timeout. Mirrors Go's `validateStartWaitFlags` (which took the already-defaulted
/// timeout); here we also apply the 30s default when unset.
fn validate_start_wait_flags(
    timeout_changed: bool,
    wait: bool,
    timeout: Option<Duration>,
) -> Result<Duration> {
    if timeout_changed && !wait {
        return Err(anyhow::anyhow!("--timeout requires --wait"));
    }
    let timeout = timeout.unwrap_or(Duration::from_secs(30));
    if wait && timeout.is_zero() {
        return Err(anyhow::anyhow!("--timeout must be greater than 0"));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `lane start --json` payload exposes the mapped URL for scripting; routes
    // are nested when present and omitted when empty.
    #[test]
    fn start_json_payload_exposes_url_and_routes() {
        use crate::config::Route;

        let bare = start_json_payload(&Domain {
            name: "myapp.test".into(),
            port: 3000,
            routes: vec![],
        });
        assert_eq!(bare["domain"], "myapp.test");
        assert_eq!(bare["port"], 3000);
        assert_eq!(bare["url"], "https://myapp.test");
        assert!(bare.get("routes").is_none(), "routes omitted when empty");

        let routed = start_json_payload(&Domain {
            name: "api.test".into(),
            port: 8080,
            routes: vec![Route {
                path: "/v1".into(),
                port: 9000,
            }],
        });
        assert_eq!(routed["url"], "https://api.test");
        assert_eq!(routed["routes"][0]["path"], "/v1");
        assert_eq!(routed["routes"][0]["port"], 9000);
    }

    // Port of TestValidateStartWaitFlags.
    #[test]
    fn validate_start_wait_flags_cases() {
        // timeout without wait -> error.
        let err = validate_start_wait_flags(true, false, Some(Duration::from_secs(30)))
            .expect_err("expected error for --timeout without --wait");
        assert!(
            err.to_string().contains("--timeout requires --wait"),
            "unexpected error: {err}"
        );

        // wait with non-positive timeout -> error.
        let err = validate_start_wait_flags(false, true, Some(Duration::from_secs(0)))
            .expect_err("expected error for --wait with zero timeout");
        assert!(
            err.to_string().contains("--timeout must be greater than 0"),
            "unexpected error: {err}"
        );

        // valid wait flags -> ok.
        assert!(validate_start_wait_flags(true, true, Some(Duration::from_secs(30))).is_ok());

        // default no wait -> ok, with the 30s default applied.
        let timeout = validate_start_wait_flags(false, false, None).expect("default no wait ok");
        assert_eq!(timeout, Duration::from_secs(30));
    }

    // Port of TestParseRouteFlags. The helper itself lives in `cli/mod.rs`
    // (shared with `up`), but the Go test ships with `start_test.go`, so the
    // assertions are ported here against `super::parse_route_flags`.
    #[test]
    fn parse_route_flags_cases() {
        use crate::config::Route;

        // empty -> no routes.
        assert!(super::super::parse_route_flags(&[])
            .expect("empty ok")
            .is_empty());

        // single route.
        let got = super::super::parse_route_flags(&["/api=8080".to_string()]).expect("single ok");
        assert_eq!(
            got,
            vec![Route {
                path: "/api".to_string(),
                port: 8080
            }]
        );

        // multiple routes.
        let got =
            super::super::parse_route_flags(&["/api=8080".to_string(), "/ws=9000".to_string()])
                .expect("multiple ok");
        assert_eq!(
            got,
            vec![
                Route {
                    path: "/api".to_string(),
                    port: 8080
                },
                Route {
                    path: "/ws".to_string(),
                    port: 9000
                }
            ]
        );

        // missing equals.
        let err = super::super::parse_route_flags(&["/api8080".to_string()])
            .expect_err("expected missing-equals error");
        assert!(
            err.to_string().contains("expected path=port"),
            "unexpected error: {err}"
        );

        // invalid port.
        let err = super::super::parse_route_flags(&["/api=notaport".to_string()])
            .expect_err("expected invalid-port error");
        assert!(
            err.to_string().contains("invalid route port"),
            "unexpected error: {err}"
        );

        // missing leading slash.
        let err = super::super::parse_route_flags(&["api=8080".to_string()])
            .expect_err("expected missing-slash error");
        assert!(
            err.to_string().contains("must start with /"),
            "unexpected error: {err}"
        );
    }
}
