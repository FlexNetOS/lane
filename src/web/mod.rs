//! The governed-egress `lane web` seam (ADR-0001 Option B, lane↔obscura).
//!
//! lane is the network control plane; obscura is a managed web-egress engine
//! that lane spawns and forces through lane's own proxy + policy. This module is
//! the **mechanism** — split into a pure, always-compiled layer (the governed-op
//! model, the spawn *plan*, and the policy gate) and a thin live layer that
//! actually spawns the child, gated behind the `obscura` cargo feature.
//!
//! # What is always compiled (and exhaustively tested)
//!
//! - [`WebOp`] — the governed operation an agent requests (`open` / `run`).
//! - [`ObscuraSpawn`] — a config-driven, pure **command plan** (program + argv +
//!   env, as data) that *pins obscura's egress through lane* and *trusts lane's
//!   CA*. Building the plan does not run anything, so the egress-pinning contract
//!   is unit-testable without obscura present.
//! - [`authorize`] — runs the deny-by-default [`crate::webpolicy`] gate on an op's
//!   target **before** any spawn.
//! - [`run`] — the entry point the CLI calls. Without the `obscura` feature it
//!   fails closed with a clear error (mirroring [`crate::acme::issue`]); with the
//!   feature it authorizes, plans, and spawns.
//!
//! # What is feature-gated (`obscura`)
//!
//! Only the live child-process spawn (`tokio::process::Command` from the plan,
//! the wait/stream, and the access-log write). The planning and gating logic stay
//! in the pure layer so they remain compiled and tested in every build.
//!
//! The daemon / MCP `lane_web` dispatcher is the documented **next** step once
//! obscura is integrated (Phase A1); the CLI path here is the v1 surface.

use anyhow::Result;

use crate::config::ObscuraConfig;
use crate::webpolicy::{DenyReason, WebPolicy};

pub mod proxy;

pub use proxy::GovernedProxy;

/// A governed web operation, requested via `lane web …` and gated by
/// [`authorize`] before obscura ever runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebOp {
    /// Open (navigate to) a URL. The URL is the policy-checked target.
    Open {
        /// The absolute `http`/`https` URL to navigate to.
        url: String,
    },
    /// Run an automation script against a target URL. The script is a local
    /// file path whose **contents** are read at plan time and handed to
    /// obscura's `fetch --eval <JS>` (obscura's `--eval` takes a JS string, not
    /// a path); `url` is the navigation target and the policy-checked egress
    /// target.
    Run {
        /// Path to the local JavaScript file obscura evaluates against the page.
        script_path: String,
        /// The navigation target (the policy-checked URL).
        url: String,
    },
}

impl WebOp {
    /// The egress target (URL) this op navigates to — the value the policy gate
    /// checks. Every governed op has exactly one.
    pub fn target(&self) -> &str {
        match self {
            WebOp::Open { url } => url,
            WebOp::Run { url, .. } => url,
        }
    }

    /// A short, stable name for the op kind, used in machine-readable output.
    pub fn kind(&self) -> &'static str {
        match self {
            WebOp::Open { .. } => "open",
            WebOp::Run { .. } => "run",
        }
    }
}

/// Run the deny-by-default policy gate on `op`'s egress target.
///
/// This is the single chokepoint every governed op passes through *before* a
/// spawn. It returns `Ok(())` only when [`WebPolicy::check`] allows the target;
/// otherwise it returns the typed [`DenyReason`] so callers can render an exact,
/// actionable message (and so the CLI/daemon report a consistent shape).
pub fn authorize(policy: &WebPolicy, op: &WebOp) -> Result<(), DenyReason> {
    match policy.check(op.target()) {
        crate::webpolicy::PolicyDecision::Allow => Ok(()),
        crate::webpolicy::PolicyDecision::Deny(reason) => Err(reason),
    }
}

