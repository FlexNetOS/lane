//! CLI surface (clap derive) and command dispatch.
//!
//! Faithful port of the Go `cmd/` package (cobra). The `Cli`/`Commands`
//! definitions and shared helpers live here; each subcommand's behavior lives
//! in its own submodule and is dispatched from [`run`].

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::config::Domain;

mod cert;
mod completions;
mod config;
mod doctor;
mod domain;
mod down;
mod inspect;
mod install;
mod list;
mod login;
mod logout;
mod logs;
mod net;
mod portfwd;
mod relay;
mod restart;
mod share;
mod start;
mod stop;
mod uninstall;
mod up;
mod upgrade;
mod version;
mod web;

pub(crate) use portfwd::{ingress_ports_reachable, should_reload_port_forwarding};

#[derive(Parser)]
#[command(
    name = "lane",
    version = crate::VERSION,
    about = "Map custom local domains to dev server ports",
    long_about = "lane maps custom local domains to dev server ports with HTTPS\n\
                  and WebSocket passthrough for HMR.\n\n  \
                  lane start myapp --port 3000       # myapp.test → localhost:3000\n  \
                  lane start app.loc --port 3000     # app.loc → localhost:3000\n  \
                  lane start api --port 8080         # add another domain\n  \
                  lane list                          # see what's running\n  \
                  lane stop myapp                    # stop one domain\n  \
                  lane stop                          # stop everything",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start proxying a domain
    Start(StartArgs),
    /// Stop proxying a domain, or stop everything
    Stop(StopArgs),
    /// Restart the lane daemon
    Restart,
    /// Start all services from .lane.yaml
    Up(UpArgs),
    /// Stop project services from .lane.yaml
    Down(DownArgs),
    /// List all domains and tunnels
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show request logs
    Logs(LogsArgs),
    /// Share a local port via tunnel
    Share(ShareArgs),
    /// Authenticate with your lane account
    Login,
    /// Log out of your lane account
    Logout,
    /// Manage custom domains
    Domain(DomainArgs),
    /// Manage certificates (key types, wildcard certs)
    Cert(cert::CertArgs),
    /// Project config helpers (generate a starter .lane.yaml)
    Config(config::ConfigArgs),
    /// Governed web egress via obscura (deny-by-default; needs --features obscura)
    Web(web::WebArgs),
    /// Cross-machine relay: trusted fleet mesh + governed streams (needs --features relay)
    Relay(relay::RelayArgs),
    /// Adopt & apply the host network plane (adopt: read-only; apply: dry-run by default; needs --features hostnet)
    Net(net::NetArgs),
    /// Diagnose setup issues
    Doctor(DoctorArgs),
    /// Live request-inspector TUI (tails the access log)
    Inspect(InspectArgs),
    /// Install an OS service that auto-starts the lane daemon at login/boot
    Install(InstallArgs),
    /// Remove all lane data and configuration
    Uninstall,
    /// Upgrade lane to the latest version
    #[command(visible_alias = "update")]
    Upgrade,
    /// Print the version
    Version(VersionArgs),
    /// Generate a shell completion script
    Completions(CompletionsArgs),
}

#[derive(Args)]
pub(crate) struct StartArgs {
    /// Domain name (e.g. myapp or app.loc)
    pub name: String,
    /// Local port to proxy to (required)
    #[arg(short, long)]
    pub port: u16,
    /// Route a path to a different port (e.g. /api=8080), repeatable
    #[arg(long = "route")]
    pub routes: Vec<String>,
    /// Access log mode: full|minimal|off
    #[arg(long = "log-mode")]
    pub log_mode: Option<String>,
    /// Enable CORS headers on proxied responses
    #[arg(long)]
    pub cors: bool,
    /// Wait for the upstream app to become reachable before returning
    #[arg(long)]
    pub wait: bool,
    /// Maximum time to wait for upstream with --wait (e.g. 30s, 2m)
    #[arg(long, value_parser = parse_duration)]
    pub timeout: Option<Duration>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Extra SAN entries for the leaf cert (comma-separated IPs or DNS names), e.g. `10.0.0.1,extra.test`
    #[arg(long)]
    pub san: Option<String>,
    /// Obtain a real Let's Encrypt cert via ACME HTTP-01 (domain must be a public FQDN; needs --features acme)
    #[arg(long)]
    pub acme: bool,
    /// Contact email for the ACME account (required with --acme)
    #[arg(long = "acme-email")]
    pub acme_email: Option<String>,
    /// Use the Let's Encrypt staging environment for --acme
    #[arg(long = "acme-staging")]
    pub acme_staging: bool,
}

