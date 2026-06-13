//! ACME (RFC 8555) certificate issuance for `lane start --acme` — obtain a real
//! Let's Encrypt certificate for a public domain via the HTTP-01 challenge.
//!
//! The **live** issuance path (network calls to the ACME CA, via `instant-acme`)
//! is behind the `acme` cargo feature, so the default build stays
//! dependency-light and never compiles the ACME client. The pure parts
//! (parameter validation, directory selection, the HTTP-01 challenge path, and
//! the challenge responder + store) are always compiled and unit-tested.
//!
//! Build with live issuance: `cargo build --features acme`. Without the feature,
//! [`issue`] returns a clear error instead of silently doing nothing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Let's Encrypt production directory URL.
pub const LE_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt staging directory URL (untrusted certs; for testing the flow).
pub const LE_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// The default address the HTTP-01 challenge responder binds.
pub const DEFAULT_CHALLENGE_ADDR: &str = "0.0.0.0:80";

/// Parameters for an ACME issuance.
#[derive(Debug, Clone)]
pub struct AcmeParams {
    /// The public FQDN to issue a certificate for.
    pub domain: String,
    /// Contact email registered with the ACME account.
    pub email: String,
    /// Use the Let's Encrypt staging environment.
    pub staging: bool,
    /// Address the HTTP-01 challenge responder binds (default `0.0.0.0:80`).
    pub challenge_addr: SocketAddr,
}

impl AcmeParams {
    /// Validate the parameters before any network work. ACME CAs only issue for
    /// resolvable **public** FQDNs, so local-only names and bare IPs are rejected
    /// up front with actionable messages.
    pub fn validate(&self) -> Result<()> {
        let d = self.domain.trim();
        if d.is_empty() {
            bail!("--acme requires a domain (the start domain must be a public FQDN)");
        }
        if d == "localhost"
            || d.ends_with(".test")
            || d.ends_with(".localhost")
            || d.ends_with(".local")
            || d.ends_with(".internal")
        {
            bail!(
                "ACME cannot issue for the local-only domain {d:?}; \
                 use a public FQDN that resolves to this host"
            );
        }
        if d.parse::<std::net::IpAddr>().is_ok() {
            bail!("ACME cannot issue for a bare IP address ({d:?})");
        }
        if !d.contains('.') {
            bail!("ACME requires a fully-qualified public domain (got {d:?})");
        }
        if self.email.trim().is_empty() {
            bail!("--acme requires a contact email (pass --acme-email <addr>)");
        }
        Ok(())
    }

    /// The ACME directory URL for this run (staging vs production).
    pub fn directory_url(&self) -> &'static str {
        if self.staging {
            LE_STAGING
        } else {
            LE_PRODUCTION
        }
    }
}

/// The HTTP-01 well-known request path for a challenge token.
pub fn challenge_path(token: &str) -> String {
    format!("/.well-known/acme-challenge/{token}")
}

/// Issued certificate material (PEM): the full chain and the matching key.
#[derive(Debug, Clone)]
pub struct Issued {
    pub cert_pem: String,
    pub key_pem: String,
}

/// Shared `token → key-authorization` map the HTTP-01 responder serves from.
/// The ACME client populates it; the responder reads it.
#[derive(Clone, Default)]
pub struct ChallengeStore(Arc<Mutex<HashMap<String, String>>>);

impl ChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Register the key-authorization for a challenge token.
    pub fn set(&self, token: &str, key_authorization: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(token.to_string(), key_authorization.to_string());
    }
    /// The key-authorization for a token, if registered.
    pub fn get(&self, token: &str) -> Option<String> {
        self.0.lock().unwrap().get(token).cloned()
    }
    /// Forget all registered challenges.
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// A running HTTP-01 challenge responder. Drop or [`Responder::shutdown`] to stop it.
pub struct Responder {
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
    /// The address actually bound (useful when binding an ephemeral `:0` port).
    pub local_addr: SocketAddr,
}

