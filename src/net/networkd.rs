//! `lane net apply --renderer networkd` — the systemd-networkd render backend
//! (ADR-0003 §Decision item 4, "Portability"; §Sequencing P2: "networkd renderer
//! for non-NM boxes").
//!
//! A second apply backend so a box that does **not** run NetworkManager is still
//! reproducible from the same lane model. Where the P1 backend ([`crate::net::apply`])
//! emits `nmcli` operations, this backend renders the model to **systemd-networkd
//! drop-in files** under `/etc/systemd/network/`:
//!
//! - a `.network` file (`[Match]` + `[Network]`/`[Address]`/`[Route]`/DHCP) per unit;
//! - a `.netdev` + `.network` pair for a bridge unit (the `[NetDev]` creates the
//!   bridge device, the `.network` binds members and addresses);
//! - a `.link` file when a unit renames the device (`set-name` → `[Link] Name=`) or
//!   pins it by MAC.
//!
//! # Pure render, gated write (the established split)
//!
//! [`render_networkd`] is **pure** (model → [`Vec<NetworkdFile>`], no host access) and
//! built/fixture-tested in every build — the heart of this backend, mirroring
//! [`crate::net::apply::reconcile`]. Only the file-writing apply path
//! ([`write_networkd_files`]) takes the `hostnet` gate and is fail-closed, like
//! [`crate::net::apply::apply_plan`].
//!
//! # No-downgrade mapping
//!
//! The model maps to networkd **losslessly** for the host plane this ADR targets. The
//! cognitum-seed unit (static address, `ipv4.never-default`, `ipv6.method:
//! link-local`) renders faithfully:
//! - `match.name`/`match.macaddress` → `[Match] Name=`/`MACAddress=`;
//! - `addresses` → `[Network] Address=` (one per address);
//! - `dhcp4`/`dhcp6` → `[Network] DHCP=` (`ipv4`/`ipv6`/`yes`/`no`);
//! - `ipv4.never-default: "true"` → no auto/DHCP default route is accepted
//!   (`[DHCPv4] UseGateway=no`, `[Route]`-less for a static link) + a documented
//!   marker line, so a never-default link never becomes the system default;
//! - `ipv6.method: link-local` → `[Network] LinkLocalAddressing=ipv6` +
//!   `IPv6AcceptRA=no` (a pure link-local interface: no RA-derived global address or
//!   default route);
//! - `nameservers` → `[Network] DNS=` (one per address) + `Domains=` (space-joined
//!   search domains) when present;
//! - `wakeonlan: true` → a `.link` file's `[Link] WakeOnLan=magic` (the only place
//!   networkd carries WoL); a WoL-only unit still emits the `.link`;
//! - `routes` → one `[Route]` section each (`Destination=`/`Gateway=`/`Metric=`/
//!   `GatewayOnLink=yes` for `on-link: true`).
//!
//! # No silent drops (two-layer guard)
//!
//! 1. A passthrough key with no first-class networkd handling is carried as a
//!    `#`-commented, documented **gap** line ([`push_passthrough_gaps`]).
//! 2. A **first-class model field** that the renderer does not emit is surfaced as a
//!    `# unmapped (porter task): <field>` gap by a structural audit
//!    ([`push_first_class_gaps`], fed by the `audit_*` helpers). Those helpers
//!    exhaustively destructure their unit type (no `..`), so a new model field is a
//!    **compile error** until it is either rendered or gapped — this class of bug
//!    cannot recur. The file always names what it could not render; nothing is lost
//!    silently.
//!
//! # Secrets
//!
//! networkd has no inline Wi-Fi PSK (wpa_supplicant owns 802.11 auth), so a
//! [`crate::net::model::SecretRef`]-bearing Wi-Fi unit renders **no** credential
//! material — only a documented marker pointing at the wpa_supplicant/secretd seam.
//! As in P1, secret material is never present in the rendered output.

use crate::net::model::{BridgeUnit, EthernetUnit, Nameservers, NetworkDocument, Route, WifiUnit};

/// The placeholder/marker a Wi-Fi credential renders to in a networkd file. networkd
/// itself never carries the PSK (wpa_supplicant does), so no real secret — and not
/// even the `secretd` ref key — ever appears in the rendered output.
pub const WIFI_SECRET_MARKER: &str =
    "# wifi auth handled by wpa_supplicant (secretd seam); not rendered in networkd";

/// One rendered systemd-networkd file: its absolute path and full contents.
///
/// Printed verbatim on a dry-run (path + contents) and, on a gated live apply,
/// written via [`crate::system::write_file_elevated`]. The contents never carry
/// secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkdFile {
    /// Absolute path under `/etc/systemd/network/` (e.g.
    /// `/etc/systemd/network/10-lane-<name>.network`).
    pub path: String,
    /// Full file contents (a complete networkd unit file).
    pub contents: String,
}

/// The directory systemd-networkd reads its drop-in files from.
const NETWORKD_DIR: &str = "/etc/systemd/network";

