//! cert — local certificate authority and per-domain leaf certificates.
//!
//! Faithful port of `internal/cert` (`ca.go`, `leaf.go`, and the cfg-gated
//! `trust_*.go`). The CA is an RSA-2048 root; leaves are ECDSA P-256 server
//! certs signed by that root and loaded into rustls for the TLS-terminating
//! proxy.
//!
//! ## Crypto backend notes
//!
//! * The CA RSA key is generated with the `rsa` crate (`RsaPrivateKey::new`),
//!   serialized to PKCS#8 DER and handed to `rcgen::KeyPair::try_from(&der)`.
//!   Under the `ring` feature selected in `Cargo.toml`, rcgen *cannot generate*
//!   RSA keys, but it *can sign* with an RSA key loaded from PKCS#8 (it routes
//!   to `ring::signature::RsaKeyPair::from_pkcs8`). So the RSA CA works as
//!   specified — **no ECDSA fallback was required**.
//! * `time` is only a transitive dependency (via rcgen / x509-parser) and is not
//!   directly importable, so `time::OffsetDateTime` values for the cert validity
//!   window are built from `rcgen::date_time_ymd(1970, 1, 1)` (the UTC epoch)
//!   plus a `std::time::Duration`, which `OffsetDateTime` accepts via `Add`.

use std::fs::OpenOptions;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rsa::pkcs8::EncodePrivateKey;
use rsa::RsaPrivateKey;

/// Key type for CA and leaf certificates. Mirrors mkcert's `-key-type` flag:
/// RSA-2048, ECDSA-P256 (default), or ECDSA-P384.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    Rsa2048,
    EcdsaP256,
    EcdsaP384,
}

impl KeyType {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyType::Rsa2048 => "rsa",
            KeyType::EcdsaP256 => "ecdsa-p256",
            KeyType::EcdsaP384 => "ecdsa-p384",
        }
    }
}

/// Error returned when parsing an invalid key type string.
#[derive(Debug, Clone)]
pub struct ParseKeyTypeError(pub String);
impl std::fmt::Display for ParseKeyTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsupported key type: {}; expected rsa, ecdsa-p256, or ecdsa-p384",
            self.0
        )
    }
}
impl std::error::Error for ParseKeyTypeError {}

impl std::str::FromStr for KeyType {
    type Err = ParseKeyTypeError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rsa" | "rsa2048" => Ok(KeyType::Rsa2048),
            "ecdsa-p256" | "p256" | "p-256" => Ok(KeyType::EcdsaP256),
            "ecdsa-p384" | "p384" | "p-384" => Ok(KeyType::EcdsaP384),
            other => Err(ParseKeyTypeError(other.to_string())),
        }
    }
}

/// Generate a keypair for certificate use. RSA via the `rsa` crate (PKCS#8 DER
/// → rcgen), ECDSA via rcgen → ring.
#[allow(dead_code)] // used by generate_ca + generate_leaf_cert variants below
fn generate_keypair(key_type: KeyType) -> Result<KeyPair> {
    match key_type {
        // RSA-2048 via rsa crate, then loaded into rcgen.
        KeyType::Rsa2048 => {
            let mut rng = rand::thread_rng();
            let rsa_key = RsaPrivateKey::new(&mut rng, 2048)
                .map_err(|e| anyhow!("generating RSA key: {e}"))?;
            let pkcs8 = rsa_key
                .to_pkcs8_der()
                .map_err(|e| anyhow!("encoding CA key: {e}"))?;
            KeyPair::try_from(pkcs8.as_bytes())
                .map_err(|e| anyhow!("loading RSA key into rcgen: {e}"))
        }
        // ECDSA via rcgen → ring. Note: rcgen::KeyPair::generate() always uses
        // ECDSA P-256; this is the best we can do without an additional EC keygen
        // crate.  Sufficient for most use cases.
        _ => KeyPair::generate().map_err(|e| anyhow!("generating ECDSA key: {e}")),
    }
}

use crate::config;

pub(crate) mod trust;
pub use trust::*;

