# Troubleshooting

This guide is built around `lane doctor`, the fastest way to understand the state
of your local setup. Run it first whenever something misbehaves:

```bash
lane doctor
```

A healthy machine looks like this:

```
  ✓  CA certificate        valid, expires 2036-06-02
  ✓  CA trust              trusted by OS
  ✓  Port forwarding       active (80→10080, 443→10443)
  ✓  Hosts: myapp.test     present in /etc/hosts
  ✓  Daemon                running
  ✓  Cert: myapp.test      valid, expires 2028-09-05
```

Each line is one diagnostic check with one of three states:

| Icon | Status | Meaning |
|------|--------|---------|
| `✓`  | Pass   | Working as expected. No action needed. |
| `!`  | Warn   | Not fatal, but `lane` may not behave fully (often a setup step that has not run yet, or something that is configured but not currently active). |
| `✗`  | Fail   | Broken. The associated feature will not work until you fix it. |

`doctor` runs the checks in this fixed order: **CA certificate**, **CA trust**,
**Port forwarding**, one **Hosts** line per configured domain, **Daemon**, then one
**Cert** line per configured domain. The sections below walk through each check, then
cover common issues that `doctor` cannot fully diagnose on its own.

---

## `lane doctor` checks

### CA certificate

Reads `~/.lane/ca/rootCA.pem` and inspects its expiry.

| State | Message | What it means |
|-------|---------|---------------|
| Pass  | `valid, expires YYYY-MM-DD` | The local root CA exists and has more than 30 days of life left. |
| Warn  | `expires soon (YYYY-MM-DD)` | The CA expires in under 30 days. Browsers will keep trusting it until then. |
| Fail  | `not found` | `~/.lane/ca/rootCA.pem` is missing. |
| Fail  | `invalid PEM` / `cannot parse: ...` | The CA file is corrupt. |

**Fixes**

- **`not found`** — the CA has never been generated, or `~/.lane/ca` was deleted.
  Run any `lane start` (for example `lane start myapp --port 3000`); first-run setup
  regenerates the CA automatically. You will be prompted once to trust it.