/// A pure, fully-resolved plan for spawning obscura: the program to exec, its
/// argv, and the environment overlay. Built by [`ObscuraSpawn::plan`] from the
/// resolved [`ObscuraConfig`] and a governed [`WebOp`]; nothing here runs a
/// process. The live layer (`obscura` feature) turns this into a
/// `tokio::process::Command`.
///
/// The plan is the heart of "obscura is under lane's network control at the
/// packet level": it *always* pins egress to the lane-controlled proxy and
/// *always* trusts lane's CA, and it takes the obscura binary from config rather
/// than the ambient `$PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObscuraSpawn {
    /// The obscura program to exec (an explicit path from config).
    pub program: String,
    /// The argv passed to obscura (excluding `program`).
    pub args: Vec<String>,
    /// Environment overlay applied to the child (key, value) pairs. These pin
    /// egress through lane and point obscura at lane's CA bundle.
    pub envs: Vec<(String, String)>,
}

/// The error returned when an [`ObscuraSpawn`] plan cannot be built because the
/// egress-pinning invariants are not satisfiable from config. Distinct from a
/// policy denial: this means the *seam* is misconfigured, not that the *target*
/// is forbidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnPlanError {
    /// `obscura_bin` is not configured. lane never resolves obscura from
    /// `$PATH`, so without an explicit path there is nothing to spawn.
    MissingBin,
    /// A [`WebOp::Run`]'s `script_path` could not be read. obscura's `fetch
    /// --eval` takes a JS *string*, so lane reads the file at plan time;
    /// failing to read it fails closed (the carried `String` is the I/O error)
    /// rather than silently sending an empty eval.
    ScriptUnreadable(String),
}

impl std::fmt::Display for SpawnPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnPlanError::MissingBin => f.write_str(
                "obscura binary not configured: set `obscura_bin` (or LANE_OBSCURA_BIN); \
                 lane never resolves obscura from $PATH",
            ),
            SpawnPlanError::ScriptUnreadable(err) => {
                write!(f, "cannot read automation script for `fetch --eval`: {err}")
            }
        }
    }
}

impl std::error::Error for SpawnPlanError {}

