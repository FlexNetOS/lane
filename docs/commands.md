# lane — Command Reference

`lane` maps clean, trusted HTTPS local domains to your dev-server ports and exposes
local ports on the public internet through `*.lane.show` tunnels. This document is the
complete reference for every `lane` subcommand: synopsis, behavior, flags, arguments,
and examples.

It is a faithful Rust port of the Go tool [`slim`](https://github.com/kamranahmedse/slim),
so command behavior, flag names, error messages, and on-disk layout match `slim` exactly,
with the project renamed to `lane` throughout (binary `lane`, state in `~/.lane`, project
file `.lane.yaml`, tunnel domain `*.lane.show`, internal proxy ports `10080`/`10443`).

## Conventions

- **Domain names** are normalized: a name with no dot gets a `.test` TLD appended
  (`myapp` becomes `myapp.test`). A name that already contains a dot is used verbatim
  (`app.loc`, `my.demo`). Names are also lowercased, trimmed, and stripped of a trailing
  dot before normalization.
- **Domain validation** applies to `start`: each dot-separated label must match
  `^[a-z0-9]([a-z0-9-]*[a-z0-9])?$` (lowercase alphanumeric, hyphens allowed internally),
  the full name must be 253 characters or fewer, and each label 63 or fewer. Ports must be
  in `1–65535`.
- **Durations** (`--ttl`, `--timeout`) accept Go-style values such as `500ms`, `30s`,
  `30m`, `1h`, `2h`.
- **Repeatable flags** such as `--route` may be passed multiple times to accumulate values.
- On any error the CLI prints `Error: <message>` (with a red `Error:` prefix) to stderr and
  exits non-zero.

## Global usage

```
lane <command> [arguments] [flags]
```

The first time a command needs the proxy (for example `lane start`), `lane` runs first-run
setup automatically: it generates a local root CA, adds it to your OS trust store (you may
be prompted for your password once), and installs port-forwarding rules (80→10080,
443→10443). The proxy itself runs as a detached daemon; the CLI communicates with it over a
Unix-domain socket at `~/.lane/lane.sock`.

---

## start

Map a local domain to a port and start proxying it over HTTPS. Runs first-time setup
automatically if needed.

### Synopsis

```
lane start <name> --port <port> [--route <path=port>]... [--log-mode <mode>] [--cors] [--wait [--timeout <duration>]] [--json]
```

### Description

`lane start` registers a domain in `~/.lane/config.yaml`, adds it to `/etc/hosts`, ensures a
leaf TLS certificate exists for it (generating one signed by the local CA if needed), loads
port-forwarding rules, and starts (or reloads) the proxy daemon. Once running,
`https://<name>` is reverse-proxied to `http://localhost:<port>` with full WebSocket/HMR
passthrough.

The name argument is normalized: with no dot it becomes a `.test` domain. Starting a name
that already exists updates its port and routes in place. If the resulting name ends in
`.local`, `lane` prints a warning (`.local` is reserved for mDNS and can cause slow DNS
resolution); the domain is still started.

### Arguments

| Argument | Required | Description |
|---|---|---|
| `<name>` | yes (exactly one) | Domain to serve. Bare names get `.test` appended; names with a dot are used as-is (`app.loc`, `my.demo`). |

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--port` | `-p` | int | _(required)_ | Local upstream port to proxy to. Must be 1–65535. The command fails if omitted. |
| `--route` | | string (repeatable) | _(none)_ | Route a URL path prefix to a different upstream port, in `path=port` form (for example `/api=8080`). May be given multiple times. The path must start with `/`; longest-prefix match wins at request time. |
| `--log-mode` | | string | _(inherits config)_ | Access-log verbosity for the proxy: `full`, `minimal`, or `off`. When omitted, the existing configured mode is left unchanged. |
| `--cors` | | bool | `false` | Enable CORS headers on proxied responses. Only applied when the flag is explicitly set; otherwise the existing config value is preserved. |
| `--wait` | | bool | `false` | Wait for the upstream app to become reachable before the command returns. Each upstream port (the main port plus every route port) is polled until reachable or the timeout elapses. |
| `--timeout` | | duration | `30s` | Maximum time to wait per upstream when `--wait` is set. Passing `--timeout` without `--wait` is an error; the value must be greater than 0. |
| `--json` | | bool | `false` | Emit a JSON object instead of the human output: `{ "domain", "port", "url", "routes"? }`, where `url` is `https://<domain>`. With `--wait`, the progress lines are written to stderr so stdout stays pure JSON. |

### Examples

```bash
# Serve myapp.test from localhost:3000
lane start myapp --port 3000

# Custom TLD, path routing, and CORS, waiting up to 60s for upstreams
lane start app.loc --port 3000 --route /api=8080 --route /ws=9000 --cors --wait --timeout 60s

# Capture the mapped URL in a script
lane start myapp --port 3000 --json
```

On success it prints the mapped URL(s), for example:

```
✓ https://myapp.test  →  localhost:3000
```

---

## stop

Stop proxying a single domain, or stop everything and shut down the daemon.

### Synopsis

```
lane stop [name] [--json]
```

### Description

With a `name`, `lane stop` removes that domain from the config and from `/etc/hosts`. If it
was the last domain, the daemon is shut down (`Stopped <name> (daemon shut down)`);
otherwise the daemon is reloaded (`Stopped <name>`). Stopping a name that is not running
fails with `<name> is not running`.

With no argument, `lane stop` removes all domains from the config and `/etc/hosts` and shuts
down the daemon, printing `Stopped all domains.`. If nothing is configured and the daemon is
not running, it prints `Nothing is running.`.

### Arguments

| Argument | Required | Description |
|---|---|---|
| `[name]` | no (at most one) | Domain to stop. Normalized like `start`. Omit to stop all domains. |

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--json` | | bool | `false` | Emit a JSON object instead of the human output: `{ "stopped": [domain…], "daemon": "shutdown" \| "reloaded" \| "not_running", "warnings"? }`. |

### Examples

```bash
lane stop myapp     # stop one domain
lane stop           # stop everything and shut down the daemon
lane stop --json    # JSON result for scripts
```

---

## up

Start all services defined in a `.lane.yaml` project file.

### Synopsis

```
lane up [--config <path>] [--json]
```

### Description

`lane up` loads a project config and starts every service it declares. By default it
discovers `.lane.yaml` by walking up from the current directory toward the filesystem root;
`--config` overrides discovery with an explicit path. The config is validated (non-empty
service list, valid log mode, valid per-service domain/port, no duplicate domains, valid
routes) before anything runs.

For each service it upserts the domain into `~/.lane/config.yaml`, adds the hosts entry, and
ensures a leaf certificate. Project-level `cors` and `log_mode` are applied to the global
config. It then starts or reloads the daemon and prints the mapped URLs. The path actually
used is echoed as `Using <path>`.

A `.lane.yaml` looks like:

```yaml
services:
  - domain: myapp
    port: 3000
    routes:
      - path: /api
        port: 8080
  - domain: dashboard
    port: 5173
  - domain: app.loc
    port: 4000
log_mode: minimal   # full | minimal | off
cors: true          # enable CORS headers on proxied responses
```

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--config` | `-c` | string | _(auto-discover)_ | Path to a `.lane.yaml` file. When omitted, `lane` searches the current directory and its parents. |
| `--json` | | bool | `false` | Emit a JSON object instead of the human output: `{ "config", "started": [{ "name", "port", "routes"? }] }`. Suppresses the `Using <path>` line and the services table. |

### Examples

```bash
lane up                                # start all services from discovered .lane.yaml
lane up --config /path/to/.lane.yaml   # use an explicit project file
lane up --json                         # JSON for scripts: which services started
```

---

## down

Stop the services defined in a `.lane.yaml` project file, leaving other domains running.

### Synopsis

```
lane down [--config <path>] [--json]
```

### Description

`lane down` discovers (or, with `--config`, loads) a `.lane.yaml`, validates it, then removes
exactly the domains listed in `services` from the global config and from `/etc/hosts`. Any
domains not in the project file are left untouched. If no project domains remain in the
global config afterward the daemon is shut down; otherwise it is reloaded. It prints
`Stopped <N> project service(s).`.

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--config` | `-c` | string | _(auto-discover)_ | Path to a `.lane.yaml` file. When omitted, `lane` searches the current directory and its parents. |
| `--json` | | bool | `false` | Emit a JSON object instead of the human output: `{ "stopped": [domain…], "remaining", "daemon": "stopped" \| "reloaded" \| "not_running", "warnings"? }`. |

### Examples

```bash
lane down                                # stop services from discovered .lane.yaml
lane down --config /path/to/.lane.yaml   # use an explicit project file
lane down --json                         # JSON for scripts: which services stopped
```

---

## list

List all configured domains (with reachability) and active tunnels. Alias: `ls`.

### Synopsis

```
lane list [--json]
lane ls   [--json]
```

### Description

`lane list` prints a table of configured domains with their upstream ports and a live status
column, followed by a table of active `lane.show` tunnels. When the daemon is running, it
ensures port-forwarding is loaded and checks both ingress (ports 80/443 reachable locally)
and per-upstream reachability:

- `● reachable` — upstream port is accepting connections.
- `● unreachable` — upstream port is not responding.
- `● ingress down` — the daemon is running but local ingress (80/443) is not reachable, so
  every domain is reported down.
- `-` — status unknown (daemon not running).

Routes are listed indented beneath their domain, each with its own port and status. Active
tunnels are fetched from the lane API using your saved login token (if any) and shown as
`<subdomain>.lane.show`, the tunnel URL, and a request count. When there is nothing to show
it prints `No domains or tunnels. Use 'lane start' or 'lane share' to create one.`. If
domains are configured but the daemon is not running, it appends
`Proxy is not running. Use 'lane start' to start it.`.

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--json` | | bool | `false` | Emit machine-readable JSON instead of tables. The object has `domains` and `tunnels` arrays; each domain carries `domain`, `port`, optional `healthy` boolean, and a `routes` array (`path`, `port`, optional `healthy`). |

### Examples

```bash
lane list           # human-readable tables
lane ls             # same, via alias
lane list --json    # JSON for scripts and tooling
```

---

## logs

Show the proxy access log, optionally filtered by domain, followed, or cleared.

### Synopsis

```
lane logs [name] [-f | --follow] [--flush] [-n <count> | --lines <count>] [--json]
```

### Description

`lane logs` reads `~/.lane/access.log` and prints each request, colorized by status code.
Both the full and minimal log-line formats are recognized and pretty-printed:

- full: `time  domain  method  path → upstream  status  duration`
- minimal: `time  domain  status  duration`

If no log file exists yet it prints `No logs yet. Start a domain first with 'lane start'.`.
A `name` argument filters output to lines containing that (normalized) domain. With
`--follow`, `lane` seeks to the end of the file and tails new lines (like `tail -f`),
polling every 100ms. With `--flush`, the log file is truncated to empty and the command
prints `Cleared access logs.` (or `No logs to clear.` if the file does not exist).

### Arguments

| Argument | Required | Description |
|---|---|---|
| `[name]` | no (at most one) | Domain to filter log lines by. Normalized like `start`. Not allowed together with `--flush`. |

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--follow` | `-f` | bool | `false` | Continuously tail the log, printing new lines as they arrive. |
| `--flush` | | bool | `false` | Clear (truncate) the access-log file. Cannot be combined with `--follow` (`--flush cannot be used with --follow`) or with a `name` argument (`--flush does not support domain filter`). |
| `--lines` | `-n` | int | _(all)_ | Show only the last `<count>` matching records. |
| `--json` | | bool | `false` | Emit each record as a compact NDJSON object (one per line) instead of the colorized line. Full records carry `ts`, `domain`, `method`, `path`, `upstream`, `status`, `duration`; minimal records carry `ts`, `domain`, `status`, `duration`. Honored in both the tail and `--follow` paths. |

### Examples

```bash
lane logs               # print all access logs
lane logs myapp         # only lines for myapp.test
lane logs -f            # follow new requests live
lane logs -n 50         # last 50 records
lane logs --json        # NDJSON for log processors
lane logs --flush       # clear the access log
```

---

## share

Expose a local port to the internet via a `lane.show` tunnel. Requires `lane login`.

### Synopsis

```
lane share --port <port> [--subdomain <name> | --domain <fqdn>] [--password <secret>] [--ttl <duration>] [--json]
```

### Description

`lane share` opens a secure `wss` tunnel to the lane control plane and bridges public HTTP
requests to `http://localhost:<port>`. It requires authentication; if you are not logged in
it fails with `not logged in — run 'lane login' first`. On connect it prints the public URL
(and, if a custom domain is configured, the domain URL too), the password if one was set, and
streams a colorized log line per request until you press `Ctrl+C` to disconnect.

The tunnel endpoint and API base are configurable via the `LANE_TUNNEL_SERVER` and
`LANE_TUNNEL_SERVER_API` environment variables (defaults `wss://app.lane.sh/tunnel` and
`https://app.lane.sh`). With no `--subdomain` a random subdomain is assigned.

`--subdomain` and `--domain` are mutually exclusive (`cannot use --subdomain and --domain
together`). Vanity subdomains are screened against a protected-brand blocklist; a name that
resembles a protected brand is rejected (`subdomain "..." is not allowed: resembles a
protected brand name`). Custom subdomains, custom domains, and password protection require a
Pro subscription on the hosted service; without one, `lane` prints an explanatory upgrade
notice listing the free vs. Pro options instead of erroring.

> The hosted tunnel/control-plane service is not part of this repository — `lane` ships only
> the client and wire protocol. Point it at a compatible server with the environment
> variables above.

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--port` | `-p` | int | _(required)_ | Local port to expose. Must be 1–65535 (`invalid port <n>: must be between 1 and 65535`). |
| `--subdomain` | | string | _(random)_ | Request a vanity subdomain, yielding `https://<name>.lane.show`. Pro feature; screened against a brand blocklist. Mutually exclusive with `--domain`. |
| `--password` | | string | _(none)_ | Require this password for tunnel access. Pro feature. |
| `--ttl` | | duration | _(none)_ | Tunnel time-to-live, e.g. `30m`, `1h`. Free tier: max 1h. Pro: unlimited. When elapsed, the tunnel disconnects automatically. |
| `--domain` | | string | _(none)_ | Serve the tunnel on a custom domain you have added and verified (see `lane domain`). Pro feature. Mutually exclusive with `--subdomain`. |
| `--json` | | bool | `false` | Emit an NDJSON event stream instead of the human output: one `{ "event": "connected", "url", "port", "local", "domain_url"?, "password"? }` on connect, a `{ "event": "request", "method", "path", "status", "duration" }` per proxied request, and `{ "event": "disconnected" }` on Ctrl+C. The Pro-subscription path emits `{ "event": "error", "error", "upgrade_url" }`. Scripts read the first line to capture the public `url`. |

### Examples

```bash
lane share --port 3000                              # random subdomain
lane share --port 3000 --subdomain demo             # https://demo.lane.show
lane share --port 3000 --password secret            # password protected
lane share --port 3000 --ttl 30m                    # auto-expire after 30 minutes
lane share --port 3000 --domain myapp.example.com   # custom domain
lane share --port 3000 --json                       # NDJSON stream; capture the public URL
```

---

## login

Authenticate with your lane account.

### Synopsis

```
lane login
```

### Description

`lane login` performs a device-style (CLI) OAuth flow against the lane API. If a saved token
is still valid you stay logged in. Otherwise `lane` requests a login code, opens your browser
to the authorization URL (printing the URL if a browser cannot be opened), and polls for
completion for up to 30 seconds. On success the credentials are saved to `~/.lane/auth.json`
(mode 0600) and it prints `Logged in as <name> (<email>)` — or `Already logged in as ...`
when the existing token was reused. The flow times out with `login timed out — please try
again` if not completed in time.

### Arguments

_None._

### Flags

_None._

### Examples

```bash
lane login
```

---

## logout

Log out of your lane account.

### Synopsis

```
lane logout
```

### Description

`lane logout` revokes the saved token server-side (best-effort) and deletes the local
credentials file `~/.lane/auth.json`, then prints `Logged out.`. Running it when already
logged out is a no-op success.

### Arguments

_None._

### Flags

_None._

### Examples

```bash
lane logout
```

---

## domain

Manage custom tunnel domains. Requires `lane login`. Each subcommand calls the lane API with
your saved token.

### Synopsis

```
lane domain add <domain> [--json]
lane domain list [--json]
lane domain verify <domain> [--json]
lane domain remove <domain> [--json]
```

Every `domain` subcommand accepts `--json` to emit a machine-readable object instead of the
human output (shapes documented per subcommand below); the human output is unchanged without
the flag.

### domain add

Register a custom domain with your account.

```
lane domain add <domain>
```

Posts the domain to the API and prints the DNS record you must create to prove ownership: an
`A` record at `<domain>` pointing to the returned target IP. It reminds you to disable any
proxy (for example Cloudflare's orange-cloud) and to allow time for DNS propagation, then
suggests running `lane domain verify <domain>`.

| Argument | Required | Description |
|---|---|---|
| `<domain>` | yes (exactly one) | The fully-qualified custom domain to add (for example `tunnel.example.com`). |

Flags: `--json` — emit `{ "domain", "target_ip", "dns": { "type", "name", "value" } }` (the
verification A record) instead of the human block. A failed add is a hard error (non-zero exit).

```bash
lane domain add tunnel.example.com
lane domain add tunnel.example.com --json
```

### domain list

List the custom domains on your account.

```
lane domain list
```

Fetches and prints a table of domains with `DOMAIN`, `STATUS`, and `ADDED` columns. Status is
one of `● active`, `● generating cert` (issuing the TLS certificate), or `● pending`
(awaiting DNS verification). When you have none it prints `No custom domains. Use 'lane
domain add <domain>' to add one.`.

Arguments: _none._  Flags: `--json` — emit a JSON array of domain objects
(`{ "id", "domain", "status", "created_at" }`); an empty list serializes to `[]`.

```bash
lane domain list
lane domain list --json
```

### domain verify

Check DNS for a custom domain and trigger verification.

```
lane domain verify <domain>
```

Looks up the domain's ID on your account (failing with `domain <domain> not found — use
'lane domain add' first` if absent) and asks the API to verify it. The step result reflects
the outcome: `verified` once active, `issuing certificate (this may take a moment)` while the
cert is being provisioned, or `done` otherwise.

| Argument | Required | Description |
|---|---|---|
| `<domain>` | yes (exactly one) | The custom domain to verify. |

Flags: `--json` — emit `{ "domain", "verified", "status"?, "error"? }`. A domain-level failure
(not found / API / transport) becomes `{ "verified": false, "error" }` and exits 0; consumers
branch on `verified`.

```bash
lane domain verify tunnel.example.com
lane domain verify tunnel.example.com --json
```

### domain remove

Remove a custom domain from your account.

```
lane domain remove <domain>
```

Resolves the domain's ID (failing with `domain <domain> not found` if absent) and deletes it.
If the domain currently has an active tunnel, the API responds with a conflict; `lane` warns
that removing it will disconnect the tunnel and prompts for confirmation before forcing the
removal. On success it prints `✓ Removed <domain>`.

| Argument | Required | Description |
|---|---|---|
| `<domain>` | yes (exactly one) | The custom domain to remove. |

Flags: `--json` — emit `{ "domain", "removed", "error"? }`. `--json` is non-interactive: an
un-forced 409 (active tunnel) becomes `{ "removed": false, "error" }` instead of prompting for
a forced removal (re-run without `--json` to confirm one).

```bash
lane domain remove tunnel.example.com
lane domain remove tunnel.example.com --json
```

---

## doctor

Diagnose setup issues and print a pass/warn/fail checklist.

### Synopsis

```
lane doctor [--json]
```

### Description

`lane doctor` runs a series of diagnostic checks and prints each with an icon (`✓` pass, `!`
warn, `✗` fail). Checks include:

- **CA certificate** — present, parseable, not expired (warns if expiring within 30 days,
  showing the `YYYY-MM-DD` expiry).
- **CA trust** — whether the root CA is trusted by the OS trust store (platform-specific;
  warns on unsupported platforms).
- **Port forwarding** — whether the 80→10080 / 443→10443 rules are configured, loaded, and
  serving ingress; warns or fails with a remediation hint when not.
- **Hosts: `<domain>`** — one per configured domain, confirming the marked `/etc/hosts`
  entry is present.
- **Daemon** — running and answering IPC.
- **Cert: `<domain>`** — one per configured domain, validating the leaf certificate and its
  expiry.

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--json` | | bool | `false` | Emit the report as JSON — `{ "results": [{ "name", "status", "message" }] }`, where `status` is `pass`, `warn`, or `fail` — instead of the human checklist. `doctor` stays read-only and never triggers a sudo prompt. |

### Example

```bash
lane doctor
lane doctor --json   # machine-readable report
```

```
✓  CA certificate        valid, expires 2036-06-02
✓  CA trust              trusted by OS
✓  Port forwarding       active (80→10080, 443→10443)
✓  Hosts: myapp.test     present in /etc/hosts
!  Daemon                not running
✓  Cert: myapp.test      valid, expires 2028-09-05
```

---

## upgrade

Update `lane` to the latest release. Alias: `update`.

### Synopsis

```
lane upgrade
lane update
```

### Description

`lane upgrade` resolves the latest GitHub release tag for the `lane` repository, compares it
to the running version, and stops early with `Already up to date (<version>)` if current.
Otherwise it downloads the platform-specific release archive
(`lane_<version>_<os>_<arch>.tar.gz`), verifies its SHA-256 against the published
`checksums.txt`, extracts the `lane` binary, and atomically replaces the running executable —
falling back to `sudo install` if the target directory is not writable. On success it prints
`Upgraded to <version>`. The OS/arch mapping matches the release artifacts (`macos`→`darwin`,
`x86_64`→`amd64`, `aarch64`→`arm64`).

### Arguments

_None._

### Flags

_None._

### Examples

```bash
lane upgrade
lane update     # alias
```

---

## install

Install an OS service that auto-starts the `lane` daemon at login/boot.

### Synopsis

```
lane install --service [--enable] [--print] [--json]
```

### Description

`lane install --service` writes a **user-level** service definition for this platform — a systemd
user unit (`~/.config/systemd/user/lane.service`) on Linux, or a launchd LaunchAgent
(`~/Library/LaunchAgents/com.lane.daemon.plist`) on macOS. The unit's start command re-execs the
`lane` binary in daemon mode (`_LANE_DAEMON=1`), exactly as the detached daemon does, with
restart-on-failure. No root is required (the daemon elevates per privileged op as usual).

- `--enable` also enables and starts the service now (`systemctl --user enable --now lane.service`
  / `launchctl load <path>`).
- `--print` renders the unit to stdout and writes nothing (useful for review or custom install).
- Without `--service`, the command errors — it is currently the only supported install target.

### Flags

| Flag | Description |
|---|---|
| `--service` | Install the lane daemon service unit (required). |
| `--enable` | Enable and start the service immediately after writing it. |
| `--print` | Print the unit to stdout instead of installing it. |
| `--json` | Emit `{manager, path, written, enabled}` instead of human output. |

### JSON

`--json` prints an object:

```json
{
  "manager": "systemd (user unit)",
  "path": "/home/you/.config/systemd/user/lane.service",
  "written": true,
  "enabled": false
}
```

### Examples

```bash
lane install --service              # write the service unit
lane install --service --enable     # write it and start it now
lane install --service --print      # preview the unit without installing
lane install --service --json       # machine-readable result
```

## uninstall

Remove all `lane` data, configuration, and the binary itself.

### Synopsis

```
lane uninstall
```

### Description

`lane uninstall` removes everything `lane` installed. Because it touches the OS trust store,
port-forwarding rules, and `/etc/hosts`, it re-executes itself with `sudo` (preserving `HOME`)
when not already running as root. It then performs each step, reporting `done` or
`skipped (...)` per step:

1. Stop the daemon (if running).
2. Remove the CA from the OS trust store.
3. Remove the port-forwarding rules.
4. Clean `lane`'s entries from `/etc/hosts`.
5. Remove the `~/.lane/` directory (CA, certs, config, logs, tokens, socket).
6. Remove the `lane` binary.

It finishes with `lane has been completely removed.`.

### Arguments

_None._

### Flags

_None._

### Examples

```bash
lane uninstall
```

---

## version

Print the `lane` version.

### Synopsis

```
lane version [--json]
```

### Description

Prints `lane <version>` and exits. With `--json`, prints a self-describing object instead.

### Arguments

_None._

### Flags

| Flag | Short | Type | Default | Meaning |
|---|---|---|---|---|
| `--json` | | bool | `false` | Emit `{ "name", "version" }` (pretty-printed) instead of the `lane <version>` line. |

### Examples

```bash
lane version
# lane 0.1.0

lane version --json
# { "name": "lane", "version": "0.1.0" }
```

---

## Environment variables

| Variable | Default | Used by | Meaning |
|---|---|---|---|
| `LANE_TUNNEL_SERVER` | `wss://app.lane.sh/tunnel` | `share` | WebSocket endpoint of the tunnel control plane. |
| `LANE_TUNNEL_SERVER_API` | `https://app.lane.sh` | `share`, `login`, `logout`, `list`, `domain` | HTTP API base URL for auth, domains, and active-tunnel queries. |

## Files

| Path | Purpose |
|---|---|
| `~/.lane/config.yaml` | Configured domains, `log_mode`, and `cors`. |
| `~/.lane/access.log` | Proxy access log (read by `logs`). |
| `~/.lane/auth.json` | Saved login credentials (mode 0600). |
| `~/.lane/lane.sock` | Unix-domain socket for CLI↔daemon IPC. |
| `~/.lane/lane.pid` | Daemon PID file. |
| `~/.lane/ca/` | Local root CA certificate and key. |
| `~/.lane/certs/` | Per-domain leaf certificates and keys. |
| `.lane.yaml` | Per-project service definitions (used by `up` / `down`). |
