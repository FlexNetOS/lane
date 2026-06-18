//! `lane net adopt` — the live host reader (ADR-0003 §Decision item 2, "Adopter").
//!
//! Reads the host's existing network configuration and emits the Rust-native,
//! lossless [`super::model`] (a superset of netplan v2). The adopt is **read-only
//! and sanitizing** — it never mutates the host and never copies secret material.
//!
//! # Why nmcli, not `/etc/netplan`
//!
//! The estate renders with NetworkManager, and `nmcli` reads the full connection
//! config **unprivileged** and **secret-safe**: without `--show-secrets` (which
//! lane MUST NEVER pass) nmcli already prints every PSK/802.1x/WEP field as the
//! literal token `<hidden>`. By contrast `/etc/netplan/*.yaml` is root-only
//! (mode 600) and can carry raw PSK/802.1x material. Sourcing from nmcli keeps
//! `lane net adopt` runnable without `sudo` (no human password wall) and is
//! sanitizing by construction. lane *additionally* applies its own secret filter
//! (see [`is_secret_property`]) as defense in depth, so even a value that somehow
//! is not `<hidden>` is replaced by a [`SecretRef`] placeholder, never copied.
//!
//! # Layering
//!
//! - [`parse_nmcli_connection`] is **pure** (no host access) and built/tested in
//!   every build: it turns the captured terse text of one nmcli connection into a
//!   model [`Unit`]. It is the unit-testable core (feed it fixture lines).
//! - The thin `nmcli`-spawning wrappers ([`adopt_all`], [`adopt_connection`],
//!   [`list_connections`]) are gated behind the `hostnet` cargo feature, mirroring
//!   how [`crate::relay`]/`web` gate only their effectful paths.

use crate::net::model::{
    AccessPoint, BridgeUnit, EthernetUnit, MatchRule, NetworkDocument, NmPassthrough, Renderer,
    SecretRef, WifiUnit,
};

/// The netplan unit kind a connection maps to, derived from its `connection.type`.
///
/// netplan groups units into `ethernets:` / `wifis:` / `bridges:` blocks; the
/// nmcli `connection.type` (`802-3-ethernet`, `802-11-wireless`, `bridge`, …)
/// selects which block a connection is adopted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// `802-3-ethernet` → netplan `ethernets:`.
    Ethernet,
    /// `802-11-wireless` → netplan `wifis:`.
    Wifi,
    /// `bridge` → netplan `bridges:`.
    Bridge,
}

impl UnitKind {
    /// Map an nmcli `connection.type` string to a [`UnitKind`].
    ///
    /// Returns `None` for types lane's host plane does not adopt (loopback,
    /// container/VM-runtime devices, …) so the caller can skip them.
    pub fn from_nm_type(nm_type: &str) -> Option<UnitKind> {
        match nm_type {
            "802-3-ethernet" => Some(UnitKind::Ethernet),
            "802-11-wireless" => Some(UnitKind::Wifi),
            "bridge" => Some(UnitKind::Bridge),
            _ => None,
        }
    }
}

/// A single adopted unit: the model unit plus the stable netplan unit id it is
/// keyed by in the `ethernets:`/`wifis:`/`bridges:` map.
///
/// Per ADR §3 the reconciliation identity is `match`+name, **not** the
/// regenerated NM UUID; the netplan unit id (`NM-<uuid>`) is just the map key.
#[derive(Debug, Clone, PartialEq)]
pub enum Unit {
    /// An `ethernets:` unit.
    Ethernet {
        /// netplan unit id (map key, e.g. `"NM-<uuid>"`).
        id: String,
        /// The adopted ethernet unit.
        unit: Box<EthernetUnit>,
    },
    /// A `wifis:` unit.
    Wifi {
        /// netplan unit id (map key).
        id: String,
        /// The adopted Wi-Fi unit.
        unit: Box<WifiUnit>,
    },
    /// A `bridges:` unit.
    Bridge {
        /// netplan unit id (map key).
        id: String,
        /// The adopted bridge unit.
        unit: Box<BridgeUnit>,
    },
}

impl Unit {
    /// The netplan unit id this unit is keyed by.
    pub fn id(&self) -> &str {
        match self {
            Unit::Ethernet { id, .. } | Unit::Wifi { id, .. } | Unit::Bridge { id, .. } => id,
        }
    }