impl ObscuraSpawn {
    /// Build the spawn plan for `op` from the resolved obscura `config`.
    ///
    /// Pure: returns the planned [`ObscuraSpawn`] as data, or a
    /// [`SpawnPlanError`] if the egress-pinning invariants cannot be met (or a
    /// [`WebOp::Run`] script cannot be read). The resulting plan matches
    /// obscura's REAL CLI and ALWAYS:
    ///
    /// - takes `program` from `config.bin` (never `$PATH`),
    /// - emits, as **globals** (before the subcommand), `--proxy <config.proxy>`,
    ///   `--ca <ca_pem_path>`, and `--allow-private-network` (lane's proxy
    ///   listens on loopback and obscura blocks loopback/RFC1918 by default, so
    ///   without this obscura cannot even reach lane's proxy), plus
    ///   `--user-agent <ua>` when configured, and
    /// - additionally pins egress + CA trust via the standard
    ///   `HTTP_PROXY`/`HTTPS_PROXY` (+ lowercase) and CA-bundle env vars so a
    ///   flag-ignoring obscura build still cannot escape the pin.
    ///
    /// The subcommand is obscura's `fetch <url>`: a [`WebOp::Open`] maps to
    /// `fetch <url>`, and a [`WebOp::Run`] reads the script file's contents and
    /// maps to `fetch <url> --eval <JS-CONTENTS>` (obscura's `--eval` takes a JS
    /// string, not a path). `--stealth` (when `config.stealth`) is appended
    /// **after** the `fetch` subcommand because it is per-subcommand in obscura,
    /// not a global (and requires obscura to be built `--features stealth`).
    ///
    /// `proxy` is the egress endpoint obscura is pinned to. In the LIVE path it
    /// is **lane's own governed-proxy** address ([`crate::web::GovernedProxy::addr`])
    /// — NOT the user's `obscura_proxy` config. obscura therefore routes every
    /// connection through lane's loopback governor, where it is policy-checked
    /// and logged. (The user's `obscura_proxy`, when set, is repurposed as an
    /// optional *upstream* for the governed proxy itself — see
    /// [`crate::web::GovernedProxy::start_with_upstream`].) Keeping `proxy` a
    /// parameter (rather than reading it from config) is what lets the live
    /// caller pin obscura to lane.
    pub fn plan(
        config: &ObscuraConfig,
        proxy: &str,
        ca_pem_path: &str,
        op: &WebOp,
    ) -> Result<ObscuraSpawn, SpawnPlanError> {
        let program = config.bin.clone().ok_or(SpawnPlanError::MissingBin)?;
        let proxy = proxy.to_string();

        // Globals (before the subcommand): egress pinning (`--proxy`, also
        // surfaced via env below for builds that honor proxy env over a flag) +
        // CA trust (`--ca`) so lane-terminated TLS validates in obscura, and
        // `--allow-private-network` so obscura's SSRF guard permits reaching
        // lane's loopback proxy (the governed spawn intentionally routes through
        // lane's 127.0.0.1 listener — egress stays pinned to lane).
        let mut args = vec![
            "--proxy".to_string(),
            proxy.clone(),
            "--ca".to_string(),
            ca_pem_path.to_string(),
            "--allow-private-network".to_string(),
        ];

        if let Some(ua) = &config.user_agent {
            args.push("--user-agent".to_string());
            args.push(ua.clone());
        }

        // The op itself — always obscura's `fetch <url>` subcommand.
        match op {
            WebOp::Open { url } => {
                args.push("fetch".to_string());
                args.push(url.clone());
            }
            WebOp::Run { script_path, url } => {
                // obscura's `--eval` takes a JS string, not a path: read the
                // script at plan time and fail closed if it is unreadable
                // (never send an empty eval).
                let script = std::fs::read_to_string(script_path)
                    .map_err(|e| SpawnPlanError::ScriptUnreadable(e.to_string()))?;
                args.push("fetch".to_string());
                args.push(url.clone());
                args.push("--eval".to_string());
                args.push(script);
            }
        }

        // `--stealth` is PER-SUBCOMMAND in obscura (on `fetch`), not a global,
        // and needs obscura built `--features stealth`. Emit it after `fetch`.
        if config.stealth {
            args.push("--stealth".to_string());
        }

        // Pin egress and CA trust via env too — belt-and-suspenders so an obscura
        // build that ignores a flag still cannot open ungoverned egress.
        let envs = vec![
            ("HTTP_PROXY".to_string(), proxy.clone()),
            ("HTTPS_PROXY".to_string(), proxy.clone()),
            ("http_proxy".to_string(), proxy.clone()),
            ("https_proxy".to_string(), proxy.clone()),
            ("LANE_OBSCURA_PROXY".to_string(), proxy),
            ("SSL_CERT_FILE".to_string(), ca_pem_path.to_string()),
            ("LANE_CA".to_string(), ca_pem_path.to_string()),
        ];

        Ok(ObscuraSpawn {
            program,
            args,
            envs,
        })
    }
}

/// The outcome of a governed `lane web` op, for machine-readable (`--json`)
/// reporting and human summaries.
#[derive(Debug, Clone)]
pub struct WebOutcome {
    /// The op kind (`open` / `run`).
    pub op: &'static str,
    /// The egress target that was checked.
    pub target: String,
    /// `true` if the policy allowed the op (and, with the feature, it spawned).
    pub allowed: bool,
}

