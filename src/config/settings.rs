//! Configuration model, validation, and persistence.
//!
//! Faithful port of `internal/config/config.go`.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::LazyLock;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::paths::{config_path, dir};

/// Domain-label validity: lowercase alphanumeric with internal hyphens.
static VALID_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$").unwrap());

/// Log everything (default).
pub const LOG_MODE_FULL: &str = "full";
/// Log a compact one-line summary per request.
pub const LOG_MODE_MINIMAL: &str = "minimal";
/// Disable access logging.
pub const LOG_MODE_OFF: &str = "off";

/// A path-prefix route forwarding to a local port.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Route {
    pub path: String,
    pub port: u16,
}

/// A mapped local domain and the routes it serves.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Domain {
    pub name: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
}

/// On-disk configuration (`~/.lane/config.yaml`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub domains: Vec<Domain>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub log_mode: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cors: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Append `.test` when the name has no dot, mirroring slim's bare-name shorthand.
pub fn normalize_domain(name: &str) -> String {
    if !name.contains('.') {
        format!("{name}.test")
    } else {
        name.to_string()
    }
}

/// Validate a route path/port pair.
///
/// `port` is `i64` so out-of-range CLI input yields the exact Go error text.
pub fn validate_route(path: &str, port: i64) -> Result<()> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(anyhow!("route path must start with /"));
    }
    if !(1..=65535).contains(&port) {
        return Err(anyhow!(
            "invalid route port {port}: must be between 1 and 65535"
        ));
    }
    Ok(())
}

/// Validate a domain name and its port.
///
/// `port` is `i64` so out-of-range CLI input yields the exact Go error text.
pub fn validate_domain(name: &str, port: i64) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("domain name cannot be empty"));
    }
    if name.len() > 253 {
        return Err(anyhow!(
            "domain name {name:?} is too long: must be 253 characters or fewer"
        ));
    }
    for label in name.split('.') {
        if label.len() > 63 {
            return Err(anyhow!(
                "domain label {label:?} is too long: must be 63 characters or fewer"
            ));
        }
        if !VALID_LABEL.is_match(label) {
            return Err(anyhow!(
                "invalid domain name {name:?}: labels must be lowercase alphanumeric with hyphens"
            ));
        }
    }
    if !(1..=65535).contains(&port) {
        return Err(anyhow!("invalid port {port}: must be between 1 and 65535"));
    }
    Ok(())
}

/// Validate a log-mode string (case/space-insensitive; "" means full).
pub fn validate_log_mode(mode: &str) -> Result<()> {
    match normalize_log_mode(mode).as_str() {
        LOG_MODE_FULL | LOG_MODE_MINIMAL | LOG_MODE_OFF => Ok(()),
        _ => Err(anyhow!(
            "invalid log mode {mode:?}: must be one of full|minimal|off"
        )),
    }
}

fn normalize_log_mode(mode: &str) -> String {
    let mode = mode.trim().to_lowercase();
    if mode.is_empty() {
        LOG_MODE_FULL.to_string()
    } else {
        mode
    }
}

impl Domain {
    /// Longest-prefix path match; falls back to the domain's own port.
    pub fn match_route(&self, req_path: &str) -> u16 {
        let mut best_len = 0usize;
        let mut best_port = self.port;
        for r in &self.routes {
            if r.path.len() <= best_len {
                continue;
            }
            let rp = r.path.as_bytes();
            let req = req_path.as_bytes();
            let matched = req_path == r.path
                || (req_path.starts_with(&r.path)
                    && (rp[rp.len() - 1] == b'/'
                        || (req.len() > rp.len() && req[rp.len()] == b'/')));
            if matched {
                best_len = r.path.len();
                best_port = r.port;
            }
        }
        best_port
    }
}

impl Config {
    /// Resolve the effective log mode ("" -> full).
    pub fn effective_log_mode(&self) -> String {
        normalize_log_mode(&self.log_mode)
    }

    /// Index of the domain with the given name, if present.
    pub fn find_domain(&self, name: &str) -> Option<usize> {
        self.domains.iter().position(|d| d.name == name)
    }

    /// Upsert a domain (replacing port+routes if it exists) and persist.
    pub fn set_domain(&mut self, name: &str, port: u16, routes: Vec<Route>) -> Result<()> {
        if let Some(idx) = self.find_domain(name) {
            self.domains[idx].port = port;
            self.domains[idx].routes = routes;
        } else {
            self.domains.push(Domain {
                name: name.to_string(),
                port,
                routes,
            });
        }
        self.save()
    }