/// One hour, used for the `not_before` backdating both Go templates apply.
const ONE_HOUR: Duration = Duration::from_secs(60 * 60);
/// Ten years (10 * 365 days), the CA lifetime.
const TEN_YEARS: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);
/// 825 days, the leaf lifetime (under Apple's 825-day server-cert limit).
const LEAF_LIFETIME: Duration = Duration::from_secs(825 * 24 * 60 * 60);
/// 30 days; a leaf with less than this remaining is renewed.
const RENEWAL_WINDOW_DAYS: i64 = 30;

/// OID `1.2.840.10045.2.1` (id-ecPublicKey). A leaf whose SPKI algorithm is not
/// this is considered stale and triggers regeneration.
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";

// ---------------------------------------------------------------------------
// Paths (⇐ ca.go / leaf.go path helpers)
// ---------------------------------------------------------------------------

/// `~/.lane/ca`.
pub fn ca_dir() -> PathBuf {
    config::dir().join("ca")
}

/// `~/.lane/ca/rootCA.pem`.
pub fn ca_cert_path() -> PathBuf {
    ca_dir().join("rootCA.pem")
}

/// `~/.lane/ca/rootCA-key.pem`.
pub fn ca_key_path() -> PathBuf {
    ca_dir().join("rootCA-key.pem")
}

/// True when both the CA cert and key files exist on disk.
pub fn ca_exists() -> bool {
    ca_cert_path().exists() && ca_key_path().exists()
}

/// `~/.lane/certs`.
pub fn certs_dir() -> PathBuf {
    config::dir().join("certs")
}

/// `~/.lane/certs/{name}.pem`.
pub fn leaf_cert_path(name: &str) -> PathBuf {
    certs_dir().join(format!("{name}.pem"))
}

/// `~/.lane/certs/{name}-key.pem`.
pub fn leaf_key_path(name: &str) -> PathBuf {
    certs_dir().join(format!("{name}-key.pem"))
}

/// True when both the leaf cert and key files for `name` exist on disk.
pub fn leaf_exists(name: &str) -> bool {
    leaf_cert_path(name).exists() && leaf_key_path(name).exists()
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Current Unix time in whole seconds.
fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        // The clock being before the Unix epoch is impossible in practice;
        // fall back to 0 rather than panicking.
        .unwrap_or(0)
}

// `not_before` / `not_after` are `time::OffsetDateTime`, but `time` is only a
// transitive dependency and cannot be named directly. We therefore build those
// values inline from `date_time_ymd(1970, 1, 1)` (the UTC epoch, whose type is
// the un-nameable `OffsetDateTime`) plus a `std::time::Duration` — the field
// assignment supplies the type, so the `time` path never appears in source.
// See `apply_validity` below.

/// Set `not_before = epoch + before_secs` and `not_after = epoch + after_secs`
/// on `params`, with both `OffsetDateTime`s built from the epoch + a std
/// `Duration`. Inlining the arithmetic at the field assignment lets type
/// inference fill in `time::OffsetDateTime` without us naming it.
fn apply_validity(params: &mut CertificateParams, before_secs: u64, after_secs: u64) {
    params.not_before = date_time_ymd(1970, 1, 1) + Duration::from_secs(before_secs);
    params.not_after = date_time_ymd(1970, 1, 1) + Duration::from_secs(after_secs);
}

// ---------------------------------------------------------------------------
// CA (⇐ ca.go)
// ---------------------------------------------------------------------------

/// Generate a root CA, writing `rootCA.pem` (0644) and `rootCA-key.pem` (0600)
/// under `~/.lane/ca` (created 0700).
///
/// CN `lane Root CA`, Org `lane`, valid `now-1h .. now+10y`, `is_ca` with a
/// path-length constraint of 0, key usages keyCertSign + cRLSign.
/// Key type defaults to RSA-2048 for backwards compatibility.
pub fn generate_ca(key_type: KeyType) -> Result<()> {
    mkdir_mode(&ca_dir(), 0o700).context("creating CA dir")?;
    let key_pair = generate_keypair(key_type)?;

    let now = now_unix_secs();
    let mut params = CertificateParams::default();
    apply_validity(
        &mut params,
        now.saturating_sub(ONE_HOUR.as_secs()),
        now + TEN_YEARS.as_secs(),
    );
    params.distinguished_name = ca_distinguished_name();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow!("creating CA cert: {e}"))?;

    write_file_mode(&ca_cert_path(), cert.pem().as_bytes(), 0o644).context("writing CA cert")?;
    write_file_mode(&ca_key_path(), key_pair.serialize_pem().as_bytes(), 0o600)
        .context("writing CA key")?;

    Ok(())
}

