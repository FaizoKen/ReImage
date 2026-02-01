use axum::{
    middleware,
    routing::get,
    Router,
};
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::timeout::TimeoutLayer;

use crate::cache::CacheManager;
use crate::config::Config;
use crate::http_client::HttpClient;
use crate::server::middleware::{
    auth_middleware, cors_middleware, rate_limit_middleware, referer_middleware,
    request_logging_middleware, security_headers_middleware,
};
use crate::server::routes::{favicon, handle_image, health, robots_txt, AppState};

/// Create the Axum application router
pub fn create_app(config: Arc<Config>) -> Router {
    // Initialize shared state
    let http_client = Arc::new(HttpClient::new(&config));
    let cache_manager = Arc::new(CacheManager::new(&config));

    let state = AppState {
        config: config.clone(),
        http_client,
        cache_manager,
    };

    // Build middleware stack
    // Order matters: outermost middleware runs first
    // 1. Request logging (adds request ID, logs request/response)
    // 2. Rate limiting (by IP or API key)
    // 3. CORS (handles preflight, adds headers)
    // 4. Authentication (API key or HMAC signature)
    // 5. Referer validation (anti-hotlinking)
    // 6. Security headers (X-Frame-Options, CSP, etc.)
    // 7. Timeout (request timeout)
    let middleware_stack = ServiceBuilder::new()
        .layer(TimeoutLayer::new(config.request_timeout))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            security_headers_middleware_with_state,
        ));

    // Build router with all middleware layers
    Router::new()
        .route("/image", get(handle_image))
        .route("/health", get(health))
        .route("/robots.txt", get(robots_txt))
        .route("/favicon.ico", get(favicon))
        .layer(middleware_stack)
        // Apply state-dependent middleware
        .layer(middleware::from_fn_with_state(
            config.clone(),
            referer_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            cors_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            request_logging_middleware,
        ))
        .with_state(state)
}

/// Security headers middleware that accepts state (for future configurability)
async fn security_headers_middleware_with_state(
    _state: axum::extract::State<Arc<Config>>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    security_headers_middleware(req, next).await
}