    /// Insert this unit into the matching block of a [`NetworkDocument`].
    pub fn insert_into(self, doc: &mut NetworkDocument) {
        match self {
            Unit::Ethernet { id, unit } => {
                doc.network.ethernets.insert(id, *unit);
            }
            Unit::Wifi { id, unit } => {
                doc.network.wifis.insert(id, *unit);
            }
            Unit::Bridge { id, unit } => {
                doc.network.bridges.insert(id, *unit);
            }
        }
    }
}

/// Report whether an nmcli property name names secret material that must NEVER be
/// copied into the adopted model.
///
/// Matches the credential-bearing NM property families: PSKs (`*.psk`), passwords
/// (`*.password`, including 802.1x `*-password`), private keys / WEP keys
/// (`*.key`, `802-11-wireless-security.wep-key0`…), and any `802-1x.*password*`
/// EAP credential. When this returns true the property's value is replaced by a
/// [`SecretRef`] placeholder; the real value is resolved at render time from
/// `secretd` (env-ctl), never committed.
pub fn is_secret_property(prop: &str) -> bool {
    let p = prop.to_ascii_lowercase();
    // 802.1x EAP credentials: password, private-key, ca-cert-password, etc.
    if p.starts_with("802-1x.") && (p.contains("password") || p.contains("private-key")) {
        return true;
    }
    // Generic credential suffixes across any setting (e.g. wifi-security.psk,
    // wifi-security.wep-key0, *.password, vpn.secrets, gsm.password).
    p.ends_with(".psk")
        || p.ends_with(".password")
        || p.ends_with("-password")
        || p.ends_with(".key")
        || p.contains(".wep-key")
        || p.ends_with(".secrets")
}

/// The canonical secret property a wifi `key-mgmt` requires, or `None` when the
/// key management needs no credential.
///
/// nmcli masks EVERY wifi secret slot as the token `<hidden>` regardless of
/// whether a credential is actually set (verified on nmcli 1.54: an `owe` AP
/// still reports `psk:<hidden>` and `wep-key0:<hidden>`), so the *value* is not a
/// reliable presence signal. `key-mgmt` IS: `owe`/`none`/empty are credential-
/// free; `sae`/`wpa-psk` use a PSK; `wpa-eap`/`802.1x` use the 802.1x password.
/// This drives whether a [`SecretRef`] is emitted and which property it names —
/// so OWE/open estate APs (the snapshot case) carry NO secret ref, while a real
/// PSK/EAP network references the right `secretd` key.
fn key_mgmt_secret_property(key_mgmt: Option<&str>) -> Option<&'static str> {
    match key_mgmt {
        Some("wpa-psk" | "sae") => Some("802-11-wireless-security.psk"),
        Some("wpa-eap" | "ieee8021x" | "wpa-eap-suite-b-192") => Some("802-1x.password"),
        // owe / none / open / unset → no credential.
        _ => None,
    }
}

/// Whether a terse nmcli value counts as "absent" — empty, or a sentinel NM uses
/// for an unset typed property (`-1`, `default`, `auto`, `0`, `unknown`). These
/// carry no host intent and would only add noise to the lossless passthrough, so
/// they are dropped on adoption (a re-adopt of the rendered model is stable).
fn is_blank_nm_value(value: &str) -> bool {
    matches!(value, "" | "-1" | "default" | "auto" | "unknown")
}

/// Whether a *normalized* value is an NM "off"/default sentinel that carries no
/// affirmative host intent — so it is dropped from the passthrough escape hatch
/// to keep the model (and every diff) to genuinely-set configuration only.
///
/// This is what keeps the adopted `cognitum-seed` unit's passthrough to the
/// snapshot's `{ ipv4.never-default: true, ipv6.method: link-local }` instead of
/// the ~30 lines of NM `0`/`0x0`/`false` bookkeeping defaults nmcli emits. A
/// `true`/non-zero/`link-local`-style value is affirmative intent and is kept.
fn is_default_nm_value(value: &str) -> bool {
    matches!(value, "false" | "0" | "0x0" | "none")
}

/// Normalize an nmcli boolean (`yes`/`no`) to the canonical `true`/`false` that
/// the adopted snapshot uses (e.g. `ipv4.never-default: "true"`). Non-boolean
/// values pass through unchanged.
fn normalize_nm_value(value: &str) -> String {
    match value {
        "yes" => "true".to_string(),
        "no" => "false".to_string(),
        other => other.to_string(),
    }
}

