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

    let on_request: Box<dyn Fn(RequestEvent) + Send + Sync> = Box::new(|e: RequestEvent| {
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

    let arrow = term::dim("→");
    let target = term::dim(format!("localhost:{port}"));

    println!();
    println!(
        "{} {}  {arrow}  {target}",
        term::check_mark(),
        term::green(&url)
    );
    let domain_url = client.domain_url();
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

    // Block until interrupted (Go waited on the SIGINT/SIGTERM context).
    let _ = tokio::signal::ctrl_c().await;

    client.close().await;
    println!("\nDisconnected.");
    Ok(())
}
