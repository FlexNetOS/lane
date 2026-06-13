//! `lane install --service` — generate and install an OS service that
//! auto-starts the lane daemon at login/boot (a systemd user unit on Linux, a
//! launchd LaunchAgent on macOS).
//!
//! `--print` renders the unit to stdout instead of installing it (transparency +
//! scripting); `--enable` also enables and starts it; `--json` emits a
//! machine-readable result, mirroring lane's other `--json` surfaces.

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::service;

/// `lane install [--service] [--enable] [--print] [--json]`.
pub async fn run(args: &super::InstallArgs) -> Result<()> {
    if !args.service {
        bail!("lane install: specify --service (the only supported install target today)");
    }

    // --print: render to stdout, never touch the filesystem.
    if args.print {
        let manager = service::Manager::detect()?;
        let exe = std::env::current_exe().context("cannot locate the lane binary")?;
        print!("{}", service::render(manager, &exe));
        return Ok(());
    }

    let installed = service::install(args.enable)?;

    if args.json {
        let value = json!({
            "manager": installed.manager,
            "path": installed.path.display().to_string(),
            "written": true,
            "enabled": installed.enabled,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).context("marshaling JSON")?
        );
        return Ok(());
    }

    println!(
        "{} installed lane service ({})",
        crate::term::check_mark(),
        installed.manager
    );
    println!("  {}", installed.path.display());
    if installed.enabled {
        println!("{} enabled + started", crate::term::check_mark());
    } else {
        println!("  enable with: {}", installed.enable_hint);
    }
    Ok(())
}
