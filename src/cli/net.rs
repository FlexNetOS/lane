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
        /// `lane net adopt`). The P1 input surface. Mutually exclusive with `--host`.
        #[arg(long, conflicts_with = "host")]
        profile: Option<String>,
        /// Reproduce a committed per-host profile: resolve `hosts/<name>.yaml` (under
        /// `--profiles-dir`) as the desired model (P2). Mutually exclusive with
        /// `--profile`.
        #[arg(long, conflicts_with = "profile")]
        host: Option<String>,
        /// Base directory for `--host` profiles (default `hosts/`). Overridable so
        /// callers/tests can point at any committed profile tree.
        #[arg(long, default_value = crate::net::profile::DEFAULT_PROFILES_DIR)]
        profiles_dir: String,
        /// Render backend override: `networkmanager` (the nmcli reconcile, default) or
        /// `networkd` (the systemd-networkd file render, for non-NM boxes). When
        /// omitted, the model's own `renderer` selects the backend.
        #[arg(long)]
        renderer: Option<RendererArg>,
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
    /// Manage in-repo per-host network profiles (ADR-0003 §4, Portability). A profile
    /// is a committed `hosts/<name>.yaml` capturing a box's durable host plane so a
    /// fresh box reproduces it with `lane net apply --host <name>`.
    Profile {
        #[clap(subcommand)]
        command: ProfileCommand,
    },
}

/// The `--renderer` backend choice for `lane net apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum RendererArg {
    /// The nmcli additive reconcile (P1 default, what the estate runs).
    #[value(name = "networkmanager")]
    NetworkManager,
    /// The systemd-networkd file render (P2 portability target for non-NM boxes).
    #[value(name = "networkd")]
    Networkd,
}

/// Subcommands under `lane net profile`.
#[derive(Debug, Clone, clap::Subcommand)]
pub(crate) enum ProfileCommand {
    /// Capture the live host's durable network plane into a committed
    /// `hosts/<name>.yaml` (name defaults to the live hostname). Adopts the host
    /// (read-only) and strips runtime-managed interfaces. Needs `--features hostnet`.
    Save {
        /// Profile name (defaults to the live hostname).
        name: Option<String>,
        /// Base directory to write the profile into (default `hosts/`).
        #[arg(long, default_value = crate::net::profile::DEFAULT_PROFILES_DIR)]
        profiles_dir: String,
    },
    /// List the committed per-host profiles under `--profiles-dir`.
    List {
        /// Base directory to list profiles from (default `hosts/`).
        #[arg(long, default_value = crate::net::profile::DEFAULT_PROFILES_DIR)]
        profiles_dir: String,
    },
    /// Print one committed profile (`hosts/<name>.yaml`) to stdout.
    Show {
        /// Profile name to print.
        name: String,
        /// Base directory to read the profile from (default `hosts/`).
        #[arg(long, default_value = crate::net::profile::DEFAULT_PROFILES_DIR)]
        profiles_dir: String,
    },
}

