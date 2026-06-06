//! `lane up` — start all services declared in `.lane.yaml`.
//!
//! Faithful port of `cmd/up.go`. Discovers (or loads via `--config`) the nearest
//! project file, validates it, merges its services into the global config under
//! a file lock, ensures `/etc/hosts` + leaf certs for each service, loads port
//! forwarding, starts or reloads the daemon, and prints the resulting services.

use anyhow::{Context, Result};

use crate::config::{self, Domain};
use crate::daemon::{self, MessageType, Request};
use crate::project::{self, ProjectConfig};
use crate::{cert, setup, system};

use super::{print_services, should_reload_port_forwarding};

/// Run `lane up`.
pub async fn run(args: &super::UpArgs) -> Result<()> {
    // Discover the project file, or load it explicitly when --config is given.
    let (pc, path) = if let Some(cfg_path) = &args.config {
        let pc = project::load(std::path::Path::new(cfg_path))?;
        (pc, std::path::PathBuf::from(cfg_path))
    } else {
        project::discover()?
    };

    pc.validate()?;

    // --json suppresses the human chatter ("Using …" + the services table) so the
    // only thing on stdout is the JSON object emitted at the end.
    if !args.json {
        println!("Using {}", path.display());
    }

    setup::ensure_first_run()?;

    merge_project_into_config(&pc)?;

    for svc in &pc.services {
        system::add_host(&svc.domain)
            .with_context(|| format!("updating /etc/hosts for {}", svc.domain))?;
        cert::ensure_leaf_cert(&svc.domain)
            .with_context(|| format!("generating certificate for {}", svc.domain))?;
    }

    // First port-forwarding reload (skipped inside the detached daemon child).
    if !daemon::is_child() {
        let pf = system::new_port_forwarder();
        if should_reload_port_forwarding(pf.as_ref(), daemon::is_running().await) {
            pf.ensure_loaded()
                .context("loading port forwarding rules")?;
        }
    }

    // Start the daemon if it's down, otherwise tell it to reload. Go discarded
    // the IPC response and surfaced only transport errors, so we do the same.
    if !daemon::is_running().await {
        setup::ensure_proxy_ports_available()?;
        daemon::run_detached().context("starting daemon")?;
        daemon::wait_for_daemon().await?;
    } else {
        daemon::send_ipc(Request {
            msg_type: MessageType::Reload,
            data: None,
        })
        .await
        .context("reloading daemon")?;
    }

    // Second port-forwarding reload now that the daemon is up.
    if !daemon::is_child() {
        let pf = system::new_port_forwarder();
        if should_reload_port_forwarding(pf.as_ref(), true) {
            pf.ensure_loaded()
                .context("loading port forwarding rules")?;
        }
    }

    let domains: Vec<Domain> = pc
        .services
        .iter()
        .map(|svc| Domain {
            name: svc.domain.clone(),
            port: svc.port,
            routes: svc.routes.clone(),
        })
        .collect();

    if args.json {
        let payload = up_json_payload(&path.display().to_string(), &domains);
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("marshaling JSON")?
        );
    } else {
        print_services(&domains);
    }

    Ok(())
}

/// Build the `lane up --json` payload: the resolved config path and the services
/// that were started (each `Domain` serializes as `{name, port, routes?}`).
fn up_json_payload(config_path: &str, domains: &[Domain]) -> serde_json::Value {
    serde_json::json!({
        "config": config_path,
        "started": domains,
    })
}

