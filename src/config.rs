use std::env;
use std::time::Duration;

fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_int(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .map(|v| v == "true" || v == "1")
        .unwrap_or(default)
}

#[derive(Debug, Clone)]
pub struct Config {
    // Server
    pub port: u16,
    pub connection_timeout: Duration,
    pub body_limit: usize,
    pub request_timeout: Duration,

    // Security / Limits
    pub max_url_length: usize,
    pub max_text_length: usize,
    pub max_overlays: usize,
    pub max_texts: usize,
    pub max_dimension: u32,
    pub min_dimension: u32,
    pub max_radius: u32,

    // HTTP Agent
    pub agent_connect_timeout: Duration,
    pub agent_headers_timeout: Duration,
    pub agent_body_timeout: Duration,
    pub agent_connections: usize,
    pub agent_pipelining: usize,
    pub agent_keepalive_timeout: Duration,
    pub agent_keepalive_max_timeout: Duration,
    pub agent_reject_unauthorized: bool,

    // Fetch timeouts
    pub fetch_timeout_main: Duration,
    pub fetch_timeout_overlay: Duration,

    // Retry configuration
    pub fetch_max_retries: u32,
    pub fetch_retry_delay: Duration,
    pub fetch_retry_backoff_multiplier: f64,

    // DNS configuration
    pub dns_cache_ttl: Duration,
    pub dns_lookup_timeout: Duration,

    // Download
    pub max_download_size: usize,

    // Cache
    pub output_cache_size_mb: u64,
    pub output_cache_ttl: Duration,
    pub source_cache_size_mb: u64,
    pub overlay_cache_max: u64,
    pub overlay_cache_ttl: Duration,
    pub mask_cache_max: u64,
    pub mask_cache_ttl: Duration,

    // Output
    pub webp_quality: u8,
    pub webp_effort: u8,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            // Server
            port: env_u32("PORT", 80) as u16,
            connection_timeout: Duration::from_millis(env_u64("CONNECTION_TIMEOUT", 5000)),
            body_limit: env_u64("BODY_LIMIT", 1024) as usize,
            request_timeout: Duration::from_millis(env_u64("REQUEST_TIMEOUT_MS", 30000)),

            // Security / Limits
            max_url_length: env_u64("MAX_URL_LENGTH", 2048) as usize,
            max_text_length: env_u64("MAX_TEXT_LENGTH", 500) as usize,
            max_overlays: env_u64("MAX_OVERLAYS", 10) as usize,
            max_texts: env_u64("MAX_TEXTS", 10) as usize,
            max_dimension: env_u32("MAX_DIMENSION", 8000),
            min_dimension: env_u32("MIN_DIMENSION", 1),
            max_radius: env_u32("MAX_RADIUS", 4000),

            // HTTP Agent
            agent_connect_timeout: Duration::from_millis(env_u64("AGENT_CONNECT_TIMEOUT", 10000)),
            agent_headers_timeout: Duration::from_millis(env_u64("AGENT_HEADERS_TIMEOUT", 10000)),
            agent_body_timeout: Duration::from_millis(env_u64("AGENT_BODY_TIMEOUT", 30000)),
            agent_connections: env_u64("AGENT_CONNECTIONS", 128) as usize,
            agent_pipelining: env_u64("AGENT_PIPELINING", 10) as usize,
            agent_keepalive_timeout: Duration::from_millis(env_u64("AGENT_KEEPALIVE_TIMEOUT", 30000)),
            agent_keepalive_max_timeout: Duration::from_millis(env_u64("AGENT_KEEPALIVE_MAX_TIMEOUT", 60000)),
            agent_reject_unauthorized: env_bool("AGENT_REJECT_UNAUTHORIZED", false),

            // Fetch timeouts
            fetch_timeout_main: Duration::from_millis(env_u64("FETCH_TIMEOUT_MAIN", 15000)),
            fetch_timeout_overlay: Duration::from_millis(env_u64("FETCH_TIMEOUT_OVERLAY", 10000)),

            // Retry configuration
            fetch_max_retries: env_u32("FETCH_MAX_RETRIES", 3),
            fetch_retry_delay: Duration::from_millis(env_u64("FETCH_RETRY_DELAY_MS", 500)),
            fetch_retry_backoff_multiplier: env_f64("FETCH_RETRY_BACKOFF_MULTIPLIER", 1.5),

            // DNS configuration
            dns_cache_ttl: Duration::from_millis(env_u64("DNS_CACHE_TTL_MS", 300000)),
            dns_lookup_timeout: Duration::from_millis(env_u64("DNS_LOOKUP_TIMEOUT_MS", 5000)),

            // Download
            max_download_size: env_u64("MAX_DOWNLOAD_SIZE_MB", 10) as usize * 1024 * 1024,

            // Cache
            output_cache_size_mb: env_u64("OUTPUT_CACHE_SIZE_MB", 750),
            output_cache_ttl: Duration::from_secs(env_u64("OUTPUT_CACHE_TTL_SEC", 600)),
            source_cache_size_mb: env_u64("SOURCE_CACHE_SIZE_MB", 500),
            overlay_cache_max: env_u64("OVERLAY_CACHE_MAX", 200),
            overlay_cache_ttl: Duration::from_secs(env_u64("OVERLAY_CACHE_TTL_SEC", 1800)),
            mask_cache_max: env_u64("MASK_CACHE_MAX", 500),
            mask_cache_ttl: Duration::from_secs(env_u64("MASK_CACHE_TTL_SEC", 3600)),

            // Output
            webp_quality: env_u32("WEBP_QUALITY", 80) as u8,
            webp_effort: env_u32("WEBP_EFFORT", 0) as u8,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env()
    }
}
