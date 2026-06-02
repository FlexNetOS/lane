//! `lane uninstall` — remove all lane data and configuration.
//!
//! Faithful port of `cmd/uninstall.go`. When not running as root the command
//! re-executes itself under `sudo --preserve-env=HOME … uninstall` (inheriting
//! stdio) and returns the child's exit status. As root it stops the daemon,
//! removes the CA from the trust store, tears down port forwarding, cleans
//! `/etc/hosts`, removes `~/.lane`, and finally removes the `lane` binary.
//!
//! The Go original ran every action through `term.RunSteps`, whose `Step.Run`
//! is synchronous. Stopping the daemon needs `daemon::is_running` /
//! `daemon::send_ipc`, which are async in this port. To preserve behavior we
//! perform the daemon-stop as an awaited block that prints its own check/warn
//! line in the same style as a step, then drive the remaining (synchronous)
//! steps through [`crate::term::step::run_steps`].

use std::process::Command;

use anyhow::{Context, Result};

use crate::cert;
use crate::config;
use crate::daemon::{self, MessageType, Request};
use crate::osutil;
use crate::system;
use crate::term::{self, step::Step};

/// Run the uninstall flow.
pub async fn run() -> Result<()> {
    if osutil::geteuid() != 0 {
        let exe = std::env::current_exe().context("failed to find lane binary")?;

        let status = Command::new("sudo")
            .arg("--preserve-env=HOME")
            .arg(&exe)
            .arg("uninstall")
            .status()
            .context("failed to find lane binary")?;

        if status.success() {
            return Ok(());
        }
        // Mirror Go's `return sudoCmd.Run()`: a non-zero child surfaces as an
        // error carrying the exit status text.
        return match status.code() {
            Some(code) => Err(anyhow::anyhow!("exit status {code}")),
            None => Err(anyhow::anyhow!("signal: killed")),
        };
    }

    println!("Uninstalling lane...");

    // Step 1: stop the daemon. This is the only async action; resolve it here
    // and print a step-style line ourselves rather than inside run_steps (whose
    // closures are synchronous).
    stop_daemon_step().await;

    // The remaining steps are synchronous, exactly as in the Go original.
    let steps = vec![
        Step {
            name: "Removing CA from trust store".to_string(),
            run: Box::new(|| Ok(skip_on_err(cert::untrust_ca()))),
            interactive: false,
        },
        Step {
            name: "Removing port forwarding rules".to_string(),
            run: Box::new(|| {
                let pf = system::new_port_forwarder();
                Ok(skip_on_err(pf.disable()))
            }),
            interactive: false,
        },
        Step {
            name: "Cleaning /etc/hosts".to_string(),
            run: Box::new(|| Ok(skip_on_err(system::remove_all_hosts()))),
            interactive: false,
        },
        Step {
            name: "Removing ~/.lane/".to_string(),
            run: Box::new(|| {
                // Mirror Go's `os.RemoveAll(config.Dir())` — error ignored.
                let _ = std::fs::remove_dir_all(config::dir());
                Ok("done".to_string())
            }),
            interactive: false,
        },
        Step {
            name: "Removing lane binary".to_string(),
            run: Box::new(|| {
                let exe = match std::env::current_exe() {
                    Ok(p) => p,
                    Err(e) => return Ok(format!("skipped ({e})")),
                };
                match std::fs::remove_file(&exe) {
                    Ok(()) => Ok("done".to_string()),
                    Err(e) => Ok(format!("skipped ({e})")),
                }
            }),
            interactive: false,
        },
    ];

    term::step::run_steps(steps)?;

    println!("\nlane has been completely removed.");
    Ok(())
}

/// Stop the running daemon, printing a step-style line for the outcome.
///
/// Mirrors the Go "Stopping daemon" step: skipped when the daemon is not
/// running, skipped (with the error text) when the shutdown IPC fails, otherwise
/// done. The output matches `term::step::print_result`'s formatting so the line
/// is indistinguishable from a `run_steps`-driven step.
async fn stop_daemon_step() {
    let name = "Stopping daemon";

    let result = if !daemon::is_running().await {
        "skipped (not running)".to_string()
    } else {
        match daemon::send_ipc(Request {
            msg_type: MessageType::Shutdown,
            data: None,
        })
        .await
        {
            Ok(_) => "done".to_string(),
            Err(e) => format!("skipped ({e})"),
        }
    };

    if let Some(reason) = result.strip_prefix("skipped") {
        // `print_result` renders `"{warn} {name} (skipped …)"`; reproduce it.
        println!("{} {} (skipped{})", term::warn_mark(), name, reason);
    } else {
        println!("{} {}", term::check_mark(), name);
    }
}

/// Run `f`, returning `"done"` on success or `"skipped ({err})"` on failure,
/// mirroring the Go steps' `fmt.Sprintf("skipped (%v)", err)` shape.
fn skip_on_err(f: Result<()>) -> String {
    match f {
        Ok(()) => "done".to_string(),
        Err(e) => format!("skipped ({e})"),
    }
}