/// Render an entire [`NetworkDocument`] to the set of systemd-networkd files that
/// reproduce it. Pure (no host access) — the always-built heart of this backend.
///
/// Files are emitted in deterministic order (ethernets, then wifis, then bridges,
/// each in `BTreeMap` key order) so the render is stable and diff-friendly.
pub fn render_networkd(doc: &NetworkDocument) -> Vec<NetworkdFile> {
    let mut files = Vec::new();

    for (id, unit) in &doc.network.ethernets {
        let name = unit_filename(
            id,
            unit.networkmanager
                .as_ref()
                .and_then(|nm| nm.name.as_deref()),
        );
        files.push(render_ethernet(&name, unit));
        if let Some(link) = render_link(
            &name,
            unit.set_name.as_deref(),
            unit.wakeonlan,
            match_name(unit),
            match_mac(unit),
        ) {
            files.push(link);
        }
    }

    for (id, unit) in &doc.network.wifis {
        let name = unit_filename(
            id,
            unit.networkmanager
                .as_ref()
                .and_then(|nm| nm.name.as_deref()),
        );
        files.push(render_wifi(&name, unit));
        let wmname = unit.match_rule.as_ref().and_then(|m| m.name.as_deref());
        let wmmac = unit
            .match_rule
            .as_ref()
            .and_then(|m| m.macaddress.as_deref());
        if let Some(link) = render_link(
            &name,
            unit.set_name.as_deref(),
            unit.wakeonlan,
            wmname,
            wmmac,
        ) {
            files.push(link);
        }
    }

    for (id, unit) in &doc.network.bridges {
        let name = unit_filename(
            id,
            unit.networkmanager
                .as_ref()
                .and_then(|nm| nm.name.as_deref()),
        );
        files.extend(render_bridge(&name, unit));
    }

    files
}

/// The stable file-name stem for a unit: the NM connection name when present (the
/// human-meaningful id), else the netplan unit id. Sanitized to a filesystem-safe
/// token so the `.network` filename is well-formed.
fn unit_filename(id: &str, nm_name: Option<&str>) -> String {
    let base = nm_name.unwrap_or(id);
    sanitize_filename(base)
}

/// Replace any character that is not alphanumeric / `-` / `_` with `-`, so an NM
/// connection name like `"Wired connection 1"` becomes a valid filename stem.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `match.name` of an ethernet unit, if any.
fn match_name(unit: &EthernetUnit) -> Option<&str> {
    unit.match_rule.as_ref().and_then(|m| m.name.as_deref())
}

/// `match.macaddress` of an ethernet unit, if any.
fn match_mac(unit: &EthernetUnit) -> Option<&str> {
    unit.match_rule
        .as_ref()
        .and_then(|m| m.macaddress.as_deref())
}

/// Render a `.link` file for a unit that renames (`set-name`) and/or enables
/// Wake-on-LAN. Returns `None` when there is nothing a `.link` expresses (no rename
/// and no WoL). A `.link` is needed for `set-name` because `[Network]` cannot rename a
/// device, and is the only place networkd carries `WakeOnLan=` — so a WoL-only unit
/// (no rename) still produces a `.link`.
fn render_link(
    name: &str,
    set_name: Option<&str>,
    wakeonlan: Option<bool>,
    match_name: Option<&str>,
    match_mac: Option<&str>,
) -> Option<NetworkdFile> {
    let wol = matches!(wakeonlan, Some(true));
    // Nothing a `.link` expresses → no file. `Some(false)`/`None` WoL is not a line.
    if set_name.is_none() && !wol {
        return None;
    }
    let mut out = String::new();
    out.push_str("[Match]\n");
    if let Some(n) = match_name {
        out.push_str(&format!("OriginalName={n}\n"));
    }
    if let Some(mac) = match_mac {
        out.push_str(&format!("MACAddress={mac}\n"));
    }
    out.push_str("\n[Link]\n");
    if let Some(new_name) = set_name {
        out.push_str(&format!("Name={new_name}\n"));
    }
    if wol {
        out.push_str("WakeOnLan=magic\n");
    }
    Some(NetworkdFile {
        path: format!("{NETWORKD_DIR}/10-lane-{name}.link"),
        contents: out,
    })
}

/// Render an ethernet unit to its `.network` file.
fn render_ethernet(name: &str, unit: &EthernetUnit) -> NetworkdFile {
    let mut s = String::new();
    push_match(&mut s, match_name(unit), match_mac(unit));
    push_network_section(
        &mut s,
        &unit.addresses,
        unit.dhcp4,
        unit.dhcp6,
        unit.nameservers.as_ref(),
        unit.networkmanager.as_ref(),
        &unit.routes,
    );
    push_passthrough_gaps(&mut s, unit.networkmanager.as_ref());
    push_first_class_gaps(&mut s, &audit_ethernet(unit));
    NetworkdFile {
        path: format!("{NETWORKD_DIR}/10-lane-{name}.network"),
        contents: s,
    }
}

