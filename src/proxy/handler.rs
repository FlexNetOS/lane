//! Request routing, host normalization, reverse proxying and CORS.
//!
//! Faithful port of `internal/proxy/handler.go`. Go used `net/http` +
//! `httputil.ReverseProxy`; here we drive `hyper`/`hyper-util` directly. The
//! pure routing logic (`normalize_host`, `DomainRouter::match_route`) is kept
//! side-effect free so it stays directly unit-testable, exactly as in Go.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};

use crate::log;

use super::pages::render_upstream_down;
use super::server::{ProxyClient, Server};

/// Boxed response body used throughout the proxy.
pub type ResponseBody = BoxBody<Bytes, std::io::Error>;

/// A path-prefix route forwarding to a local port. Sorted by prefix length
/// descending within a [`DomainRouter`].
#[derive(Clone, Debug)]
pub struct PathRoute {
    pub prefix: String,
    pub port: u16,
}

/// Per-domain router: a default upstream plus longest-prefix path routes.
#[derive(Clone, Debug, Default)]
pub struct DomainRouter {
    pub default_port: u16,
    /// Whether CORS handling is enabled for this domain's upstreams.
    pub cors: bool,
    /// Path routes, sorted by prefix length descending.
    pub path_routes: Vec<PathRoute>,
}

impl DomainRouter {
    /// Longest-prefix path match. Returns the matched upstream port and, when a
    /// path route matched, the matched prefix (for `StripPrefix` semantics).
    /// Falls back to the default port and `None` prefix.
    ///
    /// Ports Go's `domainRouter.match` byte-for-byte.
    pub fn match_route(&self, req_path: &str) -> (u16, Option<&str>) {
        for pr in &self.path_routes {
            let prefix = pr.prefix.as_bytes();
            let path = req_path.as_bytes();
            let matched = req_path == pr.prefix
                || (req_path.starts_with(&pr.prefix)
                    && (prefix[prefix.len() - 1] == b'/'
                        || (path.len() > prefix.len() && path[prefix.len()] == b'/')));
            if matched {
                return (pr.port, Some(pr.prefix.as_str()));
            }
        }
        (self.default_port, None)
    }
}

/// Normalize a `Host` header value to a bare hostname.
///
/// Ports Go's `normalizeHost`: lowercase + trim space, strip a single trailing
/// dot, strip the `:port` suffix, strip surrounding `[]` (IPv6), strip a
/// trailing dot once more.
pub fn normalize_host(host: &str) -> String {
    let mut host = host.trim().to_lowercase();
    host = strip_suffix_dot(&host);

    // Go: net.SplitHostPort succeeds for "h:p"; otherwise if exactly one ':'
    // present, trim from the last ':'.
    if let Some(parsed) = split_host_port(&host) {
        host = parsed;
    } else if host.matches(':').count() == 1 {
        if let Some(idx) = host.rfind(':') {
            host = host[..idx].to_string();
        }
    }

    host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    strip_suffix_dot(&host)
}

fn strip_suffix_dot(s: &str) -> String {
    s.strip_suffix('.').unwrap_or(s).to_string()
}

/// Mimic Go's `net.SplitHostPort`: succeeds only for `host:port` where the port
/// is the trailing field after the last colon, the host part contains no stray
/// colon (unless bracketed), and the address is well-formed. We need only enough
/// fidelity to match the proxy's inputs: `name:port`, `[v6]:port`, bare `name`,
/// or bare `[v6]`. Returns the host on success, `None` otherwise.
fn split_host_port(host: &str) -> Option<String> {
    if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6: "[addr]" or "[addr]:port".
        let close = rest.find(']')?;
        let addr = &rest[..close];
        let after = &rest[close + 1..];
        if after.is_empty() {
            // "[addr]" with no port: SplitHostPort returns an error ("missing
            // port"), so report no split here.
            return None;
        }
        if let Some(port) = after.strip_prefix(':') {
            if port.is_empty() || port.contains(':') {
                return None;
            }
            return Some(addr.to_string());
        }
        return None;
    }

    // Unbracketed: must contain exactly one ':' to be a valid host:port (more
    // than one ':' means a bare IPv6 literal, which SplitHostPort rejects).
    let colons = host.matches(':').count();
    if colons != 1 {
        return None;
    }
    let idx = host.rfind(':')?;
    Some(host[..idx].to_string())
}

