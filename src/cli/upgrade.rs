//! `lane upgrade` — upgrade lane to the latest released version.
//!
//! Faithful port of `cmd/upgrade.go`. Resolves the latest tag from the GitHub
//! `releases/latest` redirect, and (when newer than the running build)
//! downloads the platform archive + `checksums.txt`, verifies the SHA-256,
//! extracts the `lane` binary, and replaces the current executable in place
//! (falling back to `sudo install` on a permission error).
//!
//! The Go original drove all four actions through `term.RunSteps`, whose
//! `Step.Run` is synchronous. The network downloads here use the async
//! `reqwest` client, so the "Downloading archive" and "Verifying checksum"
//! actions are performed in the async body with their step-style lines printed
//! manually; only the synchronous "Extracting" and (interactive) "Replacing
//! binary" actions are driven through [`crate::term::step::run_steps`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::term::{self, step::Step};

/// The upstream release repository (`owner/name`). Patched to the real repo at
/// release time; see ARCHITECTURE.md's release-artifacts section.
const REPO: &str = "drdave-flexnetos/lane";

/// Run the upgrade flow.
pub async fn run() -> Result<()> {
    let tag = latest_tag(REPO)
        .await
        .map_err(|e| anyhow!("failed to check latest version: {e}"))?;
    let latest = tag.strip_prefix('v').unwrap_or(&tag).to_string();

    if latest == crate::VERSION {
        println!("\nAlready up to date ({})\n", crate::VERSION);
        return Ok(());
    }
    println!("\nUpdating {} → {latest}\n", crate::VERSION);

    let exe = std::env::current_exe().context("failed to locate current binary")?;
    let exe = std::fs::canonicalize(&exe).context("failed to resolve binary path")?;

    let tmp_dir = mkdtemp("lane-upgrade-").context("failed to create temp directory")?;
    // Best-effort cleanup of the temp dir on the way out (mirrors Go's defer).
    let _cleanup = TmpDirGuard(tmp_dir.clone());

    let filename = format!("lane_{latest}_{}_{}.tar.gz", goos(), goarch());
    let archive_url = format!("https://github.com/{REPO}/releases/download/{tag}/{filename}");
    let checksum_url = format!("https://github.com/{REPO}/releases/download/{tag}/checksums.txt");
    let archive_path = tmp_dir.join(&filename);
    let binary_path = tmp_dir.join("lane");

    // "Downloading archive" — performed async, printed in step style.
    run_async_step("Downloading archive", || async {
        download_file(&archive_url, &archive_path).await
    })
    .await?;

    // "Verifying checksum" — performed async, printed in step style.
    run_async_step("Verifying checksum", || async {
        verify_checksum(&checksum_url, &archive_path, &filename).await
    })
    .await?;

    // The remaining actions are synchronous; drive them through run_steps so the
    // interactive "Replacing binary" step keeps its dim `· name` rendering.
    let archive_path_extract = archive_path.clone();
    let binary_path_extract = binary_path.clone();
    let binary_path_replace = binary_path.clone();
    let exe_replace = exe.clone();
    term::step::run_steps(vec![
        Step {
            name: "Extracting".to_string(),
            run: Box::new(move || {
                extract_binary(&archive_path_extract, &binary_path_extract)
                    .map(|()| "done".to_string())
            }),
            interactive: false,
        },
        Step {
            name: "Replacing binary".to_string(),
            interactive: true,
            run: Box::new(move || {
                replace_binary(&binary_path_replace, &exe_replace).map(|()| "done".to_string())
            }),
        },
    ])?;

    println!("\nUpgraded to {latest}");
    Ok(())
}

/// Run an async action and print a `run_steps`-style success/failure line.
///
/// On success prints `"{check} {name}"`; on error prints `"{cross} {name}"` and
/// returns the error, matching `term::step::run_step`'s behavior (the Go steps'
/// "done"/"ok" status labels are not displayed for non-`skipped` results) so the
/// manual download steps are indistinguishable from spinner-driven ones.
async fn run_async_step<F, Fut>(name: &str, f: F) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    match f().await {
        Ok(()) => {
            println!("{} {name}", term::check_mark());
            Ok(())
        }
        Err(e) => {
            println!("{} {name}", term::cross_mark());
            Err(e)
        }
    }
}

/// Resolve the latest release tag from the `releases/latest` redirect.
///
/// Mirrors Go's `latestTag`: issue a request with redirects disabled and read
/// the `Location` header, which points at `…/releases/tag/{tag}`.
async fn latest_tag(repo: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let resp = client
        .get(format!("https://github.com/{repo}/releases/latest"))
        .send()
        .await?;

    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if loc.is_empty() {
        return Err(anyhow!("no redirect from releases/latest"));
    }

    let parts: Vec<&str> = loc.split("/tag/").collect();
    if parts.len() != 2 {
        return Err(anyhow!("unexpected redirect URL: {loc}"));
    }

    Ok(parts[1].to_string())
}

