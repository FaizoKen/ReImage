use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use moka::future::Cache;
use once_cell::sync::OnceCell;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use crate::config::Config;
use crate::security::ssrf::is_private_ip;

static DNS_RESOLVER: OnceCell<Arc<DnsResolver>> = OnceCell::new();

pub struct DnsResolver {
    resolver: TokioAsyncResolver,
    cache: Cache<String, IpAddr>,
    lookup_timeout: Duration,
}

impl DnsResolver {
    pub fn new(config: &Config) -> Self {
        // Configure resolver to prefer IPv4
        let mut opts = ResolverOpts::default();
        opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4thenIpv6;
        opts.cache_size = 1024;
        opts.use_hosts_file = false; // Don't use hosts file for security

        let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), opts);

        let cache = Cache::builder()
            .max_capacity(10000)
            .time_to_live(config.dns_cache_ttl)
            .build();

        Self {
            resolver,
            cache,
            lookup_timeout: config.dns_lookup_timeout,
        }
    }

    /// Lookup hostname and return first IP address
    /// Returns error if hostname resolves to private IP
    pub async fn lookup(&self, hostname: &str) -> Result<IpAddr, String> {
        // Check cache first
        if let Some(ip) = self.cache.get(hostname).await {
            // Re-validate cached IP (in case it's somehow private)
            if is_private_ip(ip) {
                return Err("URL resolves to private IP".to_string());
            }
            return Ok(ip);
        }

        // Perform DNS lookup with timeout
        let lookup_result = timeout(self.lookup_timeout, self.resolver.lookup_ip(hostname)).await;

        match lookup_result {
            Ok(Ok(response)) => {
                // Get first IP address (IPv4 preferred due to resolver config)
                if let Some(ip) = response.iter().next() {
                    // Check if IP is private
                    if is_private_ip(ip) {
                        return Err("URL resolves to private IP".to_string());
                    }

                    // Cache the result
                    self.cache.insert(hostname.to_string(), ip).await;
                    Ok(ip)
                } else {
                    Err("No IP addresses found for hostname".to_string())
                }
            }
            Ok(Err(_)) => Err("Could not resolve hostname".to_string()),
            Err(_) => Err("DNS lookup timeout".to_string()),
        }
    }

    /// Get or initialize the global DNS resolver
    pub fn global(config: &Config) -> Arc<DnsResolver> {
        DNS_RESOLVER
            .get_or_init(|| Arc::new(DnsResolver::new(config)))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dns_lookup() {
        let config = Config::default();
        let resolver = DnsResolver::new(&config);

        // This should work (public DNS)
        let result = resolver.lookup("google.com").await;
        assert!(result.is_ok());
    }
}