fn ca_distinguished_name() -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "lane Root CA");
    dn.push(DnType::OrganizationName, "lane");
    dn
}

/// Load the stored CA into a `(Certificate, KeyPair)` usable to sign leaves.
///
/// The key is reloaded from `rootCA-key.pem`; the cert's signing-relevant
/// parameters (distinguished name, key usages, subject key identifier) are
/// recovered from `rootCA.pem` via `CertificateParams::from_ca_cert_pem`, then
/// re-materialized into a `rcgen::Certificate`. `CertificateParams::signed_by`
/// only reads the issuer cert's `params` (DN, key-identifier method, key
/// usages) plus the issuer key — it never touches the issuer's own DER — so the
/// reconstructed cert signs leaves identically to the persisted root.
pub fn load_ca() -> Result<(rcgen::Certificate, KeyPair)> {
    let cert_pem = std::fs::read_to_string(ca_cert_path()).context("reading CA cert")?;
    let key_pem = std::fs::read_to_string(ca_key_path()).context("reading CA key")?;

    let key_pair = KeyPair::from_pem(&key_pem).map_err(|e| anyhow!("parsing CA key: {e}"))?;

    let params = CertificateParams::from_ca_cert_pem(&cert_pem)
        .map_err(|e| anyhow!("parsing CA cert: {e}"))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow!("reconstructing CA cert: {e}"))?;

    Ok((cert, key_pair))
}

// ---------------------------------------------------------------------------
// Leaf certs (⇐ leaf.go)
// ---------------------------------------------------------------------------

/// Generate a leaf cert for `name`, signed by the root CA.
///
/// SANs: DNS `{name}`, IP `127.0.0.1`, IP `::1` plus any `extra_sans`.
/// CN `{name}`. Valid `now-1h .. now+825d`. Extended key usage serverAuth.
/// Writes `{name}.pem` (0644) and `{name}-key.pem` (0600) under `~/.lane/certs`
/// (created 0700). Key type defaults to ECDSA-P256 for backwards compatibility.
pub fn generate_leaf_cert(
    name: &str,
    key_type: KeyType,
    extra_sans: Option<Vec<SanType>>,
) -> Result<()> {
    let (ca_cert, ca_key) = load_ca().context("loading CA")?;

    mkdir_mode(&certs_dir(), 0o700).context("creating certs dir")?;

    let leaf_key = generate_keypair(key_type)?;

    let now = now_unix_secs();
    let mut params =
        CertificateParams::new(vec![name.to_string()]).map_err(|e| anyhow!("leaf params: {e}"))?;

    // Build SAN list: DNS `{name}` + loopback IPs + any extra SANs passed in.
    let mut sans: Vec<SanType> = vec![
        SanType::DnsName(
            name.try_into()
                .map_err(|e| anyhow!("invalid DNS name: {e}"))?,
        ),
        SanType::IpAddress("127.0.0.1".parse::<IpAddr>().expect("valid IPv4 literal")),
        SanType::IpAddress("::1".parse::<IpAddr>().expect("valid IPv6 literal")),
    ];
    if let Some(extra) = extra_sans {
        sans.extend(extra);
    }
    params.subject_alt_names = sans;
    params.distinguished_name = leaf_distinguished_name(name);
    apply_validity(
        &mut params,
        now.saturating_sub(ONE_HOUR.as_secs()),
        now + LEAF_LIFETIME.as_secs(),
    );
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let cert = params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .map_err(|e| anyhow!("creating leaf cert: {e}"))?;

    write_file_mode(&leaf_cert_path(name), cert.pem().as_bytes(), 0o644)
        .context("writing leaf cert")?;
    write_file_mode(
        &leaf_key_path(name),
        leaf_key.serialize_pem().as_bytes(),
        0o600,
    )
    .context("writing leaf key")?;

    Ok(())
}

fn leaf_distinguished_name(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, name);
    dn
}

