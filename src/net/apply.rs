//! `lane net apply` — the additive reconcile renderer (ADR-0003 §Decision item 3,
//! "Renderer"; §Sequencing P1).
//!
//! Takes a **desired** [`NetworkDocument`] (the model lane owns, e.g. read from a
//! committed `--profile` file) and the **current** adopted host model, and computes
//! an ordered [`ReconcilePlan`] of `nmcli` operations that would make the host match
//! the desired model. The plan is then either printed (`--dry-run`, the **default**)
//! or executed (`--apply`) via [`crate::osutil::run_privileged`].
//!
//! # The additive discipline (SAFETY-CRITICAL)
//!
//! The reconcile is **purely additive** (ADR §3, §Consequences "never flush
//! addresses it does not own"):
//!
//! - For each unit in the DESIRED model, the plan emits an [`NmcliOp::Add`] (when no
//!   current connection matches its stable key) or an [`NmcliOp::Modify`] (only the
//!   properties that differ).
//! - It **NEVER** emits a delete or flush of any connection that is not in the
//!   desired model. There is no delete operation in the P1 plan at all — units lane
//!   does not own are left completely untouched. (Deletion of owned-but-removed
//!   units is explicitly out of scope for P1.)
//! - Matching a desired unit to a current connection is by the **stable key**
//!   (`networkmanager.name` connection id, falling back to `match.name` interface),
//!   **NEVER** the regenerated NM UUID (ADR §3).
//!
//! # Layering (pure planning always built; live apply gated)
//!
//! - [`reconcile`], the per-unit property projection, [`is_runtime_bridge`] and the
//!   [`NmcliOp::to_argv`] rendering are **pure** (no host access): built and
//!   unit-tested in every build. This is the always-built planning core — the heart
//!   of P1.
//! - Only [`apply_plan`] (which spawns `nmcli` through `run_privileged`) takes the
//!   `hostnet` gate, mirroring [`crate::net::adopt`]'s "pure parser always built,
//!   live reader gated" split.
//!
//! # Secrets
//!
//! A unit carrying a [`SecretRef`] (a Wi-Fi PSK/802.1x credential) renders the
//! credential property with the placeholder token [`SECRET_PLACEHOLDER`] in the
//! plan — the **real** value is resolved at apply time from `secretd` (env-ctl) and
//! is NEVER embedded in the plan text, so a dry-run can never print secret material.

use std::collections::BTreeMap;

use crate::net::model::{BridgeUnit, EthernetUnit, NetworkDocument, Route, SecretRef, WifiUnit};

/// The placeholder a [`SecretRef`] renders to in a plan. The real credential is
/// resolved at apply time from `secretd` (env-ctl); it is **never** embedded in the
/// plan text, so a dry-run never prints secret material.
pub const SECRET_PLACEHOLDER: &str = "<resolved-at-apply>";

/// One `nmcli` operation in a [`ReconcilePlan`].
///
/// The argument vectors are built by [`NmcliOp::to_argv`] — **never** shell strings
/// or interpolated commands (same argv-only discipline as [`crate::net::adopt`]'s
/// fixed nmcli arg allowlist). There is **no delete/flush variant**: the P1
/// reconcile is additive-only (ADR §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NmcliOp {
    /// Create a new connection that the desired model has but the host lacks.
    /// `con_name` is the stable connection id; `nm_type` the nmcli connection type
    /// (`ethernet`/`wifi`/`bridge`); `ifname` the bound interface (if any); `sets`
    /// the ordered `property value` pairs to apply on creation.
    Add {
        /// Stable connection id (`con-name`).
        con_name: String,
        /// nmcli connection type (`ethernet` / `wifi` / `bridge`).
        nm_type: String,
        /// Bound interface name, if the unit has a `match.name`.
        ifname: Option<String>,
        /// Ordered `(property, value)` pairs set on creation.
        sets: Vec<(String, String)>,
    },
    /// Modify an existing connection: set only the properties that differ from the
    /// current host state. `sets` is non-empty (an empty modify is never emitted).
    Modify {
        /// Stable connection id of the existing connection.
        con_name: String,
        /// Ordered `(property, value)` pairs to change.
        sets: Vec<(String, String)>,
    },
}

