use axum::{
    body::Body,
    http::{header::HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response,
};

/// Security headers middleware
/// Adds security headers to all responses matching Node.js implementation
pub async fn security_headers_middleware(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;

    let headers = response.headers_mut();

    // X-Content-Type-Options: nosniff
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );

    // X-Frame-Options: DENY
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );

    // X-XSS-Protection: 1; mode=block
    headers.insert(
        HeaderName::from_static("x-xss-protection"),
        HeaderValue::from_static("1; mode=block"),
    );

    // Content-Security-Policy
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; img-src 'self'"),
    );

    // Referrer-Policy: no-referrer
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );

    // Permissions-Policy: interest-cohort=()
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("interest-cohort=()"),
    );

    response
}
