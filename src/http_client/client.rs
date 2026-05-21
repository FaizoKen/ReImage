use bytes::Bytes;
use once_cell::sync::OnceCell;
use reqwest::{Client, StatusCode};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::config::Config;
use crate::http_client::dns::DnsResolver;
use crate::http_client::retry::{is_retryable_error, RetryConfig};
use crate::image::validation::{is_valid_content_type, is_valid_image_buffer};
use crate::security::url_validator::{
    validate_resolved_ip, validate_url_format_with_options, UrlValidationOptions,
};

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
    validation_options: UrlValidationOptions,
    fetch_timeout_main: Duration,
    fetch_timeout_overlay: Duration,
    /// Lowercased SELF_HOST (e.g. `reimage.faizo.net`) used to short-circuit
    /// fetches that target our own public hostname, bypassing the Cloudflare
    /// round-trip. None disables the optimisation.
    self_host: Option<String>,
    /// Local port the axum server is listening on — paired with self_host.
    self_port: u16,
}

impl HttpClient {
    pub fn new(config: &Config) -> Self {
        // Note: We perform DNS resolution ourselves for SSRF protection
        // and pass the resolved IP to requests via the resolve() method per-request
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

        // Build URL validation options
        let validation_options = UrlValidationOptions::new(config.max_url_length)
            .with_https_only(config.require_https)
            .with_allowed_domains(config.allowed_domains.clone())
            .with_blocked_domains(config.blocked_domains.clone());

        Self {
            client,
            dns_resolver,
            retry_config,
            max_download_size: config.max_download_size,
            validation_options,
            fetch_timeout_main: config.fetch_timeout_main,
            fetch_timeout_overlay: config.fetch_timeout_overlay,
            self_host: config.self_host.clone(),
            self_port: config.port,
        }
    }

    /// Create a client with DNS pinning for a specific hostname -> IP mapping
    /// This prevents DNS rebinding attacks
    fn create_pinned_client(
        &self,
        hostname: &str,
        resolved_ip: IpAddr,
        port: u16,
    ) -> Client {
        let socket_addr = SocketAddr::new(resolved_ip, port);

        Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("ImageProxy/1.0")
            // Pin DNS: force this hostname to resolve to our validated IP
            .resolve(hostname, socket_addr)
            .gzip(true)
            .deflate(true)
            .build()
            .unwrap_or_else(|_| self.client.clone())
    }

