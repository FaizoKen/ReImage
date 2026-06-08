use ipnetwork::IpNetwork;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// Private/internal IP ranges to block (SSRF protection)
static PRIVATE_RANGES: Lazy<Vec<IpNetwork>> = Lazy::new(|| {
    vec![
        // IPv4 ranges
        "127.0.0.0/8".parse().unwrap(),        // Loopback
        "10.0.0.0/8".parse().unwrap(),         // Private Class A
        "172.16.0.0/12".parse().unwrap(),      // Private Class B
        "192.168.0.0/16".parse().unwrap(),     // Private Class C
        "169.254.0.0/16".parse().unwrap(),     // Link-local (AWS/Azure/GCP metadata)
        "0.0.0.0/8".parse().unwrap(),          // Current network
        "100.64.0.0/10".parse().unwrap(),      // Carrier-grade NAT
        "192.0.0.0/24".parse().unwrap(),       // IETF Protocol Assignments
        "192.0.2.0/24".parse().unwrap(),       // TEST-NET-1
        "198.18.0.0/15".parse().unwrap(),      // Benchmarking
        "198.51.100.0/24".parse().unwrap(),    // TEST-NET-2
        "203.0.113.0/24".parse().unwrap(),     // TEST-NET-3
        "224.0.0.0/4".parse().unwrap(),        // Multicast
        "240.0.0.0/4".parse().unwrap(),        // Reserved
        "255.255.255.255/32".parse().unwrap(), // Broadcast
        // IPv6 ranges
        "::1/128".parse().unwrap(),       // IPv6 loopback
        "::/128".parse().unwrap(),        // IPv6 unspecified
        "::ffff:0:0/96".parse().unwrap(), // IPv4-mapped IPv6
        "64:ff9b::/96".parse().unwrap(),  // IPv4/IPv6 translation
        "100::/64".parse().unwrap(),      // Discard prefix
        "2001::/32".parse().unwrap(),     // Teredo
        "2001:10::/28".parse().unwrap(),  // ORCHID
        "2001:20::/28".parse().unwrap(),  // ORCHIDv2
        "2001:db8::/32".parse().unwrap(), // Documentation
        "2002::/16".parse().unwrap(),     // 6to4
        "fc00::/7".parse().unwrap(),      // IPv6 unique local
        "fe80::/10".parse().unwrap(),     // IPv6 link-local
        "ff00::/8".parse().unwrap(),      // IPv6 multicast
    ]
});

/// Blocked hostnames (cloud metadata endpoints, localhost variants)
static BLOCKED_HOSTNAMES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    // Localhost variants
    set.insert("localhost");
    set.insert("localhost.localdomain");
    set.insert("ip6-localhost");
    set.insert("ip6-loopback");
    set.insert("127.0.0.1");
    set.insert("0.0.0.0");
    set.insert("[::1]");
    set.insert("[::]");

    // AWS metadata endpoints
    set.insert("169.254.169.254");
    set.insert("fd00:ec2::254");
    set.insert("instance-data");
    set.insert("instance-data.ec2.internal");

    // GCP metadata endpoints
    set.insert("metadata.google.internal");
    set.insert("metadata.goog");
    set.insert("metadata");

    // Azure metadata endpoints
    set.insert("169.254.169.253");
    set.insert("metadata.azure.com");
    set.insert("management.azure.com");

    // DigitalOcean metadata
    set.insert("169.254.169.254"); // Same as AWS

    // Oracle Cloud metadata
    set.insert("169.254.169.254"); // Same as AWS

    // Kubernetes internal
    set.insert("kubernetes");
    set.insert("kubernetes.default");
    set.insert("kubernetes.default.svc");
    set.insert("kubernetes.default.svc.cluster.local");

    // Alibaba Cloud metadata
    set.insert("100.100.100.200");

    // Hetzner Cloud metadata
    set.insert("169.254.169.254"); // Same as AWS

    set
});

/// Check if an IP address is in a private/blocked range
/// Also handles IPv6-mapped IPv4 addresses (::ffff:x.x.x.x)
pub fn is_private_ip(ip: IpAddr) -> bool {
    // Handle IPv6-mapped IPv4 addresses (potential bypass vector)
    let effective_ip = match ip {
        IpAddr::V6(v6) => {
            // Check for IPv6-mapped IPv4 (::ffff:x.x.x.x)
            if let Some(v4) = v6.to_ipv4_mapped() {
                IpAddr::V4(v4)
            } else if v6.segments()[..6] == [0, 0, 0, 0, 0, 0] {
                // Check for IPv4-compatible IPv6 (deprecated but still possible)
                // Format: ::x.x.x.x
                let segments = v6.segments();
                let ipv4 = Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    segments[6] as u8,
                    (segments[7] >> 8) as u8,
                    segments[7] as u8,
                );
                IpAddr::V4(ipv4)
            } else {
                IpAddr::V6(v6)
            }
        }
        v4 => v4,
    };

    PRIVATE_RANGES
        .iter()
        .any(|range| range.contains(effective_ip))
}

