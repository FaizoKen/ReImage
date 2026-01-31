use axum::{
    body::Body,
    extract::ConnectInfo,
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
use moka::sync::Cache;
use once_cell::sync::Lazy;
use serde_json::json;
use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
    time::Duration,
};

/// Rate limiters per IP address
static RATE_LIMITERS: Lazy<Cache<IpAddr, Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>>> =
    Lazy::new(|| {
        Cache::builder()
            .max_capacity(100_000)
            .time_to_idle(Duration::from_secs(120))
            .build()
    });

/// Get or create rate limiter for IP
fn get_rate_limiter(ip: IpAddr) -> Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>> {
    RATE_LIMITERS.get_with(ip, || {
        // 100 requests per minute
        let quota = Quota::per_minute(NonZeroU32::new(100).unwrap());
        Arc::new(RateLimiter::direct(quota))
    })
}

/// Extract client IP from request headers or connection
pub fn extract_client_ip<B>(req: &Request<B>, conn_info: Option<&SocketAddr>) -> IpAddr {
    // Try X-Forwarded-For header first
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(ip_str) = value.split(',').next() {
                if let Ok(ip) = ip_str.trim().parse::<IpAddr>() {
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

    // Fall back to connection info
    conn_info
        .map(|addr| addr.ip())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap())
}

/// Rate limiting middleware
pub async fn rate_limit_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let client_ip = extract_client_ip(&req, Some(&addr));
    let limiter = get_rate_limiter(client_ip);

    match limiter.check() {
        Ok(_) => next.run(req).await,
        Err(_) => {
            let body = Json(json!({
                "error": "Too Many Requests",
                "message": "Rate limit exceeded. Please slow down.",
                "retryAfter": 60
            }));

            (StatusCode::TOO_MANY_REQUESTS, body).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_creation() {
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        let limiter = get_rate_limiter(ip);

        // Should allow first request
        assert!(limiter.check().is_ok());
    }
}
