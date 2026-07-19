//! SSRF (Server-Side Request Forgery) protection for `web_fetch`.
//!
//! Policy:
//! - Non-public addresses (loopback, RFC 1918, link-local, CGNAT, TEST-NET,
//!   multicast, etc.) are blocked by default.
//! - Local access is opt-in via tool params (`WebFetchParams::allow_local`,
//!   set from `[toolset.web_fetch] allow_local` or `DS_WEB_FETCH_ALLOW_LOCAL=1`).
//!   Even when enabled, only **explicit** loopback hosts are allowed
//!   (`localhost`, `127.0.0.0/8` literals, `::1`). A public hostname that
//!   resolves to loopback/private stays blocked.
//!
//! Reference: [IANA IPv4 Special-Purpose Address Registry](https://www.iana.org/assignments/iana-ipv4-special-registry/)

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::Url;

use super::error::WebFetchError;

/// Hostnames/IP literals that may reach loopback when local binding is
/// enabled. Public names that *resolve* to loopback are not included — that
/// closes DNS rebinding through a non-local hostname.
pub(crate) fn is_explicit_local_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host);
    // Drop IPv6 zone id if present (`fe80::1%lo0`).
    let host = host.split('%').next().unwrap_or(host);

    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    false
}

/// Returns `true` if an IP is not globally routable and should be treated as
/// local/private for SSRF.
pub(crate) fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => is_non_public_ipv6(v6),
    }
}

fn is_non_public_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        // "This network" (RFC 1122) 0.0.0.0/8
        || ipv4_in_cidr(ip, [0, 0, 0, 0], 8)
        // CGNAT (RFC 6598) 100.64.0.0/10 — cloud metadata-ish
        || ipv4_in_cidr(ip, [100, 64, 0, 0], 10)
        // IETF Protocol Assignments (RFC 6890) 192.0.0.0/24
        || ipv4_in_cidr(ip, [192, 0, 0, 0], 24)
        // TEST-NET-1 (RFC 5737)
        || ipv4_in_cidr(ip, [192, 0, 2, 0], 24)
        // Benchmarking (RFC 2544)
        || ipv4_in_cidr(ip, [198, 18, 0, 0], 15)
        // TEST-NET-2 / TEST-NET-3
        || ipv4_in_cidr(ip, [198, 51, 100, 0], 24)
        || ipv4_in_cidr(ip, [203, 0, 113, 0], 24)
        // Reserved (RFC 6890) 240.0.0.0/4
        || ipv4_in_cidr(ip, [240, 0, 0, 0], 4)
}

fn ipv4_in_cidr(ip: Ipv4Addr, base: [u8; 4], prefix: u8) -> bool {
    let ip = u32::from(ip);
    let base = u32::from(Ipv4Addr::from(base));
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (ip & mask) == (base & mask)
}

fn is_non_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_non_public_ipv4(v4);
    }
    // Anything not globally routable: loopback, ULA, link-local, unspecified, multicast.
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
}

/// Loopback including IPv4-mapped forms (`::ffff:127.0.0.1`).
///
/// `IpAddr::is_loopback` is false for mapped addresses even when the embedded
/// v4 is loopback, so local opt-in must use this helper.
fn is_loopback_addr(ip: IpAddr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
        IpAddr::V4(_) => false,
    }
}

/// Whether a resolved address is blocked for this request host.
///
/// Dual-gate: even with local binding allowed, only explicit loopback hosts
/// may use loopback IPs; private/link-local never open via this flag.
pub(crate) fn is_blocked_for_host(ip: IpAddr, host: &str, allow_local: bool) -> bool {
    if !is_non_public_ip(ip) {
        return false;
    }
    if allow_local && is_loopback_addr(ip) && is_explicit_local_host(host) {
        return false;
    }
    true
}