impl Responder {
    /// Signal the responder to stop and wait for it to drain.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.handle.await;
    }
}

/// Start a minimal HTTP-01 responder serving `/.well-known/acme-challenge/<token>`
/// from `store` (200 `text/plain` with the key-authorization; 404 otherwise).
/// Returns once the listener is bound.
pub async fn serve_http01(store: ChallengeStore, addr: SocketAddr) -> Result<Responder> {
    let listener = TcpListener::bind(addr).await.with_context(|| {
        format!("binding HTTP-01 responder on {addr} (needs the port free + privilege for :80)")
    })?;
    let local_addr = listener.local_addr()?;
    let (tx, mut rx) = oneshot::channel();

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    if let Ok((mut sock, _)) = accepted {
                        let store = store.clone();
                        tokio::spawn(async move {
                            let _ = serve_one(&mut sock, &store).await;
                        });
                    }
                }
            }
        }
    });

    Ok(Responder {
        shutdown: Some(tx),
        handle,
        local_addr,
    })
}

/// Handle one HTTP-01 request: read the request line, extract the path, and
/// answer from the store.
async fn serve_one(sock: &mut tokio::net::TcpStream, store: &ChallengeStore) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let response = match path
        .strip_prefix("/.well-known/acme-challenge/")
        .and_then(|token| store.get(token))
    {
        Some(keyauth) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            keyauth.len(),
            keyauth
        ),
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
    };
    sock.write_all(response.as_bytes()).await?;
    sock.flush().await?;
    Ok(())
}

/// Obtain a certificate for `params.domain` via the ACME HTTP-01 challenge.
///
/// Only available with the `acme` cargo feature; the no-feature build returns a
/// clear error (see the module docs).
#[cfg(feature = "acme")]
pub async fn issue(params: &AcmeParams) -> Result<Issued> {
    use instant_acme::{
        Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    };

    params.validate()?;

    let (account, _credentials) = Account::create(
        &NewAccount {
            contact: &[&format!("mailto:{}", params.email.trim())],
            terms_of_service_agreed: true,
            only_return_existing: false,
        },
        params.directory_url(),
        None,
    )
    .await
    .context("creating ACME account")?;

    let identifier = Identifier::Dns(params.domain.trim().to_string());
    let mut order = account
        .new_order(&NewOrder {
            identifiers: &[identifier],
        })
        .await
        .context("placing ACME order")?;

    // Register the HTTP-01 key-authorizations and collect the challenge URLs.
    let store = ChallengeStore::new();
    let authorizations = order
        .authorizations()
        .await
        .context("fetching authorizations")?;
    let mut challenge_urls = Vec::new();
    for authz in &authorizations {
        if matches!(authz.status, AuthorizationStatus::Valid) {
            continue;
        }
        let challenge = authz
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .context("no HTTP-01 challenge offered for authorization")?;
        let key_auth = order.key_authorization(challenge);
        store.set(&challenge.token, key_auth.as_str());
        challenge_urls.push(challenge.url.clone());
    }

    // Serve the challenges, then tell the CA they are ready.
    let responder = serve_http01(store.clone(), params.challenge_addr).await?;
    for url in &challenge_urls {
        order
            .set_challenge_ready(url)
            .await
            .context("signaling challenge ready")?;
    }

    // Poll the order until it is Ready (validated) or fails.
    let mut tries = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let state = order.refresh().await.context("refreshing ACME order")?;
        match state.status {
            OrderStatus::Ready | OrderStatus::Valid => break,
            OrderStatus::Invalid => {
                responder.shutdown().await;
                bail!("ACME order failed: {:?}", state.error);
            }
            _ => {}
        }
        tries += 1;
        if tries > 30 {
            responder.shutdown().await;
            bail!("ACME order not ready after ~60s; check that {} resolves to this host and :80 is reachable", params.domain);
        }
    }
    responder.shutdown().await;

    // Finalize with a fresh keypair + CSR, then download the certificate chain.
    let key_pair = rcgen::KeyPair::generate().context("generating certificate key")?;
    let csr = rcgen::CertificateParams::new(vec![params.domain.trim().to_string()])
        .context("building CSR params")?
        .serialize_request(&key_pair)
        .context("serializing CSR")?;
    order
        .finalize(csr.der())
        .await
        .context("finalizing ACME order")?;

    let mut tries = 0;
    let cert_pem = loop {
        if let Some(pem) = order
            .certificate()
            .await
            .context("downloading certificate")?
        {
            break pem;
        }
        tries += 1;
        if tries > 30 {
            bail!("ACME certificate not issued after finalize timeout");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    };

    Ok(Issued {
        cert_pem,
        key_pem: key_pair.serialize_pem(),
    })
}