/// Parse one terse nmcli line (`setting.property:value`) into `(prop, value)`.
///
/// nmcli's `-t` output uses the FIRST colon as the field separator and does **not**
/// escape colons that appear inside the value (verified on nmcli 1.54: e.g.
/// `802-11-wireless.seen-bssids:AA:29:48:…`), so the value is the remainder of the
/// line verbatim. Lines without a colon are ignored.
fn split_nmcli_line(line: &str) -> Option<(&str, &str)> {
    line.split_once(':')
}

/// Parse the terse `nmcli -t -f all connection show <NAME>` output of ONE
/// connection into a model [`Unit`].
///
/// `lines` are the raw terse lines (`setting.property:value`); `nm_type` is the
/// connection's `connection.type` (selecting ethernet/wifi/bridge). This function
/// is **pure** — no host access — so it is unit-testable from captured fixtures.
///
/// Mapping:
/// - `connection.id` → [`NmPassthrough::name`] + (for ethernet/wifi) the
///   [`MatchRule::name`] fallback and the netplan unit id source.
/// - `connection.uuid` → [`NmPassthrough::uuid`] and the `NM-<uuid>` unit id.
/// - `connection.interface-name` → [`MatchRule::name`].
/// - `802-3-ethernet.mac-address` / `802-11-wireless.mac-address` →
///   [`MatchRule::macaddress`].
/// - `ipv4.addresses` (comma-separated) → [`EthernetUnit::addresses`].
/// - `ipv4.method == "auto"` → `dhcp4 = true`; `ipv6.method == "auto"|"dhcp"` →
///   `dhcp6 = true`.
/// - `*.wake-on-lan` (a non-`default`/`disable` magic mode) → `wakeonlan = true`.
/// - `802-11-wireless.ssid` + `802-11-wireless-security.key-mgmt` → an
///   [`AccessPoint`]; any matched secret property → [`AccessPoint::password`] as a
///   [`SecretRef`], never the value.
/// - `bridge.interfaces` is not exposed per-connection by nmcli's terse view, so
///   bridge member interfaces are left empty here (P1 renderer territory).
/// - **every other** non-blank `ipvX.*`, `802-3-ethernet.*`, `802-11-wireless*.*`
///   property that has no structured slot → [`NmPassthrough::passthrough`]
///   (the lossless escape hatch), with `yes`/`no` normalized to `true`/`false`.
///
/// Secret sanitizing is enforced here: a property for which [`is_secret_property`]
/// is true never contributes its value to passthrough — it is dropped (its
/// credential surfaces only via the [`SecretRef`] access-point password).
pub fn parse_nmcli_connection(lines: &[&str], nm_type: &str) -> Option<Unit> {
    let kind = UnitKind::from_nm_type(nm_type)?;

    // First pass: collect the raw (prop, value) pairs, sanitizing secrets out.
    let mut id: Option<String> = None;
    let mut uuid: Option<String> = None;
    let mut iface: Option<String> = None;
    let mut mac: Option<String> = None;
    let mut addresses: Vec<String> = Vec::new();
    let mut dhcp4: Option<bool> = None;
    let mut dhcp6: Option<bool> = None;
    let mut wakeonlan: Option<bool> = None;
    let mut ssid: Option<String> = None;
    let mut key_mgmt: Option<String> = None;
    let mut passthrough = std::collections::BTreeMap::<String, String>::new();

    for line in lines {
        let Some((prop, value)) = split_nmcli_line(line) else {
            continue;
        };

        // Sanitize (defense in depth): a secret property's VALUE is NEVER read or
        // copied into the model — drop the line entirely. Whether a credential
        // reference is emitted is decided separately from `key-mgmt` (nmcli masks
        // every secret slot as "<hidden>" regardless of presence, so the value is
        // not a reliable signal — see `key_mgmt_secret_property`).
        if is_secret_property(prop) {
            continue;
        }

        match prop {
            "connection.id" if !value.is_empty() => id = Some(value.to_string()),
            "connection.uuid" if !value.is_empty() => uuid = Some(value.to_string()),
            "connection.interface-name" if !value.is_empty() => iface = Some(value.to_string()),
            "802-3-ethernet.mac-address" | "802-11-wireless.mac-address" if !value.is_empty() => {
                mac = Some(value.to_string())
            }
            "ipv4.addresses" if !value.is_empty() => {
                addresses.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );
            }
            "ipv4.method" => match value {
                "auto" => dhcp4 = Some(true),
                "manual" | "link-local" | "disabled" | "shared" => dhcp4 = Some(false),
                _ => {}
            },
            "ipv6.method" => {
                match value {
                    "auto" | "dhcp" => dhcp6 = Some(true),
                    "manual" | "link-local" | "ignore" | "disabled" | "shared" => {
                        dhcp6 = Some(false)
                    }
                    _ => {}
                }
                // ipv6.method also carries host intent (e.g. "link-local"); keep
                // it verbatim in passthrough so it is lossless adopt→render.
                if !is_blank_nm_value(value) {
                    passthrough.insert(prop.to_string(), value.to_string());
                }
            }
            "802-11-wireless.ssid" if !value.is_empty() => ssid = Some(value.to_string()),
            "802-11-wireless-security.key-mgmt" if !value.is_empty() => {
                key_mgmt = Some(value.to_string())
            }
            "802-3-ethernet.wake-on-lan" | "802-11-wireless.wake-on-wlan" => {
                // NM magic-mode strings/bitmasks; "default"/"0"/"disable" mean off.
                if !is_blank_nm_value(value) && value != "disable" && value != "0x0" {
                    wakeonlan = Some(true);
                }
            }
            _ => {
                // Lossless escape hatch: any remaining ipvX/ethernet/wifi property
                // with no structured slot is preserved verbatim (yes/no normalized
                // to true/false to match the snapshot shape). NM "unset" sentinels
                // and "off"/default values are dropped so the passthrough carries
                // only affirmative host intent (the snapshot's clean shape).
                if is_passthrough_property(prop) && !is_blank_nm_value(value) {
                    let normalized = normalize_nm_value(value);
                    if !is_default_nm_value(&normalized) {
                        passthrough.insert(prop.to_string(), normalized);
                    }
                }
            }
        }
    }

    // The netplan unit id: stable "NM-<uuid>" per the snapshot (falls back to the
    // connection id, then the interface name, so a unit is always keyed).
    let unit_id = uuid
        .as_ref()
        .map(|u| format!("NM-{u}"))
        .or_else(|| id.clone())
        .or_else(|| iface.clone())
        .unwrap_or_else(|| "NM-unknown".to_string());

    let match_rule = build_match_rule(iface.clone(), mac.clone());
    let networkmanager = build_nm_passthrough(id.clone(), uuid.clone(), passthrough);

    match kind {
        UnitKind::Ethernet => {
            let unit = EthernetUnit {
                renderer: Some(Renderer::NetworkManager),
                match_rule,
                addresses,
                dhcp4,
                dhcp6,
                wakeonlan,
                networkmanager,
                ..EthernetUnit::default()
            };
            Some(Unit::Ethernet {
                id: unit_id,
                unit: Box::new(unit),
            })
        }
        UnitKind::Wifi => {
            let mut access_points = std::collections::BTreeMap::new();
            if let Some(ssid) = ssid {
                // Emit a SecretRef ONLY when key-mgmt actually requires a
                // credential (sae/wpa-psk → psk, wpa-eap → 802.1x password). The
                // value is NEVER copied — only a `secretd` reference. OWE/open APs
                // (the estate's case) need no credential → password stays None.
                let password = key_mgmt_secret_property(key_mgmt.as_deref()).map(|prop| {
                    let conn = id.clone().unwrap_or_else(|| ssid.clone());
                    SecretRef::new(format!("{conn}/{prop}"))
                });
                access_points.insert(ssid, AccessPoint { key_mgmt, password });
            }
            let unit = WifiUnit {
                renderer: Some(Renderer::NetworkManager),
                match_rule,
                addresses,
                dhcp4,
                dhcp6,
                wakeonlan,
                networkmanager,
                access_points,
                ..WifiUnit::default()
            };
            Some(Unit::Wifi {
                id: unit_id,
                unit: Box::new(unit),
            })
        }
        UnitKind::Bridge => {
            let unit = BridgeUnit {
                renderer: Some(Renderer::NetworkManager),
                addresses,
                dhcp4,
                dhcp6,
                networkmanager,
                ..BridgeUnit::default()
            };
            Some(Unit::Bridge {
                id: unit_id,
                unit: Box::new(unit),
            })
        }
    }
}

