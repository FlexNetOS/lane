# Host network snapshot — adopt-consume input (2026-06-17)

Captured from the primary RuVector workstation (`drdave-TRX50-AI-TOP`) as the **adoption input**
for [ADR-0003](../adr/ADR-0003-host-network-adopt-consume.md): the existing host network
configuration that `lane` must adopt-consume and re-express as a Rust-native, portable network
plane so the meta estate is reproducible on any box.

**Secrets are redacted.** Wi-Fi here is OWE/open (`wifi-security.key-mgmt: owe`) so no PSKs exist;
any future PSK/802.1x/`password`/`psk`/`key`/`secret` material MUST come from `secretd`
(env-ctl), never be committed. This file is **shape, not credentials**.

## Topology (the stack to adopt)
- **Source of truth:** `netplan` (`/etc/netplan/*.yaml`, `version: 2`).
- **Renderer:** `NetworkManager` (`01-network-manager-all.yaml` → `renderer: NetworkManager`).
- **Live keyfiles:** generated, volatile, under `/run/NetworkManager/system-connections/netplan-NM-*`.
- **Adoption implication:** the durable artifacts are the **`/etc/netplan/90-NM-<uuid>.yaml`** files;
  NM keyfiles are derived. `nmcli connection add/modify/delete` round-trips through netplan
  (writes/removes the `/etc/netplan` YAML). lane's model must own this netplan↔NM layering, keyed by
  stable `match: { name: <iface> }` + connection name (NOT the regenerated UUID).

## Interfaces (driver inventory)
| iface | driver | role |
|-------|--------|------|
| `eno1` | atlantic | 10G ethernet, DHCP (primary uplink) |
| `enp73s0` | atlantic | 10G ethernet (down) |
| `wlp71s0` | ath12k_wifi7_pci | Wi-Fi 7 (OWE APs) |
| `enxead865c61ec9` | cdc_ncm | **Cognitum Seed** custody NIC — static `169.254.42.2/24`, never-default (env-ctl USB-unlock factor) |
| `enx2a72dafbabc9` | rndis_host | Cognitum Seed second gadget (rndis) |
| `eno1`/virtual | virtual | `docker0`, `veth*`, `virbr0`, `br-*`, `lo` |

## NM connections (metadata only)
```
netplan-eno1            802-3-ethernet  eno1               autoconnect=yes  (DHCP)
cognitum-seed-linklocal 802-3-ethernet  enxead865c61ec9    autoconnect=yes  (static 169.254.42.2/24, never-default)
FlexNetOS               802-11-wireless  (wifi)            autoconnect=yes
TP-Link_2.4GHz_C54CB0   802-11-wireless  (wifi, OWE)       autoconnect=yes
TP-Link_6GHz_C00782     802-11-wireless  (wifi, OWE)       autoconnect=yes
TP-Link_6GHz_C54CB2     802-11-wireless  (wifi, OWE)       autoconnect=yes
Wired connection 1..4   802-3-ethernet   (unbound)         autoconnect=yes
docker0/br-*/virbr0/lo  bridge/loopback  (virtual; container/VM managed — likely OUT of scope)
```

## netplan files present
```
00-installer-config.yaml        eno1 DHCP (by macaddress 10:ff:e0:b3:9e:55, set-name eno1)
01-network-manager-all.yaml     renderer: NetworkManager
90-NM-<uuid>.yaml  ×7           one per NM connection (wifis + the cognitum-seed ethernet)
```

## Example adopted unit (the cognitum-seed-net component, env-ctl PR #115)
```yaml
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
```

## Notes for the porter
- **Out of scope (probably):** `docker0`, `veth*`, `virbr0`, `br-*` are container/VM-runtime managed —
  adopt the *host* plane (physical + special-purpose link-local), not runtime-ephemeral bridges.
- **Special-purpose link-local** (the cognitum-seed NIC) is the proof case: a non-default, additive,
  match-by-name static address that must survive reboot/replug/carrier-bounce. env-ctl owns the
  *component*; lane should own the *network-config representation* it renders to.
- Regenerate this snapshot with the sanitizing capture (no raw `/etc/netplan` dumps — they may carry
  PSK/802.1x material on other boxes).