/// Feature-off gate: ACME issuance requires building with `--features acme`.
#[cfg(not(feature = "acme"))]
pub async fn issue(_params: &AcmeParams) -> Result<Issued> {
    bail!("ACME issuance is not compiled in; rebuild lane with `--features acme`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    fn params(domain: &str, email: &str) -> AcmeParams {
        AcmeParams {
            domain: domain.into(),
            email: email.into(),
            staging: false,
            challenge_addr: "127.0.0.1:0".parse().unwrap(),
        }
    }

    #[test]
    fn validate_rejects_local_and_bare_ip() {
        assert!(params("myapp.test", "me@x.com").validate().is_err());
        assert!(params("localhost", "me@x.com").validate().is_err());
        assert!(params("dev.local", "me@x.com").validate().is_err());
        assert!(params("127.0.0.1", "me@x.com").validate().is_err());
        assert!(params("nodot", "me@x.com").validate().is_err());
        assert!(params("example.com", "").validate().is_err());
    }

    #[test]
    fn validate_accepts_public_fqdn() {
        assert!(params("app.example.com", "me@x.com").validate().is_ok());
    }

    #[test]
    fn directory_url_selects_env() {
        assert_eq!(
            params("a.example.com", "e@x").directory_url(),
            LE_PRODUCTION
        );
        let mut p = params("a.example.com", "e@x");
        p.staging = true;
        assert_eq!(p.directory_url(), LE_STAGING);
    }

    #[test]
    fn challenge_path_is_well_known() {
        assert_eq!(
            challenge_path("tok123"),
            "/.well-known/acme-challenge/tok123"
        );
    }

    #[test]
    fn store_set_get_clear() {
        let s = ChallengeStore::new();
        assert_eq!(s.get("t"), None);
        s.set("t", "t.thumb");
        assert_eq!(s.get("t").as_deref(), Some("t.thumb"));
        s.clear();
        assert_eq!(s.get("t"), None);
    }

    async fn http_get(addr: SocketAddr, path: &str) -> String {
        let mut sock = TcpStream::connect(addr).await.unwrap();
        sock.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).await.unwrap();
        resp
    }

    #[tokio::test]
    async fn responder_serves_known_token_and_404s_unknown() {
        let store = ChallengeStore::new();
        store.set("tokABC", "tokABC.thumbprint");
        let responder = serve_http01(store, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = responder.local_addr;

        let ok = http_get(addr, "/.well-known/acme-challenge/tokABC").await;
        assert!(ok.contains("200 OK"), "{ok}");
        assert!(ok.contains("tokABC.thumbprint"), "{ok}");

        let missing = http_get(addr, "/.well-known/acme-challenge/nope").await;
        assert!(missing.contains("404"), "{missing}");

        responder.shutdown().await;
    }

    // In the default (no-feature) build, issue() must fail closed, not no-op.
    // (Not run in the feature build, where issue() does real network work.)
    #[cfg(not(feature = "acme"))]
    #[tokio::test]
    async fn issue_without_feature_errors_cleanly() {
        let p = params("app.example.com", "me@x.com");
        let err = issue(&p).await.unwrap_err().to_string();
        assert!(err.contains("--features acme"), "{err}");
    }
}
