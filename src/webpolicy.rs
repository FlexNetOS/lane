//! Pure, deny-by-default web-egress policy validator — the always-compiled core
//! of lane's future governed-egress `lane web` seam (ADR-0001, lane↔obscura).
//!
//! This module is the **mechanism**, built fail-closed and with **no** network
//! I/O and **no** dependency on obscura. Given a requested target (a URL, or a
//! decomposed host + port + scheme) it returns an [`Allow`](PolicyDecision::Allow)
//! or a typed [`Deny`](PolicyDecision::Deny) carrying the reason. The `src/web/`
//! seam, the `lane web` CLI, and the daemon wiring are separate later tasks and
//! are intentionally absent here.
//!
//! # Deny-by-default
//!
//! Nothing is allowed unless it matches an explicit allowlist rule **and** trips
//! none of the SSRF guards. A default [`WebPolicy`] (empty allowlist) denies
//! everything. A target is allowed only when **all** hold:
//!
//! 1. the scheme is `http` or `https` (the only candidate-allowable schemes),
//! 2. the host matches an allow rule (exact host or domain suffix),
//! 3. the port is in the allowed-port set (default `{80, 443}`), and
//! 4. if the host is an IP literal, that IP trips no SSRF guard
//!    (loopback / private / link-local / unspecified / multicast / reserved).
//!
//! # SSRF guards
//!
//! Classic server-side-request-forgery protections: a governed browser must not
//! be tricked into reaching the local machine (lane's own daemon), the private
//! network, or cloud metadata endpoints. See [`DenyReason`] for the full set.
//!
//! # DNS is out of scope (by design)
//!
//! This is a **pure** validator: it inspects only the literal/parsed target and
//! any IP literal it is given. It does **not** resolve hostnames. A hostname
//! that passes the allowlist here can still resolve to a forbidden address at
//! request time (DNS rebinding); re-validating the *resolved* IP at
//! resolution time against [`WebPolicy::check_ip`] is the **daemon's** job, not
//! this module's. Allowlisting a host does not exempt it from IP-literal
//! guarding: a literal like `http://10.0.0.1/` is denied even if `10.0.0.1`
//! were somehow allowlisted.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// The candidate-allowable URL schemes. Every other scheme (`file`, `ftp`,
/// `gopher`, `data`, …) is denied outright by the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
}

impl Scheme {
    /// The default port for the scheme (`80` for http, `443` for https), used
    /// when a target omits an explicit port.
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }

    /// Parse a scheme from its lowercase wire name, accepting only the
    /// candidate-allowable `http` / `https`. Comparison is case-insensitive.
    fn from_str_ci(s: &str) -> Option<Scheme> {
        if s.eq_ignore_ascii_case("http") {
            Some(Scheme::Http)
        } else if s.eq_ignore_ascii_case("https") {
            Some(Scheme::Https)
        } else {
            None
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scheme::Http => f.write_str("http"),
            Scheme::Https => f.write_str("https"),
        }
    }
}

