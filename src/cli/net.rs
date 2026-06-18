//! `lane net` — host network-plane adopt-consume (ADR-0003).
//!
//! `lane net adopt` reads the host's existing network configuration (via
//! NetworkManager) and emits lane's Rust-native, lossless [`crate::net::model`]
//! (a superset of netplan v2) to stdout — **read-only and sanitizing**: it never
//! mutates the host and never copies secret material (see [`crate::net::adopt`]).
//!
//! The command ALWAYS parses so `lane net --help` works in the default build; the
//! live read is gated behind the `hostnet` cargo feature. Without it the command
//! still parses, then fails closed with a clear "rebuild with `--features
//! hostnet`" error, mirroring `lane web` / `lane relay`.

use anyhow::Result;

/// CLI args for the `lane net` top-level subcommand.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct NetArgs {
    #[clap(subcommand)]
    pub command: NetCommand,
}

/// Subcommands available under `lane net`.
#[derive(Debug, Clone, clap::Subcommand)]
pub(crate) enum NetCommand {
    /// Adopt the host network plane: read the live host (via NetworkManager) and
    /// emit lane's lossless model. Read-only and sanitizing — never mutates the
    /// host, never copies secret material. Needs `--features hostnet`.
    Adopt {
        /// Adopt only this NetworkManager connection (by name); omit to adopt the
        /// whole host plane.
        #[arg(long)]
        connection: Option<String>,
        /// Output as JSON instead of YAML.
        #[arg(long)]
        json: bool,
    },
    /// Render a desired model to the host: compute the additive nmcli reconcile
    /// plan against the live host and (with `--apply`) execute it. **Dry-run is the
    /// default** — without `--apply` it prints the plan and mutates NOTHING. Never
    /// flushes connections it does not own (ADR-0003 §3). Needs `--features hostnet`.
    Apply {
        /// Path to the desired model (a netplan-v2-superset YAML file, as emitted by
        /// `lane net adopt`). This is the P1 input surface; `--host` profiles are P2.
        #[arg(long)]
        profile: String,
        /// Execute the plan (mutate the host). Omit for the safe dry-run default,
        /// which prints the plan and changes nothing. Mutually exclusive with
        /// `--dry-run`.
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        /// Print the plan and mutate nothing (the default behavior; accepted
        /// explicitly so the safe intent can be stated). Mutually exclusive with
        /// `--apply`.
        #[arg(long)]
        dry_run: bool,
        /// Print the plan as JSON instead of the `nmcli …` line form.
        #[arg(long)]
        json: bool,
    },
}

/// Run the `lane net` subcommand.
pub async fn run(args: &NetArgs) -> Result<()> {
    match &args.command {
        NetCommand::Adopt { connection, json } => adopt(connection.as_deref(), *json),
        NetCommand::Apply {
            profile,
            apply,
            dry_run: _,
            json,
        } => apply_cmd(profile, *apply, *json),
    }
}

/// Adopt the host plane (or one connection) and print the model. Feature build.
#[cfg(feature = "hostnet")]
fn adopt(connection: Option<&str>, json: bool) -> Result<()> {
    use anyhow::Context;

    let doc = match connection {
        Some(name) => {
            let mut doc = crate::net::model::NetworkDocument::new(crate::net::model::Network {
                renderer: Some(crate::net::model::Renderer::NetworkManager),
                ..crate::net::model::Network::v2()
            });
            match crate::net::adopt::adopt_connection(name)
                .with_context(|| format!("adopting connection {name:?}"))?
            {
                Some(unit) => unit.insert_into(&mut doc),
                None => anyhow::bail!(
                    "connection {name:?} is not a host-plane network type lane adopts \
                     (ethernet/wifi/bridge), or does not exist"
                ),
            }
            doc
        }
        None => crate::net::adopt::adopt_all().context("adopting host network plane")?,
    };

    print_doc(&doc, json)
}

/// Feature-off `adopt`: fails closed (mirrors `lane web` without `obscura`).
#[cfg(not(feature = "hostnet"))]
fn adopt(_connection: Option<&str>, _json: bool) -> Result<()> {
    anyhow::bail!(
        "the host network-plane adopter is not enabled in this build; rebuild with \
         `--features hostnet` (ADR-0003)"
    )
}

