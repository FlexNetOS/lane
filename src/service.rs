//! Generate and install an OS service definition that auto-starts the lane proxy
//! daemon at login/boot — a **systemd user unit** on Linux, a **launchd
//! LaunchAgent** on macOS. Inspired by the daemon-lifecycle reference tools (the
//! systemd/launchd sections of `docs/reference/repositories.md`).
//!
//! The service runs at the **user** level (no root): this mirrors how lane
//! already runs its daemon as the invoking user (it elevates per privileged op
//! via [`crate::osutil::run_privileged`]), so installing the service needs no
//! privilege. The unit's start command re-execs the lane binary in daemon mode
//! (`_LANE_DAEMON=1`), exactly as [`crate::daemon::run_detached`] does.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The launchd label / reverse-DNS identifier for the lane LaunchAgent. Mirrors
/// the macOS `pf` anchor convention (`com.lane`).
const LAUNCHD_LABEL: &str = "com.lane.daemon";

/// Which OS service manager lane targets on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// Linux: a systemd **user** unit (`systemctl --user`).
    Systemd,
    /// macOS: a launchd **LaunchAgent** (`launchctl`).
    Launchd,
}

impl Manager {
    /// Detect the service manager for the current platform. Errors on platforms
    /// lane does not manage (matching lane's macOS/Linux-only support surface).
    pub fn detect() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Manager::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Manager::Systemd)
        } else {
            bail!("lane install --service supports only Linux (systemd) and macOS (launchd)")
        }
    }

    /// Human label, e.g. `systemd (user unit)` / `launchd (LaunchAgent)`.
    pub fn label(self) -> &'static str {
        match self {
            Manager::Systemd => "systemd (user unit)",
            Manager::Launchd => "launchd (LaunchAgent)",
        }
    }

    /// Absolute path the unit file is written to, under `$HOME`:
    /// - systemd: `~/.config/systemd/user/lane.service`
    /// - launchd: `~/Library/LaunchAgents/com.lane.daemon.plist`
    ///
    /// Resolved via `dirs::home_dir()` (like [`crate::config::dir`]), so an
    /// overridden `HOME` redirects it for tests.
    pub fn unit_path(self) -> Result<PathBuf> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(match self {
            Manager::Systemd => home.join(".config/systemd/user/lane.service"),
            Manager::Launchd => home.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist")),
        })
    }

    /// The shell hint shown when the service was written but not enabled.
    fn enable_hint(self) -> &'static str {
        match self {
            Manager::Systemd => "systemctl --user enable --now lane.service",
            Manager::Launchd => "launchctl load <path>",
        }
    }
}

/// Render the systemd user unit text for a lane daemon started from `exe`.
/// `ExecStart` re-execs the binary with `_LANE_DAEMON=1` (the daemon trigger,
/// per [`crate::daemon`]); `Restart=on-failure` keeps it alive.
pub fn render_systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=lane local HTTPS proxy daemon\n\
         Documentation=https://github.com/FlexNetOS/lane\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=_LANE_DAEMON=1\n\
         ExecStart={exe}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    )
}

/// Render the launchd LaunchAgent plist for a lane daemon started from `exe`.
/// `RunAtLoad` + `KeepAlive` give boot-start and restart-on-exit; the daemon
/// trigger env (`_LANE_DAEMON=1`) is set via `EnvironmentVariables`.
pub fn render_launchd_plist(exe: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{exe}</string>\n\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>_LANE_DAEMON</key>\n\
         \t\t<string>1</string>\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        label = LAUNCHD_LABEL,
        exe = exe.display()
    )
}

/// Render the unit text for the given manager + binary path (pure; no I/O).
pub fn render(manager: Manager, exe: &Path) -> String {
    match manager {
        Manager::Systemd => render_systemd_unit(exe),
        Manager::Launchd => render_launchd_plist(exe),
    }
}

