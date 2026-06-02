//! Host-file (`/etc/hosts`) management.
//!
//! Faithful port of `internal/system/hostfile.go`. The pure string transforms
//! are factored out (`compute_added`, `compute_removed`, `compute_removed_all`,
//! `has_marked_entry`, `line_has_host`) so they can be unit-tested without
//! touching the filesystem; the public `add_host`/`remove_host`/
//! `remove_all_hosts` wire the real read of `/etc/hosts` plus
//! `write_file_elevated`.

use std::fs;

use anyhow::{Context, Result};

use super::elevated::write_file_elevated;

/// Absolute path of the system hosts file.
pub const HOSTS_PATH: &str = "/etc/hosts";
/// Marker comment appended to every entry this tool manages.
pub const MARKER: &str = "# lane";

/// Add a marked `127.0.0.1` entry for `name` to `/etc/hosts`.
///
/// No-op (returns `Ok`) when a marked entry for `name` already exists, matching
/// Go's `AddHost`.
pub fn add_host(name: &str) -> Result<()> {
    let content = read_hosts()?;

    if has_marked_entry(&content, name) {
        return Ok(());
    }

    let updated = compute_added(&content, name);
    write_file_elevated(HOSTS_PATH, &updated)
}

/// Remove the marked entry for `name` from `/etc/hosts`.
pub fn remove_host(name: &str) -> Result<()> {
    let content = read_hosts()?;
    let updated = compute_removed(&content, name);
    write_file_elevated(HOSTS_PATH, &updated)
}

/// Remove every marked entry from `/etc/hosts`.
pub fn remove_all_hosts() -> Result<()> {
    let content = read_hosts()?;
    let updated = compute_removed_all(&content);
    write_file_elevated(HOSTS_PATH, &updated)
}

/// Read `/etc/hosts`, wrapping errors like Go's `"reading hosts file: %w"`.
fn read_hosts() -> Result<String> {
    let bytes = fs::read(HOSTS_PATH).context("reading hosts file")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Pure transform: append a marked entry for `name` to `content`.
///
/// Mirrors Go: `strings.TrimRight(content, "\n") + "\n" + entry + "\n"` where
/// `entry = "127.0.0.1 <name> # lane"`.
pub fn compute_added(content: &str, name: &str) -> String {
    let entry = format!("127.0.0.1 {} {}", name, MARKER);
    let trimmed = content.trim_end_matches('\n');
    format!("{}\n{}\n", trimmed, entry)
}

/// Pure transform: drop lines that both reference host `name` and carry the
/// marker, joining the remaining lines with `\n` (Go's `strings.Join`).
pub fn compute_removed(content: &str, name: &str) -> String {
    let filtered: Vec<&str> = content
        .split('\n')
        .filter(|line| !(line_has_host(line, name) && line.contains(MARKER)))
        .collect();
    filtered.join("\n")
}

/// Pure transform: drop every line carrying the marker, joining the remaining
/// lines with `\n`.
pub fn compute_removed_all(content: &str) -> String {
    let filtered: Vec<&str> = content
        .split('\n')
        .filter(|line| !line.contains(MARKER))
        .collect();
    filtered.join("\n")
}

/// Report whether `content` contains a marked entry for `hostname`.
pub fn has_marked_entry(content: &str, hostname: &str) -> bool {
    content
        .split('\n')
        .any(|line| line_has_host(line, hostname) && line.contains(MARKER))
}

/// Report whether `line`, split on whitespace, contains `hostname` as a field
/// (Go's `strings.Fields` semantics — splits on runs of any whitespace).
fn line_has_host(line: &str, hostname: &str) -> bool {
    line.split_whitespace().any(|field| field == hostname)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_has_host_cases() {
        // Port of TestLineHasHost.
        let cases: &[(&str, &str, bool)] = &[
            ("127.0.0.1 myapp.test # lane", "myapp.test", true),
            ("127.0.0.1 other.test # lane", "myapp.test", false),
            ("127.0.0.1 myapp.test.extra # lane", "myapp.test", false),
            ("# comment", "myapp.test", false),
            ("", "myapp.test", false),
            ("127.0.0.1\tmyapp.test\t# lane", "myapp.test", true),
        ];
        for (line, hostname, want) in cases {
            assert_eq!(
                line_has_host(line, hostname),
                *want,
                "line_has_host({line:?}, {hostname:?})"
            );
        }
    }

    #[test]
    fn has_marked_entry_cases() {
        // Port of TestHasMarkedEntry.
        let content = "127.0.0.1 localhost\n127.0.0.1 myapp.test # lane\n";
        assert!(
            has_marked_entry(content, "myapp.test"),
            "expected to find marked entry for myapp.test"
        );
        assert!(
            !has_marked_entry(content, "other.test"),
            "did not expect to find marked entry for other.test"
        );
        assert!(
            !has_marked_entry("", "myapp.test"),
            "did not expect to find entry in empty content"
        );
    }

    #[test]
    fn add_appends_marked_entry() {
        // Port of TestAddHostAppendsMarkedEntry (pure-function form): the
        // appended content carries both the hostname and the marker.
        let content = "127.0.0.1 localhost\n";
        let wrote = compute_added(content, "myapp.test");
        assert!(
            wrote.contains("myapp.test") && wrote.contains(MARKER),
            "expected hosts entry to be appended, got {wrote:?}"
        );
    }

    #[test]
    fn add_noop_when_entry_already_exists() {
        // Port of TestAddHostNoopWhenEntryAlreadyExists: the no-op decision is
        // driven by has_marked_entry, which guards the write in add_host.
        let content = "127.0.0.1 myapp.test # lane\n";
        assert!(has_marked_entry(content, "myapp.test"));
    }

    #[test]
    fn remove_removes_only_marked_matching_entry() {
        // Port of TestRemoveHostRemovesOnlyMarkedMatchingEntry.
        let content = [
            "127.0.0.1 localhost",
            "127.0.0.1 myapp.test # lane",
            "127.0.0.1 myapp.test # another-tool",
            "127.0.0.1 api.test # lane",
            "",
        ]
        .join("\n");

        let wrote = compute_removed(&content, "myapp.test");
        assert!(
            !wrote.contains("myapp.test # lane"),
            "expected marked myapp entry to be removed, got {wrote:?}"
        );
        assert!(
            wrote.contains("myapp.test # another-tool"),
            "expected non-marked myapp entry to remain, got {wrote:?}"
        );
        assert!(
            wrote.contains("api.test # lane"),
            "expected other marked entries to remain, got {wrote:?}"
        );
    }

    #[test]
    fn remove_all_removes_all_marked_entries() {
        // Port of TestRemoveAllHostsRemovesAllMarkedEntries.
        let content = [
            "127.0.0.1 localhost",
            "127.0.0.1 myapp.test # lane",
            "127.0.0.1 api.test # lane",
            "127.0.0.1 other.test # another-tool",
            "",
        ]
        .join("\n");

        let wrote = compute_removed_all(&content);
        assert!(
            !wrote.contains("# lane"),
            "expected all lane marked lines removed, got {wrote:?}"
        );
        assert!(
            wrote.contains("other.test # another-tool"),
            "expected unrelated entries to remain, got {wrote:?}"
        );
    }

    // TODO(test-phase): TestHostMutatorsPropagateReadErrors — exercises the
    // /etc/hosts read path; covered by the IO seam, not a pure unit test.
}