/// Merge the project config's services into the global config under a file lock.
///
/// Mirrors the `WithLock` closure in `cmd/up.go`: set `cors`, normalize and set
/// `log_mode` when present, upsert each service's domain (replacing port+routes
/// when it already exists), and save.
fn merge_project_into_config(pc: &ProjectConfig) -> Result<()> {
    config::with_lock(|| {
        let mut cfg = config::load()?;
        cfg.cors = pc.cors;
        if !pc.log_mode.is_empty() {
            cfg.log_mode = pc.log_mode.trim().to_lowercase();
        }
        for svc in &pc.services {
            if let Some(idx) = cfg.find_domain(&svc.domain) {
                cfg.domains[idx].port = svc.port;
                cfg.domains[idx].routes = svc.routes.clone();
            } else {
                cfg.domains.push(Domain {
                    name: svc.domain.clone(),
                    port: svc.port,
                    routes: svc.routes.clone(),
                });
            }
        }
        cfg.save()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Route;
    use crate::project::Service;
    use serial_test::serial;

    // `lane up --json` payload carries the config path and the started services;
    // each Domain nests `{name, port, routes?}` (routes omitted when empty).
    #[test]
    fn up_json_payload_has_config_and_started() {
        let domains = vec![
            Domain {
                name: "myapp.test".into(),
                port: 3000,
                routes: vec![],
            },
            Domain {
                name: "api.test".into(),
                port: 8080,
                routes: vec![Route {
                    path: "/v1".into(),
                    port: 9000,
                }],
            },
        ];
        let v = up_json_payload("/tmp/.lane.yaml", &domains);
        assert_eq!(v["config"], "/tmp/.lane.yaml");
        let started = v["started"].as_array().expect("started is array");
        assert_eq!(started.len(), 2);
        assert_eq!(v["started"][0]["name"], "myapp.test");
        assert_eq!(v["started"][0]["port"], 3000);
        // routes omitted when empty (skip_serializing_if on Domain::routes).
        assert!(v["started"][0].get("routes").is_none());
        assert_eq!(v["started"][1]["routes"][0]["path"], "/v1");
        assert_eq!(v["started"][1]["routes"][0]["port"], 9000);
    }

    /// Point HOME at an isolated temp dir so `config::dir()` resolves there.
    fn isolate_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        tmp
    }

    // Port of TestUpStartsDaemonForProjectServices — the config-merge core: two
    // services are upserted into a fresh config.
    #[test]
    #[serial]
    fn merge_writes_project_services() {
        let _home = isolate_home();

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

        merge_project_into_config(&pc).expect("merge");

        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 2, "expected 2 domains");
    }

    // The merge upserts existing domains (replacing port+routes) and appends new
    // ones, leaving unrelated domains untouched.
    #[test]
    #[serial]
    fn merge_upserts_existing_and_appends_new() {
        let _home = isolate_home();

        // Seed an existing config with one of the project's domains plus an
        // unrelated domain.
        let seed = config::Config {
            domains: vec![
                Domain {
                    name: "myapp.test".into(),
                    port: 1111,
                    routes: vec![],
                },
                Domain {
                    name: "other.test".into(),
                    port: 9000,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };
        seed.save().expect("seed save");

        let pc = ProjectConfig {
            services: vec![
                Service {
                    domain: "myapp.test".into(),
                    port: 3000,
                    routes: vec![config::Route {
                        path: "/api".into(),
                        port: 8080,
                    }],
                },
                Service {
                    domain: "new.test".into(),
                    port: 4000,
                    routes: vec![],
                },
            ],
            log_mode: " Minimal ".into(),
            cors: true,
        };

        merge_project_into_config(&pc).expect("merge");

        let cfg = config::load().expect("load");
        assert_eq!(cfg.domains.len(), 3, "expected myapp, other, new");
        assert!(cfg.cors, "cors should be set from project");
        assert_eq!(cfg.log_mode, "minimal", "log_mode normalized");

        let myapp = cfg.find_domain("myapp.test").expect("myapp present");
        assert_eq!(cfg.domains[myapp].port, 3000, "port replaced");
        assert_eq!(cfg.domains[myapp].routes.len(), 1, "routes replaced");

        assert!(cfg.find_domain("other.test").is_some(), "unrelated kept");
        assert!(cfg.find_domain("new.test").is_some(), "new appended");
    }

    // Port of TestUpValidationError — duplicate domains fail validation before
    // any side effects.
    #[test]
    fn validation_rejects_duplicate_domains() {
        let pc = ProjectConfig {
            services: vec![
                Service {
                    domain: "myapp.test".into(),
                    port: 3000,
                    routes: vec![],
                },
                Service {
                    domain: "myapp.test".into(),
                    port: 4000,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };

        let err = pc.validate().expect_err("expected duplicate-domain error");
        assert!(
            err.to_string().contains("duplicate"),
            "unexpected error: {err}"
        );
    }
}
