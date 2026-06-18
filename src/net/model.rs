//! The ADR-0003 host-network-plane model — a Rust-native, **lossless superset of
//! netplan v2**.
//!
//! The top-level [`NetworkDocument`] mirrors a netplan v2 file
//! (`{ network: { version: 2, renderer?, ethernets?, wifis?, bridges? } }`) and
//! is the round-trip unit for `lane net adopt` (read host → emit model) and
//! `lane net apply` (render model → host). Per the ADR's **no-downgrade
//! contract**, the model is a strict *superset* of what netplan expresses: a
//! field lane omits would be a silent skip on adoption, so the unit types model
//! the full common field set (addresses, routes, dhcp, match, nameservers,
//! wake-on-lan, …) even when a given snapshot uses only a few, plus an
//! open-ended [`NmPassthrough`] map that makes adoption lossless for *arbitrary*
//! NetworkManager keys (e.g. `ipv4.never-default`, `ipv6.method`).
//!
//! ## Why `BTreeMap` for unit maps
//!
//! netplan YAML mappings (the `ethernets:`/`wifis:`/`bridges:` blocks and the NM
//! passthrough block) are *semantically unordered*. A [`BTreeMap`] is therefore
//! semantically lossless, gives deterministic serialization (stable diffs), and
//! — unlike an insertion-ordered map — needs **no new dependency**. Units are
//! keyed by their stable **netplan unit id** (e.g. `"NM-<uuid>"`), which is the
//! identity used in the netplan file; per ADR §3 the *renderer* keys reconciles
//! by stable `match`+name, **never** by the regenerated NM UUID.
//!
//! ## Secrets are never inline
//!
//! Per ADR §1 ("Secrets are references to `secretd`, never inline"), every
//! PSK/802.1x/password field is a [`SecretRef`] — a reference to a
//! `secretd`-resolved secret. A literal secret string is **unrepresentable** in
//! this model, so the model is always safe to commit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which backend renders the model on the host. Serde spellings are netplan's
/// exact spellings (`"NetworkManager"` / `"networkd"`), preserved for fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Renderer {
    /// netplan's `renderer: NetworkManager` (the estate's renderer).
    #[serde(rename = "NetworkManager")]
    NetworkManager,
    /// netplan's `renderer: networkd` (the portability target for non-NM boxes).
    #[serde(rename = "networkd")]
    Networkd,
}

/// Top-level host-network document — mirrors a netplan v2 file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkDocument {
    /// The single `network:` mapping. netplan nests everything under this key.
    pub network: Network,
}

impl NetworkDocument {
    /// Wrap a [`Network`] in the top-level document.
    pub fn new(network: Network) -> NetworkDocument {
        NetworkDocument { network }
    }
}

/// The `network:` block: version, optional default renderer, and the unit maps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Network {
    /// netplan schema version. Always `2` for the plane lane adopts.
    pub version: u8,

    /// Default renderer for units that do not name their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,

    /// Ethernet units, keyed by stable netplan unit id (e.g. `"NM-<uuid>"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ethernets: BTreeMap<String, EthernetUnit>,

    /// Wi-Fi units, keyed by stable netplan unit id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wifis: BTreeMap<String, WifiUnit>,

    /// Bridge units, keyed by stable netplan unit id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bridges: BTreeMap<String, BridgeUnit>,
}

impl Network {
    /// A v2 `network:` block with no units and no default renderer.
    pub fn v2() -> Network {
        Network {
            version: 2,
            renderer: None,
            ethernets: BTreeMap::new(),
            wifis: BTreeMap::new(),
            bridges: BTreeMap::new(),
        }
    }
}

impl Default for Network {
    fn default() -> Network {
        Network::v2()
    }
}

/// An `ethernets:` unit — the common netplan/NM field superset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EthernetUnit {
    /// Per-unit renderer override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,

    /// Match rule selecting the physical device (by name and/or MAC).
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_rule: Option<MatchRule>,

    /// netplan `set-name`: rename the matched device to this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_name: Option<String>,

    /// Static addresses in CIDR form (e.g. `"169.254.42.2/24"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,

    /// Static routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,

    /// IPv4 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp4: Option<bool>,

    /// IPv6 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp6: Option<bool>,

    /// DNS nameservers (addresses + search domains).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nameservers: Option<Nameservers>,

    /// Wake-on-LAN toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wakeonlan: Option<bool>,

    /// Open-ended NetworkManager passthrough (the lossless escape hatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networkmanager: Option<NmPassthrough>,
}

