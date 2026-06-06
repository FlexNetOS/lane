//! `lane domain` — manage custom domains.
//!
//! Faithful port of `cmd/domain.go`. Implements the `add`, `list`, `verify`,
//! and `remove` subcommands against the lane API. The network calls run inside
//! `term::run_steps` step closures (mirroring Go, where the spinner spins while
//! the request is in flight). Those closures are synchronous (`FnOnce`), so the
//! async `reqwest` calls are driven via `block_in_place` + the current Tokio
//! runtime handle.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::config;
use crate::log;
use crate::term;
use crate::term::step::{run_steps, Step};

use super::{DomainArgs, DomainCommands};

/// A custom-domain record returned by the API. JSON tags match the Go struct
/// (`id`, `domain`, `status`, `created_at`).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DomainEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    created_at: String,
}

/// Run the `domain` command, dispatching on the subcommand.
pub async fn run(args: &DomainArgs) -> Result<()> {
    match &args.command {
        DomainCommands::Add { domain, json } => add(domain, *json).await,
        DomainCommands::List { json } => list(*json).await,
        DomainCommands::Verify { domain, json } => verify(domain, *json).await,
        DomainCommands::Remove { domain } => remove(domain).await,
    }
}

// --- subcommands -----------------------------------------------------------

/// `lane domain add <domain>`.
async fn add(domain: &str, json: bool) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;
    let domain = domain.to_string();

    // `--json` skips the spinner + DNS-record block and emits the record as data
    // so scripts can create it programmatically. A failure to add is a hard error
    // (non-zero exit) — there is no record to emit — unlike `verify --json`, where
    // "not verified yet" is a normal `{verified:false}` outcome.
    if json {
        let target_ip = do_add(&token, &domain).await?;
        let payload = AddJson {
            dns: DnsRecord {
                record_type: "A".to_string(),
                name: domain.clone(),
                value: target_ip.clone(),
            },
            domain: domain.clone(),
            target_ip,
        };
        let data = serde_json::to_string_pretty(&payload).context("marshaling JSON")?;
        println!("{data}");
        return Ok(());
    }

    // Shared slot for the step closure to write the resolved target IP into,
    // mirroring Go's captured `var targetIP string`.
    let target_ip = Arc::new(Mutex::new(String::new()));

    let step_domain = domain.clone();
    let step_token = token.clone();
    let step_target = Arc::clone(&target_ip);

    run_steps(vec![Step {
        name: format!("Adding domain {domain}"),
        interactive: false,
        run: Box::new(move || {
            block_on(async move {
                let ip = do_add(&step_token, &step_domain).await?;
                *step_target.lock().unwrap() = ip;
                Ok("done".to_string())
            })
        }),
    }])?;

    let target_ip = target_ip.lock().unwrap().clone();

    println!("\nAdd the following DNS record to verify ownership:\n");
    println!("  Type:  {}", term::bold("A"));
    println!("  Name:  {}", term::bold(&domain));
    println!("  Value: {}\n", term::bold(&target_ip));
    println!(
        "{} If using Cloudflare, disable the proxy (grey cloud / DNS only).",
        term::dim("*")
    );
    println!(
        "{} DNS changes can take a few minutes to propagate.\n",
        term::dim("*")
    );
    println!(
        "Then run: {}",
        term::cyan(format!("lane domain verify {domain}"))
    );

    Ok(())
}

/// Structured result of `lane domain add --json`.
#[derive(Serialize)]
struct AddJson {
    domain: String,
    target_ip: String,
    dns: DnsRecord,
}

/// The DNS A record the user must create to verify ownership.
#[derive(Serialize)]
struct DnsRecord {
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    value: String,
}

