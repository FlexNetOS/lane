//! Project configuration discovery and validation.
//!
//! Faithful port of `internal/project/project.go`. A project declares its
//! services in a `.lane.yaml` file; `find` walks up the directory tree from the
//! current working directory looking for one, `load` parses and normalizes it,
//! and `discover` combines the two.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;

/// The on-disk project file name.
pub const FILE_NAME: &str = ".lane.yaml";

/// Render a commented starter `.lane.yaml` for `lane config template`, seeded
/// with one example service (`domain` → `port`). The active lines parse through
/// [`load`]; the commented `routes`/options document the full schema. Inspired
/// by consul-template's config scaffolding.
///
/// Pure (no I/O) so it is unit-testable without spawning the binary.
pub fn render_template(domain: &str, port: u16) -> String {
    format!(
        "# .lane.yaml — lane project config. Run `lane up` from this directory.\n\
         # Docs: https://github.com/FlexNetOS/lane/blob/main/docs/configuration.md\n\
         \n\
         services:                  # required; at least one entry\n\
         \x20 - domain: {domain}    # bare label -> {domain}.test; any TLD honored verbatim\n\
         \x20   port: {port}        # upstream localhost port (1-65535)\n\
         \x20   # routes:           # optional; per-path port overrides on this domain\n\
         \x20   #   - path: /api    # must start with \"/\"\n\
         \x20   #     port: 8080\n\
         \x20   #   - path: /ws\n\
         \x20   #     port: 9000\n\
         \n\
         log_mode: full             # optional; full | minimal | off  (default: full)\n\
         cors: false                # optional; default: false\n"
    )
}

/// A single service mapping in a project file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Service {
    pub domain: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<config::Route>,
}

/// The parsed contents of a `.lane.yaml` file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub log_mode: String,
    #[serde(default)]
    pub cors: bool,
}

/// Locate the nearest `.lane.yaml`, walking up from the current directory to
/// the filesystem root.
pub fn find() -> Result<PathBuf> {
    let dir = std::env::current_dir().context("getting working directory")?;
    find_from(&dir)
}

/// Walk up from `start` to the filesystem root looking for `.lane.yaml`.
///
/// Factored out of [`find`] so tests can drive an isolated `TempDir` tree
/// without changing the process working directory.
pub fn find_from(start: &Path) -> Result<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let path = dir.join(FILE_NAME);
        if path.exists() {
            return Ok(path);
        }

        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => {
                return Err(anyhow!(
                    "no {FILE_NAME} found (searched up to filesystem root)"
                ))
            }
        }
    }
}

/// Read and parse a `.lane.yaml` file, normalizing each service domain.
pub fn load(path: &Path) -> Result<ProjectConfig> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    let mut pc: ProjectConfig =
        serde_yaml::from_slice(&data).with_context(|| format!("parsing {}", path.display()))?;

    for svc in &mut pc.services {
        svc.domain = config::normalize_domain(&svc.domain);
    }

    Ok(pc)
}

/// Find and load the nearest project file, returning the config and its path.
pub fn discover() -> Result<(ProjectConfig, PathBuf)> {
    let path = find()?;
    let pc = load(&path)?;
    Ok((pc, path))
}

