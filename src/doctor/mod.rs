//! doctor — environment diagnostics for the local proxy.
//!
//! Faithful port of Go's `internal/doctor` package (`doctor.go` plus the
//! cfg-gated `trust_*.go`). Each check produces a [`CheckResult`]; [`run`]
//! gathers them into a [`Report`] in the same order as slim:
//!
//! 1. CA certificate validity
//! 2. CA trust (platform-specific)
//! 3. Port forwarding
//! 4. `/etc/hosts` entry per configured domain
//! 5. Daemon (via IPC)
//! 6. Leaf certificate per configured domain
//!
//! Certificate expiry is classified with `x509-parser`: already expired ⇒
//! `Fail`, under 30 days remaining ⇒ `Warn`, otherwise `Pass`. Dates are
//! formatted `%Y-%m-%d` to mirror Go's `2006-01-02`.

use std::net::ToSocketAddrs;
use std::path::Path;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde::Serialize;

use crate::config;
use crate::daemon;
use crate::system;

/// Outcome of a single diagnostic check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The check succeeded.
    Pass,
    /// The check found a non-fatal issue.
    Warn,
    /// The check found a problem that needs fixing.
    Fail,
}

/// A named diagnostic result with its status and a human-readable message.
#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: Status,
    pub message: String,
}

/// The full set of diagnostic results, in slim's check order.
///
/// Serializes as `{ "checks": [...] }` (the `results` field is renamed for the
/// JSON key), mirroring `lane list --json`'s single top-level object.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    #[serde(rename = "checks")]
    pub results: Vec<CheckResult>,
}

/// 30 days, the "expires soon" threshold for certificates.
const RENEWAL_WINDOW: chrono::Duration = chrono::Duration::days(30);

/// Run every diagnostic check and collect the results.
///
/// Mirrors Go's `Run`: configuration is loaded best-effort (a load error is
/// treated as "no config", so the per-domain checks are simply skipped).
pub async fn run() -> Report {
    let cfg = config::load().ok();

    let mut results: Vec<CheckResult> = Vec::new();
    results.push(check_ca_cert());
    results.push(check_ca_trust());
    results.push(check_port_forwarding().await);

    if let Some(cfg) = &cfg {
        for d in &cfg.domains {
            results.push(check_hosts_file(&d.name));
        }
    }

    results.push(check_daemon().await);

    if let Some(cfg) = &cfg {
        for d in &cfg.domains {
            results.push(check_leaf_cert(&d.name));
        }
    }

    Report { results }
}

/// Classify a certificate at `path` named `name`, with `not_found` used when the
/// file is unreadable and `parse_msg` used when DER parsing fails. Shared by the
/// CA and leaf checks, whose only differences are the names and messages.
fn check_cert_file(name: &str, path: &Path, not_found: &str, parse_msg: &str) -> CheckResult {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: not_found.to_string(),
            };
        }
    };

    let pem = match x509_parser::pem::parse_x509_pem(&data) {
        Ok((_, pem)) => pem,
        Err(_) => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: "invalid PEM".to_string(),
            };
        }
    };

    let cert = match x509_parser::parse_x509_certificate(&pem.contents) {
        Ok((_, cert)) => cert,
        Err(_) => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: parse_msg.to_string(),
            };
        }
    };

    let not_after_ts = cert.validity().not_after.timestamp();
    let not_after = match Utc.timestamp_opt(not_after_ts, 0).single() {
        Some(t) => t,
        None => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: parse_msg.to_string(),
            };
        }
    };

    let remaining = not_after - Utc::now();
    if remaining <= chrono::Duration::zero() {
        return CheckResult {
            name: name.to_string(),
            status: Status::Fail,
            message: "expired".to_string(),
        };
    }
    if remaining < RENEWAL_WINDOW {
        return CheckResult {
            name: name.to_string(),
            status: Status::Warn,
            message: format!("expires soon ({})", not_after.format("%Y-%m-%d")),
        };
    }

    CheckResult {
        name: name.to_string(),
        status: Status::Pass,
        message: format!("valid, expires {}", not_after.format("%Y-%m-%d")),
    }
}

/// Check the root CA certificate's presence and validity.
fn check_ca_cert() -> CheckResult {
    // Go's parse-failure message embeds the underlying error ("cannot parse: " +
    // err); x509-parser does not surface a comparable string, so we keep the
    // stable "cannot parse" prefix.
    check_cert_file(
        "CA certificate",
        &crate::cert::ca_cert_path(),
        "not found",
        "cannot parse",
    )
}