/// CORS headers added when the proxy itself answers (preflight) or wishes to
/// echo the origin. Mirrors Go's `setCORSHeaders`.
fn set_cors_headers(headers: &mut header::HeaderMap, origin: &HeaderValue) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS, HEAD"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Accept, Authorization, Content-Type, X-Requested-With"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
}

/// Strip any upstream CORS headers so the proxy's own values win. Mirrors Go's
/// `stripCORSHeaders` (used as the ReverseProxy `ModifyResponse`).
fn strip_cors_headers(headers: &mut header::HeaderMap) {
    headers.remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    headers.remove(header::ACCESS_CONTROL_ALLOW_METHODS);
    headers.remove(header::ACCESS_CONTROL_ALLOW_HEADERS);
    headers.remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS);
    headers.remove(header::ACCESS_CONTROL_MAX_AGE);
    headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
}

fn empty_body() -> ResponseBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full_body<B: Into<Bytes>>(chunk: B) -> ResponseBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

/// Build the `404 page not found` body Go's `http.NotFound` writes.
fn not_found() -> Response<ResponseBody> {
    let mut resp = Response::new(full_body("404 page not found\n"));
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// The upstream-down 502 page, rendered like Go's ReverseProxy `ErrorHandler`.
fn upstream_down(host: &str, port: u16) -> Response<ResponseBody> {
    let body = render_upstream_down(host, port);
    let mut resp = Response::new(full_body(body));
    *resp.status_mut() = StatusCode::BAD_GATEWAY;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

/// Top-level request handler. Mirrors Go's `buildHandler`'s closure.
///
/// Returns `Infallible` errors so it slots directly into a hyper service: all
/// failure modes are expressed as HTTP responses, exactly like Go.
pub async fn handle(
    server: Arc<Server>,
    req: Request<Incoming>,
) -> Result<Response<ResponseBody>, Infallible> {
    Ok(serve(server, req).await)
}

async fn serve(server: Arc<Server>, req: Request<Incoming>) -> Response<ResponseBody> {
    let host = normalize_host(host_header(&req));

    // Snapshot the router + cors flag under the read lock, mirroring Go's
    // `cfgMu.RLock()` window.
    let (router, cors) = {
        let st = server.state.read().await;
        (st.routers.get(&host).cloned(), st.cfg.cors)
    };

    let Some(router) = router else {
        return not_found();
    };

    // CORS: when enabled and an Origin header is present, the proxy echoes the
    // CORS headers (Go sets them on `w` before forwarding) and short-circuits an
    // OPTIONS preflight with 204. Mirrors Go's `buildHandler`.
    let cors_origin = if cors {
        req.headers()
            .get(header::ORIGIN)
            .filter(|v| !v.is_empty())
            .cloned()
    } else {
        None
    };
    if let Some(origin) = &cors_origin {
        if req.method() == Method::OPTIONS {
            let mut resp = Response::new(empty_body());
            *resp.status_mut() = StatusCode::NO_CONTENT;
            set_cors_headers(resp.headers_mut(), origin);
            return resp;
        }
    }

    let path = req.uri().path().to_string();
    let request_uri = request_uri(req.uri());
    let method = req.method().to_string();
    let (port, matched_prefix) = router.match_route(&path);

    let start = Instant::now();
    let mut resp = proxy_request(&server.client, req, &host, port, matched_prefix, cors).await;

    // Apply the proxy's own CORS headers (Go set these on `w` before the reverse
    // proxy ran; `ModifyResponse` stripped the upstream's, so the proxy's win).
    if let Some(origin) = &cors_origin {
        set_cors_headers(resp.headers_mut(), origin);
    }

    let status = resp.status().as_u16();

    log::request(&host, &method, &request_uri, port, status, start.elapsed());

    resp
}

/// Forward `req` to `http://localhost:{port}{stripped-path}`, preserving the
/// inbound `Host` header, streaming bodies, and bridging WebSocket/Upgrade
/// connections. On an upstream connect error, renders the upstream-down page.
async fn proxy_request(
    client: &ProxyClient,
    mut req: Request<Incoming>,
    host: &str,
    port: u16,
    matched_prefix: Option<&str>,
    cors: bool,
) -> Response<ResponseBody> {
    // Compute the upstream path (StripPrefix semantics for matched routes).
    let in_path = req.uri().path();
    let upstream_path = match matched_prefix {
        Some(prefix) => strip_prefix_path(in_path, prefix),
        None => in_path.to_string(),
    };
    let path_and_query = match req.uri().query() {
        Some(q) => format!("{upstream_path}?{q}"),
        None => upstream_path,
    };

    let upstream_uri: Uri = match format!("http://localhost:{port}{path_and_query}").parse() {
        Ok(u) => u,
        Err(_) => return upstream_down(host, port),
    };

    // Is this an Upgrade request (e.g. WebSocket)?
    let is_upgrade = req
        .headers()
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false)
        && req.headers().contains_key(header::UPGRADE);

    // Capture the client-side upgrade future before the body is consumed. This
    // removes the `OnUpgrade` from the request's extensions.
    let client_upgrade = if is_upgrade {
        Some(hyper::upgrade::on(&mut req))
    } else {
        None
    };

    // Preserve the inbound Host header (Go: `pr.Out.Host = pr.In.Host`).
    let inbound_host = host_header(&req).to_string();

    // Decompose the inbound request: reuse its method + headers for the outbound
    // request and stream its body.
    let (in_parts, in_body) = req.into_parts();

    let mut builder = Request::builder().method(in_parts.method).uri(upstream_uri);
    for (name, value) in in_parts.headers.iter() {
        builder = builder.header(name, value);
    }

    let out_body: ResponseBody = in_body.map_err(std::io::Error::other).boxed();

    let mut out_req = match builder.body(out_body) {
        Ok(r) => r,
        Err(_) => return upstream_down(host, port),
    };
    if let Ok(hv) = HeaderValue::from_str(&inbound_host) {
        out_req.headers_mut().insert(header::HOST, hv);
    }

    let mut upstream_resp = match client.request(out_req).await {
        Ok(r) => r,
        Err(_) => return upstream_down(host, port),
    };

    // WebSocket / Upgrade bridge: a 101 from upstream means we splice the two
    // upgraded connections together.
    if let Some(client_upgrade) = client_upgrade {
        if upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
            // Take the upstream upgrade future before decomposing the response.
            let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);
            return bridge_upgrade(upstream_resp, client_upgrade, upstream_upgrade);
        }
    }

    // Normal response: copy status + headers, optionally strip upstream CORS,
    // stream the body.
    let (mut parts, body) = upstream_resp.into_parts();
    if cors {
        strip_cors_headers(&mut parts.headers);
    }
    let resp_body: ResponseBody = body.map_err(std::io::Error::other).boxed();
    Response::from_parts(parts, resp_body)
}

