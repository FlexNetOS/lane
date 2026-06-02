//! The HTTPS reverse proxy server.
//!
//! Faithful port of `internal/proxy/server.go`. Go drove `net/http` with a
//! `tls.Config.GetCertificate` callback + HTTP/2; here we drive `hyper`/
//! `hyper-util`/`tokio-rustls` directly with a `rustls::ResolvesServerCert`.
//!
//! The server holds:
//! - async routing state (`cfg`, `routers`, `known`, `default_domain`) behind a
//!   `tokio::sync::RwLock`, used by the request handler;
//! - a synchronous snapshot of `{known, default_domain}` behind a
//!   `std::sync::RwLock`, used by the (synchronous) TLS cert resolver;
//! - a certificate cache (`std::sync::RwLock`) with singleflight-guarded
//!   on-demand leaf generation, mirroring Go's `singleflight.Group`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::{watch, RwLock};
use tokio_rustls::TlsAcceptor;

use crate::config::{self, Config, PROXY_HTTPS_PORT, PROXY_HTTP_PORT};
use crate::log;

use super::handler::{self, DomainRouter, PathRoute, ResponseBody};

/// The pooled upstream HTTP client used to reverse-proxy requests.
pub type ProxyClient = Client<HttpConnector, BoxBody<Bytes, std::io::Error>>;

/// Mutable routing state, read by the async request handler under a tokio RwLock.
#[derive(Default)]
pub(crate) struct State {
    pub cfg: Config,
    pub routers: HashMap<String, DomainRouter>,
    pub known: HashSet<String>,
    pub default_domain: String,
}

/// Synchronous snapshot used by the (sync) TLS certificate resolver.
#[derive(Default)]
struct CertState {
    known: HashSet<String>,
    default_domain: String,
}

/// The reverse proxy server.
pub struct Server {
    /// Async routing state.
    pub(crate) state: RwLock<State>,
    /// Sync snapshot for the cert resolver.
    cert_state: StdRwLock<CertState>,
    /// Cached leaf certs keyed by domain.
    cert_cache: StdRwLock<HashMap<String, Arc<CertifiedKey>>>,
    /// Per-host generation locks (singleflight-equivalent).
    cert_locks: StdMutex<HashMap<String, Arc<StdMutex<()>>>>,
    /// Pooled upstream client.
    pub(crate) client: ProxyClient,
    /// Bound HTTP (plaintext) address; overridable in tests.
    http_addr: StdRwLock<String>,
    /// Bound HTTPS address; overridable in tests.
    https_addr: StdRwLock<String>,
    /// Graceful-shutdown signal: send `true` to stop the accept loops.
    shutdown_tx: watch::Sender<bool>,
}

/// Build the pooled upstream transport, mirroring Go's `newUpstreamTransport`:
/// large idle pools and a 2h idle timeout.
fn new_upstream_client() -> ProxyClient {
    Client::builder(TokioExecutor::new())
        // Go: MaxIdleConnsPerHost = 128 (closest pool knob hyper-util exposes).
        .pool_max_idle_per_host(128)
        // Go: IdleConnTimeout = 2h.
        .pool_idle_timeout(Duration::from_secs(2 * 60 * 60))
        .build_http()
}

impl Server {
    /// Construct a server for `cfg`. Mirrors Go's `NewServer`.
    pub fn new(cfg: Config) -> Self {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        Server {
            state: RwLock::new(State {
                cfg,
                ..Default::default()
            }),
            cert_state: StdRwLock::new(CertState::default()),
            cert_cache: StdRwLock::new(HashMap::new()),
            cert_locks: StdMutex::new(HashMap::new()),
            client: new_upstream_client(),
            http_addr: StdRwLock::new(format!("0.0.0.0:{PROXY_HTTP_PORT}")),
            https_addr: StdRwLock::new(format!("0.0.0.0:{PROXY_HTTPS_PORT}")),
            shutdown_tx,
        }
    }