/// Check that the CA certificate is trusted by the OS.
fn check_ca_trust() -> CheckResult {
    verify_ca_is_trusted()
}

/// Check that OS-level port forwarding (80→10080, 443→10443) is in place.
async fn check_port_forwarding() -> CheckResult {
    let name = "Port forwarding";
    let pf = system::new_port_forwarder();

    if !pf.is_enabled() {
        return CheckResult {
            name: name.to_string(),
            status: Status::Warn,
            message: "not configured".to_string(),
        };
    }

    if !pf.is_loaded() {
        let msg = "configured but inactive (run: sudo pfctl -e && sudo pfctl -f /etc/pf.conf)";
        let status = if daemon::is_running().await {
            Status::Fail
        } else {
            Status::Warn
        };
        return CheckResult {
            name: name.to_string(),
            status,
            message: msg.to_string(),
        };
    }

    if daemon::is_running().await {
        let missing = missing_ingress_ports();
        if !missing.is_empty() {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: format!(
                    "configured but local ingress is down on {} (run: sudo pfctl -e && sudo pfctl -f /etc/pf.conf)",
                    missing.join(", ")
                ),
            };
        }
    }

    CheckResult {
        name: name.to_string(),
        status: Status::Pass,
        message: format!(
            "active (80→{}, 443→{})",
            config::PROXY_HTTP_PORT,
            config::PROXY_HTTPS_PORT
        ),
    }
}

/// TCP-dial 127.0.0.1:80 and :443 (500ms timeout) and collect the unreachable
/// ports, in order. Mirrors Go's `missingIngressPorts`.
fn missing_ingress_ports() -> Vec<String> {
    let ports: [u16; 2] = [80, 443];
    let mut missing = Vec::new();
    for port in ports {
        if !dial_reachable(port, Duration::from_millis(500)) {
            missing.push(port.to_string());
        }
    }
    missing
}

