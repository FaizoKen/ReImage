use axum::{
    body::Body,
    extract::State,
    http::{header::REFERER, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::sync::Arc;
use url::Url;

use crate::config::Config;

/// Referer validation middleware
/// Prevents hotlinking by checking the Referer header against an allowlist
pub async fn referer_middleware(
    State(config): State<Arc<Config>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Skip check if referer validation is not enabled
    if !config.referer_check_enabled {
        return next.run(req).await;
    }

    // Skip check if no allowed referers configured (allow all)
    if config.allowed_referers.is_empty() {
        return next.run(req).await;
    }

    // Extract referer header
    let referer = req
        .headers()
        .get(REFERER)
        .and_then(|h| h.to_str().ok());

    // Allow requests with no referer (direct access, privacy tools, etc.)
    // This is configurable - some may want to require referer
    if referer.is_none() {
        // You could make this configurable: REQUIRE_REFERER=true
        // For now, allow requests without referer to be more permissive
        return next.run(req).await;
    }

    let referer = referer.unwrap();

    // Parse referer URL to extract host
    let referer_host = match Url::parse(referer) {
        Ok(url) => url.host_str().map(|s| s.to_lowercase()),
        Err(_) => None,
    };

    // If we can't parse the referer, reject
    let referer_host = match referer_host {
        Some(h) => h,
        None => return forbidden_response("Invalid referer"),
    };

    // Check if referer is allowed
    if !is_referer_allowed(&referer_host, &config.allowed_referers) {
        tracing::warn!(
            referer = %referer,
            referer_host = %referer_host,
            "Blocked request with unauthorized referer"
        );
        return forbidden_response("Referer not allowed");
    }

    next.run(req).await
}

/// Check if a referer host is in the allowed list
fn is_referer_allowed(referer_host: &str, allowed_referers: &[String]) -> bool {
    for allowed in allowed_referers {
        // Exact match
        if allowed.eq_ignore_ascii_case(referer_host) {
            return true;
        }

        // Wildcard subdomain match (e.g., *.example.com)
        if let Some(pattern) = allowed.strip_prefix("*.") {
            // Check if referer_host ends with the pattern
            if referer_host.eq_ignore_ascii_case(pattern) {
                // Exact match with the base domain
                return true;
            }
            if referer_host.ends_with(&format!(".{}", pattern.to_lowercase())) {
                // Subdomain match
                return true;
            }
        }
    }

    false
}

fn forbidden_response(message: &str) -> Response {
    let body = Json(json!({
        "error": "Forbidden",
        "message": message
    }));
    (StatusCode::FORBIDDEN, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_referer_match() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_referer_allowed("example.com", &allowed));
        assert!(is_referer_allowed("EXAMPLE.COM", &allowed));
        assert!(!is_referer_allowed("other.com", &allowed));
    }

    #[test]
    fn test_wildcard_referer_match() {
        let allowed = vec!["*.example.com".to_string()];

        // Should match subdomains
        assert!(is_referer_allowed("sub.example.com", &allowed));
        assert!(is_referer_allowed("deep.sub.example.com", &allowed));

        // Should match base domain too
        assert!(is_referer_allowed("example.com", &allowed));

        // Should not match other domains
        assert!(!is_referer_allowed("notexample.com", &allowed));
        assert!(!is_referer_allowed("other.com", &allowed));
    }

    #[test]
    fn test_multiple_allowed_referers() {
        let allowed = vec![
            "example.com".to_string(),
            "*.trusted.com".to_string(),
        ];

        assert!(is_referer_allowed("example.com", &allowed));
        assert!(is_referer_allowed("sub.trusted.com", &allowed));
        assert!(!is_referer_allowed("untrusted.com", &allowed));
    }
}
