use ipnetwork::IpNetwork;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;

/// Private/internal IP ranges to block (SSRF protection)
static PRIVATE_RANGES: Lazy<Vec<IpNetwork>> = Lazy::new(|| {
    vec![
        "127.0.0.0/8".parse().unwrap(),     // Loopback
        "10.0.0.0/8".parse().unwrap(),      // Private Class A
        "172.16.0.0/12".parse().unwrap(),   // Private Class B
        "192.168.0.0/16".parse().unwrap(),  // Private Class C
        "169.254.0.0/16".parse().unwrap(),  // Link-local
        "0.0.0.0/8".parse().unwrap(),       // Current network
        "100.64.0.0/10".parse().unwrap(),   // Carrier-grade NAT
        "192.0.0.0/24".parse().unwrap(),    // IETF Protocol Assignments
        "192.0.2.0/24".parse().unwrap(),    // TEST-NET-1
        "198.51.100.0/24".parse().unwrap(), // TEST-NET-2
        "203.0.113.0/24".parse().unwrap(),  // TEST-NET-3
        "224.0.0.0/4".parse().unwrap(),     // Multicast
        "240.0.0.0/4".parse().unwrap(),     // Reserved
        "255.255.255.255/32".parse().unwrap(), // Broadcast
        "::1/128".parse().unwrap(),         // IPv6 loopback
        "fc00::/7".parse().unwrap(),        // IPv6 unique local
        "fe80::/10".parse().unwrap(),       // IPv6 link-local
        "ff00::/8".parse().unwrap(),        // IPv6 multicast
    ]
});

/// Blocked hostnames
static BLOCKED_HOSTNAMES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("localhost");
    set.insert("localhost.localdomain");
    set.insert("ip6-localhost");
    set.insert("ip6-loopback");
    set.insert("metadata.google.internal");
    set.insert("metadata");
    set.insert("169.254.169.254");
    set
});

/// Check if an IP address is in a private/blocked range
pub fn is_private_ip(ip: IpAddr) -> bool {
    PRIVATE_RANGES.iter().any(|range| range.contains(ip))
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
    parts.iter().all(|part| {
        part.parse::<u8>().is_ok()
    })
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
    fn test_private_ip_detection() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("169.254.1.1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn test_blocked_hostnames() {
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("LOCALHOST"));
        assert!(is_blocked_hostname("metadata.google.internal"));
        assert!(!is_blocked_hostname("example.com"));
    }
}
