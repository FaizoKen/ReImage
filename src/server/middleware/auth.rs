use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::sync::Arc;

use crate::config::Config;

type HmacSha256 = Hmac<Sha256>;

/// Authentication middleware
/// Validates API keys or HMAC-signed URLs
pub async fn auth_middleware(
    State(config): State<Arc<Config>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Skip auth if not required
    if !config.require_auth {
        return next.run(req).await;
    }

    // Try API key authentication first
    if let Some(api_key) = extract_api_key(&req) {
        if config.api_keys.contains(&api_key) {
            return next.run(req).await;
        }
    }

    // Try HMAC signature authentication
    if let Some(ref secret) = config.hmac_secret {
        if verify_hmac_signature(&req, secret) {
            return next.run(req).await;
        }
    }

    // Authentication failed
    unauthorized_response()
}

/// Extract API key from request
/// Supports: Authorization header (Bearer token), X-API-Key header, or query param
fn extract_api_key(req: &Request<Body>) -> Option<String> {
    // Try Authorization header (Bearer token)
    if let Some(auth_header) = req.headers().get(AUTHORIZATION) {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }

    // Try X-API-Key header
    if let Some(api_key_header) = req.headers().get("x-api-key") {
        if let Ok(value) = api_key_header.to_str() {
            return Some(value.to_string());
        }
    }

    // Try query parameter 'api_key'
    if let Some(query) = req.uri().query() {
        for param in query.split('&') {
            if let Some(key) = param.strip_prefix("api_key=") {
                return Some(key.to_string());
            }
        }
    }

    None
}

/// Verify HMAC signature in the request
/// Expected format: ?sig=<hmac>&expires=<timestamp>&...other_params
fn verify_hmac_signature(req: &Request<Body>, secret: &str) -> bool {
    let query = match req.uri().query() {
        Some(q) => q,
        None => return false,
    };

    // Parse query parameters
    let mut signature: Option<String> = None;
    let mut expires: Option<i64> = None;
    let mut params_to_sign: Vec<(&str, &str)> = Vec::new();

    for param in query.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            match key {
                "sig" => signature = Some(value.to_string()),
                "expires" => expires = value.parse().ok(),
                _ => params_to_sign.push((key, value)),
            }
        }
    }

    // Check if signature and expires are present
    let sig = match signature {
        Some(s) => s,
        None => return false,
    };

    // Check expiration
    if let Some(exp) = expires {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        if now > exp {
            return false; // Signature expired
        }

        params_to_sign.push(("expires", query.split("expires=").nth(1).unwrap_or("").split('&').next().unwrap_or("")));
    }

    // Sort parameters for consistent signing
    params_to_sign.sort_by(|a, b| a.0.cmp(b.0));

    // Build string to sign: path + sorted query params
    let path = req.uri().path();
    let params_string: String = params_to_sign
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    let string_to_sign = if params_string.is_empty() {
        path.to_string()
    } else {
        format!("{}?{}", path, params_string)
    };

    // Compute HMAC
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(string_to_sign.as_bytes());

    // Compare signatures (constant-time comparison via verify_slice)
    let expected_sig = hex::encode(mac.finalize().into_bytes());

    // Constant-time comparison to prevent timing attacks
    constant_time_compare(&sig, &expected_sig)
}

/// Constant-time string comparison to prevent timing attacks
fn constant_time_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

fn unauthorized_response() -> Response {
    let body = Json(json!({
        "error": "Unauthorized",
        "message": "Invalid or missing API key"
    }));
    (StatusCode::UNAUTHORIZED, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare("abc", "abc"));
        assert!(!constant_time_compare("abc", "abd"));
        assert!(!constant_time_compare("abc", "abcd"));
    }
}
