//! Pure, deny-by-default trusted-node allowlist — the always-compiled security
//! core of the cross-machine relay (ADR-0002 §2 "Identity & trust").
//!
//! A lane node accepts an inbound relay connection **only** from a NodeId on an
//! explicit allowlist. This module is the **mechanism**, built fail-closed and
//! with **no** dependency on iroh: it operates on NodeIds as normalized strings
//! so the trust check (the load-bearing security decision) is compiled and
//! exhaustively tested in **every** build, including the default no-`relay` one.
//!
//! # Deny-by-default
//!
//! An empty allowlist trusts **nothing**. A NodeId is trusted only when it
//! matches (after normalization) an entry on the list. There is no wildcard, no
//! "trust all", and no implicit self-trust — exactly the SSRF-safe posture the
//! ADR mandates for the new inbound attack surface.
//!
//! # NodeId shape
//!
//! An iroh NodeId is the ed25519 public key rendered as 64 lowercase hex
//! characters. [`normalize_node_id`] lowercases and trims, and
//! [`parse_node_id`] additionally validates the 64-hex-char shape so a
//! malformed allowlist entry or a malformed remote id can be rejected without
//! pulling in iroh. (The live endpoint additionally parses the id through iroh's
//! own `EndpointId::from_str`, which is the cryptographic source of truth; this
//! pure check is a fast, dependency-free pre-filter and the basis of the
//! always-compiled tests.)

/// The exact character length of a NodeId rendered as lowercase hex (an ed25519
/// public key is 32 bytes ⇒ 64 hex characters).
pub const NODE_ID_HEX_LEN: usize = 64;

/// Lowercase and trim a NodeId string into its canonical comparison form. Does
/// **not** validate the shape — use [`parse_node_id`] when a malformed id must be
/// rejected.
pub fn normalize_node_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

/// Validate and canonicalize a NodeId string: it must be exactly
/// [`NODE_ID_HEX_LEN`] lowercase hex characters after normalization. Returns the
/// normalized id, or an error describing why it is malformed.
///
/// This is dependency-free (no iroh) so it can run in any build; the live
/// endpoint still re-parses through iroh's `EndpointId` for the cryptographic
/// guarantee.
pub fn parse_node_id(id: &str) -> Result<String, String> {
    let norm = normalize_node_id(id);
    if norm.is_empty() {
        return Err("empty node id".to_string());
    }
    if norm.len() != NODE_ID_HEX_LEN {
        return Err(format!(
            "invalid node id length: expected {NODE_ID_HEX_LEN} hex chars, got {}",
            norm.len()
        ));
    }
    if !norm.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid node id: must be hexadecimal".to_string());
    }
    Ok(norm)
}

/// `true` if `node_id` is trusted by `allowlist` (deny-by-default).
///
/// The decision is normalized on both sides (case-insensitive, trimmed). An
/// **empty** `allowlist` trusts nothing — the single most important property of
/// this function and the inbound-security posture of the whole relay. Entries
/// that are not themselves valid NodeIds never match a valid remote id.
pub fn is_trusted(allowlist: &[String], node_id: &str) -> bool {
    let want = normalize_node_id(node_id);
    if want.is_empty() {
        return false;
    }
    allowlist
        .iter()
        .any(|entry| normalize_node_id(entry) == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const ID_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    #[test]
    fn empty_allowlist_trusts_nothing() {
        // The load-bearing deny-by-default property.
        let allow: Vec<String> = Vec::new();
        assert!(!is_trusted(&allow, ID_A));
        assert!(!is_trusted(&allow, ID_B));
        assert!(!is_trusted(&allow, ""));
    }

    #[test]
    fn trusts_an_exact_listed_id() {
        let allow = vec![ID_A.to_string()];
        assert!(is_trusted(&allow, ID_A));
    }

    #[test]
    fn does_not_trust_an_unlisted_id() {
        let allow = vec![ID_A.to_string()];
        assert!(!is_trusted(&allow, ID_B));
    }

    #[test]
    fn match_is_case_and_whitespace_insensitive() {
        let allow = vec![format!("  {}  ", ID_A.to_uppercase())];
        assert!(is_trusted(&allow, ID_A));
        assert!(is_trusted(&allow, &ID_A.to_uppercase()));
    }

    #[test]
    fn empty_remote_id_is_never_trusted_even_if_listed() {
        // A blank entry must not become a "trust empty" wildcard.
        let allow = vec![String::new(), "   ".to_string()];
        assert!(!is_trusted(&allow, ""));
        assert!(!is_trusted(&allow, "   "));
    }

    #[test]
    fn multiple_entries_match_any() {
        let allow = vec![ID_A.to_string(), ID_B.to_string()];
        assert!(is_trusted(&allow, ID_A));
        assert!(is_trusted(&allow, ID_B));
        assert!(!is_trusted(
            &allow,
            "9999999999999999999999999999999999999999999999999999999999999999"
        ));
    }

    #[test]
    fn parse_node_id_accepts_valid_hex() {
        assert_eq!(parse_node_id(ID_A).unwrap(), ID_A);
        // Uppercase + surrounding whitespace normalize to lowercase, trimmed.
        assert_eq!(
            parse_node_id(&format!("  {}  ", ID_A.to_uppercase())).unwrap(),
            ID_A
        );
    }

    #[test]
    fn parse_node_id_rejects_bad_shapes() {
        assert!(parse_node_id("").is_err());
        assert!(parse_node_id("   ").is_err());
        // Too short.
        assert!(parse_node_id("abc123").is_err());
        // Right length, non-hex char.
        let bad = "z".repeat(NODE_ID_HEX_LEN);
        assert!(parse_node_id(&bad).is_err());
        // Too long.
        let long = "a".repeat(NODE_ID_HEX_LEN + 1);
        assert!(parse_node_id(&long).is_err());
    }

    #[test]
    fn normalize_is_idempotent() {
        let once = normalize_node_id(&format!("  {}  ", ID_A.to_uppercase()));
        assert_eq!(normalize_node_id(&once), once);
        assert_eq!(once, ID_A);
    }
}