    /// Apply a configuration: ensure+load every domain's leaf cert, build the
    /// routers, and atomically swap in the new state + cert cache. Mirrors Go's
    /// `applyConfig`.
    pub(crate) async fn apply_config(&self, cfg: Config) -> Result<()> {
        let mut routers: HashMap<String, DomainRouter> = HashMap::with_capacity(cfg.domains.len());
        let mut known: HashSet<String> = HashSet::with_capacity(cfg.domains.len());
        let mut cert_cache: HashMap<String, Arc<CertifiedKey>> =
            HashMap::with_capacity(cfg.domains.len());
        let mut default_domain = String::new();

        for (i, d) in cfg.domains.iter().enumerate() {
            if i == 0 {
                default_domain = d.name.clone();
            }

            ensure_leaf(&d.name).with_context(|| format!("ensuring cert for {}", d.name))?;
            let cert =
                load_leaf(&d.name).with_context(|| format!("loading cert for {}", d.name))?;

            let mut path_routes: Vec<PathRoute> = d
                .routes
                .iter()
                .map(|r| PathRoute {
                    prefix: r.path.clone(),
                    port: r.port,
                })
                .collect();
            // Sort by prefix length descending (Go: sort.Slice on prefix len).
            path_routes.sort_by_key(|r| std::cmp::Reverse(r.prefix.len()));

            routers.insert(
                d.name.clone(),
                DomainRouter {
                    default_port: d.port,
                    path_routes,
                },
            );
            known.insert(d.name.clone());
            cert_cache.insert(d.name.clone(), Arc::new(cert));
        }

        {
            let mut st = self.state.write().await;
            st.cfg = cfg;
            st.routers = routers;
            st.known = known.clone();
            st.default_domain = default_domain.clone();
        }
        {
            let mut cs = self.cert_state.write().unwrap();
            cs.known = known;
            cs.default_domain = default_domain;
        }
        {
            let mut cc = self.cert_cache.write().unwrap();
            *cc = cert_cache;
        }

        Ok(())
    }

    /// Reload config from disk and apply it. Mirrors Go's `ReloadConfig`.
    pub async fn reload_config(&self) -> Result<Config> {
        let cfg = config::load()?;
        self.apply_config(cfg.clone()).await?;
        Ok(cfg)
    }

    /// Resolve a leaf cert for an SNI host. Synchronous (rustls calls it from a
    /// sync context). Mirrors Go's `getCertificate` including its singleflight
    /// behavior on a cache miss.
    fn get_certificate(&self, server_name: Option<&str>) -> Result<Arc<CertifiedKey>> {
        let name = match server_name {
            None | Some("") => {
                let def = self.cert_state.read().unwrap().default_domain.clone();
                if def.is_empty() {
                    return Err(anyhow!("no domains configured"));
                }
                def
            }
            Some(sni) => handler::normalize_host(sni),
        };

        if !self.cert_state.read().unwrap().known.contains(&name) {
            return Err(anyhow!("domain {name} is not configured"));
        }

        if let Some(cert) = self.cached_certificate(&name) {
            return Ok(cert);
        }

        // Singleflight: one generation per host. Acquire (or create) the per-host
        // lock, then double-check the cache under it.
        let host_lock = {
            let mut locks = self.cert_locks.lock().unwrap();
            locks
                .entry(name.clone())
                .or_insert_with(|| Arc::new(StdMutex::new(())))
                .clone()
        };
        let _guard = host_lock.lock().unwrap();

        if let Some(cert) = self.cached_certificate(&name) {
            return Ok(cert);
        }

        ensure_leaf(&name).with_context(|| format!("ensuring cert for {name}"))?;
        let cert = Arc::new(load_leaf(&name)?);

        {
            let mut cc = self.cert_cache.write().unwrap();
            cc.insert(name.clone(), cert.clone());
        }

        Ok(cert)
    }

    fn cached_certificate(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        self.cert_cache.read().unwrap().get(name).cloned()
    }

