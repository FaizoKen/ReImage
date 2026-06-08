use image::{DynamicImage, GenericImageView};
use std::sync::Arc;

use crate::cache::CacheManager;
use crate::config::Config;
use crate::http_client::{FetchError, HttpClient};
use crate::image::processor::{
    apply_overlay_decorations, apply_rounded_corners, calculate_dimensions, decode_image,
    get_metadata, resize_image, DecorationParams,
};
use crate::server::error::{AppError, AppResult};

/// Processed overlay ready for compositing. The image is `Arc`-wrapped so
/// repeated cache hits can reuse the decoded pixels with no copy.
#[derive(Debug, Clone)]
pub struct ProcessedOverlay {
    pub image: Arc<DynamicImage>,
    pub x: i64,
    pub y: i64,
}

/// Configuration for an overlay
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    pub url: String,
    pub x: i64,
    pub y: i64,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub radius: Option<u32>,
    /// Master enable for the drop shadow around the overlay. When false, the
    /// `shadow_*` overrides are ignored and the overlay renders bare.
    pub decoration: bool,
    /// Shadow Y offset in pixels. `None` → auto-scale from overlay size
    /// (matches the legacy behavior for URLs that pass only `odeco=1`).
    pub shadow_offset_y: Option<u32>,
    /// Shadow blur radius in CSS-style pixels (converted to sigma = blur/2 inside
    /// the renderer). `None` → auto-scale from overlay size.
    pub shadow_blur: Option<u32>,
    /// Shadow alpha as a 0..=100 percentage. `None` → 50% (matches the modal
    /// preview default of `rgba(0,0,0,0.5)`).
    pub shadow_alpha_pct: Option<u32>,
}

/// Process a single overlay (fetch, resize, apply radius)
pub async fn process_overlay(
    config: &OverlayConfig,
    http_client: &HttpClient,
    cache_manager: &CacheManager,
    _app_config: &Config,
) -> AppResult<ProcessedOverlay> {
    // Generate cache key for the un-decorated overlay (decoration is applied after
    // cache hit so we don't need to know the pad offset to use a cached entry).
    let cache_key = format!(
        "{}_{}_{}_{}",
        config.url,
        config.max_width.unwrap_or(0),
        config.max_height.unwrap_or(0),
        config.radius.unwrap_or(0)
    );

    // Resolve the decoded+resized+rounded overlay: either straight from the
    // overlay cache, or by fetching the source and transforming it. All the
    // CPU-bound transform work runs on the blocking pool so it never stalls a
    // tokio worker thread (matching how the main image path is handled).
    let base_image = if let Some(cached_image) = cache_manager.get_overlay(&cache_key).await {
        cached_image
    } else {
        // Check source cache first
        let buffer = if let Some(cached) = cache_manager.get_source(&config.url).await {
            cached
        } else {
            // Fetch image
            let fetched =
                http_client
                    .fetch_image(&config.url, true)
                    .await
                    .map_err(|e| match e {
                        FetchError::NotFound => {
                            AppError::NotFound("Overlay image not found".to_string())
                        }
                        FetchError::Permanent(msg) => AppError::BadRequest(msg),
                        FetchError::Transient(msg) => AppError::FetchFailed(msg),
                    })?;

            // Cache source
            cache_manager
                .set_source(config.url.clone(), fetched.clone())
                .await;
            fetched
        };

        // Decode + resize + round on the blocking pool — these are CPU-bound
        // (and rayon-parallel internally), so running them on a tokio worker
        // would block that worker for the whole transform.
        let max_width = config.max_width;
        let max_height = config.max_height;
        let radius = config.radius;
        let image = tokio::task::spawn_blocking(move || -> AppResult<DynamicImage> {
            let mut image = decode_image(&buffer)?;
            let (orig_width, orig_height) = get_metadata(&image)?;

            if max_width.is_some() || max_height.is_some() {
                let (new_width, new_height) =
                    calculate_dimensions(orig_width, orig_height, max_width, max_height);
                image = resize_image(&image, new_width, new_height);
            }

            if let Some(radius) = radius {
                if radius > 0 {
                    image = apply_rounded_corners(&image, radius)?;
                }
            }

            Ok(image)
        })
        .await
        .map_err(|e| AppError::Internal(format!("overlay CPU task failed: {}", e)))??;

        // Cache the decoded image directly — Arc-wrapped so hits clone the
        // pointer, not the pixels.
        let image_arc = Arc::new(image);
        cache_manager
            .set_overlay(cache_key, image_arc.clone())
            .await;
        image_arc
    };

    // Bare overlays need no further CPU work — return immediately.
    if !config.decoration {
        return Ok(ProcessedOverlay {
            image: base_image,
            x: config.x,
            y: config.y,
        });
    }

    // Decoration (drop shadow) is CPU-bound too — blur + composite — so it also
    // goes to the blocking pool. This runs on cache hits as well, since the
    // shadow params are per-request and not baked into the overlay cache.
    let cfg = config.clone();
    tokio::task::spawn_blocking(move || finalize_overlay(base_image, &cfg))
        .await
        .map_err(|e| AppError::Internal(format!("overlay decoration task failed: {}", e)))
}

/// Apply ring + drop shadow decoration (if requested) and adjust the overlay's
/// render position so the visible image stays at the caller-specified (x, y).
fn finalize_overlay(image: Arc<DynamicImage>, config: &OverlayConfig) -> ProcessedOverlay {
    if !config.decoration {
        return ProcessedOverlay {
            image,
            x: config.x,
            y: config.y,
        };
    }
    let (w, h) = image.dimensions();
    let params = DecorationParams::resolve(
        w,
        h,
        config.shadow_offset_y,
        config.shadow_blur,
        config.shadow_alpha_pct,
    );
    match apply_overlay_decorations(&image, &params) {
        Ok((decorated, pad_x, pad_y)) => ProcessedOverlay {
            image: Arc::new(decorated),
            x: config.x - pad_x as i64,
            y: config.y - pad_y as i64,
        },
        Err(e) => {
            tracing::warn!(
                "Overlay decoration failed, falling back to plain overlay: {:?}",
                e
            );
            ProcessedOverlay {
                image,
                x: config.x,
                y: config.y,
            }
        }
    }
}

/// Process multiple overlays in parallel
pub async fn process_overlays(
    configs: Vec<OverlayConfig>,
    http_client: Arc<HttpClient>,
    cache_manager: Arc<CacheManager>,
    app_config: Arc<Config>,
) -> Vec<ProcessedOverlay> {
    use futures::future::join_all;

    let futures: Vec<_> = configs
        .into_iter()
        .map(|config| {
            let http = http_client.clone();
            let cache = cache_manager.clone();
            let app_cfg = app_config.clone();
            async move {
                match process_overlay(&config, &http, &cache, &app_cfg).await {
                    Ok(overlay) => Some(overlay),
                    Err(e) => {
                        tracing::warn!("Overlay processing failed: {:?}", e);
                        None
                    }
                }
            }
        })
        .collect();

    join_all(futures).await.into_iter().flatten().collect()
}
