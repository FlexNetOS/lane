# lane — Architecture & Port Contract

`lane` is a faithful Rust port of the Go tool **slim** (`github.com/kamranahmedse/slim`).
Original source (read-only reference): `/home/drdave/Downloads/tmp/router-lane/slim-extract/slim-main`.

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
src/service.rs        lane-original (Phase 7): OS service-unit generation (systemd/launchd)
src/inspect.rs        lane-original (Phase 7): request-inspector data model (parse + selection)
src/acme.rs           lane-original (Phase 7): ACME HTTP-01 issuance; live path gated by `acme` feature
src/webpolicy.rs      lane-original (Phase 8; ADR-0001): pure deny-by-default web-egress policy gate
src/web/              lane-original (Phase 8; ADR-0001): governed `lane web` seam; live spawn gated by `obscura` feature
src/relay/            lane-original (Phase C; ADR-0002): cross-machine relay (iroh); allowlist.rs/identity.rs pure, live.rs gated by `relay` feature
src/setup/            ⇐ internal/setup
src/doctor/           ⇐ internal/doctor         (mod.rs + trust check cfg-gated)
src/daemon/           ⇐ internal/daemon         (mod.rs run/detach/ipc-handlers; socket.rs; protocol.rs)
src/cli/              ⇐ cmd/                    (one file per command + root.rs)
```

`src/lib.rs` will declare exactly these modules:
`config, osutil, httperr, term, log, protocol, tunnel, cert, system, auth, project, proxy, service, inspect, acme, setup, doctor, daemon, cli`.
(`service`, `inspect`, and `acme` are lane-original Phase-7 additions — no slim counterpart.)

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
pub fn render_template(domain: &str, port: u16) -> String;  // lane-original (Phase 7): commented starter .lane.yaml (round-trips through load)
```
CLI: `lane config template [--domain <d>] [--port <p>] [--output <path>] [--force]` (`src/cli/config.rs`).

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
    pub local_host: String /* empty ⇒ localhost; lane-original reverse-tunnel */, pub local_port: u16,
    pub password: String, pub ttl: Option<Duration>, pub on_request: Option<Box<dyn Fn(RequestEvent)+Send+Sync>>,
    pub hops: Vec<HopSpec> /* empty ⇒ direct dial; lane-original multi-hop proxy chain */ }
