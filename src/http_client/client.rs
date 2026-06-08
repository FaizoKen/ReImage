use bytes::Bytes;
use hmac::{Hmac, Mac};
use moka::sync::Cache as SyncCache;
use once_cell::sync::{Lazy, OnceCell};
use reqwest::{Client, StatusCode};
use sha2::Sha256;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

use crate::config::Config;
use crate::http_client::dns::DnsResolver;
use crate::http_client::retry::{is_retryable_error, RetryConfig};
use crate::image::validation::{is_valid_content_type, is_valid_image_buffer};
use crate::security::url_validator::{
    validate_resolved_ip, validate_url_format_with_options, UrlValidationOptions,
};

type HmacSha256 = Hmac<Sha256>;

/// Validity window for self-signed loopback URLs. The receiver is our own
/// axum stack a millisecond away — we only need the signature to outlive
/// the local handler hop, not survive an embed cache.
const LOOPBACK_SIG_TTL_SECS: u64 = 60;

static HTTP_CLIENT: OnceCell<Arc<HttpClient>> = OnceCell::new();

/// Pool of DNS-pinned reqwest clients, keyed by (hostname, resolved IP, port).
/// Building a `reqwest::Client` is expensive (TLS context, connector,
/// connection pool). Re-using the pinned client across requests to the same
/// (host, ip, port) preserves keep-alive connections and avoids the
/// per-request TLS handshake cost. Bounded so we don't accumulate clients
/// indefinitely; old entries expire on a DNS-cache-ish timescale.
static PINNED_CLIENTS: Lazy<SyncCache<(String, IpAddr, u16), Client>> = Lazy::new(|| {
    SyncCache::builder()
        .max_capacity(1024)
        .time_to_idle(Duration::from_secs(300))
        .build()
});

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
    /// HMAC secret for self-signing SELF_HOST loopback URLs. Required when
    /// REQUIRE_AUTH=true: the loopback fetch re-enters the axum stack and
    /// hits the same auth middleware, so without this the inner request
    /// 401s. None when REQUIRE_AUTH is off (loopback skips signing too).
    hmac_secret: Option<Vec<u8>>,
    /// Timeouts used when (re)building a pinned client. Captured from config
    /// so cached clients honour configured limits.
    agent_connect_timeout: Duration,
    agent_body_timeout: Duration,
    agent_keepalive_timeout: Duration,
    agent_pool_max_idle: usize,
    agent_reject_unauthorized: bool,
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
            // Only carry the secret if auth is actually enforced; this keeps
            // the dev / public-mode path allocation-free.
            hmac_secret: if config.require_auth {
                config
                    .hmac_secret
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.as_bytes().to_vec())
            } else {
                None
            },
            agent_connect_timeout: config.agent_connect_timeout,
            agent_body_timeout: config.agent_body_timeout,
            agent_keepalive_timeout: config.agent_keepalive_timeout,
            agent_pool_max_idle: config.agent_connections,
            agent_reject_unauthorized: config.agent_reject_unauthorized,
        }
    }

    /// Get-or-build a client with DNS pinned for (hostname, ip, port).
    /// Reuses the connection pool across requests to the same origin so we
    /// don't pay the TLS handshake + pool-setup cost every fetch.
    fn get_pinned_client(&self, hostname: &str, resolved_ip: IpAddr, port: u16) -> Client {
        let key = (hostname.to_string(), resolved_ip, port);
        let connect_timeout = self.agent_connect_timeout;
        let body_timeout = self.agent_body_timeout;
        let keepalive = self.agent_keepalive_timeout;
        let pool_max = self.agent_pool_max_idle;
        let reject_unauthorized = self.agent_reject_unauthorized;
        let fallback = self.client.clone();
        PINNED_CLIENTS.get_with(key, move || {
            let socket_addr = SocketAddr::new(resolved_ip, port);
            Client::builder()
                .connect_timeout(connect_timeout)
                .timeout(body_timeout)
                .pool_max_idle_per_host(pool_max)
                .pool_idle_timeout(keepalive)
                .user_agent("ImageProxy/1.0")
                .danger_accept_invalid_certs(!reject_unauthorized)
                // Pin DNS: force this hostname to resolve to our validated IP.
                .resolve(hostname, socket_addr)
                .gzip(true)
                .deflate(true)
                .build()
                .unwrap_or(fallback)
        })
    }

    /// Fetch image from URL with retry logic
    pub async fn fetch_image(&self, url: &str, is_overlay: bool) -> Result<Bytes, FetchError> {
        // Validate URL format first (includes HTTPS-only and domain checks)
        let validation = validate_url_format_with_options(url, &self.validation_options);
        if !validation.valid {
            return Err(FetchError::Permanent(
                validation
                    .reason
                    .unwrap_or_else(|| "Invalid URL".to_string()),
            ));
        }

        let parsed_url = validation.url.unwrap();
        let hostname = validation.hostname.unwrap();

        // Same-host short-circuit: if this URL targets our own public hostname,
        // bypass DNS + Cloudflare and fetch the rewritten URL over loopback.
        // /image is excluded to prevent self-recursive loops.
        if let Some(rewrite) = maybe_rewrite_self(
            &parsed_url,
            &hostname,
            self.self_host.as_deref(),
            self.self_port,
        )? {
            let timeout = if is_overlay {
                self.fetch_timeout_overlay
            } else {
                self.fetch_timeout_main
            };
            let loopback_ip: IpAddr = "127.0.0.1".parse().unwrap();
            // The loopback request re-enters the same axum stack, so the
            // auth middleware will inspect it just like an external call.
            // Sign here with a short-lived signature so REQUIRE_AUTH=true
            // doesn't 401 our own internal hop. No-op when auth is off.
            let signed_rewrite = sign_loopback_url(&rewrite, self.hmac_secret.as_deref());
            // No retries on the loopback hop — failures here are local-router
            // rejections, retrying won't help.
            return self
                .fetch_once_with_resolved_ip(
                    &signed_rewrite,
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
            .map_err(FetchError::Permanent)?;

        validate_resolved_ip(resolved_ip).map_err(FetchError::Permanent)?;

        // Get the port from the URL
        let port =
            parsed_url
                .port_or_known_default()
                .unwrap_or(if parsed_url.scheme() == "https" {
                    443
                } else {
                    80
                });

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
            match self
                .fetch_once_with_resolved_ip(url, &hostname, resolved_ip, port, timeout)
                .await
            {
                Ok(buffer) => {
                    if attempt > 0 {
                        tracing::info!(
                            url = url,
                            attempt = attempt,
                            "Image fetch succeeded after retry"
                        );
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
        // Use a cached, DNS-pinned client (built once per (host, ip, port))
        // so we preserve keep-alive connections and skip the TLS handshake
        // on subsequent fetches to the same origin.
        let pinned_client = self.get_pinned_client(hostname, resolved_ip, port);

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
            return Err(FetchError::Permanent(
                "Invalid image magic bytes".to_string(),
            ));
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

/// Append `expires` + `sig` query params to a loopback URL so it satisfies
/// the auth middleware on re-entry. Mirrors the canonical algorithm in
/// `server/middleware/auth.rs::verify_hmac_signature` exactly: parse query
/// pairs, drop any pre-existing `sig`/`expires`, append a fresh `expires`,
/// stable-sort by key, hash `"<path>?<joined>"` under HMAC-SHA256, hex-
/// encode.
///
/// When `secret` is `None` (auth disabled), returns the URL unchanged —
/// the public-mode dev path stays zero-overhead.
fn sign_loopback_url(url: &str, secret: Option<&[u8]>) -> String {
    let Some(secret) = secret else {
        return url.to_string();
    };

    // Find scheme://host boundary so we can isolate the path+query.
    let scheme_end = match url.find("://") {
        Some(i) => i + 3,
        None => return url.to_string(),
    };
    let rest = &url[scheme_end..];
    let path_start = match rest.find('/') {
        Some(i) => scheme_end + i,
        None => return url.to_string(),
    };

    let (path, query) = match url[path_start..].find('?') {
        Some(q) => (&url[path_start..path_start + q], &url[path_start + q + 1..]),
        // No query → reimage routes always need params, leave alone.
        None => return url.to_string(),
    };

    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + LOOPBACK_SIG_TTL_SECS;
    let expires_str = expires.to_string();

    let mut pairs: Vec<(&str, &str)> = Vec::with_capacity(16);
    for param in query.split('&') {
        if param.is_empty() {
            continue;
        }
        let (k, v) = match param.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        if k == "sig" || k == "expires" {
            continue;
        }
        pairs.push((k, v));
    }
    pairs.push(("expires", &expires_str));

    // Stable sort by key — matches the server's `sort_by(|a, b| a.0.cmp(b.0))`.
    pairs.sort_by(|a, b| a.0.cmp(b.0));

    let joined = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");
    let to_sign = format!("{}?{}", path, joined);

    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return url.to_string(),
    };
    mac.update(to_sign.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());

    format!("{}&expires={}&sig={}", url, expires, sig)
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
        let out = maybe_rewrite_self(&url, "example.com", Some("reimage.faizo.net"), 8080).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn rewrite_preserves_path_and_query() {
        let url = parse("https://reimage.faizo.net/gradient?c=5865F2&w=600");
        let out =
            maybe_rewrite_self(&url, "reimage.faizo.net", Some("reimage.faizo.net"), 8080).unwrap();
        assert_eq!(
            out.as_deref(),
            Some("http://127.0.0.1:8080/gradient?c=5865F2&w=600")
        );
    }

    #[test]
    fn rewrite_handles_no_query() {
        let url = parse("https://reimage.faizo.net/health");
        let out =
            maybe_rewrite_self(&url, "reimage.faizo.net", Some("reimage.faizo.net"), 8080).unwrap();
        assert_eq!(out.as_deref(), Some("http://127.0.0.1:8080/health"));
    }

    #[test]
    fn rewrite_is_case_insensitive() {
        let url = parse("https://ReImage.Faizo.Net/gradient?c=ff0000");
        let out =
            maybe_rewrite_self(&url, "reimage.faizo.net", Some("REIMAGE.FAIZO.NET"), 8080).unwrap();
        assert_eq!(
            out.as_deref(),
            Some("http://127.0.0.1:8080/gradient?c=ff0000")
        );
    }

    #[test]
    fn rewrite_rejects_self_image_loop() {
        let url = parse("https://reimage.faizo.net/image?src=https://example.com/x.png");
        let result = maybe_rewrite_self(&url, "reimage.faizo.net", Some("reimage.faizo.net"), 8080);
        match result {
            Err(FetchError::Permanent(msg)) => assert!(msg.contains("recursive")),
            other => panic!("expected Permanent error, got {:?}", other),
        }
    }

    /// Re-implement the server's verify step inline to assert that an URL
    /// produced by `sign_loopback_url` would be accepted by
    /// `verify_hmac_signature`. Keeps the two algorithms locked together: if
    /// either side drifts, this test fails.
    fn verify_like_server(url: &str, secret: &[u8]) -> bool {
        let scheme_end = url.find("://").unwrap() + 3;
        let rest = &url[scheme_end..];
        let path_start = scheme_end + rest.find('/').unwrap();
        let q_off = url[path_start..].find('?').unwrap();
        let path = &url[path_start..path_start + q_off];
        let query = &url[path_start + q_off + 1..];

        let mut sig: Option<String> = None;
        let mut expires: Option<&str> = None;
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        for param in query.split('&') {
            let (k, v) = param.split_once('=').unwrap();
            match k {
                "sig" => sig = Some(v.to_string()),
                "expires" => expires = Some(v),
                _ => pairs.push((k, v)),
            }
        }
        if let Some(e) = expires {
            pairs.push(("expires", e));
        }
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let joined = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        let to_verify = format!("{}?{}", path, joined);
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(to_verify.as_bytes());
        let expected = hex::encode(mac.finalize().into_bytes());
        sig.as_deref() == Some(expected.as_str())
    }

    #[test]
    fn loopback_signing_disabled_when_secret_unset() {
        let url = "http://127.0.0.1:8080/gradient?c=ff0000";
        let out = sign_loopback_url(url, None);
        assert_eq!(out, url);
    }

    #[test]
    fn loopback_signature_verifies_against_server_algorithm() {
        let secret = b"unit-test-secret-please-do-not-leak";
        let url = "http://127.0.0.1:8080/gradient?c=ff0000";
        let signed = sign_loopback_url(url, Some(secret));
        assert!(signed.starts_with(url));
        assert!(signed.contains("&expires="));
        assert!(signed.contains("&sig="));
        assert!(
            verify_like_server(&signed, secret),
            "signed URL must verify with the same algorithm the server uses; got {}",
            signed
        );
    }

    #[test]
    fn loopback_signature_preserves_multi_value_keys() {
        // /image uses `overlay[]=A&overlay[]=B` style; the stable sort must
        // keep the relative order of duplicate keys so client and server
        // produce the same canonical string.
        let secret = b"unit-test-secret-please-do-not-leak";
        let url = "http://127.0.0.1:8080/image?src=foo&overlay%5B%5D=A&overlay%5B%5D=B&maxw=480";
        let signed = sign_loopback_url(url, Some(secret));
        assert!(verify_like_server(&signed, secret));
    }
}
