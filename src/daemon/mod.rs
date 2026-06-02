//! Background daemon: detach, IPC, lifecycle.
//!
//! Faithful port of `internal/daemon/daemon.go`. The CLI starts the proxy by
//! re-executing the current binary with `_LANE_DAEMON=1` in a detached child
//! ([`run_detached`]); the child runs the proxy in the foreground via
//! [`run_foreground`]. The CLI then talks to the daemon over a unix domain
//! socket ([`socket::send_ipc`]).
//!
//! The Go original used `github.com/sevlyar/go-daemon` for the re-exec/detach
//! dance; here we drive it directly with `std::process::Command` plus a
//! `pre_exec` that calls `setsid(2)` and `umask(027)`.

mod protocol;
mod socket;

pub use protocol::*;
pub use socket::*;

use std::io::Read;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config;
use crate::log;
use crate::proxy;

/// True when this process is the re-executed detached daemon child
/// (`_LANE_DAEMON=1`). Mirrors Go's `IsChild` (which used `go-daemon`'s
/// `WasReborn`).
pub fn is_child() -> bool {
    std::env::var("_LANE_DAEMON").as_deref() == Ok("1")
}

/// Return true if the daemon is up: the socket exists and a `status` request
/// succeeds. Mirrors Go's `IsRunning`.
///
/// Async (unlike Go's sync helper) because the CLI runs under `#[tokio::main]`
/// and [`send_ipc`] is async.
pub async fn is_running() -> bool {
    if !config::socket_path().exists() {
        return false;
    }

    match send_ipc(Request {
        msg_type: MessageType::Status,
        data: None,
    })
    .await
    {
        Ok(resp) => resp.ok,
        Err(_) => false,
    }
}

/// Re-exec the current binary as a detached background daemon. Mirrors Go's
/// `RunDetached`.
///
/// Creates `~/.lane`, then spawns the current executable with `_LANE_DAEMON=1`,
/// null stdio, and a `pre_exec` hook that detaches the process (`setsid`) and
/// sets the daemon umask (`027`). The parent returns immediately; it does not
/// wait for the child.
pub fn run_detached() -> Result<()> {
    std::fs::create_dir_all(config::dir())?;

    let exe = std::env::current_exe().context("daemonize")?;

    let mut cmd = std::process::Command::new(exe);
    cmd.env("_LANE_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: only async-signal-safe libc calls run between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            libc::umask(0o027);
            Ok(())
        });
    }

    cmd.spawn().context("daemonize")?;

    // Parent returns immediately; the child detaches and runs the proxy.
    Ok(())
}

/// Wait up to 5 seconds for the daemon to come up. Mirrors Go's
/// `WaitForDaemon`.
///
/// Truncates `~/.lane/daemon.err`, then polls [`is_running`] 50 times at 100ms
/// intervals. On timeout it surfaces the startup error file's contents if
/// present, otherwise a generic timeout error.
pub async fn wait_for_daemon() -> Result<()> {
    let err_path = config::dir().join("daemon.err");
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&err_path);

    for _ in 0..50 {
        if is_running().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if let Ok(mut f) = std::fs::File::open(&err_path) {
        let mut data = String::new();
        if f.read_to_string(&mut data).is_ok() && !data.is_empty() {
            return Err(anyhow!("daemon failed to start: {}", data.trim()));
        }
    }
    Err(anyhow!("daemon failed to start within 5 seconds"))
}

/// Persist a startup error so the launching CLI can surface it
/// (`~/.lane/daemon.err`). Mirrors the Go child's error-file write in
/// `RunDetached`.
pub fn write_startup_error(err: &anyhow::Error) {
    let err_path = config::dir().join("daemon.err");
    let _ = std::fs::write(&err_path, format!("{err:#}\n"));
}

/// The daemon body, run in the foreground of the detached child process.
/// Mirrors Go's `run`.
///
/// Loads config, opens the access log, builds the proxy server, serves the IPC
/// socket, writes the pid file, installs SIGINT/SIGTERM handlers for graceful
/// shutdown, then runs the server until it stops.
pub async fn run_foreground() -> Result<()> {
    let cfg = config::load()?;

    log::set_output(&config::log_path(), &cfg.effective_log_mode()).context("opening log file")?;

    let server = Arc::new(proxy::Server::new(cfg));

    // IPC handler closure dispatches on the message type; Status/Reload do async
    // work, so the handler returns a boxed future.
    let ipc_server = {
        let server = Arc::clone(&server);
        let handler: Handler = Arc::new(move |req: Request| {
            let server = Arc::clone(&server);
            Box::pin(async move { handle_ipc(req, server).await })
        });
        IpcServer::new(handler)?
    };

    tokio::spawn(async move { ipc_server.serve().await });

    std::fs::write(config::pid_path(), std::process::id().to_string())
        .context("writing pid file")?;

    // Graceful shutdown wiring: trigger exactly once on the first signal.
    {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            cleanup(&server).await;
        });
    }

    let result = Arc::clone(&server).start().await;

    // Ensure cleanup runs on normal exit too (idempotent: pid/socket removal).
    cleanup(&server).await;

    result
}

/// Wait for the first SIGINT or SIGTERM. Mirrors Go's `signal.Notify` on
/// `SIGINT, SIGTERM` followed by a single channel receive.
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return,
    };
    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    }
}

/// Graceful shutdown: stop the server, remove the pid file, and remove the
/// socket. Mirrors Go's `cleanup` closure (`sync.Once`-guarded there; here
/// shutdown is idempotent so repeated calls are harmless).
async fn cleanup(server: &Arc<proxy::Server>) {
    server.shutdown().await;
    let _ = std::fs::remove_file(config::pid_path());
    let _ = std::fs::remove_file(config::socket_path());
}

