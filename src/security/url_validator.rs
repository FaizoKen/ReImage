use crate::security::ssrf::{is_blocked_hostname, is_ipv4_address, is_private_ip};
use std::collections::HashSet;
use std::net::IpAddr;
use once_cell::sync::Lazy;
use url::Url;

static ALLOWED_PROTOCOLS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("http");
    set.insert("https");
    set
});

#[derive(Debug)]
pub struct ValidationResult {
    pub valid: bool,
    pub reason: Option<String>,
    pub url: Option<Url>,
    pub hostname: Option<String>,
}

impl ValidationResult {
    pub fn success(url: Url, hostname: String) -> Self {
        Self {
            valid: true,
            reason: None,
            url: Some(url),
            hostname: Some(hostname),
        }
    }

    pub fn failure(reason: &str) -> Self {
        Self {
            valid: false,
            reason: Some(reason.to_string()),
            url: None,
            hostname: None,
        }
    }
}

/// Validate URL format and basic security checks (before DNS resolution)
pub fn validate_url_format(url_string: &str, max_length: usize) -> ValidationResult {
    // Check URL length
    if url_string.is_empty() || url_string.len() > max_length {
        return ValidationResult::failure("URL too long or empty");
    }

    // Parse URL
    let parsed = match Url::parse(url_string) {
        Ok(url) => url,
        Err(_) => return ValidationResult::failure("Invalid URL format"),
    };

    // Check protocol
    if !ALLOWED_PROTOCOLS.contains(parsed.scheme()) {
        return ValidationResult::failure("Protocol not allowed");
    }

    // Get hostname
    let hostname = match parsed.host_str() {
        Some(h) => h.to_lowercase(),
        None => return ValidationResult::failure("No hostname in URL"),
    };

    // Check blocked hostnames
    if is_blocked_hostname(&hostname) {
        return ValidationResult::failure("Hostname not allowed");
    }

    // Check for IP address in hostname
    if is_ipv4_address(&hostname) {
        if let Ok(ip) = hostname.parse::<IpAddr>() {
            if is_private_ip(ip) {
                return ValidationResult::failure("Private IP addresses not allowed");
            }
        }
    }

    ValidationResult::success(parsed, hostname)
}

/// Validate that a resolved IP address is safe
pub fn validate_resolved_ip(ip: IpAddr) -> Result<(), String> {
    if is_private_ip(ip) {
        Err("URL resolves to private IP".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_urls() {
        let result = validate_url_format("https://example.com/image.jpg", 2048);
        assert!(result.valid);
    }

    #[test]
    fn test_blocked_protocols() {
        let result = validate_url_format("file:///etc/passwd", 2048);
        assert!(!result.valid);

        let result = validate_url_format("ftp://example.com/file", 2048);
        assert!(!result.valid);
    }

    #[test]
    fn test_blocked_hostnames() {
        let result = validate_url_format("http://localhost/image.jpg", 2048);
        assert!(!result.valid);

        let result = validate_url_format("http://169.254.169.254/metadata", 2048);
        assert!(!result.valid);
    }

    #[test]
    fn test_private_ips() {
        let result = validate_url_format("http://127.0.0.1/image.jpg", 2048);
        assert!(!result.valid);

        let result = validate_url_format("http://10.0.0.1/image.jpg", 2048);
        assert!(!result.valid);

        let result = validate_url_format("http://192.168.1.1/image.jpg", 2048);
        assert!(!result.valid);
    }
}