/// Build the [`MatchRule`] from the device interface name and/or MAC. Returns
/// `None` when neither is known (an unbound connection).
fn build_match_rule(name: Option<String>, macaddress: Option<String>) -> Option<MatchRule> {
    if name.is_none() && macaddress.is_none() {
        return None;
    }
    Some(MatchRule { name, macaddress })
}

/// Build the [`NmPassthrough`] block, or `None` when there is nothing to carry.
fn build_nm_passthrough(
    name: Option<String>,
    uuid: Option<String>,
    passthrough: std::collections::BTreeMap<String, String>,
) -> Option<NmPassthrough> {
    if name.is_none() && uuid.is_none() && passthrough.is_empty() {
        return None;
    }
    Some(NmPassthrough {
        name,
        uuid,
        passthrough,
    })
}

/// Whether a property belongs in the lossless NM passthrough map: the addressing
/// and link settings families (`ipv4.*`, `ipv6.*`, `802-3-ethernet.*`,
/// `802-11-wireless.*`, `802-11-wireless-security.*`). The voluminous
/// `connection.*`/`proxy.*` bookkeeping is intentionally excluded — it carries no
/// portable host intent and would bloat every diff.
fn is_passthrough_property(prop: &str) -> bool {
    prop.starts_with("ipv4.")
        || prop.starts_with("ipv6.")
        || prop.starts_with("802-3-ethernet.")
        || prop.starts_with("802-11-wireless.")
        || prop.starts_with("802-11-wireless-security.")
}

