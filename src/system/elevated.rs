//! Elevated file writes — write a file directly, falling back to `sudo tee`.
//!
//! Faithful port of `internal/system/elevated.go`:
//!
//! ```go
//! func writeFileElevated(path string, content string) error {
//!     err := os.WriteFile(path, []byte(content), 0644)
//!     if err == nil {
//!         return nil
//!     }
//!     if !os.IsPermission(err) {
//!         return err
//!     }
//!     cmd := exec.Command("sudo", "tee", path)
//!     cmd.Stdin = strings.NewReader(content)
//!     cmd.Stdout = nil
//!     cmd.Stderr = os.Stderr
//!     return cmd.Run()
//! }
//! ```

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

/// Write `content` to `path`. Attempts a direct write with mode `0644` first;
/// on a permission error, retries via `sudo tee <path>` feeding `content` on
/// stdin (discarding stdout, inheriting stderr) exactly like the Go original.
pub fn write_file_elevated(path: &str, content: &str) -> Result<()> {
    match direct_write(path, content) {
        Ok(()) => return Ok(()),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::PermissionDenied {
                return Err(anyhow::Error::new(e));
            }
        }
    }

    // Permission denied: fall back to `sudo tee`.
    let mut cmd = Command::new("sudo");
    cmd.arg("tee")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn().map_err(anyhow::Error::new)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .map_err(anyhow::Error::new)?;
        // Drop stdin to signal EOF to `tee`.
        drop(stdin);
    }

    let status = child.wait().map_err(anyhow::Error::new)?;
    if status.success() {
        Ok(())
    } else {
        match status.code() {
            Some(code) => Err(anyhow!("exit status {}", code)),
            None => Err(anyhow!("signal: killed")),
        }
    }
}

/// Direct write of `content` to `path` with mode `0644`, matching Go's
/// `os.WriteFile(path, []byte(content), 0644)` (create/truncate semantics).
fn direct_write(path: &str, content: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn write_file_elevated_direct_write_success() {
        // Port of TestWriteFileElevatedDirectWriteSuccess.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("hosts.test");
        let path_str = path.to_str().unwrap();
        let content = "127.0.0.1 myapp.test # lane\n";

        write_file_elevated(path_str, content).expect("writeFileElevated");

        let mut got = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut got)
            .unwrap();
        assert_eq!(got, content);
    }

    #[test]
    fn write_file_elevated_returns_non_permission_error() {
        // Port of TestWriteFileElevatedReturnsNonPermissionError: a missing
        // parent directory yields a non-permission error which propagates
        // (it does NOT trigger the sudo fallback).
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing").join("hosts.test");
        let path_str = path.to_str().unwrap();

        let err = write_file_elevated(path_str, "x");
        assert!(err.is_err(), "expected error for missing parent directory");
    }
}