/// True when a TCP connection to `127.0.0.1:port` succeeds within `timeout`.
fn dial_reachable(port: u16, timeout: Duration) -> bool {
    let addr = match (std::net::Ipv4Addr::LOCALHOST, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
    {
        Some(a) => a,
        None => return false,
    };
    std::net::TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Check that `domain` is present in `/etc/hosts` with the lane marker.
fn check_hosts_file(domain: &str) -> CheckResult {
    let name = format!("Hosts: {domain}");

    let content = match std::fs::read_to_string("/etc/hosts") {
        Ok(c) => c,
        Err(_) => {
            return CheckResult {
                name,
                status: Status::Fail,
                message: "cannot read /etc/hosts".to_string(),
            };
        }
    };

    if system::has_marked_entry(&content, domain) {
        CheckResult {
            name,
            status: Status::Pass,
            message: "present in /etc/hosts".to_string(),
        }
    } else {
        CheckResult {
            name,
            status: Status::Fail,
            message: "missing from /etc/hosts".to_string(),
        }
    }
}

/// Check whether the daemon is running and answering IPC.
async fn check_daemon() -> CheckResult {
    let name = "Daemon";

    if !daemon::is_running().await {
        return CheckResult {
            name: name.to_string(),
            status: Status::Warn,
            message: "not running".to_string(),
        };
    }

    let req = daemon::Request {
        msg_type: daemon::MessageType::Status,
        data: None,
    };
    match daemon::send_ipc(req).await {
        Ok(resp) if resp.ok => CheckResult {
            name: name.to_string(),
            status: Status::Pass,
            message: "running".to_string(),
        },
        _ => CheckResult {
            name: name.to_string(),
            status: Status::Fail,
            message: "running but IPC failed".to_string(),
        },
    }
}

/// Check the leaf certificate's presence and validity for `domain`.
fn check_leaf_cert(domain: &str) -> CheckResult {
    let name = format!("Cert: {domain}");
    check_cert_file(
        &name,
        &crate::cert::leaf_cert_path(domain),
        "not found",
        "cannot parse",
    )
}

// ---------------------------------------------------------------------------
// CA trust verification (⇐ trust_linux.go / trust_darwin.go / trust_unsupported.go)
// ---------------------------------------------------------------------------

/// System CA anchor directories searched on Linux.
#[cfg(target_os = "linux")]
const CA_DIRS: &[&str] = &[
    "/usr/local/share/ca-certificates",
    "/etc/pki/ca-trust/source/anchors",
    "/etc/ca-certificates/trust-source/anchors",
];

/// Linux: the CA is trusted when its anchor basename is found in one of the
/// system CA directories.
#[cfg(target_os = "linux")]
fn verify_ca_is_trusted() -> CheckResult {
    let name = "CA trust";
    let ca_path = crate::cert::ca_cert_path();
    let ca_base = ca_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    for dir in CA_DIRS {
        let anchor = Path::new(dir).join(&ca_base);
        if anchor.exists() {
            return CheckResult {
                name: name.to_string(),
                status: Status::Pass,
                message: format!("trusted by OS (found in {dir})"),
            };
        }
    }

    CheckResult {
        name: name.to_string(),
        status: Status::Fail,
        message: "not found in system CA directories".to_string(),
    }
}

/// macOS: the CA is trusted when `security verify-cert` exits 0.
#[cfg(target_os = "macos")]
fn verify_ca_is_trusted() -> CheckResult {
    let name = "CA trust";
    let ca_path = crate::cert::ca_cert_path();

    let ok = std::process::Command::new("security")
        .arg("verify-cert")
        .arg("-c")
        .arg(&ca_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        CheckResult {
            name: name.to_string(),
            status: Status::Pass,
            message: "trusted by OS".to_string(),
        }
    } else {
        CheckResult {
            name: name.to_string(),
            status: Status::Fail,
            message: "not trusted by OS".to_string(),
        }
    }
}

/// Other platforms: trust verification is unsupported.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn verify_ca_is_trusted() -> CheckResult {
    CheckResult {
        name: "CA trust".to_string(),
        status: Status::Warn,
        message: "trust verification not supported on this platform".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{date_time_ymd, CertificateParams, DistinguishedName, DnType, KeyPair};
    use serial_test::serial;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Point `HOME` at a fresh temp dir so `config::dir()` (and thus every cert
    /// path) is isolated. Returns the guard plus the home path.
    fn isolated_home() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        std::env::set_var("HOME", dir.path());
        let home = dir.path().to_path_buf();
        (dir, home)
    }

    /// Build a self-signed ECDSA P-256 test cert (CN "test") whose `not_after`
    /// is `offset_secs` from now, and return its PEM bytes. Mirrors the Go test
    /// helper `generateTestCertPEM`.
    fn generate_test_cert_pem(offset_secs: i64) -> Vec<u8> {
        let key = KeyPair::generate().expect("generate key");

        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "test");
        params.distinguished_name = dn;

        let now = Utc::now().timestamp();
        let epoch = date_time_ymd(1970, 1, 1);
        // not_before = now - 1h, mirroring the Go template.
        params.not_before = epoch + Duration::from_secs((now - 3600).max(0) as u64);
        // not_after = now + offset (may be in the past for the "expired" case).
        let after = now + offset_secs;
        params.not_after = if after >= 0 {
            epoch + Duration::from_secs(after as u64)
        } else {
            epoch
        };

        let cert = params.self_signed(&key).expect("self-signed cert");
        cert.pem().into_bytes()
    }

    /// Write PEM bytes to a freshly-created cert directory + path (0700 dir).
    fn write_cert(path: &Path, pem: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir cert dir");
        std::fs::write(path, pem).expect("write cert");
    }

    #[test]
    #[serial]
    fn check_ca_cert_missing() {
        let (_guard, _home) = isolated_home();
        // No CA file on disk -> Fail with "not found".
        let r = check_ca_cert();
        assert_eq!(
            r.status,
            Status::Fail,
            "missing cert should Fail: {}",
            r.message
        );
        assert_eq!(r.message, "not found");
    }

    #[test]
    #[serial]
    fn check_ca_cert_valid_warn_expired() {
        let (_guard, _home) = isolated_home();
        let path = crate::cert::ca_cert_path();

        write_cert(&path, &generate_test_cert_pem(365 * 24 * 3600));
        let r = check_ca_cert();
        assert_eq!(
            r.status,
            Status::Pass,
            "valid cert should Pass: {}",
            r.message
        );
        assert!(
            r.message.starts_with("valid, expires "),
            "message: {}",
            r.message
        );

        write_cert(&path, &generate_test_cert_pem(10 * 24 * 3600));
        let r = check_ca_cert();
        assert_eq!(
            r.status,
            Status::Warn,
            "expiring cert should Warn: {}",
            r.message
        );
        assert!(
            r.message.starts_with("expires soon ("),
            "message: {}",
            r.message
        );

        write_cert(&path, &generate_test_cert_pem(-3600));
        let r = check_ca_cert();
        assert_eq!(
            r.status,
            Status::Fail,
            "expired cert should Fail: {}",
            r.message
        );
        assert_eq!(r.message, "expired");
    }

    #[test]
    #[serial]
    fn check_ca_cert_invalid_pem() {
        let (_guard, _home) = isolated_home();
        let path = crate::cert::ca_cert_path();
        write_cert(&path, b"not a pem");
        let r = check_ca_cert();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.message, "invalid PEM");
    }

    #[test]
    #[serial]
    fn check_leaf_cert_valid_and_missing() {
        let (_guard, _home) = isolated_home();

        let path = crate::cert::leaf_cert_path("myapp.test");
        write_cert(&path, &generate_test_cert_pem(365 * 24 * 3600));
        let r = check_leaf_cert("myapp.test");
        assert_eq!(
            r.status,
            Status::Pass,
            "valid leaf should Pass: {}",
            r.message
        );

        // A different (unwritten) domain has no cert -> Fail "not found".
        let r = check_leaf_cert("other.test");
        assert_eq!(
            r.status,
            Status::Fail,
            "missing leaf should Fail: {}",
            r.message
        );
        assert_eq!(r.message, "not found");
    }

    #[test]
    fn check_hosts_file_marker_logic() {
        // check_hosts_file reads the real /etc/hosts, so we exercise the pure
        // marker logic that drives it (system::has_marked_entry), matching the
        // intent of Go's TestCheckHostsFile without depending on /etc/hosts.
        assert!(system::has_marked_entry(
            "127.0.0.1 myapp.test # lane\n",
            "myapp.test"
        ));
        assert!(!system::has_marked_entry(
            "127.0.0.1 localhost\n",
            "myapp.test"
        ));
    }

    #[test]
    fn missing_ingress_ports_unreachable() {
        // A connection to 127.0.0.1:1 should fail quickly; this exercises the
        // dial path used by missing_ingress_ports without touching privileged
        // ports 80/443. (Functional parity for the dial seam.)
        assert!(!dial_reachable(1, Duration::from_millis(200)));
    }

    #[test]
    #[serial]
    #[cfg(target_os = "linux")]
    fn verify_ca_is_trusted_not_found() {
        let (_guard, _home) = isolated_home();
        // The temp-home CA basename will not be present in the system CA dirs.
        let r = verify_ca_is_trusted();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.message, "not found in system CA directories");
    }

    #[test]
    fn report_serializes_checks_with_lowercase_status() {
        // Locks the JSON contract host-independently: top-level `checks` key,
        // each check exposes name/status/message, and `Status` serializes to the
        // stable lowercase strings `pass`/`warn`/`fail`.
        let report = Report {
            results: vec![
                CheckResult {
                    name: "CA certificate".to_string(),
                    status: Status::Pass,
                    message: "valid".to_string(),
                },
                CheckResult {
                    name: "Daemon".to_string(),
                    status: Status::Warn,
                    message: "not running".to_string(),
                },
                CheckResult {
                    name: "Hosts".to_string(),
                    status: Status::Fail,
                    message: "missing".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&report).expect("serialize report");
        assert!(json.contains("\"checks\""), "top-level checks key: {json}");
        assert!(json.contains("\"status\":\"pass\""), "pass status: {json}");
        assert!(json.contains("\"status\":\"warn\""), "warn status: {json}");
        assert!(json.contains("\"status\":\"fail\""), "fail status: {json}");
        assert!(json.contains("\"name\":\"CA certificate\""), "name: {json}");
        assert!(json.contains("\"message\":\"valid\""), "message: {json}");
    }

    // TODO(test-phase): TestCheckPortForwarding — requires mocking
    // system::new_port_forwarder + daemon::is_running + the dial seam, which are
    // not injectable in the Rust port; covered by integration tests later.

    // TODO(test-phase): TestCheckDaemon — requires a running async daemon IPC
    // endpoint; handled in the daemon integration layer.

    // TODO(test-phase): TestRun — exercises the full async pipeline including
    // daemon IPC and port forwarding; handled in the integration layer.
}