    /// Build the rustls server config: no client auth, our SNI resolver, ALPN
    /// `["h2","http/1.1"]`.
    fn tls_config(self: &Arc<Self>) -> Arc<ServerConfig> {
        crate::install_crypto_provider();
        let resolver = Arc::new(CertResolver {
            server: Arc::clone(self),
        });
        let mut cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Arc::new(cfg)
    }

    /// Start both listeners and serve until shutdown. Mirrors Go's `Start`.
    ///
    /// Binds the HTTP (redirect) and HTTPS ports first (returning the Go-style
    /// "listening on {addr}" error on conflict, closing the HTTP listener if the
    /// HTTPS bind fails), then serves each connection on its own task.
    pub async fn start(self: Arc<Self>) -> Result<()> {
        let cfg = { self.state.read().await.cfg.clone() };
        self.apply_config(cfg).await?;

        let http_addr = self.http_addr.read().unwrap().clone();
        let https_addr = self.https_addr.read().unwrap().clone();

        let http_ln = TcpListener::bind(&http_addr)
            .await
            .with_context(|| format!("listening on {http_addr}"))?;

        let tls_ln = match TcpListener::bind(&https_addr).await {
            Ok(ln) => ln,
            Err(e) => {
                // Go closes the HTTP listener on HTTPS bind failure.
                drop(http_ln);
                return Err(anyhow::Error::new(e).context(format!("listening on {https_addr}")));
            }
        };

        let tls_config = self.tls_config();
        let acceptor = TlsAcceptor::from(tls_config);

        log::info(&format!(
            "HTTP  listening on {http_addr} (redirects to HTTPS)"
        ));
        log::info(&format!("HTTPS listening on {https_addr}"));

        let domains = { self.state.read().await.cfg.domains.clone() };
        for d in &domains {
            log::info(&format!("  {} → localhost:{}", d.name, d.port));
            for r in &d.routes {
                log::info(&format!("    {} → localhost:{}", r.path, r.port));
            }
        }

        let http_task = tokio::spawn(serve_http_redirect(http_ln, self.shutdown_tx.subscribe()));
        let https_task = tokio::spawn(serve_https(
            Arc::clone(&self),
            tls_ln,
            acceptor,
            self.shutdown_tx.subscribe(),
        ));

        // Wait for both accept loops to finish (they finish on shutdown).
        let (r1, r2) = tokio::join!(http_task, https_task);
        // Join errors only on panic; surface the first listener error if any.
        match (flatten(r1), flatten(r2)) {
            (Err(e), _) => Err(e),
            (_, Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    /// Signal graceful shutdown. Mirrors Go's `Shutdown`.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    // --- test seams -------------------------------------------------------

    #[cfg(test)]
    fn set_addrs(&self, http_addr: &str, https_addr: &str) {
        *self.http_addr.write().unwrap() = http_addr.to_string();
        *self.https_addr.write().unwrap() = https_addr.to_string();
    }

    #[cfg(test)]
    fn is_known_domain(&self, name: &str) -> bool {
        self.cert_state.read().unwrap().known.contains(name)
    }
}

fn flatten(r: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    match r {
        Ok(inner) => inner,
        Err(join) => Err(anyhow!("serve task panicked: {join}")),
    }
}

/// Wait until the shutdown signal flips to `true`.
async fn wait_for_shutdown(rx: &mut watch::Receiver<bool>) {
    // If already signalled, return immediately.
    if *rx.borrow() {
        return;
    }
    // Otherwise wait for a change to `true` (sender dropped also stops us).
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

/// The rustls SNI certificate resolver. Mirrors Go's `tls.Config.GetCertificate`.
#[derive(Debug)]
struct CertResolver {
    server: Arc<Server>,
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        // Go returned an error (failing the handshake) for unknown/empty SNI;
        // rustls signals the same by returning `None`.
        self.server.get_certificate(client_hello.server_name()).ok()
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").finish_non_exhaustive()
    }
}

/// The HTTP (plaintext) accept loop: every request 301-redirects to HTTPS.
async fn serve_http_redirect(
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service_fn(redirect_service))
                        .await;
                });
            }
        }
    }
}