/// Check if a hostname is blocked
pub fn is_blocked_hostname(hostname: &str) -> bool {
    BLOCKED_HOSTNAMES.contains(hostname.to_lowercase().as_str())
}

/// Check if a string looks like an IPv4 address
pub fn is_ipv4_address(s: &str) -> bool {
    // Simple check: 4 numbers separated by dots
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|part| part.parse::<u8>().is_ok())
}

/// Check if a string looks like an IPv6 address
pub fn is_ipv6_address(s: &str) -> bool {
    // IPv6 addresses contain colons and may be wrapped in brackets
    let addr = s.trim_start_matches('[').trim_end_matches(']');
    addr.contains(':') && addr.parse::<Ipv6Addr>().is_ok()
}

/// Check if an IP string (v4 or v6) is a private address
pub fn is_private_ip_string(s: &str) -> bool {
    let addr = s.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = addr.parse::<IpAddr>() {
        is_private_ip(ip)
    } else {
        false
    }
}

/// Validate that a hostname/IP is safe to connect to
pub fn validate_ip(ip_str: &str) -> Result<(), String> {
    match IpAddr::from_str(ip_str) {
        Ok(ip) => {
            if is_private_ip(ip) {
                Err("Private IP addresses not allowed".to_string())
            } else {
                Ok(())
            }
        }
        Err(_) => Err("Invalid IP address".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_private_ip_detection_ipv4() {
        // Private ranges
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("169.254.1.1".parse().unwrap())); // Link-local (metadata)
        assert!(is_private_ip("100.64.0.1".parse().unwrap())); // Carrier-grade NAT

        // Public IPs should pass
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn test_private_ip_detection_ipv6() {
        // IPv6 private ranges
        assert!(is_private_ip("::1".parse().unwrap())); // Loopback
        assert!(is_private_ip("fc00::1".parse().unwrap())); // Unique local
        assert!(is_private_ip("fe80::1".parse().unwrap())); // Link-local
        assert!(is_private_ip("ff02::1".parse().unwrap())); // Multicast

        // Public IPv6 should pass
        assert!(!is_private_ip("2607:f8b0:4004:800::200e".parse().unwrap()));
    }

    #[test]
    fn test_ipv6_mapped_ipv4_bypass() {
        // These are IPv6-mapped IPv4 addresses that could bypass naive checks
        // ::ffff:127.0.0.1 should be detected as private
        let mapped_loopback: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_private_ip(mapped_loopback));

        // ::ffff:10.0.0.1 should be detected as private
        let mapped_private: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(is_private_ip(mapped_private));

        // ::ffff:169.254.169.254 (AWS metadata) should be detected as private
        let mapped_metadata: IpAddr = "::ffff:169.254.169.254".parse().unwrap();
        assert!(is_private_ip(mapped_metadata));

        // ::ffff:8.8.8.8 (public) should NOT be private
        let mapped_public: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(!is_private_ip(mapped_public));
    }

    #[test]
    fn test_blocked_hostnames() {
        // Localhost variants
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("LOCALHOST"));
        assert!(is_blocked_hostname("localhost.localdomain"));

        // Cloud metadata endpoints
        assert!(is_blocked_hostname("metadata.google.internal"));
        assert!(is_blocked_hostname("metadata.azure.com"));
        assert!(is_blocked_hostname("169.254.169.254"));
        assert!(is_blocked_hostname("instance-data"));

        // Kubernetes
        assert!(is_blocked_hostname("kubernetes.default.svc"));

        // Valid hostnames should pass
        assert!(!is_blocked_hostname("example.com"));
        assert!(!is_blocked_hostname("images.example.com"));
    }

    #[test]
    fn test_ipv4_address_detection() {
        assert!(is_ipv4_address("192.168.1.1"));
        assert!(is_ipv4_address("10.0.0.1"));
        assert!(is_ipv4_address("255.255.255.255"));

        assert!(!is_ipv4_address("example.com"));
        assert!(!is_ipv4_address("::1"));
        assert!(!is_ipv4_address("192.168.1"));
        assert!(!is_ipv4_address("192.168.1.1.1"));
    }

    #[test]
    fn test_ipv6_address_detection() {
        assert!(is_ipv6_address("::1"));
        assert!(is_ipv6_address("2001:db8::1"));
        assert!(is_ipv6_address("[::1]"));
        assert!(is_ipv6_address("::ffff:127.0.0.1"));

        assert!(!is_ipv6_address("192.168.1.1"));
        assert!(!is_ipv6_address("example.com"));
    }
}
