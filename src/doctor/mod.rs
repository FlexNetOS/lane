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

    match pf.forwarding_status() {
        // Rule confirmed absent (the probe ran with enough privilege and found
        // it missing) -> the original "not configured" Fail.
        system::ForwardingStatus::Absent => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Fail,
                message: "not configured".to_string(),
            };
        }
        // Could not determine without root. Doctor is read-only and must not
        // trigger a sudo prompt, so warn honestly instead of a false Fail.
        system::ForwardingStatus::Unknown => {
            return CheckResult {
                name: name.to_string(),
                status: Status::Warn,
                message: "cannot verify without root (run: sudo lane doctor)".to_string(),
            };
        }
        // Present: fall through to the loaded / ingress checks below.
        system::ForwardingStatus::Present => {}
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

/// Pure decision for the Linux trust check: return the first anchor path in
/// `paths` for which `exists` reports `true`, else `None`. Factored out so the
/// decision is unit-testable with an injected existence predicate (no root,
/// no real trust store).
#[cfg(target_os = "linux")]
fn anchor_present(paths: &[&str], exists: impl Fn(&str) -> bool) -> Option<String> {
    paths.iter().find(|p| exists(p)).map(|p| (*p).to_string())
}

/// Build the `CA trust` [`CheckResult`] from the located anchor (if any). Pure,
/// so the Pass/Fail messages can be asserted host-independently in tests.
#[cfg(target_os = "linux")]
fn trust_result(anchor: Option<String>) -> CheckResult {
    let name = "CA trust";
    match anchor {
        Some(path) => {
            let dir = Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            CheckResult {
                name: name.to_string(),
                status: Status::Pass,
                message: format!("trusted by OS (found in {dir})"),
            }
        }
        None => CheckResult {
            name: name.to_string(),
            status: Status::Fail,
            message: "not found in system CA directories".to_string(),
        },
    }
}

/// Linux: the CA is trusted when the installer's anchor file (basename
/// `lane.crt`) is present in one of the system trust-store anchor directories.
///
/// The candidate paths come from [`crate::cert::trust::linux_anchor_paths`] —
/// the same list the installer writes — so the check matches the *actual*
/// on-disk anchor basename rather than the CA source file's `rootCA.pem`.
#[cfg(target_os = "linux")]
fn verify_ca_is_trusted() -> CheckResult {
    let anchors = crate::cert::trust::linux_anchor_paths();
    trust_result(anchor_present(&anchors, |p| Path::new(p).exists()))
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

/// Auto-heal a single doctor check failure. Returns Some(message) on success or None
/// if no auto-heal path exists for this check.
pub async fn auto_heal_check(r: &CheckResult) -> anyhow::Result<Option<String>> {
    match r.name.as_str() {
        "CA certificate" => {
            crate::cert::generate_ca(crate::cert::KeyType::Rsa2048)?;
            Ok(Some("regenerated root CA cert and key".to_string()))
        }
        "CA trust" => {
            match crate::cert::trust_ca() {
                Ok(()) => Ok(Some("installed CA into OS trust store".to_string())),
                Err(e) => Ok(Some(format!("install attempt: {e} (may require sudo)"))),
            }
        }
        name if name.starts_with("Hosts:") => {
            let domain = &name["Hosts: ".len()..];
            system::add_host(domain)?;
            Ok(Some(format!("added {domain} to /etc/hosts")))
        }
        name if name.starts_with("Leaf: ") => {
            let domain = &name["Leaf: ".len()..];
            crate::cert::generate_leaf_cert(domain, crate::cert::KeyType::EcdsaP256, None)?;
            Ok(Some(format!("regenerated leaf cert for {domain}")))
        }
        _ => Ok(None), // Port forwarding / daemon etc. — not auto-healable without sudo.
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
    #[cfg(target_os = "linux")]
    fn verify_ca_is_trusted_not_found() {
        // When no anchor is located, the trust check Fails with the stable
        // message. Driven through the pure `trust_result` so the assertion is
        // host-independent (the real `verify_ca_is_trusted` reads fixed system
        // anchor paths that may or may not exist on the test host).
        let r = trust_result(None);
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.message, "not found in system CA directories");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn verify_ca_is_trusted_found_reports_dir() {
        // When the debian anchor is located, Pass with the parent directory in
        // the message — the on-disk anchor basename, never `rootCA.pem`.
        let r = trust_result(Some(
            "/usr/local/share/ca-certificates/lane.crt".to_string(),
        ));
        assert_eq!(r.status, Status::Pass);
        assert_eq!(
            r.message,
            "trusted by OS (found in /usr/local/share/ca-certificates)"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_present_finds_installed_lane_anchor() {
        // Bug #5 regression guard (Linux): the trust check must look for the
        // installer's actual anchor (`lane.crt`), NOT the CA source basename
        // (`rootCA.pem`).
        let anchors = crate::cert::trust::linux_anchor_paths();
        let debian = "/usr/local/share/ca-certificates/lane.crt";

        // (a) only the debian lane.crt anchor exists -> Some(that path).
        let got = anchor_present(&anchors, |p| p == debian);
        assert_eq!(got.as_deref(), Some(debian));

        // (b) nothing exists -> None.
        assert_eq!(anchor_present(&anchors, |_| false), None);

        // (c) the searched basenames are `lane.crt`, never `rootCA.pem`.
        for a in anchors {
            assert!(
                a.ends_with("/lane.crt"),
                "doctor must search for lane.crt, got: {a}"
            );
            assert!(
                !a.ends_with("rootCA.pem"),
                "doctor must NOT search for rootCA.pem (bug #5): {a}"
            );
        }
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