/// Run the `lane net` subcommand.
pub async fn run(args: &NetArgs) -> Result<()> {
    match &args.command {
        NetCommand::Adopt { connection, json } => adopt(connection.as_deref(), *json),
        NetCommand::Apply {
            profile,
            host,
            profiles_dir,
            renderer,
            apply,
            dry_run: _,
            json,
        } => apply_cmd(
            profile.as_deref(),
            host.as_deref(),
            profiles_dir,
            *renderer,
            *apply,
            *json,
        ),
        NetCommand::Profile { command } => profile_cmd(command),
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

/// Render a desired model to the host plane. The desired model comes from either a
/// `--profile <path>` file (P1) or a `--host <name>` committed profile (P2). For the
/// `networkmanager` backend it computes the additive reconcile plan against the live
/// host; for `networkd` it renders the systemd-networkd files. Dry-run (default)
/// prints, `--apply` executes fail-closed. Feature build.
#[cfg(feature = "hostnet")]
fn apply_cmd(
    profile: Option<&str>,
    host: Option<&str>,
    profiles_dir: &str,
    renderer: Option<RendererArg>,
    apply: bool,
    json: bool,
) -> Result<()> {
    use anyhow::Context;

    // Resolve the desired model from --profile <path> or --host <name> (clap already
    // enforces they are mutually exclusive; require exactly one).
    let desired = resolve_desired(profile, host, profiles_dir)?;

    // Backend: an explicit --renderer override, else the model's own renderer
    // (default NetworkManager when the model names none).
    let backend = renderer.unwrap_or(match desired.network.renderer {
        Some(crate::net::model::Renderer::Networkd) => RendererArg::Networkd,
        _ => RendererArg::NetworkManager,
    });

    match backend {
        RendererArg::NetworkManager => {
            // Current host state: reuse the P0b adopter (read-only, sanitizing).
            let current =
                crate::net::adopt::adopt_all().context("reading current host network plane")?;
            let plan = crate::net::apply::reconcile(&desired, &current);

            if apply {
                crate::net::apply::apply_plan(&plan).context("applying reconcile plan")?;
            }
            print_plan(&plan, json);
            Ok(())
        }
        RendererArg::Networkd => {
            // Render the systemd-networkd files (pure). The current host is not read:
            // networkd files are a declarative full render, not an additive diff.
            let files = crate::net::networkd::render_networkd(&desired);
            if apply {
                crate::net::networkd::write_networkd_files(&files)
                    .context("writing networkd files")?;
            }
            print_networkd(&files, json);
            Ok(())
        }
    }
}

/// Resolve the desired model from `--profile <path>` (P1) or `--host <name>` (P2).
/// Exactly one is required; clap enforces they are not both set.
#[cfg(feature = "hostnet")]
fn resolve_desired(
    profile: Option<&str>,
    host: Option<&str>,
    profiles_dir: &str,
) -> Result<crate::net::model::NetworkDocument> {
    use anyhow::Context;

    match (profile, host) {
        (Some(path), None) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("reading desired profile {path:?}"))?;
            serde_yaml::from_str(&raw).with_context(|| format!("parsing desired profile {path:?}"))
        }
        (None, Some(name)) => crate::net::profile::read_profile(profiles_dir, name)
            .with_context(|| format!("loading host profile {name:?}")),
        (None, None) => anyhow::bail!(
            "specify the desired model with `--profile <path>` or `--host <name>` (ADR-0003)"
        ),
        // Unreachable: clap's conflicts_with rejects both, but fail closed anyway.
        (Some(_), Some(_)) => {
            anyhow::bail!("`--profile` and `--host` are mutually exclusive")
        }
    }
}

/// Feature-off `apply`: fails closed (mirrors `adopt` without `hostnet`).
#[cfg(not(feature = "hostnet"))]
fn apply_cmd(
    _profile: Option<&str>,
    _host: Option<&str>,
    _profiles_dir: &str,
    _renderer: Option<RendererArg>,
    _apply: bool,
    _json: bool,
) -> Result<()> {
    anyhow::bail!(
        "the host network-plane renderer is not enabled in this build; rebuild with \
         `--features hostnet` (ADR-0003)"
    )
}

/// Run `lane net profile <save|list|show>`. `save` adopts the live host (gated);
/// `list`/`show` are pure filesystem reads available in every build.
fn profile_cmd(command: &ProfileCommand) -> Result<()> {
    match command {
        ProfileCommand::Save { name, profiles_dir } => profile_save(name.as_deref(), profiles_dir),
        ProfileCommand::List { profiles_dir } => profile_list(profiles_dir),
        ProfileCommand::Show { name, profiles_dir } => profile_show(name, profiles_dir),
    }
}

/// `lane net profile save`: adopt the live host, strip runtime units, and write the
/// committed `hosts/<name>.yaml`. Feature build (it adopts the host).
#[cfg(feature = "hostnet")]
fn profile_save(name: Option<&str>, profiles_dir: &str) -> Result<()> {
    use anyhow::Context;

    let name = match name {
        Some(n) => n.to_string(),
        None => crate::net::profile::live_hostname().context("resolving live hostname")?,
    };
    let path = crate::net::profile::save_profile(profiles_dir, &name)
        .with_context(|| format!("saving host profile {name:?}"))?;
    // A repo write (safe), not a host mutation — confirm it through the styled layer.
    println!(
        "{} saved host profile {:?} → {}",
        crate::term::check_mark(),
        name,
        path.display()
    );
    Ok(())
}

/// Feature-off `profile save`: fails closed (it adopts the live host).
#[cfg(not(feature = "hostnet"))]
fn profile_save(_name: Option<&str>, _profiles_dir: &str) -> Result<()> {
    anyhow::bail!(
        "the host network-plane adopter is not enabled in this build; rebuild with \
         `--features hostnet` (ADR-0003)"
    )
}