/// Why a target was denied. Every variant carries enough context to render a
/// clear, actionable message via [`Display`](fmt::Display).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum DenyReason {
    /// The target could not be parsed into scheme + host (+ optional port).
    MalformedTarget(String),
    /// The scheme is not one of the candidate-allowable `http` / `https`.
    SchemeNotAllowed(String),
    /// The host matched no allow rule (deny-by-default).
    HostNotAllowed(String),
    /// The port is not in the allowed-port set.
    PortNotAllowed(u16),
    /// The target is a loopback address (`127.0.0.0/8`, `::1`) or `localhost`.
    Loopback,
    /// The target is a private / RFC1918 address (`10/8`, `172.16/12`,
    /// `192.168/16`) or an IPv6 unique-local address (`fc00::/7`).
    PrivateNetwork,
    /// The target is a link-local address (`169.254.0.0/16` — including the
    /// cloud metadata IP `169.254.169.254` — or `fe80::/10`).
    LinkLocal,
    /// The target is a carrier-grade-NAT / shared address (`100.64.0.0/10`).
    SharedAddress,
    /// The target is the unspecified address (`0.0.0.0`, `::`).
    Unspecified,
    /// The target is a multicast address.
    Multicast,
    /// The target falls in a reserved / non-routable range (e.g. IPv4
    /// `240.0.0.0/4`, the `255.255.255.255` broadcast, or IPv4-as-IPv6 forms
    /// that map onto a guarded IPv4 range).
    Reserved,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenyReason::MalformedTarget(t) => write!(f, "malformed target: {t}"),
            DenyReason::SchemeNotAllowed(s) => {
                write!(f, "scheme not allowed: {s} (only http/https are permitted)")
            }
            DenyReason::HostNotAllowed(h) => {
                write!(f, "host not allowed: {h} (no matching allowlist rule)")
            }
            DenyReason::PortNotAllowed(p) => write!(f, "port not allowed: {p}"),
            DenyReason::Loopback => f.write_str("blocked: loopback address"),
            DenyReason::PrivateNetwork => f.write_str("blocked: private/internal network address"),
            DenyReason::LinkLocal => f.write_str("blocked: link-local address"),
            DenyReason::SharedAddress => f.write_str("blocked: shared/carrier-grade-NAT address"),
            DenyReason::Unspecified => f.write_str("blocked: unspecified address"),
            DenyReason::Multicast => f.write_str("blocked: multicast address"),
            DenyReason::Reserved => f.write_str("blocked: reserved/non-routable address"),
        }
    }
}

/// The decision returned by every policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// The target passed every guard and matched the allowlist.
    Allow,
    /// The target was denied; the variant carries the reason.
    Deny(DenyReason),
}

impl PolicyDecision {
    /// `true` if the decision is [`Allow`](PolicyDecision::Allow).
    pub fn is_allowed(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }

    /// `true` if the decision is a [`Deny`](PolicyDecision::Deny).
    pub fn is_denied(&self) -> bool {
        matches!(self, PolicyDecision::Deny(_))
    }

    /// The [`DenyReason`] if denied, else `None`.
    pub fn deny_reason(&self) -> Option<&DenyReason> {
        match self {
            PolicyDecision::Deny(r) => Some(r),
            PolicyDecision::Allow => None,
        }
    }
}

/// A single allowlist rule against which a host is matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum HostRule {
    /// Match exactly this host (case-insensitive). `example.com` matches only
    /// `example.com`, never a sub-domain.
    Exact(String),
    /// Match this domain and any sub-domain of it. Stored without the leading
    /// dot: a rule of `example.com` matches `example.com` and
    /// `api.example.com`, but **not** `notexample.com` nor
    /// `example.com.evil.com`.
    DomainSuffix(String),
}

impl HostRule {
    /// `true` if `host` (already lowercased, trailing dot stripped) matches.
    fn matches(&self, host: &str) -> bool {
        match self {
            HostRule::Exact(want) => host == want.as_str(),
            HostRule::DomainSuffix(domain) => {
                let domain = domain.as_str();
                host == domain
                    || (host.len() > domain.len()
                        && host.ends_with(domain)
                        && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
            }
        }
    }
}

/// A deny-by-default web-egress policy. Hold the allowlist of host rules, the
/// set of allowed ports, and the SSRF-guard toggles. Construct the default
/// (deny-everything) with [`WebPolicy::default`] and relax it with the builder
/// methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct WebPolicy {
    /// Explicit host allowlist. Empty ⇒ everything denied (deny-by-default).
    pub allow_hosts: Vec<HostRule>,
    /// Allowed destination ports. Defaults to `{80, 443}`.
    pub allow_ports: Vec<u16>,
    /// When `true` (default) IP-literal targets are checked against the SSRF
    /// guards. Disabling it is unsafe and intended only for tests; the daemon
    /// must never disable it.
    pub guard_ip_literals: bool,
}

impl Default for WebPolicy {
    /// Deny-everything: empty allowlist, ports `{80, 443}`, IP guards on.
    fn default() -> Self {
        WebPolicy {
            allow_hosts: Vec::new(),
            allow_ports: vec![80, 443],
            guard_ip_literals: true,
        }
    }
}

impl WebPolicy {
    /// A fresh deny-everything policy (alias for [`WebPolicy::default`]).
    pub fn deny_all() -> Self {
        WebPolicy::default()
    }

