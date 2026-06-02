//! Unix-domain-socket IPC transport between the CLI and the daemon.
//!
//! Faithful port of `internal/daemon/socket.go`. The Go version used
//! `json.NewDecoder(conn).Decode` (which reads exactly one JSON value, leaving
//! the connection half-open) together with `json.NewEncoder(conn).Encode`.
//!
//! In the async port the client writes the request, shuts down its write half
//! (signalling EOF), the server reads the request to EOF, runs the handler, and
//! writes the JSON response. This is equivalent for a one-shot request/response
//! exchange and avoids the framing ambiguity of a streaming decoder.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::config;

use super::protocol::{Request, Response};

/// Connect timeout when dialing the daemon socket (mirrors Go's 5s).
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-connection deadline on the server side (mirrors Go's 30s `SetDeadline`).
const CONN_DEADLINE: Duration = Duration::from_secs(30);

/// Send a single request to the running daemon and return its response.
///
/// Mirrors Go's `SendIPC`: dial the unix socket (5s connect timeout), write the
/// JSON request, then read and parse the JSON response. A connection failure is
/// wrapped with the same human-facing hint as the Go original.
pub async fn send_ipc(req: Request) -> Result<Response> {
    let sock_path = config::socket_path();

    let connect = UnixStream::connect(&sock_path);
    let mut conn = match tokio::time::timeout(DIAL_TIMEOUT, connect).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => {
            return Err(crate::httperr::wrap(
                "connecting to daemon (is lane running?)",
                e,
            ));
        }
        Err(_elapsed) => {
            // A connect timeout surfaces as a synthetic timed-out io error so the
            // network hint logic still produces the right message.
            let e = std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out");
            return Err(crate::httperr::wrap(
                "connecting to daemon (is lane running?)",
                e,
            ));
        }
    };

    let payload = serde_json::to_vec(&req).context("sending request")?;

    conn.write_all(&payload).await.context("sending request")?;
    // Signal end-of-request so the server's read_to_end returns.
    conn.shutdown().await.context("sending request")?;

    let mut buf = Vec::new();
    conn.read_to_end(&mut buf)
        .await
        .context("reading response")?;

    let resp: Response = serde_json::from_slice(&buf).context("reading response")?;
    Ok(resp)
}

/// A request handler: maps a [`Request`] to a [`Response`].
///
/// Boxed as an async-returning closure so handlers can perform IO (config load,
/// upstream health probes, config reload) while serving a connection.
pub type Handler = Arc<
    dyn Fn(Request) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
        + Send
        + Sync,
>;

/// IPC server bound to the daemon's unix domain socket.
///
/// Mirrors Go's `IPCServer`. Holds the [`UnixListener`] and a [`Handler`]; the
/// accept loop spawns one task per connection.
pub struct IpcServer {
    listener: UnixListener,
    handler: Handler,
}

impl IpcServer {
    /// Bind the IPC socket and return a server.
    ///
    /// Mirrors Go's `NewIPCServer`: remove any stale socket, ensure the parent
    /// directory exists (`0755`), then listen on the unix socket.
    pub fn new(handler: Handler) -> Result<Self> {
        let sock_path = config::socket_path();

        // Remove any stale socket; ignore errors (matches Go's `os.Remove`).
        let _ = std::fs::remove_file(&sock_path);

        if let Some(parent) = sock_path.parent() {
            create_dir_all_0755(parent)?;
        }

        let listener = UnixListener::bind(&sock_path).context("listening on socket")?;

        Ok(IpcServer { listener, handler })
    }

    /// Accept connections until the listener is closed, handling each on its own
    /// task. Mirrors Go's `Serve`.
    pub async fn serve(&self) {
        loop {
            let (conn, _addr) = match self.listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            let handler = Arc::clone(&self.handler);
            tokio::spawn(handle_conn(conn, handler));
        }
    }

    /// Drop the listener and remove the socket file. Mirrors Go's `Close`.
    pub fn close(self) {
        // Dropping the listener releases the bound socket.
        drop(self.listener);
        let _ = std::fs::remove_file(config::socket_path());
    }
}

/// Serve a single connection: read the request to EOF, run the handler, write
/// the response. Mirrors Go's `handleConn` (with a 30s overall deadline).
async fn handle_conn(mut conn: UnixStream, handler: Handler) {
    let _ = tokio::time::timeout(CONN_DEADLINE, async move {
        let mut buf = Vec::new();
        if conn.read_to_end(&mut buf).await.is_err() {
            return;
        }

        let resp = match serde_json::from_slice::<Request>(&buf) {
            Ok(req) => handler(req).await,
            Err(e) => Response {
                ok: false,
                error: e.to_string(),
                data: None,
            },
        };

        if let Ok(out) = serde_json::to_vec(&resp) {
            let _ = conn.write_all(&out).await;
            let _ = conn.shutdown().await;
        }
    })
    .await;
}

