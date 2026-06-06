//! `lane stop` (⇐ `cmd/stop.go`).
//!
//! Faithful port: `stop <name>` removes one domain; `stop` with no argument
//! removes everything and shuts the daemon down.

use anyhow::{Context, Result};

use crate::config;
use crate::daemon::{self, MessageType, Request};
use crate::system;

/// Dispatch `stop`: no name -> stop everything; otherwise stop the one domain.
pub async fn run(args: &super::StopArgs) -> Result<()> {
    match &args.name {
        None => stop_all(args.json).await,
        Some(name) => stop_one(&super::normalize_name(name), args.json).await,
    }
}

/// Build the `lane stop --json` payload: the domains stopped, the resulting
/// daemon state, and any non-fatal warnings (omitted when empty).
fn stop_json_payload(stopped: &[String], daemon: &str, warnings: &[String]) -> serde_json::Value {
    let mut v = serde_json::json!({
        "stopped": stopped,
        "daemon": daemon,
    });
    if !warnings.is_empty() {
        v["warnings"] = serde_json::json!(warnings);
    }
    v
}

/// Print a `lane stop --json` payload to stdout.
fn print_stop_json(stopped: &[String], daemon: &str, warnings: &[String]) -> Result<()> {
    let payload = stop_json_payload(stopped, daemon, warnings);
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).context("marshaling JSON")?
    );
    Ok(())
}

/// Stop a single domain. Mirrors Go's `stopOne`.
async fn stop_one(name: &str, json: bool) -> Result<()> {
    let mut remaining_domains = 0usize;

    config::with_lock(|| {
        let mut cfg = config::load()?;

        if cfg.find_domain(name).is_none() {
            return Err(anyhow::anyhow!("{name} is not running"));
        }

        cfg.remove_domain(name)?;
        remaining_domains = cfg.domains.len();
        Ok(())
    })?;

    system::remove_host(name).context("updating /etc/hosts")?;

    let running = daemon::is_running().await;
    let daemon_state = if !running {
        "not_running"
    } else if remaining_domains == 0 {
        "shutdown"
    } else {
        "reloaded"
    };
    if running {
        let (msg_type, ctx) = if remaining_domains == 0 {
            (MessageType::Shutdown, "stopping daemon")
        } else {
            (MessageType::Reload, "reloading daemon")
        };
        daemon::send_ipc(Request {
            msg_type,
            data: None,
        })
        .await
        .context(ctx)?;
    }

    if json {
        print_stop_json(&[name.to_string()], daemon_state, &[])?;
    } else if daemon_state == "shutdown" {
        println!("Stopped {name} (daemon shut down)");
    } else {
        println!("Stopped {name}");
    }

    Ok(())
}

/// Stop every domain and shut the daemon down. Mirrors Go's `stopAll`.
async fn stop_all(json: bool) -> Result<()> {
    let mut domains: Vec<config::Domain> = Vec::new();

    config::with_lock(|| {
        let mut cfg = config::load()?;
        domains = cfg.domains.clone();
        if !domains.is_empty() {
            cfg.domains = Vec::new();
            return cfg.save();
        }
        Ok(())
    })?;

    let running = daemon::is_running().await;
    if domains.is_empty() && !running {
        if json {
            print_stop_json(&[], "not_running", &[])?;
        } else {
            println!("Nothing is running.");
        }
        return Ok(());
    }

    let mut warnings: Vec<String> = Vec::new();
    for d in &domains {
        if let Err(e) = system::remove_host(&d.name) {
            if json {
                warnings.push(format!("failed to remove {} from /etc/hosts: {e}", d.name));
            } else {
                println!("Warning: failed to remove {} from /etc/hosts: {e}", d.name);
            }
        }
    }

    if running {
        daemon::send_ipc(Request {
            msg_type: MessageType::Shutdown,
            data: None,
        })
        .await
        .context("stopping daemon")?;
    }

    if json {
        let stopped: Vec<String> = domains.iter().map(|d| d.name.clone()).collect();
        let daemon_state = if running { "shutdown" } else { "not_running" };
        print_stop_json(&stopped, daemon_state, &warnings)?;
    } else {
        println!("Stopped all domains.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Domain};

    /// Set HOME to an isolated temp dir and create `~/.lane`. Mirrors Go's
    /// `setupStopTestHooks` HOME isolation.
    fn with_isolated_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().expect("TempDir");
        std::env::set_var("HOME", tmp.path());
        std::fs::create_dir_all(config::dir()).expect("MkdirAll config dir");
        tmp
    }

    /// Write a config seeded with `domains`. Mirrors Go's `seedDomains`.
    fn seed_domains(domains: Vec<Domain>) {
        let cfg = Config {
            domains,
            ..Default::default()
        };
        cfg.save().expect("save seeded config");
    }

    // `lane stop --json` payload: stopped domains + daemon state; `warnings` is
    // omitted when empty and present otherwise.
    #[test]
    fn stop_json_payload_shape() {
        let v = stop_json_payload(&["myapp.test".to_string()], "reloaded", &[]);
        assert_eq!(v["stopped"][0], "myapp.test");
        assert_eq!(v["daemon"], "reloaded");
        assert!(v.get("warnings").is_none(), "warnings omitted when empty");

        let v2 = stop_json_payload(&[], "not_running", &["boom".to_string()]);
        assert_eq!(v2["stopped"].as_array().unwrap().len(), 0);
        assert_eq!(v2["daemon"], "not_running");
        assert_eq!(v2["warnings"][0], "boom");
    }

    // Port of TestStopOneDomainNotFound.
    //
    // This branch returns its error from inside `with_lock` before any daemon or
    // host-file interaction, so it is a pure config-file test.
    #[tokio::test]
    #[serial_test::serial]
    async fn stop_one_domain_not_found() {
        let _home = with_isolated_home();
        seed_domains(vec![Domain {
            name: "api.test".to_string(),
            port: 8080,
            routes: Vec::new(),
        }]);

        let err = stop_one("myapp.test", false)
            .await
            .expect_err("expected stop_one to fail for missing domain");
        assert!(
            err.to_string().contains("is not running"),
            "unexpected error: {err}"
        );

        // The config must be untouched when the domain is missing.
        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 1);
        assert_eq!(cfg.domains[0].name, "api.test");
    }

    // TODO(test-phase): TestStopOneSendsShutdownWhenLastDomain,
    // TestStopOneSendsReloadWhenDomainsRemain,
    // TestStopAllRemovesHostsAndSendsShutdown, TestStopAllDaemonShutdownError —
    // Go injected fn-pointer seams for system.RemoveHost / daemon.IsRunning /
    // daemon.SendIPC. In Rust those are concrete (async, socket-backed) calls, so
    // these paths require a live daemon socket and /etc/hosts writes; covered in
    // the integration phase.

    // TODO(test-phase): TestStopAllNoDomainsNoDaemon — exercises the "Nothing is
    // running." branch, which calls daemon::is_running() (socket probe). Without
    // injecting the daemon-running predicate this depends on ambient daemon
    // state; deferred to the integration phase.
}