/// A `wifis:` unit — the ethernet field superset plus `access-points`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WifiUnit {
    /// Per-unit renderer override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,

    /// Match rule selecting the physical device (by name and/or MAC).
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_rule: Option<MatchRule>,

    /// netplan `set-name`: rename the matched device to this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_name: Option<String>,

    /// Static addresses in CIDR form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,

    /// Static routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,

    /// IPv4 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp4: Option<bool>,

    /// IPv6 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp6: Option<bool>,

    /// DNS nameservers (addresses + search domains).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nameservers: Option<Nameservers>,

    /// Wake-on-LAN toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wakeonlan: Option<bool>,

    /// Open-ended NetworkManager passthrough (the lossless escape hatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networkmanager: Option<NmPassthrough>,

    /// Access points, keyed by SSID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub access_points: BTreeMap<String, AccessPoint>,
}

/// A `bridges:` unit. Bridges carry the common addressing fields plus their
/// member interface list; container/VM-runtime bridges are out of scope per the
/// adoption snapshot, but the host plane may own deliberate bridges.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BridgeUnit {
    /// Per-unit renderer override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,

    /// Member interface names bound into the bridge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,

    /// Static addresses in CIDR form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,

    /// Static routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,

    /// IPv4 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp4: Option<bool>,

    /// IPv6 DHCP toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp6: Option<bool>,

    /// DNS nameservers (addresses + search domains).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nameservers: Option<Nameservers>,

    /// Open-ended NetworkManager passthrough (the lossless escape hatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networkmanager: Option<NmPassthrough>,
}

/// netplan `match:` — selects a physical device by name and/or MAC.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MatchRule {
    /// Match by interface name (e.g. `"enxead865c61ec9"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Match by MAC address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macaddress: Option<String>,
}

/// A static route entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Route {
    /// Destination prefix (e.g. `"default"` or `"10.0.0.0/24"`).
    pub to: String,

    /// Gateway address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,

    /// Route metric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u32>,

    /// netplan `on-link`: gateway is directly reachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_link: Option<bool>,
}

/// DNS configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Nameservers {
    /// Nameserver addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,

    /// DNS search domains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search: Vec<String>,
}

/// netplan `networkmanager:` block — NM-specific identity plus the open-ended
/// `passthrough` key map.
///
/// The `passthrough` map is what makes adoption **lossless for arbitrary NM
/// keys**: any NetworkManager setting that lane has no first-class field for
/// (e.g. `ipv4.never-default: "true"`, `ipv6.method: "link-local"`) survives
/// adopt→render verbatim through this map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NmPassthrough {
    /// NM connection name (e.g. `"cognitum-seed-linklocal"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// NM connection UUID. Identity for reconciliation is `match`+name, **not**
    /// this UUID (it is regenerated), but it is preserved for fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// Open-ended NM key→value map (e.g. `ipv4.never-default: "true"`). The
    /// lossless escape hatch for arbitrary NM settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub passthrough: BTreeMap<String, String>,
}

/// A Wi-Fi access point. Carries key-management metadata and an optional
/// password that is **always** a [`SecretRef`] — never inline material.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AccessPoint {
    /// 802.11 key management (e.g. `"owe"`, `"psk"`, `"eap"`). For the adopted
    /// snapshot this is OWE/open, so no secret exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_mgmt: Option<String>,

    /// The PSK/802.1x credential — a **reference to `secretd`**, never inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SecretRef>,
}

/// A reference to a `secretd`-resolved secret.
///
/// Per ADR-0003 §1, "Secrets are references to `secretd`, never inline." This
/// newtype is the **only** way a PSK/802.1x/password is represented in the
/// model: it holds a `secretd` lookup key (e.g. `"cognitum-seed/wifi-psk"`),
/// resolved to real material only at render time by env-ctl. A literal secret
/// string is unrepresentable here, so the model is always safe to commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// The `secretd` lookup key — a reference, not the secret itself.
    pub secretd: String,
}