/// Create `dir` and parents with mode `0755`, like Go's `os.MkdirAll(_, 0755)`.
fn create_dir_all_0755(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::MessageType;
    use std::sync::Arc;

    /// Set HOME to an isolated temp dir and create `~/.lane`. Mirrors Go's
    /// `initDaemonTestConfig` / `initDaemonStateTestConfig`.
    fn with_isolated_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().expect("TempDir");
        std::env::set_var("HOME", tmp.path());
        std::fs::create_dir_all(config::dir()).expect("MkdirAll config dir");
        tmp
    }

    fn handler_from<F>(f: F) -> Handler
    where
        F: Fn(Request) -> Response + Send + Sync + 'static,
    {
        Arc::new(move |req| {
            let resp = f(req);
            Box::pin(async move { resp })
        })
    }

    // Port of TestIPCServerRoundTrip.
    #[tokio::test]
    #[serial_test::serial]
    async fn ipc_server_round_trip() {
        let _home = with_isolated_home();

        let handler = handler_from(|req: Request| {
            if req.msg_type != MessageType::Status {
                return Response {
                    ok: false,
                    error: "unexpected request".to_string(),
                    data: None,
                };
            }
            Response {
                ok: true,
                error: String::new(),
                data: Some(serde_json::json!({ "ok": true })),
            }
        });

        let srv = IpcServer::new(handler).expect("NewIPCServer");
        let srv = Arc::new(srv);
        let serve_handle = {
            let srv = Arc::clone(&srv);
            tokio::spawn(async move { srv.serve().await })
        };

        let resp = send_ipc(Request {
            msg_type: MessageType::Status,
            data: None,
        })
        .await
        .expect("SendIPC");

        assert!(resp.ok, "expected OK response, got {resp:?}");
        assert_eq!(
            resp.data,
            Some(serde_json::json!({ "ok": true })),
            "unexpected response payload"
        );

        // Stop the accept loop; the temp HOME teardown removes the socket file.
        serve_handle.abort();
        drop(srv);
    }

    // Port of TestIPCServerReturnsErrorOnInvalidJSON.
    #[tokio::test]
    #[serial_test::serial]
    async fn ipc_server_returns_error_on_invalid_json() {
        let _home = with_isolated_home();

        let srv = IpcServer::new(handler_from(|_req| Response {
            ok: true,
            error: String::new(),
            data: None,
        }))
        .expect("NewIPCServer");
        let srv = Arc::new(srv);
        let serve_handle = {
            let srv = Arc::clone(&srv);
            tokio::spawn(async move { srv.serve().await })
        };

        let mut conn = UnixStream::connect(config::socket_path())
            .await
            .expect("Dial");
        conn.write_all(b"not json\n").await.expect("Write");
        conn.shutdown().await.expect("shutdown write");

        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read response");
        let resp: Response = serde_json::from_slice(&buf).expect("Decode response");

        assert!(!resp.ok, "expected error response, got {resp:?}");
        assert!(
            !resp.error.is_empty(),
            "expected decode error message, got {resp:?}"
        );

        // Stop the accept loop; the temp HOME teardown removes the socket file.
        serve_handle.abort();
        drop(srv);
    }

    // Port of TestSendIPCWhenDaemonNotRunning.
    #[tokio::test]
    #[serial_test::serial]
    async fn send_ipc_when_daemon_not_running() {
        let _home = with_isolated_home();
        let _ = std::fs::remove_file(config::socket_path());

        let err = send_ipc(Request {
            msg_type: MessageType::Status,
            data: None,
        })
        .await
        .expect_err("expected SendIPC to fail when socket is missing");

        assert!(
            err.to_string().contains("is lane running?"),
            "unexpected error: {err}"
        );
    }

    // Port of TestIPCServerCloseRemovesSocket.
    #[tokio::test]
    #[serial_test::serial]
    async fn ipc_server_close_removes_socket() {
        let _home = with_isolated_home();

        let srv = IpcServer::new(handler_from(|_req| Response {
            ok: true,
            error: String::new(),
            data: None,
        }))
        .expect("NewIPCServer");

        let sock_path = config::socket_path();
        assert!(sock_path.exists(), "expected socket file to exist");

        srv.close();
        assert!(
            !sock_path.exists(),
            "expected socket to be removed on close"
        );
    }
}