#[derive(Args)]
pub(crate) struct StopArgs {
    /// Domain to stop (omit to stop everything)
    pub name: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct UpArgs {
    /// Path to .lane.yaml
    #[arg(short, long)]
    pub config: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct DownArgs {
    /// Path to .lane.yaml
    #[arg(short, long)]
    pub config: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct ListArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct DoctorArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Auto-heal any `Fail` issues found by the diagnostic checks
    #[arg(long)]
    pub fix: bool,
}

#[derive(Args)]
pub(crate) struct VersionArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct InspectArgs {
    /// Only show requests for this domain (optional filter)
    pub name: Option<String>,
}

#[derive(Args)]
pub(crate) struct InstallArgs {
    /// Install the OS service that auto-starts the lane daemon at login/boot
    #[arg(long)]
    pub service: bool,
    /// Also enable and start the service immediately
    #[arg(long)]
    pub enable: bool,
    /// Print the service unit to stdout instead of installing it
    #[arg(long)]
    pub print: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct LogsArgs {
    /// Filter by domain name
    pub name: Option<String>,
    /// Follow log output
    #[arg(short, long)]
    pub follow: bool,
    /// Clear the access log file
    #[arg(long)]
    pub flush: bool,
    /// Output as JSON (one NDJSON object per line)
    #[arg(long)]
    pub json: bool,
    /// Show only the last N matching records
    #[arg(short = 'n', long)]
    pub lines: Option<i64>,
}

#[derive(Args)]
pub(crate) struct ShareArgs {
    /// Local port to expose (or pass a reverse-tunnel forward spec argument)
    #[arg(short, long)]
    pub port: Option<u16>,
    /// Reverse-tunnel forward spec, chisel-style: R:[remotePort:][localHost:]localPort
    /// (e.g. R:3000:localhost:8080) — forward to a specific local upstream
    #[arg(value_name = "FORWARD")]
    pub forward: Option<String>,
    /// Vanity subdomain name
    #[arg(long)]
    pub subdomain: Option<String>,
    /// Require password for tunnel access
    #[arg(long)]
    pub password: Option<String>,
    /// Tunnel time-to-live (e.g. 30m, 1h). Free: max 1h, Pro: unlimited
    #[arg(long, value_parser = parse_duration)]
    pub ttl: Option<Duration>,
    /// Custom domain for this tunnel
    #[arg(long)]
    pub domain: Option<String>,
    /// Proxy hop to route the tunnel dial through (repeatable, in order):
    /// [scheme://][user:pass@]host:port, scheme socks5 (default) or http
    /// (e.g. --hop socks5://proxy.corp:1080 --hop http://gw.corp:8080)
    #[arg(long = "hop", value_name = "PROXY")]
    pub hops: Vec<String>,
    /// Output as JSON (NDJSON event stream: connected, request*, disconnected)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub(crate) struct CompletionsArgs {
    /// Shell to generate the completion script for
    pub shell: clap_complete::Shell,
}

#[derive(Args)]
pub(crate) struct DomainArgs {
    #[command(subcommand)]
    pub command: DomainCommands,
}

#[derive(Subcommand)]
pub(crate) enum DomainCommands {
    /// Add a custom domain
    Add {
        /// Domain to add (e.g. myapp.example.com)
        domain: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List custom domains
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Verify DNS for a custom domain
    Verify {
        /// Domain to verify
        domain: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove a custom domain
    Remove {
        /// Domain to remove
        domain: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Parse a Go-style duration string ("30m", "1h", "500ms", "2s").
fn parse_duration(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| format!("invalid duration {s:?}: {e}"))
}

/// Parse args and dispatch to the matching subcommand handler.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Start(a) => start::run(&a).await,
        Commands::Stop(a) => stop::run(&a).await,
        Commands::Restart => restart::run().await,
        Commands::Up(a) => up::run(&a).await,
        Commands::Down(a) => down::run(&a).await,
        Commands::List(a) => list::run(&a).await,
        Commands::Logs(a) => logs::run(&a).await,
        Commands::Share(a) => share::run(&a).await,
        Commands::Login => login::run().await,
        Commands::Logout => logout::run().await,
        Commands::Domain(a) => domain::run(&a).await,
        Commands::Web(a) => web::run(&a).await,
        Commands::Relay(a) => relay::run(&a).await,
        Commands::Net(a) => net::run(&a).await,
        Commands::Doctor(a) => doctor::run(&a).await,
        Commands::Inspect(a) => inspect::run(&a).await,
        Commands::Cert(a) => cert::run(&a).await,
        Commands::Config(a) => config::run(&a).await,
        Commands::Install(a) => install::run(&a).await,
        Commands::Uninstall => uninstall::run().await,
        Commands::Upgrade => upgrade::run().await,
        Commands::Version(a) => version::run(&a).await,
        // Completion generation is synchronous (no I/O beyond stdout); don't await.
        Commands::Completions(a) => completions::run(&a),
    }
}

// --- shared helpers (ported from cmd/root.go) ------------------------------

/// Normalize CLI domain input: lowercase, trim, drop a trailing dot, then apply
/// the `.test` default TLD. Mirrors Go's `normalizeName`.
pub(crate) fn normalize_name(input: &str) -> String {
    let trimmed = input.trim().to_lowercase();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(&trimmed);
    crate::config::normalize_domain(trimmed)
}

/// Print the configured services with aligned `https://… → localhost:port`
/// rows. Mirrors Go's `printServices`.
pub(crate) fn print_services(domains: &[Domain]) {
    use crate::term::{check_mark, dim, green};

    let mut max_len = 0usize;
    for d in domains {
        let u = "https://".len() + d.name.len();
        max_len = max_len.max(u);
        for r in &d.routes {
            max_len = max_len.max(u + r.path.len());
        }
    }

    let arrow = dim("→");
    for d in domains {
        let url = format!("https://{}", d.name);
        println!(
            "{} {}  {}  {}",
            check_mark(),
            green(format!("{url:<max_len$}")),
            arrow,
            dim(format!("localhost:{}", d.port)),
        );
        for r in &d.routes {
            let route_url = format!("{url}{}", r.path);
            println!(
                "  {}  {}  {}",
                green(format!("{route_url:<max_len$}")),
                arrow,
                dim(format!("localhost:{}", r.port)),
            );
        }
    }
}

/// Parse repeatable `--route path=port` flags into [`crate::config::Route`]s.
/// Mirrors Go's `parseRouteFlags`.
pub(crate) fn parse_route_flags(flags: &[String]) -> Result<Vec<crate::config::Route>> {
    let mut routes = Vec::with_capacity(flags.len());
    for f in flags {
        let (path, port_str) = f.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("invalid route {f:?}: expected path=port (e.g. /api=8080)")
        })?;
        let port: i64 = port_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid route port {port_str:?}"))?;
        crate::config::validate_route(path, port)?;
        routes.push(crate::config::Route {
            path: path.to_string(),
            port: port as u16,
        });
    }
    Ok(routes)
}
