//! `lane net profile` — in-repo per-host network profiles (ADR-0003 §Decision
//! item 4, "Portability"; §Sequencing P2).
//!
//! The portability payoff of the host-network plane: capture a box's host network
//! model into a **committed in-repo file** (`hosts/<name>.yaml`), so a fresh box
//! reproduces it with `lane net apply --host <name>`. The committed model lives in
//! the repo and travels with it — the "meta is truly portable" win — while the live
//! `/etc/netplan`/NM keyfiles that do *not* travel stay a per-box render target.
//!
//! # What a profile contains (and what it deliberately excludes)
//!
//! A profile is the **durable HOST plane only**. When [`save_profile`] adopts the
//! live host it strips every runtime-managed interface ([`strip_runtime_units`],
//! built on [`crate::net::apply::is_runtime_unit`]): `docker0`/`virbr0`/`br-*`/
//! `veth*`/`lo` are recreated by Docker/libvirt on the new box, **not** by lane, so
//! committing them would make the profile box-specific noise. What remains is the
//! reproducible host plane — exactly what `apply --host` should reconcile.
//!
//! # Layering (pure path always built; live adopt gated)
//!
//! - [`profile_path`], [`write_profile`], [`read_profile`], [`list_profiles`] and
//!   [`strip_runtime_units`] are **pure** filesystem/model helpers — no host access —
//!   so they are built and unit-tested in every build. A profile is a *repo write*
//!   (safe), never a host mutation.
//! - Only [`save_profile`] (which adopts the live host via `nmcli`) takes the
//!   `hostnet` gate and [`live_hostname`] (which reads the box's hostname), mirroring
//!   the "pure core always built, effectful path gated" split of [`crate::net::adopt`]
//!   and [`crate::net::apply`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::net::model::NetworkDocument;

/// The default in-repo directory holding committed per-host profiles.
///
/// Repo-relative: a profile is `hosts/<name>.yaml` under the repo root. Overridable
/// via `lane net profile --profiles-dir <dir>` (so tests write into a `TempDir` and
/// never touch the real repo).
pub const DEFAULT_PROFILES_DIR: &str = "hosts";

/// The path of the committed profile for host `name` under `dir`: `<dir>/<name>.yaml`.
///
/// Pure (no I/O). The base directory is the caller's (the CLI default is
/// [`DEFAULT_PROFILES_DIR`]); the per-host file is always `<name>.yaml` so the
/// committed shape is `hosts/<name>.yaml`.
pub fn profile_path(dir: impl AsRef<Path>, name: &str) -> PathBuf {
    dir.as_ref().join(format!("{name}.yaml"))
}

/// Validate that `name` is a single plain filename component — no path separator, no
/// `..`, not empty, not absolute — so a profile name from the CLI can never escape the
/// profiles directory (`hosts/<name>.yaml`). Returns the validated name on success.
///
/// `name` flows from user input (`profile save <name>`, `apply --host <name>`,
/// `profile show <name>`); without this guard `profile_path` would happily join
/// `"../escape"` and read/write outside the profiles dir. Rejected with a clear,
/// Go-faithful error.
fn validate_profile_name(name: &str) -> Result<&str> {
    if name.is_empty() {
        anyhow::bail!("invalid profile name {name:?}: must not be empty");
    }
    if name == "." || name == ".." || name.contains("..") {
        anyhow::bail!("invalid profile name {name:?}: must not contain \"..\"");
    }
    // A single plain filename component: no `/` (or platform separator) anywhere.
    if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        anyhow::bail!(
            "invalid profile name {name:?}: must be a single filename component, not a path"
        );
    }
    // Defensive: anything Path sees as more than one normal component is rejected too.
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(name),
        _ => anyhow::bail!(
            "invalid profile name {name:?}: must be a single filename component, not a path"
        ),
    }
}

/// Serialize `doc` to the committed profile file `<dir>/<name>.yaml` (creating `dir`
/// if needed), via the model's own serde (`serde_yaml`). Pure I/O — a repo write,
/// never a host mutation.
///
/// Returns the path written, so the caller can confirm it to the user.
pub fn write_profile(dir: impl AsRef<Path>, name: &str, doc: &NetworkDocument) -> Result<PathBuf> {
    validate_profile_name(name)?;
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating profiles directory {}", dir.display()))?;
    let path = profile_path(dir, name);
    let yaml = serde_yaml::to_string(doc).context("serializing host profile to YAML")?;
    std::fs::write(&path, yaml)
        .with_context(|| format!("writing host profile {}", path.display()))?;
    Ok(path)
}

