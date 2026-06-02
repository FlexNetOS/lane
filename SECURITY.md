# Security Policy

`lane` gives your dev servers trusted local HTTPS by installing a locally-generated
root certificate authority (CA) into your operating system's trust store, and it
performs a handful of privileged operations to wire up `/etc/hosts`, the OS trust
store, and port forwarding. This document explains exactly what `lane` installs on
your machine, the trust model and blast radius, how to remove everything, and how to
report a vulnerability.

It is written to be precise rather than alarming. The design mirrors that of the
upstream tool [`slim`](https://github.com/kamranahmedse/slim) and of comparable local
HTTPS tooling such as `mkcert`: a single per-machine root CA, with private keys that
never leave your computer.

## What `lane` installs

Everything `lane` creates lives under `~/.lane/`, plus a small number of entries in
system files. On first run (`lane start`, `lane up`) it provisions the items below.

### Local certificate authority

- **CA private key** — `~/.lane/ca/rootCA-key.pem`, an **RSA-2048** key, written with
  file mode **`0600`** (owner read/write only). The directory `~/.lane/ca` is created
  with mode `0700`.
- **CA certificate** — `~/.lane/ca/rootCA.pem`, a self-signed root certificate
  (subject `CN=lane Root CA`, `O=lane`), valid for **10 years**, marked `IsCA` with a
  path-length constraint of `0` and key usage limited to `keyCertSign | crlSign`. It
  is written `0644` because the public certificate is, by design, public.

The CA private key is generated locally with a cryptographically secure RNG and
**never leaves the machine**. `lane` does not transmit it anywhere, does not back it
up, and has no remote escrow.

### Per-domain leaf certificates

For each domain you run (for example `myapp.test`), `lane` mints a short-lived leaf
certificate signed by your local CA:

- **Leaf key** — `~/.lane/certs/{name}-key.pem`, an **ECDSA P-256** key, mode `0600`.
- **Leaf certificate** — `~/.lane/certs/{name}.pem`, mode `0644`, valid for **825 days**
  with SANs `DNS={name}`, `IP=127.0.0.1`, `IP=::1`, extended key usage `serverAuth`.
  Leaf certs are auto-renewed when fewer than **30 days** remain.

Because the leaf certs only carry `127.0.0.1`/`::1` SANs and serve a domain that
resolves to localhost via `/etc/hosts`, they are not useful against any host other
than your own loopback.

### Tunnel credentials

- **Account token** — `lane login` performs a device-style OAuth flow against the lane
  API and stores the returned bearer token, name, and email in `~/.lane/auth.json`
  with mode **`0600`**. This token authenticates `lane share` and the
  `lane domain ...` commands.
- **Tunnel token** — `~/.lane/tunnel-token`, a randomly generated 32-byte hex token
  (mode `0600`) used by the tunnel client. It is created on demand.

### System-level changes (require privilege)

- **`/etc/hosts`** — one `127.0.0.1 <domain> # lane` line per active domain. Every
  line `lane` adds carries the trailing `# lane` marker so removal is exact and
  scoped; `lane` never touches lines it did not add.
- **OS trust store** — the CA certificate is copied to the platform's trust anchor
  directory and the trust database is rebuilt (see below).
- **Port forwarding** — firewall rules redirect ports 80 and 443 to `lane`'s
  unprivileged high ports `10080` and `10443`.

The proxy itself listens only on `:10080` and `:10443`. **No `lane` process binds a
privileged port (80/443) for the lifetime of the daemon**; the kernel firewall does
the redirect instead, so the long-running daemon runs unprivileged.

## Trust model and blast radius

When you trust `lane`'s CA, your browsers and tools will accept **any** certificate
signed by `~/.lane/ca/rootCA-key.pem` as valid. That is precisely what makes
`https://myapp.test` work without warnings.

The security boundary is therefore the **CA private key file**. The blast radius is:

- **Anyone who can read `~/.lane/ca/rootCA-key.pem` can mint certificates your machine
  will trust for any hostname** — `github.com`, your bank, anything. With that key and
  a position to intercept your traffic (a shared machine, a backup that leaked, malware
  running as your user), they could impersonate arbitrary HTTPS sites to *you*.
- The risk is local. The key never touches the network, and the issued leaf certs only
  cover loopback addresses, so a stolen CA key does not by itself let an attacker
  impersonate sites to anyone other than the owner of the machine where that CA is
  trusted.

Practical guidance:

- **Guard `~/.lane/`.** Keep its `0600`/`0700` permissions. Do not loosen them, do not
  commit it to a repo, and do not copy `rootCA-key.pem` to another host or to shared
  storage. Treat it like an SSH private key.
- **Exclude `~/.lane/ca` and `~/.lane/auth.json` from backups and sync tools** unless
  the backup is itself encrypted and access-controlled. A CA key in an unencrypted
  cloud backup is a CA key on someone else's infrastructure.
- **Do not run `lane` as root** for normal use. Run it as your user; it will request
  elevation (via `sudo`) only for the specific privileged steps described below.
- **One CA per machine.** Do not share a generated CA across machines or users.
- If you suspect the CA key was exposed, **rotate immediately**: run `lane uninstall`
  (or `lane doctor` + remove trust), then `lane start` again to generate a fresh CA and
  re-trust it. The old CA cert can also be manually removed from the OS trust store.

## Privileged operations and why they are needed

`lane` runs unprivileged by default. It shells out to `sudo` only for the operations
that genuinely require it, and only for the duration of that one command. You may be
prompted for your password on first run and when system state changes.

| Operation | Why privilege is required | Mechanism |
|---|---|---|
| Add the CA to the OS trust store | Trust anchor directories and the trust DB are root-owned | Linux: write the cert to a system anchor dir (with a `sudo tee` fallback) then `update-ca-certificates` / `update-ca-trust extract`. macOS: `security add-trusted-cert ... /Library/Keychains/System.keychain` |
| Edit `/etc/hosts` | `/etc/hosts` is root-owned | Direct write if permitted, otherwise `sudo tee` |
| Set up port forwarding (80/443 → 10080/10443) | Manipulating the kernel packet filter needs root | Linux: `iptables` NAT chain `LANE` with `REDIRECT` rules and an `OUTPUT -o lo` jump. macOS: a `pf` anchor `com.lane` at `/etc/pf.anchors/com.lane`, enabled via `pfctl` |
| `lane uninstall` | Reverses all of the above | Re-execs itself under `sudo --preserve-env=HOME` |

What `lane` deliberately does **not** do:

- It does not bind privileged ports for any sustained period — the daemon listens on
  `10080`/`10443` and relies on the firewall redirect, so the long-lived process is
  unprivileged.
- It does not run a privileged background service. The detached proxy daemon runs as
  your user and is reached over a Unix-domain socket at `~/.lane/lane.sock`.
- It does not modify trust for anything beyond its own single CA certificate, and every
  `/etc/hosts` line it manages is tagged `# lane`.

Only on Linux and macOS are these operations supported; on other platforms `lane`
reports the step as unsupported rather than performing it.

## Tunnel authentication and `*.lane.show`

`lane share` exposes a local port over a public `wss` tunnel.

- Authentication uses the **bearer token** in `~/.lane/auth.json` (mode `0600`),
  obtained via `lane login`. The token is sent as `Authorization: Bearer <token>` to
  the lane API and in the tunnel registration frame.
- `lane logout` removes `~/.lane/auth.json` and makes a best-effort call to revoke the
  token server-side.
- Tunnel endpoints are configurable with the `LANE_TUNNEL_SERVER` and
  `LANE_TUNNEL_SERVER_API` environment variables. The hosted control plane and the
  `*.lane.show` domain are **not** part of this repository; this codebase ships only the
  tunnel client and wire protocol.
- A tunnel is public for as long as it is connected. Use `--password` to require a
  shared password and `--ttl` (e.g. `--ttl 30m`) to auto-expire the tunnel. Sharing a
  port exposes that local service to the internet for the session's duration — treat the
  URL as a public endpoint and do not tunnel services holding sensitive data without a
  password.

If a `*.lane.show` tunnel is being used for phishing, malware, or other abuse, report
it to the operator of the tunnel service you are pointed at.

## Removing trust completely

To remove the trust relationship and all on-disk state:

```bash
lane uninstall
```

`lane uninstall` re-execs under `sudo` and, as discrete best-effort steps:

1. **Stops the daemon** (via the IPC socket).
2. **Removes the CA from the OS trust store** — deletes the anchor file and rebuilds the
   trust DB (`update-ca-certificates` / `update-ca-trust extract`, or
   `security remove-trusted-cert` on macOS).
3. **Removes the port-forwarding rules** — flushes and deletes the `iptables` chain
   `LANE` (Linux) or the `pf` anchor `com.lane` (macOS).
4. **Cleans `/etc/hosts`** — removes every line carrying the `# lane` marker.
5. **Deletes `~/.lane/`** — including the CA key, all leaf certs, and credentials.
6. **Removes the `lane` binary.**

To audit the current state without removing anything, run:

```bash
lane doctor
```

`lane doctor` reports pass/warn/fail for the CA certificate, whether the CA is trusted
by the OS, port forwarding, each domain's `/etc/hosts` entry, the daemon, and each leaf
certificate. If you want to drop only the trust relationship while keeping `~/.lane`,
you can remove the CA anchor from your OS trust store manually:

- **Debian/Ubuntu:** delete `/usr/local/share/ca-certificates/lane.crt`, then
  `sudo update-ca-certificates --fresh`.
- **RHEL/Fedora:** delete `/etc/pki/ca-trust/source/anchors/lane.crt`, then
  `sudo update-ca-trust extract`.
- **Arch:** delete `/etc/ca-certificates/trust-source/anchors/lane.crt`, then
  `sudo update-ca-trust extract`.
- **macOS:** `sudo security remove-trusted-cert -d ~/.lane/ca/rootCA.pem`, or remove
  the `lane Root CA` entry from the System keychain in Keychain Access.

## Updating securely

`lane upgrade` (alias `lane update`) downloads a signed release archive from GitHub and
**verifies its SHA-256 checksum** against the published `checksums.txt` before
replacing the running binary. Builds are reproducible from source with
`cargo build --release`; if you prefer, build and install the binary yourself rather
than using the install script.

## Supported versions

Security fixes are applied to the latest released version. We recommend always running
the most recent release and keeping `lane` current with `lane upgrade`.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Previous minor releases | Best-effort, fix shipped in the next release |
| Pre-1.0 / development builds | Not separately supported — upgrade to the latest |

## Reporting a vulnerability

Please report security issues **privately** and **do not open a public GitHub issue**
for an unfixed vulnerability.

- **Preferred:** open a private advisory via GitHub Security Advisories on the
  repository (`Security` → `Report a vulnerability`).
- **Email:** `revenaugh.david@gmail.com` with a clear description, affected version
  (`lane version`), platform, and reproduction steps.

When reporting, please include:

- The version (`lane version`) and OS/architecture.
- A description of the issue and its impact (e.g. CA key exposure, privilege escalation,
  `/etc/hosts` injection, tunnel auth bypass).
- Reproduction steps or a proof-of-concept, and any logs (with secrets redacted —
  never include the contents of `rootCA-key.pem`, `auth.json`, or `tunnel-token`).

What to expect:

- **Acknowledgement** within a few business days.
- A coordinated fix and, where appropriate, a published advisory crediting the reporter
  (unless you prefer to remain anonymous).
- Please allow reasonable time for a fix before any public disclosure. We will keep you
  updated on progress.

Thank you for helping keep `lane` users safe.