    /// Add an exact-host allow rule (builder style). Case is normalized on
    /// match, so callers may pass any casing.
    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.allow_hosts
            .push(HostRule::Exact(normalize_host(&host.into())));
        self
    }

    /// Add a domain-suffix allow rule (builder style). A leading dot is
    /// optional: `.example.com` and `example.com` behave identically and both
    /// match `example.com` and any sub-domain.
    pub fn allow_domain(mut self, domain: impl Into<String>) -> Self {
        let domain = domain.into();
        let trimmed = domain.trim_start_matches('.');
        self.allow_hosts
            .push(HostRule::DomainSuffix(normalize_host(trimmed)));
        self
    }

    /// Replace the allowed-port set (builder style).
    pub fn allow_ports(mut self, ports: impl IntoIterator<Item = u16>) -> Self {
        self.allow_ports = ports.into_iter().collect();
        self
    }

    /// Add a single allowed port (builder style).
    pub fn allow_port(mut self, port: u16) -> Self {
        if !self.allow_ports.contains(&port) {
            self.allow_ports.push(port);
        }
        self
    }

    /// Validate a full target URL, e.g. `https://api.example.com:8443/path`.
    ///
    /// The URL is parsed manually (no DNS, no new dependency): scheme, host,
    /// optional port, with the rest ignored. A missing port defaults to the
    /// scheme's default port. Anything that fails to parse is
    /// [`DenyReason::MalformedTarget`].
    pub fn check(&self, target: &str) -> PolicyDecision {
        match ParsedTarget::parse(target) {
            Ok(p) => self.check_addr(&p.host, p.port, p.scheme),
            Err(reason) => PolicyDecision::Deny(reason),
        }
    }

    /// Validate a decomposed target: `host` (a hostname or IP literal), `port`,
    /// and `scheme`. This is the core check; [`check`](WebPolicy::check) parses
    /// a URL down to this.
    pub fn check_addr(&self, host: &str, port: u16, scheme: Scheme) -> PolicyDecision {
        // (Scheme is already constrained to http/https by the type; a textual
        //  target with another scheme is rejected during parsing.)
        let _ = scheme;

        let host = normalize_host(host);
        if host.is_empty() {
            return PolicyDecision::Deny(DenyReason::MalformedTarget("empty host".to_string()));
        }

        // Port allowlist.
        if !self.allow_ports.contains(&port) {
            return PolicyDecision::Deny(DenyReason::PortNotAllowed(port));
        }

        // If the host is an IP literal, guard it directly regardless of the
        // allowlist. `localhost` is treated as loopback by name.
        if host == "localhost" {
            return PolicyDecision::Deny(DenyReason::Loopback);
        }
        if let Some(ip) = parse_ip_literal(&host) {
            if self.guard_ip_literals {
                if let Some(reason) = guard_ip(ip) {
                    return PolicyDecision::Deny(reason);
                }
            }
            // A bare public-IP literal is only allowed if it matches an allow
            // rule (deny-by-default still applies to IPs).
            if self.host_allowed(&host) {
                return PolicyDecision::Allow;
            }
            return PolicyDecision::Deny(DenyReason::HostNotAllowed(host));
        }

        // Hostname path: must match the allowlist. (Resolution-time IP
        // re-validation is the daemon's job — see the module docs.)
        if self.host_allowed(&host) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(DenyReason::HostNotAllowed(host))
        }
    }

    /// Validate an already-resolved [`IpAddr`] against the SSRF guards and the
    /// port allowlist. The daemon calls this at DNS-resolution time to defeat
    /// rebinding: a hostname that passed [`check`](WebPolicy::check) is re-checked
    /// here once it resolves to a concrete address.
    pub fn check_ip(&self, ip: IpAddr, port: u16) -> PolicyDecision {
        if !self.allow_ports.contains(&port) {
            return PolicyDecision::Deny(DenyReason::PortNotAllowed(port));
        }
        match guard_ip(ip) {
            Some(reason) => PolicyDecision::Deny(reason),
            None => PolicyDecision::Allow,
        }
    }

    /// `true` if `host` (normalized) matches any allow rule.
    fn host_allowed(&self, host: &str) -> bool {
        self.allow_hosts.iter().any(|r| r.matches(host))
    }
}

