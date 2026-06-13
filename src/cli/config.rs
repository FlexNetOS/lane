//! `lane config` — project-config helpers. Currently one subcommand,
//! `lane config template`, which scaffolds a commented starter `.lane.yaml`
//! (inspired by consul-template's config scaffolding).

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};

use crate::project;
use crate::term;

#[derive(Args)]
pub(crate) struct ConfigArgs {
    #[clap(subcommand)]
    pub command: ConfigCommand,
}

/// Subcommands available under `lane config`.
#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Generate a commented starter `.lane.yaml` project config
    Template(TemplateArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct TemplateArgs {
    /// Seed the example service domain (default: `myapp`)
    #[arg(long, default_value = "myapp")]
    pub domain: String,
    /// Seed the example service port (default: 3000)
    #[arg(long, default_value_t = 3000)]
    pub port: u16,
    /// Write to this file instead of stdout (default: print to stdout)
    #[arg(short, long)]
    pub output: Option<String>,
    /// Overwrite the output file if it already exists
    #[arg(long)]
    pub force: bool,
}

/// `lane config <subcommand>`.
pub async fn run(args: &ConfigArgs) -> Result<()> {
    match &args.command {
        ConfigCommand::Template(t) => template(t),
    }
}

/// `lane config template` — render (and optionally write) a starter `.lane.yaml`.
fn template(args: &TemplateArgs) -> Result<()> {
    if args.port < 1 {
        return Err(anyhow!(
            "invalid port {}: must be between 1 and 65535",
            args.port
        ));
    }

    let rendered = project::render_template(&args.domain, args.port);

    let Some(path) = &args.output else {
        print!("{rendered}");
        return Ok(());
    };

    if std::path::Path::new(path).exists() && !args.force {
        bail!("refusing to overwrite {path} (use --force)");
    }
    std::fs::write(path, &rendered).map_err(|e| anyhow!("writing {path}: {e}"))?;
    println!("{} wrote {path}", term::check_mark());
    Ok(())
}