/// Execute a governed `lane web` op end to end: authorize via the policy gate,
/// then (with the `obscura` feature) plan and spawn obscura pinned through lane.
///
/// Deny-by-default: a denial returns an `anyhow` error carrying the
/// [`DenyReason`] message and never spawns anything.
///
/// Without the `obscura` feature this fails closed with a clear, actionable
/// error after authorization — mirroring [`crate::acme::issue`]'s no-feature
/// stub. The policy gate still runs so a denied op reports a denial (not a
/// "not enabled" error) in every build.
pub async fn run(
    policy: &WebPolicy,
    config: &ObscuraConfig,
    ca_pem_path: &str,
    tls_inspect: bool,
    op: &WebOp,
) -> Result<WebOutcome> {
    // Gate first, in every build: deny-by-default precedes any feature check.
    // This is the ENTRY-op gate (defense in depth): the entry URL is checked
    // before any spawn, AND — in the feature build — every egress connection
    // obscura subsequently opens is independently checked by lane's governed
    // proxy. Two layers, same deny-by-default policy.
    if let Err(reason) = authorize(policy, op) {
        anyhow::bail!("denied: {reason}");
    }

    run_authorized(policy, config, ca_pem_path, tls_inspect, op).await?;

    Ok(WebOutcome {
        op: op.kind(),
        target: op.target().to_string(),
        allowed: true,
    })
}