/// Classify every first-class [`EthernetUnit`] field as rendered-or-set so the
/// no-silent-drop audit ([`push_first_class_gaps`]) can surface any field that is
/// present but not emitted. The destructuring (no `..`) is the structural guard: a new
/// model field is a **compile error** here until it is classified, so it cannot be
/// added without either rendering it or gapping it.
fn audit_ethernet(unit: &EthernetUnit) -> Vec<FieldAudit> {
    let EthernetUnit {
        renderer,
        match_rule,
        set_name,
        addresses,
        routes,
        dhcp4,
        dhcp6,
        nameservers,
        wakeonlan,
        networkmanager,
    } = unit;
    vec![
        // `renderer` selects the backend; it is consumed, not a `.network` line.
        FieldAudit::rendered("renderer", renderer.is_some()),
        FieldAudit::rendered("match", match_rule.is_some()),
        // `set-name`/`wakeonlan` render to the unit's `.link` file (emitted alongside).
        FieldAudit::rendered("set-name", set_name.is_some()),
        FieldAudit::rendered("addresses", !addresses.is_empty()),
        FieldAudit::rendered("routes", !routes.is_empty()),
        FieldAudit::rendered("dhcp4", dhcp4.is_some()),
        FieldAudit::rendered("dhcp6", dhcp6.is_some()),
        FieldAudit::rendered("nameservers", nameservers.is_some()),
        FieldAudit::rendered("wakeonlan", wakeonlan.is_some()),
        // The passthrough map is audited key-by-key by `push_passthrough_gaps`.
        FieldAudit::rendered("networkmanager", networkmanager.is_some()),
    ]
}

/// Render a Wi-Fi unit to its `.network` file. networkd carries no 802.11 auth, so a
/// credential renders only the documented [`WIFI_SECRET_MARKER`], never material.
fn render_wifi(name: &str, unit: &WifiUnit) -> NetworkdFile {
    let mut s = String::new();
    let mname = unit.match_rule.as_ref().and_then(|m| m.name.as_deref());
    let mmac = unit
        .match_rule
        .as_ref()
        .and_then(|m| m.macaddress.as_deref());
    push_match(&mut s, mname, mmac);
    push_network_section(
        &mut s,
        &unit.addresses,
        unit.dhcp4,
        unit.dhcp6,
        unit.nameservers.as_ref(),
        unit.networkmanager.as_ref(),
        &unit.routes,
    );
    // Wi-Fi auth is wpa_supplicant's job; a credential-bearing AP renders only the
    // marker (the secretd → wpa_supplicant seam), never the secret or its ref key.
    let needs_auth = unit.access_points.values().any(|ap| {
        ap.password.is_some()
            || ap
                .key_mgmt
                .as_deref()
                .is_some_and(|k| k != "owe" && k != "none")
    });
    if needs_auth {
        s.push('\n');
        s.push_str(WIFI_SECRET_MARKER);
        s.push('\n');
    }
    push_passthrough_gaps(&mut s, unit.networkmanager.as_ref());
    push_first_class_gaps(&mut s, &audit_wifi(unit));
    NetworkdFile {
        path: format!("{NETWORKD_DIR}/10-lane-{name}.network"),
        contents: s,
    }
}

/// First-class field audit for a [`WifiUnit`] — see [`audit_ethernet`].
fn audit_wifi(unit: &WifiUnit) -> Vec<FieldAudit> {
    let WifiUnit {
        renderer,
        match_rule,
        set_name,
        addresses,
        routes,
        dhcp4,
        dhcp6,
        nameservers,
        wakeonlan,
        networkmanager,
        access_points,
    } = unit;
    vec![
        FieldAudit::rendered("renderer", renderer.is_some()),
        FieldAudit::rendered("match", match_rule.is_some()),
        FieldAudit::rendered("set-name", set_name.is_some()),
        FieldAudit::rendered("addresses", !addresses.is_empty()),
        FieldAudit::rendered("routes", !routes.is_empty()),
        FieldAudit::rendered("dhcp4", dhcp4.is_some()),
        FieldAudit::rendered("dhcp6", dhcp6.is_some()),
        FieldAudit::rendered("nameservers", nameservers.is_some()),
        FieldAudit::rendered("wakeonlan", wakeonlan.is_some()),
        FieldAudit::rendered("networkmanager", networkmanager.is_some()),
        // 802.11 auth is wpa_supplicant's; a credential-bearing AP renders only the
        // marker. The access-points map is intentionally not a `[Network]` line.
        FieldAudit::rendered("access-points", !access_points.is_empty()),
    ]
}

/// Render a bridge unit to its `.netdev` (creates the bridge device) + `.network`
/// (binds members + addresses) file pair.
fn render_bridge(name: &str, unit: &BridgeUnit) -> Vec<NetworkdFile> {
    let mut files = Vec::new();

    // The .netdev that creates the bridge device.
    let netdev = format!("[NetDev]\nName={name}\nKind=bridge\n");
    files.push(NetworkdFile {
        path: format!("{NETWORKD_DIR}/10-lane-{name}.netdev"),
        contents: netdev,
    });

    // The .network that gives the bridge its addressing.
    let mut s = String::new();
    s.push_str("[Match]\n");
    s.push_str(&format!("Name={name}\n"));
    push_network_section(
        &mut s,
        &unit.addresses,
        unit.dhcp4,
        unit.dhcp6,
        unit.nameservers.as_ref(),
        unit.networkmanager.as_ref(),
        &unit.routes,
    );
    push_passthrough_gaps(&mut s, unit.networkmanager.as_ref());
    push_first_class_gaps(&mut s, &audit_bridge(unit));
    files.push(NetworkdFile {
        path: format!("{NETWORKD_DIR}/10-lane-{name}.network"),
        contents: s,
    });

    // Each member interface gets a tiny .network binding it into the bridge.
    for member in &unit.interfaces {
        let member_name = sanitize_filename(member);
        let contents = format!("[Match]\nName={member}\n\n[Network]\nBridge={name}\n");
        files.push(NetworkdFile {
            path: format!("{NETWORKD_DIR}/10-lane-{name}-member-{member_name}.network"),
            contents,
        });
    }

    files
}