/// The result of installing the service unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The service manager used (`systemd (user unit)` / `launchd (LaunchAgent)`).
    pub manager: &'static str,
    /// Absolute path of the written unit file.
    pub path: PathBuf,
    /// Whether the unit was also enabled/started (`--enable`).
    pub enabled: bool,
    /// Shell command to enable it later (shown when `enabled` is false).
    pub enable_hint: &'static str,
}

/// Write the lane service unit for this platform (creating parent dirs), using
/// the current executable as the daemon binary. When `enable` is true, also
/// enable + start it via the platform tool (`systemctl --user enable --now` /
/// `launchctl load`).
pub fn install(enable: bool) -> Result<Installed> {
    let manager = Manager::detect()?;
    let exe = std::env::current_exe().context("cannot locate the lane binary")?;
    let path = manager.unit_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating service directory {}", parent.display()))?;
    }
    std::fs::write(&path, render(manager, &exe))
        .with_context(|| format!("writing service unit {}", path.display()))?;

    if enable {
        enable_service(manager, &path)?;
    }

    Ok(Installed {
        manager: manager.label(),
        path,
        enabled: enable,
        enable_hint: manager.enable_hint(),
    })
}

/// Enable + start the freshly written unit using the platform service tool.
fn enable_service(manager: Manager, path: &Path) -> Result<()> {
    let status = match manager {
        Manager::Systemd => std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "lane.service"])
            .status()
            .context("running systemctl --user enable --now lane.service")?,
        Manager::Launchd => std::process::Command::new("launchctl")
            .arg("load")
            .arg(path)
            .status()
            .context("running launchctl load")?,
    };
    if !status.success() {
        bail!("service enable command failed with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Point `HOME` at an isolated temp dir so `unit_path()`/`install()` resolve
    /// there. Keep the returned guard alive for the test's duration.
    fn isolate_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        tmp
    }

    #[test]
    fn test_systemd_unit_shape() {
        let unit = render_systemd_unit(Path::new("/usr/local/bin/lane"));
        assert!(unit.contains("ExecStart=/usr/local/bin/lane"), "{unit}");
        assert!(unit.contains("Environment=_LANE_DAEMON=1"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
        assert!(unit.contains("Restart=on-failure"), "{unit}");
    }

    #[test]
    fn test_launchd_plist_shape() {
        let plist = render_launchd_plist(Path::new("/usr/local/bin/lane"));
        assert!(
            plist.contains("<string>com.lane.daemon</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/usr/local/bin/lane</string>"),
            "{plist}"
        );
        assert!(plist.contains("<key>_LANE_DAEMON</key>"), "{plist}");
        assert!(plist.contains("<key>RunAtLoad</key>"), "{plist}");
        assert!(plist.contains("<key>KeepAlive</key>"), "{plist}");
        // Well-formed plist envelope.
        assert!(plist.starts_with("<?xml"), "{plist}");
        assert!(plist.trim_end().ends_with("</plist>"), "{plist}");
    }

    #[test]
    fn test_render_dispatches_by_manager() {
        let exe = Path::new("/x/lane");
        assert_eq!(render(Manager::Systemd, exe), render_systemd_unit(exe));
        assert_eq!(render(Manager::Launchd, exe), render_launchd_plist(exe));
    }

    #[test]
    #[serial_test::serial]
    fn test_unit_path_under_home() {
        let _home = isolate_home();
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            Manager::Systemd.unit_path().unwrap(),
            home.join(".config/systemd/user/lane.service")
        );
        assert_eq!(
            Manager::Launchd.unit_path().unwrap(),
            home.join("Library/LaunchAgents/com.lane.daemon.plist")
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_install_writes_unit_without_enabling() {
        let _home = isolate_home();
        let installed = install(false).expect("install should write the unit");
        assert!(!installed.enabled);
        assert!(installed.path.exists(), "unit file should exist");
        let body = std::fs::read_to_string(&installed.path).unwrap();
        assert!(
            body.contains("_LANE_DAEMON"),
            "unit should set the daemon env: {body}"
        );
    }
}
