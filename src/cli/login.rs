//! `lane login` — authenticate with your lane account.
//!
//! Faithful port of `cmd/login.go`. Loads any existing credentials, runs the
//! browser-based OAuth login flow, then reports whether the user was already
//! logged in (same token) or freshly logged in.

use anyhow::Result;

use crate::auth;

/// Run the `login` command.
pub async fn run() -> Result<()> {
    // Best-effort load of existing credentials (ignored on error, matching Go's
    // `existing, _ := auth.LoadAuth()`).
    let existing = auth::load_auth().ok().flatten();

    let info = auth::login().await?;

    if let Some(existing) = &existing {
        if existing.token == info.token {
            println!("Already logged in as {} ({})", info.name, info.email);
            return Ok(());
        }
    }

    println!("Logged in as {} ({})", info.name, info.email);
    Ok(())
}