    /// Remove a domain by name and persist; errors if not found.
    pub fn remove_domain(&mut self, name: &str) -> Result<()> {
        match self.find_domain(name) {
            None => Err(anyhow!("domain {name} not found")),
            Some(idx) => {
                self.domains.remove(idx);
                self.save()
            }
        }
    }

    /// Write the config to `~/.lane/config.yaml` (dir 0755, file 0644).
    pub fn save(&self) -> Result<()> {
        mkdir_all_mode(&dir()).context("creating config dir")?;

        let data = serde_yaml::to_string(self).context("marshaling config")?;

        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o644);
        let mut f = opts.open(config_path()).context("writing config")?;
        use std::io::Write;
        f.write_all(data.as_bytes()).context("writing config")?;
        Ok(())
    }
}

/// `mkdir -p` with mode 0755 applied to created components only, matching Go's
/// `os.MkdirAll(dir, 0755)` (existing dirs are left untouched; mode is subject
/// to umask, exactly as in Go).
fn mkdir_all_mode(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(path)
}

/// Load the config from disk; a missing file yields the default.
///
/// Bare domain names are migrated to their normalized (`.test`) form and the
/// config is re-saved when any migration occurred.
pub fn load() -> Result<Config> {
    let data = match fs::read(config_path()) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(anyhow::Error::new(e).context("reading config")),
    };

    let mut cfg: Config = serde_yaml::from_slice(&data).context("parsing config")?;

    let mut migrated = false;
    for d in &mut cfg.domains {
        let normalized = normalize_domain(&d.name);
        if normalized != d.name {
            d.name = normalized;
            migrated = true;
        }
    }
    if migrated {
        let _ = cfg.save();
    }

    Ok(cfg)
}

