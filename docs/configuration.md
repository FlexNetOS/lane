# Configuration Reference

`lane` is configured through two YAML files and a handful of environment
variables:

| File | Scope | Written by | Schema root key |
|---|---|---|---|
| `.lane.yaml` | A single project, checked into the repo | You (by hand) | `services` |
| `~/.lane/config.yaml` | The machine-global proxy state | `lane` (managed) | `domains` |

The project file (`.lane.yaml`) describes what *you* want to run; `lane up`
reads it and merges its services into the global config. The global config
(`~/.lane/config.yaml`) is the live state that the proxy daemon serves — you
rarely edit it directly, since `lane start`, `lane stop`, `lane up`, and
`lane down` keep it in sync for you.

> The two files use deliberately different key names. The project file lists
> `services` with a `domain:` field; the global config lists `domains` with a
> `name:` field. Do not copy one schema into the other.

---

## The project file: `.lane.yaml`

Place a `.lane.yaml` at the root of a project to declare every domain it needs.
`lane up` walks up from the current directory to the filesystem root looking for
this file (or takes an explicit path via `lane up --config /path/to/.lane.yaml`),
validates it, and starts all of its services at once. `lane down` stops them
again.

### Full schema

```yaml
services:                 # required, must contain at least one entry
  - domain: myapp         # required; bare label -> myapp.test
    port: 3000            # required; the upstream localhost port (1-65535)
    routes:               # optional; per-path port overrides on this domain
      - path: /api        # must start with "/"
        port: 8080        # 1-65535
      - path: /ws
        port: 9000
  - domain: dashboard
    port: 5173
  - domain: app.loc       # any TLD is honored verbatim
    port: 4000

log_mode: minimal         # optional; full | minimal | off  (default: full)
cors: true                # optional; default: false
```

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `services` | list | yes | — | Must contain at least one service. |
| `services[].domain` | string | yes | — | A bare label (no dot) becomes `<label>.test`. A name containing a dot is used as-is. Validated per the [domain naming rules](#domain-naming-rules). |
| `services[].port` | integer | yes | — | Upstream port on `localhost`, `1`–`65535`. |
| `services[].routes` | list | no | none | Path-based port overrides; longest matching prefix wins, otherwise traffic goes to `services[].port`. |
| `services[].routes[].path` | string | yes (within a route) | — | Must begin with `/`. |
| `services[].routes[].port` | integer | yes (within a route) | — | `1`–`65535`. |
| `log_mode` | string | no | `full` | One of `full`, `minimal`, `off`. Case-insensitive; surrounding whitespace is trimmed. |
| `cors` | boolean | no | `false` | When `true`, CORS headers are added to proxied responses and preflight `OPTIONS` requests are answered. |

### Validation rules

`lane up` rejects a project file that does not satisfy all of the following,
reporting the first failure:

- `services` is non-empty (`no .lane.yaml found` is reported separately when the
  file itself is missing).
- Every `domain` is a valid domain name and every `port` is in `1`–`65535`.
- Domain names are unique across services — a duplicate raises
  `duplicate domain "<name>"`.
- Every route `path` starts with `/` and every route `port` is in range.
- `log_mode`, if set, is one of `full`, `minimal`, `off`.

### How `lane up` applies it

Running `lane up` merges the project's services into the global config rather
than replacing it:

- The project's `cors` value overwrites the global `cors` setting.
- A non-empty project `log_mode` overwrites the global `log_mode`.
- Each service is upserted into `domains` by name: an existing domain has its
  `port` and `routes` replaced; a new domain is appended.

This means domains you started ad hoc with `lane start` survive a `lane up`, and
`lane down` only stops the services named in the current `.lane.yaml`.

### Routes and path matching

A `routes` list lets one domain front several upstreams by URL path:

```yaml
services:
  - domain: myapp
    port: 3000           # everything not matched below
    routes:
      - path: /api       # myapp.test/api, myapp.test/api/...
        port: 8080
      - path: /ws        # myapp.test/ws  (WebSocket upgrades pass through)
        port: 9000
```

Matching uses the longest route prefix whose `path` is either an exact match or
a proper path-segment prefix of the request path (so `/api` matches `/api` and
`/api/users`, but not `/apidocs`). Requests that match no route are proxied to
the domain's base `port`. The same routing is available on the command line via
repeatable `--route` flags: `lane start myapp --port 3000 --route /api=8080
--route /ws=9000`.

---

## The global config: `~/.lane/config.yaml`

This is the authoritative state the proxy daemon serves. It is created and
updated for you by `lane start`/`stop`/`up`/`down`; the file is written with
mode `0644` and updated under an exclusive file lock (`~/.lane/config.lock`) so
concurrent commands don't corrupt it. When the daemon receives a reload it
re-reads this file and rebuilds its routers and certificates.

### Schema

```yaml
domains:                  # the live set of proxied domains
  - name: myapp.test      # fully-normalized domain name
    port: 3000
    routes:               # optional, omitted when empty
      - path: /api
        port: 8080
  - name: dashboard.test
    port: 5173
log_mode: minimal         # full | minimal | off  (omitted when empty == full)
cors: true                # omitted when false
```

| Field | Type | Notes |
|---|---|---|
| `domains` | list | The active proxied domains. |
| `domains[].name` | string | Stored fully normalized (a bare label is rewritten to `<label>.test`). |
| `domains[].port` | integer | Upstream port. |
| `domains[].routes` | list | Same `path`/`port` shape as the project file; omitted from the file when empty. |
| `log_mode` | string | `full`, `minimal`, or `off`. Omitted from the file when it would be the default (`full`). An empty/absent value is treated as `full`. |
| `cors` | boolean | Omitted from the file when `false`. |

### Self-healing migration

On load, `lane` normalizes any unqualified `domains[].name` (adding `.test`) and
rewrites the file if anything changed. A missing config file is treated as an
empty config, not an error — the first `lane start` materializes it.

> Editing this file by hand works but is fragile: the next `lane start` /
> `lane up` will rewrite it. Prefer the CLI, or capture intent in a project
> `.lane.yaml`.

---

## Log modes

`log_mode` (in either file) controls the access log written to
`~/.lane/access.log` and surfaced by `lane logs`:

| Mode | Behavior | Line format (tab-separated) |
|---|---|---|
| `full` (default) | Every request, with method, path, and upstream. | `HH:MM:SS  domain  method  path  upstream  status  duration` |
| `minimal` | Every request, condensed. | `HH:MM:SS  domain  status  duration` |
| `off` | No access logging; the writer is disabled. | — |

The value is case-insensitive and trimmed; an empty value means `full`. The log
file is appended to and rotated when it exceeds 10 MB.

---

## The `~/.lane` directory

Everything `lane` persists lives under `~/.lane` (resolved from the user's home
directory). A populated tree looks like this:

```
~/.lane/
├── ca/
│   ├── rootCA.pem          # RSA-2048 root CA certificate          (0644)
│   └── rootCA-key.pem      # root CA private key                   (0600)
├── certs/
│   ├── myapp.test.pem      # per-domain ECDSA leaf certificate     (0644)
│   ├── myapp.test-key.pem  # per-domain leaf private key           (0600)
│   └── ...                 # one cert/key pair per domain
├── config.yaml             # global proxy config (see above)       (0644)
├── config.lock             # advisory flock for config writes      (0644)
├── access.log              # request access log (rotated at 10 MB)
├── lane.sock               # Unix-domain socket for CLI <-> daemon IPC
├── lane.pid                # daemon process id
├── tunnel-token            # locally-generated tunnel client token (0600)
├── auth.json               # login session: token, name, email     (0600)
├── pf.token                # macOS pf reference token (macOS only)  (0600)
└── daemon.err              # daemon startup stderr (created on failure)
```

| Path | Purpose |
|---|---|
| `ca/rootCA.pem`, `ca/rootCA-key.pem` | The locally-generated root CA. The certificate is installed into your OS trust store; the key signs every leaf certificate. The `ca/` directory is created with mode `0700`. |
| `certs/<domain>.pem`, `certs/<domain>-key.pem` | Per-domain leaf certificate and key, selected by SNI at TLS handshake time. The `certs/` directory is created with mode `0700`. |
| `config.yaml` | The global config described above. |
| `config.lock` | Empty lock file; `lane` holds an exclusive `flock` on it while mutating `config.yaml`. |
| `access.log` | The proxy access log. Read with `lane logs`, tail with `lane logs -f`, clear with `lane logs --flush`. |
| `lane.sock` | The daemon's IPC socket. The CLI sends small JSON messages (`status`, `reload`, `shutdown`) over it. |
| `lane.pid` | The running daemon's PID, written at startup. |
| `tunnel-token` | A random hex token generated on first `lane share`, identifying this client to the tunnel server. |
| `auth.json` | Your `lane login` session: `{ "token": ..., "name": ..., "email": ... }`. Created by `lane login`, removed by `lane logout`. |
| `pf.token` | macOS only: the `pfctl -E` reference token recorded when port forwarding is enabled, so it can be disabled cleanly. Not present on Linux. |
| `daemon.err` | Captures the daemon's stderr if it fails to start; surfaced when `lane` cannot reach a freshly launched daemon. |

`lane uninstall` removes this entire directory along with the CA trust, the
`/etc/hosts` entries `lane` added, and the port-forwarding rules.

---

## Environment variables

`lane` reads a small set of environment variables. The two tunnel variables let
you point the client at a self-hosted or alternative control plane (the hosted
service is not part of this repository — `lane` ships only the client and wire
protocol).

| Variable | Default | Purpose |
|---|---|---|
| `LANE_TUNNEL_SERVER` | `wss://app.lane.sh/tunnel` | WebSocket (`wss://`) endpoint the `lane share` client connects to. |
| `LANE_TUNNEL_SERVER_API` | `https://app.lane.sh` | HTTPS API base used for `lane login`, `lane logout`, and `lane domain` operations. |

Example — share through a self-hosted tunnel server:

```bash
export LANE_TUNNEL_SERVER=wss://tunnel.example.com/tunnel
export LANE_TUNNEL_SERVER_API=https://tunnel.example.com
lane login
lane share --port 3000
```

These variables only affect the public-sharing features. Local HTTPS proxying
(`lane start`/`up`) never contacts the network.

---

## Domain naming rules

Domain names are validated identically by `lane start`, the global config, and
the project file.

- **Default TLD.** A name with no dot gets `.test` appended: `myapp` becomes
  `myapp.test`. A name containing a dot is used verbatim, so any TLD works —
  `app.loc`, `my.demo`, `api.internal`.
- **Length.** The full name must be 253 characters or fewer; each
  dot-separated label must be 63 characters or fewer.
- **Label syntax.** Each label must match `^[a-z0-9]([a-z0-9-]*[a-z0-9])?$`:
  lowercase letters, digits, and internal hyphens only — no leading or trailing
  hyphen, no uppercase, no underscores.
- **Port.** The associated port must be in `1`–`65535`.

Invalid input produces a precise error, e.g.
`invalid domain name "My_App": labels must be lowercase alphanumeric with
hyphens` or `domain label "<63+ chars>" is too long: must be 63 characters or
fewer`.

> **Avoid `.local`.** It is reserved for mDNS (Bonjour/Avahi) and can cause slow
> or failed DNS resolution on macOS and Linux. `lane start <name>.local` still
> works but prints a warning; prefer `.test` (the default) or another
> non-reserved TLD.

---

## Certificates

`lane` runs its own certificate authority so browsers trust `https://*.test`
(and any other domain you map) with no warnings. The relevant files live under
`~/.lane/ca` and `~/.lane/certs`.

### Root CA

- **Key:** RSA-2048, stored at `~/.lane/ca/rootCA-key.pem` (mode `0600`).
- **Subject:** Organization `lane`, Common Name `lane Root CA`.
- **Validity:** 10 years from generation.
- **Constraints:** marked as a CA with a path-length constraint of 0 (it may
  only sign leaf certificates, not intermediates), with key usages
  `certSign` and `crlSign`.
- **Trust:** the certificate (`~/.lane/ca/rootCA.pem`, mode `0644`) is installed
  into the OS trust store on first run — via `update-ca-certificates` /
  `update-ca-trust` on Linux, or `security add-trusted-cert` on macOS. You may
  be prompted for your password once.

### Leaf certificates

One leaf certificate is generated per domain and selected at handshake time by
SNI:

- **Key:** ECDSA P-256, stored at `~/.lane/certs/<domain>-key.pem` (mode `0600`).
- **Subject / SAN:** Common Name and DNS SAN set to the domain name; IP SANs
  `127.0.0.1` and `::1` are included so connections by loopback address also
  validate.
- **Validity:** 825 days (kept under Apple's leaf-lifetime limit).
- **Usage:** `digitalSignature` + `keyEncipherment` key usage and the
  `serverAuth` extended key usage.
- **Signing:** signed by the root CA above.

### Automatic renewal

Whenever a domain is started, `lane` calls `ensure_leaf_cert`, which regenerates
the leaf certificate if any of the following is true:

- the certificate or its key file is missing,
- the file cannot be parsed,
- the key is not ECDSA (e.g. left over from an older format), or
- the certificate has fewer than **30 days** of validity remaining.

Otherwise the existing certificate is reused. You can inspect certificate health
at any time with `lane doctor`, which reports the CA's expiry, whether it is
trusted by the OS, and each leaf certificate's expiry date.
