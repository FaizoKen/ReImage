use bytes::Bytes;
use once_cell::sync::OnceCell;
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::config::Config;
use crate::http_client::dns::DnsResolver;
use crate::http_client::retry::{is_permanent_error, is_retryable_error, RetryConfig};
use crate::image::validation::{is_valid_content_type, is_valid_image_buffer};
use crate::security::url_validator::{validate_resolved_ip, validate_url_format};

static HTTP_CLIENT: OnceCell<Arc<HttpClient>> = OnceCell::new();

#[derive(Debug)]
pub enum FetchError {
    /// Permanent error - should not retry
    Permanent(String),
    /// Transient error - can retry
    Transient(String),
    /// Not found
    NotFound,
}

pub struct HttpClient {
    client: Client,
    dns_resolver: Arc<DnsResolver>,
    retry_config: RetryConfig,
    max_download_size: usize,
    max_url_length: usize,
    fetch_timeout_main: Duration,
    fetch_timeout_overlay: Duration,
}

impl HttpClient {
    pub fn new(config: &Config) -> Self {
        let client = Client::builder()
            .connect_timeout(config.agent_connect_timeout)
            .timeout(config.agent_body_timeout)
            .pool_max_idle_per_host(config.agent_connections)
            .pool_idle_timeout(config.agent_keepalive_timeout)
            .user_agent("ImageProxy/1.0")
            .danger_accept_invalid_certs(!config.agent_reject_unauthorized)
            .gzip(true)
            .deflate(true)
            .build()
            .expect("Failed to build HTTP client");

        let dns_resolver = DnsResolver::global(config);

        let retry_config = RetryConfig::new(
            config.fetch_max_retries,
            config.fetch_retry_delay,
            config.fetch_retry_backoff_multiplier,
        );

        Self {
            client,
            dns_resolver,
            retry_config,
            max_download_size: config.max_download_size,
            max_url_length: config.max_url_length,
            fetch_timeout_main: config.fetch_timeout_main,
            fetch_timeout_overlay: config.fetch_timeout_overlay,
        }
    }

    /// Fetch image from URL with retry logic
    pub async fn fetch_image(&self, url: &str, is_overlay: bool) -> Result<Bytes, FetchError> {
        // Validate URL format first
        let validation = validate_url_format(url, self.max_url_length);
        if !validation.valid {
            return Err(FetchError::Permanent(
                validation.reason.unwrap_or_else(|| "Invalid URL".to_string()),
            ));
        }

        let hostname = validation.hostname.unwrap();

        // Resolve DNS and validate IP
        let ip = self
            .dns_resolver
            .lookup(&hostname)
            .await
            .map_err(|e| FetchError::Permanent(e))?;

        validate_resolved_ip(ip).map_err(|e| FetchError::Permanent(e))?;

        // Perform fetch with retry
        let timeout = if is_overlay {
            self.fetch_timeout_overlay
        } else {
            self.fetch_timeout_main
        };

        let mut last_error = FetchError::Transient("Unknown error".to_string());

        for attempt in 0..=self.retry_config.max_retries {
            if attempt > 0 {
                let delay = self.retry_config.get_delay(attempt - 1);
                tracing::info!(
                    url = url,
                    attempt = attempt,
                    delay_ms = delay.as_millis(),
                    "Retrying image fetch"
                );
                sleep(delay).await;
            }

            match self.fetch_once(url, timeout).await {
                Ok(buffer) => {
                    if attempt > 0 {
                        tracing::info!(url = url, attempt = attempt, "Image fetch succeeded after retry");
                    }
                    return Ok(buffer);
                }
                Err(err) => {
                    last_error = err;

                    // Check if error is permanent (don't retry)
                    if let FetchError::Permanent(_) = &last_error {
                        tracing::warn!(url = url, "Image fetch failed (permanent error)");
                        return Err(last_error);
                    }

                    if let FetchError::NotFound = &last_error {
                        return Err(last_error);
                    }

                    // Log retry attempt
                    if attempt < self.retry_config.max_retries {
                        tracing::warn!(
                            url = url,
                            attempt = attempt,
                            error = ?last_error,
                            "Image fetch failed, will retry"
                        );
                    }
                }
            }
        }

        tracing::warn!(
            url = url,
            retries = self.retry_config.max_retries,
            "Image fetch failed after all retries"
        );
        Err(last_error)
    }

    async fn fetch_once(&self, url: &str, timeout: Duration) -> Result<Bytes, FetchError> {
        let response = tokio::time::timeout(timeout, self.client.get(url).send())
            .await
            .map_err(|_| FetchError::Transient("Request timeout".to_string()))?
            .map_err(|e| {
                if is_retryable_error(&e, None) {
                    FetchError::Transient(e.to_string())
                } else {
                    FetchError::Permanent(e.to_string())
                }
            })?;

        let status = response.status();

        if status == StatusCode::NOT_FOUND {
            return Err(FetchError::NotFound);
        }

        if !status.is_success() {
            let status_code = status.as_u16();
            if status_code >= 500 || status_code == 429 {
                return Err(FetchError::Transient(format!("HTTP {}", status_code)));
            }
            return Err(FetchError::Permanent(format!("HTTP {}", status_code)));
        }

        // Validate content type
        if let Some(content_type) = response.headers().get("content-type") {
            let ct = content_type.to_str().unwrap_or("");
            if !is_valid_content_type(ct) {
                return Err(FetchError::Permanent("Invalid content type".to_string()));
            }
        }

        // Check content length
        if let Some(length) = response.content_length() {
            if length as usize > self.max_download_size {
                return Err(FetchError::Permanent("Image too large".to_string()));
            }
        }

        // Download body
        let bytes = response
            .bytes()
            .await
            .map_err(|e| FetchError::Transient(e.to_string()))?;

        // Check actual size
        if bytes.len() > self.max_download_size {
            return Err(FetchError::Permanent(
                "Image too large after download".to_string(),
            ));
        }

        // Validate magic bytes
        if !is_valid_image_buffer(&bytes) {
            return Err(FetchError::Permanent("Invalid image magic bytes".to_string()));
        }

        Ok(bytes)
    }

    /// Get or initialize the global HTTP client
    pub fn global(config: &Config) -> Arc<HttpClient> {
        HTTP_CLIENT
            .get_or_init(|| Arc::new(HttpClient::new(config)))
            .clone()
    }
}