/// Splice the client and upstream upgraded connections. Returns the 101 with the
/// upstream status + headers; the actual bidirectional copy runs in a spawned
/// task.
fn bridge_upgrade(
    upstream_resp: Response<Incoming>,
    client_upgrade: hyper::upgrade::OnUpgrade,
    upstream_upgrade: hyper::upgrade::OnUpgrade,
) -> Response<ResponseBody> {
    let (parts, _body) = upstream_resp.into_parts();

    tokio::spawn(async move {
        let (client_up, upstream_up) = match tokio::join!(client_upgrade, upstream_upgrade) {
            (Ok(c), Ok(u)) => (c, u),
            _ => return,
        };
        let mut client_io = hyper_util::rt::TokioIo::new(client_up);
        let mut upstream_io = hyper_util::rt::TokioIo::new(upstream_up);
        let _ = tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await;
    });

    // Return the 101 (with upstream status + headers) to the client. The body is
    // empty; the connection is taken over by the upgrade.
    let mut resp = Response::new(empty_body());
    *resp.status_mut() = parts.status;
    *resp.headers_mut() = parts.headers;
    resp
}

/// `http.StripPrefix` semantics: remove `prefix` from the front of `path`. The
/// caller only invokes this when `match_route` already confirmed the prefix
/// matches, so the remainder is empty or begins with `/` (or the prefix itself
/// ended with `/`). Empty or slash-less remainders are normalized to a leading
/// `/`, matching modern Go (the test expects `/api` -> `/`).
fn strip_prefix_path(path: &str, prefix: &str) -> String {
    let stripped = path.strip_prefix(prefix).unwrap_or(path);
    if stripped.is_empty() {
        "/".to_string()
    } else if stripped.starts_with('/') {
        stripped.to_string()
    } else {
        format!("/{stripped}")
    }
}

/// The inbound `Host` (hyper places the authority in the URI for HTTP/2 and in
/// the `Host` header for HTTP/1; prefer the header, falling back to authority).
fn host_header(req: &Request<Incoming>) -> &str {
    if let Some(h) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return h;
    }
    req.uri().authority().map(|a| a.as_str()).unwrap_or("")
}