/// POST the domain to the API and return the resolved target IP. Mirrors the Go
/// add flow; non-2xx responses surface via [`api_error`].
async fn do_add(token: &str, domain: &str) -> Result<String> {
    let body = serde_json::to_vec(&json!({ "domain": domain }))
        .map_err(|e| anyhow!("encoding request: {e}"))?;

    let client = http_client(Duration::from_secs(10))?;
    let resp = client
        .post(format!("{}/api/domains", config::api_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow!("adding domain: {e}"))?;

    let status = resp.status().as_u16();
    if status != 200 && status != 201 {
        return Err(api_error(resp, "failed to add domain").await);
    }

    #[derive(Default, Deserialize)]
    struct AddResult {
        #[serde(default)]
        target_ip: String,
    }
    let result: AddResult = resp
        .json()
        .await
        .map_err(|e| anyhow!("decoding response: {e}"))?;

    Ok(result.target_ip)
}

/// `lane domain list`.
async fn list(json: bool) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;

    let domains: Arc<Mutex<Vec<DomainEntry>>> = Arc::new(Mutex::new(Vec::new()));

    let step_token = token.clone();
    let step_domains = Arc::clone(&domains);

    run_steps(vec![Step {
        name: "Fetching domains".to_string(),
        interactive: false,
        run: Box::new(move || {
            let fetched = block_on(fetch_domains(&step_token))?;
            *step_domains.lock().unwrap() = fetched;
            Ok("done".to_string())
        }),
    }])?;

    let domains = domains.lock().unwrap();

    if json {
        let data = serde_json::to_string_pretty(&*domains).context("marshaling JSON")?;
        println!("{data}");
        return Ok(());
    }

    if domains.is_empty() {
        println!("No custom domains. Use 'lane domain add <domain>' to add one.");
        return Ok(());
    }

    println!();

    let mut rows: Vec<Vec<String>> = Vec::new();
    for d in domains.iter() {
        let status = match d.status.as_str() {
            "active" => term::green("● active"),
            "issuing_cert" => term::cyan("● generating cert"),
            _ => term::yellow("● pending"),
        };
        let added = match DateTime::parse_from_rfc3339(&d.created_at) {
            Ok(t) => log::format_time_ago(t.with_timezone(&Local)),
            Err(_) => d.created_at.clone(),
        };
        rows.push(vec![d.domain.clone(), status, added]);
    }

    println!(
        "{}",
        term::table::render_table(&["DOMAIN", "STATUS", "ADDED"], &rows)
    );

    Ok(())
}

/// `lane domain verify <domain>`.
async fn verify(domain: &str, json: bool) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;
    let domain = domain.to_string();

    // `--json` skips the spinner UI and emits a structured outcome so CI/scripts
    // can consume it. A domain-level failure (not found, API rejection, transport)
    // becomes `{verified:false, error}` rather than a human error line; the
    // process still exits 0 — consumers branch on the `verified` field.
    if json {
        let payload = match do_verify(&token, &domain).await {
            Ok(status) => VerifyJson {
                domain: domain.clone(),
                verified: status == "active",
                status: Some(status),
                error: None,
            },
            Err(e) => VerifyJson {
                domain: domain.clone(),
                verified: false,
                status: None,
                error: Some(e.to_string()),
            },
        };
        let data = serde_json::to_string_pretty(&payload).context("marshaling JSON")?;
        println!("{data}");
        return Ok(());
    }

    let step_token = token.clone();
    let step_domain = domain.clone();

    run_steps(vec![Step {
        name: format!("Verifying DNS for {domain}"),
        interactive: false,
        run: Box::new(move || {
            block_on(async move {
                let status = do_verify(&step_token, &step_domain).await?;
                Ok(verify_message(&status))
            })
        }),
    }])
}

/// Structured result of `lane domain verify --json`.
#[derive(Serialize)]
struct VerifyJson {
    domain: String,
    verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Fetch the domain, POST its verify endpoint, and return the raw API status
/// string (`"active"`, `"issuing_cert"`, …; empty if the body is undecodable).
/// Errors mirror Go: not-found and non-200 responses surface their message.
async fn do_verify(token: &str, domain: &str) -> Result<String> {
    let domains = fetch_domains(token).await?;
    let domain_id = find_domain_id(&domains, domain);
    if domain_id.is_empty() {
        return Err(anyhow!(
            "domain {domain} not found — use 'lane domain add' first"
        ));
    }

    let client = http_client(Duration::from_secs(10))?;
    let resp = client
        .post(format!(
            "{}/api/domains/{domain_id}/verify",
            config::api_base_url()
        ))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| anyhow!("verifying domain: {e}"))?;

    let status = resp.status().as_u16();
    if status != 200 {
        let status_line = status_string(resp.status());
        let body_bytes = resp.bytes().await.unwrap_or_default();
        let mut msg = String::from_utf8_lossy(&body_bytes).trim().to_string();
        if msg.is_empty() {
            msg = status_line;
        }
        return Err(anyhow!("{msg}"));
    }

    #[derive(Default, Deserialize)]
    struct VerifyResult {
        #[serde(default)]
        status: String,
    }
    match resp.json::<VerifyResult>().await {
        Ok(v) => Ok(v.status),
        Err(_) => Ok(String::new()),
    }
}

/// Map a raw verify status string to the human spinner message (Go parity).
fn verify_message(status: &str) -> String {
    match status {
        "active" => "verified".to_string(),
        "issuing_cert" => "issuing certificate (this may take a moment)".to_string(),
        _ => "done".to_string(),
    }
}