impl ProjectConfig {
    /// Validate the project: at least one service, a valid log mode (if set),
    /// valid per-service domains, no duplicate domains, and valid routes.
    pub fn validate(&self) -> Result<()> {
        if self.services.is_empty() {
            return Err(anyhow!("no services defined in {FILE_NAME}"));
        }

        if !self.log_mode.is_empty() {
            config::validate_log_mode(&self.log_mode)?;
        }

        let mut seen = std::collections::HashSet::new();
        for svc in &self.services {
            config::validate_domain(&svc.domain, i64::from(svc.port))
                .with_context(|| format!("service {:?}", svc.domain))?;
            if !seen.insert(svc.domain.clone()) {
                return Err(anyhow!("duplicate domain {:?}", svc.domain));
            }

            for r in &svc.routes {
                config::validate_route(&r.path, i64::from(r.port))
                    .with_context(|| format!("service {:?} route {:?}", svc.domain, r.path))?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn test_find() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmp_dir = tmp.path();
        let sub_dir = tmp_dir.join("a").join("b").join("c");
        fs::create_dir_all(&sub_dir).unwrap();

        // Place .lane.yaml in tmp_dir, search from sub_dir.
        let config_path = tmp_dir.join(FILE_NAME);
        fs::write(&config_path, b"services: []\n").unwrap();

        let got = find_from(&sub_dir).expect("find");
        assert_eq!(got, config_path);
    }

    #[test]
    fn test_find_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();

        let err = find_from(tmp.path()).expect_err("expected error when no .lane.yaml found");
        assert!(
            err.to_string().contains("no .lane.yaml found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_and_validate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(FILE_NAME);

        let content = "services:
  - domain: myapp
    port: 3000
    routes:
      - path: /api
        port: 8080
  - domain: dashboard
    port: 5173
log_mode: minimal
";
        fs::write(&path, content).unwrap();

        let pc = load(&path).expect("load");
        assert_eq!(pc.services.len(), 2, "expected 2 services");
        assert_eq!(pc.services[0].domain, "myapp.test");
        assert_eq!(pc.services[0].port, 3000);
        assert_eq!(pc.services[0].routes.len(), 1);
        assert_eq!(pc.services[0].routes[0].path, "/api");
        assert_eq!(pc.log_mode, "minimal");

        pc.validate().expect("validate");
    }

    #[test]
    fn test_load_normalizes_bare_domains() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(FILE_NAME);

        let content = "services:
  - domain: myapp
    port: 3000
  - domain: app.loc
    port: 4000
";
        fs::write(&path, content).unwrap();

        let pc = load(&path).expect("load");
        assert_eq!(
            pc.services[0].domain, "myapp.test",
            "expected bare domain normalized to myapp.test"
        );
        assert_eq!(
            pc.services[1].domain, "app.loc",
            "expected custom TLD preserved as app.loc"
        );
    }

    #[test]
    fn test_validate_duplicate() {
        let pc = ProjectConfig {
            services: vec![
                Service {
                    domain: "myapp".into(),
                    port: 3000,
                    routes: vec![],
                },
                Service {
                    domain: "myapp".into(),
                    port: 4000,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };
        let err = pc
            .validate()
            .expect_err("expected error for duplicate domains");
        assert!(
            err.to_string().contains("duplicate"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_validate_empty_services() {
        let pc = ProjectConfig::default();
        pc.validate()
            .expect_err("expected error for empty services");
    }

    #[test]
    fn test_validate_invalid_route() {
        let pc = ProjectConfig {
            services: vec![Service {
                domain: "myapp".into(),
                port: 3000,
                routes: vec![config::Route {
                    path: "api".into(),
                    port: 8080,
                }],
            }],
            ..Default::default()
        };
        pc.validate()
            .expect_err("expected error for route without leading slash");
    }

    #[test]
    fn test_discover() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(FILE_NAME);

        let content = "services:
  - domain: myapp
    port: 3000
";
        fs::write(&path, content).unwrap();

        // discover() reads cwd via find(); drive the equivalent logic through
        // find_from + load to avoid mutating the process working directory.
        let found_path: PathBuf = find_from(tmp.path()).expect("find");
        assert_eq!(found_path, path);
        let pc = load(&found_path).expect("load");
        assert_eq!(pc.services.len(), 1, "expected 1 service");
    }

    #[test]
    fn test_render_template_round_trips_through_load() {
        let rendered = render_template("myapp", 3000);
        // It is documented (commented) and self-describing.
        assert!(rendered.starts_with("# .lane.yaml"), "{rendered}");
        assert!(rendered.contains("# routes:"), "routes shown as a comment");

        // The active lines load + validate as a real project config.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(FILE_NAME);
        fs::write(&path, &rendered).unwrap();
        let pc = load(&path).expect("rendered template should load");
        pc.validate().expect("rendered template should validate");
        assert_eq!(pc.services.len(), 1);
        assert_eq!(pc.services[0].port, 3000);
        assert_eq!(pc.log_mode, "full");
        assert!(!pc.cors);
        // The commented routes must NOT be active.
        assert!(pc.services[0].routes.is_empty(), "routes are commented out");
    }

    #[test]
    fn test_render_template_seeds_custom_domain_and_port() {
        let rendered = render_template("api.local", 8080);
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(FILE_NAME);
        fs::write(&path, &rendered).unwrap();
        let pc = load(&path).expect("load");
        // Bare-vs-FQDN normalization is `load`'s job; the port is verbatim.
        assert_eq!(pc.services[0].port, 8080);
        assert!(pc.services[0].domain.contains("api.local"));
    }
}