impl NmcliOp {
    /// Render this op to the exact `nmcli` argument vector it represents.
    ///
    /// - [`NmcliOp::Add`] → `connection add type <T> con-name <N> [ifname <I>]
    ///   <prop> <value> …`
    /// - [`NmcliOp::Modify`] → `connection modify <N> <prop> <value> …`
    ///
    /// No shell, no interpolation: each token is a distinct argv element.
    pub fn to_argv(&self) -> Vec<String> {
        match self {
            NmcliOp::Add {
                con_name,
                nm_type,
                ifname,
                sets,
            } => {
                let mut argv = vec![
                    "connection".to_string(),
                    "add".to_string(),
                    "type".to_string(),
                    nm_type.clone(),
                    "con-name".to_string(),
                    con_name.clone(),
                ];
                if let Some(iface) = ifname {
                    argv.push("ifname".to_string());
                    argv.push(iface.clone());
                }
                for (prop, value) in sets {
                    argv.push(prop.clone());
                    argv.push(value.clone());
                }
                argv
            }
            NmcliOp::Modify { con_name, sets } => {
                let mut argv = vec![
                    "connection".to_string(),
                    "modify".to_string(),
                    con_name.clone(),
                ];
                for (prop, value) in sets {
                    argv.push(prop.clone());
                    argv.push(value.clone());
                }
                argv
            }
        }
    }

    /// The stable connection id this op targets.
    pub fn con_name(&self) -> &str {
        match self {
            NmcliOp::Add { con_name, .. } | NmcliOp::Modify { con_name, .. } => con_name,
        }
    }
}

/// An ordered, additive plan of `nmcli` operations.
///
/// Produced by [`reconcile`]; printed verbatim on a dry-run and executed op-by-op,
/// fail-closed, on `--apply`. An **empty** plan means the host already matches the
/// desired model (idempotence).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcilePlan {
    /// The ordered operations. Empty ⇒ nothing to do.
    pub ops: Vec<NmcliOp>,
}

impl ReconcilePlan {
    /// Whether the plan is empty (host already matches desired — idempotent).
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Render the plan as one `nmcli …` line per op, for human/machine display.
    /// Secret material is never present (it is already a [`SECRET_PLACEHOLDER`]).
    pub fn render_text(&self) -> String {
        if self.ops.is_empty() {
            return "# no changes — host already matches desired model\n".to_string();
        }
        let mut out = String::new();
        for op in &self.ops {
            out.push_str("nmcli ");
            out.push_str(&op.to_argv().join(" "));
            out.push('\n');
        }
        out
    }
}

/// Whether an interface/connection name belongs to a runtime manager (Docker,
/// libvirt, container/VM bridges and virtual ethernet pairs) — so the reconcile
/// leaves it entirely untouched (ADR-0003 P0b pre-decision (a)).
///
/// The host adopter still *lists* these (they exist on the box), but the reconcile
/// must never plan a change against a Docker/libvirt-owned bridge or a `veth` pair:
/// those are owned by their runtime, not by lane's host plane. Matches `lo`,
/// `docker0`, `virbr*`, `br-*` and `veth*` by name pattern.
pub fn is_runtime_bridge(name: &str) -> bool {
    name == "lo"
        || name == "docker0"
        || name.starts_with("virbr")
        || name.starts_with("br-")
        || name.starts_with("veth")
}

/// The NM **bookkeeping** passthrough keys that are NOT semantically significant for
/// diffing — NM-injected defaults that carry no host intent. Normalizing these out
/// is what makes re-applying an unchanged unit yield an **empty** plan (idempotence).
///
/// Each entry is justified as an NM-default the renderer never needs to assert:
/// - `ipv4.may-fail` / `ipv6.may-fail` — NM connectivity-wait bookkeeping (defaults
///   to `yes`); the address/method properties carry the real intent.
/// - `ipv4.dhcp-send-hostname-deprecated` / `ipv6.dhcp-send-hostname-deprecated` —
///   NM renamed `dhcp-send-hostname`; the `-deprecated` alias is pure NM
///   bookkeeping mirrored from the live one and is never authored by lane.
///
/// The bias is **lossless**: a key is normalized out ONLY when it can be justified
/// as an NM-injected default. Semantically-significant keys MUST still diff and
/// round-trip (`ipv4.never-default`, `ipv6.method`, addresses, routes, `*.key-mgmt`,
/// dhcp) — they are deliberately absent from this set.
const BOOKKEEPING_PASSTHROUGH_KEYS: &[&str] = &[
    "ipv4.may-fail",
    "ipv6.may-fail",
    "ipv4.dhcp-send-hostname-deprecated",
    "ipv6.dhcp-send-hostname-deprecated",
];

/// Whether `key` is an NM bookkeeping passthrough key normalized out of diffs.
fn is_bookkeeping_key(key: &str) -> bool {
    BOOKKEEPING_PASSTHROUGH_KEYS.contains(&key)
}

/// The stable reconcile key for a unit: the NM connection name
/// (`networkmanager.name`) when present, else the `match.name` interface. **Never**
/// the regenerated NM UUID (ADR §3). Returns `None` for a unit with neither (it
/// cannot be stably keyed, so it is skipped).
fn ethernet_key(unit: &EthernetUnit) -> Option<String> {
    unit.networkmanager
        .as_ref()
        .and_then(|nm| nm.name.clone())
        .or_else(|| unit.match_rule.as_ref().and_then(|m| m.name.clone()))
}