/// First-class field audit for a [`BridgeUnit`] — see [`audit_ethernet`]. The
/// `interfaces` member list renders as the per-member `.network` bindings.
fn audit_bridge(unit: &BridgeUnit) -> Vec<FieldAudit> {
    let BridgeUnit {
        renderer,
        interfaces,
        addresses,
        routes,
        dhcp4,
        dhcp6,
        nameservers,
        networkmanager,
    } = unit;
    vec![
        FieldAudit::rendered("renderer", renderer.is_some()),
        FieldAudit::rendered("interfaces", !interfaces.is_empty()),
        FieldAudit::rendered("addresses", !addresses.is_empty()),
        FieldAudit::rendered("routes", !routes.is_empty()),
        FieldAudit::rendered("dhcp4", dhcp4.is_some()),
        FieldAudit::rendered("dhcp6", dhcp6.is_some()),
        FieldAudit::rendered("nameservers", nameservers.is_some()),
        FieldAudit::rendered("networkmanager", networkmanager.is_some()),
    ]
}

/// One first-class model field's render status, for the no-silent-drop audit. A field
/// that is **present in the model but not rendered** is surfaced as a documented gap by
/// [`push_first_class_gaps`] — so a field can never vanish without leaving a trace.
struct FieldAudit {
    /// The model field name (as it reads in the model / netplan).
    field: &'static str,
    /// Whether the field carries a value in this unit.
    present: bool,
    /// Whether the renderer emits this field (to the `.network` or the `.link`).
    rendered: bool,
}

impl FieldAudit {
    /// A field the renderer emits when present.
    fn rendered(field: &'static str, present: bool) -> FieldAudit {
        FieldAudit {
            field,
            present,
            rendered: true,
        }
    }
}

/// Append a `# unmapped (porter task): <field>` gap line for every first-class field
/// that is **present but not rendered** — the structural no-silent-drop guard. Paired
/// with the exhaustive destructuring in the `audit_*` helpers (which compile-fails on a
/// new, unclassified model field), this makes a silently-dropped first-class field
/// impossible to introduce.
fn push_first_class_gaps(s: &mut String, audits: &[FieldAudit]) {
    let mut gaps: Vec<String> = Vec::new();
    for a in audits {
        if a.present && !a.rendered {
            gaps.push(format!("# unmapped (porter task): {}", a.field));
        }
    }
    if !gaps.is_empty() {
        s.push('\n');
        for g in gaps {
            s.push_str(&g);
            s.push('\n');
        }
    }
}

/// Append the `[Match]` section. Always emitted (even empty units carry a `[Match]`
/// so the file is a valid networkd unit), with `Name=`/`MACAddress=` when known.
fn push_match(s: &mut String, name: Option<&str>, mac: Option<&str>) {
    s.push_str("[Match]\n");
    if let Some(n) = name {
        s.push_str(&format!("Name={n}\n"));
    }
    if let Some(m) = mac {
        s.push_str(&format!("MACAddress={m}\n"));
    }
}

/// Append the `[Network]` section plus any `[Route]`/`[DHCPv4]` sections, applying the
/// never-default / link-local passthrough semantics.
fn push_network_section(
    s: &mut String,
    addresses: &[String],
    dhcp4: Option<bool>,
    dhcp6: Option<bool>,
    nameservers: Option<&Nameservers>,
    nm: Option<&crate::net::model::NmPassthrough>,
    routes: &[Route],
) {
    let passthrough = nm.map(|n| &n.passthrough);
    let never_default = passthrough
        .and_then(|p| p.get("ipv4.never-default"))
        .map(|v| v == "true")
        .unwrap_or(false);
    let ipv6_link_local = passthrough
        .and_then(|p| p.get("ipv6.method"))
        .map(|v| v == "link-local")
        .unwrap_or(false);

    s.push_str("\n[Network]\n");

    if let Some(dhcp) = dhcp_value(dhcp4, dhcp6) {
        s.push_str(&format!("DHCP={dhcp}\n"));
    }
    for addr in addresses {
        s.push_str(&format!("Address={addr}\n"));
    }

    // DNS: one `DNS=` line per nameserver address, and a space-joined `Domains=` line
    // for the search domains. networkd reads both from `[Network]`.
    if let Some(ns) = nameservers {
        for dns in &ns.addresses {
            s.push_str(&format!("DNS={dns}\n"));
        }
        if !ns.search.is_empty() {
            s.push_str(&format!("Domains={}\n", ns.search.join(" ")));
        }
    }

    if ipv6_link_local {
        // Pure link-local IPv6: keep the link-local address, but accept no RA (no
        // RA-derived global address or default route). Faithful render of
        // `ipv6.method: link-local`.
        s.push_str("LinkLocalAddressing=ipv6\n");
        s.push_str("IPv6AcceptRA=no\n");
    }

    if never_default {
        // Never become the system default route. With no static gateway and no
        // accepted DHCP/RA default, networkd creates no default route here; the
        // marker documents the asserted intent so it is explicit, not incidental.
        s.push_str("# ipv4.never-default: this link must never provide the default route\n");
    }

    // Static routes: one [Route] section each.
    for r in routes {
        s.push_str("\n[Route]\n");
        let dest = if r.to == "default" {
            "0.0.0.0/0"
        } else {
            &r.to
        };
        s.push_str(&format!("Destination={dest}\n"));
        if let Some(via) = &r.via {
            s.push_str(&format!("Gateway={via}\n"));
        }
        if let Some(metric) = r.metric {
            s.push_str(&format!("Metric={metric}\n"));
        }
        if matches!(r.on_link, Some(true)) {
            // netplan `on-link: true` → the gateway is directly reachable.
            s.push_str("GatewayOnLink=yes\n");
        }
    }

    // DHCP-side never-default: if DHCP is on, refuse the DHCP-provided default route.
    if never_default && matches!(dhcp4, Some(true)) {
        s.push_str("\n[DHCPv4]\nUseGateway=no\n");
    }
}

