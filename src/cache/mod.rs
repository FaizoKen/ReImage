use bytes::Bytes;
use moka::future::Cache;
use once_cell::sync::OnceCell;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;

static CACHE_MANAGER: OnceCell<Arc<CacheManager>> = OnceCell::new();

/// Manages all 4 caching tiers
pub struct CacheManager {
    /// Output cache - final WebP images (size-based, TTL, stale-while-revalidate)
    pub output: Cache<String, Bytes>,

    /// Source cache - downloaded source images (permanent LRU, no TTL)
    pub source: Cache<String, Bytes>,

    /// Overlay cache - processed overlays (count-based, TTL)
    pub overlay: Cache<String, Bytes>,

    /// Mask cache - SVG masks for rounded corners (count-based, TTL)
    pub mask: Cache<String, Bytes>,
}

impl CacheManager {
    pub fn new(config: &Config) -> Self {
        // Output cache: size-based with TTL
        let output = Cache::builder()
            .max_capacity(config.output_cache_size_mb * 1024 * 1024)
            .time_to_live(config.output_cache_ttl)
            .weigher(|_key: &String, value: &Bytes| -> u32 {
                value.len().try_into().unwrap_or(u32::MAX)
            })
            .build();

        // Source cache: permanent LRU (no TTL)
        let source = Cache::builder()
            .max_capacity(config.source_cache_size_mb * 1024 * 1024)
            .weigher(|_key: &String, value: &Bytes| -> u32 {
                value.len().try_into().unwrap_or(u32::MAX)
            })
            .build();

        // Overlay cache: count-based with TTL
        let overlay = Cache::builder()
            .max_capacity(config.overlay_cache_max)
            .time_to_live(config.overlay_cache_ttl)
            .build();

        // Mask cache: count-based with TTL
        let mask = Cache::builder()
            .max_capacity(config.mask_cache_max)
            .time_to_live(config.mask_cache_ttl)
            .build();

        Self {
            output,
            source,
            overlay,
            mask,
        }
    }

    /// Get output from cache
    pub async fn get_output(&self, key: &str) -> Option<Bytes> {
        self.output.get(key).await
    }

    /// Set output in cache
    pub async fn set_output(&self, key: String, value: Bytes) {
        self.output.insert(key, value).await;
    }

    /// Get source image from cache
    pub async fn get_source(&self, url: &str) -> Option<Bytes> {
        self.source.get(url).await
    }

    /// Set source image in cache
    pub async fn set_source(&self, url: String, value: Bytes) {
        self.source.insert(url, value).await;
    }

    /// Get overlay from cache
    pub async fn get_overlay(&self, key: &str) -> Option<Bytes> {
        self.overlay.get(key).await
    }

    /// Set overlay in cache
    pub async fn set_overlay(&self, key: String, value: Bytes) {
        self.overlay.insert(key, value).await;
    }

    /// Get mask from cache
    pub async fn get_mask(&self, key: &str) -> Option<Bytes> {
        self.mask.get(key).await
    }

    /// Set mask in cache
    pub async fn set_mask(&self, key: String, value: Bytes) {
        self.mask.insert(key, value).await;
    }

    /// Clear all caches (for graceful shutdown)
    pub fn clear(&self) {
        self.output.invalidate_all();
        self.source.invalidate_all();
        self.overlay.invalidate_all();
        self.mask.invalidate_all();
    }

    /// Get or initialize the global cache manager
    pub fn global(config: &Config) -> Arc<CacheManager> {
        CACHE_MANAGER
            .get_or_init(|| Arc::new(CacheManager::new(config)))
            .clone()
    }
}

/// Generate cache key for output cache
pub fn get_cache_key(params: &ImageParams) -> String {
    let serialize = |arr: &[String]| arr.join(",");
    let serialize_i64 = |arr: &[i64]| {
        arr.iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let serialize_u32 = |arr: &[u32]| {
        arr.iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };

    [
        params.src.as_str(),
        &params.maxw.map(|v| v.to_string()).unwrap_or_default(),
        &params.maxh.map(|v| v.to_string()).unwrap_or_default(),
        &params.rad.unwrap_or(0).to_string(),
        &serialize(&params.overlay),
        &serialize_i64(&params.ox),
        &serialize_i64(&params.oy),
        &serialize_u32(&params.omaxw),
        &serialize_u32(&params.omaxh),
        &serialize_u32(&params.orad),
        &serialize(&params.text),
        &serialize_i64(&params.tx),
        &serialize_i64(&params.ty),
        &serialize_u32(&params.ts),
        &serialize(&params.tc),
        &serialize(&params.tf),
        &serialize_u32(&params.tmaxw),
        &serialize_u32(&params.tmaxh),
        &serialize(&params.ta),
    ]
    .join("|")
}

/// Image request parameters
#[derive(Debug, Default, Clone)]
pub struct ImageParams {
    pub src: String,
    pub maxw: Option<u32>,
    pub maxh: Option<u32>,
    pub rad: Option<u32>,
    pub overlay: Vec<String>,
    pub ox: Vec<i64>,
    pub oy: Vec<i64>,
    pub omaxw: Vec<u32>,
    pub omaxh: Vec<u32>,
    pub orad: Vec<u32>,
    pub text: Vec<String>,
    pub tx: Vec<i64>,
    pub ty: Vec<i64>,
    pub ts: Vec<u32>,
    pub tc: Vec<String>,
    pub tf: Vec<String>,
    pub tmaxw: Vec<u32>,
    pub tmaxh: Vec<u32>,
    pub ta: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_generation() {
        let params = ImageParams {
            src: "https://example.com/image.jpg".to_string(),
            maxw: Some(800),
            maxh: Some(600),
            rad: Some(10),
            ..Default::default()
        };

        let key = get_cache_key(&params);
        assert!(key.contains("https://example.com/image.jpg"));
        assert!(key.contains("800"));
        assert!(key.contains("600"));
        assert!(key.contains("10"));
    }
}