/// Read and deserialize the committed profile `<dir>/<name>.yaml` into a model,
/// via the model's own serde. The round-trip counterpart of [`write_profile`].
pub fn read_profile(dir: impl AsRef<Path>, name: &str) -> Result<NetworkDocument> {
    validate_profile_name(name)?;
    let path = profile_path(dir, name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading host profile {}", path.display()))?;
    let doc: NetworkDocument = serde_yaml::from_str(&raw)
        .with_context(|| format!("parsing host profile {}", path.display()))?;
    Ok(doc)
}

/// List the committed profile names under `dir` (each `<name>.yaml` → `<name>`),
/// sorted. A missing directory is not an error — it lists empty (no profiles yet).
pub fn list_profiles(dir: impl AsRef<Path>) -> Result<Vec<String>> {
    let dir = dir.as_ref();
    let mut names = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // No profiles directory yet ⇒ no profiles (not an error).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(e) => return Err(e).with_context(|| format!("listing profiles in {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Return a copy of `doc` with every **runtime-managed** unit removed — the
/// durable-host-plane filter for a committed profile.
///
/// A unit is runtime-managed when its stable key (NM connection name, else the
/// `match.name` interface) or its bound interface matches
/// [`crate::net::apply::is_runtime_unit`] — `docker0`/`virbr0`/`br-*`/`veth*`/`lo`.
/// Those are recreated by Docker/libvirt on a fresh box, so committing them would
/// make the profile box-specific. This is the **same** exclusion the reconcile
/// applies, so a saved profile re-applied via `--host` is idempotent against the
/// host it was captured from.
pub fn strip_runtime_units(doc: &NetworkDocument) -> NetworkDocument {
    use crate::net::apply::is_runtime_unit;

    let mut out = doc.clone();

    out.network.ethernets.retain(|_id, unit| {
        let key = unit
            .networkmanager
            .as_ref()
            .and_then(|nm| nm.name.clone())
            .or_else(|| unit.match_rule.as_ref().and_then(|m| m.name.clone()));
        let ifname = unit.match_rule.as_ref().and_then(|m| m.name.as_deref());
        // No stable key ⇒ keep (it cannot be a recognized runtime unit by key);
        // still check the interface name.
        !is_runtime_unit(key.as_deref().unwrap_or_default(), ifname)
    });

    out.network.wifis.retain(|_id, unit| {
        let key = unit
            .networkmanager
            .as_ref()
            .and_then(|nm| nm.name.clone())
            .or_else(|| unit.match_rule.as_ref().and_then(|m| m.name.clone()));
        let ifname = unit.match_rule.as_ref().and_then(|m| m.name.as_deref());
        !is_runtime_unit(key.as_deref().unwrap_or_default(), ifname)
    });

    out.network.bridges.retain(|_id, unit| {
        let key = unit
            .networkmanager
            .as_ref()
            .and_then(|nm| nm.name.as_deref());
        // Bridges have no `match`; key on the NM connection name.
        !is_runtime_unit(key.unwrap_or_default(), None)
    });

    out
}

// --- live host capture (feature-gated) -------------------------------------

/// Adopt the live host plane, strip its runtime-managed units, and write the result
/// as the committed profile `<dir>/<name>.yaml`. Returns the path written.
///
/// This is the **save** path: it adopts the current host (reusing
/// [`crate::net::adopt::adopt_all`], which is read-only and sanitizing — no secret
/// material is ever copied), strips runtime units ([`strip_runtime_units`]) so the
/// committed model is the durable HOST plane only, and serializes it to the repo. The
/// adopt is the only host access (a *read*); the write is a repo write, not a host
/// mutation.
#[cfg(feature = "hostnet")]
pub fn save_profile(dir: impl AsRef<Path>, name: &str) -> Result<PathBuf> {
    let adopted = crate::net::adopt::adopt_all().context("adopting host network plane")?;
    let host_plane = strip_runtime_units(&adopted);
    write_profile(dir, name, &host_plane)
}

/// The live box's hostname — the default profile name for `lane net profile save`.
///
/// Reads it via the libc `gethostname(2)` wrapper (the same "tiny libc wrapper"
/// convention lane already uses for `geteuid`/`setsid`), so no new dependency and no
/// `hostname` subprocess. Gated with the rest of the live-host path.
#[cfg(feature = "hostnet")]
pub fn live_hostname() -> Result<String> {
    // POSIX HOST_NAME_MAX is 255; +1 for the NUL terminator.
    let mut buf = vec![0u8; 256];
    // SAFETY: `gethostname` writes at most `buf.len()` bytes into the buffer and
    // NUL-terminates when it fits; the pointer/length are valid for `buf`.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        anyhow::bail!("gethostname failed: {}", std::io::Error::last_os_error());
    }
    // Truncate at the first NUL, then decode as UTF-8.
    let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    let name = String::from_utf8(buf).context("hostname was not valid UTF-8")?;
    if name.is_empty() {
        anyhow::bail!("host has an empty hostname; pass an explicit profile name");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::{
        BridgeUnit, EthernetUnit, MatchRule, Network, NmPassthrough, Renderer,
    };
    use tempfile::TempDir;

    /// Build the cognitum-seed link-local ethernet unit as a one-connection model
    /// (the adoption snapshot shape).
    fn cognitum_seed_doc() -> NetworkDocument {
        let mut passthrough = std::collections::BTreeMap::new();
        passthrough.insert("ipv4.never-default".to_string(), "true".to_string());
        passthrough.insert("ipv6.method".to_string(), "link-local".to_string());

        let eth = EthernetUnit {
            renderer: Some(Renderer::NetworkManager),
            match_rule: Some(MatchRule {
                name: Some("enxead865c61ec9".to_string()),
                macaddress: None,
            }),
            addresses: vec!["169.254.42.2/24".to_string()],
            dhcp4: Some(false),
            networkmanager: Some(NmPassthrough {
                name: Some("cognitum-seed-linklocal".to_string()),
                uuid: Some("70b82336-d3cd-4204-90aa-fe8a1ed5e769".to_string()),
                passthrough,
            }),
            ..EthernetUnit::default()
        };

        let mut network = Network {
            renderer: Some(Renderer::NetworkManager),
            ..Network::v2()
        };
        network
            .ethernets
            .insert("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769".to_string(), eth);
        NetworkDocument::new(network)
    }

    #[test]
    fn profile_path_is_dir_slash_name_yaml() {
        let p = profile_path("hosts", "trx50");
        assert_eq!(p, PathBuf::from("hosts/trx50.yaml"));
    }

    /// Acceptance (1) — the reproduce loop's round-trip: write a model via the
    /// profile path into a TempDir, read it back, assert equality. No real repo write.
    #[test]
    fn save_round_trips_model_equal() {
        let tmp = TempDir::new().unwrap();
        let doc = cognitum_seed_doc();

        let path = write_profile(tmp.path(), "trx50", &doc).unwrap();
        assert_eq!(path, profile_path(tmp.path(), "trx50"));
        assert!(path.exists(), "profile file must be written");

        let back = read_profile(tmp.path(), "trx50").unwrap();
        assert_eq!(doc, back, "profile must round-trip equal (no field loss)");
    }

    /// Acceptance (2) — the saved profile excludes runtime bridges. A model carrying
    /// docker0/virbr0/br-*/veth* + the durable cognitum-seed unit strips to the
    /// host plane only.
    #[test]
    fn strip_excludes_runtime_units_keeps_host_plane() {
        let mut doc = cognitum_seed_doc();

        // Add runtime-managed bridges (Docker/libvirt) + a veth ethernet.
        for name in ["docker0", "virbr0", "br-abc123"] {
            doc.network.bridges.insert(
                format!("NM-{name}"),
                BridgeUnit {
                    networkmanager: Some(NmPassthrough {
                        name: Some(name.to_string()),
                        ..NmPassthrough::default()
                    }),
                    ..BridgeUnit::default()
                },
            );
        }
        doc.network.ethernets.insert(
            "NM-veth".to_string(),
            EthernetUnit {
                match_rule: Some(MatchRule {
                    name: Some("veth9f3c1a".to_string()),
                    macaddress: None,
                }),
                ..EthernetUnit::default()
            },
        );

        let stripped = strip_runtime_units(&doc);

        // Every runtime bridge is gone.
        assert!(
            stripped.network.bridges.is_empty(),
            "runtime bridges must be stripped from a committed profile, got {:?}",
            stripped.network.bridges.keys().collect::<Vec<_>>()
        );
        // The veth ethernet is gone; the durable host unit remains.
        assert_eq!(
            stripped.network.ethernets.len(),
            1,
            "only the durable host plane unit should remain"
        );
        assert!(stripped
            .network
            .ethernets
            .contains_key("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769"));

        // And the saved file likewise carries no runtime-unit names.
        let tmp = TempDir::new().unwrap();
        write_profile(tmp.path(), "trx50", &stripped).unwrap();
        let yaml = std::fs::read_to_string(profile_path(tmp.path(), "trx50")).unwrap();
        assert!(!yaml.contains("docker0"));
        assert!(!yaml.contains("virbr0"));
        assert!(!yaml.contains("br-abc123"));
        assert!(!yaml.contains("veth9f3c1a"));
        assert!(yaml.contains("cognitum-seed-linklocal"));
    }

    /// Acceptance (3) — reproduce idempotence: `apply` loading a `--host` profile that
    /// equals the current adopted (runtime-stripped) state yields an EMPTY plan.
    #[test]
    fn apply_host_profile_equal_to_current_is_empty_plan() {
        use crate::net::apply::reconcile;

        let tmp = TempDir::new().unwrap();

        // The "adopted current" host (with a runtime bridge that gets stripped on save).
        let mut adopted = cognitum_seed_doc();
        adopted.network.bridges.insert(
            "NM-docker0".to_string(),
            BridgeUnit {
                networkmanager: Some(NmPassthrough {
                    name: Some("docker0".to_string()),
                    ..NmPassthrough::default()
                }),
                ..BridgeUnit::default()
            },
        );

        // Save = strip runtime + write; this is what `profile save` commits.
        let host_plane = strip_runtime_units(&adopted);
        write_profile(tmp.path(), "trx50", &host_plane).unwrap();

        // Reproduce: load the committed `--host` profile as the desired model and
        // reconcile against the same adopted host → EMPTY plan (idempotent), because
        // the reconcile excludes the runtime bridge on the current side too.
        let desired = read_profile(tmp.path(), "trx50").unwrap();
        let plan = reconcile(&desired, &adopted);
        assert!(
            plan.is_empty(),
            "a host profile equal to current adopted state must yield an empty plan, got:\n{}",
            plan.render_text()
        );
    }

    #[test]
    fn list_profiles_lists_committed_yaml_names_sorted() {
        let tmp = TempDir::new().unwrap();
        write_profile(tmp.path(), "zeta", &cognitum_seed_doc()).unwrap();
        write_profile(tmp.path(), "alpha", &cognitum_seed_doc()).unwrap();
        // A non-yaml sibling must be ignored.
        std::fs::write(tmp.path().join("README.md"), "not a profile").unwrap();

        let names = list_profiles(tmp.path()).unwrap();
        assert_eq!(names, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    /// Path-traversal hardening: a profile name containing `..` or a path separator is
    /// rejected, so `read_profile`/`write_profile` (and thus save/show/apply) can never
    /// escape the profiles directory.
    #[test]
    fn profile_name_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let doc = cognitum_seed_doc();

        for bad in ["../escape", "a/b", "..", "sub/../escape", ""] {
            let w = write_profile(tmp.path(), bad, &doc);
            assert!(
                w.is_err(),
                "write_profile must reject malicious name {bad:?}"
            );
            let r = read_profile(tmp.path(), bad);
            assert!(
                r.is_err(),
                "read_profile must reject malicious name {bad:?}"
            );
        }

        // The error names the offending input (Go-faithful, helpful message).
        let err = write_profile(tmp.path(), "../escape", &doc).unwrap_err();
        assert!(
            err.to_string().contains("invalid profile name"),
            "error must explain the rejection, got: {err}"
        );

        // A plain name still works (the guard is not over-broad).
        assert!(write_profile(tmp.path(), "trx50", &doc).is_ok());
        assert!(read_profile(tmp.path(), "trx50").is_ok());
    }

    /// The guard must not have written anything outside the profiles dir for a
    /// traversal attempt — a rejected name leaves no file on disk.
    #[test]
    fn rejected_name_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("escape.yaml");
        // `../escape` from inside a `hosts` subdir would land at `outside`.
        let dir = tmp.path().join("hosts");
        let _ = write_profile(&dir, "../escape", &cognitum_seed_doc());
        assert!(
            !outside.exists(),
            "a rejected traversal name must not create a file outside the profiles dir"
        );
    }

    #[test]
    fn list_profiles_missing_dir_is_empty_not_error() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-such-hosts-dir");
        let names = list_profiles(&missing).unwrap();
        assert!(
            names.is_empty(),
            "missing profiles dir lists empty, not error"
        );
    }
}
