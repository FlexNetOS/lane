//! Node identity for the relay (ADR-0002 §2): a persistent iroh `SecretKey`
//! stored at `~/.lane/relay/node.key`, from which the stable NodeId (the
//! ed25519 public key) is derived.
//!
//! The on-disk layout (pure path helpers) is always compiled so the rest of lane
//! can reason about where the key lives without the `relay` feature. The actual
//! key load/generate (which needs the iroh `SecretKey` type) is gated behind the
//! `relay` feature.

use std::path::PathBuf;

/// The relay state directory, `~/.lane/relay`.
pub fn relay_dir() -> PathBuf {
    crate::config::dir().join("relay")
}

/// The persisted node-identity key file, `~/.lane/relay/node.key`. It holds the
/// 32-byte ed25519 secret rendered as 64 lowercase hex characters and is created
/// `0600` on first `lane relay up`.
pub fn node_key_path() -> PathBuf {
    relay_dir().join("node.key")
}

/// Live, iroh-backed identity operations (feature-gated).
#[cfg(feature = "relay")]
pub use live::*;

#[cfg(feature = "relay")]
mod live {
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    use anyhow::{Context, Result};
    use iroh::SecretKey;

    use super::{node_key_path, relay_dir};

    /// Load the persisted node identity, or generate and persist a fresh one on
    /// first use. The key file is created `0600` (private), under `~/.lane/relay`
    /// (`0700`).
    ///
    /// Returns the loaded/created [`SecretKey`]; its
    /// [`public`](SecretKey::public) is the stable NodeId.
    pub fn load_or_generate_secret_key() -> Result<SecretKey> {
        let path = node_key_path();
        if let Ok(contents) = fs::read_to_string(&path) {
            let bytes = decode_hex32(contents.trim())
                .with_context(|| format!("parsing node key at {}", path.display()))?;
            return Ok(SecretKey::from_bytes(&bytes));
        }

        // No key yet (or unreadable): generate and persist a new one.
        let key = SecretKey::generate();
        persist_secret_key(&key).context("persisting new node key")?;
        Ok(key)
    }

    /// Write `key` to `~/.lane/relay/node.key` as 64-char lowercase hex, dir
    /// `0700`, file `0600`.
    pub fn persist_secret_key(key: &SecretKey) -> Result<()> {
        let dir = relay_dir();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating relay dir {}", dir.display()))?;

        // The secret is the 32 raw bytes; persist them as lowercase hex (no new
        // dependency, symmetric with `decode_hex32` on load).
        let encoded = encode_hex(&key.to_bytes());

        let path = node_key_path();
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts
            .open(&path)
            .with_context(|| format!("writing node key {}", path.display()))?;
        use std::io::Write;
        f.write_all(encoded.as_bytes())
            .with_context(|| format!("writing node key {}", path.display()))?;
        Ok(())
    }

    /// The NodeId (public key) string for a secret key, as 64-char lowercase hex.
    pub fn node_id_string(key: &SecretKey) -> String {
        key.public().to_string()
    }

    /// Encode bytes as lowercase hex.
    fn encode_hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Decode exactly 32 lowercase/uppercase hex bytes into a `[u8; 32]`.
    fn decode_hex32(s: &str) -> Result<[u8; 32]> {
        let s = s.trim();
        if s.len() != 64 {
            anyhow::bail!("expected 64 hex chars for a 32-byte key, got {}", s.len());
        }
        let mut out = [0u8; 32];
        let bytes = s.as_bytes();
        for (i, slot) in out.iter_mut().enumerate() {
            let hi = hex_val(bytes[i * 2])?;
            let lo = hex_val(bytes[i * 2 + 1])?;
            *slot = (hi << 4) | lo;
        }
        Ok(out)
    }

    /// One hex digit → its nibble value.
    fn hex_val(b: u8) -> Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            other => anyhow::bail!("invalid hex digit: {}", other as char),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::relay::allowlist::parse_node_id;

        fn isolate_home() -> tempfile::TempDir {
            let tmp = tempfile::TempDir::new().unwrap();
            std::env::set_var("HOME", tmp.path());
            tmp
        }

        #[test]
        #[serial_test::serial]
        fn generates_persists_and_reloads_a_stable_identity() {
            let _home = isolate_home();

            // First call generates + persists.
            let key1 = load_or_generate_secret_key().expect("generate");
            let id1 = node_id_string(&key1);
            // The derived NodeId is a valid 64-hex-char id.
            assert_eq!(parse_node_id(&id1).unwrap(), id1);

            // The key file exists and is 0600.
            let path = node_key_path();
            assert!(path.exists(), "node.key should be persisted");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "node.key must be private (0600)");
            }

            // Second call reloads the SAME identity (stable across runs).
            let key2 = load_or_generate_secret_key().expect("reload");
            assert_eq!(
                node_id_string(&key2),
                id1,
                "identity must be stable across loads"
            );
        }

        #[test]
        #[serial_test::serial]
        fn persisted_key_round_trips_through_disk() {
            let _home = isolate_home();
            let key = SecretKey::generate();
            persist_secret_key(&key).expect("persist");
            let loaded = load_or_generate_secret_key().expect("load");
            assert_eq!(node_id_string(&loaded), node_id_string(&key));
        }
    }
}
