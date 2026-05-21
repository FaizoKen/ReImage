use axum::{
    body::Body,
    extract::State,
    http::{
        header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
            ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
            ACCESS_CONTROL_MAX_AGE, ORIGIN,
        },
        HeaderValue, Method, Request, StatusCode,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::config::Config;

/// CORS middleware
/// Handles preflight requests and adds CORS headers to responses
pub async fn cors_middleware(
    State(config): State<Arc<Config>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Skip CORS if not enabled
    if !config.cors_enabled {
        return next.run(req).await;
    }

    // Extract origin from request
    let origin = req
        .headers()
        .get(ORIGIN)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Handle preflight requests
    if req.method() == Method::OPTIONS {
        return handle_preflight(&config, origin.as_deref());
    }

    // Process the request
    let mut response = next.run(req).await;

    // Add CORS headers to response
    add_cors_headers(&config, &mut response, origin.as_deref());

    response
}

/// Handle CORS preflight requests
fn handle_preflight(config: &Config, origin: Option<&str>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();

    if let Some(origin) = origin {
        if is_origin_allowed(config, origin) {
            let headers = response.headers_mut();

            headers.insert(
                ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(origin).unwrap_or_else(|_| HeaderValue::from_static("*")),
            );

            headers.insert(
                ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, HEAD, OPTIONS"),
            );

            headers.insert(
                ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("Content-Type, Authorization, X-API-Key, X-Request-ID"),
            );

            headers.insert(
                ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("86400"), // 24 hours
            );

            if config.cors_allow_credentials {
                headers.insert(
                    ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
    }

    response
}

/// Add CORS headers to a response
fn add_cors_headers(config: &Config, response: &mut Response, origin: Option<&str>) {
    if let Some(origin) = origin {
        if is_origin_allowed(config, origin) {
            let headers = response.headers_mut();

            headers.insert(
                ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(origin).unwrap_or_else(|_| HeaderValue::from_static("*")),
            );

            if config.cors_allow_credentials {
                headers.insert(
                    ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
    }
}

/// Check if an origin is allowed
fn is_origin_allowed(config: &Config, origin: &str) -> bool {
    // If no origins configured, allow all (wildcard mode)
    if config.cors_allowed_origins.is_empty() {
        return true;
    }

    // Check if origin matches any allowed origin
    for allowed in &config.cors_allowed_origins {
        if allowed == "*" {
            return true;
        }

        // Exact match
        if allowed == origin {
            return true;
        }

        // Wildcard subdomain match (e.g., *.example.com)
        if let Some(pattern) = allowed.strip_prefix("*.") {
            if origin.ends_with(pattern) {
                // Make sure it's a subdomain match, not just a suffix match
                let prefix = origin.strip_suffix(pattern).unwrap_or("");
                if prefix.is_empty() || prefix.ends_with('.') {
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn test_config(origins: Vec<&str>) -> Config {
        Config {
            cors_enabled: true,
            cors_allowed_origins: origins.into_iter().map(|s| s.to_string()).collect(),
            cors_allow_credentials: false,
            // Default other fields
            port: 8080,
            connection_timeout: std::time::Duration::from_secs(5),
            body_limit: 1024,
            request_timeout: std::time::Duration::from_secs(30),
            max_url_length: 2048,
            max_text_length: 500,
            max_overlays: 10,
            max_texts: 10,
            max_dimension: 8000,
            min_dimension: 1,
            max_radius: 4000,
            max_bg_width: 480,
            max_overlay_size: 256,
            gradient_max_width: 480,
            gradient_max_height: 160,
            require_auth: false,
            api_keys: HashSet::new(),
            hmac_secret: None,
            require_https: false,
            allowed_domains: HashSet::new(),
            blocked_domains: HashSet::new(),
            self_host: None,
            referer_check_enabled: false,
            allowed_referers: vec![],
            rate_limit_per_minute: 100,
            rate_limit_by_key: false,
            trusted_proxies: vec![],
            agent_connect_timeout: std::time::Duration::from_secs(10),
            agent_headers_timeout: std::time::Duration::from_secs(10),
            agent_body_timeout: std::time::Duration::from_secs(30),
            agent_connections: 128,
            agent_pipelining: 10,
            agent_keepalive_timeout: std::time::Duration::from_secs(30),
            agent_keepalive_max_timeout: std::time::Duration::from_secs(60),
            agent_reject_unauthorized: true,
            fetch_timeout_main: std::time::Duration::from_secs(15),
            fetch_timeout_overlay: std::time::Duration::from_secs(10),
            fetch_max_retries: 3,
            fetch_retry_delay: std::time::Duration::from_millis(500),
            fetch_retry_backoff_multiplier: 1.5,
            dns_cache_ttl: std::time::Duration::from_secs(300),
            dns_lookup_timeout: std::time::Duration::from_secs(5),
            max_download_size: 10 * 1024 * 1024,
            max_output_size: 5 * 1024 * 1024,
            output_cache_size_mb: 750,
            output_cache_ttl: std::time::Duration::from_secs(600),
            source_cache_size_mb: 500,
            overlay_cache_max: 200,
            overlay_cache_ttl: std::time::Duration::from_secs(1800),
            mask_cache_max: 500,
            mask_cache_ttl: std::time::Duration::from_secs(3600),
            webp_quality: 80,
            webp_effort: 0,
            enable_request_logging: true,
        }
    }

    #[test]
    fn test_origin_allowed_wildcard() {
        let config = test_config(vec![]);
        assert!(is_origin_allowed(&config, "https://example.com"));
        assert!(is_origin_allowed(&config, "https://any.domain.com"));
    }

    #[test]
    fn test_origin_allowed_exact_match() {
        let config = test_config(vec!["https://example.com"]);
        assert!(is_origin_allowed(&config, "https://example.com"));
        assert!(!is_origin_allowed(&config, "https://other.com"));
    }

    #[test]
    fn test_origin_allowed_subdomain_wildcard() {
        let config = test_config(vec!["*.example.com"]);
        assert!(is_origin_allowed(&config, "https://sub.example.com"));
        assert!(is_origin_allowed(&config, "https://deep.sub.example.com"));
        assert!(!is_origin_allowed(&config, "https://notexample.com"));
    }
}