/// Download `url` to `dest`. Mirrors Go's `downloadFile`.
async fn download_file(url: &str, dest: &Path) -> Result<()> {
    let resp = reqwest::get(url).await?;

    let status = resp.status();
    if status.as_u16() != 200 {
        return Err(anyhow!("HTTP {}", status.as_u16()));
    }

    let body = resp.bytes().await?;
    let mut f = std::fs::File::create(dest)?;
    f.write_all(&body)?;
    f.flush()?;
    Ok(())
}

/// Download the `checksums.txt`, find the line for `filename`, and verify the
/// SHA-256 of `file_path`. Mirrors Go's `verifyChecksum`.
async fn verify_checksum(checksum_url: &str, file_path: &Path, filename: &str) -> Result<()> {
    let resp = reqwest::get(checksum_url)
        .await
        .map_err(|e| anyhow!("failed to download checksums: {e}"))?;

    let status = resp.status();
    if status.as_u16() != 200 {
        return Err(anyhow!(
            "failed to download checksums: HTTP {}",
            status.as_u16()
        ));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("failed to read checksums: {e}"))?;

    let mut expected_hash = String::new();
    for line in body.split('\n') {
        if line.trim_end().ends_with(filename) {
            if let Some(field) = line.split_whitespace().next() {
                expected_hash = field.to_string();
                break;
            }
        }
    }
    if expected_hash.is_empty() {
        return Err(anyhow!("checksum not found for {filename}"));
    }

    let mut f = std::fs::File::open(file_path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual_hash = hex::encode(hasher.finalize());

    if actual_hash != expected_hash {
        return Err(anyhow!("expected {expected_hash}, got {actual_hash}"));
    }

    Ok(())
}

/// Extract the single `lane` regular-file entry from the gzip-compressed tar
/// archive at `archive_path` to `dest_path` (mode `0755`). Mirrors Go's
/// `extractBinary`.
fn extract_binary(archive_path: &Path, dest_path: &Path) -> Result<()> {
    let f = std::fs::File::open(archive_path)?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(gz);

    for entry in ar.entries()? {
        let mut entry = entry?;
        let header = entry.header();

        // Only a regular file named exactly "lane" (by basename) qualifies.
        if header.entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry.path()?;
        let is_lane = path
            .file_name()
            .map(|n| n == std::ffi::OsStr::new("lane"))
            .unwrap_or(false);
        if !is_lane {
            continue;
        }

        let mut out = open_executable(dest_path)?;
        std::io::copy(&mut entry, &mut out)?;
        out.flush()?;
        return Ok(());
    }

    Err(anyhow!("lane binary not found in archive"))
}

/// Replace the binary at `dst_path` with `src_path`, atomically via a temp file
/// in the destination directory + rename. On a permission error (creating the
/// temp file or renaming) fall back to `sudo install`. Mirrors Go's
/// `replaceBinary`.
fn replace_binary(src_path: &Path, dst_path: &Path) -> Result<()> {
    let dir = dst_path.parent().unwrap_or_else(|| Path::new("."));

    let (tmp_path, mut tmp) = match create_temp_in(dir, ".lane-upgrade-") {
        Ok(v) => v,
        Err(e) => {
            if !is_permission_denied(&e) {
                return Err(anyhow!("failed to replace binary: {e}"));
            }
            return replace_binary_sudo(src_path, dst_path);
        }
    };
    // Best-effort removal of the temp file (mirrors Go's defer os.Remove).
    let _tmp_guard = TmpFileGuard(tmp_path.clone());

    let mut in_file = std::fs::File::open(src_path)?;
    std::io::copy(&mut in_file, &mut tmp)?;
    set_mode_0755(&tmp_path)?;
    tmp.flush()?;
    drop(tmp);

    match std::fs::rename(&tmp_path, dst_path) {
        Ok(()) => Ok(()),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::PermissionDenied {
                return Err(anyhow!("failed to replace binary: {e}"));
            }
            replace_binary_sudo(src_path, dst_path)
        }
    }
}

/// Replace the binary via `sudo install -m 0755 src dst`, inheriting stdio.
/// Mirrors Go's `replaceBinarySudo`.
fn replace_binary_sudo(src_path: &Path, dst_path: &Path) -> Result<()> {
    let status = Command::new("sudo")
        .arg("install")
        .arg("-m")
        .arg("0755")
        .arg(src_path)
        .arg(dst_path)
        .status()
        .map_err(|e| anyhow!("failed to replace binary with sudo: {e}"))?;
    if !status.success() {
        let code = status
            .code()
            .map(|c| format!("exit status {c}"))
            .unwrap_or_else(|| "signal: killed".to_string());
        return Err(anyhow!("failed to replace binary with sudo: {code}"));
    }
    Ok(())
}

