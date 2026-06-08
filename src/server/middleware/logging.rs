use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header::HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use std::{net::SocketAddr, sync::Arc, time::Instant};
use uuid::Uuid;

use crate::config::Config;
use crate::server::middleware::rate_limit::extract_client_ip;

/// Header name for request ID
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Request logging middleware
/// Adds request ID to all requests and logs request/response details
pub async fn request_logging_middleware(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    // Generate or extract request ID
    let request_id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Add request ID to request headers for downstream use
    req.headers_mut().insert(
        X_REQUEST_ID.clone(),
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    // Skip logging if disabled
    if !config.enable_request_logging {
        let mut response = next.run(req).await;
        // Still add request ID to response
        response.headers_mut().insert(
            X_REQUEST_ID.clone(),
            HeaderValue::from_str(&request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
        );
        return response;
    }

    // Extract request details for logging
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let client_ip = extract_client_ip(&req, Some(&addr), &config.trusted_proxies);

    // Extract source URL host for logging (if present in query)
    let src_host = uri.query().and_then(|q| {
        q.split('&')
            .find(|p| p.starts_with("src="))
            .and_then(|p| p.strip_prefix("src="))
            .and_then(|url| {
                // URL decode and extract host
                percent_decode(url)
                    .and_then(|decoded| url::Url::parse(&decoded).ok())
                    .and_then(|parsed| parsed.host_str().map(|h| h.to_string()))
            })
    });

    let start = Instant::now();

    // Process the request
    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status().as_u16();

    // Determine cache status from response headers
    let cache_status = response
        .headers()
        .get("x-cache")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("NONE");

    // Log the request
    tracing::info!(
        request_id = %request_id,
        method = %method,
        path = %path,
        client_ip = %client_ip,
        src_host = src_host.as_deref().unwrap_or("-"),
        status = status,
        duration_ms = duration.as_millis(),
        cache = %cache_status,
        "request processed"
    );

    // Add request ID to response headers
    let mut response = response;
    response.headers_mut().insert(
        X_REQUEST_ID.clone(),
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    response
}

/// Simple percent decoding for URL parameters
fn percent_decode(input: &str) -> Option<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                } else {
                    return None;
                }
            } else {
                return None;
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percent_decode() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com%2Fimage.jpg"),
            Some("https://example.com/image.jpg".to_string())
        );
        assert_eq!(
            percent_decode("hello+world"),
            Some("hello world".to_string())
        );
        assert_eq!(percent_decode("simple"), Some("simple".to_string()));
    }
}