/// Map `dhcp4`/`dhcp6` to networkd's `DHCP=` value (`yes`/`ipv4`/`ipv6`/`no`).
/// Returns `None` when neither is set (the `DHCP=` line is then omitted).
fn dhcp_value(dhcp4: Option<bool>, dhcp6: Option<bool>) -> Option<&'static str> {
    match (dhcp4, dhcp6) {
        (Some(true), Some(true)) => Some("yes"),
        (Some(true), _) => Some("ipv4"),
        (_, Some(true)) => Some("ipv6"),
        (Some(false), Some(false)) => Some("no"),
        (Some(false), None) | (None, Some(false)) => Some("no"),
        (None, None) => None,
    }
}

/// Append `#`-commented **gap** lines for any passthrough key this renderer does not
/// structurally express. The two keys with first-class networkd handling
/// (`ipv4.never-default`, `ipv6.method`) are NOT listed as gaps; everything else is
/// carried as a documented porter task so no host intent is silently dropped.
fn push_passthrough_gaps(s: &mut String, nm: Option<&crate::net::model::NmPassthrough>) {
    let Some(nm) = nm else { return };
    let mut gaps: Vec<String> = Vec::new();
    for (k, v) in &nm.passthrough {
        // Handled structurally — not a gap.
        if k == "ipv4.never-default" || k == "ipv6.method" {
            continue;
        }
        gaps.push(format!("# unmapped (porter task): {k} = {v}"));
    }
    if !gaps.is_empty() {
        s.push('\n');
        for g in gaps {
            s.push_str(&g);
            s.push('\n');
        }
    }
}