/// Dispatch a single IPC request against the running server. Mirrors Go's
/// `handleIPC`.
async fn handle_ipc(req: Request, server: Arc<proxy::Server>) -> Response {
    match req.msg_type {
        MessageType::Shutdown => {
            // Reuse the signal path so cleanup runs exactly once, matching Go's
            // `p.Signal(syscall.SIGTERM)` to its own pid.
            // SAFETY: kill(2) with our own pid and SIGTERM is sound.
            unsafe {
                libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM);
            }
            Response {
                ok: true,
                error: String::new(),
                data: None,
            }
        }
        MessageType::Status => handle_status().await,
        MessageType::Reload => handle_reload(&server).await,
    }
}

/// Build a status response: load config, assemble domain/route info, probe all
/// upstream ports concurrently, and fill in health. Mirrors Go's `handleStatus`.
async fn handle_status() -> Response {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return err_response(e.to_string()),
    };

    let mut all_ports: Vec<u16> = Vec::new();
    let mut domains: Vec<DomainInfo> = Vec::with_capacity(cfg.domains.len());
    for d in &cfg.domains {
        let mut info = DomainInfo {
            name: d.name.clone(),
            port: d.port,
            healthy: false,
            routes: Vec::new(),
        };
        all_ports.push(d.port);
        for r in &d.routes {
            info.routes.push(RouteInfo {
                path: r.path.clone(),
                port: r.port,
                healthy: false,
            });
            all_ports.push(r.port);
        }
        domains.push(info);
    }

    let health = proxy::check_upstreams(&all_ports).await;
    let mut idx = 0;
    for d in &mut domains {
        d.healthy = health[idx];
        idx += 1;
        for r in &mut d.routes {
            r.healthy = health[idx];
            idx += 1;
        }
    }

    let status = StatusData {
        running: true,
        pid: std::process::id() as i32,
        domains,
    };

    match serde_json::to_value(&status) {
        Ok(data) => Response {
            ok: true,
            error: String::new(),
            data: Some(data),
        },
        Err(e) => err_response(e.to_string()),
    }
}

/// Reload config and re-point the access log. Mirrors Go's `handleReload`.
async fn handle_reload(server: &Arc<proxy::Server>) -> Response {
    let cfg = match server.reload_config().await {
        Ok(c) => c,
        Err(e) => return err_response(e.to_string()),
    };
    if let Err(e) = log::set_output(&config::log_path(), &cfg.effective_log_mode()) {
        return err_response(e.to_string());
    }
    Response {
        ok: true,
        error: String::new(),
        data: None,
    }
}

/// Helper: a failed response carrying `error`.
fn err_response(error: String) -> Response {
    Response {
        ok: false,
        error,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Domain};

    /// Set HOME to an isolated temp dir and create `~/.lane`. Mirrors Go's
    /// `initDaemonStateTestConfig`.
    fn with_isolated_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().expect("TempDir");
        std::env::set_var("HOME", tmp.path());
        std::fs::create_dir_all(config::dir()).expect("MkdirAll config dir");
        tmp
    }

    // Port of TestHandleStatusIncludesDomainHealth.
    #[tokio::test]
    #[serial_test::serial]
    async fn handle_status_includes_domain_health() {
        let _home = with_isolated_home();

        // A healthy upstream: a live listener we keep open.
        let healthy_ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let healthy_port = healthy_ln.local_addr().unwrap().port();

        // An unhealthy upstream: grab a port, then release it.
        let unhealthy_ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unhealthy_port = unhealthy_ln.local_addr().unwrap().port();
        drop(unhealthy_ln);

        let cfg = Config {
            domains: vec![
                Domain {
                    name: "healthy.test".to_string(),
                    port: healthy_port,
                    routes: Vec::new(),
                },
                Domain {
                    name: "unhealthy.test".to_string(),
                    port: unhealthy_port,
                    routes: Vec::new(),
                },
            ],
            ..Default::default()
        };
        cfg.save().expect("Save config");

        let resp = handle_status().await;
        assert!(resp.ok, "expected OK status response, got {resp:?}");

        let status: StatusData =
            serde_json::from_value(resp.data.expect("status data")).expect("Unmarshal status");
        assert!(status.running, "expected running=true, got {status:?}");
        assert_eq!(status.domains.len(), 2, "expected 2 domains");

        let mut health_by_name = std::collections::HashMap::new();
        for d in &status.domains {
            health_by_name.insert(d.name.clone(), d.healthy);
        }

        assert_eq!(
            health_by_name.get("healthy.test"),
            Some(&true),
            "expected healthy domain to be reachable, got {:?}",
            status.domains
        );
        assert_eq!(
            health_by_name.get("unhealthy.test"),
            Some(&false),
            "expected unhealthy domain to be unreachable, got {:?}",
            status.domains
        );

        drop(healthy_ln);
    }

    // Port of TestIsRunningFalseForStaleSocketPath.
    #[tokio::test]
    #[serial_test::serial]
    async fn is_running_false_for_stale_socket_path() {
        let _home = with_isolated_home();

        std::fs::write(config::socket_path(), b"not-a-socket")
            .expect("WriteFile stale socket path");

        assert!(
            !is_running().await,
            "expected is_running to be false for stale non-socket path"
        );
    }

    // TODO(test-phase): TestHandleIPCUnknownMessage — Go constructs an invalid
    // MessageType("unknown"); in Rust MessageType is a closed enum, so an unknown
    // verb fails at deserialization in handle_conn (covered by
    // socket::tests::ipc_server_returns_error_on_invalid_json) rather than in
    // handle_ipc. The dispatch is exhaustive by construction.

    // TODO(test-phase): end-to-end detach/fork (RunDetached + WaitForDaemon) —
    // requires forking a real child process; handled in the integration phase.
}
