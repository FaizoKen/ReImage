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
    fonts::FONTS,
    overlay::{process_overlays, OverlayConfig, ProcessedOverlay},
    processor::{
        apply_blur, apply_brightness, apply_rounded_corners_inplace, calculate_dimensions,
        composite_overlay, decode_image, encode_webp, get_metadata, resize_crop_cover,
        resize_image,
    },
    text::{generate_text_svg, TextConfig},
};
use crate::security::sanitize::sanitize_weight;
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
    /// Gaussian blur applied to the background after resize, in pixels of the
    /// resolved background. 0 or absent = no blur.
    #[serde(default)]
    pub blur: Vec<u32>,
    /// Background brightness as a percentage (100 = unchanged, <100 darkens).
    /// Absent = 100.
    #[serde(default)]
    pub bri: Vec<u32>,

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
    /// Overlay shadow Y offset in pixels (drop distance below the overlay).
    /// Only honored when `odeco[i]` is non-zero. Absent = auto from overlay size.
    #[serde(default)]
    pub oshy: Vec<u32>,
    /// Overlay shadow blur radius in CSS-style pixels. Internally converted to
    /// Gaussian sigma (`blur/2`). Only honored when `odeco[i]` is non-zero.
    #[serde(default)]
    pub oshb: Vec<u32>,
    /// Overlay shadow alpha as a 0..=100 percentage. Only honored when
    /// `odeco[i]` is non-zero. Absent = 50 (matches the modal preview default).
    #[serde(default)]
    pub osha: Vec<u32>,

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
    /// Text font weight(s): `normal`, `bold`, `light`, `semibold`, `black`, or a
    /// numeric 100–900. Absent = `bold`.
    #[serde(default)]
    pub tw: Vec<String>,
    /// Text outline/halo color(s) (hex or named). Absent = no outline.
    #[serde(default)]
    pub to: Vec<String>,
    /// Text outline width(s) in px. Only honored when the matching `to[i]` is
    /// set. Absent = auto-scaled from the font size.
    #[serde(default)]
    pub tow: Vec<u32>,
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

    /// Get background blur as Option (first element of the vec)
    pub fn blur(&self) -> Option<u32> {
        self.blur.first().copied()
    }

    /// Get background brightness as Option (first element of the vec)
    pub fn bri(&self) -> Option<u32> {
        self.bri.first().copied()
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

        // Validate background blur radius.
        if let Some(b) = self.blur() {
            if b > config.max_blur {
                return Err(AppError::BadRequest(format!(
                    "blur out of range (0..={})",
                    config.max_blur
                )));
            }
        }

        // Validate background brightness percentage.
        if let Some(b) = self.bri() {
            if b > config.max_brightness {
                return Err(AppError::BadRequest(format!(
                    "bri out of range (0..={})",
                    config.max_brightness
                )));
            }
        }

        // Validate overlay shadow params. Loose caps — these are visual knobs,
        // not resource-bound, so the limit just prevents absurd inputs from
        // ballooning the blur kernel cost.
        for &v in &self.oshy {
            if v > 200 {
                return Err(AppError::BadRequest(
                    "oshy out of range (0..=200)".to_string(),
                ));
            }
        }
        for &v in &self.oshb {
            if v > 200 {
                return Err(AppError::BadRequest(
                    "oshb out of range (0..=200)".to_string(),
                ));
            }
        }
        for &v in &self.osha {
            if v > 100 {
                return Err(AppError::BadRequest(
                    "osha out of range (0..=100)".to_string(),
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

        // Validate text outline width. Loose cap — a visual knob, but an absurd
        // stroke width would blow up the glyph outline rasterization cost.
        for &v in &self.tow {
            if v > 100 {
                return Err(AppError::BadRequest(
                    "tow out of range (0..=100)".to_string(),
                ));
            }
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
            blur: self.blur(),
            bri: self.bri(),
            rad: self.rad(),
            overlay: self.overlay.clone(),
            ox: self.ox.clone(),
            oy: self.oy.clone(),
            omaxw: self.omaxw.clone(),
            omaxh: self.omaxh.clone(),
            orad: self.orad.clone(),
            odeco: self.odeco.clone(),
            oshy: self.oshy.clone(),
            oshb: self.oshb.clone(),
            osha: self.osha.clone(),
            text: self.text.clone(),
            tx: self.tx.clone(),
            ty: self.ty.clone(),
            ts: self.ts.clone(),
            tc: self.tc.clone(),
            tf: self.tf.clone(),
            tmaxw: self.tmaxw.clone(),
            tmaxh: self.tmaxh.clone(),
            ta: self.ta.clone(),
            tw: self.tw.clone(),
            to: self.to.clone(),
            tow: self.tow.clone(),
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
            shadow_offset_y: query.oshy.get(i).copied(),
            shadow_blur: query.oshb.get(i).copied(),
            shadow_alpha_pct: query.osha.get(i).copied(),
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
            weight: sanitize_weight(query.tw.get(i).map(String::as_str)),
            // An outline is only applied when a non-empty color is supplied.
            outline_color: query
                .to
                .get(i)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            outline_width: query.tow.get(i).copied(),
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
    // Background appearance filters, applied after resize and before overlays
    // so the overlay stays crisp and bright against a softened backdrop.
    let blur_sigma = query.blur().unwrap_or(0) as f32;
    let brightness_factor = query.bri().map(|b| b as f32 / 100.0).unwrap_or(1.0);
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

        // Soften and dim the background before overlays and text land on top.
        if blur_sigma > 0.0 {
            result_image = apply_blur(&result_image, blur_sigma);
        }
        result_image = apply_brightness(result_image, brightness_factor);

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

    // Cheap Arc pointer-clone — the shared fontdb stays shared across all
    // requests. `font_family` points unresolved/generic families at a real
    // installed sans-serif instead of usvg's built-in serif default.
    let options = usvg::Options {
        fontdb: FONTS.db.clone(),
        font_family: FONTS.default_family.clone(),
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
