use ipnetwork::IpNetwork;
use std::collections::HashSet;
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

fn env_vec(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn env_hash_set(key: &str) -> HashSet<String> {
    env_vec(key).into_iter().collect()
}

fn env_ip_networks(key: &str) -> Vec<IpNetwork> {
    env_vec(key)
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect()
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

    /// Hard cap on the background `blur[]` parameter (Gaussian blur radius in
    /// pixels). The composer UI tops out well below this.
    pub max_blur: u32,

    /// Hard cap on the background `bri[]` parameter (brightness percentage,
    /// 100 = unchanged).
    pub max_brightness: u32,

    /// Hard cap on the `/image` endpoint's `maxw[]` parameter (background
    /// resize target width). Tighter than `max_dimension` — typically the
    /// largest width any caller has any real reason to request.
    pub max_bg_width: u32,

    /// Hard cap on each `omaxw[]` / `omaxh[]` parameter (overlay resize
    /// target). Overlays are usually avatars (≤256 px); larger values waste
    /// CPU and bandwidth.
    pub max_overlay_size: u32,

    /// Hard caps for the `/gradient` endpoint, independent of the per-image
    /// `max_dimension`. Gradients are decorative defaults and don't need to
    /// be large; capping them prevents wasted CPU on absurd requests.
    pub gradient_max_width: u32,
    pub gradient_max_height: u32,

    // Authentication
    pub require_auth: bool,
    pub api_keys: HashSet<String>,
    pub hmac_secret: Option<String>,

    // URL Security
    pub require_https: bool,
    pub allowed_domains: HashSet<String>,
    pub blocked_domains: HashSet<String>,

    /// Public hostname this server answers on (e.g. `reimage.faizo.net`). When
    /// a fetch target's host matches this, we rewrite the upstream URL to
    /// `http://127.0.0.1:{port}` so the request never leaves the box —
    /// avoiding a Cloudflare round-trip when our own output (e.g. /gradient)
    /// is used as `src` for /image. Set via SELF_HOST env var. None disables.
    pub self_host: Option<String>,

    // CORS
    pub cors_enabled: bool,
    pub cors_allowed_origins: Vec<String>,
    pub cors_allow_credentials: bool,

    // Referer Validation
    pub referer_check_enabled: bool,
    pub allowed_referers: Vec<String>,

    // Rate Limiting
    pub rate_limit_per_minute: u32,
    pub rate_limit_by_key: bool,

    // Trusted Proxies (for X-Forwarded-For)
    pub trusted_proxies: Vec<IpNetwork>,

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
    pub max_output_size: usize,

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

    // Logging
    pub enable_request_logging: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            // Server
            port: env_u32("PORT", 8080) as u16,
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
            max_blur: env_u32("MAX_BLUR", 100),
            max_brightness: env_u32("MAX_BRIGHTNESS", 400),
            max_bg_width: env_u32("MAX_BG_WIDTH", 480),
            max_overlay_size: env_u32("MAX_OVERLAY_SIZE", 256),
            gradient_max_width: env_u32("GRADIENT_MAX_WIDTH", 480),
            gradient_max_height: env_u32("GRADIENT_MAX_HEIGHT", 160),

            // Authentication
            require_auth: env_bool("REQUIRE_AUTH", false),
            api_keys: env_hash_set("API_KEYS"),
            hmac_secret: env::var("HMAC_SECRET").ok().filter(|s| !s.is_empty()),

            // URL Security
            require_https: env_bool("REQUIRE_HTTPS", false),
            allowed_domains: env_hash_set("ALLOWED_DOMAINS"),
            blocked_domains: env_hash_set("BLOCKED_DOMAINS"),
            self_host: env::var("SELF_HOST")
                .ok()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),

            // CORS
            cors_enabled: env_bool("CORS_ENABLED", false),
            cors_allowed_origins: env_vec("CORS_ALLOWED_ORIGINS"),
            cors_allow_credentials: env_bool("CORS_ALLOW_CREDENTIALS", false),

            // Referer Validation
            referer_check_enabled: env_bool("REFERER_CHECK_ENABLED", false),
            allowed_referers: env_vec("ALLOWED_REFERERS"),

            // Rate Limiting
            rate_limit_per_minute: env_u32("RATE_LIMIT_PER_MINUTE", 100),
            rate_limit_by_key: env_bool("RATE_LIMIT_BY_KEY", false),

            // Trusted Proxies
            trusted_proxies: env_ip_networks("TRUSTED_PROXIES"),

            // HTTP Agent
            agent_connect_timeout: Duration::from_millis(env_u64("AGENT_CONNECT_TIMEOUT", 10000)),
            agent_headers_timeout: Duration::from_millis(env_u64("AGENT_HEADERS_TIMEOUT", 10000)),
            agent_body_timeout: Duration::from_millis(env_u64("AGENT_BODY_TIMEOUT", 30000)),
            agent_connections: env_u64("AGENT_CONNECTIONS", 128) as usize,
            agent_pipelining: env_u64("AGENT_PIPELINING", 10) as usize,
            agent_keepalive_timeout: Duration::from_millis(env_u64("AGENT_KEEPALIVE_TIMEOUT", 30000)),
            agent_keepalive_max_timeout: Duration::from_millis(env_u64("AGENT_KEEPALIVE_MAX_TIMEOUT", 60000)),
            agent_reject_unauthorized: env_bool("AGENT_REJECT_UNAUTHORIZED", true),

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
            max_output_size: env_u64("MAX_OUTPUT_SIZE_MB", 5) as usize * 1024 * 1024,

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

            // Logging
            enable_request_logging: env_bool("ENABLE_REQUEST_LOGGING", true),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env()
    }
}
