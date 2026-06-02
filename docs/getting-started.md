# Getting started with lane

`lane` gives your dev servers clean, trusted HTTPS local domains — `https://myapp.test`
instead of `http://localhost:3000` — with full WebSocket passthrough for hot-module
reload, plus one-command public tunnel sharing over `*.lane.show`. It is a faithful Rust
port of the Go tool [`slim`](https://github.com/kamranahmedse/slim), rebuilt on
`tokio` + `hyper` + `rustls`.

This guide takes you from an empty machine to two domains, a path route, live logs, and a
clean shutdown in about five minutes.

```
myapp.test        → localhost:3000
myapp.test/api    → localhost:8080
dashboard.test    → localhost:5173
app.loc           → localhost:4000
```

---

## Prerequisites

- **macOS or Linux.** `lane` configures OS trust and port forwarding on these two
  platforms only (`security`/`update-ca-certificates`/`update-ca-trust` for trust, `pf`
  on macOS and `iptables` on Linux for forwarding). Other platforms run with graceful
  "unsupported" messages but cannot install trust or forwarding.
- **Rust 1.82 or later** — only if you are building from source. The prebuilt installer
  needs no toolchain.
- **`sudo` access.** First run touches the system trust store, `/etc/hosts`, and the
  packet-filter / NAT rules, so `lane` will ask for your password once.
- A running dev server (Next.js, Vite, Rails, an API, anything that binds a local port).

> **Tip:** avoid the `.local` TLD. It is reserved for mDNS and can cause slow DNS
> resolution on macOS and Linux. `lane` warns you if you try to use it. Stick with `.test`
> (the default) or another TLD such as `.loc` or `.demo`.

---

## Install

### Option A — prebuilt binary (curl installer)

Once releases are published, the installer detects your OS/arch, downloads the matching
archive, verifies its SHA-256 checksum, and installs the `lane` binary to
`/usr/local/bin`:

```bash
curl -sL https://lane.sh/install.sh | sh
```

A successful run looks like this:

```
$ curl -sL https://lane.sh/install.sh | sh
Step 1/7: Detecting platform...
Step 2/7: Resolving latest release...
Installing lane 0.1.0 (linux/amd64)...
Step 3/7: Downloading archive...
######################################################################## 100.0%
Step 4/7: Downloading checksums...
Step 5/7: Verifying checksum...
Step 6/7: Extracting archive...
Step 7/7: Installing binary to /usr/local/bin (sudo password may be required)...
Installed lane to /usr/local/bin/lane
lane 0.1.0
```

If `/usr/local/bin` is not writable by your user, the installer prompts for `sudo` at the
final step only.

### Option B — build from source

Building from source requires Rust 1.82+ (`rustc --version` to check; install via
[rustup](https://rustup.rs) if needed).

```bash
git clone https://github.com/lane-sh/lane.git
cd lane
cargo build --release
install -m0755 target/release/lane /usr/local/bin/lane
```

The last step may need `sudo` depending on the permissions on `/usr/local/bin`:

```bash
sudo install -m0755 target/release/lane /usr/local/bin/lane
```

### Verify the install

```bash
$ lane version
lane 0.1.0
```

If `lane: command not found`, confirm `/usr/local/bin` is on your `PATH`.

---

## First run

The first time you start a domain, `lane` runs one-time setup automatically — there is no
separate `init` step. Point it at an already-running dev server:

```bash
$ lane start myapp --port 3000
· Generating root CA done
· Trusting root CA (you may be prompted for your password) done
Setting up port forwarding (80→10080, 443→10443) done
✓ https://myapp.test  →  localhost:3000
```

Here is exactly what happened, in order:

1. **CA generation.** `lane` created a private root certificate authority under
   `~/.lane/ca/` — an RSA-2048 key (`rootCA-key.pem`, mode `0600`) and a 10-year self-signed
   certificate (`rootCA.pem`, mode `0644`) with subject org `lane` and CN `lane Root CA`.
   This CA signs short-lived per-domain leaf certificates so browsers trust
   `https://*.test` with no warnings.

2. **OS trust prompt.** `lane` added the CA to your system trust store. On Linux it writes
   the anchor (`lane.crt`) into the distro's CA directory and runs
   `update-ca-certificates` / `update-ca-trust`; on macOS it runs `security
   add-trusted-cert`. This is the step that **prompts for your password** — it is the only
   interactive step, and it only runs when the CA does not yet exist.

3. **Port forwarding.** Because browsers expect `https://myapp.test` on port 443 (and
   `http` on 80) but the proxy listens on high, unprivileged ports `10080`/`10443`,
   `lane` installs redirect rules: `iptables` NAT chain `LANE` on Linux, or the `pf`
   anchor `com.lane` on macOS. This maps `80 → 10080` and `443 → 10443`, so no process has
   to hold privileged ports long-term.

4. **Leaf certificate.** `lane` generated an ECDSA-P256 leaf cert for `myapp.test`
   (valid 825 days, auto-renewed under 30 days remaining), signed by your CA, with SANs for
   the domain plus `127.0.0.1` and `::1`. It lives in `~/.lane/certs/`.

5. **Hosts entry.** `myapp.test` was added to `/etc/hosts`, pointing at loopback and tagged
   with a `# lane` marker so `lane` can manage and later remove its own entries cleanly.

6. **Daemon launch.** The proxy started as a detached background daemon (re-exec'd with
   `setsid`). The CLI talks to it over a Unix-domain socket at `~/.lane/lane.sock`. The
   daemon binds `:10080` (which 301-redirects to HTTPS) and `:10443` (TLS via `rustls`,
   HTTP/1.1 + HTTP/2), picking the right certificate per request by SNI.

Everything `lane` writes lives under `~/.lane/`:

```
~/.lane/
├── ca/
│   ├── rootCA.pem
│   └── rootCA-key.pem
├── certs/
│   ├── myapp.test.pem
│   └── myapp.test-key.pem
├── config.yaml
├── access.log
├── lane.sock
└── lane.pid
```

Subsequent `lane start` invocations skip CA generation, trust, and port forwarding — they
are already in place — so they return almost instantly and never prompt for a password.

> **Headless / scripted hosts:** if port forwarding cannot be installed (no `iptables`/`pf`,
> or restricted environment), the setup step reports `skipped (...)` and continues rather
> than failing. You can still reach the proxy directly on `:10443`, and `lane doctor` will
> flag forwarding as not configured.

---

## Verify it works

### In the browser

Open **https://myapp.test**. You should see your dev server with a valid padlock and **no
certificate warning** — the page is served over real HTTPS, trusted by the locally
installed CA. WebSocket connections (e.g. Vite/Next HMR) pass straight through, so live
reload keeps working.

If your dev server isn't actually listening on port 3000 yet, `lane` serves a friendly
"upstream is down" page instead of a connection error — start your app and refresh.

### With `lane doctor`

`lane doctor` runs a pass / warn / fail checklist across every moving part of the setup:

```bash
$ lane doctor
  ✓  CA certificate        valid, expires 2036-06-02
  ✓  CA trust              trusted by OS
  ✓  Port forwarding       active (80→10080, 443→10443)
  ✓  Hosts: myapp.test    present in /etc/hosts
  ✓  Daemon                running
  ✓  Cert: myapp.test     valid, expires 2028-09-05
```

What each line means:

| Check | Healthy | Common warnings / failures |
|---|---|---|
| **CA certificate** | present and not near expiry | `not found`, `expired`, `expires soon (...)` |
| **CA trust** | installed in the OS trust store | not trusted → re-run a `lane start`, or trust manually |
| **Port forwarding** | NAT/pf rules active for 80→10080, 443→10443 | `not configured`, `configured but inactive (...)` |
| **Hosts: \<domain\>** | `# lane` entry present in `/etc/hosts` | `missing from /etc/hosts` |
| **Daemon** | running and answering IPC | `not running` (warn), `running but IPC failed` (fail) |
| **Cert: \<domain\>** | leaf cert valid for the domain | `not found`, `expired`, `expires soon (...)` |

A `!` (warn) is yellow and a `✗` (fail) is red. The `Daemon: not running` warning is
expected and harmless after `lane stop` — it just means nothing is being proxied right now.

---

## A 5-minute tour

This walkthrough assumes you have dev servers running on a few ports. Adjust the port
numbers to match your own apps.

### 1. Start two domains

```bash
$ lane start myapp --port 3000
✓ https://myapp.test  →  localhost:3000

$ lane start dashboard --port 5173
✓ https://dashboard.test  →  localhost:5173
```

The second `start` reuses the existing CA, trust, and forwarding, then reloads the running
daemon over the socket — no password prompt, no restart. To use a different TLD, pass a
full domain:

```bash
$ lane start app.loc --port 4000
✓ https://app.loc  →  localhost:4000
```

### 2. Add a path route

Route specific URL paths to different upstream ports on the same domain. Here `myapp.test`
serves the frontend on 3000, but `myapp.test/api` proxies to an API on 8080 and
`myapp.test/ws` to a websocket service on 9000. Re-running `start` on an existing domain
updates it in place:

```bash
$ lane start myapp --port 3000 --route /api=8080 --route /ws=9000
✓ https://myapp.test      →  localhost:3000
  https://myapp.test/api  →  localhost:8080
  https://myapp.test/ws   →  localhost:9000
```

Routing uses longest-prefix matching: a request to `/api/users` goes to 8080, while `/`
and everything else go to 3000.

> **Project files:** instead of repeating flags, declare every service for a project in a
> `.lane.yaml` at its root, then run `lane up` to start them all and `lane down` to stop
> them. See the [README](../README.md#local-usage) for the schema.

### 3. List what's running

```bash
$ lane list
DOMAIN              PORT  STATUS
myapp.test          3000  ● reachable
  /api              8080  ● reachable
  /ws               9000  ● unreachable
dashboard.test      5173  ● reachable
app.loc             4000  ● reachable
```

`● reachable` means the daemon can open a TCP connection to that upstream port right now;
`● unreachable` means nothing is listening there yet (start that service). `lane ls` is an
alias for `lane list`. For machine-readable output, add `--json`:

```bash
$ lane list --json
{
  "domains": [
    {
      "domain": "myapp.test",
      "port": 3000,
      "healthy": true,
      "routes": [
        { "path": "/api", "port": 8080, "healthy": true },
        { "path": "/ws", "port": 9000, "healthy": false }
      ]
    },
    {
      "domain": "dashboard.test",
      "port": 5173,
      "healthy": true
    },
    {
      "domain": "app.loc",
      "port": 4000,
      "healthy": true
    }
  ],
  "tunnels": []
}
```

If you have an active public tunnel (after `lane login` + `lane share`), it appears in a
second table and in the `tunnels` array.

### 4. Tail the logs

Browse to your domains, then watch requests stream in. `-f` (or `--follow`) tails like
`tail -f`; pass a domain name to filter:

```bash
$ lane logs -f myapp
14:22:01 myapp.test GET / → 3000 200 12.4ms
14:22:01 myapp.test GET /assets/index.js → 3000 200 3.1ms
14:22:02 myapp.test GET /api/users → 8080 200 41.2ms
14:22:05 myapp.test GET /api/missing → 8080 404 8.7ms
14:22:09 myapp.test GET /ws → 9000 502 1.0ms
```

Status codes are color-coded (2xx green, 3xx cyan, 4xx yellow, 5xx red). Run `lane logs`
with no flags to dump the log and exit. Use `--flush` to clear the access log file (it
cannot be combined with `--follow` or a domain filter):

```bash
$ lane logs --flush
Cleared access logs.
```

The log format is controlled by `--log-mode full|minimal|off` on `start`, or `log_mode` in
`.lane.yaml`. `full` (the default) includes method, path, and upstream; `minimal` shows
just domain, status, and duration.

### 5. Stop everything

Stop a single domain — its `/etc/hosts` entry is removed and the daemon reloads:

```bash
$ lane stop dashboard
Stopped dashboard.test
```

Stop everything — this removes all hosts entries and shuts the daemon down:

```bash
$ lane stop
Stopped all domains.
```

If you stop the last remaining domain individually, `lane` shuts the daemon down for you:

```bash
$ lane stop myapp
Stopped myapp.test (daemon shut down)
```

Stopping does **not** uninstall the CA, trust, or port-forward rules — those stay in place
so your next `lane start` is instant. To remove absolutely everything `lane` installed
(CA trust, certs, `/etc/hosts` entries, port-forward rules, and `~/.lane`), run:

```bash
lane uninstall
```

---

## Next steps

- **Share a port publicly:** `lane login`, then `lane share --port 3000` for a random
  `*.lane.show` URL. Add `--subdomain`, `--password`, `--ttl`, or `--domain` to customize.
- **Manage custom tunnel domains:** `lane domain add|list|verify|remove`.
- **Keep current:** `lane upgrade` (alias `lane update`) self-updates from the latest
  GitHub release with checksum verification.
- **Diagnose anything:** `lane doctor` is the first thing to run when something misbehaves.

See [`../README.md`](../README.md) for the full command reference and
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) for the design and module map.