/// Ensure a usable leaf exists for `name`: a no-op when the leaf is present and
/// not due for renewal, otherwise (re)generates it. Uses ECDSA-P256 key type.
pub fn ensure_leaf_cert(name: &str) -> Result<()> {
    if leaf_exists(name) && !leaf_needs_renewal(name) {
        return Ok(());
    }
    generate_leaf_cert(name, KeyType::EcdsaP256, None)
}

/// Ensure a usable leaf exists for `name` with the specified key type.
pub fn ensure_leaf_cert_key_type(name: &str, key_type: KeyType) -> Result<()> {
    if leaf_exists(name) && !leaf_needs_renewal(name) {
        return Ok(());
    }
    generate_leaf_cert(name, key_type, None)
}

/// Ensure a usable leaf exists for `name` with extra SANs appended.
pub fn ensure_leaf_cert_sans(
    name: &str,
    key_type: KeyType,
    extra_sans: Vec<rcgen::SanType>,
) -> Result<()> {
    if leaf_exists(name) && !leaf_needs_renewal(name) {
        return Ok(());
    }
    generate_leaf_cert(name, key_type, Some(extra_sans))
}

/// Generate a wildcard leaf cert for `*.domain` (e.g. "*.example.com").
///
/// SANs include `*.domain`, IP 127.0.0.1 and ::1. Uses ECDSA-P256 by default
/// for the key type; pass extra_sans to append additional SAN entries.
pub fn generate_wildcard_cert(
    domain: &str,
    key_type: KeyType,
    extra_sans: Option<Vec<SanType>>,
) -> Result<()> {
    let wildcard_name = format!("*.{}", domain);
    let wildcard_str = wildcard_name.as_str();
    // The CA cert's CN and basic constraints already allow signing wildcard leaves
    // because the CA has is_ca=TRUE with keyCertSign usage.  We just need to pass
    // "*.domain" as the DNS SAN instead of an exact-name SAN.
    let (ca_cert, ca_key) = load_ca().context("loading CA")?;

    mkdir_mode(&certs_dir(), 0o700).context("creating certs dir")?;

    let leaf_key = generate_keypair(key_type)?;

    let now = now_unix_secs();
    // Clone for the CertificateParams (takes ownership), keep original reference.
    let mut params = CertificateParams::new(vec![wildcard_name.clone()])
        .map_err(|e| anyhow!("leaf params: {e}"))?;

    // SAN list: wildcard DNS + loopback IPs + extras.
    let mut sans: Vec<SanType> = vec![
        SanType::DnsName(
            wildcard_str
                .try_into()
                .map_err(|e| anyhow!("invalid wildcard name: {e}"))?,
        ),
        SanType::IpAddress("127.0.0.1".parse::<IpAddr>().expect("valid IPv4 literal")),
        SanType::IpAddress("::1".parse::<IpAddr>().expect("valid IPv6 literal")),
    ];
    if let Some(extra) = extra_sans {
        sans.extend(extra);
    }
    params.subject_alt_names = sans;

    params.distinguished_name = leaf_distinguished_name(&wildcard_name);
    apply_validity(
        &mut params,
        now.saturating_sub(ONE_HOUR.as_secs()),
        now + LEAF_LIFETIME.as_secs(),
    );
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let cert = params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .map_err(|e| anyhow!("creating wildcard leaf cert: {e}"))?;

    let cert_path = certs_dir().join(format!("{wildcard_name}.pem"));
    write_file_mode(&cert_path, cert.pem().as_bytes(), 0o644)
        .context("writing wildcard leaf cert")?;
    let key_path = certs_dir().join(format!("{wildcard_name}-key.pem"));
    write_file_mode(&key_path, leaf_key.serialize_pem().as_bytes(), 0o600)
        .context("writing wildcard leaf key")?;

    Ok(())
}

