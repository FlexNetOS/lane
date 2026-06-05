# lane — Architecture & Port Contract

`lane` is a faithful Rust port of the Go tool **slim** (`github.com/kamranahmedse/slim`).
Original source (read-only reference): `/home/drdave/Downloads/slim-extract/slim-main`.

This document is the **binding cross-module API contract**. Implementation agents MUST
match the public signatures below so modules integrate without churn. When a Go behavior
is ambiguous, the Go source is the source of truth — port behavior exactly (same error
strings where practical, same ordering, same edge cases).

## Global conventions

- Runtime: `tokio` (multi-threaded). `main.rs` is `#[tokio::main]`.
- Errors: functions return `anyhow::Result<T>` unless a more specific type is noted.
  Reproduce Go error message text closely (tests assert substrings).
- Logging/printing to the user goes through `crate::term` / `crate::log`, never raw
  `eprintln!` except where Go used `fmt.Fprintf(os.Stderr, ...)`.
- Install the rustls crypto provider once at process start (in `main` and in the daemon
  entrypoint): `rustls::crypto::ring::default_provider().install_default().ok();`
- `unsafe` is allowed only for libc calls (`geteuid`, `setsid`, `umask`) behind tiny wrappers.

## slim → lane renames (apply everywhere)

| Concept | slim | lane |
|---|---|---|
| Base dir | `~/.slim` | `~/.lane` |
| Socket | `slim.sock` | `lane.sock` |
| Pid | `slim.pid` | `lane.pid` |
| Hosts marker | `# slim` | `# lane` |
| iptables chain | `SLIM` | `LANE` |
| pf anchor name / file | `com.slim` / `/etc/pf.anchors/com.slim` | `com.lane` / `/etc/pf.anchors/com.lane` |
| Linux CA anchor basename | `slim.crt` | `lane.crt` |
| CA subject | Org `slim`, CN `slim Root CA` | Org `lane`, CN `lane Root CA` |
| Daemon re-exec marker env | (go-daemon internal) | `_LANE_DAEMON=1` |
| Tunnel server env | `SLIM_TUNNEL_SERVER` / `SLIM_TUNNEL_SERVER_API` | `LANE_TUNNEL_SERVER` / `LANE_TUNNEL_SERVER_API` |
| Default API base | `https://app.slim.sh` | `https://app.lane.sh` |
| Default tunnel server | `wss://app.slim.sh/tunnel` | `wss://app.lane.sh/tunnel` |
| Tunnel display domain | `.slim.show` | `.lane.show` |
| Error header | `X-Slim-Error` | `X-Lane-Error` |
| Console log prefix | `[slim]` | `[lane]` |
| Project file | `.slim.yaml` | `.lane.yaml` |
| Binary / upgrade repo | `kamranahmedse/slim` | `FlexNetOS/lane` |

Keep ports identical: `PROXY_HTTP_PORT = 10080`, `PROXY_HTTPS_PORT = 10443`.

## Module map (Rust module ⇐ Go package)

```
src/lib.rs            pub mod declarations + crate prelude (NO logic; owned by orchestrator)
src/main.rs           entrypoint: install crypto provider; if env _LANE_DAEMON -> daemon::run_foreground(); else cli::run()
src/config/           ⇐ internal/config        (mod.rs = config.go ; paths.rs = paths.go)
src/osutil/           ⇐ internal/osutil
src/httperr/          ⇐ internal/httperr       (mod.rs: from_response/status_hint ; network.rs: network_hint/wrap)
src/term/             ⇐ internal/term          (mod.rs styles+confirm+status ; step.rs RunSteps ; table.rs borderless table)
src/log/              ⇐ internal/log
src/protocol/         ⇐ protocol/protocol.go   (frame encode/decode + raw HTTP (de)serialize)
src/tunnel/           ⇐ internal/tunnel        (mod.rs re-exports; client.rs; subdomain.rs; pages.rs)
src/cert/             ⇐ internal/cert          (mod.rs ca+leaf+ensure ; trust.rs cfg-gated per OS)
src/system/           ⇐ internal/system        (hostfile.rs; portfwd.rs trait+impls; elevated.rs)
src/auth/             ⇐ internal/auth
src/project/  -> put in src/config/project.rs OR own module src/project — USE src/project/mod.rs
src/proxy/            ⇐ internal/proxy          (server.rs; handler.rs; health.rs; pages.rs)
src/setup/            ⇐ internal/setup
src/doctor/           ⇐ internal/doctor         (mod.rs + trust check cfg-gated)
src/daemon/           ⇐ internal/daemon         (mod.rs run/detach/ipc-handlers; socket.rs; protocol.rs)
src/cli/              ⇐ cmd/                    (one file per command + root.rs)
```

