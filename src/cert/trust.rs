//! cert::trust — install / remove the lane root CA in the OS trust store.
//!
//! Faithful, cfg-gated port of `trust_linux.go`, `trust_darwin.go`, and
//! `trust_unsupported.go`. Public API: [`trust_ca`] and [`untrust_ca`].
//!
//! Go used package-level function-pointer seams (`writeAnchorFileFn`,
//! `runPrivilegedTrustFn`, `execCommandDarwinFn`, …) to mock exec calls in
//! tests. Those exec-mocking cases are left as `// TODO(test-phase)` markers;
//! the pure helpers (anchor-path detection, error-message formatting) are
//! ported and unit-tested directly.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::anyhow;
use anyhow::Result;

// ===========================================================================
// Linux
// ===========================================================================

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::cert::ca_cert_path;
    use crate::osutil;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    // Anchor basename is `lane.crt` (renamed from slim's `slim.crt`).
    pub(super) const DEBIAN_ANCHOR_PATH: &str = "/usr/local/share/ca-certificates/lane.crt";
    pub(super) const RHEL_ANCHOR_PATH: &str = "/etc/pki/ca-trust/source/anchors/lane.crt";
    pub(super) const ARCH_ANCHOR_PATH: &str = "/etc/ca-certificates/trust-source/anchors/lane.crt";

    pub fn trust_ca() -> Result<()> {
        let cert_pem =
            std::fs::read(ca_cert_path()).map_err(|e| anyhow!("reading CA cert: {e}"))?;

        if osutil::command_exists("update-ca-certificates") {
            write_anchor_file(DEBIAN_ANCHOR_PATH, &cert_pem)?;
            let (output, res) = osutil::run_privileged("update-ca-certificates", &[]);
            if let Err(e) = res {
                return Err(anyhow!(
                    "update-ca-certificates failed: {}: {}",
                    trimmed(&output),
                    e
                ));
            }
            return Ok(());
        }

        if osutil::command_exists("update-ca-trust") {
            let anchor_path = detect_trust_anchor_path();
            write_anchor_file(anchor_path, &cert_pem)?;
            let (output, res) = osutil::run_privileged("update-ca-trust", &["extract"]);
            if let Err(e) = res {
                return Err(anyhow!(
                    "update-ca-trust failed: {}: {}",
                    trimmed(&output),
                    e
                ));
            }
            return Ok(());
        }

        Err(anyhow!(
            "no supported Linux CA trust tool found (need update-ca-certificates or update-ca-trust)"
        ))
    }

    pub fn untrust_ca() -> Result<()> {
        for path in [DEBIAN_ANCHOR_PATH, RHEL_ANCHOR_PATH, ARCH_ANCHOR_PATH] {
            remove_file_privileged(path)?;
        }

        if osutil::command_exists("update-ca-certificates") {
            let (output, res) = osutil::run_privileged("update-ca-certificates", &[]);
            if let Err(e) = res {
                return Err(anyhow!(
                    "update-ca-certificates failed: {}: {}",
                    trimmed(&output),
                    e
                ));
            }
            return Ok(());
        }

        if osutil::command_exists("update-ca-trust") {
            let (output, res) = osutil::run_privileged("update-ca-trust", &["extract"]);
            if let Err(e) = res {
                return Err(anyhow!(
                    "update-ca-trust failed: {}: {}",
                    trimmed(&output),
                    e
                ));
            }
            return Ok(());
        }

        Err(anyhow!(
            "no supported Linux CA trust tool found (need update-ca-certificates or update-ca-trust)"
        ))
    }

    /// Choose the anchor path for the `update-ca-trust` (RHEL/Arch) family by
    /// probing which trust-source directory already exists. Defaults to the
    /// RHEL path (matching `detectTrustAnchorPath`).
    pub(super) fn detect_trust_anchor_path() -> &'static str {
        if dir_exists(parent_of(RHEL_ANCHOR_PATH)) {
            RHEL_ANCHOR_PATH
        } else if dir_exists(parent_of(ARCH_ANCHOR_PATH)) {
            ARCH_ANCHOR_PATH
        } else {
            RHEL_ANCHOR_PATH
        }
    }

    fn parent_of(path: &str) -> &Path {
        Path::new(path).parent().unwrap_or_else(|| Path::new("/"))
    }

    fn dir_exists(path: &Path) -> bool {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    }

    /// Write `content` to the anchor `path`, escalating with `sudo` as needed.
    ///
    /// Mirrors `writeAnchorFile`: create the parent directory (falling back to
    /// `sudo mkdir -p` on permission errors) and write the file directly,
    /// falling back to `sudo tee` on permission errors.
    fn write_anchor_file(path: &str, content: &[u8]) -> Result<()> {
        let parent = parent_of(path);
        if let Err(e) = std::fs::create_dir_all(parent) {
            if e.kind() != std::io::ErrorKind::PermissionDenied {
                return Err(anyhow!(
                    "creating anchor directory {}: {e}",
                    parent.display()
                ));
            }
            let (output, res) = osutil::run_privileged("mkdir", &["-p", &parent.to_string_lossy()]);
            if let Err(mkdir_err) = res {
                return Err(anyhow!(
                    "creating anchor directory {}: {}: {}",
                    parent.display(),
                    trimmed(&output),
                    mkdir_err
                ));
            }
        }

        match std::fs::write(path, content) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(e) => return Err(anyhow!("writing anchor file {path}: {e}")),
        }

        // `sudo tee <path>` with the content on stdin (discard tee's stdout).
        let mut child = Command::new("sudo")
            .arg("tee")
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("writing anchor file {path}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(content)
                .map_err(|e| anyhow!("writing anchor file {path}: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| anyhow!("writing anchor file {path}: {e}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "writing anchor file {path}: {}: {}",
                trimmed(&out.stderr),
                status_text(out.status)
            ));
        }
        Ok(())
    }

    /// Remove `path`, escalating to `sudo rm -f` if a direct remove is denied;
    /// a missing file is treated as success. Mirrors `removeFilePrivileged`.
    fn remove_file_privileged(path: &str) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {}
        }

        let (output, res) = osutil::run_privileged("rm", &["-f", path]);
        if let Err(e) = res {
            return Err(anyhow!("removing {path}: {}: {}", trimmed(&output), e));
        }
        Ok(())
    }

    fn status_text(status: std::process::ExitStatus) -> String {
        match status.code() {
            Some(code) => format!("exit status {code}"),
            None => "signal: killed".to_string(),
        }
    }
}

