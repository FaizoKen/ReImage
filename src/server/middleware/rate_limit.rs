use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use ipnetwork::IpNetwork;
use moka::sync::Cache;
use once_cell::sync::Lazy;
use serde_json::json;
use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
    time::Duration,
};

use crate::config::Config;

/// Keyless, in-memory, default-clock rate limiter — the concrete `governor`
/// type we use everywhere. Aliased to keep the `Cache` signatures readable.
type DirectRateLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// Rate limiters per IP address
static RATE_LIMITERS: Lazy<Cache<IpAddr, Arc<DirectRateLimiter>>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(100_000)
        .time_to_idle(Duration::from_secs(120))
        .build()
});

/// Rate limiters per API key
static API_KEY_RATE_LIMITERS: Lazy<Cache<String, Arc<DirectRateLimiter>>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(10_000)
        .time_to_idle(Duration::from_secs(120))
        .build()
});

/// Get or create rate limiter for IP
fn get_rate_limiter(
    ip: IpAddr,
    rate_limit: u32,
) -> Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>> {
    RATE_LIMITERS.get_with(ip, || {
        let quota =
            Quota::per_minute(NonZeroU32::new(rate_limit).unwrap_or(NonZeroU32::new(100).unwrap()));
        Arc::new(RateLimiter::direct(quota))
    })
}

/// Get or create rate limiter for API key
fn get_api_key_rate_limiter(
    key: &str,
    rate_limit: u32,
) -> Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>> {
    API_KEY_RATE_LIMITERS.get_with(key.to_string(), || {
        let quota =
            Quota::per_minute(NonZeroU32::new(rate_limit).unwrap_or(NonZeroU32::new(100).unwrap()));
        Arc::new(RateLimiter::direct(quota))
    })
}

/// Check if an IP is in the trusted proxy list
fn is_trusted_proxy(ip: IpAddr, trusted_proxies: &[IpNetwork]) -> bool {
    trusted_proxies.iter().any(|network| network.contains(ip))
}

/// Extract client IP from request headers or connection
/// Only trusts X-Forwarded-For if the direct connection is from a trusted proxy
pub fn extract_client_ip<B>(
    req: &Request<B>,
    conn_info: Option<&SocketAddr>,
    trusted_proxies: &[IpNetwork],
) -> IpAddr {
    let direct_ip = conn_info
        .map(|addr| addr.ip())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

    // Only trust forwarded headers if the direct connection is from a trusted proxy
    if !trusted_proxies.is_empty() && is_trusted_proxy(direct_ip, trusted_proxies) {
        // Try X-Forwarded-For header
        // Format: X-Forwarded-For: client, proxy1, proxy2
        // We want the rightmost IP that is NOT a trusted proxy
        if let Some(forwarded) = req.headers().get("x-forwarded-for") {
            if let Ok(value) = forwarded.to_str() {
                let ips: Vec<&str> = value.split(',').map(|s| s.trim()).collect();

                // Walk backwards to find the first non-trusted-proxy IP
                for ip_str in ips.iter().rev() {
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        if !is_trusted_proxy(ip, trusted_proxies) {
                            return ip;
                        }
                    }
                }

                // If all IPs are trusted proxies, use the first one (original client)
                if let Some(first_ip) = ips.first() {
                    if let Ok(ip) = first_ip.parse::<IpAddr>() {
                        return ip;
                    }
                }
            }
        }

        // Try X-Real-IP header
        if let Some(real_ip) = req.headers().get("x-real-ip") {
            if let Ok(value) = real_ip.to_str() {
                if let Ok(ip) = value.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
    }

    // Fall back to direct connection IP
    direct_ip
}

/// Extract API key from request (for per-key rate limiting)
fn extract_api_key(req: &Request<Body>) -> Option<String> {
    // Try X-API-Key header
    if let Some(api_key_header) = req.headers().get("x-api-key") {
        if let Ok(value) = api_key_header.to_str() {
            return Some(value.to_string());
        }
    }

    // Try Authorization header (Bearer token)
    if let Some(auth_header) = req.headers().get("authorization") {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Rate limiting middleware with trusted proxy support
pub async fn rate_limit_middleware(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Health probes (polled at a fixed cadence by upstream status pages)
    // shouldn't eat into the per-IP request budget.
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let client_ip = extract_client_ip(&req, Some(&addr), &config.trusted_proxies);

    // Determine which rate limiter to use
    let is_rate_limited = if config.rate_limit_by_key {
        // Rate limit by API key if present, otherwise by IP
        if let Some(api_key) = extract_api_key(&req) {
            let limiter = get_api_key_rate_limiter(&api_key, config.rate_limit_per_minute);
            limiter.check().is_err()
        } else {
            let limiter = get_rate_limiter(client_ip, config.rate_limit_per_minute);
            limiter.check().is_err()
        }
    } else {
        // Rate limit by IP only
        let limiter = get_rate_limiter(client_ip, config.rate_limit_per_minute);
        limiter.check().is_err()
    };

    if is_rate_limited {
        let body = Json(json!({
            "error": "Too Many Requests",
            "message": "Rate limit exceeded. Please slow down.",
            "retryAfter": 60
        }));

        return (StatusCode::TOO_MANY_REQUESTS, body).into_response();
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn test_rate_limiter_creation() {
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        let limiter = get_rate_limiter(ip, 100);

        // Should allow first request
        assert!(limiter.check().is_ok());
    }

    #[test]
    fn test_trusted_proxy_check() {
        let trusted: Vec<IpNetwork> = vec![
            "10.0.0.0/8".parse().unwrap(),
            "192.168.1.0/24".parse().unwrap(),
        ];

        assert!(is_trusted_proxy("10.0.0.1".parse().unwrap(), &trusted));
        assert!(is_trusted_proxy("192.168.1.100".parse().unwrap(), &trusted));
        assert!(!is_trusted_proxy("8.8.8.8".parse().unwrap(), &trusted));
        assert!(!is_trusted_proxy("192.168.2.1".parse().unwrap(), &trusted));
    }

    #[test]
    fn test_extract_client_ip_no_trusted_proxies() {
        let req = Request::builder()
            .header("x-forwarded-for", "1.2.3.4, 5.6.7.8")
            .body(Body::empty())
            .unwrap();

        let conn_addr: SocketAddr = "9.9.9.9:12345".parse().unwrap();
        let trusted: Vec<IpNetwork> = vec![];

        // Without trusted proxies, should return direct connection IP
        let ip = extract_client_ip(&req, Some(&conn_addr), &trusted);
        assert_eq!(ip, "9.9.9.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_extract_client_ip_with_trusted_proxies() {
        let req = Request::builder()
            .header("x-forwarded-for", "1.2.3.4, 10.0.0.5")
            .body(Body::empty())
            .unwrap();

        let conn_addr: SocketAddr = "10.0.0.1:12345".parse().unwrap();
        let trusted: Vec<IpNetwork> = vec!["10.0.0.0/8".parse().unwrap()];

        // With trusted proxy, should return first non-trusted IP
        let ip = extract_client_ip(&req, Some(&conn_addr), &trusted);
        assert_eq!(ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }
}