// forward.rs (lane-original, Phase 7): chisel-style reverse-tunnel spec
pub struct ForwardSpec { pub remote_port: Option<u16>, pub local_host: String, pub local_port: u16 }
impl FromStr for ForwardSpec; // "R:[remotePort:][localHost:]localPort" — remote_port advisory (lane assigns the URL)
// hops.rs (lane-original, Phase 7): gost/chisel-style multi-hop proxy chain (CLIENT-SIDE dialing; wire format unchanged)
pub enum HopScheme { Socks5, Http }            impl HopScheme { pub fn as_str(self) -> &'static str }
pub struct HopAuth { pub username: String, pub password: String }
pub struct HopSpec { pub scheme: HopScheme, pub host: String, pub port: u16, pub auth: Option<HopAuth> }
impl HopSpec { pub fn authority(&self) -> String }
impl FromStr for HopSpec; // "[scheme://][user:pass@]host:port" — scheme socks5(default)|http; port 1..=65535; host non-empty
// dialer.rs (lane-original, Phase 7): builds the TCP byte-tunnel through the hop chain before the wss upgrade
pub async fn dial_through_hops(hops: &[HopSpec], target: &str) -> Result<TcpStream>; // empty chain ⇒ direct connect; live cross-host path un-CI-able
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
binary frame: decode_frame -> deserialize_request -> issue to `http://{local_host}:{local_port}{uri}` (host defaults `localhost`)
via `reqwest` -> serialize_response -> `encode_frame` -> write binary. Ping every 20s. Reconnect with
exponential backoff (1s..30s). Close codes 4000 (TTL) / 4001 (dropped) -> stop. On forward error,
respond with `render_server_down` as a 502 wire response and header `X-Lane-Error: connection-failed`.
The read/forward loop runs as a spawned task; `connect` returns once registered.

When `ClientOptions.hops` is non-empty, the dial is routed through the proxy chain (lane-original,
Phase 7): `dialer::dial_through_hops` opens a TCP stream to hop 1, asks each hop to CONNECT to the
next authority (SOCKS5 per RFC 1928/1929, or HTTP `CONNECT` per RFC 7231) ending at the tunnel
server's `host:port`, then the `wss` TLS+WebSocket upgrade runs over that stream via
`client_async_tls_with_config`. This is a purely **client-side dialing** decision — the wire format
above is unchanged. The per-hop protocol encoders are unit-tested; the live chain across real
intermediate proxies is un-CI-able (needs real SOCKS5/HTTP egress hosts), documented like ACME's
live Let's Encrypt round-trip. No new dependency: the dialer is pure-Rust over `tokio` TCP.

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
pub enum ForwardingStatus { Present, Absent, Unknown }   // three-way probe result
pub trait PortForwarder {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
    fn is_loaded(&self) -> bool;
    fn ensure_loaded(&self) -> Result<()>;
    fn forwarding_status(&self) -> ForwardingStatus;     // default: Present if is_enabled else Absent
}
pub fn new_port_forwarder() -> Box<dyn PortForwarder>;  // cfg(linux)->LinuxPortFwd, cfg(darwin)->DarwinPortFwd, else Unsupported
```
`forwarding_status` distinguishes "could not check without root" (`Unknown`) from genuinely-absent so the
read-only `doctor` probe never escalates with sudo. Linux maps the `iptables -C` exit code:
0->`Present`, 4 (permission denied)->`Unknown`, other non-zero / spawn error->`Absent`; `is_enabled() ==
(forwarding_status() == Present)`.
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

## src/service  (lane-original — Phase 7; no slim counterpart)

OS service-unit generation so the lane daemon auto-starts at login/boot. User-level (no root):
the unit's start command re-execs the lane binary with `_LANE_DAEMON=1` (same trigger as
`daemon::run_detached`). Render fns are pure (unit-testable); `install()` does the I/O.

```rust
pub enum Manager { Systemd, Launchd }
impl Manager {
    pub fn detect() -> Result<Self>;            // Linux→Systemd, macOS→Launchd, else error
    pub fn label(self) -> &'static str;          // "systemd (user unit)" / "launchd (LaunchAgent)"
    pub fn unit_path(self) -> Result<PathBuf>;   // ~/.config/systemd/user/lane.service | ~/Library/LaunchAgents/com.lane.daemon.plist
}
pub fn render_systemd_unit(exe: &Path) -> String;   // [Service] Environment=_LANE_DAEMON=1, ExecStart={exe}, Restart=on-failure, WantedBy=default.target
pub fn render_launchd_plist(exe: &Path) -> String;  // Label=com.lane.daemon, ProgramArguments=[exe], _LANE_DAEMON=1, RunAtLoad, KeepAlive
pub fn render(manager: Manager, exe: &Path) -> String;
pub struct Installed { pub manager: &'static str, pub path: PathBuf, pub enabled: bool, pub enable_hint: &'static str }
pub fn install(enable: bool) -> Result<Installed>;  // write unit (mkdir -p); if enable: systemctl --user enable --now | launchctl load
```
CLI: `lane install --service [--enable] [--print] [--json]` (`src/cli/install.rs`).

## src/acme  (lane-original — Phase 7; no slim counterpart)

ACME (RFC 8555) certificate issuance for `lane start --acme` — a real Let's Encrypt cert via the
HTTP-01 challenge. The **live** issuance path (`instant-acme`, network) is behind the **`acme` cargo
feature**; the default build never compiles the ACME client (dependency-light). Pure parts +
challenge responder are always compiled and tested.

```rust
pub struct AcmeParams { pub domain, email: String, pub staging: bool, pub challenge_addr: SocketAddr }
impl AcmeParams { pub fn validate(&self) -> Result<()>;  // reject .test/.local/localhost/bare-IP/empty-email
                  pub fn directory_url(&self) -> &'static str; }  // LE prod vs staging
pub fn challenge_path(token: &str) -> String;            // /.well-known/acme-challenge/{token}
pub struct ChallengeStore(/* token → key-authorization */);  // set/get/clear
pub async fn serve_http01(store, addr) -> Result<Responder>; // minimal HTTP-01 responder (200 keyauth / 404)
pub struct Issued { pub cert_pem: String, pub key_pem: String }
#[cfg(feature = "acme")]      pub async fn issue(&AcmeParams) -> Result<Issued>;  // account→order→http-01→finalize→download
#[cfg(not(feature = "acme"))] pub async fn issue(&AcmeParams) -> Result<Issued>;  // fail-closed: "rebuild with --features acme"
```
CLI: `lane start --acme [--acme-email <addr>] [--acme-staging]`. Issued certs are written to
`~/.lane/acme/{domain}/{cert,key}.pem` by `cert::write_acme`; the proxy resolver (`load_leaf`/
`ensure_leaf` in `proxy::server`) **prefers an on-disk ACME cert** (`cert::acme_exists` →
`cert::load_acme_tls`) over the CA-signed leaf, so a real cert is served without clobbering the leaf
store. HTTP-01 responder addr overridable via `LANE_ACME_HTTP_ADDR` (default `0.0.0.0:80`).
Build live: `cargo build --features acme`.

## src/inspect  (lane-original — Phase 7; no slim counterpart)

Data model + pure logic for `lane inspect`, the live request-inspector TUI. Tails the daemon's
access log (the proxy's per-request record) and renders requests in a scrollable table + detail
pane. The interactive shell (crossterm alternate screen / raw mode / key events, comfy-table
rendering) is in `src/cli/inspect.rs`; non-TTY stdout prints a one-shot snapshot.

```rust
pub struct Entry { pub ts, domain, method, path, upstream, status, duration: String }  // method/path/upstream empty in minimal mode
impl Entry { pub fn parse(line: &str) -> Option<Entry>; }   // 7 cols = full, 4 cols = minimal (mirrors cli::logs)
pub struct State { pub entries: Vec<Entry>, pub selected: usize }
impl State { pub fn new(); push(Entry); push_line(&str)->bool; select_next(); select_prev(); selected()->Option<&Entry> }
```
CLI: `lane inspect [name]` (`src/cli/inspect.rs`). New dep: `crossterm` (already in-tree via comfy-table).

## src/webpolicy + src/web  (lane-original — Phase 8; ADR-0001 lane↔obscura seam; no slim counterpart)

The **governed-egress `lane web` seam** (ADR-0001 Option B): lane is the network control plane;
obscura is a managed web-egress engine that lane spawns and **pins through lane's own governed forward
proxy + policy**. obscura is an **external child process**, never a linked crate. The live spawn is
behind the **`obscura` cargo feature** (`obscura = []`, no new dependency); the default build compiles
none of the live spawn but **always** compiles + tests the pure layer (gate + spawn-plan) **and the
governed proxy** (`src/web/proxy.rs`). Deny-by-default everywhere.

**Two-layer governance (defense in depth).** (1) The *entry* op's URL is gated by `web::authorize`
(`webpolicy::check`) before any spawn. (2) lane RUNS its own **governed forward proxy** (`GovernedProxy`,
`src/web/proxy.rs`) on an ephemeral loopback port and pins obscura's egress to it (obscura's `--proxy`
points at lane), so **every connection obscura opens** — not just the entry URL — is independently
policy-checked and access-logged. lane is the actual egress governor at the packet level (ADR-0001
§2/§4), not merely a config-passer.

`src/webpolicy` — pure, I/O-free, deny-by-default validator (the gate):
```rust
pub enum Scheme { Http, Https }                                  // only candidate-allowable schemes
pub enum DenyReason { MalformedTarget(String), SchemeNotAllowed(String), HostNotAllowed(String),
                      PortNotAllowed(u16), Loopback, PrivateNetwork, LinkLocal, SharedAddress,
                      Unspecified, Multicast, Reserved }          // serde; Display = actionable msg
pub enum PolicyDecision { Allow, Deny(DenyReason) }              // is_allowed()/is_denied()/deny_reason()
pub enum HostRule { Exact(String), DomainSuffix(String) }       // serde
pub struct WebPolicy { pub allow_hosts: Vec<HostRule>, pub allow_ports: Vec<u16>, pub guard_ip_literals: bool }
impl Default for WebPolicy;                                       // DENY-EVERYTHING: empty allowlist, ports {80,443}, guards on
impl WebPolicy { pub fn deny_all()->Self; allow_host(impl Into<String>)->Self; allow_domain(..)->Self;
                 allow_ports(impl IntoIterator<Item=u16>)->Self; allow_port(u16)->Self;          // builders
                 pub fn check(&self, target:&str)->PolicyDecision;                                // parse URL → check_addr
                 pub fn check_addr(&self, host:&str, port:u16, scheme:Scheme)->PolicyDecision;    // core
                 pub fn check_ip(&self, ip:IpAddr, port:u16)->PolicyDecision; }                   // daemon's resolution-time re-check (anti-rebind)
```
DNS is out of scope by design (pure): `check` does not resolve hostnames; re-validating the *resolved*
IP via `check_ip` is the **daemon's** job (DNS-rebinding defense). IP-literal targets are SSRF-guarded
regardless of the allowlist.

`src/web` — the seam mechanism (pure layer always compiled; live spawn `#[cfg(feature="obscura")]`):
```rust
pub enum WebOp { Open { url: String }, Run { script_path: String, url: String } }   // the governed op
impl WebOp { pub fn target(&self)->&str;  pub fn kind(&self)->&'static str; }        // "open"|"run"; target = policy-checked URL
pub fn authorize(policy:&WebPolicy, op:&WebOp) -> Result<(), DenyReason>;            // the gate: runs webpolicy::check before any spawn
pub struct ObscuraSpawn { pub program: String, pub args: Vec<String>, pub envs: Vec<(String,String)> }  // pure command PLAN (data, runs nothing)
pub enum SpawnPlanError { MissingBin, ScriptUnreadable(String) }                     // seam-misconfigured / Run script unreadable (≠ a policy denial); Display+Error
impl ObscuraSpawn { pub fn plan(cfg:&ObscuraConfig, proxy:&str, ca_pem_path:&str, op:&WebOp) -> Result<ObscuraSpawn, SpawnPlanError>; }
//   `proxy` is supplied by the LIVE caller (lane's GovernedProxy::addr), NOT read from config — that is how obscura is pinned to lane.
pub struct WebOutcome { pub op: &'static str, pub target: String, pub allowed: bool }
pub async fn run(policy:&WebPolicy, cfg:&ObscuraConfig, ca_pem_path:&str, op:&WebOp) -> Result<WebOutcome>;
//   gate FIRST (deny-by-default precedes any feature check) → (feature) start GovernedProxy, plan pinned to its addr, spawn obscura, shut proxy down on exit / (no feature) fail-closed
// #[cfg(not(feature="obscura"))] run_authorized() → Err: "obscura integration is not enabled … rebuild with --features obscura (Phase A1)"

// src/web/proxy.rs — lane's OWN governed forward proxy (ALWAYS compiled + unit-tested; no obscura dep):
pub struct GovernedProxy { /* loopback listener + accept task */ }
impl GovernedProxy {
  pub async fn start(policy: WebPolicy) -> Result<GovernedProxy>;                      // bind 127.0.0.1:0, spawn accept loop (direct egress, no MITM)
  pub async fn start_with_upstream(policy: WebPolicy, upstream: Option<String>) -> Result<GovernedProxy>; // upstream=Some → chain allowed egress through it (after governance); no MITM
  pub async fn start_with_options(policy: WebPolicy, upstream: Option<String>, tls_inspect: bool) -> Result<GovernedProxy>; // full options: optional upstream chaining + optional TLS-inspect (MITM)
  pub fn addr(&self) -> String;        // "http://127.0.0.1:<port>" to hand obscura's --proxy
  pub fn socket_addr(&self) -> SocketAddr;
  pub fn shutdown(&self);              // abort accept loop; also runs on Drop (RAII)
}
//   CONNECT host:port → webpolicy.check_addr(host,port,Https) FIRST (both modes): DENY → 403, never connects.
//     ALLOW + tls_inspect=false (default) → 200 + copy_bidirectional opaque TLS bytes to upstream TcpStream (no MITM).
//     ALLOW + tls_inspect=true → handle_connect_mitm: 200 ack → terminate client TLS with a per-host leaf signed by lane's CA
//       (cert::ensure_leaf_cert + load_leaf_tls; obscura trusts lane's CA via --ca) → on the decrypted stream loop over requests
//       (keep-alive): reconstruct https://host[:port]/path, webpolicy.check(url) PATH-AWARE deny-by-default → DENY → 403 over TLS;
//       ALLOW → forward via in-tree reqwest (RE-ORIGINATING REAL, VALIDATED TLS to the true upstream; honors upstream chaining) →
//       relay response back over the client TLS stream. Fail-closed: any TLS/parse/leaf/forward error logs + closes (NEVER an
//       ungoverned tunnel fallback). MITM is on the obscura↔lane hop only — lane↔origin keeps full cert validation.
//   absolute-form HTTP (GET http://host/…) → webpolicy.check(url): ALLOW → forward via in-tree reqwest, relay response; DENY → 403. Malformed/origin-form → 403 (fail-closed).
//   EVERY request (ALLOW and DENY) logged via crate::log::info ("web-egress ALLOW/DENY <METHOD> <url>") — the single observability point (ADR §4).
//   tunnel-mode (default) governs at host/port; inspect-mode governs at full-URL/path. MITM is OPT-IN (config web_tls_inspect, default false).
```
**Emitted obscura CLI (matches obscura's REAL `crates/obscura-cli` surface; requires obscura ≥ the
Phase A1-2 `--ca` capability):** `plan()` emits obscura's **globals first**, then its `fetch`
subcommand — there is no `open`/`run` subcommand in obscura.
- Globals (before the subcommand), in order: `--proxy <governed-proxy-addr>`, `--ca <ca_pem_path>`,
  `--allow-private-network`, and `--user-agent <ua>` when configured. The `<governed-proxy-addr>` is
  **lane's own `GovernedProxy::addr`** (a loopback URL), supplied by the live caller — not the user's
  `obscura_proxy` config. `--allow-private-network` is
  **mandatory**: lane's proxy listens on loopback (`127.0.0.1`) and obscura BLOCKS loopback/RFC1918 by
  default via its SSRF guard, so without it obscura cannot even connect to lane's proxy. The governed
  spawn intentionally routes through lane's loopback listener, so private-network access to *reach the
  proxy* is required and safe — obscura's egress stays pinned to lane.
- Subcommand: `WebOp::Open { url }` → `fetch <url>`. `WebOp::Run { script_path, url }` →
  `fetch <url> --eval <SCRIPT-CONTENTS>` — obscura's `--eval` takes a JS **string**, not a path, so
  `plan()` reads the script file's contents at plan time (fail-closed: `ScriptUnreadable(String)` if the
  file can't be read — never an empty eval).
- `--stealth` (when `obscura_stealth`) is appended **after** the `fetch` subcommand — it is
  per-subcommand in obscura, not a global, and requires obscura built `--features stealth`.

**Egress-pinning contract (the heart of "under lane's control at the packet level"):** `plan()` is the
pure function that enforces it and is fully unit-tested without obscura. The plan ALWAYS (a) takes
`program` from config (`obscura_bin`), **never** the ambient `$PATH`; (b) sets `--proxy <proxy>` — the
`proxy` parameter the LIVE caller passes, which is `GovernedProxy::addr` — **and** the standard
`HTTP_PROXY`/`HTTPS_PROXY` (+ lowercase) + `LANE_OBSCURA_PROXY` env so a flag-ignoring obscura build
still cannot escape the pin; (c) trusts lane's CA via `--ca <ca_pem_path>` + `SSL_CERT_FILE`/`LANE_CA`
(obscura honors `SSL_CERT_FILE` as a CA fallback since A1-2). `plan()` refuses
(`MissingBin`/`ScriptUnreadable`) rather than emit an unpinned or empty-eval spawn.

The live `run_authorized` (feature) is the wiring that makes lane the governor: it (1) starts a
`GovernedProxy` on loopback with the same `WebPolicy`; (2) calls `plan(cfg, governed.addr(), …)` so
obscura is pinned to **lane's** proxy; (3) builds a `tokio::process::Command` from the plan and logs the
governed request via `crate::log::info`; (4) waits on obscura's exit, then **shuts the governed proxy
down** (explicit `shutdown()` plus the `Drop` RAII guard). CA path comes from
`crate::cert::ca_cert_path()`. obscura must be built so `obscura_bin` points at a real binary (and, for
stealth, built `--features stealth`).

**`obscura_proxy` = OPTIONAL upstream (semantics change).** `obscura_proxy` is no longer the proxy
obscura points at (obscura points at lane's governed proxy). It is repurposed as the optional
**upstream** lane's governed proxy chains *allowed* traffic through *after* governance — so an org can
still route governed egress via a corporate proxy. Both cases are implemented: `obscura_proxy` unset →
the governed proxy connects directly; `obscura_proxy` set (validated `http://host:port`) → allowed HTTP
egresses via a reqwest client pointed at the upstream and allowed CONNECTs tunnel via a **nested
CONNECT** through it (a malformed upstream URL is rejected at `start`, fail-closed). Governance always
runs **first** — a denied target never reaches the upstream. The MITM (`tls_inspect`) path likewise
honors the upstream chain when forwarding decrypted requests.

**Config keys** (in `src/config`, all `#[serde(default)]` → old `.lane.yaml` still parses; inert
without the feature): `obscura_bin/obscura_proxy: Option<String>`, `obscura_stealth: bool`,
`obscura_user_agent: Option<String>`, `web_allow_hosts/web_allow_domains: Vec<String>`,
`web_allow_ports: Vec<u16>`, `web_deny_paths/web_allow_paths: Vec<String>` (path-rule prefixes; only
enforced when a full URL/path is seen — plain-HTTP forward + TLS-inspect), and
`web_tls_inspect: bool` (**default false**; opt-in TLS-inspection/MITM for the governed proxy).
`Config::obscura() -> ObscuraConfig { bin, proxy, stealth, user_agent }`
applies `LANE_OBSCURA_{BIN,PROXY,STEALTH,USER_AGENT}` env overlays (env wins; empty string does not
override the file; stealth OR-ed). `Config::web_policy() -> WebPolicy` builds the deny-by-default gate
from the allow-lists (empty `web_allow_ports` keeps `{80,443}`) **plus the `web_deny_paths` /
`web_allow_paths` path rules**. `Config::web_tls_inspect() -> bool` returns the flag OR the
`LANE_WEB_TLS_INSPECT` env override (truthy ⇒ on).

CLI: `lane web open <url>` / `lane web run <script> --url <url>` (`src/cli/web.rs`), both `--json`
(`{op, target, allowed, error?}`). The command is **always present** (so `lane web --help` works in the
default build); only the live action is gated. Flow: `config::load` → `web_policy()` → `obscura()` →
`web_tls_inspect()` → `web::run(policy, obscura, ca_pem, tls_inspect, op)`. In a `--features obscura`
build this now: gates the entry URL → starts lane's governed proxy (with the config's `tls_inspect`) →
spawns real obscura pinned through it → every egress connection is policy-checked + logged →
shuts the proxy down on exit. The default build still fail-closes after the gate with the clear "rebuild
with `--features obscura`" error. `obscura_bin` must point at a real obscura binary. Build live:
`cargo build --features obscura`.

**Path-level TLS-inspect (MITM) — IMPLEMENTED, opt-in.** With `web_tls_inspect: true` (default false),
allowed HTTPS `CONNECT`s are TLS-terminated and governed at the full-URL/path level: lane mints a
per-host leaf signed by its **own CA** (which obscura already trusts via the always-emitted `--ca`),
decrypts the obscura↔lane hop, runs the path-aware `webpolicy::check(url)` on each request (so
`web_deny_paths` / `web_allow_paths` bite on HTTPS), logs the full URL, then **re-originates real,
validated TLS** to the true upstream (lane↔origin keeps normal cert validation). Off ⇒ opaque CONNECT
tunnels governed at host/port (matching webpolicy's tunnel granularity). The two granularities:
tunnel-mode = host/port; inspect-mode = full-URL/path. MITM only ever intercepts obscura's OWN governed
egress, never third-party traffic, and fails closed on any error (never an ungoverned tunnel).

**NEXT (deferred to Phase A1, NOT built here):** a daemon IPC / MCP `lane_web` dispatcher so agents
reach the seam the same way they reach obscura's MCP — but through lane's gate (reusing `web::authorize`
+ `webpolicy::check_ip` at resolution time).

## src/relay  (lane-original — Phase C; ADR-0002 cross-machine relay)

The **cross-machine relay**: every lane node is a relay-capable **iroh** (QUIC p2p) peer in a trusted
fleet mesh. A node accepts inbound connections **only** from NodeIds on a **deny-by-default
trusted-node allowlist**, and — before bridging a relayed request to a local service — runs the
**same** deny-by-default `crate::webpolicy` check + access-log it runs for local traffic. That is
**governance-across-the-link**: a cross-machine request is trust-checked, webpolicy-checked, and logged
at the **destination** node exactly like a local one (ADR-0002 §"governance composition"; mirrors
`src/web/proxy.rs` GovernedProxy semantics — same deny+log shape, reusing `webpolicy`).

The iroh transport is behind the **`relay` cargo feature** (`relay = ["dep:iroh", "iroh/tls-ring"]`);
the default build compiles none of it. **MSRV 1.89** (`rust-version` in `Cargo.toml`) is driven by
modern iroh — the default (no-relay) build still compiles on the pinned stable toolchain. The
**security core and wire framing are always compiled and tested** in every build.

**Transport / iroh 0.98 API used** (read the vendored source, not older iroh — 0.98 renamed
`NodeId`/`NodeAddr` → `EndpointId`/`EndpointAddr`): `iroh::Endpoint::builder(presets::Minimal)`
(`Minimal` needs the `tls-ring` crypto provider — matches lane's process-wide ring provider via
`install_crypto_provider()`), `.secret_key(SecretKey).alpns(vec![b"lane/relay/1"]).relay_mode(..).bind()`;
`endpoint.id() -> EndpointId`, `endpoint.addr() -> EndpointAddr`, `endpoint.accept()` →
`Incoming::await` → `Connection`, `conn.remote_id() -> EndpointId`, `conn.accept_bi()/open_bi()` →
`(SendStream, RecvStream)`. Streams implement tokio `AsyncRead`/`AsyncWrite`, so the bridge is
`tokio::io::copy_bidirectional` between `tokio::io::join(recv, send)` and a local `TcpStream`.

`src/relay/allowlist.rs` — **pure, always-compiled, deny-by-default node trust** (the security core):
```rust
pub fn is_trusted(allowlist: &[String], node_id: &str) -> bool;  // empty allowlist ⇒ trust NOTHING
pub fn parse_node_id(id: &str) -> Result<String, String>;        // validate 64-hex-char NodeId shape
pub fn normalize_node_id(id: &str) -> String;                     // lowercase + trim
```

`src/relay/identity.rs` — node identity (path helpers pure; load/gen `#[cfg(feature="relay")]`):
```rust
pub fn relay_dir() -> PathBuf;        // ~/.lane/relay (0700)
pub fn node_key_path() -> PathBuf;    // ~/.lane/relay/node.key (0600, 32-byte secret as 64-hex)
#[cfg(feature="relay")] pub fn load_or_generate_secret_key() -> Result<SecretKey>;  // stable across runs
#[cfg(feature="relay")] pub fn node_id_string(key: &SecretKey) -> String;           // derived NodeId
```

`src/relay/mod.rs` — **wire protocol** (pure, always-compiled + unit-tested):
- **ALPN:** `lane/relay/1` (`RELAY_ALPN`).
- **Request frame** (connector → acceptor, on a fresh bi-stream): 2-byte big-endian length `N` +
  `N` UTF-8 bytes of `host:port` (`TargetRequest::{encode,parse,wire_string}`; bracketed IPv6
  `[::1]:443` supported; `MAX_TARGET_LEN`).
- **Response frame** (acceptor → connector): 1 status byte — `RESP_OK` (0, allowed + connected; opaque
  bridged bytes follow) or `RESP_DENIED` (1) + 2-byte BE reason length + UTF-8 reason
  (`encode_denied`).

`src/relay/live.rs` — iroh transport (`#[cfg(feature="relay")]`):
```rust
pub struct RelayEndpoint { /* bound iroh Endpoint */ }
impl RelayEndpoint {
    pub async fn bind(secret_key: SecretKey, relay_mode: RelayMode) -> Result<RelayEndpoint>;
    pub fn node_id(&self) -> String;  pub fn endpoint(&self) -> &Endpoint;  pub async fn close(&self);
}
pub struct AcceptConfig { pub trusted_nodes: Vec<String>, pub policy: WebPolicy }
// THE GOVERNED ACCEPT LOOP (governance-across-the-link):
//   accept → reject if !is_trusted(remote_id)  (deny-by-default node trust, log + close)
//          → read TargetRequest frame
//          → policy.check_addr(host, port, Https)  (SAME webpolicy as local; deny → error frame, NO connect, log)
//          → TcpStream::connect((host,port)) → RESP_OK → copy_bidirectional   (ALLOW; log)
pub async fn run_accept_loop(endpoint: &Endpoint, config: AcceptConfig) -> Result<()>;
// CONNECT side: dial trusted node → open_bi → send TargetRequest → read status → bridge local ⇄ stream
pub async fn connect_and_bridge(endpoint:&Endpoint, peer:EndpointAddr, target:&TargetRequest, local:TcpStream) -> Result<()>;
pub async fn serve_local_bridge(endpoint:Endpoint, peer:EndpointAddr, target:TargetRequest, listener:TcpListener) -> Result<()>;
pub fn relay_mode_from_config(cfg:&Config) -> RelayMode;  // driven by relay_servers: empty→Default (n0 public relays); ["disabled"]→Disabled; URLs→Custom(self-hosted DERP); all-invalid→Default (fail-safe). DISTINCT from node-role relay_mode.
pub fn endpoint_addr_from_parts(node_id:EndpointId, direct:impl IntoIterator<Item=SocketAddr>) -> EndpointAddr;
```

**Config keys** (in `src/config`, all `#[serde(default)]` → old `.lane.yaml` still parses; inert
without the feature): `relay_trusted_nodes: Vec<String>` (deny-by-default NodeId allowlist),
`relay_mode: Option<String>` (`peer`|`relay`; default `peer`; unknown ⇒ `peer`),
`relay_servers: Vec<String>` (DERP/relay-server URLs: empty ⇒ n0 public relays; `["disabled"]` ⇒
relaying off / direct-only; one-or-more URLs ⇒ self-hosted DERP via `RelayMode::Custom`, invalid
entries skipped, all-invalid falls back to Default — `relay_mode_from_config` wires this; availability
not security, so it is fail-safe). `Config::relay_effective_mode() -> String` (the node ROLE, distinct).

CLI: `src/cli/relay.rs` — `lane relay up` (start the governed node; `--json {node_id, listening,
trusted_count}`), `lane relay connect <NodeId>/<host:port> [--local-port N]` (bridge a local port to a
governed remote service), `lane relay trust/untrust <NodeId>` (manage the allowlist, persisted to
config), `lane relay status` (`--json`). The whole family is **always present** (so `lane relay --help`
works in the default build); only the iroh-using `up`/`connect` are feature-gated — without the feature
they fail closed with "rebuild with `--features relay`" (mirrors `lane web` / `lane start --acme`). The
allowlist/identity/config PURE logic is always compiled and tested.

**Hermetic two-node tests** (`#[cfg(feature="relay")]`, in `src/relay/live.rs`): two in-process iroh
`Endpoint`s bound with `RelayMode::Disabled` + **direct addressing** (node B's `EndpointAddr` =
NodeId + its bound loopback `ip_addrs()`, handed to node A's `connect`) — **no real internet/DERP**.
Proofs: (1) two-node reachability — A opens a stream to B and bytes round-trip through a governed bridge
to a local echo; (2) governance-across-the-link — a target denied by B's webpolicy is refused (error
frame, **no upstream connect**, logged) while an allowed target bridges, AND an **untrusted** node's
connection is rejected (deny-by-default).

**Cross-machine validation** (the hardware-dependent NAT-traversal case the hermetic tests cannot
cover): see [`docs/relay-validation.md`](docs/relay-validation.md) — an operator runbook for proving
real two-host relay (direct vs relayed path, governance refusal at the target, deny-by-default trust)
and for pinning self-hosted DERP relays via `relay_servers`.

## src/net  (lane-original — W2; ADR-0003 host network-plane adopt-consume)

lane's **host network plane**: a Rust-native, declarative, **lossless superset of netplan v2** that
lane *adopts* from the live host and (later slices) *consumes* (diff/render/reconcile) so a box is
reproducible from the repo. Always-compiled pure layer + feature-gated effectful reader, mirroring the
`relay`/`web` "pure core always built, live path gated" precedent.

### src/net/model  (P0a — always compiled, always tested)

```rust
pub struct NetworkDocument { pub network: Network }       // mirrors a netplan v2 file
pub struct Network { pub version: u8, pub renderer: Option<Renderer>,
                     pub ethernets/wifis/bridges: BTreeMap<String, _Unit> }   // units keyed by stable netplan unit id
pub enum   Renderer { NetworkManager /* "NetworkManager" */, Networkd /* "networkd" */ }
pub struct EthernetUnit / WifiUnit / BridgeUnit { match, set-name, addresses, routes,
                     dhcp4/6, nameservers, wakeonlan, networkmanager, … }       // common netplan/NM field superset
pub struct MatchRule { name, macaddress }                  // reconciliation identity = match+name, NOT the NM UUID
pub struct NmPassthrough { name, uuid, passthrough: BTreeMap<String,String> }  // lossless escape hatch for arbitrary NM keys
pub struct AccessPoint { key_mgmt, password: Option<SecretRef> }
pub struct SecretRef { pub secretd: String }               // PSK/802.1x is ALWAYS a secretd reference, never inline material
```

A literal secret is **unrepresentable** in the model (only `SecretRef`), so it is always safe to commit.
`BTreeMap` for unit/passthrough maps → semantically-lossless, deterministic serialization, no new dep.

### src/net/adopt  (P0b — pure parser always compiled; `nmcli` reader behind `hostnet`)

```rust
// PURE (no host access) — built & unit-tested in every build:
pub fn parse_nmcli_connection(lines: &[&str], nm_type: &str) -> Option<Unit>;  // terse nmcli text → model unit
pub fn is_secret_property(prop: &str) -> bool;             // *.psk/*.password/*.key/802-1x.*password* → never copied
pub enum UnitKind { Ethernet, Wifi, Bridge }  pub fn UnitKind::from_nm_type(&str) -> Option<UnitKind>;
pub enum Unit { Ethernet{id,unit} | Wifi{id,unit} | Bridge{id,unit} }  // unit + its netplan map key
pub fn Unit::id(&self) -> &str;   pub fn Unit::insert_into(self, &mut NetworkDocument);

// LIVE nmcli reader — #[cfg(feature = "hostnet")] only:
pub struct ConnectionRef { name, nm_type, device }
pub fn list_connections() -> Result<Vec<ConnectionRef>>;  // nmcli -t -f NAME,TYPE,DEVICE connection show
pub fn adopt_connection(name: &str) -> Result<Option<Unit>>;   // nmcli -t -f all connection show <NAME>
pub fn adopt_all() -> Result<NetworkDocument>;            // adopt every host-plane connection
```

**Source = nmcli, NOT `/etc/netplan`.** nmcli reads the full connection config **unprivileged** (no
sudo/human wall) and **secret-safe**: without `--show-secrets` (which lane **never** passes — guarded by
a fixed-args allowlist + `debug_assert!`) nmcli masks every credential slot as `<hidden>`. `/etc/netplan`
is root-only (mode 600) and can carry raw PSK/802.1x material. **Parse rule** (verified on nmcli 1.54):
terse lines are `setting.property:value`; nmcli does **not** escape colons in values (`seen-bssids:AA:29:…`),
so the field name is up to the first colon and the value is the line remainder. **Sanitizing is twofold:**
(1) any `is_secret_property` line is dropped — its value is never read; (2) a `SecretRef` is emitted only
when **`key-mgmt`** actually requires a credential (`sae`/`wpa-psk`→psk, `wpa-eap`→802.1x password) — NOT
inferred from the always-masked value, so OWE/open APs carry **no** ref. **Passthrough** keeps only
affirmative host intent (NM `0`/`0x0`/`false`/`default`/`-1`/`auto` sentinels are dropped) so diffs stay
meaningful; `yes`/`no` normalize to `true`/`false` to match the adopted snapshot shape.

### src/net/apply  (P1 — additive reconcile planner always compiled; `nmcli` apply behind `hostnet`)

```rust
// PURE (no host access) — built & unit-tested in every build:
pub const SECRET_PLACEHOLDER: &str = "<resolved-at-apply>";   // a SecretRef renders to this in the plan, never material
pub enum NmcliOp { Add{con_name,nm_type,ifname,sets} | Modify{con_name,sets} }   // NO delete/flush variant (additive-only)
pub fn NmcliOp::to_argv(&self) -> Vec<String>;   pub fn NmcliOp::con_name(&self) -> &str;   // argv vectors only, no shell
pub struct ReconcilePlan { pub ops: Vec<NmcliOp> }   pub fn is_empty(&self) -> bool;   pub fn render_text(&self) -> String;
pub fn reconcile(desired: &NetworkDocument, current: &NetworkDocument) -> ReconcilePlan;   // the additive diff — heart of P1
pub fn is_runtime_bridge(name: &str) -> bool;    // lo/docker0/virbr*/br-*/veth* excluded from the reconcile-current view

// LIVE nmcli apply — #[cfg(feature = "hostnet")] only:
pub fn apply_plan(plan: &ReconcilePlan) -> Result<()>;   // exec each op via osutil::run_privileged("nmcli", argv), fail-closed
```

**Additive-only (ADR §3, SAFETY-CRITICAL).** For each DESIRED unit, `reconcile` emits an `Add` (no current
match) or a `Modify` of only the differing properties. It **never** emits a delete/flush — `NmcliOp` has no
such variant — so connections present in `current` but absent from `desired` are left completely untouched
(deletion of owned-but-removed units is out of scope for P1). Matching is by the **stable key**
(`networkmanager.name`, else `match.name`), **never** the regenerated NM UUID. **Idempotence:** before
diffing, each unit is projected to a canonical nmcli property map and NM **bookkeeping** passthrough keys
(`ipv4.may-fail`/`ipv6.may-fail`/`ipv4.dhcp-send-hostname-deprecated`/`ipv6.dhcp-send-hostname-deprecated`)
are normalized out, so re-applying an unchanged unit yields an EMPTY plan; semantically-significant keys
(`ipv4.never-default`, `ipv6.method`, addresses, routes, `*.key-mgmt`, dhcp) still diff (lossless bias — a
key is normalized only when justified as an NM default). **Runtime exclusion:** `is_runtime_bridge` keeps the
reconcile from ever planning against Docker/libvirt bridges or `veth` pairs. **Secrets:** a `SecretRef`
renders the credential property as `SECRET_PLACEHOLDER`; the real value is resolved at apply time from
`secretd` (env-ctl) — never in the plan text. `apply_plan` is **fail-closed**: it stops on the first nmcli
error and refuses to execute a plan that still carries an unresolved placeholder.

### src/cli/net  (`lane net adopt` / `lane net apply`)

`lane net adopt [--connection <name>] [--json]` reads the host plane and prints the model (YAML default,
JSON with `--json`) to stdout — **read-only, no host mutation**.

`lane net apply --profile <path> [--dry-run|--apply] [--json]` reads the desired model from `<path>` (a
netplan-v2-superset YAML, as emitted by `adopt`; `--host` profiles are P2), adopts the current host (reusing
`adopt_all`), computes the additive `reconcile` plan, and prints it (`nmcli …` lines, or JSON ops with
`--json`). **`--dry-run` is the default** — `--apply` is the explicit opt-in that executes the plan via
`apply_plan`; the two flags are mutually exclusive so a merged binary never mutates by accident.

The command always parses (`lane net --help` works in the default build); the live read/apply is gated behind
`hostnet` and fails closed without it ("rebuild with `--features hostnet`"), mirroring `lane web`/`lane relay`.

```toml
# Cargo.toml — gates only the live nmcli reader + CLI path; pulls in NO new dependency
# (uses std::process + the already-present serde_yaml), like obscura/relay:
hostnet = []
```

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
`verify_ca_is_trusted()` cfg-gated: linux checks for the installer's anchor file (basename `lane.crt`)
at the `cert::trust::linux_anchor_paths()` locations — the single source of truth shared with the
installer, NOT the CA source basename `rootCA.pem`; darwin `security verify-cert`; else Warn. Cert
expiry via x509-parser; date format `%Y-%m-%d`. `check_daemon`/`check_port_forwarding` call daemon +
system. `check_port_forwarding` maps `PortForwarder::forwarding_status()`: `Present`->Pass,
`Absent`->Fail "not configured", `Unknown`->Warn "cannot verify without root (run: sudo lane doctor)"
(doctor is read-only and must not trigger a sudo prompt). The IPC + health checks are async in our
impl; `run()` may be `async fn run() -> Report` (preferred) — CLI awaits it. Mark in cli.

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
`completions <shell>` (lane-specific, not a Go port): emits a `clap_complete`-generated shell completion
script (bash/zsh/fish/powershell/elvish) to stdout. Synchronous (`completions::run(&args) -> anyhow::Result<()>`,
not awaited); raw script written to `std::io::stdout()`, bypassing `crate::term` like `version --json`.
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