/// Redirect handler: `301` to `https://{host}{request-uri}`. Mirrors Go's HTTP
/// server handler.
async fn redirect_service(
    req: Request<Incoming>,
) -> std::result::Result<Response<ResponseBody>, std::convert::Infallible> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| req.uri().authority().map(|a| a.as_str().to_string()))
        .unwrap_or_default();
    let request_uri = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let target = format!("https://{host}{request_uri}");
    let mut resp = Response::new(empty_body());
    *resp.status_mut() = StatusCode::MOVED_PERMANENTLY;
    if let Ok(hv) = HeaderValue::from_str(&target) {
        resp.headers_mut().insert(header::LOCATION, hv);
    }
    Ok(resp)
}

fn empty_body() -> ResponseBody {
    use http_body_util::{BodyExt, Empty};
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

/// The HTTPS accept loop: TLS-accept each connection, then serve it with
/// HTTP/1.1+HTTP/2 auto-negotiation and upgrade support.
async fn serve_https(
    server: Arc<Server>,
    listener: TcpListener,
    acceptor: TlsAcceptor,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let acceptor = acceptor.clone();
                let server = Arc::clone(&server);
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = TokioIo::new(tls_stream);
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let server = Arc::clone(&server);
                        async move { handler::handle(server, req).await }
                    });
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, svc)
                        .await;
                });
            }
        }
    }
}

// --- certificate seam ------------------------------------------------------
//
// Go used package-level function pointers (`ensureLeafCertFn`, `loadLeafTLSFn`)
// so tests could swap the real `cert` implementation for a stub. We reproduce
// that seam: in non-test builds these call straight through to `crate::cert`;
// in test builds they consult overridable hooks.

#[cfg(not(test))]
fn ensure_leaf(name: &str) -> Result<()> {
    crate::cert::ensure_leaf_cert(name)
}

#[cfg(not(test))]
fn load_leaf(name: &str) -> Result<CertifiedKey> {
    crate::cert::load_leaf_tls(name)
}

#[cfg(test)]
fn ensure_leaf(name: &str) -> Result<()> {
    tests::call_ensure(name)
}

