use axum::{
    async_trait,
    extract::{FromRequestParts, State},
    http::{header, request::Parts, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use image::DynamicImage;
use serde::Deserialize;
use std::sync::Arc;

use crate::cache::{get_cache_key, CacheManager, ImageParams};
use crate::config::Config;
use crate::http_client::{FetchError, HttpClient};
use crate::image::{
    overlay::{process_overlays, OverlayConfig, ProcessedOverlay},
    processor::{
        apply_rounded_corners, calculate_dimensions, composite_overlay, decode_image,
        encode_webp, get_metadata, resize_image,
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

    /// Validate query parameters against limits
    pub fn validate(&self, config: &Config) -> AppResult<()> {
        // Validate src URL length
        if self.src.is_empty() || self.src.len() > config.max_url_length {
            return Err(AppError::BadRequest("Invalid source URL".to_string()));
        }

        // Validate dimensions
        if let Some(w) = self.maxw() {
            if w < config.min_dimension || w > config.max_dimension {
                return Err(AppError::BadRequest("Invalid maxw parameter".to_string()));
            }
        }
        if let Some(h) = self.maxh() {
            if h < config.min_dimension || h > config.max_dimension {
                return Err(AppError::BadRequest("Invalid maxh parameter".to_string()));
            }
        }

        // Validate radius
        if let Some(r) = self.rad() {
            if r > config.max_radius {
                return Err(AppError::BadRequest("Invalid rad parameter".to_string()));
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
            rad: self.rad(),
            overlay: self.overlay.clone(),
            ox: self.ox.clone(),
            oy: self.oy.clone(),
            omaxw: self.omaxw.clone(),
            omaxh: self.omaxh.clone(),
            orad: self.orad.clone(),
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

    // Decode image
    let base_image = decode_image(&source_buffer)?;

    // Get metadata and validate
    let (orig_width, orig_height) = get_metadata(&base_image)?;

    // Calculate final dimensions
    let (final_width, final_height) =
        calculate_dimensions(orig_width, orig_height, query.maxw(), query.maxh());

    // Process overlays in parallel
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

    // Generate text SVG if needed
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

    let text_svg = if !text_configs.is_empty() {
        Some(generate_text_svg(&text_configs, final_width, final_height, config.max_text_length))
    } else {
        None
    };

    // Process the image
    let mut result_image = base_image;

    // Resize if needed
    if query.maxw().is_some() || query.maxh().is_some() {
        result_image = resize_image(&result_image, final_width, final_height);
    }

    // Apply rounded corners to base image if needed
    if let Some(radius) = query.rad() {
        if radius > 0 {
            result_image = apply_rounded_corners(&result_image, radius)?;
        }
    }

    // Composite overlays
    if !processed_overlays.is_empty() || text_svg.is_some() {
        let mut base_rgba = result_image.to_rgba8();

        // Composite image overlays
        for overlay in processed_overlays {
            composite_overlay(&mut base_rgba, &overlay.image, overlay.x, overlay.y);
        }

        // Composite text SVG
        if let Some(svg_bytes) = text_svg {
            if let Ok(text_image) = render_svg_to_image(&svg_bytes, final_width, final_height) {
                composite_overlay(&mut base_rgba, &text_image, 0, 0);
            }
        }

        result_image = DynamicImage::ImageRgba8(base_rgba);
    }

    // Encode to WebP
    let output = encode_webp(&result_image, config.webp_quality)?;

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

/// Render SVG to image using resvg
fn render_svg_to_image(svg_data: &[u8], width: u32, height: u32) -> AppResult<DynamicImage> {
    use resvg::tiny_skia;
    use resvg::usvg;

    // Load system fonts for text rendering
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();

    let tree = usvg::Tree::from_data(svg_data, &options)
        .map_err(|e| AppError::ImageProcessing(format!("Failed to parse SVG: {}", e)))?;

    let size = tree.size();
    let scale_x = width as f32 / size.width();
    let scale_y = height as f32 / size.height();

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| AppError::ImageProcessing("Failed to create pixmap".to_string()))?;

    let transform = tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Convert to image::RgbaImage
    let data = pixmap.data().to_vec();
    let rgba = image::RgbaImage::from_raw(width, height, data)
        .ok_or_else(|| AppError::ImageProcessing("Failed to create RGBA image".to_string()))?;

    Ok(DynamicImage::ImageRgba8(rgba))
}