// --- platform mapping -------------------------------------------------------

/// Rust OS string mapped to the release artifact's OS token (`macos`→`darwin`).
fn goos() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// Rust arch string mapped to the release artifact's arch token.
fn goarch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

// --- filesystem helpers -----------------------------------------------------

/// Create a unique temp directory under the system temp dir whose name starts
/// with `prefix`, mirroring Go's `os.MkdirTemp("", prefix+"*")`.
fn mkdtemp(prefix: &str) -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..1000 {
        let candidate = base.join(format!("{prefix}{:016x}", rand::random::<u64>()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create temp directory",
    ))
}

/// Create a unique temp file in `dir` whose name starts with `prefix`, returning
/// its path and an open writable handle. Mirrors Go's `os.CreateTemp(dir, …)`.
fn create_temp_in(dir: &Path, prefix: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::fs::OpenOptions;
    for _ in 0..1000 {
        let candidate = dir.join(format!("{prefix}{:016x}", rand::random::<u64>()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create temp file",
    ))
}

/// Open `path` for writing with mode `0755`, truncating, mirroring Go's
/// `os.OpenFile(_, O_CREATE|O_WRONLY|O_TRUNC, 0755)`.
fn open_executable(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)
}

/// Set `path`'s permissions to `0755`. Mirrors Go's `tmp.Chmod(0755)`.
fn set_mode_0755(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// True when `err` is a permission-denied I/O error, mirroring Go's
/// `os.IsPermission`.
fn is_permission_denied(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
}

/// Removes a temp directory tree on drop (best effort).
struct TmpDirGuard(PathBuf);
impl Drop for TmpDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Removes a temp file on drop (best effort).
struct TmpFileGuard(PathBuf);
impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goos_maps_macos_to_darwin() {
        // Functional parity for the OS token mapping used in artifact names.
        match std::env::consts::OS {
            "macos" => assert_eq!(goos(), "darwin"),
            "linux" => assert_eq!(goos(), "linux"),
            other => assert_eq!(goos(), other),
        }
    }

    #[test]
    fn goarch_maps_known_arches() {
        match std::env::consts::ARCH {
            "x86_64" => assert_eq!(goarch(), "amd64"),
            "aarch64" => assert_eq!(goarch(), "arm64"),
            other => assert_eq!(goarch(), other),
        }
    }

    #[test]
    fn latest_strip_v_prefix() {
        // Mirrors `strings.TrimPrefix(tag, "v")`.
        assert_eq!("v1.2.3".strip_prefix('v').unwrap_or("v1.2.3"), "1.2.3");
        assert_eq!("1.2.3".strip_prefix('v').unwrap_or("1.2.3"), "1.2.3");
    }

    // Build a gzip-tar archive containing a single regular file named `name`
    // with `contents`, and return its bytes.
    fn build_archive(name: &str, contents: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let gz = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, name, contents)
            .expect("append");
        let gz = builder.into_inner().expect("finish tar");
        gz.finish().expect("finish gz")
    }

    #[test]
    fn extract_binary_finds_lane() {
        let tmp = TempArchiveDir::new();
        let archive = tmp.path().join("a.tar.gz");
        std::fs::write(&archive, build_archive("lane", b"#!/bin/true\n")).unwrap();

        let dest = tmp.path().join("out");
        extract_binary(&archive, &dest).expect("extract");

        assert_eq!(std::fs::read(&dest).unwrap(), b"#!/bin/true\n");
    }

    #[test]
    fn extract_binary_missing_errors() {
        let tmp = TempArchiveDir::new();
        let archive = tmp.path().join("a.tar.gz");
        // Archive contains a different binary -> "lane binary not found".
        std::fs::write(&archive, build_archive("notlane", b"x")).unwrap();

        let dest = tmp.path().join("out");
        let err = extract_binary(&archive, &dest).expect_err("expected missing-binary error");
        assert_eq!(err.to_string(), "lane binary not found in archive");
    }

    #[test]
    fn replace_binary_atomic_rename() {
        let tmp = TempArchiveDir::new();
        let src = tmp.path().join("new-lane");
        let dst = tmp.path().join("installed-lane");
        std::fs::write(&src, b"new\n").unwrap();
        std::fs::write(&dst, b"old\n").unwrap();

        replace_binary(&src, &dst).expect("replace");
        assert_eq!(std::fs::read(&dst).unwrap(), b"new\n");

        // The replaced binary is executable (0755).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    /// A self-cleaning temp directory for the file-based tests above.
    struct TempArchiveDir(PathBuf);
    impl TempArchiveDir {
        fn new() -> Self {
            TempArchiveDir(mkdtemp("lane-upgrade-test-").expect("mkdtemp"))
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempArchiveDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
