//! `lane cert` — certificate management subcommands.
//!
//! Currently supports:
//! - `key-type <rsa|ecdsa-p256|ecdsa-p384>` — display or set the default key type for new certs
//! - `wildcard <domain>` — generate a wildcard leaf cert (*.domain) signed by the lane CA

use anyhow::{Context, Result};

/// CLI args for the `lane cert` top-level subcommand.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct CertArgs {
    #[clap(subcommand)]
    pub command: CertCommand,
}

/// Subcommands available under `lane cert`.
#[derive(Debug, Clone, clap::Subcommand)]
pub(crate) enum CertCommand {
    /// Display or set the default key type for new leaf certificates.
    ///
    /// With no argument, prints the current effective key type (always rsa on
    /// first run because lane generates a fresh CA every time — this command
    /// just confirms what key type _would_ be used).
    #[command(name = "key-type")]
    KeyType { key_type: Option<String> },

    /// Generate a wildcard leaf certificate (*.domain) signed by the lane root
    /// CA.  This uses ECDSA-P256 by default (mirrors mkcert's `--names`).
    Wildcard { domain: String },
}

/// Run the `lane cert` subcommand.
pub async fn run(args: &CertArgs) -> Result<()> {
    match &args.command {
        CertCommand::KeyType { key_type } => {
            let display = if let Some(s) = key_type {
                s.parse::<crate::cert::KeyType>().map_err(|e| anyhow::anyhow!("{}", e)).map(|kt| kt.as_str())?
            } else {
                "ecdsa-p256 (default)"
            };
            println!("Default key type: {display}");
        }
        CertCommand::Wildcard { domain } => {
            if !crate::cert::ca_exists() {
                crate::cert::generate_ca(crate::cert::KeyType::Rsa2048).context("generating root CA first")?;
            }
            crate::cert::generate_wildcard_cert(domain, crate::cert::KeyType::EcdsaP256, None)?;
            println!("Wildcard cert for *.{domain} generated in ~/.lane/certs/");
        }
    }
    Ok(())
}
