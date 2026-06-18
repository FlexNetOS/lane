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
}

/// Run the `lane net` subcommand.
pub async fn run(args: &NetArgs) -> Result<()> {
    match &args.command {
        NetCommand::Adopt { connection, json } => adopt(connection.as_deref(), *json),
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
}