/// The stable reconcile key for a Wi-Fi unit (see [`ethernet_key`]).
fn wifi_key(unit: &WifiUnit) -> Option<String> {
    unit.networkmanager
        .as_ref()
        .and_then(|nm| nm.name.clone())
        .or_else(|| unit.match_rule.as_ref().and_then(|m| m.name.clone()))
}

/// The stable reconcile key for a bridge unit (bridges have no `match`, so only the
/// NM connection name).
fn bridge_key(unit: &BridgeUnit) -> Option<String> {
    unit.networkmanager.as_ref().and_then(|nm| nm.name.clone())
}

/// The bound interface name of a unit (`match.name`), if any — used as `ifname` on
/// `connection add`.
fn ethernet_ifname(unit: &EthernetUnit) -> Option<String> {
    unit.match_rule.as_ref().and_then(|m| m.name.clone())
}

/// As [`ethernet_ifname`] for a Wi-Fi unit.
fn wifi_ifname(unit: &WifiUnit) -> Option<String> {
    unit.match_rule.as_ref().and_then(|m| m.name.clone())
}

/// Render the model's routes to the nmcli `ipvX.routes` property value (the
/// space-separated `to via metric` form nmcli accepts), keyed by address family.
/// Returns `(ipv4_routes, ipv6_routes)` value strings, each empty when none apply.
fn routes_to_nmcli(routes: &[Route]) -> (String, String) {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for r in routes {
        // nmcli route value: "<dest> <gateway> [metric]". netplan "default" maps to
        // the family-appropriate default prefix.
        let dest = if r.to == "default" {
            "0.0.0.0/0".to_string()
        } else {
            r.to.clone()
        };
        let is_v6 = r.to.contains(':') || r.via.as_deref().is_some_and(|g| g.contains(':'));
        let mut entry = dest;
        if let Some(via) = &r.via {
            entry.push(' ');
            entry.push_str(via);
            if let Some(metric) = r.metric {
                entry.push(' ');
                entry.push_str(&metric.to_string());
            }
        }
        if is_v6 {
            v6.push(entry);
        } else {
            // A literal "default" with no via stays the IPv4 default prefix.
            v4.push(entry);
        }
    }
    (v4.join(", "), v6.join(", "))
}

/// Flatten one ethernet unit into its canonical, **diff-ready** nmcli property map.
///
/// This is the projection both desired and current units pass through before diffing:
/// it pulls the structured fields (addresses, dhcp, routes) AND the lossless
/// passthrough into a single `property → value` map, then **normalizes out** the NM
/// bookkeeping keys ([`is_bookkeeping_key`]) so re-applying an unchanged unit yields
/// an empty diff. Semantically-significant passthrough (`ipv4.never-default`,
/// `ipv6.method`, …) is kept verbatim.
fn ethernet_properties(unit: &EthernetUnit) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();

    if !unit.addresses.is_empty() {
        props.insert("ipv4.addresses".to_string(), unit.addresses.join(", "));
    }
    if let Some(dhcp4) = unit.dhcp4 {
        props.insert(
            "ipv4.method".to_string(),
            if dhcp4 { "auto" } else { "manual" }.to_string(),
        );
    }
    if let Some(dhcp6) = unit.dhcp6 {
        props.insert(
            "ipv6.method".to_string(),
            if dhcp6 { "auto" } else { "manual" }.to_string(),
        );
    }
    let (v4_routes, v6_routes) = routes_to_nmcli(&unit.routes);
    if !v4_routes.is_empty() {
        props.insert("ipv4.routes".to_string(), v4_routes);
    }
    if !v6_routes.is_empty() {
        props.insert("ipv6.routes".to_string(), v6_routes);
    }

    // The lossless passthrough carries the rest (never-default, ipv6.method when set
    // by the host, etc). It is layered AFTER the structured fields so an explicit
    // host `ipv6.method: link-local` overrides a derived dhcp6 method.
    if let Some(nm) = &unit.networkmanager {
        for (k, v) in &nm.passthrough {
            props.insert(k.clone(), v.clone());
        }
    }

    // Normalize NM bookkeeping out so an unchanged unit diffs to nothing.
    props.retain(|k, _| !is_bookkeeping_key(k));
    props
}

