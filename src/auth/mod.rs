//! auth — CLI authentication and credential storage.
//!
//! Faithful port of `internal/auth/auth.go` from the Go tool `slim`. Handles the
//! browser-based OAuth CLI login flow, validation of stored tokens against the
//! API, on-disk credential storage under `~/.lane/auth.json`, and the persisted
//! tunnel token under `~/.lane/tunnel-token`.
//!
//! This module is async: HTTP requests use the `reqwest` async client (mirroring
//! Go's `net/http`), and the browser is opened via the cross-platform `open`
//! crate (Go switched on `runtime.GOOS` between `open`/`xdg-open`).

use std::time::Duration;

use anyhow::anyhow;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::httperr;

/// Stored credentials. Serialized to `~/.lane/auth.json`.
///
/// JSON tags match the Go struct: `token`, `name`, `email`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

/// Log in, reusing valid stored credentials when possible.
///
/// Mirrors Go's `Login`: if an existing auth file holds a token that still
/// validates against the API, return it unchanged. Otherwise clear any stale
/// credentials and run the browser OAuth flow.
pub async fn login() -> anyhow::Result<Info> {
    let existing = load_auth().ok().flatten();

    if let Some(info) = &existing {
        if validate_token(&info.token).await {
            return Ok(info.clone());
        }
    }

    if existing.is_some() {
        let _ = logout();
    }

    start_oauth_login().await
}

/// Returns `true` when `GET {api}/api/me` with the bearer token responds 200.
///
/// Any transport error (or non-200 status) yields `false`, exactly like Go's
/// `validateToken`.
async fn validate_token(token: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let resp = match client
        .get(format!("{}/api/me", config::api_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return false,
    };

    resp.status().as_u16() == 200
}

/// Response from `POST {api}/api/auth/cli`.
#[derive(Deserialize)]
struct CliResponse {
    #[serde(default)]
    code: String,
    #[serde(default)]
    url: String,
}

/// Start the browser-based OAuth login flow.
///
/// Mirrors Go's `startOAuthLogin`: request a CLI auth code + URL, open the URL
/// in a browser (falling back to printing it), then poll for completion.
async fn start_oauth_login() -> anyhow::Result<Info> {
    let client = reqwest::Client::new();

    let resp = match client
        .post(format!("{}/api/auth/cli", config::api_base_url()))
        .header("Content-Type", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Err(httperr::wrap("failed to start login", e)),
    };

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.bytes().await.unwrap_or_default();
        return Err(anyhow!(
            "failed to start login: {}",
            httperr::from_response_blocking(status, &body)
        ));
    }

    let cli_resp: CliResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => return Err(anyhow!("failed to parse response: {}", e)),
    };

    println!("Opening browser to log in...");
    if open::that(&cli_resp.url).is_err() {
        println!("Could not open browser. Please visit:\n  {}", cli_resp.url);
    }

    poll_for_completion(&cli_resp.code).await
}

/// `user` object inside the poll response.
#[derive(Default, Deserialize)]
struct PollUser {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

/// Response from `GET {api}/api/auth/cli/poll`.
#[derive(Default, Deserialize)]
struct PollResult {
    #[serde(default)]
    status: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    user: PollUser,
}

/// The most recent poll failure, retained so it can be surfaced on timeout.
///
/// Transport errors keep the original `reqwest::Error` so [`httperr::wrap`] can
/// classify network conditions (matching Go's `net.Error` checks); HTTP-status
/// and decode failures are kept as preformatted error strings.
enum PollError {
    Transport(reqwest::Error),
    Message(String),
}

/// Poll for OAuth completion until a 30s deadline, mirroring Go's
/// `pollForCompletion`.
async fn poll_for_completion(code: &str) -> anyhow::Result<Info> {
    println!("Waiting for authentication...");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| httperr::wrap("login failed", e))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);

    let mut last_poll_err: Option<PollError> = None;

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;

        let poll_resp = match client
            .get(format!(
                "{}/api/auth/cli/poll?code={}",
                config::api_base_url(),
                code
            ))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_poll_err = Some(PollError::Transport(e));
                continue;
            }
        };

        let status = poll_resp.status().as_u16();
        if status != 200 {
            let body = poll_resp.bytes().await.unwrap_or_default();
            last_poll_err = Some(PollError::Message(
                httperr::from_response_blocking(status, &body).to_string(),
            ));
            continue;
        }

        let result: PollResult = match poll_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                last_poll_err = Some(PollError::Message(format!("decoding poll response: {e}")));
                continue;
            }
        };

        if result.status != "complete" {
            continue;
        }

        let auth = Info {
            token: result.token,
            name: result.user.name,
            email: result.user.email,
        };

        save_auth(&auth).map_err(|e| anyhow!("failed to save credentials: {}", e))?;

        return Ok(auth);
    }

    match last_poll_err {
        Some(PollError::Transport(e)) => Err(httperr::wrap("login failed", e)),
        Some(PollError::Message(msg)) => Err(anyhow!("login failed: {}", msg)),
        None => Err(anyhow!("login timed out — please try again")),
    }
}

/// Return stored credentials, erroring if not logged in.
///
/// Mirrors Go's `Require`.
pub fn require() -> anyhow::Result<Info> {
    match load_auth()? {
        Some(info) => Ok(info),
        None => Err(anyhow!("not logged in — run 'lane login' first")),
    }
}

