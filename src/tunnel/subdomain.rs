//! Subdomain validation.
//!
//! Faithful port of `internal/tunnel/subdomain.go`. Rejects subdomains that —
//! once lowercased and stripped of `-`/`.` — equal or contain a protected brand
//! name, to prevent phishing-style tunnel names.

use anyhow::{bail, Result};

/// Protected brand names. Ported verbatim from Go's `blockedSubdomains`.
const BLOCKED_SUBDOMAINS: &[&str] = &[
    "paypal",
    "apple",
    "google",
    "microsoft",
    "facebook",
    "instagram",
    "amazon",
    "netflix",
    "spotify",
    "twitter",
    "linkedin",
    "github",
    "dropbox",
    "icloud",
    "chase",
    "wellsfargo",
    "bankofamerica",
    "citibank",
    "coinbase",
    "binance",
    "stripe",
    "venmo",
    "cashapp",
    "zelle",
    "metamask",
    "outlook",
    "hotmail",
    "yahoo",
    "whatsapp",
    "telegram",
    "signal",
    "discord",
    "slack",
    "zoom",
    "docusign",
    "adobe",
    "salesforce",
    "shopify",
    "ebay",
    "walmart",
    "usps",
    "fedex",
    "ups",
    "dhl",
];

/// Validate a requested tunnel subdomain.
///
/// An empty subdomain is allowed (the server assigns one). Otherwise the value
/// is lowercased and stripped of `-` and `.`; if the normalized form equals or
/// contains any protected brand name, the subdomain is rejected with the same
/// error text as Go.
pub fn validate_subdomain(subdomain: &str) -> Result<()> {
    if subdomain.is_empty() {
        return Ok(());
    }

    let lower = subdomain.to_lowercase();
    let normalized = lower.replace('-', "").replace('.', "");

    for brand in BLOCKED_SUBDOMAINS {
        if normalized == *brand || normalized.contains(brand) {
            bail!("subdomain {subdomain:?} is not allowed: resembles a protected brand name");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of subdomain_test.go::TestValidateSubdomain.
    #[test]
    fn test_validate_subdomain() {
        let cases: &[(&str, bool)] = &[
            ("", false),
            ("myapp", false),
            ("cool-project", false),
            ("demo", false),
            ("paypal", true),
            ("pay-pal", true),
            ("paypal-login", true),
            ("my-google-app", true),
            ("apple", true),
            ("facebook", true),
            ("PAYPAL", true),
            ("Amazon", true),
            ("chase-bank", true),
            ("my-stripe-test", true),
        ];

        for &(input, want_err) in cases {
            let got_err = validate_subdomain(input).is_err();
            assert_eq!(
                got_err, want_err,
                "validate_subdomain({input:?}) err = {got_err}, want_err {want_err}"
            );
        }
    }
}