/// Flatten a Wi-Fi unit into its canonical nmcli property map (see
/// [`ethernet_properties`]). Adds `ssid` / `key-mgmt` and a credential property
/// whose value is the [`SECRET_PLACEHOLDER`] when the access point carries a
/// [`SecretRef`] — the real secret is resolved at apply time, never in the plan.
fn wifi_properties(unit: &WifiUnit) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();

    if !unit.addresses.is_empty() {
        props.insert("ipv4.addresses".to_string(), unit.addresses.join(", "));
    }
    if let Some(dhcp4) = unit.dhcp4 {
        props.insert(
            "ipv4.method".to_string(),
            if dhcp4 { "auto" } else { "manual" }.to_string(),
        );
    }
    if let Some(dhcp6) = unit.dhcp6 {
        props.insert(
            "ipv6.method".to_string(),
            if dhcp6 { "auto" } else { "manual" }.to_string(),
        );
    }
    let (v4_routes, v6_routes) = routes_to_nmcli(&unit.routes);
    if !v4_routes.is_empty() {
        props.insert("ipv4.routes".to_string(), v4_routes);
    }
    if !v6_routes.is_empty() {
        props.insert("ipv6.routes".to_string(), v6_routes);
    }

    // Access points: ssid + key-mgmt + (placeholder) credential. The estate's APs
    // are OWE/open (no secret); a PSK/EAP AP renders only the placeholder.
    for (ssid, ap) in &unit.access_points {
        props.insert("802-11-wireless.ssid".to_string(), ssid.clone());
        if let Some(km) = &ap.key_mgmt {
            props.insert("802-11-wireless-security.key-mgmt".to_string(), km.clone());
        }
        if let Some(secret) = &ap.password {
            props.insert(
                secret_property(ap.key_mgmt.as_deref()),
                placeholder_for(secret),
            );
        }
    }

    if let Some(nm) = &unit.networkmanager {
        for (k, v) in &nm.passthrough {
            props.insert(k.clone(), v.clone());
        }
    }

    props.retain(|k, _| !is_bookkeeping_key(k));
    props
}

/// Flatten a bridge unit into its canonical nmcli property map (see
/// [`ethernet_properties`]). Adds the member-interface list.
fn bridge_properties(unit: &BridgeUnit) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();

    if !unit.interfaces.is_empty() {
        props.insert("bridge.interfaces".to_string(), unit.interfaces.join(", "));
    }
    if !unit.addresses.is_empty() {
        props.insert("ipv4.addresses".to_string(), unit.addresses.join(", "));
    }
    if let Some(dhcp4) = unit.dhcp4 {
        props.insert(
            "ipv4.method".to_string(),
            if dhcp4 { "auto" } else { "manual" }.to_string(),
        );
    }
    if let Some(dhcp6) = unit.dhcp6 {
        props.insert(
            "ipv6.method".to_string(),
            if dhcp6 { "auto" } else { "manual" }.to_string(),
        );
    }
    let (v4_routes, v6_routes) = routes_to_nmcli(&unit.routes);
    if !v4_routes.is_empty() {
        props.insert("ipv4.routes".to_string(), v4_routes);
    }
    if !v6_routes.is_empty() {
        props.insert("ipv6.routes".to_string(), v6_routes);
    }

    if let Some(nm) = &unit.networkmanager {
        for (k, v) in &nm.passthrough {
            props.insert(k.clone(), v.clone());
        }
    }

    props.retain(|k, _| !is_bookkeeping_key(k));
    props
}

/// The nmcli credential property a Wi-Fi `key-mgmt` requires (mirrors
/// [`crate::net::adopt`]'s mapping). Defaults to the PSK slot.
fn secret_property(key_mgmt: Option<&str>) -> String {
    match key_mgmt {
        Some("wpa-eap" | "ieee8021x" | "wpa-eap-suite-b-192") => "802-1x.password".to_string(),
        _ => "802-11-wireless-security.psk".to_string(),
    }
}

/// The placeholder a [`SecretRef`] renders to in the plan. NEVER the real value —
/// the `secretd` integration resolves it at apply time (env-ctl). The reference key
/// is intentionally not embedded either, so plan text carries no secret material.
fn placeholder_for(_secret: &SecretRef) -> String {
    SECRET_PLACEHOLDER.to_string()
}