/// `lane domain remove <domain>`.
async fn remove(domain: &str) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;
    let domain = domain.to_string();

    let domains = fetch_domains(&token).await?;

    let domain_id = find_domain_id(&domains, &domain);
    if domain_id.is_empty() {
        return Err(anyhow!("domain {domain} not found"));
    }

    let delete_url = format!("{}/api/domains/{domain_id}", config::api_base_url());

    let client = http_client(Duration::from_secs(10))?;
    let resp = client
        .delete(&delete_url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| anyhow!("removing domain: {e}"))?;

    let status = resp.status().as_u16();

    if status == 409 {
        // Drop the response body (Go's `resp.Body.Close()`).
        drop(resp);

        println!(
            "\n{} has an active tunnel. Removing it will disconnect the tunnel.",
            term::bold(&domain)
        );
        if !term::confirm_prompt("Continue?") {
            return Ok(());
        }

        let step_token = token.clone();
        let step_domain = domain.clone();
        let force_url = format!("{delete_url}?force=true");

        return run_steps(vec![Step {
            name: format!("Removing domain {step_domain}"),
            interactive: false,
            run: Box::new(move || {
                block_on(async move {
                    let client = http_client(Duration::from_secs(10))?;
                    let resp = client
                        .delete(&force_url)
                        .header("Authorization", format!("Bearer {step_token}"))
                        .send()
                        .await
                        .map_err(|e| anyhow!("removing domain: {e}"))?;

                    let status = resp.status().as_u16();
                    if status != 200 && status != 204 {
                        return Err(api_error(resp, "failed to remove domain").await);
                    }

                    Ok("done".to_string())
                })
            }),
        }]);
    }

    if status != 200 && status != 204 {
        return Err(api_error(resp, "failed to remove domain").await);
    }

    println!("\n{} Removed {domain}", term::check_mark());
    Ok(())
}

// --- helpers ---------------------------------------------------------------

/// Find a domain's ID by case-insensitive name match. Mirrors Go's
/// `findDomainID`; returns an empty string when not found.
fn find_domain_id(domains: &[DomainEntry], name: &str) -> String {
    for d in domains {
        if d.domain.eq_ignore_ascii_case(name) {
            return d.id.clone();
        }
    }
    String::new()
}

/// Build an error from a non-OK API response. Mirrors Go's `apiError`: decode a
/// `{ "error": "..." }` body and format `"{action}: {error}"`, falling back to
/// the HTTP status line as `"{action}: {status}"`.
async fn api_error(resp: reqwest::Response, action: &str) -> anyhow::Error {
    let status_line = status_string(resp.status());

    #[derive(Default, Deserialize)]
    struct ErrResp {
        #[serde(default)]
        error: String,
    }

    let err_resp: ErrResp = resp.json().await.unwrap_or_default();

    if !err_resp.error.is_empty() {
        anyhow!("{action}: {}", err_resp.error)
    } else {
        anyhow!("{action}: {status_line}")
    }
}

/// Fetch the caller's custom domains. Mirrors Go's `fetchDomains` (5s timeout).
async fn fetch_domains(token: &str) -> Result<Vec<DomainEntry>> {
    let client = http_client(Duration::from_secs(5))?;
    let resp = client
        .get(format!("{}/api/domains", config::api_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| anyhow!("fetching domains: {e}"))?;

    if resp.status().as_u16() != 200 {
        return Err(anyhow!(
            "failed to fetch domains: {}",
            status_string(resp.status())
        ));
    }

    #[derive(Default, Deserialize)]
    struct Body {
        #[serde(default)]
        domains: Vec<DomainEntry>,
    }
    let body: Body = resp
        .json()
        .await
        .map_err(|e| anyhow!("decoding response: {e}"))?;

    Ok(body.domains)
}

/// Build a reqwest client with the given request timeout, surfacing the build
/// error in Go's "creating request" idiom.
fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| anyhow!("creating request: {e}"))
}

/// Render an HTTP status like Go's `resp.Status` (e.g. `"404 Not Found"`).
fn status_string(status: reqwest::StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => format!("{}", status.as_u16()),
    }
}