impl SecretRef {
    /// Build a reference to the `secretd` secret at `key`.
    pub fn new(key: impl Into<String>) -> SecretRef {
        SecretRef {
            secretd: key.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact "Example adopted unit" YAML from the host snapshot doc
    /// (`docs/adopt/host-nm-snapshot-2026-06-17.md`) — the cognitum-seed
    /// link-local ethernet. This is the round-trip acceptance fixture.
    const SNAPSHOT_UNIT_YAML: &str = r#"
network:
  version: 2
  ethernets:
    NM-<uuid>:
      renderer: NetworkManager
      match: { name: "enxead865c61ec9" }
      addresses: ["169.254.42.2/24"]
      networkmanager:
        name: "cognitum-seed-linklocal"
        passthrough: { ipv4.never-default: "true", ipv6.method: "link-local" }
"#;

    #[test]
    fn round_trip_snapshot_unit() {
        // YAML → model
        let doc: NetworkDocument = serde_yaml::from_str(SNAPSHOT_UNIT_YAML).unwrap();

        // Assert the parsed fields exactly.
        assert_eq!(doc.network.version, 2);
        assert!(doc.network.renderer.is_none());
        assert_eq!(doc.network.ethernets.len(), 1);

        let unit = doc
            .network
            .ethernets
            .get("NM-<uuid>")
            .expect("unit keyed by netplan unit id");
        assert_eq!(unit.renderer, Some(Renderer::NetworkManager));
        assert_eq!(
            unit.match_rule.as_ref().and_then(|m| m.name.as_deref()),
            Some("enxead865c61ec9")
        );
        assert_eq!(unit.addresses, vec!["169.254.42.2/24".to_string()]);

        let nm = unit.networkmanager.as_ref().expect("networkmanager block");
        assert_eq!(nm.name.as_deref(), Some("cognitum-seed-linklocal"));
        // never-default and ipv6 link-local survive via the passthrough map.
        assert_eq!(
            nm.passthrough.get("ipv4.never-default").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            nm.passthrough.get("ipv6.method").map(String::as_str),
            Some("link-local")
        );

        // model → YAML → model: semantic round-trip with no field loss.
        let serialized = serde_yaml::to_string(&doc).unwrap();
        let reparsed: NetworkDocument = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(doc, reparsed);

        // never-default / link-local still present after the round-trip.
        let nm2 = reparsed.network.ethernets["NM-<uuid>"]
            .networkmanager
            .as_ref()
            .unwrap();
        assert_eq!(
            nm2.passthrough
                .get("ipv4.never-default")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            nm2.passthrough.get("ipv6.method").map(String::as_str),
            Some("link-local")
        );
    }

    #[test]
    fn superset_fields_present() {
        let mut eth = EthernetUnit {
            match_rule: Some(MatchRule {
                name: Some("eno1".to_string()),
                macaddress: Some("10:ff:e0:b3:9e:55".to_string()),
            }),
            addresses: vec!["10.0.0.5/24".to_string()],
            dhcp4: Some(false),
            wakeonlan: Some(true),
            nameservers: Some(Nameservers {
                addresses: vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()],
                search: vec!["example.test".to_string()],
            }),
            ..EthernetUnit::default()
        };
        eth.routes.push(Route {
            to: "default".to_string(),
            via: Some("10.0.0.1".to_string()),
            metric: Some(100),
            on_link: Some(true),
        });

        let mut network = Network::v2();
        network.ethernets.insert("NM-eno1".to_string(), eth);
        let doc = NetworkDocument::new(network);

        let serialized = serde_yaml::to_string(&doc).unwrap();
        let reparsed: NetworkDocument = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(doc, reparsed);

        // Spot-check the superset fields survived the round-trip.
        let unit = &reparsed.network.ethernets["NM-eno1"];
        let route = &unit.routes[0];
        assert_eq!(route.via.as_deref(), Some("10.0.0.1"));
        assert_eq!(route.metric, Some(100));
        assert_eq!(route.on_link, Some(true));
        assert_eq!(unit.dhcp4, Some(false));
        assert_eq!(unit.wakeonlan, Some(true));
        assert_eq!(
            unit.match_rule.as_ref().unwrap().macaddress.as_deref(),
            Some("10:ff:e0:b3:9e:55")
        );
        assert_eq!(
            unit.nameservers.as_ref().unwrap().addresses,
            vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()]
        );
    }

    #[test]
    fn secret_is_reference_not_inline() {
        let ap = AccessPoint {
            key_mgmt: Some("psk".to_string()),
            password: Some(SecretRef::new("cognitum-seed/wifi-psk")),
        };

        let mut access_points = BTreeMap::new();
        access_points.insert("FlexNetOS".to_string(), ap);
        let wifi = WifiUnit {
            access_points,
            ..WifiUnit::default()
        };

        let serialized = serde_yaml::to_string(&wifi).unwrap();

        // The output references secretd by key, and contains NO literal secret.
        assert!(serialized.contains("secretd"));
        assert!(serialized.contains("cognitum-seed/wifi-psk"));
        assert!(
            !serialized.contains("hunter2"),
            "no literal secret material may appear in the model"
        );

        // And it round-trips equal: the reference is preserved, not flattened.
        let reparsed: WifiUnit = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(wifi, reparsed);
        assert_eq!(
            reparsed.access_points["FlexNetOS"]
                .password
                .as_ref()
                .map(|s| s.secretd.as_str()),
            Some("cognitum-seed/wifi-psk")
        );
    }

    #[test]
    fn renderer_spellings() {
        // NetworkManager ⇄ "NetworkManager"
        assert_eq!(
            serde_yaml::to_string(&Renderer::NetworkManager)
                .unwrap()
                .trim(),
            "NetworkManager"
        );
        assert_eq!(
            serde_yaml::from_str::<Renderer>("NetworkManager").unwrap(),
            Renderer::NetworkManager
        );

        // Networkd ⇄ "networkd" (netplan's lowercase spelling)
        assert_eq!(
            serde_yaml::to_string(&Renderer::Networkd).unwrap().trim(),
            "networkd"
        );
        assert_eq!(
            serde_yaml::from_str::<Renderer>("networkd").unwrap(),
            Renderer::Networkd
        );
    }
}