// --- live nmcli reader (feature-gated) -------------------------------------

/// A connection's identity from `nmcli connection show` (list view).
#[cfg(feature = "hostnet")]
#[derive(Debug, Clone)]
pub struct ConnectionRef {
    /// `connection.id` (e.g. `"cognitum-seed-linklocal"`).
    pub name: String,
    /// `connection.type` (e.g. `"802-3-ethernet"`).
    pub nm_type: String,
    /// Bound device, if any (e.g. `"enxead865c61ec9"`).
    pub device: String,
}

/// List the host's NetworkManager connections (id, type, device) via
/// `nmcli -t -f NAME,TYPE,DEVICE connection show`.
#[cfg(feature = "hostnet")]
pub fn list_connections() -> anyhow::Result<Vec<ConnectionRef>> {
    let out = run_nmcli(&["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show"])?;
    let mut refs = Vec::new();
    for line in out.lines() {
        // NAME may itself contain a colon; nmcli does not escape it, but the last
        // two fields (TYPE, DEVICE) are colon-free tokens, so split from the right.
        let mut parts = line.rsplitn(3, ':');
        let device = parts.next().unwrap_or("").to_string();
        let nm_type = parts.next().unwrap_or("").to_string();
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        refs.push(ConnectionRef {
            name,
            nm_type,
            device,
        });
    }
    Ok(refs)
}

/// Adopt every host connection lane's plane owns into a [`NetworkDocument`].
///
/// Skips connection types lane does not adopt (loopback, container/VM bridges are
/// adopted as bridges only if explicitly typed `bridge`; runtime `veth`/`tun` are
/// not NM connections). Read-only and sanitizing.
#[cfg(feature = "hostnet")]
pub fn adopt_all() -> anyhow::Result<NetworkDocument> {
    let mut doc = NetworkDocument::new(crate::net::model::Network {
        renderer: Some(Renderer::NetworkManager),
        ..crate::net::model::Network::v2()
    });
    for c in list_connections()? {
        if UnitKind::from_nm_type(&c.nm_type).is_none() {
            continue;
        }
        if let Some(unit) = adopt_connection(&c.name)? {
            unit.insert_into(&mut doc);
        }
    }
    Ok(doc)
}

/// Adopt a single connection by name into a [`Unit`], or `None` if its type is
/// not one lane adopts. Reads `nmcli -t -f all connection show <NAME>` — **never**
/// `--show-secrets`.
#[cfg(feature = "hostnet")]
pub fn adopt_connection(name: &str) -> anyhow::Result<Option<Unit>> {
    let out = run_nmcli(&["-t", "-f", "all", "connection", "show", name])?;
    let lines: Vec<&str> = out.lines().collect();
    let nm_type = lines
        .iter()
        .find_map(|l| split_nmcli_line(l).filter(|(p, _)| *p == "connection.type"))
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();
    Ok(parse_nmcli_connection(&lines, &nm_type))
}