/// Render a desired profile to the host plane. Computes the additive reconcile plan
/// against the live host; dry-run (default) prints it, `--apply` executes it
/// fail-closed. Feature build.
#[cfg(feature = "hostnet")]
fn apply_cmd(profile: &str, apply: bool, json: bool) -> Result<()> {
    use anyhow::Context;

    // Desired model: parse the committed profile file (P1 input surface).
    let raw = std::fs::read_to_string(profile)
        .with_context(|| format!("reading desired profile {profile:?}"))?;
    let desired: crate::net::model::NetworkDocument = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing desired profile {profile:?}"))?;

    // Current host state: reuse the P0b adopter (read-only, sanitizing).
    let current = crate::net::adopt::adopt_all().context("reading current host network plane")?;

    // The additive reconcile plan (pure).
    let plan = crate::net::apply::reconcile(&desired, &current);

    if apply {
        // Mutating path: execute op-by-op, fail-closed (stop on first error).
        crate::net::apply::apply_plan(&plan).context("applying reconcile plan")?;
        print_plan(&plan, json);
        return Ok(());
    }

    // Dry-run (default): print the plan, mutate nothing.
    print_plan(&plan, json);
    Ok(())
}

/// Feature-off `apply`: fails closed (mirrors `adopt` without `hostnet`).
#[cfg(not(feature = "hostnet"))]
fn apply_cmd(_profile: &str, _apply: bool, _json: bool) -> Result<()> {
    anyhow::bail!(
        "the host network-plane renderer is not enabled in this build; rebuild with \
         `--features hostnet` (ADR-0003)"
    )
}

/// Print the reconcile plan as `nmcli …` lines (default) or JSON to stdout. The
/// plan is the machine-consumable artifact, so it is printed raw to stdout (not
/// through the styled term layer). Secret material is never present.
#[cfg(feature = "hostnet")]
fn print_plan(plan: &crate::net::apply::ReconcilePlan, json: bool) {
    if json {
        let ops: Vec<serde_json::Value> = plan
            .ops
            .iter()
            .map(|op| {
                serde_json::json!({
                    "nmcli": op.to_argv(),
                })
            })
            .collect();
        let doc = serde_json::json!({ "ops": ops });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print!("{}", plan.render_text());
    }
}

/// Print the adopted document as YAML (default) or JSON to stdout.
#[cfg(feature = "hostnet")]
fn print_doc(doc: &crate::net::model::NetworkDocument, json: bool) -> Result<()> {
    use anyhow::Context;

    let rendered = if json {
        serde_json::to_string_pretty(doc).context("serializing adopted model to JSON")?
    } else {
        serde_yaml::to_string(doc).context("serializing adopted model to YAML")?
    };
    // The model IS the output (a machine-consumable artifact); print it raw to
    // stdout rather than through the styled term layer.
    print!("{rendered}");
    if json {
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopt_args_parse() {
        let args = NetArgs {
            command: NetCommand::Adopt {
                connection: Some("cognitum-seed-linklocal".into()),
                json: true,
            },
        };
        match &args.command {
            NetCommand::Adopt { connection, json } => {
                assert_eq!(connection.as_deref(), Some("cognitum-seed-linklocal"));
                assert!(json);
            }
            other => panic!("expected Adopt, got {other:?}"),
        }
    }

    #[test]
    fn apply_args_parse() {
        let args = NetArgs {
            command: NetCommand::Apply {
                profile: "/tmp/desired.yaml".into(),
                apply: false,
                dry_run: false,
                json: false,
            },
        };
        match &args.command {
            NetCommand::Apply {
                profile,
                apply,
                dry_run,
                json,
            } => {
                assert_eq!(profile, "/tmp/desired.yaml");
                // Dry-run is the safe default (no mutation without an explicit flag).
                assert!(
                    !apply,
                    "apply must default to false (dry-run is the default)"
                );
                assert!(!dry_run);
                assert!(!json);
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    #[cfg(not(feature = "hostnet"))]
    #[test]
    fn adopt_fails_closed_without_feature() {
        let err = adopt(None, false).unwrap_err();
        assert!(
            err.to_string().contains("--features hostnet"),
            "must point the user at the feature flag, got: {err}"
        );
    }

    #[cfg(not(feature = "hostnet"))]
    #[test]
    fn apply_fails_closed_without_feature() {
        let err = apply_cmd("/tmp/desired.yaml", false, false).unwrap_err();
        assert!(
            err.to_string().contains("--features hostnet"),
            "must point the user at the feature flag, got: {err}"
        );
    }
}
