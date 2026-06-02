//! `lane down` — stop the services declared in `.lane.yaml`.
//!
//! Faithful port of `cmd/down.go`. Discovers (or loads via `--config`) the
//! project file, validates it, removes only its domains from the global config
//! (leaving other domains running), removes each `/etc/hosts` entry, then either
//! shuts the daemon down (no domains left) or reloads it.

use std::collections::HashSet;

use anyhow::{Context, Result};

use crate::config;
use crate::daemon::{self, MessageType, Request};
use crate::project::{self, ProjectConfig};
use crate::system;

/// Run `lane down`.
pub async fn run(args: &super::DownArgs) -> Result<()> {
    // Discover the project file, or load it explicitly when --config is given.
    let pc = if let Some(cfg_path) = &args.config {
        project::load(std::path::Path::new(cfg_path))?
    } else {
        let (pc, _path) = project::discover()?;
        pc
    };

    pc.validate()?;

    let remaining_domains = remove_project_domains(&pc)?;

    for svc in &pc.services {
        if let Err(e) = system::remove_host(&svc.domain) {
            println!(
                "Warning: failed to remove {} from /etc/hosts: {}",
                svc.domain, e
            );
        }
    }

    if daemon::is_running().await {
        let msg_type = down_ipc_type(remaining_domains);
        let context = if remaining_domains == 0 {
            "stopping daemon"
        } else {
            "reloading daemon"
        };
        daemon::send_ipc(Request {
            msg_type,
            data: None,
        })
        .await
        .context(context)?;
    }

    println!("Stopped {} project service(s).", pc.services.len());
    Ok(())
}

/// Remove every domain named by the project's services from the global config
/// under a file lock, returning the number of domains that remain.
///
/// Mirrors the `WithLock` closure in `cmd/down.go`.
fn remove_project_domains(pc: &ProjectConfig) -> Result<usize> {
    config::with_lock(|| {
        let mut cfg = config::load()?;
        let remove: HashSet<&str> = pc.services.iter().map(|s| s.domain.as_str()).collect();
        cfg.domains.retain(|d| !remove.contains(d.name.as_str()));
        let remaining = cfg.domains.len();
        cfg.save()?;
        Ok(remaining)
    })
}

/// Choose the IPC message for `down`: shut the daemon down when no domains
/// remain, otherwise reload it. Mirrors the branch in `cmd/down.go`.
fn down_ipc_type(remaining_domains: usize) -> MessageType {
    if remaining_domains == 0 {
        MessageType::Shutdown
    } else {
        MessageType::Reload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Domain;
    use crate::project::Service;
    use serial_test::serial;

    /// Point HOME at an isolated temp dir so `config::dir()` resolves there.
    fn isolate_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        tmp
    }

    /// Seed the on-disk config with the given domains. Mirrors the Go test
    /// helper `seedDomains`.
    fn seed_domains(domains: Vec<Domain>) {
        let cfg = config::Config {
            domains,
            ..Default::default()
        };
        cfg.save().expect("seed save");
    }

    // Port of TestDownRemovesProjectServices — only project domains are removed;
    // an unrelated domain remains, so the IPC type is reload.
    #[test]
    #[serial]
    fn removes_only_project_services() {
        let _home = isolate_home();

        seed_domains(vec![
            Domain {
                name: "myapp.test".into(),
                port: 3000,
                routes: vec![],
            },
            Domain {
                name: "api.test".into(),
                port: 8080,
                routes: vec![],
            },
            Domain {
                name: "other.test".into(),
                port: 9000,
                routes: vec![],
            },
        ]);

        let pc = ProjectConfig {
            services: vec![
                Service {
                    domain: "myapp.test".into(),
                    port: 3000,
                    routes: vec![],
                },
                Service {
                    domain: "api.test".into(),
                    port: 8080,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };

        let remaining = remove_project_domains(&pc).expect("remove");
        assert_eq!(remaining, 1, "expected one domain to remain");
        assert_eq!(
            down_ipc_type(remaining),
            MessageType::Reload,
            "expected reload IPC (other domain remains)"
        );

        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 1, "expected only 'other' to remain");
        assert_eq!(cfg.domains[0].name, "other.test");
    }

    // Port of TestDownShutdownsWhenNoDomains — removing the last domain yields a
    // shutdown IPC.
    #[test]
    #[serial]
    fn shuts_down_when_no_domains_remain() {
        let _home = isolate_home();

        seed_domains(vec![Domain {
            name: "myapp.test".into(),
            port: 3000,
            routes: vec![],
        }]);

        let pc = ProjectConfig {
            services: vec![Service {
                domain: "myapp.test".into(),
                port: 3000,
                routes: vec![],
            }],
            ..Default::default()
        };

        let remaining = remove_project_domains(&pc).expect("remove");
        assert_eq!(remaining, 0, "expected no domains to remain");
        assert_eq!(
            down_ipc_type(remaining),
            MessageType::Shutdown,
            "expected shutdown IPC"
        );

        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 0);
    }
}
