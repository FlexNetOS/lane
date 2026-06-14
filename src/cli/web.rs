//! `lane web` — governed web egress through obscura (ADR-0001 Option B).
//!
//! `lane web open <url>` navigates to a URL; `lane web run <script>` runs an
//! automation script against a target URL. Every op is **deny-by-default**: the
//! requested URL is checked against the [`crate::webpolicy`] gate (built from the
//! config allow-lists) *before* obscura is ever spawned. The live spawn is gated
//! behind the `obscura` cargo feature — without it the command still parses and
//! authorizes, then fails closed with a clear "rebuild with `--features obscura`"
//! error (mirroring `lane start --acme`).
//!
//! The command is ALWAYS present so `lane web --help` works in the default build;
//! only the live action is feature-gated.

use anyhow::{Context, Result};

use crate::config;
use crate::web::{self, WebOp};

/// CLI args for the `lane web` top-level subcommand.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct WebArgs {
    #[clap(subcommand)]
    pub command: WebCommand,
}

/// Subcommands available under `lane web`.
#[derive(Debug, Clone, clap::Subcommand)]
pub(crate) enum WebCommand {
    /// Open (navigate to) a URL through lane's governed egress.
    Open {
        /// The absolute http/https URL to navigate to (deny-by-default).
        url: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run an automation script against a target URL through governed egress.
    Run {
        /// Path to the local automation script obscura runs.
        script: String,
        /// The initial navigation target (the policy-checked URL).
        #[arg(long)]
        url: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run the `lane web` subcommand.
pub async fn run(args: &WebArgs) -> Result<()> {
    let (op, json) = match &args.command {
        WebCommand::Open { url, json } => (WebOp::Open { url: url.clone() }, *json),
        WebCommand::Run { script, url, json } => (
            WebOp::Run {
                script_path: script.clone(),
                url: url.clone(),
            },
            *json,
        ),
    };

    let cfg = config::load().context("loading config")?;
    let policy = cfg.web_policy();
    let obscura = cfg.obscura();
    let tls_inspect = cfg.web_tls_inspect();
    let ca_pem = crate::cert::ca_cert_path();
    let ca_pem = ca_pem.to_string_lossy();

    let kind = op.kind();
    let target = op.target().to_string();

    match web::run(&policy, &obscura, &ca_pem, tls_inspect, &op).await {
        Ok(outcome) => {
            if json {
                print_json(outcome.op, &outcome.target, true, None);
            } else {
                crate::log::info(&format!("web {kind} {target} — done"));
            }
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            if json {
                // Machine-readable failure: exit non-zero but emit the shape.
                print_json(kind, &target, false, Some(&msg));
            }
            Err(e)
        }
    }
}

/// Emit the machine-readable `{op, target, allowed, error?}` result.
fn print_json(op: &str, target: &str, allowed: bool, error: Option<&str>) {
    let mut payload = serde_json::json!({
        "op": op,
        "target": target,
        "allowed": allowed,
    });
    if let Some(err) = error {
        payload["error"] = serde_json::Value::String(err.to_string());
    }
    match serde_json::to_string_pretty(&payload) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{{\"op\":\"{op}\",\"target\":\"{target}\",\"allowed\":{allowed}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_args_map_to_open_op() {
        let args = WebArgs {
            command: WebCommand::Open {
                url: "https://example.com/".into(),
                json: true,
            },
        };
        match &args.command {
            WebCommand::Open { url, json } => {
                assert_eq!(url, "https://example.com/");
                assert!(json);
            }
            _ => panic!("expected Open"),
        }
    }

    #[test]
    fn run_args_carry_script_and_url() {
        let args = WebArgs {
            command: WebCommand::Run {
                script: "/tmp/s.js".into(),
                url: "https://example.com/start".into(),
                json: false,
            },
        };
        let op = match &args.command {
            WebCommand::Run { script, url, .. } => WebOp::Run {
                script_path: script.clone(),
                url: url.clone(),
            },
            _ => panic!("expected Run"),
        };
        assert_eq!(op.target(), "https://example.com/start");
        assert_eq!(op.kind(), "run");
    }
}
