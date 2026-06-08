use ahash::AHasher;
use bytes::Bytes;
use image::DynamicImage;
use moka::future::Cache;
use once_cell::sync::OnceCell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::config::Config;

static CACHE_MANAGER: OnceCell<Arc<CacheManager>> = OnceCell::new();

/// Manages all 4 caching tiers
pub struct CacheManager {
    /// Output cache - final WebP images (size-based, TTL, stale-while-revalidate)
    pub output: Cache<String, Bytes>,

    /// Source cache - downloaded source images (permanent LRU, no TTL)
    pub source: Cache<String, Bytes>,

    /// Overlay cache - processed (resized + rounded) overlays kept decoded
    /// so cache hits skip the PNG encode/decode round-trip that used to
    /// happen on every reuse. `Arc<DynamicImage>` is cheap to clone.
    pub overlay: Cache<String, Arc<DynamicImage>>,

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
    pub async fn get_overlay(&self, key: &str) -> Option<Arc<DynamicImage>> {
        self.overlay.get(key).await
    }

    /// Set overlay in cache
    pub async fn set_overlay(&self, key: String, value: Arc<DynamicImage>) {
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

/// Types that can feed their cache-distinguishing fields into a hasher.
///
/// This lets the output-cache key be computed straight from the live request
/// struct on the hot path — no intermediate `ImageParams` allocation (which
/// used to clone the `src` String plus ~20 `Vec`s per request) — while the
/// unit tests still exercise the exact same hashing through `ImageParams`.
pub trait CacheKeyInput {
    fn hash_into<H: Hasher>(&self, hasher: &mut H);
}

/// Generate cache key for output cache using fast hashing.
/// Uses ahash for O(1) allocation-free key generation.
pub fn get_cache_key<T: CacheKeyInput>(input: &T) -> String {
    let mut hasher = AHasher::default();
    input.hash_into(&mut hasher);
    // Convert hash to hex string (16 chars)
    format!("{:016x}", hasher.finish())
}

impl CacheKeyInput for ImageParams {
    fn hash_into<H: Hasher>(&self, hasher: &mut H) {
        // Hash all parameters directly without string allocation
        self.src.hash(hasher);
        self.maxw.hash(hasher);
        self.maxh.hash(hasher);
        self.focy.hash(hasher);
        self.blur.hash(hasher);
        self.bri.hash(hasher);
        self.rad.hash(hasher);

        // Hash overlay parameters
        self.overlay.hash(hasher);
        self.ox.hash(hasher);
        self.oy.hash(hasher);
        self.omaxw.hash(hasher);
        self.omaxh.hash(hasher);
        self.orad.hash(hasher);
        self.odeco.hash(hasher);
        self.oshy.hash(hasher);
        self.oshb.hash(hasher);
        self.osha.hash(hasher);

        // Hash text parameters
        self.text.hash(hasher);
        self.tx.hash(hasher);
        self.ty.hash(hasher);
        self.ts.hash(hasher);
        self.tc.hash(hasher);
        self.tf.hash(hasher);
        self.tmaxw.hash(hasher);
        self.tmaxh.hash(hasher);
        self.ta.hash(hasher);
        self.tw.hash(hasher);
        self.to.hash(hasher);
        self.tow.hash(hasher);
    }
}

/// Image request parameters
#[derive(Debug, Default, Clone)]
pub struct ImageParams {
    pub src: String,
    pub maxw: Option<u32>,
    pub maxh: Option<u32>,
    /// Vertical crop focus (0–100); paired with the `maxw`+`maxh` cover-crop.
    pub focy: Option<u32>,
    /// Background Gaussian blur radius in pixels; `None`/0 = no blur.
    pub blur: Option<u32>,
    /// Background brightness percentage; `None` = 100 (unchanged).
    pub bri: Option<u32>,
    pub rad: Option<u32>,
    pub overlay: Vec<String>,
    pub ox: Vec<i64>,
    pub oy: Vec<i64>,
    pub omaxw: Vec<u32>,
    pub omaxh: Vec<u32>,
    pub orad: Vec<u32>,
    pub odeco: Vec<u32>,
    /// Per-overlay shadow Y offset in pixels. Only honored when `odeco[i]` is set.
    pub oshy: Vec<u32>,
    /// Per-overlay shadow blur radius (CSS-style px). Only honored when `odeco[i]` is set.
    pub oshb: Vec<u32>,
    /// Per-overlay shadow alpha (0..=100). Only honored when `odeco[i]` is set.
    pub osha: Vec<u32>,
    pub text: Vec<String>,
    pub tx: Vec<i64>,
    pub ty: Vec<i64>,
    pub ts: Vec<u32>,
    pub tc: Vec<String>,
    pub tf: Vec<String>,
    pub tmaxw: Vec<u32>,
    pub tmaxh: Vec<u32>,
    pub ta: Vec<String>,
    /// Per-text font weight tokens (`bold`, `normal`, `600`, …).
    pub tw: Vec<String>,
    /// Per-text outline color(s); empty/absent disables the outline.
    pub to: Vec<String>,
    /// Per-text outline width(s) in px.
    pub tow: Vec<u32>,
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

        // Key should be a 16-character hex string (64-bit hash)
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));

        // Same params should produce same key (deterministic)
        let key2 = get_cache_key(&params);
        assert_eq!(key, key2);

        // Different params should produce different key
        let different_params = ImageParams {
            src: "https://example.com/other.jpg".to_string(),
            maxw: Some(800),
            maxh: Some(600),
            rad: Some(10),
            ..Default::default()
        };
        let different_key = get_cache_key(&different_params);
        assert_ne!(key, different_key);
    }
}