- **`invalid PEM` / `cannot parse`** — the CA files are damaged. The clean fix is a
  full reset: `lane uninstall`, then `lane start ...` to provision a fresh CA. (`lane
  uninstall` removes `~/.lane`, the trust-store entry, the port-forward rules, and the
  hosts entries; see [Uninstall and cleanup](#uninstall-and-cleanup).)
- **`expires soon`** — regenerate the CA the same way (`lane uninstall` then a fresh
  `lane start`). Note that regenerating the root CA invalidates every previously issued
  leaf certificate, so all domains get new certs on next start.

A 10-year CA lifetime means this check should almost never warn or fail in normal use.

### CA trust

Confirms your OS actually trusts the `lane` root CA. This check is platform-specific.

**Linux** — `doctor` looks for the anchor file `lane.crt` in the system CA directories:

```
/usr/local/share/ca-certificates/lane.crt        (Debian/Ubuntu)
/etc/pki/ca-trust/source/anchors/lane.crt         (RHEL/Fedora)
/etc/ca-certificates/trust-source/anchors/lane.crt (Arch)
```

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `trusted by OS (found in <dir>)` | The anchor is installed in one of the known directories. |
| Fail  | `not found in system CA directories` | No anchor file is present; the CA is untrusted. |

**macOS** — `doctor` runs `security verify-cert -c ~/.lane/ca/rootCA.pem` against the
System keychain:

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `trusted by OS` | The cert verifies against the system trust settings. |
| Fail  | `not trusted by OS` | The cert is not present in (or not trusted by) the System keychain. |

On any other platform this check returns **Warn** (`trust verification not supported on
this platform`) — `lane`'s trust automation only covers macOS and Linux.

**Fixes**

- The trust step normally runs during first-run setup (the "Trusting root CA" step,
  which prompts for your password). If it failed or was skipped, re-running `lane start
  ...` re-attempts it.
- **Linux**: `lane` installs the anchor and then runs `update-ca-certificates` (Debian/
  Ubuntu) or `update-ca-trust extract` (RHEL/Arch) under `sudo`. If the anchor is
  present but the check still fails, the extract step likely did not run — re-run the
  matching command yourself:

  ```bash
  sudo update-ca-certificates       # Debian / Ubuntu
  sudo update-ca-trust extract      # RHEL / Fedora / Arch
  ```

  If `doctor` reports `not found in system CA directories`, neither `update-ca-
  certificates` nor `update-ca-trust` was available when you set up. Install the
  `ca-certificates` package and re-run `lane start`.
- **macOS**: trust is installed via `security add-trusted-cert ... -k /Library/
  Keychains/System.keychain`, which requires admin rights and prompts for your password.
  If the prompt was dismissed, re-run `lane start`. You can confirm the cert manually in
  Keychain Access under **System → Certificates → lane Root CA**.

> A passing CA-trust check is what lets browsers load `https://*.test` with no warning.
> If this check fails, you will see certificate warnings in the browser even though the
> proxy itself is working — see [Browser still warns about the certificate](#browser-still-warns-about-the-certificate).

### Port forwarding

Confirms that traffic to ports 80/443 is being redirected to the proxy's high ports
(`10080`/`10443`). This is the single most common source of "it loads on `:10443` but
not on `:443`" confusion.

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `active (80→10080, 443→10443)` | Redirect rules are configured and loaded. |
| Warn  | `not configured` | Port forwarding has never been set up. |
| Warn  | `configured but inactive (...)` | Rules exist but are not currently loaded, and the daemon is not running. |
| Fail  | `configured but inactive (...)` | Rules exist but are not loaded, and the daemon *is* running (so traffic is actively being dropped). |
| Fail  | `configured but local ingress is down on 80, 443 (...)` | Rules are loaded and the daemon is running, but ports 80/443 do not actually accept connections. |

The `local ingress is down` failure is detected by dialing `127.0.0.1:80` and
`127.0.0.1:443` with a 500 ms timeout; whichever ports fail to connect are listed.

**Platform specifics**

- **Linux** uses `iptables` NAT rules in a dedicated chain named **`LANE`**: a
  `REDIRECT` of `127.0.0.1` `:80 → :10080` and `:443 → :10443`, plus an `OUTPUT` jump
  (`-o lo -p tcp -j LANE`).
- **macOS** uses a `pf` anchor named **`com.lane`** (rules in `/etc/pf.anchors/
  com.lane`, wired into `/etc/pf.conf`), enabled via `pfctl -E`.

**Fixes**

- **`not configured`** — run a `lane start`, which sets up forwarding during first-run
  setup. You will be prompted for your password.
- **`configured but inactive`** — `pf` (macOS) or the rules (Linux) need to be reloaded.
  On macOS the suggested command is included in the message:

  ```bash
  sudo pfctl -e && sudo pfctl -f /etc/pf.conf
  ```

  Starting any domain (`lane start ...`) also reloads forwarding automatically. After a
  reboot on macOS, `pf` may need re-enabling; the next `lane start` handles this.
- **`local ingress is down`** — the redirect targets (`:10080`/`:10443`) are not
  reachable, which almost always means the daemon is not actually listening. Confirm the
  daemon is up (`lane list`), check `~/.lane/daemon.err`, and restart with `lane start`.
  See also [Ports 80/443 not redirecting](#ports-80443-not-redirecting).

To inspect the rules directly:

```bash
# Linux
sudo iptables -t nat -L LANE -n

# macOS
sudo pfctl -a com.lane -s nat
```

### Hosts: `<domain>`

One line per configured domain. Confirms the domain resolves to localhost via a
`lane`-managed entry in `/etc/hosts`. `lane` only recognizes entries it added itself,
marked with a trailing `# lane` comment.

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `present in /etc/hosts` | A `lane`-marked entry for the domain exists. |
| Fail  | `missing from /etc/hosts` | No marked entry found. The domain will not resolve. |
| Fail  | `cannot read /etc/hosts` | `/etc/hosts` is unreadable. |

**Fixes**

- **`missing from /etc/hosts`** — re-run `lane start <domain> --port N`. `lane` writes
  the entry (and removes it on `lane stop`). If you hand-edited `/etc/hosts` and removed
  the `# lane` marker, `lane` no longer "sees" the line — restore the marker or let
  `lane start` re-add a managed entry.
- **`cannot read /etc/hosts`** — a permissions or filesystem problem unrelated to
  `lane`. Verify the file exists and is readable (`ls -l /etc/hosts`).

### Daemon

Checks whether the background proxy daemon is running and answering on its Unix-domain
socket (`~/.lane/lane.sock`).

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `running` | The daemon is up and responded to a status request over IPC. |
| Warn  | `not running` | No daemon is running. This is normal when no domains are active. |
| Fail  | `running but IPC failed` | A socket exists but the daemon did not answer (stale or wedged). |

**Fixes**

- **`not running`** — expected when nothing is started. `lane start <domain> --port N`
  launches the daemon (and `lane stop` shuts it down once the last domain is removed).
- **`running but IPC failed`** — the socket is stale or the process is unhealthy. See
  [Daemon will not start or is wedged](#daemon-will-not-start-or-is-wedged).

### Cert: `<domain>`

One line per configured domain. Inspects the per-domain leaf certificate at
`~/.lane/certs/<domain>.pem`.

| State | Message | Meaning |
|-------|---------|---------|
| Pass  | `valid, expires YYYY-MM-DD` | The leaf cert exists with more than 30 days remaining. |
| Warn  | `expires soon (YYYY-MM-DD)` | Under 30 days remain. `lane` auto-renews leaf certs at next start. |
| Fail  | `not found` | No cert file for this domain. |
| Fail  | `invalid PEM` / `cannot parse` | The cert file is corrupt. |
| Fail  | `expired` | Past its expiry date. |

**Fixes**

- **`not found` / `expired` / `invalid` / `expires soon`** — leaf certs are issued and
  auto-renewed (when under 30 days remain) by `lane start`. Re-running `lane start
  <domain> --port N` regenerates the cert. If a leaf cert was issued before you
  regenerated the root CA, regenerate it the same way.

---

## Common issues

### Browser still warns about the certificate

If `lane doctor` shows **CA trust = Pass** but the browser still shows a warning:

1. **Restart the browser completely.** Browsers cache trust roots at launch; Chrome and
   Firefox in particular will not pick up a newly installed CA until fully restarted
   (quit every window, not just the tab). Firefox uses its **own** trust store on some
   platforms — import `~/.lane/ca/rootCA.pem` via **Settings → Privacy & Security →
   Certificates → View Certificates → Authorities → Import** and tick "Trust this CA to
   identify websites."
2. **Confirm you are hitting `lane`, not a stale cert.** Make sure the URL is
   `https://<domain>` (the trusted domain), not `https://localhost:10443` (which has no
   matching SAN for `localhost` by name and will warn).

If `doctor` shows **CA trust = Warn/Fail**, fix that first — see
[CA trust](#ca-trust). The browser warning is the expected symptom of an untrusted CA.

A quick way to verify the cert independently of any browser:

```bash
echo | openssl s_client -connect myapp.test:443 -servername myapp.test 2>/dev/null \
  | openssl x509 -noout -issuer -subject -dates
```

The issuer should be `CN=lane Root CA, O=lane`.

### Ports 80/443 not redirecting

Symptom: `https://myapp.test:10443` works, but `https://myapp.test` (port 443) times
out or is refused.

1. Run `lane doctor` and read the **Port forwarding** line.
2. If it is **Warn/Fail**, the redirect rules are missing or unloaded. Re-running `lane
   start <domain> --port N` re-applies them; on macOS you can also reload `pf` manually
   with `sudo pfctl -e && sudo pfctl -f /etc/pf.conf`.
3. If it reports **local ingress is down**, the proxy daemon is not listening on
   `:10080`/`:10443`. Check that the daemon is running (`lane list`) and inspect
   `~/.lane/daemon.err`.

**Linux notes**

- `lane` requires `iptables`. If it is missing you will see `iptables not found (install
  iptables)`. Install the `iptables` package.
- Inspect the rules: `sudo iptables -t nat -L LANE -n`. You should see two `REDIRECT`
  targets (to `10080` and `10443`) and an `OUTPUT` jump to the `LANE` chain.
- The redirect only applies to `127.0.0.1`. Accessing the domain from another machine on
  the LAN will not be redirected — that is by design.

**macOS notes**

- Inspect the anchor: `sudo pfctl -a com.lane -s nat`. You should see two `rdr pass`
  rules including `port = 443`.
- After a reboot, `pf` is sometimes disabled or the anchor unloaded. The next `lane
  start` re-enables it (`pfctl -E`); or reload manually with the command above.

### "Port unavailable" on `lane start`

If start aborts with a message like:

```
proxy listener port :10443 is unavailable: ... (another local proxy/old daemon may already be running)
```

`lane` could not bind `:10080` or `:10443`. Something else is holding the port — usually
a stale `lane` daemon, or an unrelated proxy (Caddy, nginx, mkcert-based tooling, a
previous `slim` install, etc.).

1. Check whether a healthy `lane` daemon is already up: `lane list`. If your domains are
   listed and reachable, you may not need to start again.
2. Find the listener:

   ```bash
   # Linux
   sudo ss -ltnp 'sport = :10443'

   # macOS
   sudo lsof -nP -iTCP:10443 -sTCP:LISTEN
   ```

3. If it is a stale `lane` daemon, stop it cleanly with `lane stop`. If the process is
   wedged and `lane stop` does not clear it, kill it by PID (`cat ~/.lane/lane.pid`) and
   remove the stale socket (`rm -f ~/.lane/lane.sock`), then `lane start` again.
4. If a different program owns the port, stop that program or reconfigure it off
   `10080`/`10443`.

### Daemon will not start or is wedged

The proxy runs as a detached daemon; the CLI talks to it over `~/.lane/lane.sock`.

- **`lane start` reports the daemon failed to start.** On startup the CLI waits up to
  ~5 seconds for the daemon to come up, then reports either the captured startup error
  or `daemon failed to start within 5 seconds`. The actual error is written to
  `~/.lane/daemon.err` — read that file first. Common causes are a busy port (see
  [above](#port-unavailable-on-lane-start)) or an unreadable log/config file.
- **`doctor` shows `running but IPC failed`.** A socket file exists but no process is
  answering. Remove the stale socket and restart:

  ```bash
  rm -f ~/.lane/lane.sock
  lane stop          # clears state if anything is half-up
  lane start myapp --port 3000
  ```

- **`doctor` shows `not running` but you expected it up.** Starting a domain launches
  the daemon; `lane stop` (with no arguments, or stopping the last domain) shuts it down.
  This is normal lifecycle behavior, not an error.

### `.local` domains are slow

Avoid `.local` TLDs. `.local` is reserved for multicast DNS (mDNS/Bonjour/Avahi), so the
OS resolver tries mDNS before reading `/etc/hosts`, which adds noticeable latency or
intermittent failures on both macOS and Linux. `lane start` prints a warning if you use
a `.local` name:

```
Warning: .local is reserved for mDNS and may cause slow DNS resolution on macOS/Linux
```

Use `.test` (the default when you omit a TLD), or another non-reserved TLD such as
`.loc` or a custom one:

```bash
lane start myapp --port 3000        # → myapp.test
lane start app.loc --port 3000      # → app.loc
```

### Repeated `sudo` / password prompts

Several operations need elevated privileges and may prompt for your password:

- Installing/removing the CA in the OS trust store (`update-ca-certificates` /
  `update-ca-trust` on Linux, `security add-trusted-cert` on macOS).
- Writing CA anchor files into system directories (via `sudo tee` when the directory is
  not user-writable).
- Installing/loading port-forward rules (`iptables` on Linux, `pf`/`pfctl` on macOS).
- Editing `/etc/hosts`.
- `lane uninstall`, which re-invokes itself under `sudo` to clean system state.

What to expect:

- **First run** prompts once or twice (CA trust, then port forwarding). After that, the
  CA and forwarding persist, so day-to-day `lane start`/`lane stop` should not prompt
  unless forwarding needs reloading (for example after a reboot on macOS).
- Running `lane` as root (or under an active `sudo` session / cached `sudo` timestamp)
  avoids the prompts entirely, since `lane` runs privileged commands directly when
  `euid == 0`.
- If you are prompted on **every** `lane start`, your port-forward rules are probably not
  persisting — check the **Port forwarding** line in `lane doctor` and the platform notes
  in [Ports 80/443 not redirecting](#ports-80443-not-redirecting).

### Public sharing (`lane share`) issues

- **`lane share` says you are not authenticated.** Run `lane login` first; sharing
  requires an account.
- **A feature is rejected as requiring a Pro subscription.** `--subdomain`, `--domain`,
  and `--password` are Pro features, and free `--ttl` is capped at 1 hour. `lane share`
  prints the free vs. Pro option list when it hits this limit. The plain `lane share
  --port N` (random subdomain) and short `--ttl` values work on the free tier.
- **`tunnel connection failed`.** The client could not reach the tunnel server. This
  repository ships only the tunnel client and wire protocol — point it at a compatible
  server with `LANE_TUNNEL_SERVER` (the `wss` endpoint) and `LANE_TUNNEL_SERVER_API`
  (the HTTPS API base) if you are not using the default hosted service.
- **`--subdomain` and `--domain` together** are rejected (`cannot use --subdomain and
  --domain together`) — pick one.

### Uninstall and cleanup

To remove everything `lane` installed — the root CA and its trust-store entry, the
port-forward rules, the `/etc/hosts` entries, `~/.lane`, and the binary:

```bash
lane uninstall
```

`uninstall` re-runs itself under `sudo` (preserving `HOME`) so it can clean system state,
then steps through: stopping the daemon, removing the CA from the trust store, removing
port-forwarding rules, cleaning `/etc/hosts`, removing `~/.lane/`, and removing the `lane`
binary. Each step is best-effort — if one cannot complete it is marked "skipped" rather
than aborting the whole uninstall.

If you need to clean up manually (for example after a failed uninstall):

```bash
# Stop and remove local state
lane stop
rm -rf ~/.lane

# Linux: remove trust anchor + refresh, and tear down iptables
sudo rm -f /usr/local/share/ca-certificates/lane.crt \
           /etc/pki/ca-trust/source/anchors/lane.crt \
           /etc/ca-certificates/trust-source/anchors/lane.crt
sudo update-ca-certificates    # or: sudo update-ca-trust extract
sudo iptables -t nat -D OUTPUT -o lo -p tcp -j LANE
sudo iptables -t nat -F LANE
sudo iptables -t nat -X LANE

# macOS: untrust the CA and remove the pf anchor
sudo security remove-trusted-cert -d ~/.lane/ca/rootCA.pem
sudo rm -f /etc/pf.anchors/com.lane
# then remove the rdr-anchor / load anchor lines for "com.lane" from /etc/pf.conf and reload:
sudo pfctl -f /etc/pf.conf
```

Finally, remove the `/etc/hosts` lines marked with a trailing `# lane` comment.

### Updating

If `lane` itself is out of date or behaving oddly after a release, update to the latest
build:

```bash
lane upgrade        # alias: lane update
```

`upgrade` downloads the matching release archive from GitHub and verifies its SHA-256
checksum before replacing the binary. If the download or checksum check fails, the
existing binary is left untouched.

---

## Quick reference

```bash
lane doctor                 # run all diagnostic checks
lane list                   # show running domains + tunnels (use --json for scripts)
lane logs -f myapp          # tail access logs for a domain
lane logs --flush           # clear the access log
cat ~/.lane/daemon.err      # last daemon startup error
lane stop                   # stop everything and shut the daemon down
lane uninstall              # remove all lane state
```

| Path | What it is |
|------|------------|
| `~/.lane/ca/rootCA.pem` | Local root CA certificate |
| `~/.lane/certs/<domain>.pem` | Per-domain leaf certificate |
| `~/.lane/config.yaml` | Active domain configuration |
| `~/.lane/access.log` | Access log (`lane logs`) |
| `~/.lane/lane.sock` | Daemon IPC socket |
| `~/.lane/lane.pid` | Daemon PID file |
| `~/.lane/daemon.err` | Last daemon startup error |
| `.lane.yaml` | Per-project services file (`lane up` / `lane down`) |
| iptables chain `LANE` (Linux) | NAT redirect rules 80→10080, 443→10443 |
| pf anchor `com.lane` (macOS) | NAT redirect rules 80→10080, 443→10443 |
```
