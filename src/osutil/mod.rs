//! osutil — small OS helpers (privileged command execution, PATH lookup, euid).
//!
//! Faithful port of Go's `internal/osutil/osutil.go`:
//!
//! ```go
//! func RunPrivileged(name string, args ...string) ([]byte, error) {
//!     if os.Geteuid() == 0 {
//!         return exec.Command(name, args...).CombinedOutput()
//!     }
//!     all := append([]string{name}, args...)
//!     return exec.Command("sudo", all...).CombinedOutput()
//! }
//!
//! func CommandExists(name string) bool {
//!     _, err := exec.LookPath(name)
//!     return err == nil
//! }
//! ```

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Result};

/// Return the effective user id of the calling process.
///
/// Mirrors Go's `os.Geteuid()`; thin `unsafe` wrapper over `libc::geteuid()`.
pub fn geteuid() -> u32 {
    // SAFETY: `geteuid(2)` takes no arguments, has no preconditions, and is
    // always successful — it cannot fail or invoke undefined behavior.
    unsafe { libc::geteuid() }
}

/// Run a command, escalating with `sudo` when not already running as root.
///
/// Mirrors Go's `RunPrivileged`. When the effective uid is 0 the command runs
/// directly; otherwise it is prefixed with `sudo`. The returned `Vec<u8>` is the
/// combined stdout+stderr (like Go's `CombinedOutput`). The `Result` is `Ok(())`
/// on exit code 0, otherwise `Err(anyhow!("exit status <code>"))` to match the
/// text of Go's `*exec.ExitError`.
pub fn run_privileged(name: &str, args: &[&str]) -> (Vec<u8>, Result<()>) {
    let output = if geteuid() == 0 {
        Command::new(name).args(args).output()
    } else {
        let mut cmd = Command::new("sudo");
        cmd.arg(name).args(args);
        cmd.output()
    };

    match output {
        Ok(out) => {
            // Combine stdout and stderr the way Go's CombinedOutput does.
            let mut combined = out.stdout;
            combined.extend_from_slice(&out.stderr);
            let result = if out.status.success() {
                Ok(())
            } else {
                match out.status.code() {
                    Some(code) => Err(anyhow!("exit status {}", code)),
                    // Terminated by a signal (no exit code on Unix).
                    None => Err(anyhow!("signal: killed")),
                }
            };
            (combined, result)
        }
        // Failure to even spawn the process (e.g. binary not found): no output,
        // surface the I/O error.
        Err(e) => (Vec::new(), Err(anyhow::Error::new(e))),
    }
}

/// Report whether `name` is an executable reachable via `$PATH`.
///
/// Mirrors Go's `CommandExists`, which relies on `exec.LookPath`. If `name`
/// contains a path separator it is checked directly; otherwise each `$PATH`
/// entry is scanned for an executable file by that name. No external `which`
/// binary is invoked.
pub fn command_exists(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // If the name contains a path separator, treat it as a direct path (Go's
    // LookPath does this too) and check it for being an executable file.
    if name.contains('/') {
        return is_executable_file(Path::new(name));
    }

    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };

    for dir in std::env::split_paths(&path) {
        // An empty PATH element means the current directory (POSIX semantics).
        let candidate = if dir.as_os_str().is_empty() {
            Path::new(name).to_path_buf()
        } else {
            dir.join(name)
        };
        if is_executable_file(&candidate) {
            return true;
        }
    }

    false
}

/// Return true if `path` is a regular file with at least one execute bit set.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && (md.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_exists_finds_sh() {
        // `sh` is present on essentially every POSIX system and must be on PATH.
        assert!(command_exists("sh"));
    }

    #[test]
    fn command_exists_rejects_unknown() {
        assert!(!command_exists("this-command-should-not-exist-xyzzy-1234"));
    }

    #[test]
    fn command_exists_rejects_empty() {
        assert!(!command_exists(""));
    }

    #[test]
    fn command_exists_direct_path() {
        // /bin/sh is the canonical executable to check via a direct path.
        if Path::new("/bin/sh").exists() {
            assert!(command_exists("/bin/sh"));
        }
        assert!(!command_exists("/nonexistent/definitely/not/here"));
    }

    #[test]
    fn geteuid_is_consistent() {
        // Should be stable across calls within a process.
        assert_eq!(geteuid(), geteuid());
    }

    #[test]
    fn run_privileged_captures_output_and_exit() {
        // Run as root in CI-less envs is unlikely; this exercises whichever
        // branch applies. We only assert on the success path when not needing
        // sudo, since invoking sudo in tests is non-deterministic.
        if geteuid() == 0 {
            let (out, res) = run_privileged("sh", &["-c", "printf hello; printf err >&2"]);
            assert!(res.is_ok());
            // Combined output contains both stdout and stderr bytes.
            let s = String::from_utf8_lossy(&out);
            assert!(s.contains("hello"));
            assert!(s.contains("err"));

            let (_out, res) = run_privileged("sh", &["-c", "exit 3"]);
            let err = res.unwrap_err();
            assert_eq!(err.to_string(), "exit status 3");
        }
    }
}
