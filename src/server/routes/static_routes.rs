use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};

/// Robots.txt endpoint - discourage crawlers
pub async fn robots_txt() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain")],
        "User-agent: *\nDisallow: /",
    )
}

/// Favicon endpoint - return 204 No Content
pub async fn favicon() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}