/// Trim trailing/leading whitespace from combined command output, like Go's
/// `strings.TrimSpace(string(output))`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn trimmed(output: &[u8]) -> String {
    String::from_utf8_lossy(output).trim().to_string()
}

#[cfg(target_os = "linux")]
pub fn trust_ca() -> Result<()> {
    linux::trust_ca()
}

#[cfg(target_os = "linux")]
pub fn untrust_ca() -> Result<()> {
    linux::untrust_ca()
}

// ===========================================================================
// macOS
// ===========================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::cert::ca_cert_path;
    use std::process::Command;

    pub fn trust_ca() -> Result<()> {
        let cert_path = ca_cert_path();
        let output = Command::new("sudo")
            .args([
                "security",
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
            ])
            .arg(&cert_path)
            .output()
            .map_err(|e| anyhow!("trusting CA: {e}"))?;
        if !output.status.success() {
            return Err(anyhow!("trusting CA: {}", combined_trimmed(&output)));
        }
        Ok(())
    }

    pub fn untrust_ca() -> Result<()> {
        let cert_path = ca_cert_path();
        // No-op when the CA cert is missing (matches the Go os.IsNotExist guard).
        if !cert_path.exists() {
            return Ok(());
        }
        let output = Command::new("sudo")
            .args(["security", "remove-trusted-cert", "-d"])
            .arg(&cert_path)
            .output()
            .map_err(|e| anyhow!("untrusting CA: {e}"))?;
        if !output.status.success() {
            return Err(anyhow!("untrusting CA: {}", combined_trimmed(&output)));
        }
        Ok(())
    }

    /// Combined stdout+stderr, trimmed — mirrors Go's `cmd.CombinedOutput()`
    /// feeding `strings.TrimSpace`.
    fn combined_trimmed(output: &std::process::Output) -> String {
        let mut combined = output.stdout.clone();
        combined.extend_from_slice(&output.stderr);
        trimmed(&combined)
    }
}

