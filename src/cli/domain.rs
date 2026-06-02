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

use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use serde::Deserialize;
use serde_json::json;

use crate::auth;
use crate::config;
use crate::log;
use crate::term;
use crate::term::step::{run_steps, Step};

use super::{DomainArgs, DomainCommands};

/// A custom-domain record returned by the API. JSON tags match the Go struct
/// (`id`, `domain`, `status`, `created_at`).
#[derive(Clone, Debug, Default, Deserialize)]
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
        DomainCommands::Add { domain } => add(domain).await,
        DomainCommands::List => list().await,
        DomainCommands::Verify { domain } => verify(domain).await,
        DomainCommands::Remove { domain } => remove(domain).await,
    }
}

// --- subcommands -----------------------------------------------------------

/// `lane domain add <domain>`.
async fn add(domain: &str) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;
    let domain = domain.to_string();

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
                let body = serde_json::to_vec(&json!({ "domain": step_domain }))
                    .map_err(|e| anyhow!("encoding request: {e}"))?;

                let client = http_client(Duration::from_secs(10))?;
                let resp = client
                    .post(format!("{}/api/domains", config::api_base_url()))
                    .header("Authorization", format!("Bearer {step_token}"))
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

                *step_target.lock().unwrap() = result.target_ip;

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

/// `lane domain list`.
async fn list() -> Result<()> {
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
async fn verify(domain: &str) -> Result<()> {
    let info = auth::require()?;
    let token = info.token;
    let domain = domain.to_string();

    let step_token = token.clone();
    let step_domain = domain.clone();

    run_steps(vec![Step {
        name: format!("Verifying DNS for {domain}"),
        interactive: false,
        run: Box::new(move || {
            block_on(async move {
                let domains = fetch_domains(&step_token).await?;
                let domain_id = find_domain_id(&domains, &step_domain);
                if domain_id.is_empty() {
                    return Err(anyhow!(
                        "domain {step_domain} not found — use 'lane domain add' first"
                    ));
                }

                let client = http_client(Duration::from_secs(10))?;
                let resp = client
                    .post(format!(
                        "{}/api/domains/{domain_id}/verify",
                        config::api_base_url()
                    ))
                    .header("Authorization", format!("Bearer {step_token}"))
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
                let result: VerifyResult = match resp.json().await {
                    Ok(v) => v,
                    Err(_) => return Ok("done".to_string()),
                };

                Ok(match result.status.as_str() {
                    "active" => "verified".to_string(),
                    "issuing_cert" => "issuing certificate (this may take a moment)".to_string(),
                    _ => "done".to_string(),
                })
            })
        }),
    }])
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
}