/// Run `f` while holding an exclusive `flock` on `~/.lane/config.lock`.
pub fn with_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    mkdir_all_mode(&dir()).context("creating config dir")?;

    let lock_path = dir().join("config.lock");
    // Go used O_CREATE|O_RDONLY, which the OS allows; Rust's `OpenOptions`
    // rejects `create` without `write`/`append`, so request write too. We only
    // ever flock the descriptor — the file's contents are irrelevant.
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).mode(0o644);
    let file = opts.open(&lock_path).context("opening lock file")?;

    // Use the fs2 trait methods explicitly: on Rust >= 1.89 `std::fs::File`
    // gained inherent `lock_exclusive`/`unlock`, so fully-qualify through
    // `FileExt` to keep the import used and avoid any method-resolution surprise.
    FileExt::lock_exclusive(&file).context("acquiring config lock")?;
    let result = f();
    let _ = FileExt::unlock(&file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point `HOME` at an isolated temp dir so `config::dir()` resolves there.
    /// Returns the guard `TempDir` (keep it alive for the test's duration).
    fn isolate_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        tmp
    }

    #[test]
    fn test_normalize_domain() {
        let cases = [
            ("myapp", "myapp.test"),
            ("api", "api.test"),
            ("myapp.test", "myapp.test"),
            ("app.loc", "app.loc"),
            ("my.custom.domain", "my.custom.domain"),
            ("app.local", "app.local"),
            ("web.dev", "web.dev"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_domain(input), want, "normalize_domain({input:?})");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_load_migrates_bare_domain_names() {
        let _home = isolate_home();

        let cfg = Config {
            domains: vec![
                Domain {
                    name: "myapp".into(),
                    port: 3000,
                    routes: vec![],
                },
                Domain {
                    name: "api".into(),
                    port: 8080,
                    routes: vec![],
                },
                Domain {
                    name: "app.loc".into(),
                    port: 9000,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };
        cfg.save().expect("save");

        let loaded = load().expect("load");
        assert_eq!(loaded.domains[0].name, "myapp.test");
        assert_eq!(loaded.domains[1].name, "api.test");
        assert_eq!(loaded.domains[2].name, "app.loc");

        let reloaded = load().expect("load after migration");
        assert_eq!(reloaded.domains[0].name, "myapp.test");
    }

    #[test]
    fn test_validate_domain() {
        let long63 = "a".repeat(63);
        let long64 = "a".repeat(64);
        let long_two = format!("{}.{}", "a".repeat(63), "b".repeat(63));
        let cases: &[(&str, i64, bool)] = &[
            ("myapp", 3000, false),
            ("my-app", 8080, false),
            ("a", 1, false),
            ("abc123", 65535, false),
            ("a-b-c", 3000, false),
            ("123", 3000, false),
            ("", 3000, true),
            ("-abc", 3000, true),
            ("abc-", 3000, true),
            ("ABC", 3000, true),
            ("my_app", 3000, true),
            ("my.app", 3000, false),
            ("web.roadmap", 3000, false),
            ("a.b.c", 3000, false),
            ("my..app", 3000, true),
            (".myapp", 3000, true),
            ("myapp.", 3000, true),
            ("web.-bad", 3000, true),
            ("my app", 3000, true),
            (long63.as_str(), 3000, false),
            (long64.as_str(), 3000, true),
            (long_two.as_str(), 3000, false),
            ("myapp", 0, true),
            ("myapp", -1, true),
            ("myapp", 65536, true),
        ];
        for (name, port, want_err) in cases {
            let err = validate_domain(name, *port).is_err();
            assert_eq!(
                err, *want_err,
                "validate_domain({name:?}, {port}) wantErr {want_err}"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_config_lifecycle() {
        let _home = isolate_home();

        let cfg = load().expect("load empty");
        assert_eq!(cfg.domains.len(), 0);

        let mut cfg = cfg;
        cfg.set_domain("myapp.test", 3000, vec![]).expect("set");

        let cfg = load().expect("load after set");
        assert_eq!(cfg.domains.len(), 1);
        assert_eq!(cfg.domains[0].name, "myapp.test");
        assert_eq!(cfg.domains[0].port, 3000);

        assert_eq!(cfg.find_domain("myapp.test"), Some(0));
        assert_eq!(cfg.find_domain("nonexistent"), None);

        let mut cfg = cfg;
        cfg.set_domain("myapp.test", 4000, vec![]).expect("update");
        let cfg = load().expect("reload");
        assert_eq!(cfg.domains[0].port, 4000);

        let mut cfg = cfg;
        cfg.set_domain("api.test", 8080, vec![]).expect("second");
        let cfg = load().expect("reload");
        assert_eq!(cfg.domains.len(), 2);

        let mut cfg = cfg;
        cfg.remove_domain("myapp.test").expect("remove");
        let cfg = load().expect("reload");
        assert_eq!(cfg.domains.len(), 1);
        assert_eq!(cfg.domains[0].name, "api.test");

        let mut cfg = cfg;
        assert!(
            cfg.remove_domain("nonexistent").is_err(),
            "expected error removing nonexistent domain"
        );
    }

    #[test]
    fn test_validate_route() {
        let cases: &[(&str, i64, bool)] = &[
            ("/api", 8080, false),
            ("/", 3000, false),
            ("/api/v1", 9000, false),
            ("", 8080, true),
            ("api", 8080, true),
            ("/api", 0, true),
            ("/api", 65536, true),
        ];
        for (path, port, want_err) in cases {
            let err = validate_route(path, *port).is_err();
            assert_eq!(
                err, *want_err,
                "validate_route({path:?}, {port}) wantErr {want_err}"
            );
        }
    }

    #[test]
    fn test_match_route() {
        let d = Domain {
            name: "myapp".into(),
            port: 3000,
            routes: vec![
                Route {
                    path: "/api".into(),
                    port: 8080,
                },
                Route {
                    path: "/api/v2".into(),
                    port: 9090,
                },
                Route {
                    path: "/ws".into(),
                    port: 9000,
                },
            ],
        };
        let cases: &[(&str, u16)] = &[
            ("/", 3000),
            ("/about", 3000),
            ("/api", 8080),
            ("/api/users", 8080),
            ("/api/v2", 9090),
            ("/api/v2/items", 9090),
            ("/apikeys", 3000),
            ("/ws", 9000),
            ("/ws/chat", 9000),
        ];
        for (req, want) in cases {
            assert_eq!(d.match_route(req), *want, "match_route({req:?})");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_set_domain_with_routes() {
        let _home = isolate_home();

        let mut cfg = load().expect("load");
        let routes = vec![Route {
            path: "/api".into(),
            port: 8080,
        }];
        cfg.set_domain("myapp.test", 3000, routes)
            .expect("set with routes");

        let cfg = load().expect("reload");
        assert_eq!(cfg.domains[0].routes.len(), 1);
        assert_eq!(cfg.domains[0].routes[0].path, "/api");

        let mut cfg = cfg;
        cfg.set_domain("myapp.test", 3000, vec![])
            .expect("clear routes");
        let cfg = load().expect("reload");
        assert_eq!(cfg.domains[0].routes.len(), 0);
    }

    #[test]
    fn test_log_mode() {
        let cfg = Config::default();
        assert_eq!(cfg.effective_log_mode(), LOG_MODE_FULL);

        for mode in ["", "full", "minimal", "off", " Full "] {
            assert!(
                validate_log_mode(mode).is_ok(),
                "validate_log_mode({mode:?}) should be ok"
            );
        }

        assert!(
            validate_log_mode("verbose").is_err(),
            "expected error for invalid log mode"
        );
    }
}
