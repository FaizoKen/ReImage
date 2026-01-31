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
use crate::server::middleware::{rate_limit_middleware, security_headers_middleware};
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
    let middleware_stack = ServiceBuilder::new()
        .layer(TimeoutLayer::new(config.request_timeout))
        .layer(middleware::from_fn(security_headers_middleware));

    // Build router
    Router::new()
        .route("/image", get(handle_image))
        .route("/health", get(health))
        .route("/robots.txt", get(robots_txt))
        .route("/favicon.ico", get(favicon))
        .layer(middleware_stack)
        .layer(middleware::from_fn(rate_limit_middleware))
        .with_state(state)
}
