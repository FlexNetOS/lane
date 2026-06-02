//! `lane logout` — log out of your lane account.
//!
//! Faithful port of `cmd/logout.go`. Best-effort revokes the stored token on
//! the server (a 5s `DELETE /api/auth/token` with the bearer token), then
//! removes the local credentials.

use std::time::Duration;

use anyhow::Result;

use crate::auth;
use crate::config;

/// Run the `logout` command.
pub async fn run() -> Result<()> {
    let info = auth::load_auth()?;

    if let Some(info) = &info {
        if !info.token.is_empty() {
            revoke_token(&info.token).await;
        }
    }

    auth::logout()?;

    println!("Logged out.");
    Ok(())
}

/// Best-effort server-side token revocation. Mirrors Go's `revokeToken`: every
/// failure (client build, request, transport) is silently ignored.
async fn revoke_token(token: &str) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    let _ = client
        .delete(format!("{}/api/auth/token", config::api_base_url()))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await;
}
