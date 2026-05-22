use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Bad Request: {0}")]
    BadRequest(String),

    #[error("Not Found: {0}")]
    NotFound(String),

    #[error("Request Timeout")]
    Timeout,

    #[error("Payload Too Large")]
    PayloadTooLarge,

    #[error("Too Many Requests")]
    RateLimited,

    #[error("Service Unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Internal Server Error: {0}")]
    Internal(String),

    #[error("Image processing error: {0}")]
    ImageProcessing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("SSRF blocked: {0}")]
    SsrfBlocked(String),

    #[error("Fetch failed: {0}")]
    FetchFailed(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "Bad Request", msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "Not Found", msg.clone()),
            AppError::Timeout => (
                StatusCode::REQUEST_TIMEOUT,
                "Request Timeout",
                "The request took too long to process".to_string(),
            ),
            AppError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
                "Request body exceeds the allowed limit".to_string(),
            ),
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "Too Many Requests",
                "Rate limit exceeded. Please slow down.".to_string(),
            ),
            AppError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                msg.clone(),
            ),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "An unexpected error occurred".to_string(),
            ),
            AppError::ImageProcessing(msg) => {
                if msg.contains("unsupported") || msg.contains("corrupt") || msg.contains("Invalid") {
                    (
                        StatusCode::BAD_REQUEST,
                        "Bad Request",
                        "Image format unsupported or corrupt".to_string(),
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal Server Error",
                        "An unexpected error occurred".to_string(),
                    )
                }
            }
            AppError::InvalidUrl(msg) => (
                StatusCode::BAD_REQUEST,
                "Bad Request",
                msg.clone(),
            ),
            AppError::SsrfBlocked(_) => (
                StatusCode::BAD_REQUEST,
                "Bad Request",
                "Invalid or disallowed source URL".to_string(),
            ),
            AppError::FetchFailed(msg) => {
                if msg.contains("not found") || msg.contains("404") {
                    (
                        StatusCode::NOT_FOUND,
                        "Not Found",
                        "Source image not found or inaccessible".to_string(),
                    )
                } else {
                    (
                        StatusCode::BAD_REQUEST,
                        "Bad Request",
                        "Failed to fetch image".to_string(),
                    )
                }
            }
        };

        // Log internal errors
        if matches!(self, AppError::Internal(_)) {
            tracing::error!("Internal error: {:?}", self);
        }

        let body = Json(json!({
            "error": error_type,
            "message": message
        }));

        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