/// Drive an async future to completion from within a synchronous step closure.
///
/// The CLI runs under a multi-threaded `#[tokio::main]`; `block_in_place` lets
/// the current worker thread block on the future without stalling the runtime.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_domain_id_is_case_insensitive() {
        let domains = vec![
            DomainEntry {
                id: "1".into(),
                domain: "Example.COM".into(),
                ..Default::default()
            },
            DomainEntry {
                id: "2".into(),
                domain: "other.dev".into(),
                ..Default::default()
            },
        ];
        assert_eq!(find_domain_id(&domains, "example.com"), "1");
        assert_eq!(find_domain_id(&domains, "EXAMPLE.com"), "1");
        assert_eq!(find_domain_id(&domains, "other.dev"), "2");
        assert_eq!(find_domain_id(&domains, "missing.io"), "");
    }

    #[test]
    fn find_domain_id_empty_list() {
        assert_eq!(find_domain_id(&[], "anything"), "");
    }

    #[test]
    fn status_string_matches_go_format() {
        assert_eq!(
            status_string(reqwest::StatusCode::NOT_FOUND),
            "404 Not Found"
        );
        assert_eq!(
            status_string(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
            "500 Internal Server Error"
        );
        assert_eq!(status_string(reqwest::StatusCode::OK), "200 OK");
    }

    #[test]
    fn domain_entry_deserializes_go_json_tags() {
        let raw = r#"{"id":"d1","domain":"app.example.com","status":"active","created_at":"2024-01-01T00:00:00Z"}"#;
        let d: DomainEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(d.id, "d1");
        assert_eq!(d.domain, "app.example.com");
        assert_eq!(d.status, "active");
        assert_eq!(d.created_at, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn domain_entry_defaults_for_missing_fields() {
        let raw = r#"{"domain":"x.test"}"#;
        let d: DomainEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(d.id, "");
        assert_eq!(d.domain, "x.test");
        assert_eq!(d.status, "");
        assert_eq!(d.created_at, "");
    }

    #[test]
    fn domain_entry_serializes_with_go_json_tags() {
        let d = DomainEntry {
            id: "d1".into(),
            domain: "app.example.com".into(),
            status: "active".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&d).expect("serialize");
        assert!(json.contains("\"id\":\"d1\""));
        assert!(json.contains("\"domain\":\"app.example.com\""));
        assert!(json.contains("\"status\":\"active\""));
        assert!(json.contains("\"created_at\":\"2024-01-01T00:00:00Z\""));
    }

    #[test]
    fn add_json_shape_has_renamed_type_field() {
        let payload = AddJson {
            domain: "app.test".into(),
            target_ip: "203.0.113.7".into(),
            dns: DnsRecord {
                record_type: "A".into(),
                name: "app.test".into(),
                value: "203.0.113.7".into(),
            },
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(json.contains("\"domain\":\"app.test\""));
        assert!(json.contains("\"target_ip\":\"203.0.113.7\""));
        // DNS record nests with a `type` key (serde rename of record_type).
        assert!(json.contains("\"dns\":{"));
        assert!(json.contains("\"type\":\"A\""));
        assert!(json.contains("\"name\":\"app.test\""));
        assert!(json.contains("\"value\":\"203.0.113.7\""));
        assert!(!json.contains("record_type"));
    }

    #[test]
    fn verify_message_maps_status_like_go() {
        assert_eq!(verify_message("active"), "verified");
        assert_eq!(
            verify_message("issuing_cert"),
            "issuing certificate (this may take a moment)"
        );
        assert_eq!(verify_message("pending"), "done");
        assert_eq!(verify_message(""), "done");
    }

    #[test]
    fn verify_json_success_shape() {
        let ok = VerifyJson {
            domain: "app.test".into(),
            verified: true,
            status: Some("active".into()),
            error: None,
        };
        let json = serde_json::to_string(&ok).expect("serialize");
        assert!(json.contains("\"domain\":\"app.test\""));
        assert!(json.contains("\"verified\":true"));
        assert!(json.contains("\"status\":\"active\""));
        // error omitted when None.
        assert!(!json.contains("error"));
    }

    #[test]
    fn verify_json_error_shape() {
        let err = VerifyJson {
            domain: "missing.test".into(),
            verified: false,
            status: None,
            error: Some("domain missing.test not found — use 'lane domain add' first".into()),
        };
        let json = serde_json::to_string(&err).expect("serialize");
        assert!(json.contains("\"verified\":false"));
        assert!(json.contains("\"error\":\"domain missing.test not found"));
        // status omitted when None.
        assert!(!json.contains("status"));
    }

    #[test]
    fn domain_list_json_is_an_array() {
        // `lane domain list --json` emits a top-level JSON array; empty -> "[]".
        let empty: Vec<DomainEntry> = Vec::new();
        assert_eq!(serde_json::to_string(&empty).expect("serialize"), "[]");

        let domains = vec![DomainEntry {
            domain: "x.test".into(),
            ..Default::default()
        }];
        let json = serde_json::to_string_pretty(&domains).expect("serialize");
        assert!(json.trim_start().starts_with('['));
        assert!(json.contains("\"domain\": \"x.test\""));
    }
}