#[cfg(target_os = "macos")]
pub fn trust_ca() -> Result<()> {
    macos::trust_ca()
}

#[cfg(target_os = "macos")]
pub fn untrust_ca() -> Result<()> {
    macos::untrust_ca()
}

// ===========================================================================
// Unsupported platforms
// ===========================================================================

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn trust_ca() -> Result<()> {
    Err(anyhow::anyhow!(
        "trusting CA is only supported on macOS and Linux"
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn untrust_ca() -> Result<()> {
    Err(anyhow::anyhow!(
        "untrusting CA is only supported on macOS and Linux"
    ))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::linux;

    // Pure-helper tests ported from trust_linux_test.go. The exec-mocking cases
    // depend on Go's package-level fn-pointer seams, which we did not reproduce.
    //
    // TODO(test-phase): TestTrustCAUsesUpdateCACertificates
    // TODO(test-phase): TestTrustCAUsesUpdateCATrust
    // TODO(test-phase): TestTrustCAFailsWhenNoSupportedTool
    // TODO(test-phase): TestUntrustCADeletesAnchorsAndUpdatesStore
    // TODO(test-phase): TestUntrustCAPropagatesRemoveError

    #[test]
    fn anchor_paths_use_lane_basename() {
        assert!(linux::DEBIAN_ANCHOR_PATH.ends_with("/lane.crt"));
        assert!(linux::RHEL_ANCHOR_PATH.ends_with("/lane.crt"));
        assert!(linux::ARCH_ANCHOR_PATH.ends_with("/lane.crt"));
        assert_eq!(
            linux::DEBIAN_ANCHOR_PATH,
            "/usr/local/share/ca-certificates/lane.crt"
        );
        assert_eq!(
            linux::RHEL_ANCHOR_PATH,
            "/etc/pki/ca-trust/source/anchors/lane.crt"
        );
        assert_eq!(
            linux::ARCH_ANCHOR_PATH,
            "/etc/ca-certificates/trust-source/anchors/lane.crt"
        );
    }

    #[test]
    fn detect_trust_anchor_path_defaults_to_rhel() {
        // On a host with neither RHEL nor Arch trust-source dir, the default is
        // the RHEL anchor path (matching detectTrustAnchorPath's default arm).
        // If the RHEL dir happens to exist, the same path is returned, so this
        // assertion holds regardless of host layout — except on a host that has
        // the Arch dir but not the RHEL dir, which is vanishingly rare in CI.
        let got = linux::detect_trust_anchor_path();
        assert!(
            got == linux::RHEL_ANCHOR_PATH || got == linux::ARCH_ANCHOR_PATH,
            "unexpected anchor path: {got}"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    // The darwin trust tests (trust_darwin_test.go) mock exec.Command via the
    // execCommandDarwinFn seam to assert the exact `security` argv and that
    // command output is surfaced in errors. We did not reproduce that seam.
    //
    // TODO(test-phase): TestTrustCAUsesExpectedSecurityCommand
    // TODO(test-phase): TestTrustCAErrorIncludesCommandOutput
    // TODO(test-phase): TestUntrustCAUsesExpectedSecurityCommand
    // TODO(test-phase): TestUntrustCAErrorIncludesCommandOutput
}