/// Compute the additive [`ReconcilePlan`] that makes `current` match `desired`.
///
/// Pure (no host access) — the heart of P1. For each unit in `desired`:
/// - keyed by its stable key (NM name, else `match.name`; never the UUID),
/// - if `current` has no matching connection → an [`NmcliOp::Add`],
/// - else → an [`NmcliOp::Modify`] of only the properties whose value differs (an
///   empty modify is dropped, giving idempotence).
///
/// **It never emits a delete/flush.** Connections present in `current` but absent
/// from `desired` produce NO ops — they are left untouched (the load-bearing
/// additive guarantee). Runtime-managed interfaces ([`is_runtime_unit`]) are
/// excluded from **both** the desired and current sides before diffing, so the
/// reconcile never plans against Docker/libvirt-owned bridges and a faithful
/// `adopt | apply` self-diff of an unchanged host is empty (idempotent).
pub fn reconcile(desired: &NetworkDocument, current: &NetworkDocument) -> ReconcilePlan {
    // Build the current view: key → properties, EXCLUDING runtime-managed bridges.
    let current_index = current_property_index(current);

    let mut ops = Vec::new();

    // Ethernets.
    for unit in desired.network.ethernets.values() {
        let Some(key) = ethernet_key(unit) else {
            continue;
        };
        let ifname = ethernet_ifname(unit);
        // Symmetric runtime-exclusion: never PLAN a change against a Docker/libvirt
        // -owned interface, just as `current_property_index` excludes them from the
        // current view. Without this, a faithful `adopt | apply` self-diff would
        // spuriously `Add` runtime bridges (adopted into desired, absent from the
        // runtime-excluded current view) — breaking whole-host idempotence.
        if is_runtime_unit(&key, ifname.as_deref()) {
            continue;
        }
        let desired_props = ethernet_properties(unit);
        push_op(
            &mut ops,
            &current_index,
            key,
            "ethernet",
            ifname,
            desired_props,
        );
    }

    // Wi-Fi.
    for unit in desired.network.wifis.values() {
        let Some(key) = wifi_key(unit) else {
            continue;
        };
        let ifname = wifi_ifname(unit);
        if is_runtime_unit(&key, ifname.as_deref()) {
            continue;
        }
        let desired_props = wifi_properties(unit);
        push_op(&mut ops, &current_index, key, "wifi", ifname, desired_props);
    }

    // Bridges.
    for unit in desired.network.bridges.values() {
        let Some(key) = bridge_key(unit) else {
            continue;
        };
        if is_runtime_unit(&key, None) {
            continue;
        }
        let desired_props = bridge_properties(unit);
        push_op(&mut ops, &current_index, key, "bridge", None, desired_props);
    }

    ReconcilePlan { ops }
}

/// A unit is runtime-managed when its stable key OR its bound interface name matches
/// [`is_runtime_bridge`] (Docker/libvirt bridges, `veth` pairs, loopback). Such units
/// are excluded from BOTH the desired and current sides of the reconcile so lane never
/// plans a change against an interface another runtime owns.
///
/// `pub(crate)` so the P2 profile writer ([`crate::net::profile`]) can strip the same
/// runtime-managed units out of a committed per-host profile (docker0/virbr0/br-*/veth*
/// are recreated by Docker/libvirt on a fresh box, never by lane).
pub(crate) fn is_runtime_unit(key: &str, ifname: Option<&str>) -> bool {
    is_runtime_bridge(key) || ifname.is_some_and(is_runtime_bridge)
}

/// Index the CURRENT model into `stable key → property map`, excluding
/// runtime-managed interfaces ([`is_runtime_bridge`]) so the reconcile never plans
/// against a Docker/libvirt-owned bridge or a `veth` pair.
fn current_property_index(current: &NetworkDocument) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut index = BTreeMap::new();

    for unit in current.network.ethernets.values() {
        if let Some(key) = ethernet_key(unit) {
            if is_runtime_unit(&key, ethernet_ifname(unit).as_deref()) {
                continue;
            }
            index.insert(key, ethernet_properties(unit));
        }
    }
    for unit in current.network.wifis.values() {
        if let Some(key) = wifi_key(unit) {
            if is_runtime_unit(&key, wifi_ifname(unit).as_deref()) {
                continue;
            }
            index.insert(key, wifi_properties(unit));
        }
    }
    for unit in current.network.bridges.values() {
        if let Some(key) = bridge_key(unit) {
            if is_runtime_unit(&key, None) {
                continue;
            }
            index.insert(key, bridge_properties(unit));
        }
    }

    index
}

/// Push the additive op for one desired unit: an `Add` if the key is absent from the
/// current index, or a `Modify` of only the differing properties (no op when nothing
/// differs). Never a delete.
fn push_op(
    ops: &mut Vec<NmcliOp>,
    current_index: &BTreeMap<String, BTreeMap<String, String>>,
    key: String,
    nm_type: &str,
    ifname: Option<String>,
    desired_props: BTreeMap<String, String>,
) {
    match current_index.get(&key) {
        None => {
            // No matching current connection → create it. `sets` is the full
            // desired property set, in deterministic (BTreeMap) order.
            ops.push(NmcliOp::Add {
                con_name: key,
                nm_type: nm_type.to_string(),
                ifname,
                sets: desired_props.into_iter().collect(),
            });
        }
        Some(current_props) => {
            // Modify only the properties whose value differs (or is newly set).
            let mut sets = Vec::new();
            for (prop, desired_value) in &desired_props {
                if current_props.get(prop) != Some(desired_value) {
                    sets.push((prop.clone(), desired_value.clone()));
                }
            }
            // Empty diff ⇒ no op (idempotence). NEVER emit a delete for properties
            // present in current but absent from desired — additive only.
            if !sets.is_empty() {
                ops.push(NmcliOp::Modify {
                    con_name: key,
                    sets,
                });
            }
        }
    }
}

// --- live apply (feature-gated) --------------------------------------------

