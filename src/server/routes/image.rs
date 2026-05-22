use axum::{
    async_trait,
    extract::{FromRequestParts, State},
    http::{header, request::Parts, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use image::DynamicImage;
use once_cell::sync::Lazy;
use resvg::usvg::fontdb;
use serde::Deserialize;
use std::sync::Arc;

use crate::cache::{get_cache_key, CacheManager, ImageParams};

/// Global font database, pre-wrapped in an Arc. Loaded once at startup;
/// every SVG render clones the Arc (pointer copy) instead of deep-cloning
/// the fontdb::Database (which used to allocate per request).
static FONT_DB: Lazy<Arc<fontdb::Database>> = Lazy::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});
use crate::config::Config;
use crate::http_client::{FetchError, HttpClient};
use crate::image::{
    overlay::{process_overlays, OverlayConfig, ProcessedOverlay},
    processor::{
        apply_rounded_corners_inplace, calculate_dimensions, composite_overlay, decode_image,
        encode_webp, get_metadata, resize_crop_cover, resize_image,
    },
    text::{generate_text_svg, TextConfig},
};
use crate::server::error::{AppError, AppResult};

/// Custom query extractor that uses serde_qs to support bracket notation (e.g., `param[]`)
pub struct QsQuery<T>(pub T);

#[async_trait]
impl<S, T> FromRequestParts<S> for QsQuery<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        let config = serde_qs::Config::new(5, false);
        let value = config
            .deserialize_str(query)
            .map_err(|e| AppError::BadRequest(format!("Failed to deserialize query string: {}", e)))?;
        Ok(QsQuery(value))
    }
}

/// Query parameters for /image endpoint
#[derive(Debug, Deserialize)]
pub struct ImageQuery {
    pub src: String,
    #[serde(default)]
    pub maxw: Vec<u32>,
    #[serde(default)]
    pub maxh: Vec<u32>,
    #[serde(default)]
    pub rad: Vec<u32>,
    /// Vertical crop focus (0–100). Only meaningful when both `maxw` and
    /// `maxh` are set — selects which slice of the cover-fit survives.
    #[serde(default)]
    pub focy: Vec<u32>,

    #[serde(default)]
    pub overlay: Vec<String>,
    #[serde(default)]
    pub ox: Vec<i64>,
    #[serde(default)]
    pub oy: Vec<i64>,
    #[serde(default)]
    pub omaxw: Vec<u32>,
    #[serde(default)]
    pub omaxh: Vec<u32>,
    #[serde(default)]
    pub orad: Vec<u32>,
    #[serde(default)]
    pub odeco: Vec<u32>,

    #[serde(default)]
    pub text: Vec<String>,
    #[serde(default)]
    pub tx: Vec<i64>,
    #[serde(default)]
    pub ty: Vec<i64>,
    #[serde(default)]
    pub ts: Vec<u32>,
    #[serde(default)]
    pub tc: Vec<String>,
    #[serde(default)]
    pub tf: Vec<String>,
    #[serde(default)]
    pub tmaxw: Vec<u32>,
    #[serde(default)]
    pub tmaxh: Vec<u32>,
    #[serde(default)]
    pub ta: Vec<String>,
}

impl ImageQuery {
    /// Get maxw as Option (first element of the vec)
    pub fn maxw(&self) -> Option<u32> {
        self.maxw.first().copied()
    }

    /// Get maxh as Option (first element of the vec)
    pub fn maxh(&self) -> Option<u32> {
        self.maxh.first().copied()
    }

    /// Get rad as Option (first element of the vec)
    pub fn rad(&self) -> Option<u32> {
        self.rad.first().copied()
    }

    /// Get focy as Option (first element of the vec)
    pub fn focy(&self) -> Option<u32> {
        self.focy.first().copied()
    }