`src/lib.rs` will declare exactly these modules:
`config, osutil, httperr, term, log, protocol, tunnel, cert, system, auth, project, proxy, setup, doctor, daemon, cli`.

---

## src/config  (⇐ internal/config/config.go + paths.go)

```rust
// paths.rs
pub const PROXY_HTTP_PORT: u16 = 10080;
pub const PROXY_HTTPS_PORT: u16 = 10443;
pub fn api_base_url() -> String;       // env LANE_TUNNEL_SERVER_API or default https://app.lane.sh
pub fn tunnel_server_url() -> String;  // env LANE_TUNNEL_SERVER or default wss://app.lane.sh/tunnel
pub fn dir() -> PathBuf;        // ~/.lane  (resolve home via dirs::home_dir(); error if none)
pub fn config_path() -> PathBuf;       // dir/config.yaml
pub fn log_path() -> PathBuf;          // dir/access.log
pub fn socket_path() -> PathBuf;       // dir/lane.sock
pub fn pid_path() -> PathBuf;          // dir/lane.pid
pub fn tunnel_token_path() -> PathBuf; // dir/tunnel-token
pub fn auth_path() -> PathBuf;         // dir/auth.json
pub fn pf_token_path() -> PathBuf;     // dir/pf.token
// Go used config.Init() to cache home dir. In Rust resolve lazily each call (cheap) — no global init needed.

// config.rs
pub const LOG_MODE_FULL: &str = "full";
pub const LOG_MODE_MINIMAL: &str = "minimal";
pub const LOG_MODE_OFF: &str = "off";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Route { pub path: String, pub port: u16 }   // yaml: path, port

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Domain {                                     // yaml: name, port, routes(omit empty)
    pub name: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {                                     // yaml: domains, log_mode(omit empty), cors(omit empty)
    #[serde(default)] pub domains: Vec<Domain>,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub log_mode: String,
    #[serde(default, skip_serializing_if = "is_false")] pub cors: bool,
}

pub fn normalize_domain(name: &str) -> String;          // add ".test" if no '.'
pub fn validate_route(path: &str, port: i64) -> Result<()>;
pub fn validate_domain(name: &str, port: i64) -> Result<()>;  // label regex ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$, <=253, label<=63, port 1..=65535
pub fn validate_log_mode(mode: &str) -> Result<()>;
impl Config {
    pub fn effective_log_mode(&self) -> String;         // normalize: ""->full
    pub fn find_domain(&self, name: &str) -> Option<usize>;  // index
    pub fn set_domain(&mut self, name: &str, port: u16, routes: Vec<Route>) -> Result<()>; // upsert + save
    pub fn remove_domain(&mut self, name: &str) -> Result<()>;  // err "domain {name} not found" + save
    pub fn save(&self) -> Result<()>;                   // mkdir ~/.lane 0755, write config.yaml 0644
}
impl Domain { pub fn match_route(&self, req_path: &str) -> u16; } // longest-prefix path route match, else self.port
pub fn load() -> Result<Config>;                        // missing file -> default; migrate-normalize domains then save if changed
pub fn with_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T>; // flock EX on ~/.lane/config.lock (fs2)
fn is_false(b: &bool) -> bool { !*b }
```
Note: ports are `u16` in Rust; validators take `i64` so out-of-range CLI input still produces the
exact Go error. Where Go stored `int` ports, store `u16` after validation.

## src/project  (⇐ internal/project/project.go)