/// A target decomposed into the fields the policy cares about.
struct ParsedTarget {
    scheme: Scheme,
    host: String,
    port: u16,
}

impl ParsedTarget {
    /// Parse `scheme://host[:port][/...]` manually. Rejects non-http(s) schemes
    /// with [`DenyReason::SchemeNotAllowed`] and unparseable input with
    /// [`DenyReason::MalformedTarget`].
    fn parse(target: &str) -> Result<ParsedTarget, DenyReason> {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err(DenyReason::MalformedTarget("empty target".to_string()));
        }

        // Split scheme. A scheme is mandatory: deny-by-default means we never
        // guess http for a bare host.
        let (scheme_str, rest) = trimmed
            .split_once("://")
            .ok_or_else(|| DenyReason::MalformedTarget(format!("missing scheme: {trimmed}")))?;
        if scheme_str.is_empty() {
            return Err(DenyReason::MalformedTarget(format!(
                "empty scheme: {trimmed}"
            )));
        }
        let scheme = match Scheme::from_str_ci(scheme_str) {
            Some(s) => s,
            None => {
                return Err(DenyReason::SchemeNotAllowed(
                    scheme_str.to_ascii_lowercase(),
                ))
            }
        };

        // Strip any userinfo (`user:pass@`), path, query, and fragment to leave
        // the authority's host[:port]. Userinfo is taken as everything up to the
        // last `@` before the first path/query/fragment delimiter.
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let host_port = match authority.rsplit_once('@') {
            Some((_userinfo, hp)) => hp,
            None => authority,
        };
        if host_port.is_empty() {
            return Err(DenyReason::MalformedTarget(format!(
                "missing host: {trimmed}"
            )));
        }

        let (host, port) = split_host_port(host_port, scheme.default_port())
            .map_err(|e| DenyReason::MalformedTarget(format!("{e}: {trimmed}")))?;
        if host.is_empty() {
            return Err(DenyReason::MalformedTarget(format!(
                "empty host: {trimmed}"
            )));
        }

        Ok(ParsedTarget { scheme, host, port })
    }
}

/// Split `host[:port]`, handling bracketed IPv6 literals (`[::1]:443`). Returns
/// `(host, port)` with `default_port` when no port is present.
fn split_host_port(host_port: &str, default_port: u16) -> Result<(String, u16), String> {
    if let Some(rest) = host_port.strip_prefix('[') {
        // Bracketed IPv6 literal: `[addr]` or `[addr]:port`.
        let close = rest
            .find(']')
            .ok_or_else(|| "unterminated IPv6 literal".to_string())?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        if after.is_empty() {
            return Ok((host.to_string(), default_port));
        }
        let port_str = after
            .strip_prefix(':')
            .ok_or_else(|| "junk after IPv6 literal".to_string())?;
        let port = parse_port(port_str)?;
        return Ok((host.to_string(), port));
    }

    match host_port.rsplit_once(':') {
        // A single `:` and the left side itself contains no `:` ⇒ host:port.
        // (A bare unbracketed IPv6 would contain multiple colons; we reject it
        //  below as ambiguous rather than mis-parse a port.)
        Some((host, port_str)) if !host.contains(':') => {
            let port = parse_port(port_str)?;
            Ok((host.to_string(), port))
        }
        Some(_) => Err("ambiguous host (unbracketed IPv6 must be in [..])".to_string()),
        None => Ok((host_port.to_string(), default_port)),
    }
}

/// Parse a decimal port in the `1..=65535` range.
fn parse_port(s: &str) -> Result<u16, String> {
    let n: u32 = s.parse().map_err(|_| format!("invalid port: {s}"))?;
    if n == 0 || n > u16::MAX as u32 {
        return Err(format!("port out of range: {s}"));
    }
    Ok(n as u16)
}

/// Lowercase a host and strip a single trailing dot (the FQDN root), so
/// `Example.COM.` and `example.com` compare equal. Surrounding brackets on an
/// IPv6 literal are stripped too.
fn normalize_host(host: &str) -> String {
    let h = host.trim();
    let h = h.strip_prefix('[').unwrap_or(h);
    let h = h.strip_suffix(']').unwrap_or(h);
    let h = h.strip_suffix('.').unwrap_or(h);
    h.to_ascii_lowercase()
}