#[cfg(test)]
fn load_leaf(name: &str) -> Result<CertifiedKey> {
    tests::call_load(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    type EnsureFn = Box<dyn Fn(&str) -> Result<()> + Send + Sync>;
    type LoadFn = Box<dyn Fn(&str) -> Result<CertifiedKey> + Send + Sync>;

    struct Hooks {
        ensure: StdMutex<Option<EnsureFn>>,
        load: StdMutex<Option<LoadFn>>,
    }

    fn hooks() -> &'static Hooks {
        static HOOKS: OnceLock<Hooks> = OnceLock::new();
        HOOKS.get_or_init(|| Hooks {
            ensure: StdMutex::new(None),
            load: StdMutex::new(None),
        })
    }

    pub(super) fn call_ensure(name: &str) -> Result<()> {
        let guard = hooks().ensure.lock().unwrap();
        match guard.as_ref() {
            Some(f) => f(name),
            None => Ok(()),
        }
    }

    pub(super) fn call_load(name: &str) -> Result<CertifiedKey> {
        let guard = hooks().load.lock().unwrap();
        match guard.as_ref() {
            Some(f) => f(name),
            None => Ok(dummy_cert()),
        }
    }

    fn set_ensure(f: EnsureFn) {
        *hooks().ensure.lock().unwrap() = Some(f);
    }
    fn set_load(f: LoadFn) {
        *hooks().load.lock().unwrap() = Some(f);
    }
    fn reset_hooks() {
        *hooks().ensure.lock().unwrap() = None;
        *hooks().load.lock().unwrap() = None;
    }

    /// Build a real (self-signed) `CertifiedKey` for tests that need the cert
    /// resolver to return something. Generated once and cloned.
    fn dummy_cert() -> CertifiedKey {
        use rcgen::{generate_simple_self_signed, CertifiedKey as RcgenKey};
        let RcgenKey { cert, key_pair } =
            generate_simple_self_signed(vec!["dummy.test".to_string()]).unwrap();
        let cert_der = cert.der().clone();
        let key_der = key_pair.serialize_der();
        let pk: rustls_pki_types::PrivateKeyDer<'static> =
            rustls_pki_types::PrivatePkcs8KeyDer::from(key_der).into();
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&pk).unwrap();
        CertifiedKey::new(vec![cert_der], signing_key)
    }

    fn known_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A server pre-seeded with known domains + cert cache, like the Go tests
    /// that build a `Server{...}` literal.
    fn seeded_server(known: &[&str], default_domain: &str, cached: &[&str]) -> Arc<Server> {
        let s = Server::new(Config::default());
        {
            let mut cs = s.cert_state.write().unwrap();
            cs.known = known_set(known);
            cs.default_domain = default_domain.to_string();
        }
        {
            let mut cc = s.cert_cache.write().unwrap();
            for name in cached {
                cc.insert(name.to_string(), Arc::new(dummy_cert()));
            }
        }
        Arc::new(s)
    }

    #[test]
    #[serial_test::serial]
    fn test_get_certificate_rejects_unknown_sni() {
        reset_hooks();
        let s = seeded_server(&["myapp.test"], "", &[]);
        let err = s.get_certificate(Some("other.test"));
        assert!(err.is_err(), "expected error for unknown SNI");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("not configured"),
            "expected not-configured error, got {msg}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_get_certificate_uses_default_domain_when_sni_empty() {
        reset_hooks();
        let s = seeded_server(&["myapp.test"], "myapp.test", &["myapp.test"]);
        let got = s.get_certificate(None);
        assert!(got.is_ok(), "getCertificate: {:?}", got.err());
    }

    #[test]
    #[serial_test::serial]
    fn test_get_certificate_no_domains_errors() {
        reset_hooks();
        let s = seeded_server(&[], "", &[]);
        let err = s.get_certificate(None);
        assert!(err.is_err());
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("no domains configured"));
    }

    #[test]
    #[serial_test::serial]
    fn test_get_certificate_uses_singleflight_on_cache_miss() {
        reset_hooks();
        let ensure_calls = Arc::new(AtomicUsize::new(0));
        let load_calls = Arc::new(AtomicUsize::new(0));

        {
            let ec = Arc::clone(&ensure_calls);
            set_ensure(Box::new(move |name: &str| {
                if name != "myapp.test" {
                    return Err(anyhow!("unexpected name"));
                }
                ec.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                Ok(())
            }));
        }
        {
            let lc = Arc::clone(&load_calls);
            set_load(Box::new(move |name: &str| {
                if name != "myapp.test" {
                    return Err(anyhow!("unexpected name"));
                }
                lc.fetch_add(1, Ordering::SeqCst);
                Ok(dummy_cert())
            }));
        }

        let s = seeded_server(&["myapp.test"], "", &[]);

        let mut handles = Vec::new();
        for _ in 0..20 {
            let s = Arc::clone(&s);
            handles.push(std::thread::spawn(move || {
                s.get_certificate(Some("myapp.test")).map(|_| ())
            }));
        }
        for h in handles {
            h.join().unwrap().expect("getCertificate concurrent error");
        }

        assert_eq!(
            ensure_calls.load(Ordering::SeqCst),
            1,
            "expected ensure once"
        );
        assert_eq!(load_calls.load(Ordering::SeqCst), 1, "expected load once");
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_apply_config_builds_routes_and_defaults() {
        reset_hooks();
        set_ensure(Box::new(|_| Ok(())));
        set_load(Box::new(|_| Ok(dummy_cert())));

        let s = Server::new(Config::default());
        let cfg = Config {
            domains: vec![
                crate::config::Domain {
                    name: "myapp.test".into(),
                    port: 3000,
                    routes: vec![],
                },
                crate::config::Domain {
                    name: "api.test".into(),
                    port: 8080,
                    routes: vec![],
                },
            ],
            ..Default::default()
        };

        s.apply_config(cfg).await.expect("applyConfig");

        let st = s.state.read().await;
        assert_eq!(st.default_domain, "myapp.test");
        assert_eq!(st.routers.len(), 2);
        assert_eq!(st.known.len(), 2);
        drop(st);
        assert_eq!(s.cert_cache.read().unwrap().len(), 2);
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_apply_config_propagates_ensure_error() {
        reset_hooks();
        set_ensure(Box::new(|name: &str| Err(anyhow!("ensure failed: {name}"))));
        set_load(Box::new(|_| Ok(dummy_cert())));

        let s = Server::new(Config::default());
        let err = s
            .apply_config(Config {
                domains: vec![crate::config::Domain {
                    name: "myapp.test".into(),
                    port: 3000,
                    routes: vec![],
                }],
                ..Default::default()
            })
            .await;
        assert!(err.is_err(), "expected applyConfig to fail");
        // anyhow's `Display` shows only the outermost context (`ensuring cert
        // for ...`); use the alternate `{:#}` to walk the full chain, which is
        // what Go's `err.Error()` exposed.
        let e = err.unwrap_err();
        assert!(format!("{e:#}").contains("ensure failed"), "got: {e:#}");
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_apply_config_propagates_load_error() {
        reset_hooks();
        set_ensure(Box::new(|_| Ok(())));
        set_load(Box::new(|_| Err(anyhow!("load failed"))));

        let s = Server::new(Config::default());
        let err = s
            .apply_config(Config {
                domains: vec![crate::config::Domain {
                    name: "myapp.test".into(),
                    port: 3000,
                    routes: vec![],
                }],
                ..Default::default()
            })
            .await;
        assert!(err.is_err(), "expected applyConfig to fail");
        assert!(err.unwrap_err().to_string().contains("loading cert"));
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_reload_config_loads_from_disk() {
        reset_hooks();
        set_ensure(Box::new(|_| Ok(())));
        set_load(Box::new(|_| Ok(dummy_cert())));

        let home = tempfile::TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());

        let file_cfg = Config {
            domains: vec![crate::config::Domain {
                name: "myapp.test".into(),
                port: 3000,
                routes: vec![],
            }],
            ..Default::default()
        };
        file_cfg.save().expect("Save config");

        let s = Server::new(Config::default());
        let loaded = s.reload_config().await.expect("ReloadConfig");
        assert_eq!(loaded.domains.len(), 1);
        assert_eq!(loaded.domains[0].name, "myapp.test");
        assert!(s.is_known_domain("myapp.test"));
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_start_fails_when_http_port_unavailable() {
        reset_hooks();
        set_ensure(Box::new(|_| Ok(())));
        set_load(Box::new(|_| Ok(dummy_cert())));

        let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy_addr = busy.local_addr().unwrap().to_string();

        let s = Arc::new(Server::new(Config::default()));
        s.set_addrs(&busy_addr, "127.0.0.1:0");

        let err = Arc::clone(&s).start().await;
        assert!(err.is_err(), "expected Start to fail when HTTP port busy");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains(&format!("listening on {busy_addr}")),
            "unexpected error: {msg}"
        );
        drop(busy);
        reset_hooks();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_start_fails_when_https_port_unavailable_and_closes_http() {
        reset_hooks();
        set_ensure(Box::new(|_| Ok(())));
        set_load(Box::new(|_| Ok(dummy_cert())));

        // Choose a free HTTP port (bind+release to learn the number).
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let busy_tls = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy_tls_addr = busy_tls.local_addr().unwrap().to_string();

        let s = Arc::new(Server::new(Config::default()));
        s.set_addrs(&http_addr, &busy_tls_addr);

        let err = Arc::clone(&s).start().await;
        assert!(err.is_err(), "expected Start to fail when HTTPS port busy");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains(&format!("listening on {busy_tls_addr}")),
            "unexpected error: {msg}"
        );

        // The HTTP listener should have been released.
        let recheck = TcpListener::bind(&http_addr).await;
        assert!(
            recheck.is_ok(),
            "expected HTTP port to be released, got {:?}",
            recheck.err()
        );
        drop(busy_tls);
        reset_hooks();
    }
}