```rust
pub const FILE_NAME: &str = ".lane.yaml";
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Service { pub domain: String, pub port: u16, #[serde(default, skip_serializing_if="Vec::is_empty")] pub routes: Vec<crate::config::Route> }
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectConfig { #[serde(default)] pub services: Vec<Service>, #[serde(default)] pub log_mode: String, #[serde(default)] pub cors: bool }
pub fn find() -> Result<PathBuf>;                  // walk up from cwd to root looking for .lane.yaml
pub fn load(path: &Path) -> Result<ProjectConfig>; // parse + normalize_domain each service.domain
pub fn discover() -> Result<(ProjectConfig, PathBuf)>;
impl ProjectConfig { pub fn validate(&self) -> Result<()>; } // non-empty, log_mode, per-service validate, dup domain check, route validate
```

## src/osutil  (⇐ internal/osutil)

```rust
pub fn run_privileged(name: &str, args: &[&str]) -> std::io::Result<std::process::Output>; // geteuid==0 -> run direct; else prefix "sudo"
pub fn command_exists(name: &str) -> bool;   // PATH lookup (which-style)
pub fn geteuid() -> u32;                      // libc::geteuid()
```
`run_privileged` returns combined stdout+stderr like Go's `CombinedOutput`. Provide a helper
`combined_output(&Output) -> String` or have callers read both. Prefer returning `Output` and
let callers format `String::from_utf8_lossy(&out.stdout)+stderr`. Keep it simple: model
Go's `([]byte, error)` as returning the combined bytes via a small wrapper:
`pub fn run_privileged(name, args) -> (Vec<u8> /*combined*/, std::io::Result<()> /*exit status as err if non-zero*/)`.
IMPLEMENTERS: choose ONE shape and use it consistently across cert/system. Recommended:
`pub fn run_privileged(name: &str, args: &[&str]) -> (Vec<u8>, Result<()>)` where `Vec<u8>` is
combined output and `Result` is `Ok(())` on exit 0 else `Err(anyhow!("exit status N"))`.

## src/httperr  (⇐ internal/httperr)

```rust
pub fn from_response_blocking(status: u16, body: &[u8]) -> anyhow::Error; // parse {error|message}, else StatusHint
pub fn status_hint(code: u16) -> &'static str;     // exact mapping from Go
pub fn network_hint(err: &(dyn std::error::Error)) -> String; // DNS/timeout/refused/unreachable hints
pub fn wrap(context: &str, err: impl std::error::Error) -> anyhow::Error; // network err -> "ctx: hint" else "ctx: {err}"
```
Adapt to `reqwest`: inspect `reqwest::Error` (is_timeout, is_connect) for network_hint instead of
Go's `net.Error`/`net.DNSError`. Keep the human strings identical.

## src/term  (⇐ internal/term)

```rust
// styles: provide functions returning owo-colors-styled Strings to mirror lipgloss ANSIColor(n)
pub fn green<S: AsRef<str>>(s: S) -> String;   // ANSI 2
pub fn red<S: AsRef<str>>(s: S) -> String;     // ANSI 1
pub fn yellow<S: AsRef<str>>(s: S) -> String;  // ANSI 3
pub fn cyan<S: AsRef<str>>(s: S) -> String;    // ANSI 6
pub fn magenta<S: AsRef<str>>(s: S) -> String; // ANSI 5
pub fn dim<S: AsRef<str>>(s: S) -> String;     // faint
pub fn bold<S: AsRef<str>>(s: S) -> String;
pub fn check_mark() -> String;  // green ✓
pub fn cross_mark() -> String;  // red ✗
pub fn warn_mark() -> String;   // yellow !
pub fn confirm_prompt(msg: &str) -> bool;          // "{msg} [y/N] " read stdin; y/yes -> true
pub fn style_for_status(code: u16) -> fn(&str)->String; // >=500 red, >=400 yellow, >=300 cyan, else green  (return a styling closure/fn)

// step.rs
pub struct Step { pub name: String, pub run: Box<dyn FnOnce() -> Result<String>>, pub interactive: bool }
pub fn run_steps(steps: Vec<Step>) -> Result<()>;  // interactive -> print "· name" then run, no spinner;
                                                   // else indicatif spinner with title=name; on result prefix "skipped" -> warn line
// table.rs  (replaces lipgloss table; borderless, 2-space right padding, bold+faint header)
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String; // returns printable block (no trailing newline)
```
NOTE: status styling — return type must be ergonomic. Recommended:
`pub fn style_for_status(code: u16) -> fn(&str) -> String` returning one of the color fns.

