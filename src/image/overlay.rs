use bytes::Bytes;
use image::DynamicImage;
use std::sync::Arc;

use crate::cache::CacheManager;
use crate::config::Config;
use crate::http_client::{FetchError, HttpClient};
use crate::image::processor::{apply_rounded_corners, decode_image, resize_image, calculate_dimensions, get_metadata};
use crate::server::error::{AppError, AppResult};

/// Processed overlay ready for compositing
#[derive(Debug, Clone)]
pub struct ProcessedOverlay {
    pub image: DynamicImage,
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
}

/// Process a single overlay (fetch, resize, apply radius)
pub async fn process_overlay(
    config: &OverlayConfig,
    http_client: &HttpClient,
    cache_manager: &CacheManager,
    app_config: &Config,
) -> AppResult<ProcessedOverlay> {
    // Generate cache key
    let cache_key = format!(
        "{}_{}_{}_{}",
        config.url,
        config.max_width.unwrap_or(0),
        config.max_height.unwrap_or(0),
        config.radius.unwrap_or(0)
    );

    // Check overlay cache
    if let Some(cached_bytes) = cache_manager.get_overlay(&cache_key).await {
        let image = decode_image(&cached_bytes)?;
        return Ok(ProcessedOverlay {
            image,
            x: config.x,
            y: config.y,
        });
    }

    // Check source cache first
    let buffer = if let Some(cached) = cache_manager.get_source(&config.url).await {
        cached
    } else {
        // Fetch image
        let fetched = http_client
            .fetch_image(&config.url, true)
            .await
            .map_err(|e| match e {
                FetchError::NotFound => AppError::NotFound("Overlay image not found".to_string()),
                FetchError::Permanent(msg) => AppError::BadRequest(msg),
                FetchError::Transient(msg) => AppError::FetchFailed(msg),
            })?;

        // Cache source
        cache_manager.set_source(config.url.clone(), fetched.clone()).await;
        fetched
    };

    // Decode image
    let mut image = decode_image(&buffer)?;

    // Get original dimensions
    let (orig_width, orig_height) = get_metadata(&image)?;

    // Resize if needed
    if config.max_width.is_some() || config.max_height.is_some() {
        let (new_width, new_height) = calculate_dimensions(
            orig_width,
            orig_height,
            config.max_width,
            config.max_height,
        );
        image = resize_image(&image, new_width, new_height);
    }

    // Apply rounded corners if needed
    if let Some(radius) = config.radius {
        if radius > 0 {
            image = apply_rounded_corners(&image, radius)?;
        }
    }

    // Encode to PNG for caching (lossless, supports alpha)
    let mut png_bytes = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| AppError::ImageProcessing(format!("Failed to encode overlay: {}", e)))?;

    // Cache processed overlay
    cache_manager.set_overlay(cache_key, Bytes::from(png_bytes)).await;

    Ok(ProcessedOverlay {
        image,
        x: config.x,
        y: config.y,
    })
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

    join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
}