/// Parse a host string as an IP literal, accepting bracketed IPv6. Returns
/// `None` for hostnames.
fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    let h = host.strip_prefix('[').unwrap_or(host);
    let h = h.strip_suffix(']').unwrap_or(h);
    h.parse::<IpAddr>().ok()
}

/// The SSRF guard: map an IP to a [`DenyReason`] if it falls in any forbidden
/// range, else `None` (a public, routable address). IPv4-mapped/-compatible
/// IPv6 addresses are unwrapped and re-checked as IPv4 so they cannot smuggle a
/// guarded IPv4 range through the v6 path.
fn guard_ip(ip: IpAddr) -> Option<DenyReason> {
    match ip {
        IpAddr::V4(v4) => guard_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = unwrap_v4_in_v6(v6) {
                // Treat an embedded IPv4 as IPv4 for guarding purposes.
                return guard_ipv4(v4).or(Some(DenyReason::Reserved));
            }
            guard_ipv6(v6)
        }
    }
}

/// Guard an IPv4 address. Order matters only for message specificity, not
/// correctness — the ranges are disjoint.
fn guard_ipv4(ip: Ipv4Addr) -> Option<DenyReason> {
    if ip.is_unspecified() {
        return Some(DenyReason::Unspecified);
    }
    if ip.is_loopback() {
        return Some(DenyReason::Loopback);
    }
    if ip.is_link_local() {
        // Includes the cloud metadata IP 169.254.169.254.
        return Some(DenyReason::LinkLocal);
    }
    if ip.is_private() {
        return Some(DenyReason::PrivateNetwork);
    }
    if is_ipv4_shared(ip) {
        // 100.64.0.0/10 carrier-grade NAT.
        return Some(DenyReason::SharedAddress);
    }
    if ip.is_multicast() || ip.is_broadcast() {
        return Some(DenyReason::Multicast);
    }
    if is_ipv4_reserved(ip) {
        // 192.0.0.0/24, 192.0.2.0/24, 198.18.0.0/15, 198.51.100.0/24,
        // 203.0.113.0/24, 240.0.0.0/4, etc.
        return Some(DenyReason::Reserved);
    }
    None
}

/// Guard an IPv6 address. `std` covers loopback/unspecified/multicast on stable;
/// unique-local (`fc00::/7`) and link-local (`fe80::/10`) are matched manually
/// because the `Ipv6Addr` helpers for them are unstable on the MSRV (1.82).
fn guard_ipv6(ip: Ipv6Addr) -> Option<DenyReason> {
    if ip.is_unspecified() {
        return Some(DenyReason::Unspecified);
    }
    if ip.is_loopback() {
        return Some(DenyReason::Loopback);
    }
    let seg0 = ip.segments()[0];
    // Link-local fe80::/10.
    if (seg0 & 0xffc0) == 0xfe80 {
        return Some(DenyReason::LinkLocal);
    }
    // Unique-local fc00::/7 (the IPv6 analogue of RFC1918).
    if (seg0 & 0xfe00) == 0xfc00 {
        return Some(DenyReason::PrivateNetwork);
    }
    if ip.is_multicast() {
        return Some(DenyReason::Multicast);
    }
    None
}