## src/log  (⇐ internal/log)

Global async access-log writer (mirrors Go's channel+goroutine). Use a `tokio` task OR a
dedicated std thread + `std::sync::mpsc`; either is fine. Guard global state with `OnceLock`/`Mutex`.

```rust
pub fn set_output(path: &Path, mode: &str) -> Result<()>;  // shutdown prev writer; off->noop; rotate if >10MB; open append; spawn writer
pub fn close();
pub fn request(domain: &str, method: &str, path: &str, upstream: u16, status: u16, dur: Duration); // tab line; minimal vs full
pub fn info(msg: &str);    // "[lane] msg" cyan prefix  (callers format args themselves)
pub fn error(msg: &str);   // "[lane] msg" red prefix
pub fn format_duration(d: Duration) -> String;  // <1ms µs, <1s ms, else %.1fs
pub fn format_time_ago(t: chrono::DateTime<chrono::Local>) -> String; // just now / Nm ago / Nh ago / Nd ago
```
`info`/`error` in Go are variadic printf; in Rust callers pass a preformatted `String`.
Provide `info`/`error` taking `&str`. (Add `info_fmt!`/`error_fmt!` macros if convenient.)
Log line formats (TAB-separated, exactly as Go):
- full:    `HH:MM:SS\tdomain\tmethod\tpath\tupstream\tstatus\tdur\n`
- minimal: `HH:MM:SS\tdomain\tstatus\tdur\n`
Timestamp uses `chrono::Local::now().format("%H:%M:%S")`.

## src/protocol  (⇐ protocol/protocol.go) — tunnel wire format

```rust
#[derive(Serialize, Deserialize, Default)]
pub struct RegistrationRequest { pub token: String, pub subdomain: String,
    #[serde(skip_serializing_if="String::is_empty", default)] pub domain: String,
    #[serde(skip_serializing_if="String::is_empty", default)] pub password: String,
    #[serde(skip_serializing_if="String::is_empty", default, rename="ttl")] pub ttl: String }
#[derive(Serialize, Deserialize, Default)]
pub struct RegistrationResponse { pub ok: bool, pub url: String, pub subdomain: String,
    #[serde(default)] pub domain: String, #[serde(default)] pub error: String }

pub fn encode_frame(request_id: u32, data: &[u8]) -> Vec<u8>;      // 4-byte BE id + data
pub fn decode_frame(frame: &[u8]) -> Result<(u32, Vec<u8>)>;       // err "frame too short" if <4

// Raw HTTP/1.x wire (matches Go httputil.DumpRequest/DumpResponse semantics):
pub struct WireRequest { pub method: String, pub uri: String, pub headers: Vec<(String,String)>, pub body: Vec<u8> }
pub struct WireResponse { pub status: u16, pub reason: String, pub headers: Vec<(String,String)>, pub body: Vec<u8> }
pub fn deserialize_request(data: &[u8]) -> Result<WireRequest>;    // parse via httparse; read body by Content-Length/chunked
pub fn serialize_response(status: u16, reason: &str, headers: &[(String,String)], body: &[u8]) -> Vec<u8>; // HTTP/1.1 status line + headers + CRLF + body
```
The tunnel server sends raw HTTP request bytes; client parses, forwards to localhost, serializes
the local response back. JSON field names MUST match Go json tags (token, subdomain, ...). Serde
field renaming: struct fields snake match tags already except `ttl` (lowercase). Use `#[serde(rename_all=...)]`
only where it matches; explicit renames otherwise. `ok`,`url` lowercase tags — fields are `ok`,`url`.

## src/tunnel  (⇐ internal/tunnel)

```rust
// subdomain.rs
pub fn validate_subdomain(subdomain: &str) -> Result<()>;  // empty ok; strip '-' and '.', if normalized == brand or contains brand -> err. Port BLOCKED list verbatim.
// pages.rs
pub fn render_server_down(port: u16, error: &str) -> String; // include_str! assets/server_down.html, replace {{.Port}} and {{.Error}} ({{if .Error}} block)
// client.rs
pub struct RequestEvent { pub method: String, pub path: String, pub status: u16, pub duration: Duration }
pub struct ClientOptions { pub server_url: String, pub token: String, pub subdomain: String, pub domain: String,
    pub local_port: u16, pub password: String, pub ttl: Option<Duration>, pub on_request: Option<Box<dyn Fn(RequestEvent)+Send+Sync>> }
pub struct Client { /* opts, domain_url, conn */ }
impl Client {
    pub fn new(opts: ClientOptions) -> Self;
    pub async fn connect(&mut self) -> Result<String>;   // dial+register, spawn read loop, return public URL
    pub fn domain_url(&self) -> String;
    pub async fn close(&self);
}
```
Use `tokio-tungstenite` (`connect_async`) for wss. Registration: send JSON text frame, read JSON
text frame. Then binary frames carry `encode_frame(request_id, raw_http_response)`. For each inbound
binary frame: decode_frame -> deserialize_request -> issue to `http://localhost:{local_port}{uri}`
via `reqwest` -> serialize_response -> `encode_frame` -> write binary. Ping every 20s. Reconnect with
exponential backoff (1s..30s). Close codes 4000 (TTL) / 4001 (dropped) -> stop. On forward error,
respond with `render_server_down` as a 502 wire response and header `X-Lane-Error: connection-failed`.
The read/forward loop runs as a spawned task; `connect` returns once registered.

## src/cert  (⇐ internal/cert)

```rust
// mod.rs
pub fn ca_dir() -> PathBuf;            // ~/.lane/ca
pub fn ca_cert_path() -> PathBuf;      // ca/rootCA.pem
pub fn ca_key_path() -> PathBuf;       // ca/rootCA-key.pem
pub fn ca_exists() -> bool;
pub fn generate_ca() -> Result<()>;    // RSA-2048 CA (rsa crate -> rcgen), CN "lane Root CA", Org "lane", 10y, is_ca, pathlen 0, keyCertSign|crlSign; write cert 0644, key 0600
pub fn load_ca() -> Result<(rcgen::Certificate, rcgen::KeyPair)>; // or return parsed CA usable to sign leaves
pub fn certs_dir() -> PathBuf;         // ~/.lane/certs
pub fn leaf_cert_path(name: &str) -> PathBuf;  // certs/{name}.pem
pub fn leaf_key_path(name: &str) -> PathBuf;   // certs/{name}-key.pem
pub fn leaf_exists(name: &str) -> bool;
pub fn generate_leaf_cert(name: &str) -> Result<()>; // ECDSA P-256 leaf signed by CA; SAN dns={name}, ip 127.0.0.1 + ::1; 825d; serverAuth; write 0644/0600
pub fn ensure_leaf_cert(name: &str) -> Result<()>;   // exists && !needs_renewal -> ok else regen
pub fn leaf_needs_renewal(name: &str) -> bool;       // missing/parse-fail/non-ECDSA/<30d left -> true (use x509-parser)
pub fn load_leaf_tls(name: &str) -> Result<rustls::sign::CertifiedKey>; // load cert+key into a rustls CertifiedKey for the SNI resolver
// trust.rs  (cfg-gated)
pub fn trust_ca() -> Result<()>;       // linux: update-ca-certificates/update-ca-trust ; darwin: security add-trusted-cert ; else err
pub fn untrust_ca() -> Result<()>;
```
rcgen RSA: generate with `rsa::RsaPrivateKey::new(rng, 2048)`, encode to PKCS#8 DER
(`rsa::pkcs8::EncodePrivateKey`), build `rcgen::KeyPair::try_from(&der)`. Persisted PEM format may
differ from Go's PKCS#1; that's fine (new tool, own dir). `load_leaf_tls` returns a
`rustls::sign::CertifiedKey` (parse PEM via rustls-pemfile, build signing key via
`rustls::crypto::ring::sign::any_supported_type`). The proxy keeps a cache keyed by domain.

Trust (linux) anchor paths: `/usr/local/share/ca-certificates/lane.crt` (debian),
`/etc/pki/ca-trust/source/anchors/lane.crt` (rhel), `/etc/ca-certificates/trust-source/anchors/lane.crt` (arch).
Port `write_anchor_file` (mkdir -p w/ sudo fallback, write w/ `sudo tee` fallback) and
`remove_file_privileged` exactly.

## src/system  (⇐ internal/system)

```rust
// hostfile.rs   (HOSTS_PATH="/etc/hosts", MARKER="# lane")
pub fn add_host(name: &str) -> Result<()>;
pub fn remove_host(name: &str) -> Result<()>;
pub fn remove_all_hosts() -> Result<()>;
pub fn has_marked_entry(content: &str, hostname: &str) -> bool;
// elevated.rs
pub fn write_file_elevated(path: &str, content: &str) -> Result<()>; // try direct (0644); on permission -> `sudo tee`
// portfwd.rs
pub trait PortForwarder {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
    fn is_loaded(&self) -> bool;
    fn ensure_loaded(&self) -> Result<()>;
}
pub fn new_port_forwarder() -> Box<dyn PortForwarder>;  // cfg(linux)->LinuxPortFwd, cfg(darwin)->DarwinPortFwd, else Unsupported
```
Linux: iptables nat chain `LANE`, REDIRECT 80->10080 & 443->10443, OUTPUT jump `-o lo`. Port all
iptables string-matching helpers verbatim. Darwin: pf anchor `com.lane`, /etc/pf.anchors/com.lane,
pf.conf wiring, `pfctl -E` reference token persisted at `pf_token_path()`. Port verbatim.
For testability, make hostfile read/write injectable like Go (e.g. module-level fn pointers via
a small `#[cfg(test)]` seam, or pass an io trait). Simplest: factor pure logic
(`compute_added(content,name)->String`, `compute_removed(...)`, `compute_removed_all(...)`) so tests
hit pure functions; `add_host` etc. wire real IO.

## src/proxy  (⇐ internal/proxy)

```rust
// health.rs
pub async fn check_upstream(port: u16) -> bool;             // TCP connect localhost:port, 1s timeout
pub async fn check_upstreams(ports: &[u16]) -> Vec<bool>;   // concurrent, cap 16
pub async fn wait_for_upstream(port: u16, timeout: Duration) -> Result<()>; // poll 200ms
// pages.rs
pub fn render_upstream_down(host: &str, port: u16) -> String; // assets/upstream_down.html, replace {{.Host}} {{.Port}}
// server.rs
pub struct Server { /* Arc<RwLock<state>>: cfg, routers by domain, known domains, default domain, cert cache */ }
impl Server {
    pub fn new(cfg: Config) -> Self;
    pub async fn start(self: Arc<Self>) -> Result<()>;      // bind :10080 (redirect->https) + :10443 (rustls, h1+h2), serve until shutdown
    pub async fn shutdown(&self);
    pub async fn reload_config(&self) -> Result<Config>;    // config::load then apply
    async fn apply_config(&self, cfg: Config) -> Result<()>;// ensure+load leaf certs, build routers
}
```
Server design (replaces net/http + httputil.ReverseProxy):
- Shared state behind `Arc`; `tokio::sync::RwLock` for cfg/routers/known-domains; cert cache
  `RwLock<HashMap<String, Arc<CertifiedKey>>>` plus on-demand generation guarded so concurrent SNI
  for the same host generates once (use a per-host `tokio::sync::Mutex` map or a simple
  generate-then-insert with double-check; singleflight-equivalent).
- TLS: implement `rustls::server::ResolvesServerCert` that, given ClientHello SNI, normalizes host,
  checks known domains, returns cached `Arc<CertifiedKey>` (generate+cache if missing). Unknown/empty
  SNI with no default -> None. Build `ServerConfig` with this resolver + ALPN `["h2","http/1.1"]`.
- Listeners: `TcpListener` on both ports. HTTP listener: every request -> 301 to
  `https://{host}{uri}`. HTTPS listener: `tokio_rustls::TlsAcceptor` accept -> serve with
  `hyper_util::server::conn::auto::Builder::new(TokioExecutor)` + `.serve_connection_with_upgrades`.
- Request handling (handler.rs): normalize Host -> find router; if none -> 404. CORS preflight if
  enabled. Longest-prefix path route match (StripPrefix semantics for matched route). Reverse-proxy to
  `http://localhost:{port}` preserving inbound Host header. WebSocket/Upgrade: detect `Upgrade` /
  101 response and bidirectionally copy via `hyper::upgrade::on` on both client and upstream. Record
  status; on upstream connect error render `render_upstream_down` as 502 text/html. After response,
  `log::request(...)`.
- Provide `buildHandler` equivalent + helpers `normalize_host`, CORS set/strip.
Match path-route matching logic byte-for-byte with Go `domainRouter.match` and `Domain.match_route`.

## src/setup  (⇐ internal/setup)

```rust
pub fn ensure_first_run() -> Result<()>;        // if !ca_exists: RunSteps[Generate CA, Trust CA (interactive)]; if !pf.is_enabled: RunSteps[Setup port forwarding (skip on err)]
pub fn ensure_proxy_ports_available() -> Result<()>;  // try bind :10080 & :10443 (std TcpListener), error w/ Go message if busy
```

## src/doctor  (⇐ internal/doctor)

```rust
#[derive(Serialize)] #[serde(rename_all = "lowercase")]
pub enum Status { Pass, Warn, Fail }                       // json: "pass" | "warn" | "fail"
#[derive(Serialize)]
pub struct CheckResult { pub name: String, pub status: Status, pub message: String }
#[derive(Serialize)]
pub struct Report { #[serde(rename = "checks")] pub results: Vec<CheckResult> } // json key: "checks"
pub fn run() -> Report;   // CA cert, CA trust (cfg per OS), port forwarding, hosts per domain, daemon (IPC), leaf cert per domain
```
`Report`/`CheckResult`/`Status` derive `Serialize` so `cli/doctor.rs` can emit the report as JSON.
`cli doctor --json` prints `serde_json::to_string_pretty(&report)` — a single top-level
`{ "checks": [ { "name", "status", "message" }, … ] }` object (mirrors `cli list --json`); without
the flag it prints the human checklist. `DoctorArgs { json: bool }` carries the flag (see `src/cli`).
`verify_ca_is_trusted()` cfg-gated: linux checks anchor file presence in known dirs; darwin
`security verify-cert`; else Warn. Cert expiry via x509-parser; date format `%Y-%m-%d`.
`check_daemon`/`check_port_forwarding` call daemon + system. The IPC + health checks are async in
our impl; `run()` may be `async fn run() -> Report` (preferred) — CLI awaits it. Mark in cli.

## src/daemon  (⇐ internal/daemon)

```rust
// protocol.rs
pub enum MessageType { Shutdown, Status, Reload }   // serde rename to "shutdown"/"status"/"reload"
#[derive(Serialize,Deserialize)] pub struct Request { pub r#type: MessageType, #[serde(default)] pub data: Option<serde_json::Value> }
#[derive(Serialize,Deserialize)] pub struct Response { pub ok: bool, #[serde(default)] pub error: String, #[serde(default)] pub data: Option<serde_json::Value> }
#[derive(Serialize,Deserialize)] pub struct StatusData { pub running: bool, pub pid: i32, pub domains: Vec<DomainInfo> }
#[derive(Serialize,Deserialize)] pub struct DomainInfo { pub name: String, pub port: u16, pub healthy: bool, #[serde(default,skip_serializing_if="Vec::is_empty")] pub routes: Vec<RouteInfo> }
#[derive(Serialize,Deserialize)] pub struct RouteInfo { pub path: String, pub port: u16, pub healthy: bool }
// socket.rs
pub async fn send_ipc(req: Request) -> Result<Response>;   // dial unix socket, write JSON, read JSON (used by CLI; uses a short-lived current_thread runtime if called from sync ctx — see note)
pub struct IpcServer; impl IpcServer { pub async fn serve(...); }
// mod.rs
pub fn is_child() -> bool;                 // env _LANE_DAEMON == "1"
pub fn is_running() -> bool;               // socket exists && send_ipc(Status).ok  (sync wrapper)
pub fn run_detached() -> Result<()>;       // re-exec self detached: Command(self_exe).env(_LANE_DAEMON,1) + pre_exec(setsid) + null stdio + setsid; parent returns
pub async fn run_foreground() -> Result<()>; // the actual daemon body (called from main when _LANE_DAEMON set)
pub fn wait_for_daemon() -> Result<()>;    // poll is_running 50x/100ms; surface ~/.lane/daemon.err
```
IPC from the CLI (sync command handlers) needs send_ipc. Since CLI runs under `#[tokio::main]`,
expose `pub async fn send_ipc(...)` and an `is_running()` that blocks on it via
`tokio::task::block_in_place` + `Handle::current().block_on` OR make `is_running`/callers async.
RECOMMENDED: make the whole CLI async (handlers are `async fn`) and `send_ipc`/`is_running`/`run`
(doctor) async. `run_detached` stays sync (spawns process). Daemon body `run_foreground` is async:
load cfg, set log output, build `Arc<Server>`, spawn IPC server on unix socket, write pid, install
SIGINT/SIGTERM handler (tokio::signal) -> graceful shutdown, run server.

## src/cli  (⇐ cmd/)

`root.rs`: clap derive `Cli` with subcommands; `pub async fn run() -> anyhow::Result<()>`.
Top-level: on error print `\nError: {e}` to stderr (red "Error:") and exit 1. `version` prints `lane {VERSION}`; `version --json` prints a pretty `{"name","version"}` object (mirrors `list`/`doctor` `--json`).
`Version` const in `root.rs` (set from build; default "0.1.0"; overridable via `LANE_VERSION` build env or `clap` `version` attr).
Subcommands (one module each), behavior ported from the matching `cmd/*.go`:
`start, stop, restart, up, down, list, logs, share, doctor, login, logout, domain(add/list/verify/remove), uninstall, upgrade`.
`restart` = daemon-level bounce; reuses the `Shutdown` IPC + `run_detached`/`wait_for_daemon` (no new IPC verb, no config/hosts mutation).
`start, stop, up, down, list, logs, share, doctor, login, logout, domain(add/list/verify/remove), uninstall, upgrade, version`.
Helpers: `normalize_name`, `print_services`, `should_reload_port_forwarding`, `ingress_ports_reachable`.
Flag/duration parsing: `--ttl`/`--timeout` accept Go-style durations ("30m","1h","2h","500ms") via
`humantime::parse_duration`. `--route path=port` repeatable. `start --port` required.
HTTP calls (auth/domain/list/upgrade) use `reqwest` async client. Browser open via `open` crate.

## Release artifacts (for `cli/upgrade`)

`upgrade` downloads from GitHub releases of repo `FlexNetOS/lane`. Artifact names match
`.github/workflows/release.yml` exactly:
`lane_{version}_{os}_{arch}.tar.gz` plus a combined `checksums.txt` (lines `"<sha256>  <file>"`).
- `{version}` = tag without leading `v`.
- `{os}`: map Rust `std::env::consts::OS` → `linux` stays `linux`, `macos` → `darwin`.
- `{arch}`: map `std::env::consts::ARCH` → `x86_64` → `amd64`, `aarch64` → `arm64`.
The archive contains a single `lane` binary. Latest tag resolved via the
`releases/latest` redirect `Location` header (port slim's `latestTag`).

## Tests
Port every `*_test.go`. Go used package-level fn-pointer seams for mocking (e.g. `readFileHostFn`).
In Rust, prefer: (a) pure-function extraction for logic tests, (b) `tempfile::TempDir` + setting
`HOME` to an isolated dir for path/config/cert tests (config::dir() reads HOME), (c) spinning real
`tokio` TCP listeners for proxy/health tests. Use `#[serial_test::serial]` for tests mutating global
log state or `HOME`. Aim to preserve the assertions of the Go tests. Place unit tests in-module
under `#[cfg(test)] mod tests`; cross-cutting ones in `tests/`.
```