/// Execute a [`ReconcilePlan`] against the host, op-by-op, **fail-closed**.
///
/// Each op runs via `nmcli` through [`crate::osutil::run_privileged`] (root direct,
/// else `sudo`). Execution **stops on the first error** — it never continues past a
/// failure, and the plan contains no destructive verb (additive-only by
/// construction). Returns `Ok(())` only when every op succeeded.
///
/// NOTE: this is the mutating path; a plan with a [`SECRET_PLACEHOLDER`] in it is
/// NOT yet runnable verbatim — the `secretd` resolution (env-ctl) substitutes the
/// real credential at this seam before the op is executed. P1 leaves that seam
/// documented; the live cognitum-seed unit is OWE/secret-free so it does not hit it.
#[cfg(feature = "hostnet")]
pub fn apply_plan(plan: &ReconcilePlan) -> anyhow::Result<()> {
    if !crate::osutil::command_exists("nmcli") {
        anyhow::bail!(
            "`nmcli` not found on PATH — `lane net apply` renders the host plane via \
             NetworkManager; install NetworkManager or run on a NM-managed host"
        );
    }

    for op in &plan.ops {
        let argv = op.to_argv();
        // A plan carrying an unresolved secret placeholder must not be executed
        // verbatim — fail closed (the secretd seam resolves it before apply).
        if argv.iter().any(|a| a == SECRET_PLACEHOLDER) {
            anyhow::bail!(
                "plan op for connection {:?} carries an unresolved secret placeholder; \
                 secretd resolution (env-ctl) is required before live apply (ADR-0003)",
                op.con_name()
            );
        }
        let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (output, result) = crate::osutil::run_privileged("nmcli", &arg_refs);
        if let Err(e) = result {
            // Fail closed: stop on the first error, surface nmcli's combined output.
            let detail = String::from_utf8_lossy(&output);
            anyhow::bail!("nmcli {:?} failed ({e}): {}", argv, detail.trim());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::{MatchRule, Network, NmPassthrough};

    /// Build the cognitum-seed link-local ethernet unit (the adoption snapshot
    /// shape) as a model document with one connection.
    fn cognitum_seed_doc() -> NetworkDocument {
        let mut passthrough = BTreeMap::new();
        passthrough.insert("ipv4.never-default".to_string(), "true".to_string());
        passthrough.insert("ipv6.method".to_string(), "link-local".to_string());

        let eth = EthernetUnit {
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

    /// Test 1 — idempotence: diffing the SAME model as both desired and current
    /// yields an EMPTY plan (proves additive + bookkeeping normalization).
    #[test]
    fn idempotence_self_diff_is_empty() {
        let doc = cognitum_seed_doc();
        let plan = reconcile(&doc, &doc);
        assert!(
            plan.is_empty(),
            "re-applying an unchanged model must yield an empty plan, got {:?}",
            plan.ops
        );
    }

    /// Test 2 — add_new_unit: a desired unit absent from current → exactly one Add
    /// op with the right argv; current units absent from desired produce NO ops.
    #[test]
    fn add_new_unit_only_touches_desired() {
        let desired = cognitum_seed_doc();
        // Current has a DIFFERENT, lane-unowned connection. Desired's unit is absent
        // from current → one Add; current's unowned unit produces nothing.
        let mut other = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: None,
            }),
            dhcp4: Some(true),
            networkmanager: Some(NmPassthrough {
                name: Some("netplan-eno1".to_string()),
                ..NmPassthrough::default()
            }),
            ..EthernetUnit::default()
        };
        other.dhcp6 = Some(true);
        let mut cur_net = Network::v2();
        cur_net.ethernets.insert("NM-other".to_string(), other);
        let current = NetworkDocument::new(cur_net);

        let plan = reconcile(&desired, &current);
        assert_eq!(
            plan.ops.len(),
            1,
            "exactly one op: the Add for the desired unit"
        );
        match &plan.ops[0] {
            NmcliOp::Add {
                con_name,
                nm_type,
                ifname,
                sets,
            } => {
                assert_eq!(con_name, "cognitum-seed-linklocal");
                assert_eq!(nm_type, "ethernet");
                assert_eq!(ifname.as_deref(), Some("enxead865c61ec9"));
                // The argv carries the address + never-default + ipv6 method.
                let argv = plan.ops[0].to_argv();
                assert!(argv.contains(&"ipv4.addresses".to_string()));
                assert!(argv.contains(&"169.254.42.2/24".to_string()));
                assert!(argv.contains(&"ipv4.never-default".to_string()));
                assert!(!sets.is_empty());
            }
            other => panic!("expected an Add op, got {other:?}"),
        }
        // The unowned `netplan-eno1` is never referenced.
        assert!(plan.ops.iter().all(|op| op.con_name() != "netplan-eno1"));
    }

    /// Test 3 — modify_changed_field: desired changes one address / adds
    /// never-default on an existing unit → exactly one Modify op touching only that
    /// property; unrelated properties produce no op.
    #[test]
    fn modify_changed_field_touches_only_the_diff() {
        let current = cognitum_seed_doc();

        // Desired = current but with a changed address.
        let mut desired = cognitum_seed_doc();
        desired
            .network
            .ethernets
            .get_mut("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769")
            .unwrap()
            .addresses = vec!["169.254.42.9/24".to_string()];

        let plan = reconcile(&desired, &current);
        assert_eq!(plan.ops.len(), 1, "one Modify op for the changed address");
        match &plan.ops[0] {
            NmcliOp::Modify { con_name, sets } => {
                assert_eq!(con_name, "cognitum-seed-linklocal");
                assert_eq!(sets.len(), 1, "only the address changed, got {sets:?}");
                assert_eq!(sets[0].0, "ipv4.addresses");
                assert_eq!(sets[0].1, "169.254.42.9/24");
            }
            other => panic!("expected a Modify op, got {other:?}"),
        }
    }

    /// Test 4 — never_flushes_unowned (load-bearing safety): current has 3
    /// connections, desired has 1 → the plan has zero delete/flush ops and only
    /// touches the 1 desired unit.
    #[test]
    fn never_flushes_unowned_connections() {
        let desired = cognitum_seed_doc();

        // Current: the cognitum-seed unit PLUS two unowned connections.
        let mut current = cognitum_seed_doc();
        let mut a = EthernetUnit {
            networkmanager: Some(NmPassthrough {
                name: Some("Wired connection 1".to_string()),
                ..NmPassthrough::default()
            }),
            dhcp4: Some(true),
            ..EthernetUnit::default()
        };
        a.match_rule = Some(MatchRule {
            name: Some("enp5s0".to_string()),
            macaddress: None,
        });
        let b = WifiUnit {
            networkmanager: Some(NmPassthrough {
                name: Some("FlexNetOS".to_string()),
                ..NmPassthrough::default()
            }),
            dhcp4: Some(true),
            ..WifiUnit::default()
        };
        current.network.ethernets.insert("NM-a".to_string(), a);
        current.network.wifis.insert("NM-b".to_string(), b);

        let plan = reconcile(&desired, &current);
        // NmcliOp has no delete/flush variant by construction — assert that and that
        // the plan is empty (the one desired unit already matches current).
        assert!(
            plan.ops
                .iter()
                .all(|op| matches!(op, NmcliOp::Add { .. } | NmcliOp::Modify { .. })),
            "the P1 plan must contain only additive ops"
        );
        // Self-matching desired unit → empty; the two unowned units are untouched.
        assert!(
            plan.is_empty(),
            "desired matches current; unowned units must not produce ops, got {:?}",
            plan.ops
        );
        // And neither unowned connection is ever named in any op.
        assert!(plan
            .ops
            .iter()
            .all(|op| op.con_name() != "Wired connection 1" && op.con_name() != "FlexNetOS"));
    }

    /// Test 5 — bookkeeping_keys_normalized_out: two units identical except for
    /// `ipv4.may-fail` / `dhcp-send-hostname-deprecated` → empty diff; but a
    /// differing `ipv4.never-default` → a Modify op (significance preserved).
    #[test]
    fn bookkeeping_keys_normalized_out_but_significance_preserved() {
        // Current carries NM bookkeeping defaults in its passthrough.
        let mut current = cognitum_seed_doc();
        {
            let nm = current
                .network
                .ethernets
                .get_mut("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769")
                .unwrap()
                .networkmanager
                .as_mut()
                .unwrap();
            nm.passthrough
                .insert("ipv4.may-fail".to_string(), "yes".to_string());
            nm.passthrough.insert(
                "ipv4.dhcp-send-hostname-deprecated".to_string(),
                "yes".to_string(),
            );
        }

        // Desired has NO bookkeeping keys but is otherwise identical → empty diff.
        let desired = cognitum_seed_doc();
        let plan = reconcile(&desired, &current);
        assert!(
            plan.is_empty(),
            "bookkeeping-only difference must normalize to an empty plan, got {:?}",
            plan.ops
        );

        // Now flip a SEMANTICALLY significant key → it must diff.
        let mut desired2 = cognitum_seed_doc();
        desired2
            .network
            .ethernets
            .get_mut("NM-70b82336-d3cd-4204-90aa-fe8a1ed5e769")
            .unwrap()
            .networkmanager
            .as_mut()
            .unwrap()
            .passthrough
            .insert("ipv4.never-default".to_string(), "false".to_string());
        let plan2 = reconcile(&desired2, &current);
        assert_eq!(
            plan2.ops.len(),
            1,
            "never-default change must produce an op"
        );
        match &plan2.ops[0] {
            NmcliOp::Modify { sets, .. } => {
                assert_eq!(sets.len(), 1);
                assert_eq!(sets[0].0, "ipv4.never-default");
                assert_eq!(sets[0].1, "false");
            }
            other => panic!("expected a Modify, got {other:?}"),
        }
    }

    /// Test 6 — runtime_bridges_excluded: a current model containing
    /// docker0/virbr0/br-x → the reconcile-current view excludes them, so a desired
    /// connection of the same name is treated as ADD (not a no-op match), proving
    /// they were excluded from the current index.
    #[test]
    fn runtime_bridges_excluded_from_current_view() {
        assert!(is_runtime_bridge("lo"));
        assert!(is_runtime_bridge("docker0"));
        assert!(is_runtime_bridge("virbr0"));
        assert!(is_runtime_bridge("br-abc123"));
        assert!(is_runtime_bridge("veth123ab"));
        assert!(!is_runtime_bridge("eno1"));
        assert!(!is_runtime_bridge("enxead865c61ec9"));

        // A current model whose only connections are runtime bridges.
        let mut net = Network::v2();
        for name in ["docker0", "virbr0", "br-xyz"] {
            let bridge = BridgeUnit {
                networkmanager: Some(NmPassthrough {
                    name: Some(name.to_string()),
                    ..NmPassthrough::default()
                }),
                ..BridgeUnit::default()
            };
            net.bridges.insert(format!("NM-{name}"), bridge);
        }
        let current = NetworkDocument::new(net);
        let index = current_property_index(&current);
        assert!(
            index.is_empty(),
            "runtime-managed bridges must be excluded from the reconcile-current view, got {:?}",
            index.keys().collect::<Vec<_>>()
        );

        // Symmetric (produce-side) exclusion: a faithful adopt→apply self-diff of a
        // host whose only connections are runtime bridges MUST be empty — the desired
        // side must drop them too, not just the current side. (Regression guard for the
        // asymmetric-exclusion bug that planned spurious `Add docker0/virbr0/br-*`.)
        let plan = reconcile(&current, &current);
        assert!(
            plan.is_empty(),
            "self-diff over runtime bridges must be idempotent (empty), got {} op(s): {}",
            plan.ops.len(),
            plan.render_text()
        );
        // And they are still skipped even against an empty current (no spurious Add).
        let plan_vs_empty = reconcile(&current, &NetworkDocument::new(Network::v2()));
        assert!(
            plan_vs_empty.is_empty(),
            "runtime bridges in desired must never be planned as Add, got: {}",
            plan_vs_empty.render_text()
        );
    }

    /// Test 7 — dry_run_plan_has_no_secret_material: a unit with a SecretRef → the
    /// rendered plan text contains no literal secret, only the placeholder.
    #[test]
    fn dry_run_plan_has_no_secret_material() {
        use crate::net::model::AccessPoint;

        let mut access_points = BTreeMap::new();
        access_points.insert(
            "FlexNetOS".to_string(),
            AccessPoint {
                key_mgmt: Some("wpa-psk".to_string()),
                // The secretd reference key is itself never the material; even so,
                // the plan must render the PLACEHOLDER, not the ref key.
                password: Some(SecretRef::new("cognitum-seed/wifi-psk")),
            },
        );
        let wifi = WifiUnit {
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
        let desired = NetworkDocument::new(net);

        // Diff against an empty current → an Add op containing the credential prop.
        let plan = reconcile(&desired, &NetworkDocument::new(Network::v2()));
        let text = plan.render_text();

        assert!(
            text.contains(SECRET_PLACEHOLDER),
            "the credential property must render the placeholder, got:\n{text}"
        );
        // No literal secret material and not even the ref key leaks into the plan.
        assert!(
            !text.contains("hunter2"),
            "no literal secret may appear in the plan text"
        );
        assert!(
            !text.contains("cognitum-seed/wifi-psk"),
            "the secretd ref key must not appear in the plan text either"
        );
        // The key-mgmt (non-secret) IS present — it is semantically significant.
        assert!(text.contains("wpa-psk"));
    }

    /// `to_argv` builds the exact nmcli argument vectors (add vs modify).
    #[test]
    fn to_argv_shapes() {
        let add = NmcliOp::Add {
            con_name: "cognitum-seed-linklocal".to_string(),
            nm_type: "ethernet".to_string(),
            ifname: Some("enxead865c61ec9".to_string()),
            sets: vec![("ipv4.addresses".to_string(), "169.254.42.2/24".to_string())],
        };
        assert_eq!(
            add.to_argv(),
            vec![
                "connection",
                "add",
                "type",
                "ethernet",
                "con-name",
                "cognitum-seed-linklocal",
                "ifname",
                "enxead865c61ec9",
                "ipv4.addresses",
                "169.254.42.2/24",
            ]
        );

        let modify = NmcliOp::Modify {
            con_name: "cognitum-seed-linklocal".to_string(),
            sets: vec![("ipv4.never-default".to_string(), "true".to_string())],
        };
        assert_eq!(
            modify.to_argv(),
            vec![
                "connection",
                "modify",
                "cognitum-seed-linklocal",
                "ipv4.never-default",
                "true",
            ]
        );
    }
}