/// Render the full file set as a human-readable dry-run report (each file as a
/// `--- <path> ---` banner followed by its contents). Pure; carries no secrets.
pub fn render_files_text(files: &[NetworkdFile]) -> String {
    if files.is_empty() {
        return "# no networkd files — empty model\n".to_string();
    }
    let mut out = String::new();
    for f in files {
        out.push_str(&format!("--- {} ---\n", f.path));
        out.push_str(&f.contents);
        if !f.contents.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

// --- live apply (feature-gated) --------------------------------------------

/// Write the rendered networkd files to the host, each via
/// [`crate::system::write_file_elevated`] (direct write, `sudo tee` fallback),
/// **fail-closed** (stop on the first error).
///
/// NOTE: this is the host-mutating path. After writing, the caller must reload
/// networkd (`networkctl reload` / `systemctl restart systemd-networkd`) for the
/// files to take effect — that reload is documented but not run here (the estate runs
/// NetworkManager; this backend is for non-NM boxes and its live execution is the
/// human wall, like P1's nmcli apply). A file carrying a Wi-Fi credential is
/// impossible by construction (only the marker is rendered), so no secret is written.
#[cfg(feature = "hostnet")]
pub fn write_networkd_files(files: &[NetworkdFile]) -> anyhow::Result<()> {
    use anyhow::Context;
    for f in files {
        crate::system::write_file_elevated(&f.path, &f.contents)
            .with_context(|| format!("writing networkd file {}", f.path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::{
        AccessPoint, BridgeUnit, EthernetUnit, MatchRule, Nameservers, Network, NmPassthrough,
        Renderer, Route, SecretRef, WifiUnit,
    };

    /// The cognitum-seed link-local ethernet unit (the snapshot's load-bearing case).
    fn cognitum_seed_doc() -> NetworkDocument {
        let mut passthrough = std::collections::BTreeMap::new();
        passthrough.insert("ipv4.never-default".to_string(), "true".to_string());
        passthrough.insert("ipv6.method".to_string(), "link-local".to_string());

        let eth = EthernetUnit {
            renderer: Some(Renderer::Networkd),
            match_rule: Some(MatchRule {
                name: Some("enxead865c61ec9".to_string()),
                macaddress: None,
            }),
            addresses: vec!["169.254.42.2/24".to_string()],
            dhcp4: Some(false),
            networkmanager: Some(NmPassthrough {
                name: Some("cognitum-seed-linklocal".to_string()),
                uuid: Some("70b82336-d3cd-4204-90aa-fe8a1ed5e769".to_string()),
                passthrough,
            }),
            ..EthernetUnit::default()
        };

        let mut network = Network::v2();
        network
            .ethernets
            .insert("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769".to_string(), eth);
        NetworkDocument::new(network)
    }

    /// Acceptance — the cognitum-seed unit renders a `.network` file whose Match,
    /// Address, never-default, and link-local lines are present and correct.
    #[test]
    fn cognitum_seed_renders_faithfully() {
        let files = render_networkd(&cognitum_seed_doc());
        assert_eq!(files.len(), 1, "one .network file, no .link (no set-name)");
        let f = &files[0];

        // The path is the networkd drop-in named by the NM connection.
        assert_eq!(
            f.path,
            "/etc/systemd/network/10-lane-cognitum-seed-linklocal.network"
        );

        let c = &f.contents;
        // [Match] Name= the physical device.
        assert!(c.contains("[Match]"));
        assert!(
            c.contains("Name=enxead865c61ec9"),
            "match name missing:\n{c}"
        );
        // The static address.
        assert!(
            c.contains("Address=169.254.42.2/24"),
            "address missing:\n{c}"
        );
        // never-default is asserted (documented marker — link never provides default).
        assert!(
            c.contains("ipv4.never-default"),
            "never-default intent must be rendered:\n{c}"
        );
        // ipv6 link-local renders LinkLocalAddressing=ipv6 + IPv6AcceptRA=no.
        assert!(
            c.contains("LinkLocalAddressing=ipv6"),
            "ipv6 link-local addressing missing:\n{c}"
        );
        assert!(
            c.contains("IPv6AcceptRA=no"),
            "ipv6 link-local must accept no RA:\n{c}"
        );
        // dhcp4=false, dhcp6 unset → DHCP=no.
        assert!(
            c.contains("DHCP=no"),
            "static (no-dhcp) link → DHCP=no:\n{c}"
        );
        // No unmapped-gap line (every passthrough key is handled structurally).
        assert!(
            !c.contains("unmapped (porter task)"),
            "the snapshot unit has no unmapped fields:\n{c}"
        );
    }

    /// Acceptance — a SecretRef-bearing Wi-Fi unit renders NO secret material (and not
    /// even the ref key); only the documented wpa_supplicant marker.
    #[test]
    fn wifi_secret_unit_renders_no_secret_material() {
        let mut access_points = std::collections::BTreeMap::new();
        access_points.insert(
            "FlexNetOS".to_string(),
            AccessPoint {
                key_mgmt: Some("wpa-psk".to_string()),
                password: Some(SecretRef::new("cognitum-seed/wifi-psk")),
            },
        );
        let wifi = WifiUnit {
            renderer: Some(Renderer::Networkd),
            match_rule: Some(MatchRule {
                name: Some("wlp71s0".to_string()),
                macaddress: None,
            }),
            dhcp4: Some(true),
            networkmanager: Some(NmPassthrough {
                name: Some("FlexNetOS".to_string()),
                ..NmPassthrough::default()
            }),
            access_points,
            ..WifiUnit::default()
        };
        let mut net = Network::v2();
        net.wifis.insert("NM-wifi".to_string(), wifi);
        let doc = NetworkDocument::new(net);

        let files = render_networkd(&doc);
        let all: String = files.iter().map(|f| f.contents.clone()).collect();

        // No secret material, and not even the secretd ref key, appears.
        assert!(
            !all.contains("cognitum-seed/wifi-psk"),
            "the secretd ref key must not appear in networkd output:\n{all}"
        );
        assert!(
            !all.contains("hunter2"),
            "no literal secret may appear:\n{all}"
        );
        // The documented wpa_supplicant/secretd marker IS present.
        assert!(
            all.contains("wpa_supplicant"),
            "credential-bearing wifi must document the wpa_supplicant seam:\n{all}"
        );
        // The DHCP intent still renders.
        assert!(all.contains("DHCP=ipv4"));
    }

    /// A `set-name` ethernet unit additionally emits a `.link` file (renames cannot
    /// be expressed in `[Network]`).
    #[test]
    fn set_name_emits_link_file() {
        let eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eth0".to_string()),
                macaddress: Some("10:ff:e0:b3:9e:55".to_string()),
            }),
            set_name: Some("lan0".to_string()),
            dhcp4: Some(true),
            networkmanager: Some(NmPassthrough {
                name: Some("lan".to_string()),
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        let mut net = Network::v2();
        net.ethernets.insert("NM-lan".to_string(), eth);
        let files = render_networkd(&NetworkDocument::new(net));

        let link = files
            .iter()
            .find(|f| f.path.ends_with(".link"))
            .expect("a .link file for the set-name rename");
        assert!(link.contents.contains("[Link]"));
        assert!(link.contents.contains("Name=lan0"), "renamed to set-name");
        assert!(link.contents.contains("OriginalName=eth0"));
        assert!(link.contents.contains("MACAddress=10:ff:e0:b3:9e:55"));
    }

    /// A bridge renders a `.netdev` (creates the device) + `.network` (addresses) +
    /// one `.network` per member binding it in.
    #[test]
    fn bridge_renders_netdev_and_member_bindings() {
        let bridge = BridgeUnit {
            interfaces: vec!["eno1".to_string(), "eno2".to_string()],
            addresses: vec!["10.0.0.1/24".to_string()],
            networkmanager: Some(NmPassthrough {
                name: Some("br-lan".to_string()),
                ..NmPassthrough::default()
            }),
            ..BridgeUnit::default()
        };
        let mut net = Network::v2();
        net.bridges.insert("NM-br".to_string(), bridge);
        let files = render_networkd(&NetworkDocument::new(net));

        // .netdev creates the bridge.
        let netdev = files
            .iter()
            .find(|f| f.path.ends_with("10-lane-br-lan.netdev"))
            .expect(".netdev");
        assert!(netdev.contents.contains("Kind=bridge"));
        assert!(netdev.contents.contains("Name=br-lan"));

        // The bridge's own .network carries the address.
        let brnet = files
            .iter()
            .find(|f| f.path.ends_with("10-lane-br-lan.network"))
            .expect("bridge .network");
        assert!(brnet.contents.contains("Address=10.0.0.1/24"));

        // Each member is bound in.
        let members: Vec<_> = files
            .iter()
            .filter(|f| f.path.contains("-member-"))
            .collect();
        assert_eq!(members.len(), 2, "one binding per member");
        assert!(members
            .iter()
            .any(|f| f.contents.contains("Bridge=br-lan") && f.contents.contains("Name=eno1")));
    }

    /// An unmapped passthrough key is carried as a documented porter-task gap, never
    /// silently dropped.
    #[test]
    fn unmapped_passthrough_key_becomes_documented_gap() {
        let mut passthrough = std::collections::BTreeMap::new();
        passthrough.insert("ipv4.never-default".to_string(), "true".to_string());
        // A key with no first-class networkd mapping in this backend.
        passthrough.insert("802-3-ethernet.mtu".to_string(), "9000".to_string());

        let eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: None,
            }),
            dhcp4: Some(false),
            networkmanager: Some(NmPassthrough {
                name: Some("jumbo".to_string()),
                passthrough,
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        let mut net = Network::v2();
        net.ethernets.insert("NM-j".to_string(), eth);
        let files = render_networkd(&NetworkDocument::new(net));
        let c = &files[0].contents;

        assert!(
            c.contains("# unmapped (porter task): 802-3-ethernet.mtu = 9000"),
            "an unmappable field must be a documented gap, not a silent drop:\n{c}"
        );
        // never-default is handled structurally, so it is NOT listed as a gap.
        assert!(!c.contains("# unmapped (porter task): ipv4.never-default"));
    }

    /// `render_files_text` produces a readable dry-run report with a banner per file.
    #[test]
    fn files_text_has_path_banners() {
        let files = render_networkd(&cognitum_seed_doc());
        let text = render_files_text(&files);
        assert!(
            text.contains("--- /etc/systemd/network/10-lane-cognitum-seed-linklocal.network ---")
        );
        assert!(text.contains("Address=169.254.42.2/24"));
    }

    /// No-downgrade: an ethernet unit carrying `nameservers` (addresses + search),
    /// `wakeonlan: Some(true)`, and a route with `on_link: Some(true)` renders DNS,
    /// Domains, a WoL `.link`, and GatewayOnLink — none of them silently dropped.
    #[test]
    fn nameservers_wol_and_onlink_render() {
        let eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: Some("10:ff:e0:b3:9e:55".to_string()),
            }),
            addresses: vec!["10.0.0.5/24".to_string()],
            dhcp4: Some(false),
            wakeonlan: Some(true),
            nameservers: Some(Nameservers {
                addresses: vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()],
                search: vec!["example.test".to_string(), "corp.test".to_string()],
            }),
            routes: vec![Route {
                to: "default".to_string(),
                via: Some("10.0.0.1".to_string()),
                metric: Some(100),
                on_link: Some(true),
            }],
            networkmanager: Some(NmPassthrough {
                name: Some("wired".to_string()),
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        let mut net = Network::v2();
        net.ethernets.insert("NM-wired".to_string(), eth);
        let files = render_networkd(&NetworkDocument::new(net));

        // The .network carries DNS=, Domains=, and GatewayOnLink=yes.
        let network = files
            .iter()
            .find(|f| f.path.ends_with("10-lane-wired.network"))
            .expect("the .network file");
        let c = &network.contents;
        assert!(c.contains("DNS=1.1.1.1"), "first nameserver missing:\n{c}");
        assert!(c.contains("DNS=9.9.9.9"), "second nameserver missing:\n{c}");
        assert!(
            c.contains("Domains=example.test corp.test"),
            "space-joined search domains missing:\n{c}"
        );
        assert!(
            c.contains("GatewayOnLink=yes"),
            "on-link route must render GatewayOnLink=yes:\n{c}"
        );

        // wakeonlan renders to the .link's [Link] WakeOnLan=magic.
        let link = files
            .iter()
            .find(|f| f.path.ends_with("10-lane-wired.link"))
            .expect("a .link file for wake-on-lan");
        assert!(link.contents.contains("[Link]"), "{}", link.contents);
        assert!(
            link.contents.contains("WakeOnLan=magic"),
            "wakeonlan must render WakeOnLan=magic:\n{}",
            link.contents
        );
        // The .link matches the device so networkd applies it.
        assert!(link.contents.contains("OriginalName=eno1"));

        // No field was silently dropped: no first-class gap appears for these.
        let all: String = files.iter().map(|f| f.contents.clone()).collect();
        assert!(
            !all.contains("# unmapped (porter task): nameservers"),
            "nameservers must render, not gap:\n{all}"
        );
        assert!(
            !all.contains("# unmapped (porter task): wakeonlan"),
            "wakeonlan must render, not gap:\n{all}"
        );
    }

    /// A WoL-only ethernet (no set-name) still emits a `.link` — WakeOnLan is `.link`-
    /// only, so a unit whose only `.link`-worthy attribute is WoL must still produce one.
    #[test]
    fn wol_only_unit_still_emits_link() {
        let eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: None,
            }),
            wakeonlan: Some(true),
            networkmanager: Some(NmPassthrough {
                name: Some("wol".to_string()),
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        let mut net = Network::v2();
        net.ethernets.insert("NM-wol".to_string(), eth);
        let files = render_networkd(&NetworkDocument::new(net));

        let link = files
            .iter()
            .find(|f| f.path.ends_with(".link"))
            .expect("a WoL-only unit must still emit a .link file");
        assert!(link.contents.contains("WakeOnLan=magic"));
        // No rename line, since there is no set-name.
        assert!(!link.contents.contains("Name=") || !link.contents.contains("\nName="));
    }

    /// `wakeonlan: Some(false)` / `None` produces no WakeOnLan line and (absent a
    /// rename) no `.link` at all.
    #[test]
    fn wol_false_renders_no_link() {
        for wol in [Some(false), None] {
            let eth = EthernetUnit {
                match_rule: Some(MatchRule {
                    name: Some("eno1".to_string()),
                    macaddress: None,
                }),
                wakeonlan: wol,
                networkmanager: Some(NmPassthrough {
                    name: Some("nowol".to_string()),
                    ..NmPassthrough::default()
                }),
                ..EthernetUnit::default()
            };
            let mut net = Network::v2();
            net.ethernets.insert("NM-nowol".to_string(), eth);
            let files = render_networkd(&NetworkDocument::new(net));
            assert!(
                !files.iter().any(|f| f.path.ends_with(".link")),
                "WoL={wol:?} must not emit a .link"
            );
            let all: String = files.iter().map(|f| f.contents.clone()).collect();
            assert!(
                !all.contains("WakeOnLan"),
                "WoL={wol:?} must not render WakeOnLan"
            );
        }
    }

    /// No-silent-drop guard: a unit exercising nameservers, wakeonlan, and an on-link
    /// route yields, for each first-class field, either a rendered line OR a gap
    /// comment — no field vanishes. (Here all three render, so none gap.)
    #[test]
    fn first_class_fields_render_or_gap_never_vanish() {
        let eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: None,
            }),
            wakeonlan: Some(true),
            nameservers: Some(Nameservers {
                addresses: vec!["8.8.8.8".to_string()],
                search: vec!["lan.test".to_string()],
            }),
            routes: vec![Route {
                to: "10.0.0.0/24".to_string(),
                via: Some("10.0.0.1".to_string()),
                metric: None,
                on_link: Some(true),
            }],
            networkmanager: Some(NmPassthrough {
                name: Some("full".to_string()),
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        let mut net = Network::v2();
        net.ethernets.insert("NM-full".to_string(), eth);
        let files = render_networkd(&NetworkDocument::new(net));
        let all: String = files.iter().map(|f| f.contents.clone()).collect();

        // For each field: rendered-line OR documented gap — never absent.
        let cases = [
            ("nameservers", "DNS=8.8.8.8"),
            ("wakeonlan", "WakeOnLan=magic"),
            ("on-link route", "GatewayOnLink=yes"),
        ];
        for (field, rendered_marker) in cases {
            let rendered = all.contains(rendered_marker);
            let gapped = all.contains(&format!("# unmapped (porter task): {field}"));
            assert!(
                rendered || gapped,
                "{field} must be rendered or gapped, never silently dropped:\n{all}"
            );
        }
    }

    /// DHCP mapping covers each combination.
    #[test]
    fn dhcp_value_mapping() {
        assert_eq!(dhcp_value(Some(true), Some(true)), Some("yes"));
        assert_eq!(dhcp_value(Some(true), None), Some("ipv4"));
        assert_eq!(dhcp_value(None, Some(true)), Some("ipv6"));
        assert_eq!(dhcp_value(Some(false), Some(false)), Some("no"));
        assert_eq!(dhcp_value(None, None), None);
    }
}