/// Resolve hostname via DNS and verify none of the resolved addresses are
/// blocked under the SSRF policy.
///
/// When `allow_local` is true, explicit loopback hosts (localhost, etc.) may
/// reach loopback addresses. Private/link-local addresses remain blocked
/// regardless.
pub(crate) async fn check_ssrf(url: &Url, allow_local: bool) -> Result<(), WebFetchError> {
    let host = url
        .host_str()
        .ok_or_else(|| WebFetchError::SingleLabelHost {
            host: String::new(),
        })?;

    // If the host is already a literal IP, check it directly.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_for_host(ip, host, allow_local) {
            return Err(WebFetchError::SsrfBlocked {
                host: host.to_string(),
                ip,
            });
        }
        return Ok(());
    }

    // DNS resolution.
    let port = url.port_or_known_default().unwrap_or(443);
    let addr_str = format!("{host}:{port}");
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&addr_str)
        .await
        .map_err(|e| WebFetchError::DnsResolution {
            host: host.to_string(),
            source: e,
        })?
        .collect();

    if addrs.is_empty() {
        return Err(WebFetchError::DnsEmpty(host.to_string()));
    }

    addrs
        .iter()
        .find(|addr| is_blocked_for_host(addr.ip(), host, allow_local))
        .map_or(Ok(()), |addr| {
            Err(WebFetchError::SsrfBlocked {
                host: host.to_string(),
                ip: addr.ip(),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_explicit_local_host ──────────────────────────────────────────

    #[test]
    fn localhost_is_explicit_local() {
        assert!(is_explicit_local_host("localhost"));
        assert!(is_explicit_local_host("LOCALHOST"));
        assert!(is_explicit_local_host("localhost."));
    }

    #[test]
    fn loopback_literals_are_explicit_local() {
        assert!(is_explicit_local_host("127.0.0.1"));
        assert!(is_explicit_local_host("127.255.255.255"));
        assert!(is_explicit_local_host("::1"));
        assert!(is_explicit_local_host("[::1]"));
    }

    #[test]
    fn non_loopback_not_explicit_local() {
        assert!(!is_explicit_local_host("example.com"));
        assert!(!is_explicit_local_host("8.8.8.8"));
        assert!(!is_explicit_local_host("10.0.0.1"));
    }

    // ── is_non_public_ipv4 ──────────────────────────────────────────────

    #[test]
    fn blocks_rfc1918_10x() {
        assert!(is_non_public_ip("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_rfc1918_172x() {
        assert!(is_non_public_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_non_public_ip("172.32.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_rfc1918_192168() {
        assert!(is_non_public_ip("192.168.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_link_local() {
        assert!(is_non_public_ip("169.254.0.1".parse().unwrap()));
        assert!(is_non_public_ip("169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn blocks_cgnat() {
        assert!(is_non_public_ip("100.64.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_unspecified() {
        assert!(is_non_public_ip("0.0.0.0".parse().unwrap()));
        assert!(is_non_public_ip("::".parse().unwrap()));
    }

    #[test]
    fn blocks_multicast() {
        assert!(is_non_public_ip("224.0.0.1".parse().unwrap()));
        assert!(is_non_public_ip("ff02::1".parse().unwrap()));
    }

    #[test]
    fn blocks_test_nets() {
        assert!(is_non_public_ip("192.0.2.1".parse().unwrap()));
        assert!(is_non_public_ip("198.51.100.1".parse().unwrap()));
        assert!(is_non_public_ip("203.0.113.1".parse().unwrap()));
    }

    #[test]
    fn loopback_is_non_public() {
        assert!(is_non_public_ip("127.0.0.1".parse().unwrap()));
        assert!(is_non_public_ip("::1".parse().unwrap()));
    }

    #[test]
    fn allows_public_ips() {
        assert!(!is_non_public_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_non_public_ip("8.8.8.8".parse().unwrap()));
    }

    // ── IPv6 ────────────────────────────────────────────────────────────

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(is_non_public_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn blocks_ipv6_unique_local() {
        assert!(is_non_public_ip("fc00::1".parse().unwrap()));
        assert!(is_non_public_ip("fd00::1".parse().unwrap()));
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6_private() {
        assert!(is_non_public_ip("::ffff:10.0.0.1".parse::<IpAddr>().unwrap()));
    }

    // ── is_blocked_for_host ─────────────────────────────────────────────

    #[test]
    fn blocks_non_public_by_default() {
        assert!(is_blocked_for_host("10.0.0.1".parse().unwrap(), "example.com", false));
        assert!(is_blocked_for_host("127.0.0.1".parse().unwrap(), "example.com", false));
        // Loopback literal + allow_local = allowed
        assert!(!is_blocked_for_host("127.0.0.1".parse().unwrap(), "127.0.0.1", true));
    }

    #[test]
    fn allow_local_only_for_explicit_loopback_hosts() {
        // example.com resolving to 127.0.0.1 via DNS rebinding stays blocked
        assert!(is_blocked_for_host("127.0.0.1".parse().unwrap(), "example.com", true));
        // localhost explicitly used is allowed
        assert!(!is_blocked_for_host("127.0.0.1".parse().unwrap(), "localhost", true));
    }

    #[test]
    fn allow_local_never_opens_private() {
        // Even with allow_local, private ranges stay blocked
        assert!(is_blocked_for_host("10.0.0.1".parse().unwrap(), "localhost", true));
        assert!(is_blocked_for_host("192.168.1.1".parse().unwrap(), "127.0.0.1", true));
    }

    // ── check_ssrf integration ──────────────────────────────────────────

    #[tokio::test]
    async fn ssrf_blocks_ip_literal_private() {
        let url = Url::parse("https://10.0.0.1/secret").unwrap();
        let result = check_ssrf(&url, false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ssrf_allows_ip_literal_public() {
        let url = Url::parse("https://1.1.1.1/").unwrap();
        let result = check_ssrf(&url, false).await;
        assert!(result.is_ok());
    }
}