/// Unwrap an IPv4-mapped (`::ffff:a.b.c.d`) or IPv4-compatible (`::a.b.c.d`,
/// excluding `::` / `::1`) IPv6 address to its IPv4 form, else `None`.
fn unwrap_v4_in_v6(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    // IPv4-mapped: ::ffff:0:0/96.
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0xffff {
        let o = ip.octets();
        return Some(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    // IPv4-compatible: ::0.0.0.0/96, but not the unspecified/loopback addresses.
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        let o = ip.octets();
        let v4 = Ipv4Addr::new(o[12], o[13], o[14], o[15]);
        if !v4.is_unspecified() && o[12] != 0 {
            return Some(v4);
        }
    }
    None
}

/// `true` for the CGNAT / shared-address block `100.64.0.0/10` (RFC 6598).
fn is_ipv4_shared(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// `true` for IPv4 ranges that are reserved / non-routable on the public
/// Internet and not already covered by a more specific guard above.
fn is_ipv4_reserved(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    // 0.0.0.0/8 "this network" (0.0.0.0 itself is caught as unspecified).
    if a == 0 {
        return true;
    }
    // 192.0.0.0/24 IETF protocol assignments.
    if a == 192 && b == 0 && c == 0 {
        return true;
    }
    // 192.0.2.0/24 TEST-NET-1.
    if a == 192 && b == 0 && c == 2 {
        return true;
    }
    // 198.18.0.0/15 benchmarking.
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    // 198.51.100.0/24 TEST-NET-2.
    if a == 198 && b == 51 && c == 100 {
        return true;
    }
    // 203.0.113.0/24 TEST-NET-3.
    if a == 203 && b == 0 && c == 113 {
        return true;
    }
    // 240.0.0.0/4 reserved for future use (255.255.255.255 is broadcast).
    if a >= 240 {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy that allows the example.com family on default ports, for
    /// exercising the allow path.
    fn example_policy() -> WebPolicy {
        WebPolicy::default()
            .allow_domain(".example.com")
            .allow_host("standalone.test")
    }

    #[test]
    fn default_denies_everything() {
        let p = WebPolicy::default();
        assert!(p.check("https://example.com/").is_denied());
        assert!(p.check("http://anything.at.all/").is_denied());
        assert_eq!(
            p.check("https://example.com/").deny_reason(),
            Some(&DenyReason::HostNotAllowed("example.com".to_string()))
        );
    }

    #[test]
    fn allow_exact_host_hit_and_miss() {
        let p = WebPolicy::default().allow_host("api.example.com");
        assert!(p.check("https://api.example.com/").is_allowed());
        // Exact rule does not match sub-domains or the bare parent.
        assert!(p.check("https://v2.api.example.com/").is_denied());
        assert!(p.check("https://example.com/").is_denied());
    }

    #[test]
    fn domain_suffix_matches_subdomains_and_apex() {
        let p = WebPolicy::default().allow_domain("example.com");
        assert!(p.check("https://example.com/").is_allowed());
        assert!(p.check("https://api.example.com/").is_allowed());
        assert!(p.check("https://a.b.c.example.com/").is_allowed());
    }

    #[test]
    fn domain_suffix_rejects_lookalikes() {
        let p = WebPolicy::default().allow_domain("example.com");
        // Substring but not a sub-domain.
        assert!(p.check("https://notexample.com/").is_denied());
        // Suffix-as-prefix attack: allowlisted domain appears as a label, not
        // the suffix.
        assert!(p.check("https://example.com.evil.com/").is_denied());
        assert!(p.check("https://evilexample.com/").is_denied());
    }

    #[test]
    fn leading_dot_in_domain_rule_is_optional() {
        let with_dot = WebPolicy::default().allow_domain(".example.com");
        let without = WebPolicy::default().allow_domain("example.com");
        assert!(with_dot.check("https://api.example.com/").is_allowed());
        assert!(without.check("https://api.example.com/").is_allowed());
    }

    #[test]
    fn host_match_is_case_insensitive() {
        let p = WebPolicy::default().allow_host("api.example.com");
        assert!(p.check("https://API.Example.COM/").is_allowed());
    }

    #[test]
    fn trailing_dot_fqdn_is_normalized() {
        let p = WebPolicy::default().allow_domain("example.com");
        assert!(p.check("https://api.example.com./").is_allowed());
    }

    #[test]
    fn loopback_ipv4_denied_even_if_allowlisted() {
        // Allowlisting the literal must not bypass the guard.
        let p = WebPolicy::default().allow_host("127.0.0.1");
        assert_eq!(
            p.check("http://127.0.0.1/").deny_reason(),
            Some(&DenyReason::Loopback)
        );
        // Anywhere in 127/8.
        assert_eq!(
            p.check("http://127.9.9.9/").deny_reason(),
            Some(&DenyReason::Loopback)
        );
    }

    #[test]
    fn loopback_ipv6_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://[::1]/").deny_reason(),
            Some(&DenyReason::Loopback)
        );
    }

    #[test]
    fn localhost_name_denied() {
        let p = WebPolicy::default().allow_host("localhost");
        assert_eq!(
            p.check("http://localhost/").deny_reason(),
            Some(&DenyReason::Loopback)
        );
    }

    #[test]
    fn private_rfc1918_ranges_denied() {
        let p = example_policy();
        for host in [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
        ] {
            assert_eq!(
                p.check(&format!("http://{host}/")).deny_reason(),
                Some(&DenyReason::PrivateNetwork),
                "{host} should be PrivateNetwork"
            );
        }
        // 172.15/172.32 are NOT in 172.16/12 — they fail on host-not-allowed,
        // not on the private guard.
        assert_eq!(
            p.check("http://172.15.0.1/").deny_reason(),
            Some(&DenyReason::HostNotAllowed("172.15.0.1".to_string()))
        );
    }

    #[test]
    fn ipv6_unique_local_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://[fc00::1]/").deny_reason(),
            Some(&DenyReason::PrivateNetwork)
        );
        assert_eq!(
            p.check("http://[fd12:3456::1]/").deny_reason(),
            Some(&DenyReason::PrivateNetwork)
        );
    }

    #[test]
    fn link_local_and_metadata_ip_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://169.254.0.1/").deny_reason(),
            Some(&DenyReason::LinkLocal)
        );
        // The cloud metadata endpoint — the canonical SSRF target.
        assert_eq!(
            p.check("http://169.254.169.254/").deny_reason(),
            Some(&DenyReason::LinkLocal)
        );
        // IPv6 link-local.
        assert_eq!(
            p.check("http://[fe80::1]/").deny_reason(),
            Some(&DenyReason::LinkLocal)
        );
    }

    #[test]
    fn cgnat_shared_address_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://100.64.0.1/").deny_reason(),
            Some(&DenyReason::SharedAddress)
        );
        // 100.63 and 100.128 are outside 100.64/10.
        assert!(matches!(
            p.check("http://100.63.0.1/").deny_reason(),
            Some(&DenyReason::HostNotAllowed(_))
        ));
    }

    #[test]
    fn unspecified_and_reserved_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://0.0.0.0/").deny_reason(),
            Some(&DenyReason::Unspecified)
        );
        // 240/4 reserved.
        assert_eq!(
            p.check("http://240.0.0.1/").deny_reason(),
            Some(&DenyReason::Reserved)
        );
        // 255.255.255.255 broadcast.
        assert_eq!(
            p.check("http://255.255.255.255/").deny_reason(),
            Some(&DenyReason::Multicast)
        );
        // TEST-NET-1.
        assert_eq!(
            p.check("http://192.0.2.5/").deny_reason(),
            Some(&DenyReason::Reserved)
        );
    }

    #[test]
    fn multicast_denied() {
        let p = example_policy();
        assert_eq!(
            p.check("http://224.0.0.1/").deny_reason(),
            Some(&DenyReason::Multicast)
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_cannot_smuggle_loopback() {
        let p = example_policy();
        // ::ffff:127.0.0.1 must be guarded as IPv4 loopback.
        assert_eq!(
            p.check("http://[::ffff:127.0.0.1]/").deny_reason(),
            Some(&DenyReason::Loopback)
        );
        // ::ffff:10.0.0.1 → private.
        assert_eq!(
            p.check("http://[::ffff:10.0.0.1]/").deny_reason(),
            Some(&DenyReason::PrivateNetwork)
        );
    }

    #[test]
    fn non_http_schemes_denied() {
        let p = example_policy();
        for target in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "gopher://example.com/",
            "data:text/plain,hi",
            "ssh://example.com/",
        ] {
            assert!(
                matches!(
                    p.check(target).deny_reason(),
                    Some(DenyReason::SchemeNotAllowed(_)) | Some(DenyReason::MalformedTarget(_))
                ),
                "{target} should be denied by scheme"
            );
        }
        // file: yields SchemeNotAllowed specifically.
        assert_eq!(
            p.check("file:///etc/passwd").deny_reason(),
            Some(&DenyReason::SchemeNotAllowed("file".to_string()))
        );
    }

    #[test]
    fn port_allowlist_default_80_443() {
        let p = WebPolicy::default().allow_domain("example.com");
        assert!(p.check("http://example.com/").is_allowed()); // 80
        assert!(p.check("https://example.com/").is_allowed()); // 443
        assert!(p.check("https://example.com:8443/").is_denied());
        assert_eq!(
            p.check("https://example.com:8443/").deny_reason(),
            Some(&DenyReason::PortNotAllowed(8443))
        );
    }

    #[test]
    fn custom_port_allowed() {
        let p = WebPolicy::default()
            .allow_domain("example.com")
            .allow_port(8443);
        assert!(p.check("https://example.com:8443/").is_allowed());
    }

    #[test]
    fn explicit_default_port_is_allowed() {
        let p = WebPolicy::default().allow_domain("example.com");
        assert!(p.check("https://example.com:443/").is_allowed());
        assert!(p.check("http://example.com:80/").is_allowed());
    }

    #[test]
    fn malformed_targets_denied() {
        let p = example_policy();
        for target in [
            "",
            "   ",
            "example.com",
            "://example.com",
            "https://",
            "https:///path",
        ] {
            assert!(
                matches!(
                    p.check(target).deny_reason(),
                    Some(DenyReason::MalformedTarget(_))
                ),
                "{target:?} should be MalformedTarget"
            );
        }
    }

    #[test]
    fn userinfo_and_path_are_stripped() {
        let p = WebPolicy::default().allow_host("api.example.com");
        // Userinfo before the host must not confuse the parser.
        assert!(p
            .check("https://user:pass@api.example.com/path?q=1#frag")
            .is_allowed());
        // An @-host injection: real host is evil.com, not example.com.
        assert!(p.check("https://api.example.com@evil.com/").is_denied());
    }

    #[test]
    fn check_addr_direct() {
        let p = WebPolicy::default().allow_host("api.example.com");
        assert!(p
            .check_addr("api.example.com", 443, Scheme::Https)
            .is_allowed());
        assert_eq!(
            p.check_addr("api.example.com", 22, Scheme::Https)
                .deny_reason(),
            Some(&DenyReason::PortNotAllowed(22))
        );
        assert_eq!(
            p.check_addr("", 443, Scheme::Https).deny_reason(),
            Some(&DenyReason::MalformedTarget("empty host".to_string()))
        );
    }

    #[test]
    fn check_ip_rebinding_revalidation() {
        // The daemon's resolution-time path: a name passed `check`, but resolves
        // to a forbidden address.
        let p = example_policy();
        assert_eq!(
            p.check_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 443)
                .deny_reason(),
            Some(&DenyReason::LinkLocal)
        );
        assert!(p
            .check_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443)
            .is_allowed());
        assert_eq!(
            p.check_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 8443)
                .deny_reason(),
            Some(&DenyReason::PortNotAllowed(8443))
        );
    }

    #[test]
    fn public_address_literal_requires_allowlist() {
        // A public IP literal trips no guard but is still deny-by-default.
        let p = WebPolicy::default();
        assert!(matches!(
            p.check("https://93.184.216.34/").deny_reason(),
            Some(DenyReason::HostNotAllowed(_))
        ));
        // Allowlisting the literal lets it through.
        let p = WebPolicy::default().allow_host("93.184.216.34");
        assert!(p.check("https://93.184.216.34/").is_allowed());
    }

    #[test]
    fn deny_reason_displays_clearly() {
        assert_eq!(
            DenyReason::Loopback.to_string(),
            "blocked: loopback address"
        );
        assert_eq!(
            DenyReason::PortNotAllowed(8080).to_string(),
            "port not allowed: 8080"
        );
        assert_eq!(
            DenyReason::SchemeNotAllowed("ftp".to_string()).to_string(),
            "scheme not allowed: ftp (only http/https are permitted)"
        );
    }

    #[test]
    fn policy_round_trips_through_serde() {
        let p = example_policy().allow_port(8443);
        let json = serde_json::to_string(&p).unwrap();
        let back: WebPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert!(back.check("https://api.example.com:8443/").is_allowed());
    }
}
