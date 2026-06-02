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
        None => stop_all().await,
        Some(name) => stop_one(&super::normalize_name(name)).await,
    }
}

/// Stop a single domain. Mirrors Go's `stopOne`.
async fn stop_one(name: &str) -> Result<()> {
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

    if daemon::is_running().await {
        if remaining_domains == 0 {
            daemon::send_ipc(Request {
                msg_type: MessageType::Shutdown,
                data: None,
            })
            .await
            .context("stopping daemon")?;
            println!("Stopped {name} (daemon shut down)");
        } else {
            daemon::send_ipc(Request {
                msg_type: MessageType::Reload,
                data: None,
            })
            .await
            .context("reloading daemon")?;
            println!("Stopped {name}");
        }
    } else {
        println!("Stopped {name}");
    }

    Ok(())
}

/// Stop every domain and shut the daemon down. Mirrors Go's `stopAll`.
async fn stop_all() -> Result<()> {
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
        println!("Nothing is running.");
        return Ok(());
    }

    for d in &domains {
        if let Err(e) = system::remove_host(&d.name) {
            println!("Warning: failed to remove {} from /etc/hosts: {e}", d.name);
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

    println!("Stopped all domains.");
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

        let err = stop_one("myapp.test")
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