/// Spawn obscura for an already-authorized op (feature build), pinned to lane's
/// own governed forward proxy so EVERY connection obscura opens is
/// policy-checked and logged — not just the entry URL.
///
/// Flow:
/// 1. Start a [`GovernedProxy`] on loopback, governing with the same `policy`.
///    The user's `config.proxy` (`obscura_proxy`), when set, becomes the proxy's
///    optional **upstream** (chained *after* governance); v1 supports the direct
///    case and fails closed with a clear error if an upstream is configured.
/// 2. Build the spawn plan pinning obscura's `--proxy` (and proxy env) to the
///    governed proxy's address — NOT the raw user config.
/// 3. Spawn obscura; await it; then shut the governed proxy down (RAII via
///    `GovernedProxy::drop`, plus an explicit `shutdown()`).
#[cfg(feature = "obscura")]
async fn run_authorized(
    policy: &WebPolicy,
    config: &ObscuraConfig,
    ca_pem_path: &str,
    tls_inspect: bool,
    op: &WebOp,
) -> Result<()> {
    use anyhow::Context;

    // Start lane's governed proxy. The user's obscura_proxy becomes the optional
    // upstream (chained after governance). `tls_inspect` (config
    // `web_tls_inspect`, default off) opts into request/path-level MITM of
    // obscura's HTTPS egress; off ⇒ opaque CONNECT tunnels (host/port only).
    let governed =
        GovernedProxy::start_with_options(policy.clone(), config.proxy.clone(), tls_inspect)
            .await
            .context("starting lane governed proxy")?;
    let proxy_addr = governed.addr();

    // Pin obscura to LANE'S governed proxy (not the user's raw config). Every
    // connection obscura opens now flows through lane's policy-checked governor.
    let plan = ObscuraSpawn::plan(config, &proxy_addr, ca_pem_path, op)
        .map_err(|e| anyhow::anyhow!("cannot spawn obscura: {e}"))?;

    // Observe the governed request in lane's access log — the single place all
    // agent web traffic lands (ADR-0001 §4). Per-connection ALLOW/DENY lines are
    // emitted by the governed proxy itself.
    crate::log::info(&format!(
        "web {} {} via {} (governed proxy {})",
        op.kind(),
        op.target(),
        plan.program,
        proxy_addr,
    ));

    let mut command = tokio::process::Command::new(&plan.program);
    command.args(&plan.args);
    for (k, v) in &plan.envs {
        command.env(k, v);
    }

    let status = command
        .status()
        .await
        .with_context(|| format!("spawning obscura ({})", plan.program));

    // obscura has exited (or failed to spawn): governance ends here. Explicit
    // shutdown in addition to the Drop guard so the port is freed promptly.
    governed.shutdown();
    let status = status?;

    if !status.success() {
        anyhow::bail!("obscura exited with status {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

/// Feature-off gate: spawning obscura requires building with `--features
/// obscura`. The op was already authorized by [`run`]; this is the single
/// sanctioned "not enabled" path (mirrors [`crate::acme::issue`]).
#[cfg(not(feature = "obscura"))]
async fn run_authorized(
    _policy: &WebPolicy,
    _config: &ObscuraConfig,
    _ca_pem_path: &str,
    _tls_inspect: bool,
    _op: &WebOp,
) -> Result<()> {
    anyhow::bail!(
        "obscura integration is not enabled in this build; rebuild with \
         `--features obscura` once obscura is integrated (Phase A1)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg(bin: Option<&str>, proxy: Option<&str>) -> ObscuraConfig {
        ObscuraConfig {
            bin: bin.map(str::to_string),
            proxy: proxy.map(str::to_string),
            stealth: false,
            user_agent: None,
        }
    }

    /// The lane governed-proxy addr the live caller pins obscura to. In tests it
    /// is a fixed loopback URL standing in for `GovernedProxy::addr`.
    const GOVERNED: &str = "http://127.0.0.1:10443";

    fn allow_example() -> WebPolicy {
        WebPolicy::default().allow_domain("example.com")
    }

    // --- WebOp model --------------------------------------------------------

    #[test]
    fn webop_target_and_kind() {
        let open = WebOp::Open {
            url: "https://example.com/".into(),
        };
        assert_eq!(open.target(), "https://example.com/");
        assert_eq!(open.kind(), "open");

        let run = WebOp::Run {
            script_path: "/tmp/s.js".into(),
            url: "https://example.com/login".into(),
        };
        assert_eq!(run.target(), "https://example.com/login");
        assert_eq!(run.kind(), "run");
    }

    // --- authorize (the gate) ----------------------------------------------

    #[test]
    fn authorize_allows_only_allowlisted_target() {
        let policy = allow_example();
        let op = WebOp::Open {
            url: "https://api.example.com/".into(),
        };
        assert!(authorize(&policy, &op).is_ok());
    }

    #[test]
    fn authorize_denies_by_default() {
        // Empty allowlist denies everything.
        let policy = WebPolicy::default();
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        assert!(matches!(
            authorize(&policy, &op),
            Err(DenyReason::HostNotAllowed(_))
        ));
    }

    #[test]
    fn authorize_denies_ssrf_loopback_even_if_allowlisted() {
        // Allowlisting localhost does not exempt it from the SSRF guard.
        let policy = WebPolicy::default().allow_host("localhost");
        let op = WebOp::Open {
            url: "http://localhost/".into(),
        };
        assert!(matches!(authorize(&policy, &op), Err(DenyReason::Loopback)));
    }

    #[test]
    fn authorize_denies_private_ip_literal() {
        let policy = allow_example();
        let op = WebOp::Open {
            url: "http://10.0.0.1/".into(),
        };
        assert!(matches!(
            authorize(&policy, &op),
            Err(DenyReason::PrivateNetwork)
        ));
    }

    #[test]
    fn authorize_denies_non_http_scheme() {
        let policy = allow_example();
        let op = WebOp::Open {
            url: "file:///etc/passwd".into(),
        };
        assert!(matches!(
            authorize(&policy, &op),
            Err(DenyReason::SchemeNotAllowed(_))
        ));
    }

    #[test]
    fn authorize_denies_disallowed_port() {
        let policy = allow_example();
        let op = WebOp::Open {
            url: "https://example.com:8443/".into(),
        };
        assert!(matches!(
            authorize(&policy, &op),
            Err(DenyReason::PortNotAllowed(8443))
        ));
    }

    // --- ObscuraSpawn plan (egress pinning) ---------------------------------

    #[test]
    fn plan_requires_bin_from_config_not_path() {
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        assert_eq!(
            ObscuraSpawn::plan(&cfg(None, None), GOVERNED, "/ca.pem", &op),
            Err(SpawnPlanError::MissingBin)
        );
    }

    #[test]
    fn plan_pins_egress_to_the_caller_supplied_governed_proxy() {
        // The proxy is now supplied by the live caller (lane's governed-proxy
        // addr), NOT read from user config. Even if the user set a different
        // obscura_proxy, the plan pins to the governed addr passed in.
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        let plan = ObscuraSpawn::plan(
            &cfg(Some("/opt/obscura"), Some("http://user-configured-proxy:9")),
            GOVERNED,
            "/ca.pem",
            &op,
        )
        .expect("plan");
        let proxy_idx = plan.args.iter().position(|a| a == "--proxy").expect("flag");
        assert_eq!(plan.args[proxy_idx + 1], GOVERNED);
        // The user's obscura_proxy is NOT what obscura is pinned to.
        assert!(!plan
            .args
            .iter()
            .any(|a| a == "http://user-configured-proxy:9"));
    }

    #[test]
    fn plan_always_pins_egress_through_lane() {
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        let plan = ObscuraSpawn::plan(
            &cfg(Some("/opt/obscura/obscura"), None),
            GOVERNED,
            "/home/u/.lane/certs/ca.pem",
            &op,
        )
        .expect("plan");

        // Program is the explicit config path, never a bare name on $PATH.
        assert_eq!(plan.program, "/opt/obscura/obscura");

        // --proxy flag is present (a global) and points at the lane listener.
        let proxy_idx = plan.args.iter().position(|a| a == "--proxy").expect("flag");
        assert_eq!(plan.args[proxy_idx + 1], GOVERNED);

        // --allow-private-network is a global so obscura's SSRF guard permits
        // reaching lane's loopback proxy, and it precedes the `fetch`
        // subcommand.
        let apn_idx = plan
            .args
            .iter()
            .position(|a| a == "--allow-private-network")
            .expect("--allow-private-network must be present");
        let fetch_idx = plan
            .args
            .iter()
            .position(|a| a == "fetch")
            .expect("fetch subcommand");
        assert!(
            apn_idx < fetch_idx,
            "--allow-private-network must be a global (before `fetch`)"
        );

        // The subcommand is `fetch <url>`, never the old `open`.
        assert!(!plan.args.iter().any(|a| a == "open"));
        assert_eq!(plan.args[fetch_idx + 1], "https://example.com/");

        // Proxy env is set on EVERY standard key (flag-ignoring builds can't
        // escape the pin).
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            let v = plan
                .envs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str());
            assert_eq!(
                v,
                Some("http://127.0.0.1:10443"),
                "env {key} must be pinned"
            );
        }

        // CA trust is wired both as a flag and via the CA-bundle env.
        assert!(plan.args.iter().any(|a| a == "--ca"));
        assert!(plan
            .envs
            .iter()
            .any(|(k, v)| k == "SSL_CERT_FILE" && v == "/home/u/.lane/certs/ca.pem"));
    }

    #[test]
    fn plan_maps_stealth_and_user_agent_flags() {
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        let config = ObscuraConfig {
            bin: Some("/opt/obscura".into()),
            proxy: None,
            stealth: true,
            user_agent: Some("lane-web/1".into()),
        };
        let plan = ObscuraSpawn::plan(&config, GOVERNED, "/ca.pem", &op).expect("plan");

        // --user-agent is a global: it precedes the `fetch` subcommand.
        let ua_idx = plan
            .args
            .iter()
            .position(|a| a == "--user-agent")
            .expect("ua flag");
        assert_eq!(plan.args[ua_idx + 1], "lane-web/1");
        let fetch_idx = plan
            .args
            .iter()
            .position(|a| a == "fetch")
            .expect("fetch subcommand");
        assert!(ua_idx < fetch_idx, "--user-agent must be a global");

        // --stealth is PER-SUBCOMMAND: it must come AFTER `fetch <url>`.
        let stealth_idx = plan
            .args
            .iter()
            .position(|a| a == "--stealth")
            .expect("stealth flag");
        assert!(
            stealth_idx > fetch_idx,
            "--stealth must follow the `fetch` subcommand"
        );
    }

    #[test]
    fn plan_without_stealth_omits_flags() {
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        let plan = ObscuraSpawn::plan(&cfg(Some("/opt/obscura"), None), GOVERNED, "/ca.pem", &op)
            .expect("plan");
        assert!(!plan.args.iter().any(|a| a == "--stealth"));
        assert!(!plan.args.iter().any(|a| a == "--user-agent"));
    }

    #[test]
    fn plan_run_op_reads_script_into_eval() {
        // obscura has no `run` subcommand: a Run op maps to
        // `fetch <url> --eval <SCRIPT-CONTENTS>`, with the script file read at
        // plan time (its contents, not its path).
        let tmp = TempDir::new().unwrap();
        let script_path = tmp.path().join("automate.js");
        std::fs::write(&script_path, "document.title").unwrap();

        let op = WebOp::Run {
            script_path: script_path.to_string_lossy().into_owned(),
            url: "https://example.com/start".into(),
        };
        let plan = ObscuraSpawn::plan(&cfg(Some("/opt/obscura"), None), GOVERNED, "/ca.pem", &op)
            .expect("plan");

        // No `run` subcommand and no `--url` flag — obscura has neither.
        assert!(!plan.args.iter().any(|a| a == "run"));
        assert!(!plan.args.iter().any(|a| a == "--url"));

        // `fetch <url>` carries the URL positionally.
        let fetch_idx = plan
            .args
            .iter()
            .position(|a| a == "fetch")
            .expect("fetch subcommand");
        assert_eq!(plan.args[fetch_idx + 1], "https://example.com/start");

        // `--eval` carries the SCRIPT CONTENTS, never the path.
        let eval_idx = plan
            .args
            .iter()
            .position(|a| a == "--eval")
            .expect("--eval flag");
        assert_eq!(plan.args[eval_idx + 1], "document.title");
        assert!(
            !plan
                .args
                .iter()
                .any(|a| a == &script_path.to_string_lossy()),
            "the script PATH must not appear in argv (only its contents)"
        );
    }

    #[test]
    fn plan_run_op_fails_closed_on_unreadable_script() {
        // A Run op whose script cannot be read fails closed with
        // ScriptUnreadable — never a silent empty `--eval`.
        let op = WebOp::Run {
            script_path: "/no/such/script/at/all.js".into(),
            url: "https://example.com/start".into(),
        };
        let err = ObscuraSpawn::plan(&cfg(Some("/opt/obscura"), None), GOVERNED, "/ca.pem", &op)
            .expect_err("must fail closed when the script is unreadable");
        assert!(matches!(err, SpawnPlanError::ScriptUnreadable(_)));
        // The Display carries the actionable context.
        assert!(err.to_string().contains("fetch --eval"), "{err}");
    }

    // --- run end-to-end -----------------------------------------------------

    #[tokio::test]
    async fn run_denies_before_any_feature_check() {
        // A denied op reports a denial in EVERY build (feature on or off),
        // never the "not enabled" error.
        let policy = WebPolicy::default(); // deny everything
        let op = WebOp::Open {
            url: "https://blocked.com/".into(),
        };
        let err = run(
            &policy,
            &cfg(Some("/x"), Some("http://p")),
            "/ca.pem",
            false,
            &op,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("denied"), "{err}");
        assert!(!err.contains("--features obscura"), "{err}");
    }

    // In the default (no-feature) build, an AUTHORIZED op must fail closed with
    // the clear "not enabled" error — not silently no-op, not a denial.
    #[cfg(not(feature = "obscura"))]
    #[tokio::test]
    async fn run_authorized_without_feature_fails_closed() {
        let policy = allow_example();
        let op = WebOp::Open {
            url: "https://example.com/".into(),
        };
        let err = run(
            &policy,
            &cfg(Some("/x"), Some("http://p")),
            "/ca.pem",
            false,
            &op,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("--features obscura"), "{err}");
        assert!(err.contains("Phase A1"), "{err}");
    }
}