/// Load stored credentials from `~/.lane/auth.json`.
///
/// Returns `Ok(None)` when the file does not exist (matching Go's
/// `os.IsNotExist` branch returning `nil, nil`).
pub fn load_auth() -> anyhow::Result<Option<Info>> {
    let data = match std::fs::read(config::auth_path()) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow!("failed to read auth file: {}", e)),
    };

    let info: Info =
        serde_json::from_slice(&data).map_err(|e| anyhow!("failed to parse auth file: {}", e))?;

    Ok(Some(info))
}

/// Remove stored credentials. A missing file is not an error.
///
/// Mirrors Go's `Logout`.
pub fn logout() -> anyhow::Result<()> {
    match std::fs::remove_file(config::auth_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("failed to remove auth file: {}", e)),
    }
}

/// Load the persisted tunnel token, generating and persisting one if absent.
///
/// Mirrors Go's `LoadOrCreateToken`: read `~/.lane/tunnel-token`; if present and
/// non-empty return it; otherwise generate 32 random bytes, hex-encode them,
/// create `~/.lane` (0755) and write the token with 0600 permissions.
pub fn load_or_create_token() -> anyhow::Result<String> {
    let token_path = config::tunnel_token_path();

    if let Ok(data) = std::fs::read(&token_path) {
        let token = String::from_utf8_lossy(&data).into_owned();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let token = hex::encode(b);

    std::fs::create_dir_all(config::dir()).map_err(|e| anyhow!("creating config dir: {}", e))?;
    write_private(&token_path, token.as_bytes())
        .map_err(|e| anyhow!("writing tunnel token: {}", e))?;

    Ok(token)
}

/// Persist credentials to `~/.lane/auth.json` with 0600 permissions, creating
/// `~/.lane` (0755) first. Mirrors Go's `saveAuth`.
fn save_auth(auth: &Info) -> anyhow::Result<()> {
    std::fs::create_dir_all(config::dir())?;
    let data = serde_json::to_vec(auth)?;
    write_private(&config::auth_path(), &data)?;
    Ok(())
}

/// Write `data` to `path` with mode 0600 (owner read/write only), matching Go's
/// `os.WriteFile(path, data, 0600)`.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)?;
    set_mode_0600(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Set `HOME` to an isolated temp dir so `config::dir()` resolves there.
    fn with_temp_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        tmp
    }

    #[test]
    fn info_serde_round_trip() {
        let info = Info {
            token: "abc123".to_string(),
            name: "Ada Lovelace".to_string(),
            email: "ada@example.com".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        // JSON tags must match the Go struct exactly.
        assert!(json.contains("\"token\":\"abc123\""));
        assert!(json.contains("\"name\":\"Ada Lovelace\""));
        assert!(json.contains("\"email\":\"ada@example.com\""));

        let decoded: Info = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, info);
    }

    #[test]
    fn info_deserialize_from_go_shape() {
        // Mirrors the on-disk auth.json produced by the Go tool.
        let raw = r#"{"token":"tok","name":"N","email":"e@x.com"}"#;
        let info: Info = serde_json::from_str(raw).unwrap();
        assert_eq!(info.token, "tok");
        assert_eq!(info.name, "N");
        assert_eq!(info.email, "e@x.com");
    }

    #[test]
    #[serial]
    fn load_or_create_token_persists() {
        let _tmp = with_temp_home();

        // First call generates and persists a token.
        let first = load_or_create_token().unwrap();
        assert_eq!(first.len(), 64, "32 random bytes hex-encoded => 64 chars");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));

        // The token file exists with 0600 permissions.
        let path = config::tunnel_token_path();
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Second call returns the same persisted token.
        let second = load_or_create_token().unwrap();
        assert_eq!(first, second);
    }

    #[test]
    #[serial]
    fn load_auth_missing_returns_none() {
        let _tmp = with_temp_home();
        assert!(load_auth().unwrap().is_none());
    }

    #[test]
    #[serial]
    fn save_then_load_round_trip() {
        let _tmp = with_temp_home();

        let info = Info {
            token: "t".to_string(),
            name: "n".to_string(),
            email: "e".to_string(),
        };
        save_auth(&info).unwrap();

        let loaded = load_auth().unwrap().unwrap();
        assert_eq!(loaded, info);

        // auth.json must be 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(config::auth_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    #[serial]
    fn logout_missing_is_ok() {
        let _tmp = with_temp_home();
        // No auth file present; logout must succeed.
        assert!(logout().is_ok());
    }

    #[test]
    #[serial]
    fn require_errors_when_not_logged_in() {
        let _tmp = with_temp_home();
        let err = require().unwrap_err();
        assert_eq!(err.to_string(), "not logged in — run 'lane login' first");
    }

    #[test]
    #[serial]
    fn save_logout_then_load_none() {
        let _tmp = with_temp_home();
        let info = Info {
            token: "t".to_string(),
            name: "n".to_string(),
            email: "e".to_string(),
        };
        save_auth(&info).unwrap();
        assert!(load_auth().unwrap().is_some());

        logout().unwrap();
        assert!(load_auth().unwrap().is_none());
    }
}