    /// Validate query parameters against limits
    pub fn validate(&self, config: &Config) -> AppResult<()> {
        // Validate src URL length
        if self.src.is_empty() || self.src.len() > config.max_url_length {
            return Err(AppError::BadRequest("Invalid source URL".to_string()));
        }

        // Validate dimensions. `maxw` (background) uses its own tighter cap
        // — composed images we ship to Discord never need to be huge.
        if let Some(w) = self.maxw() {
            if w < config.min_dimension || w > config.max_bg_width {
                return Err(AppError::BadRequest(format!(
                    "maxw out of range (1..={})",
                    config.max_bg_width
                )));
            }
        }
        if let Some(h) = self.maxh() {
            if h < config.min_dimension || h > config.max_dimension {
                return Err(AppError::BadRequest("Invalid maxh parameter".to_string()));
            }
        }

        // Overlay sizes: cap each entry tighter than `max_dimension`. Avatars
        // are the common case and never need to be larger than the background.
        for &w in &self.omaxw {
            if w < config.min_dimension || w > config.max_overlay_size {
                return Err(AppError::BadRequest(format!(
                    "omaxw out of range (1..={})",
                    config.max_overlay_size
                )));
            }
        }
        for &h in &self.omaxh {
            if h < config.min_dimension || h > config.max_overlay_size {
                return Err(AppError::BadRequest(format!(
                    "omaxh out of range (1..={})",
                    config.max_overlay_size
                )));
            }
        }

        // Validate radius
        if let Some(r) = self.rad() {
            if r > config.max_radius {
                return Err(AppError::BadRequest("Invalid rad parameter".to_string()));
            }
        }

        // Validate vertical crop focus — a 0..=100 percentage.
        if let Some(f) = self.focy() {
            if f > 100 {
                return Err(AppError::BadRequest(
                    "focy out of range (0..=100)".to_string(),
                ));
            }
        }

        // Validate overlay count
        if self.overlay.len() > config.max_overlays {
            return Err(AppError::BadRequest("Too many overlays".to_string()));
        }

        // Validate text count
        if self.text.len() > config.max_texts {
            return Err(AppError::BadRequest("Too many text overlays".to_string()));
        }

        Ok(())
    }

    /// Convert to ImageParams for cache key generation
    pub fn to_params(&self) -> ImageParams {
        ImageParams {
            src: self.src.clone(),
            maxw: self.maxw(),
            maxh: self.maxh(),
            focy: self.focy(),
            rad: self.rad(),
            overlay: self.overlay.clone(),
            ox: self.ox.clone(),
            oy: self.oy.clone(),
            omaxw: self.omaxw.clone(),
            omaxh: self.omaxh.clone(),
            orad: self.orad.clone(),
            odeco: self.odeco.clone(),
            text: self.text.clone(),
            tx: self.tx.clone(),
            ty: self.ty.clone(),
            ts: self.ts.clone(),
            tc: self.tc.clone(),
            tf: self.tf.clone(),
            tmaxw: self.tmaxw.clone(),
            tmaxh: self.tmaxh.clone(),
            ta: self.ta.clone(),
        }
    }
}

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub http_client: Arc<HttpClient>,
    pub cache_manager: Arc<CacheManager>,
}