/// Reconstruct the request URI string (path + query) like Go's
/// `r.URL.RequestURI()`.
fn request_uri(uri: &Uri) -> String {
    match uri.path_and_query() {
        Some(pq) => pq.as_str().to_string(),
        None => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO(test-phase): TestBuildHandlerRoutesKnownDomain,
    // TestBuildHandlerRoutesCustomTLD, TestBuildHandlerUnknownDomainReturnsNotFound,
    // TestBuildHandlerUpstreamDownReturnsBadGateway, TestPathRouteStripsPrefix,
    // TestCORSHeadersNotAddedByDefault, TestCORSEnabledStripsUpstreamHeaders,
    // TestCORSEnabledHandlesPreflight — these drive the full handler over a real
    // hyper connection (hyper's `Incoming` body has no public constructor, so the
    // request must come from a live server/client loop). Ported as end-to-end
    // integration tests in a later phase. The routing/normalize/CORS/strip-prefix
    // logic those tests exercise is covered by the pure-function tests below.

    #[test]
    fn test_normalize_host() {
        let cases = [
            ("myapp.test", "myapp.test"),
            ("MyApp.Test", "myapp.test"),
            ("  myapp.test  ", "myapp.test"),
            ("myapp.test.", "myapp.test"),
            ("myapp.test:443", "myapp.test"),
            ("myapp.test:8080", "myapp.test"),
            ("app.loc:443", "app.loc"),
            ("[::1]:443", "::1"),
            ("[::1]", "::1"),
            ("", ""),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_host(input), want, "normalize_host({input:?})");
        }
    }

    fn router_with_routes() -> DomainRouter {
        let mut routes = vec![
            PathRoute {
                prefix: "/api/v2".into(),
                port: 9090,
            },
            PathRoute {
                prefix: "/api".into(),
                port: 8080,
            },
            PathRoute {
                prefix: "/ws".into(),
                port: 9000,
            },
        ];
        // Mirror server.applyConfig's sort: prefix length descending.
        routes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        DomainRouter {
            default_port: 3000,
            cors: false,
            path_routes: routes,
        }
    }

    #[test]
    fn test_domain_router_match() {
        let router = router_with_routes();
        let cases: &[(&str, u16)] = &[
            ("/", 3000),
            ("/about", 3000),
            ("/api", 8080),
            ("/api/users", 8080),
            ("/api/v2", 9090),
            ("/api/v2/items", 9090),
            ("/apikeys", 3000),
            ("/ws", 9000),
            ("/ws/chat", 9000),
            ("/other", 3000),
        ];
        for (path, want_port) in cases {
            let (port, _) = router.match_route(path);
            assert_eq!(port, *want_port, "match({path:?})");
        }
    }

    #[test]
    fn test_match_returns_matched_prefix() {
        let router = router_with_routes();
        assert_eq!(router.match_route("/api/v2/items").1, Some("/api/v2"));
        assert_eq!(router.match_route("/api/users").1, Some("/api"));
        assert_eq!(router.match_route("/about").1, None);
    }

    #[test]
    fn test_strip_prefix_path() {
        // Mirrors TestPathRouteStripsPrefix expectations.
        assert_eq!(strip_prefix_path("/api/v1/health", "/api"), "/v1/health");
        assert_eq!(strip_prefix_path("/api/users/123", "/api"), "/users/123");
        assert_eq!(strip_prefix_path("/api", "/api"), "/");
        // Trailing-slash prefix leaves a slash-less remainder -> re-prefixed.
        assert_eq!(strip_prefix_path("/api/x", "/api/"), "/x");
    }

    #[test]
    fn test_set_and_strip_cors_headers() {
        let origin = HeaderValue::from_static("http://example.com");
        let mut h = header::HeaderMap::new();
        set_cors_headers(&mut h, &origin);
        assert_eq!(
            h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "http://example.com"
        );
        assert_eq!(
            h.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).unwrap(),
            "true"
        );
        assert_eq!(h.get(header::ACCESS_CONTROL_MAX_AGE).unwrap(), "86400");

        // Now simulate an upstream that set its own CORS headers and strip them.
        let mut up = header::HeaderMap::new();
        up.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("http://other.com"),
        );
        up.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET"),
        );
        up.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("X-Thing"),
        );
        strip_cors_headers(&mut up);
        assert!(up.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
        assert!(up.get(header::ACCESS_CONTROL_ALLOW_METHODS).is_none());
        assert!(up.get(header::ACCESS_CONTROL_EXPOSE_HEADERS).is_none());
    }
}