    /// Fetch image from URL with retry logic
    pub async fn fetch_image(&self, url: &str, is_overlay: bool) -> Result<Bytes, FetchError> {
        // Validate URL format first (includes HTTPS-only and domain checks)
        let validation = validate_url_format_with_options(url, &self.validation_options);
        if !validation.valid {
            return Err(FetchError::Permanent(
                validation.reason.unwrap_or_else(|| "Invalid URL".to_string()),
            ));
        }

        let parsed_url = validation.url.unwrap();
        let hostname = validation.hostname.unwrap();

        // Same-host short-circuit: if this URL targets our own public hostname,
        // bypass DNS + Cloudflare and fetch the rewritten URL over loopback.
        // /image is excluded to prevent self-recursive loops.
        if let Some(rewrite) =
            maybe_rewrite_self(&parsed_url, &hostname, self.self_host.as_deref(), self.self_port)?
        {
            let timeout = if is_overlay {
                self.fetch_timeout_overlay
            } else {
                self.fetch_timeout_main
            };
            let loopback_ip: IpAddr = "127.0.0.1".parse().unwrap();
            // No retries on the loopback hop — failures here are local-router
            // rejections, retrying won't help.
            return self
                .fetch_once_with_resolved_ip(
                    &rewrite,
                    "127.0.0.1",
                    loopback_ip,
                    self.self_port,
                    timeout,
                )
                .await;
        }

        // Resolve DNS and validate IP (SSRF protection)
        let resolved_ip = self
            .dns_resolver
            .lookup(&hostname)
            .await
            .map_err(|e| FetchError::Permanent(e))?;

        validate_resolved_ip(resolved_ip).map_err(|e| FetchError::Permanent(e))?;

        // Get the port from the URL
        let port = parsed_url.port_or_known_default().unwrap_or(
            if parsed_url.scheme() == "https" { 443 } else { 80 }
        );

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

            // DNS rebinding protection: use the resolved IP directly
            // This prevents DNS rebinding attacks where DNS returns a different IP on retry
            match self.fetch_once_with_resolved_ip(url, &hostname, resolved_ip, port, timeout).await {
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

    /// Fetch with DNS rebinding protection
    /// Uses the pre-resolved IP to prevent DNS rebinding attacks
    async fn fetch_once_with_resolved_ip(
        &self,
        url: &str,
        hostname: &str,
        resolved_ip: IpAddr,
        port: u16,
        timeout: Duration,
    ) -> Result<Bytes, FetchError> {
        // Create a client with DNS pinned to our validated IP
        // This prevents DNS rebinding attacks where a malicious DNS server
        // returns a public IP first, then a private IP on subsequent queries
        let pinned_client = self.create_pinned_client(hostname, resolved_ip, port);

        let response = tokio::time::timeout(timeout, pinned_client.get(url).send())
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

/// If `parsed_url` targets our own public hostname, return a loopback-rewritten
/// URL string (`http://127.0.0.1:{self_port}{path?query}`). Returns `Ok(None)`
/// when `self_host` is unset or the URL targets a different host. Returns
/// `Err(Permanent)` if the URL targets our own `/image` endpoint (recursive
/// loop guard).
///
/// Pure function — extracted so the rewrite logic can be unit-tested without
/// spinning up a server.
fn maybe_rewrite_self(
    parsed_url: &url::Url,
    hostname: &str,
    self_host: Option<&str>,
    self_port: u16,
) -> Result<Option<String>, FetchError> {
    let Some(self_host) = self_host else {
        return Ok(None);
    };
    if !hostname.eq_ignore_ascii_case(self_host) {
        return Ok(None);
    }
    if parsed_url.path() == "/image" {
        return Err(FetchError::Permanent(
            "self-recursive /image fetch not allowed".to_string(),
        ));
    }
    let path_and_query = match parsed_url.query() {
        Some(q) => format!("{}?{}", parsed_url.path(), q),
        None => parsed_url.path().to_string(),
    };
    Ok(Some(format!(
        "http://127.0.0.1:{}{}",
        self_port, path_and_query
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn parse(u: &str) -> Url {
        Url::parse(u).unwrap()
    }

    #[test]
    fn rewrite_skipped_when_self_host_unset() {
        let url = parse("https://reimage.faizo.net/gradient?c=5865F2");
        let out = maybe_rewrite_self(&url, "reimage.faizo.net", None, 8080).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn rewrite_skipped_for_different_host() {
        let url = parse("https://example.com/image.png");
        let out = maybe_rewrite_self(
            &url,
            "example.com",
            Some("reimage.faizo.net"),
            8080,
        )
        .unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn rewrite_preserves_path_and_query() {
        let url = parse("https://reimage.faizo.net/gradient?c=5865F2&w=600");
        let out = maybe_rewrite_self(
            &url,
            "reimage.faizo.net",
            Some("reimage.faizo.net"),
            8080,
        )
        .unwrap();
        assert_eq!(
            out.as_deref(),
            Some("http://127.0.0.1:8080/gradient?c=5865F2&w=600")
        );
    }

    #[test]
    fn rewrite_handles_no_query() {
        let url = parse("https://reimage.faizo.net/health");
        let out = maybe_rewrite_self(
            &url,
            "reimage.faizo.net",
            Some("reimage.faizo.net"),
            8080,
        )
        .unwrap();
        assert_eq!(out.as_deref(), Some("http://127.0.0.1:8080/health"));
    }

    #[test]
    fn rewrite_is_case_insensitive() {
        let url = parse("https://ReImage.Faizo.Net/gradient?c=ff0000");
        let out = maybe_rewrite_self(
            &url,
            "reimage.faizo.net",
            Some("REIMAGE.FAIZO.NET"),
            8080,
        )
        .unwrap();
        assert_eq!(
            out.as_deref(),
            Some("http://127.0.0.1:8080/gradient?c=ff0000")
        );
    }

    #[test]
    fn rewrite_rejects_self_image_loop() {
        let url = parse("https://reimage.faizo.net/image?src=https://example.com/x.png");
        let result = maybe_rewrite_self(
            &url,
            "reimage.faizo.net",
            Some("reimage.faizo.net"),
            8080,
        );
        match result {
            Err(FetchError::Permanent(msg)) => assert!(msg.contains("recursive")),
            other => panic!("expected Permanent error, got {:?}", other),
        }
    }
}
