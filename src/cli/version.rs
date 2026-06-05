//! `lane version` — print the build version, plain or as JSON.
//!
//! Default output is the byte-identical `lane {VERSION}` line; `--json` emits a
//! minimal, self-describing object `{"name","version"}` (pretty-printed,
//! mirroring `lane list --json` / `lane doctor --json`). Built only from
//! compile-time, Rust-native data (`CARGO_PKG_NAME` + [`crate::VERSION`]).

use anyhow::{Context, Result};
use serde_json::json;

/// Build the version JSON object as a pretty-printed string:
/// `{"name": <CARGO_PKG_NAME>, "version": <crate::VERSION>}`.
///
/// Pure (no I/O) so it is unit-testable without spawning the binary. Mirrors the
/// `serde_json::to_string_pretty(...).context("marshaling JSON")` pattern in
/// `doctor.rs` / `list.rs`.
fn render_json() -> Result<String> {
    let value = json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": crate::VERSION,
    });
    serde_json::to_string_pretty(&value).context("marshaling JSON")
}

/// `lane version [--json]`. With `--json`, prints the pretty version object;
/// otherwise prints the byte-identical `lane {VERSION}` line.
pub async fn run(args: &super::VersionArgs) -> Result<()> {
    if args.json {
        let data = render_json()?;
        println!("{data}");
    } else {
        println!("lane {}", crate::VERSION);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // The pure render helper must parse to exactly `{"name":"lane","version":<crate::VERSION>}`
    // — exactly two string keys, no extras, no nulls — without spawning the binary.
    #[test]
    fn test_render_json_shape() {
        let rendered = render_json().expect("render_json should succeed");
        let value: Value = serde_json::from_str(&rendered).expect("output should be valid JSON");

        let obj = value.as_object().expect("output should be a JSON object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["name", "version"], "exactly two keys: name, version");

        let name = obj.get("name").expect("name key present");
        let version = obj.get("version").expect("version key present");
        assert!(name.is_string(), "name should be a string");
        assert!(version.is_string(), "version should be a string");
        assert_eq!(name.as_str().unwrap(), "lane");
        assert_eq!(version.as_str().unwrap(), crate::VERSION);
    }

    // Pretty-printed output is multi-line, consistent with list/doctor --json.
    #[test]
    fn test_render_json_is_pretty() {
        let rendered = render_json().expect("render_json should succeed");
        assert!(
            rendered.contains('\n'),
            "pretty JSON should be multi-line: {rendered:?}"
        );
    }
}
