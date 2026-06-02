//! Upstream health checks.
//!
//! Faithful async port of `internal/proxy/health.go`. Go used blocking
//! `net.DialTimeout`; here we use `tokio::net::TcpStream::connect` guarded by a
//! `tokio::time::timeout`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Poll interval while waiting for an upstream to come up.
const UPSTREAM_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Connection timeout for a single upstream probe (mirrors Go's 1s dial timeout).
const DIAL_TIMEOUT: Duration = Duration::from_secs(1);

/// Maximum concurrent upstream probes (mirrors Go's `sem := make(chan struct{}, 16)`).
const MAX_CONCURRENT_CHECKS: usize = 16;

/// Return true if a TCP connection to `localhost:port` succeeds within 1s.
pub async fn check_upstream(port: u16) -> bool {
    let addr = format!("localhost:{port}");
    matches!(
        tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(addr)).await,
        Ok(Ok(_conn))
    )
}

/// Probe each port concurrently (capped at 16) and return results in input order.
pub async fn check_upstreams(ports: &[u16]) -> Vec<bool> {
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CHECKS));
    let mut handles = Vec::with_capacity(ports.len());

    for &port in ports {
        let sem = Arc::clone(&sem);
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            check_upstream(port).await
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        // A probe task only returns a bool and never panics; a join error
        // (e.g. cancellation) is treated as "not reachable".
        results.push(h.await.unwrap_or(false));
    }
    results
}

/// Wait until `localhost:port` becomes reachable or `timeout` elapses.
///
/// Errors immediately if `timeout` is not positive, exactly like Go.
pub async fn wait_for_upstream(port: u16, timeout: Duration) -> Result<()> {
    if timeout.is_zero() {
        return Err(anyhow!("timeout must be greater than 0"));
    }

    if check_upstream(port).await {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let mut ticker = tokio::time::interval(UPSTREAM_POLL_INTERVAL);
    // The first tick fires immediately; consume it so the first real poll waits
    // one interval, matching Go's `time.NewTicker` (first tick after interval).
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if check_upstream(port).await {
                    return Ok(());
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(anyhow!(
                    "upstream localhost:{port} did not become reachable within {}",
                    humantime::format_duration(timeout)
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn wait_for_upstream_ready_immediately() {
        let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ln.local_addr().unwrap().port();
        wait_for_upstream(port, Duration::from_millis(500))
            .await
            .expect("WaitForUpstream unexpected error");
    }

    #[tokio::test]
    async fn wait_for_upstream_becomes_ready() {
        // Bind to grab a free port, then release it.
        let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ln.local_addr().unwrap().port();
        drop(ln);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(ready_ln) = TcpListener::bind(("127.0.0.1", port)).await {
                tokio::time::sleep(Duration::from_millis(250)).await;
                drop(ready_ln);
            }
        });

        wait_for_upstream(port, Duration::from_secs(2))
            .await
            .expect("WaitForUpstream unexpected error");
    }

    #[tokio::test]
    async fn wait_for_upstream_timeout() {
        let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ln.local_addr().unwrap().port();
        drop(ln);

        let err = wait_for_upstream(port, Duration::from_millis(300)).await;
        assert!(err.is_err(), "expected timeout error, got Ok");
    }

    #[tokio::test]
    async fn wait_for_upstream_invalid_timeout() {
        let err = wait_for_upstream(3000, Duration::ZERO).await;
        assert!(err.is_err(), "expected error for invalid timeout");
    }

    #[tokio::test]
    async fn check_upstream_open_and_closed() {
        let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open_port = ln.local_addr().unwrap().port();
        assert!(check_upstream(open_port).await);
        drop(ln);

        let ln2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_port = ln2.local_addr().unwrap().port();
        drop(ln2);
        assert!(!check_upstream(closed_port).await);
    }

    #[tokio::test]
    async fn check_upstreams_preserves_order() {
        let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open_port = ln.local_addr().unwrap().port();

        let ln2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_port = ln2.local_addr().unwrap().port();
        drop(ln2);

        let results = check_upstreams(&[open_port, closed_port, open_port]).await;
        assert_eq!(results, vec![true, false, true]);
        drop(ln);
    }
}
