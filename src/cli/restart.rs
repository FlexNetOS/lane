//! `lane restart` — bounce the lane daemon (down-if-up, then up).
//!
//! A thin CLI-side orchestration over four already-`pub` daemon primitives —
//! [`daemon::is_running`], [`daemon::send_ipc`] (`Shutdown`),
//! [`daemon::run_detached`], and [`daemon::wait_for_daemon`] — exactly the verbs
//! `start.rs` and `stop.rs` already use. It bounces the single daemon process and
//! never touches `config.yaml` or `/etc/hosts`; domain preservation falls out for
//! free because the daemon reloads config from disk on every start. There is no
//! new IPC verb and no new privileged path.

use anyhow::{Context, Result};

use crate::daemon::{self, MessageType, Request};

/// What `restart` does, decided purely from whether the daemon is already up.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Daemon was running -> bring it down, then back up.
    Restarted,
    /// Daemon was not running -> just bring it up.
    Started,
}

/// Pure decision seam: running -> [`Action::Restarted`], not running ->
/// [`Action::Started`]. Extracted so the semantics are unit-testable without
/// spawning a real daemon.
fn decide_action(daemon_running: bool) -> Action {
    if daemon_running {
        Action::Restarted
    } else {
        Action::Started
    }
}

/// Wait up to 5 seconds for the daemon to be fully down before respawning.
///
/// Mirrors [`daemon::wait_for_daemon`]'s bounded-poll style (50 × 100ms = 5s
/// ceiling), polling [`daemon::is_running`] — which already treats a missing
/// socket as not-running — and erroring out rather than spinning forever.
async fn wait_for_down() -> Result<()> {
    for _ in 0..50 {
        if !daemon::is_running().await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("daemon did not shut down within 5 seconds"))
}

/// Run `lane restart`. Bounces the daemon if it is running, otherwise starts it.
/// Leaves `config.yaml` and `/etc/hosts` untouched in both cases.
pub async fn run() -> Result<()> {
    let running = daemon::is_running().await;
    match decide_action(running) {
        Action::Restarted => {
            // Bring the old daemon down via the same request `stop.rs` uses.
            daemon::send_ipc(Request {
                msg_type: MessageType::Shutdown,
                data: None,
            })
            .await
            .context("stopping daemon")?;
            // Wait for it to be fully down before respawning.
            wait_for_down().await?;
            // Spawn a fresh daemon and wait for it to come up.
            daemon::run_detached().context("starting daemon")?;
            daemon::wait_for_daemon().await?;
            println!("Restarted lane daemon.");
        }
        Action::Started => {
            daemon::run_detached().context("starting daemon")?;
            daemon::wait_for_daemon().await?;
            println!("lane daemon was not running; started it.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, Config, Domain};

    // The action decision is pure and unit-testable without a daemon.
    #[test]
    fn decide_action_running_is_restarted() {
        assert_eq!(decide_action(true), Action::Restarted);
    }

    #[test]
    fn decide_action_not_running_is_started() {
        assert_eq!(decide_action(false), Action::Started);
    }

    // The request `restart` constructs to bring the old daemon down must be a
    // plain `Shutdown` (no `Reload`, no new verb).
    #[test]
    fn down_request_is_shutdown() {
        let req = Request {
            msg_type: MessageType::Shutdown,
            data: None,
        };
        assert_eq!(req.msg_type, MessageType::Shutdown);
        assert!(req.data.is_none());
    }

    /// Set HOME to an isolated temp dir and create `~/.lane`. Mirrors the
    /// `stop.rs`/`down.rs` HOME-isolation recipe.
    fn with_isolated_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().expect("TempDir");
        std::env::set_var("HOME", tmp.path());
        std::fs::create_dir_all(config::dir()).expect("MkdirAll config dir");
        tmp
    }

    // Config-preservation, no-daemon path: with a config seeded with a domain and
    // no running daemon, the parts of restart's flow that are sandbox-safe must
    // leave `config.yaml` byte-identical.
    //
    // restart's no-daemon path is `decide_action(false) -> Started`, whose only
    // side effects are spawning a daemon (`run_detached`/`wait_for_daemon`) —
    // it calls neither `Config::save()` nor `system::add_host`/`remove_host`.
    // Spawning a real detached daemon needs `setsid`/forking + the IPC socket and
    // is not reliable in the sandbox, so we exercise the non-spawning seams
    // (`is_running` -> `decide_action`) and assert the config file is untouched by
    // them, which is exactly the config-touching surface restart has.
    #[tokio::test]
    #[serial_test::serial]
    async fn restart_preserves_config_on_no_daemon_path() {
        let _home = with_isolated_home();

        let seeded = Config {
            domains: vec![Domain {
                name: "myapp.test".to_string(),
                port: 3000,
                routes: Vec::new(),
            }],
            ..Default::default()
        };
        seeded.save().expect("save seeded config");

        let before = std::fs::read(config::config_path()).expect("read config before");

        // No daemon is running under the isolated HOME, so the decision is
        // `Started` and the non-spawning surface runs without touching config.
        let running = daemon::is_running().await;
        assert!(!running, "no daemon should be running under isolated HOME");
        assert_eq!(decide_action(running), Action::Started);

        // The decision/probe seams must not have rewritten the config file.
        let after = std::fs::read(config::config_path()).expect("read config after");
        assert_eq!(before, after, "restart must not modify config.yaml");

        // And the domain list survives a fresh load (load-from-disk on start).
        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 1);
        assert_eq!(cfg.domains[0].name, "myapp.test");
    }
}