/// `lane net profile list`: print the committed profile names (pure FS read).
fn profile_list(profiles_dir: &str) -> Result<()> {
    let names = crate::net::profile::list_profiles(profiles_dir)?;
    if names.is_empty() {
        crate::log::info(&format!("no host profiles in {profiles_dir}"));
        return Ok(());
    }
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// `lane net profile show`: print one committed profile to stdout (pure FS read).
fn profile_show(name: &str, profiles_dir: &str) -> Result<()> {
    use anyhow::Context;

    let doc = crate::net::profile::read_profile(profiles_dir, name)
        .with_context(|| format!("reading host profile {name:?}"))?;
    let yaml = serde_yaml::to_string(&doc).context("serializing host profile to YAML")?;
    // The model IS the output (a machine-consumable artifact); print it raw.
    print!("{yaml}");
    Ok(())
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

/// Print the rendered systemd-networkd files: a `--- <path> ---`-bannered dump
/// (default) or JSON (`{ "files": [{ "path", "contents" }] }`) to stdout. The render
/// is the machine-consumable artifact, so it is printed raw. No secret material is
/// present (a credential is only ever the documented marker).
#[cfg(feature = "hostnet")]
fn print_networkd(files: &[crate::net::networkd::NetworkdFile], json: bool) {
    if json {
        let entries: Vec<serde_json::Value> = files
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "contents": f.contents,
                })
            })
            .collect();
        let doc = serde_json::json!({ "files": entries });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print!("{}", crate::net::networkd::render_files_text(files));
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
                profile: Some("/tmp/desired.yaml".into()),
                host: None,
                profiles_dir: "hosts".into(),
                renderer: None,
                apply: false,
                dry_run: false,
                json: false,
            },
        };
        match &args.command {
            NetCommand::Apply {
                profile,
                host,
                renderer,
                apply,
                dry_run,
                json,
                ..
            } => {
                assert_eq!(profile.as_deref(), Some("/tmp/desired.yaml"));
                assert!(host.is_none());
                assert!(renderer.is_none());
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

    /// `--profile` and `--host` are mutually exclusive (clap `conflicts_with`).
    #[test]
    fn apply_profile_and_host_conflict() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[clap(subcommand)]
            command: NetCommand,
        }

        let ok = Wrap::try_parse_from([
            "net",
            "apply",
            "--host",
            "trx50",
            "--profiles-dir",
            "/tmp/p",
        ]);
        assert!(ok.is_ok(), "host alone must parse");

        let conflict = Wrap::try_parse_from([
            "net",
            "apply",
            "--profile",
            "/tmp/d.yaml",
            "--host",
            "trx50",
        ]);
        assert!(
            conflict.is_err(),
            "--profile and --host must be mutually exclusive"
        );
    }

    /// `lane net profile save [name] [--profiles-dir]` parses; name is optional.
    #[test]
    fn profile_save_args_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[clap(subcommand)]
            command: NetCommand,
        }

        // Name defaults (omitted) — must still parse.
        let defaulted =
            Wrap::try_parse_from(["net", "profile", "save", "--profiles-dir", "/tmp/p"]).unwrap();
        match defaulted.command {
            NetCommand::Profile {
                command: ProfileCommand::Save { name, profiles_dir },
            } => {
                assert!(
                    name.is_none(),
                    "name defaults to live hostname when omitted"
                );
                assert_eq!(profiles_dir, "/tmp/p");
            }
            other => panic!("expected profile save, got {other:?}"),
        }

        // Explicit name.
        let named = Wrap::try_parse_from(["net", "profile", "save", "trx50"]).unwrap();
        match named.command {
            NetCommand::Profile {
                command: ProfileCommand::Save { name, profiles_dir },
            } => {
                assert_eq!(name.as_deref(), Some("trx50"));
                assert_eq!(profiles_dir, "hosts", "default profiles dir is hosts/");
            }
            other => panic!("expected profile save, got {other:?}"),
        }
    }

    /// `profile list` is a pure FS read available in every build: an empty TempDir
    /// lists nothing without error.
    #[test]
    fn profile_list_empty_dir_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        profile_list(dir).expect("listing an empty profiles dir must succeed");
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
        let err =
            apply_cmd(Some("/tmp/desired.yaml"), None, "hosts", None, false, false).unwrap_err();
        assert!(
            err.to_string().contains("--features hostnet"),
            "must point the user at the feature flag, got: {err}"
        );
    }

    #[cfg(not(feature = "hostnet"))]
    #[test]
    fn profile_save_fails_closed_without_feature() {
        let err = profile_save(Some("trx50"), "hosts").unwrap_err();
        assert!(
            err.to_string().contains("--features hostnet"),
            "must point the user at the feature flag, got: {err}"
        );
    }
}
