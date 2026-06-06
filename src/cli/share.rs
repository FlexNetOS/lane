//! `lane share` — expose a local port via a lane.show tunnel.
//!
//! Faithful port of `cmd/share.go`. Validates the port and subdomain, requires
//! authentication, opens a tunnel [`Client`], and prints the public URL plus a
//! live per-request log until interrupted with Ctrl+C.

use anyhow::{anyhow, Context, Result};
use chrono::Local;

use crate::auth;
use crate::config;
use crate::log;
use crate::term;
use crate::tunnel::{self, Client, ClientOptions, RequestEvent};

/// Run the `share` command.
pub async fn run(args: &super::ShareArgs) -> Result<()> {
    let port = args.port;
    // Note: `port` is a `u16`, so it is always within 1..=65535 except for 0.
    // Go validated `port < 1 || port > 65535` against an `int`; here only 0 is
    // reachable, but we keep the exact message text for parity.
    if port < 1 {
        return Err(anyhow!("invalid port {port}: must be between 1 and 65535"));
    }

    let subdomain = args.subdomain.clone().unwrap_or_default();
    let share_domain = args.domain.clone().unwrap_or_default();

    if !subdomain.is_empty() && !share_domain.is_empty() {
        return Err(anyhow!("cannot use --subdomain and --domain together"));
    }

    tunnel::validate_subdomain(&subdomain)?;

    let info = auth::require()?;
    let token = info.token;

    let server_url = config::tunnel_server_url();

    let password = args.password.clone().unwrap_or_default();

    // In --json mode each proxied request is emitted as a compact NDJSON line
    // (a `request` event) instead of the colorized human line.
    let json = args.json;
    let on_request: Box<dyn Fn(RequestEvent) + Send + Sync> = Box::new(move |e: RequestEvent| {
        if json {
            println!("{}", share_request_json(&e));
            return;
        }
        let status_style = term::style_for_status(e.status);
        println!(
            "{}  {:<4} {}  {}  {}",
            term::dim(Local::now().format("%H:%M:%S").to_string()),
            e.method,
            e.path,
            status_style(&e.status.to_string()),
            term::dim(log::format_duration(e.duration)),
        );
    });

    let mut client = Client::new(ClientOptions {
        server_url,
        token,
        subdomain: subdomain.clone(),
        domain: share_domain.clone(),
        local_port: port,
        password: password.clone(),
        ttl: args.ttl,
        on_request: Some(on_request),
    });

    let url = match client.connect().await {
        Ok(url) => url,
        Err(err) => {
            let err_msg = err.to_string();
            if err_msg.contains("Pro subscription") {
                let feature = if !subdomain.is_empty() {
                    "Custom subdomains"
                } else if !share_domain.is_empty() {
                    "Custom domains"
                } else if !password.is_empty() {
                    "Password protection"
                } else {
                    "This feature"
                };

                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "error",
                            "error": format!("{feature} requires a Pro subscription"),
                            "upgrade_url": "https://app.lane.sh/settings/billing",
                        })
                    );
                    return Ok(());
                }

                println!(
                    "\n{} {feature} requires a Pro subscription.",
                    term::cross_mark()
                );
                println!("  Upgrade at: https://app.lane.sh/settings/billing\n");
                println!("  Free:");
                println!("  {}", term::dim(format!("lane share --port {port}")));
                println!(
                    "  {}\n",
                    term::dim(format!("lane share --port {port} --ttl 30m"))
                );
                println!("  Pro:");
                println!(
                    "  {}",
                    term::dim(format!("lane share --port {port} --subdomain myapp"))
                );
                println!(
                    "  {}",
                    term::dim(format!("lane share --port {port} --domain myapp.com"))
                );
                println!(
                    "  {}\n",
                    term::dim(format!("lane share --port {port} --password secret"))
                );
                return Ok(());
            }
            return Err(err).context("tunnel connection failed");
        }
    };

    let domain_url = client.domain_url();

    if args.json {
        // Emit the `connected` event carrying the public URL (the automation value).
        println!(
            "{}",
            share_connected_json(&url, &domain_url, port, &password)
        );
    } else {
        let arrow = term::dim("→");
        let target = term::dim(format!("localhost:{port}"));

        println!();
        println!(
            "{} {}  {arrow}  {target}",
            term::check_mark(),
            term::green(&url)
        );
        if !domain_url.is_empty() {
            println!(
                "{} {}  {arrow}  {target}",
                term::check_mark(),
                term::green(&domain_url)
            );
        }
        if !password.is_empty() {
            println!("Password: {password}");
        }
        println!("\nPress Ctrl+C to disconnect\n");
    }

    // Block until interrupted (Go waited on the SIGINT/SIGTERM context).
    let _ = tokio::signal::ctrl_c().await;

    client.close().await;
    if args.json {
        println!("{}", serde_json::json!({ "event": "disconnected" }));
    } else {
        println!("\nDisconnected.");
    }
    Ok(())
}

/// Compact NDJSON `request` event for `lane share --json` (one per proxied request).
fn share_request_json(e: &RequestEvent) -> String {
    serde_json::json!({
        "event": "request",
        "method": e.method,
        "path": e.path,
        "status": e.status,
        "duration": log::format_duration(e.duration),
    })
    .to_string()
}

/// The `connected` event for `lane share --json`: the public URL, target port,
/// and (when set) the custom-domain URL and access password.
fn share_connected_json(
    url: &str,
    domain_url: &str,
    port: u16,
    password: &str,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "event": "connected",
        "url": url,
        "port": port,
        "local": format!("localhost:{port}"),
    });
    if !domain_url.is_empty() {
        v["domain_url"] = serde_json::json!(domain_url);
    }
    if !password.is_empty() {
        v["password"] = serde_json::json!(password);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // `connected` event carries the public URL (the automation value); domain_url
    // and password are present only when set.
    #[test]
    fn share_connected_json_shape() {
        let bare = share_connected_json("https://abc.lane.show", "", 3000, "");
        assert_eq!(bare["event"], "connected");
        assert_eq!(bare["url"], "https://abc.lane.show");
        assert_eq!(bare["port"], 3000);
        assert_eq!(bare["local"], "localhost:3000");
        assert!(
            bare.get("domain_url").is_none(),
            "domain_url omitted when empty"
        );
        assert!(
            bare.get("password").is_none(),
            "password omitted when empty"
        );

        let full =
            share_connected_json("https://abc.lane.show", "https://myapp.com", 8080, "secret");
        assert_eq!(full["domain_url"], "https://myapp.com");
        assert_eq!(full["password"], "secret");
    }

    // Each proxied request is a compact single-line `request` NDJSON object.
    #[test]
    fn share_request_json_is_compact_event() {
        let e = RequestEvent {
            method: "GET".into(),
            path: "/api/health".into(),
            status: 200,
            duration: Duration::from_millis(12),
        };
        let s = share_request_json(&e);
        assert!(!s.contains('\n'), "request JSON must be single-line NDJSON");
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(v["event"], "request");
        assert_eq!(v["method"], "GET");
        assert_eq!(v["path"], "/api/health");
        assert_eq!(v["status"], 200);
        assert!(v["duration"].is_string(), "duration is a formatted string");
    }
}