/// Run `nmcli` with the given args and return its stdout as a UTF-8 string.
///
/// `--show-secrets` is **never** passed (the args are a fixed allowlist supplied
/// by this module). A non-zero exit surfaces nmcli's stderr as the error.
#[cfg(feature = "hostnet")]
fn run_nmcli(args: &[&str]) -> anyhow::Result<String> {
    use anyhow::Context;

    debug_assert!(
        !args.contains(&"--show-secrets"),
        "lane net adopt must never request secret material"
    );

    if !crate::osutil::command_exists("nmcli") {
        anyhow::bail!(
            "`nmcli` not found on PATH — `lane net adopt` adopts the host plane via \
             NetworkManager; install NetworkManager or run on a NM-managed host"
        );
    }

    let output = std::process::Command::new("nmcli")
        .args(args)
        .output()
        .context("spawning nmcli")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nmcli {:?} failed: {}", args, stderr.trim());
    }
    String::from_utf8(output.stdout).context("nmcli output was not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured `nmcli -t -f all connection show cognitum-seed-linklocal` lines
    /// from the adoption-target host (drdave-TRX50-AI-TOP, 2026-06-17). Trimmed to
    /// the load-bearing properties; the full output has ~124 lines of NM
    /// bookkeeping that adopt intentionally drops.
    const COGNITUM_SEED_LINES: &[&str] = &[
        "connection.id:cognitum-seed-linklocal",
        "connection.uuid:70b82336-d3cd-4204-90aa-fe8a1ed5e769",
        "connection.type:802-3-ethernet",
        "connection.interface-name:enxead865c61ec9",
        "connection.autoconnect:yes",
        "802-3-ethernet.mac-address:",
        "802-3-ethernet.wake-on-lan:default",
        "ipv4.method:manual",
        "ipv4.addresses:169.254.42.2/24",
        "ipv4.gateway:",
        "ipv4.never-default:yes",
        "ipv4.route-table:0",    // NM default sentinel → dropped (noise)
        "ipv4.dns-priority:0",   // NM default sentinel → dropped (noise)
        "ipv6.never-default:no", // NM "off"/false → dropped (noise)
        "ipv6.method:link-local",
        "ipv6.addr-gen-mode:default",
        "proxy.method:none",
    ];

    #[test]
    fn cognitum_seed_round_trips_to_snapshot_shape() {
        let unit =
            parse_nmcli_connection(COGNITUM_SEED_LINES, "802-3-ethernet").expect("ethernet unit");

        // Keyed by the stable netplan unit id NM-<uuid>.
        assert_eq!(unit.id(), "NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769");

        let mut doc = NetworkDocument::new(crate::net::model::Network {
            renderer: Some(Renderer::NetworkManager),
            ..crate::net::model::Network::v2()
        });
        unit.insert_into(&mut doc);

        let eth = doc
            .network
            .ethernets
            .get("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769")
            .expect("ethernet unit present");

        // Snapshot shape (docs/adopt/host-nm-snapshot-2026-06-17.md):
        //   match.name: enxead865c61ec9
        //   addresses: [169.254.42.2/24]
        //   networkmanager.name: cognitum-seed-linklocal
        //   passthrough carries never-default + ipv6 link-local
        assert_eq!(eth.renderer, Some(Renderer::NetworkManager));
        assert_eq!(
            eth.match_rule.as_ref().and_then(|m| m.name.as_deref()),
            Some("enxead865c61ec9")
        );
        assert_eq!(eth.addresses, vec!["169.254.42.2/24".to_string()]);
        assert_eq!(eth.dhcp4, Some(false)); // ipv4.method == manual

        let nm = eth.networkmanager.as_ref().expect("networkmanager block");
        assert_eq!(nm.name.as_deref(), Some("cognitum-seed-linklocal"));
        assert_eq!(
            nm.uuid.as_deref(),
            Some("70b82336-d3cd-4204-90aa-fe8a1ed5e769")
        );
        assert_eq!(
            nm.passthrough.get("ipv4.never-default").map(String::as_str),
            Some("true"),
            "yes → true normalization, lossless never-default"
        );
        assert_eq!(
            nm.passthrough.get("ipv6.method").map(String::as_str),
            Some("link-local"),
            "ipv6 link-local mode carried in passthrough"
        );
        // The passthrough is exactly the snapshot's clean shape — affirmative host
        // intent only, with the NM `0`/`false`/`default` bookkeeping dropped.
        assert_eq!(
            nm.passthrough.len(),
            2,
            "passthrough must carry only never-default + ipv6.method, got {:?}",
            nm.passthrough
        );
        // NM `0`/`false`/`default` bookkeeping must NOT pollute the passthrough.
        assert!(!nm.passthrough.contains_key("ipv4.route-table"));
        assert!(!nm.passthrough.contains_key("ipv4.dns-priority"));
        assert!(!nm.passthrough.contains_key("ipv6.never-default"));

        // And it serializes to YAML and round-trips equal (lossless).
        let yaml = serde_yaml::to_string(&doc).unwrap();
        let reparsed: NetworkDocument = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(doc, reparsed);

        // The emitted YAML must contain the snapshot's load-bearing strings.
        assert!(yaml.contains("enxead865c61ec9"));
        assert!(yaml.contains("169.254.42.2/24"));
        assert!(yaml.contains("cognitum-seed-linklocal"));
    }

    #[test]
    fn secrets_are_never_copied_into_adopted_output() {
        // A wifi connection whose PSK is present. We feed a RAW secret value (the
        // adversarial case: even if nmcli did NOT redact it to "<hidden>", lane's
        // own filter must drop it) and assert it never reaches the model.
        let wifi_lines: &[&str] = &[
            "connection.id:HomeWifi",
            "connection.uuid:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "connection.type:802-11-wireless",
            "connection.interface-name:wlp71s0",
            "802-11-wireless.ssid:HomeWifi",
            "802-11-wireless-security.key-mgmt:wpa-psk",
            "802-11-wireless-security.psk:hunter2-SUPER-SECRET",
            "802-11-wireless-security.wep-key0:DEADBEEFKEY",
            "802-1x.password:my-eap-password",
            "ipv4.method:auto",
        ];

        let unit = parse_nmcli_connection(wifi_lines, "802-11-wireless").expect("wifi unit");
        let mut doc = NetworkDocument::new(crate::net::model::Network::v2());
        unit.insert_into(&mut doc);

        let yaml = serde_yaml::to_string(&doc).unwrap();
        // HARD assertion: no raw secret material may appear anywhere in the output.
        assert!(
            !yaml.contains("hunter2-SUPER-SECRET"),
            "raw PSK leaked into adopted output"
        );
        assert!(
            !yaml.contains("DEADBEEFKEY"),
            "raw WEP key leaked into adopted output"
        );
        assert!(
            !yaml.contains("my-eap-password"),
            "raw 802.1x password leaked into adopted output"
        );

        // The credential instead surfaces as a secretd reference placeholder.
        assert!(yaml.contains("secretd"));
        let wifi = &doc.network.wifis["NM-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"];
        let ap = wifi.access_points.get("HomeWifi").expect("access point");
        assert_eq!(ap.key_mgmt.as_deref(), Some("wpa-psk"));
        let secret = ap.password.as_ref().expect("password is a secret ref");
        assert!(
            secret.secretd.starts_with("HomeWifi/"),
            "secret ref keyed by connection id and property, got {:?}",
            secret.secretd
        );
        // The reference itself must not embed the literal secret value.
        assert!(!secret.secretd.contains("hunter2-SUPER-SECRET"));
    }

    #[test]
    fn owe_open_wifi_has_no_secret_ref() {
        // The estate's TP-Link APs are OWE/open (key-mgmt: owe) — no credential
        // exists, so no SecretRef is emitted EVEN THOUGH nmcli masks the (unused)
        // psk/wep-key slots as "<hidden>". This is the exact over-eager case the
        // key-mgmt-driven emission fixes: presence is decided by key-mgmt, not by
        // the always-masked secret value.
        let owe_lines: &[&str] = &[
            "connection.id:TP-Link_6GHz_C00782",
            "connection.uuid:11111111-2222-3333-4444-555555555555",
            "connection.type:802-11-wireless",
            "connection.interface-name:wlp71s0",
            "802-11-wireless.ssid:TP-Link_6GHz_C00782",
            "802-11-wireless-security.key-mgmt:owe",
            // nmcli still masks these vestigial slots even for OWE:
            "802-11-wireless-security.psk:<hidden>",
            "802-11-wireless-security.wep-key0:<hidden>",
            "ipv4.method:auto",
        ];
        let unit = parse_nmcli_connection(owe_lines, "802-11-wireless").expect("wifi");
        let Unit::Wifi { unit, .. } = unit else {
            panic!("expected wifi unit");
        };
        let ap = unit.access_points.get("TP-Link_6GHz_C00782").unwrap();
        assert_eq!(ap.key_mgmt.as_deref(), Some("owe"));
        assert!(
            ap.password.is_none(),
            "OWE AP must carry no secret ref despite nmcli masking the unused slots"
        );
    }

    #[test]
    fn sae_wifi_emits_psk_secret_ref() {
        // SAE (WPA3-Personal) DOES use a password → a SecretRef keyed to the psk
        // property, but NEVER the masked value.
        let sae_lines: &[&str] = &[
            "connection.id:FlexNetOS",
            "connection.uuid:dd545cb7-b733-466c-b382-4a4d5c46e9df",
            "connection.type:802-11-wireless",
            "connection.interface-name:wlp71s0",
            "802-11-wireless.ssid:FlexNetOS",
            "802-11-wireless-security.key-mgmt:sae",
            "802-11-wireless-security.psk:<hidden>",
            "ipv4.method:auto",
        ];
        let unit = parse_nmcli_connection(sae_lines, "802-11-wireless").expect("wifi");
        let Unit::Wifi { unit, .. } = unit else {
            panic!("expected wifi unit");
        };
        let ap = unit.access_points.get("FlexNetOS").unwrap();
        let secret = ap.password.as_ref().expect("SAE requires a credential ref");
        assert_eq!(secret.secretd, "FlexNetOS/802-11-wireless-security.psk");
        // The reference must not embed the masked value or any material.
        assert!(!secret.secretd.contains("hidden"));
    }

    #[test]
    fn dhcp_ethernet_maps_method_auto_to_dhcp4() {
        let eno1_lines: &[&str] = &[
            "connection.id:netplan-eno1",
            "connection.uuid:99999999-0000-0000-0000-000000000000",
            "connection.type:802-3-ethernet",
            "connection.interface-name:eno1",
            "ipv4.method:auto",
            "ipv4.addresses:",
            "ipv6.method:auto",
        ];
        let unit = parse_nmcli_connection(eno1_lines, "802-3-ethernet").expect("ethernet");
        let Unit::Ethernet { unit, .. } = unit else {
            panic!("expected ethernet unit");
        };
        assert_eq!(unit.dhcp4, Some(true));
        assert_eq!(unit.dhcp6, Some(true));
        assert!(unit.addresses.is_empty());
        assert_eq!(
            unit.match_rule.as_ref().and_then(|m| m.name.as_deref()),
            Some("eno1")
        );
    }

    #[test]
    fn unadopted_types_are_skipped() {
        // loopback / unknown NM types are not part of the host plane lane adopts.
        assert!(UnitKind::from_nm_type("loopback").is_none());
        assert!(UnitKind::from_nm_type("tun").is_none());
        let lines: &[&str] = &["connection.id:lo", "connection.type:loopback"];
        assert!(parse_nmcli_connection(lines, "loopback").is_none());
    }

    #[test]
    fn is_secret_property_matches_credential_families() {
        assert!(is_secret_property("802-11-wireless-security.psk"));
        assert!(is_secret_property("802-11-wireless-security.wep-key0"));
        assert!(is_secret_property("802-1x.password"));
        assert!(is_secret_property("802-1x.private-key-password"));
        assert!(is_secret_property("vpn.secrets"));
        // Non-secrets must NOT be filtered.
        assert!(!is_secret_property("802-11-wireless-security.key-mgmt"));
        assert!(!is_secret_property("ipv4.addresses"));
        assert!(!is_secret_property("connection.id"));
    }

    #[test]
    fn line_split_keeps_colons_in_value() {
        // nmcli does not escape colons in values (e.g. seen-bssids, IPv6).
        let (prop, value) =
            split_nmcli_line("802-11-wireless.seen-bssids:AA:29:48:C5:4C:B3,A8:29:48:C5:4C:B1")
                .unwrap();
        assert_eq!(prop, "802-11-wireless.seen-bssids");
        assert_eq!(value, "AA:29:48:C5:4C:B3,A8:29:48:C5:4C:B1");
    }
}