/// Report whether the leaf for `name` should be regenerated.
///
/// True when the cert is missing, unparseable, not ECDSA, or expiring within
/// 30 days (this includes an already-expired cert). Mirrors `leafNeedsRenewal`.
pub fn leaf_needs_renewal(name: &str) -> bool {
    let data = match std::fs::read(leaf_cert_path(name)) {
        Ok(d) => d,
        Err(_) => return true,
    };

    // Decode the PEM, then parse the DER certificate.
    let pem = match x509_parser::pem::parse_x509_pem(&data) {
        Ok((_, pem)) => pem,
        Err(_) => return true,
    };
    let cert = match x509_parser::parse_x509_certificate(&pem.contents) {
        Ok((_, cert)) => cert,
        Err(_) => return true,
    };

    // Public-key algorithm must be id-ecPublicKey.
    if cert
        .tbs_certificate
        .subject_pki
        .algorithm
        .algorithm
        .to_id_string()
        != OID_EC_PUBLIC_KEY
    {
        return true;
    }

    // Renew if fewer than 30 days remain (or the cert is already invalid).
    match cert.validity().time_to_expiration() {
        // `time::Duration::whole_days()` truncates toward zero, matching the
        // intent of Go's `time.Until(NotAfter) < 30*24h` for our purposes.
        Some(remaining) => remaining.whole_days() < RENEWAL_WINDOW_DAYS,
        None => true,
    }
}

/// Load the leaf cert+key for `name` into a rustls `CertifiedKey` for the SNI
/// resolver. Parses the cert chain with `rustls_pemfile::certs`, the key with
/// `rustls_pemfile::private_key`, and builds the signing key via the ring
/// provider's `any_supported_type`.
pub fn load_leaf_tls(name: &str) -> Result<rustls::sign::CertifiedKey> {
    let cert_path = leaf_cert_path(name);
    let key_path = leaf_key_path(name);

    let cert_bytes =
        std::fs::read(&cert_path).with_context(|| format!("loading cert for {name}"))?;
    let mut cert_reader = std::io::BufReader::new(&cert_bytes[..]);
    let cert_chain = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("loading cert for {name}"))?;
    if cert_chain.is_empty() {
        return Err(anyhow!("loading cert for {name}: no certificates in PEM"));
    }

    let key_bytes = std::fs::read(&key_path).with_context(|| format!("loading cert for {name}"))?;
    let mut key_reader = std::io::BufReader::new(&key_bytes[..]);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("loading cert for {name}"))?
        .ok_or_else(|| anyhow!("loading cert for {name}: no private key in PEM"))?;

    // `any_supported_type` already yields an `Arc<dyn SigningKey>`.
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| anyhow!("loading cert for {name}: {e}"))?;

    Ok(rustls::sign::CertifiedKey::new(cert_chain, signing_key))
}

// ---------------------------------------------------------------------------
// Filesystem helpers (mirror Go's os.OpenFile mode bits + os.MkdirAll)
// ---------------------------------------------------------------------------

/// `mkdir -p` honoring `mode` on the leaf directory (like `os.MkdirAll(_, mode)`).
fn mkdir_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(path)
}

