use crate::security::ssrf::{is_blocked_hostname, is_ipv4_address, is_ipv6_address, is_private_ip};
use std::collections::HashSet;
use std::net::IpAddr;
use url::Url;

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

/// URL validation options
#[derive(Debug, Clone, Default)]
pub struct UrlValidationOptions {
    pub max_length: usize,
    pub require_https: bool,
    pub allowed_domains: HashSet<String>,
    pub blocked_domains: HashSet<String>,
}

impl UrlValidationOptions {
    pub fn new(max_length: usize) -> Self {
        Self {
            max_length,
            require_https: false,
            allowed_domains: HashSet::new(),
            blocked_domains: HashSet::new(),
        }
    }

    pub fn with_https_only(mut self, require_https: bool) -> Self {
        self.require_https = require_https;
        self
    }

    pub fn with_allowed_domains(mut self, domains: HashSet<String>) -> Self {
        self.allowed_domains = domains;
        self
    }

    pub fn with_blocked_domains(mut self, domains: HashSet<String>) -> Self {
        self.blocked_domains = domains;
        self
    }
}

/// Validate URL format and basic security checks (before DNS resolution)
/// Legacy function for backward compatibility
pub fn validate_url_format(url_string: &str, max_length: usize) -> ValidationResult {
    validate_url_format_with_options(url_string, &UrlValidationOptions::new(max_length))
}

/// Validate URL format with full options
pub fn validate_url_format_with_options(
    url_string: &str,
    options: &UrlValidationOptions,
) -> ValidationResult {
    // Check URL length
    if url_string.is_empty() || url_string.len() > options.max_length {
        return ValidationResult::failure("URL too long or empty");
    }

    // Parse URL
    let parsed = match Url::parse(url_string) {
        Ok(url) => url,
        Err(_) => return ValidationResult::failure("Invalid URL format"),
    };

    // Check protocol
    let scheme = parsed.scheme();
    if options.require_https {
        if scheme != "https" {
            return ValidationResult::failure("Only HTTPS URLs are allowed");
        }
    } else if scheme != "http" && scheme != "https" {
        return ValidationResult::failure("Protocol not allowed");
    }

    // Get hostname
    let hostname = match parsed.host_str() {
        Some(h) => h.to_lowercase(),
        None => return ValidationResult::failure("No hostname in URL"),
    };

    // Check blocked hostnames (SSRF protection)
    if is_blocked_hostname(&hostname) {
        return ValidationResult::failure("Hostname not allowed");
    }

    // Check for IP address in hostname (both IPv4 and IPv6)
    if is_ipv4_address(&hostname) || is_ipv6_address(&hostname) {
        let addr_str = hostname.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = addr_str.parse::<IpAddr>() {
            if is_private_ip(ip) {
                return ValidationResult::failure("Private IP addresses not allowed");
            }
        }
    }

    // Check domain allowlist (if configured)
    if !options.allowed_domains.is_empty()
        && !is_domain_in_list(&hostname, &options.allowed_domains)
    {
        return ValidationResult::failure("Domain not in allowlist");
    }

    // Check domain blocklist (if configured)
    if !options.blocked_domains.is_empty() && is_domain_in_list(&hostname, &options.blocked_domains)
    {
        return ValidationResult::failure("Domain is blocked");
    }

    ValidationResult::success(parsed, hostname)
}

/// Check if a hostname matches any domain in a list
/// Supports exact match and wildcard subdomain match (*.example.com)
fn is_domain_in_list(hostname: &str, domains: &HashSet<String>) -> bool {
    // Exact match
    if domains.contains(hostname) {
        return true;
    }

    // Check for wildcard matches
    for domain in domains {
        if let Some(pattern) = domain.strip_prefix("*.") {
            // Check if hostname ends with .pattern or is exactly pattern
            if hostname == pattern || hostname.ends_with(&format!(".{}", pattern)) {
                return true;
            }
        }
    }

    false
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

    #[test]
    fn test_https_only() {
        let options = UrlValidationOptions::new(2048).with_https_only(true);

        let result = validate_url_format_with_options("https://example.com/image.jpg", &options);
        assert!(result.valid);

        let result = validate_url_format_with_options("http://example.com/image.jpg", &options);
        assert!(!result.valid);
        assert_eq!(
            result.reason,
            Some("Only HTTPS URLs are allowed".to_string())
        );
    }

    #[test]
    fn test_domain_allowlist() {
        let mut allowed = HashSet::new();
        allowed.insert("images.example.com".to_string());
        allowed.insert("*.cdn.net".to_string());

        let options = UrlValidationOptions::new(2048).with_allowed_domains(allowed);

        // Exact match should work
        let result =
            validate_url_format_with_options("https://images.example.com/img.jpg", &options);
        assert!(result.valid);

        // Wildcard match should work
        let result = validate_url_format_with_options("https://us.cdn.net/img.jpg", &options);
        assert!(result.valid);

        // Non-allowed domain should fail
        let result = validate_url_format_with_options("https://other.com/img.jpg", &options);
        assert!(!result.valid);
        assert_eq!(result.reason, Some("Domain not in allowlist".to_string()));
    }

    #[test]
    fn test_domain_blocklist() {
        let mut blocked = HashSet::new();
        blocked.insert("evil.com".to_string());
        blocked.insert("*.malware.net".to_string());

        let options = UrlValidationOptions::new(2048).with_blocked_domains(blocked);

        // Blocked domain should fail
        let result = validate_url_format_with_options("https://evil.com/img.jpg", &options);
        assert!(!result.valid);

        // Wildcard blocked should fail
        let result = validate_url_format_with_options("https://sub.malware.net/img.jpg", &options);
        assert!(!result.valid);

        // Non-blocked domain should work
        let result = validate_url_format_with_options("https://good.com/img.jpg", &options);
        assert!(result.valid);
    }

    #[test]
    fn test_domain_in_list() {
        let mut list = HashSet::new();
        list.insert("example.com".to_string());
        list.insert("*.cdn.net".to_string());

        assert!(is_domain_in_list("example.com", &list));
        assert!(is_domain_in_list("cdn.net", &list));
        assert!(is_domain_in_list("us.cdn.net", &list));
        assert!(is_domain_in_list("deep.sub.cdn.net", &list));
        assert!(!is_domain_in_list("other.com", &list));
        assert!(!is_domain_in_list("notcdn.net", &list));
    }
}