/// Main image processing endpoint
pub async fn handle_image(
    State(state): State<AppState>,
    QsQuery(query): QsQuery<ImageQuery>,
) -> Result<impl IntoResponse, AppError> {
    let config = &state.config;

    // Validate parameters
    query.validate(config)?;

    // Generate cache key
    let params = query.to_params();
    let cache_key = get_cache_key(&params);

    // Fast path: check output cache BEFORE URL validation
    if let Some(cached) = state.cache_manager.get_output(&cache_key).await {
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (header::HeaderName::from_static("x-cache"), "HIT"),
            ],
            cached,
        ));
    }

    // Check source cache
    let source_buffer = if let Some(cached) = state.cache_manager.get_source(&query.src).await {
        cached
    } else {
        // Fetch source image (includes URL validation and SSRF protection)
        let fetched = state
            .http_client
            .fetch_image(&query.src, false)
            .await
            .map_err(|e| match e {
                FetchError::NotFound => {
                    AppError::NotFound("Source image not found or inaccessible".to_string())
                }
                FetchError::Permanent(msg) => AppError::BadRequest(msg),
                FetchError::Transient(msg) => AppError::FetchFailed(msg),
            })?;

        // Cache source
        state
            .cache_manager
            .set_source(query.src.clone(), fetched.clone())
            .await;
        fetched
    };

    // Process overlays in parallel (async I/O — fetches stay on the tokio runtime)
    let overlay_configs: Vec<OverlayConfig> = query
        .overlay
        .iter()
        .take(config.max_overlays)
        .enumerate()
        .map(|(i, url)| OverlayConfig {
            url: url.clone(),
            x: *query.ox.get(i).unwrap_or(&0),
            y: *query.oy.get(i).unwrap_or(&0),
            max_width: query.omaxw.get(i).copied(),
            max_height: query.omaxh.get(i).copied(),
            radius: query.orad.get(i).copied(),
            decoration: query.odeco.get(i).copied().unwrap_or(0) != 0,
        })
        .collect();

    let processed_overlays = if !overlay_configs.is_empty() {
        process_overlays(
            overlay_configs,
            state.http_client.clone(),
            state.cache_manager.clone(),
            state.config.clone(),
        )
        .await
    } else {
        Vec::new()
    };

    // Build text configs (cheap, no I/O).
    let text_configs: Vec<TextConfig> = query
        .text
        .iter()
        .take(config.max_texts)
        .enumerate()
        .map(|(i, text)| TextConfig {
            text: text.clone(),
            x: *query.tx.get(i).unwrap_or(&0),
            y: *query.ty.get(i).unwrap_or(&0),
            font_size: *query.ts.get(i).unwrap_or(&24),
            color: query.tc.get(i).cloned().unwrap_or_else(|| "#000000".to_string()),
            font_family: query.tf.get(i).cloned().unwrap_or_else(|| "Arial".to_string()),
            max_width: query.tmaxw.get(i).copied(),
            max_height: query.tmaxh.get(i).copied(),
            align: query.ta.get(i).cloned().unwrap_or_else(|| "left".to_string()),
        })
        .collect();

    // Hoist values needed inside the blocking section so we don't move
    // `query`/`config` wholesale (config is an Arc, but it's cleaner to grab
    // the few primitives the CPU path actually needs).
    let maxw = query.maxw();
    let maxh = query.maxh();
    // When both dimensions are given, crop to an exact cover fit; `focy`
    // (0–100, default centred) selects the vertical slice.
    let crop_cover = maxw.is_some() && maxh.is_some();
    let focus_y = query.focy().map(|p| p as f32 / 100.0).unwrap_or(0.5);
    let radius = query.rad().filter(|&r| r > 0);
    let max_text_length = config.max_text_length;
    let webp_quality = config.webp_quality;

    // Move all CPU-bound work (decode + resize + composite + text raster +
    // encode) off the tokio worker so other in-flight requests aren't
    // starved while one is encoding. tokio's blocking pool handles backpressure.
    let output = tokio::task::spawn_blocking(move || -> AppResult<Bytes> {
        let base_image = decode_image(&source_buffer)?;
        let (orig_width, orig_height) = get_metadata(&base_image)?;
        // Cover-crop targets the exact box; otherwise preserve aspect ratio.
        let (final_width, final_height) = match (crop_cover, maxw, maxh) {
            (true, Some(w), Some(h)) => (w, h),
            _ => calculate_dimensions(orig_width, orig_height, maxw, maxh),
        };

        let text_svg = if !text_configs.is_empty() {
            Some(generate_text_svg(
                &text_configs,
                final_width,
                final_height,
                max_text_length,
            ))
        } else {
            None
        };

        let mut result_image = base_image;
        if crop_cover {
            result_image =
                resize_crop_cover(&result_image, final_width, final_height, focus_y);
        } else if maxw.is_some() || maxh.is_some() {
            result_image = resize_image(&result_image, final_width, final_height);
        }

        // Fold rounded-corners + overlay composite + text composite into a
        // single RGBA conversion. Previously each step allocated its own buffer.
        let need_composite = !processed_overlays.is_empty() || text_svg.is_some();
        if radius.is_some() || need_composite {
            let mut base_rgba = result_image.to_rgba8();

            if let Some(r) = radius {
                apply_rounded_corners_inplace(&mut base_rgba, r);
            }

            for overlay in &processed_overlays {
                composite_overlay(&mut base_rgba, &overlay.image, overlay.x, overlay.y);
            }

            if let Some(svg_bytes) = text_svg {
                if let Ok(text_image) =
                    render_svg_to_image(&svg_bytes, final_width, final_height)
                {
                    composite_overlay(&mut base_rgba, &text_image, 0, 0);
                }
            }

            result_image = DynamicImage::ImageRgba8(base_rgba);
        }

        encode_webp(&result_image, webp_quality)
    })
    .await
    .map_err(|e| AppError::Internal(format!("CPU task failed: {}", e)))??;

    // Cache output
    state.cache_manager.set_output(cache_key, output.clone()).await;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::HeaderName::from_static("x-cache"), "MISS"),
        ],
        output,
    ))
}

/// Render SVG to image using resvg (optimized with cached font database)
fn render_svg_to_image(svg_data: &[u8], width: u32, height: u32) -> AppResult<DynamicImage> {
    use resvg::tiny_skia;
    use resvg::usvg;

    // Cheap Arc pointer-clone — fontdb stays shared across all requests.
    let options = usvg::Options {
        fontdb: FONT_DB.clone(),
        ..Default::default()
    };

    let tree = usvg::Tree::from_data(svg_data, &options)
        .map_err(|e| AppError::ImageProcessing(format!("Failed to parse SVG: {}", e)))?;

    let size = tree.size();
    let scale_x = width as f32 / size.width();
    let scale_y = height as f32 / size.height();

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| AppError::ImageProcessing("Failed to create pixmap".to_string()))?;

    let transform = tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Convert to image::RgbaImage - use the data directly without extra allocation
    let rgba = image::RgbaImage::from_raw(width, height, pixmap.take())
        .ok_or_else(|| AppError::ImageProcessing("Failed to create RGBA image".to_string()))?;

    Ok(DynamicImage::ImageRgba8(rgba))
}