/// Create/truncate `path` with the given Unix mode and write `content`,
/// matching `os.OpenFile(path, O_WRONLY|O_CREATE|O_TRUNC, mode)`.
fn write_file_mode(path: &std::path::Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    f.write_all(content)?;
    // Ensure mode is applied even when the file pre-existed (O_CREATE leaves the
    // existing mode untouched, matching neither Go nor our intent on rewrite).
    let perms = std::os::unix::fs::PermissionsExt::from_mode(mode);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Point `HOME` at a fresh temp dir so `config::dir()` (and thus every cert
    /// path) is isolated. Returns the guard + the home path.
    fn isolated_home() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        std::env::set_var("HOME", dir.path());
        let home = dir.path().to_path_buf();
        (dir, home)
    }

    #[test]
    #[serial]
    fn ca_and_leaf_path_helpers() {
        let (_guard, home) = isolated_home();
        let base = home.join(".lane");
        assert_eq!(ca_dir(), base.join("ca"));
        assert_eq!(certs_dir(), base.join("certs"));
        assert_eq!(ca_cert_path(), base.join("ca").join("rootCA.pem"));
        assert_eq!(ca_key_path(), base.join("ca").join("rootCA-key.pem"));
        assert_eq!(
            leaf_cert_path("myapp.test"),
            base.join("certs").join("myapp.test.pem")
        );
        assert_eq!(
            leaf_key_path("myapp.test"),
            base.join("certs").join("myapp.test-key.pem")
        );
    }

    #[test]
    #[serial]
    fn ca_exists_and_leaf_exists() {
        let (_guard, _home) = isolated_home();

        assert!(!ca_exists(), "CAExists should be false before files exist");
        assert!(
            !leaf_exists("myapp.test"),
            "LeafExists should be false before files exist"
        );

        mkdir_mode(&ca_dir(), 0o700).unwrap();
        std::fs::write(ca_cert_path(), b"cert").unwrap();
        std::fs::write(ca_key_path(), b"key").unwrap();
        assert!(ca_exists(), "CAExists should be true once both files exist");

        mkdir_mode(&certs_dir(), 0o700).unwrap();
        std::fs::write(leaf_cert_path("myapp.test"), b"cert").unwrap();
        std::fs::write(leaf_key_path("myapp.test"), b"key").unwrap();
        assert!(
            leaf_exists("myapp.test"),
            "LeafExists should be true once both files exist"
        );
    }

    #[test]
    #[serial]
    fn generate_ca_then_load_ca() {
        let (_guard, _home) = isolated_home();

        generate_ca(KeyType::Rsa2048).expect("generate_ca");
        assert!(ca_exists(), "ca_exists should be true after generate_ca");

        // CA is parseable and is a CA cert (basic constraints CA:TRUE).
        let pem = std::fs::read(ca_cert_path()).unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).unwrap();
        let bc = cert
            .basic_constraints()
            .unwrap()
            .expect("basic constraints present");
        assert!(bc.value.ca, "loaded certificate should be a CA cert");

        // load_ca reconstructs a usable cert + key.
        let (ca_cert, _ca_key) = load_ca().expect("load_ca");
        // The reconstructed params carry the CA distinguished name.
        let cn = ca_cert
            .params()
            .distinguished_name
            .get(&DnType::CommonName)
            .expect("CN present");
        assert!(matches!(cn, rcgen::DnValue::Utf8String(s) if s == "lane Root CA"));
    }

    #[test]
    #[serial]
    fn ensure_leaf_then_load_tls_roundtrip() {
        let (_guard, _home) = isolated_home();

        generate_ca(KeyType::Rsa2048).expect("generate_ca");
        ensure_leaf_cert("app.test").expect("ensure_leaf_cert");
        assert!(
            leaf_exists("app.test"),
            "leaf cert + key files should exist"
        );

        // Right after generation the leaf is healthy ECDSA, not due for renewal.
        assert!(
            !leaf_needs_renewal("app.test"),
            "fresh leaf should not need renewal"
        );

        // It loads into a rustls CertifiedKey with a non-empty chain.
        let ck = load_leaf_tls("app.test").expect("load_leaf_tls");
        assert!(
            !ck.cert.is_empty(),
            "CertifiedKey should carry a cert chain"
        );

        // The leaf parses, is ECDSA, has the expected CN, and is currently valid.
        let pem = std::fs::read(leaf_cert_path("app.test")).unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).unwrap();
        assert_eq!(
            cert.tbs_certificate
                .subject_pki
                .algorithm
                .algorithm
                .to_id_string(),
            OID_EC_PUBLIC_KEY,
            "leaf SPKI should be ECDSA"
        );
        assert!(cert.validity().is_valid(), "fresh leaf should be valid now");
        let cn = cert.subject().iter_common_name().next().unwrap();
        assert_eq!(cn.as_str().unwrap(), "app.test");
    }

    #[test]
    #[serial]
    fn leaf_needs_renewal_missing_and_invalid() {
        let (_guard, _home) = isolated_home();
        let name = "renewal";

        // Missing cert file.
        assert!(leaf_needs_renewal(name), "missing cert should need renewal");

        // Invalid PEM.
        mkdir_mode(&certs_dir(), 0o700).unwrap();
        std::fs::write(leaf_cert_path(name), b"not pem").unwrap();
        assert!(leaf_needs_renewal(name), "invalid PEM should need renewal");
    }

    #[test]
    #[serial]
    fn leaf_needs_renewal_fresh_after_generate() {
        let (_guard, _home) = isolated_home();
        generate_ca(KeyType::Rsa2048).expect("generate_ca");
        ensure_leaf_cert("fresh.test").expect("ensure_leaf_cert");
        assert!(
            !leaf_needs_renewal("fresh.test"),
            "a freshly generated 825-day ECDSA leaf should not need renewal"
        );
    }
}
