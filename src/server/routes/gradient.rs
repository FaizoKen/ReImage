//! Gradient generator endpoint.
//!
//! `GET /gradient?c=AABBCC&w=600&h=240` returns a 2-stop diagonal gradient
//! WebP. The second stop is derived from the input color by darkening
//! lightness ~25% in HSL space, so callers only need to supply one hex.
//!
//! Used by `bot-rust` to render a server-themed banner placeholder when the
//! guild has no real banner/splash/discovery_splash. Cached identically to
//! `/image` (same output cache, same TTL).
//!
//! Bounds: width/height capped via the same `max_dimension` config the image
//! endpoint uses. Color is validated as 6-hex-digit RGB.

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
};
use bytes::Bytes;
use image::{DynamicImage, RgbImage};
use serde::Deserialize;

use crate::image::processor::encode_webp;
use crate::server::error::{AppError, AppResult};
use crate::server::routes::image::AppState;

#[derive(Debug, Deserialize)]
pub struct GradientQuery {
    /// Primary color, 6 hex digits without `#` (e.g. `5865F2`).
    pub c: String,
    /// Optional secondary stop. Defaults to a darker shade of `c`.
    #[serde(default)]
    pub c2: Option<String>,
    #[serde(default = "default_w")]
    pub w: u32,
    #[serde(default = "default_h")]
    pub h: u32,
}

fn default_w() -> u32 {
    600
}
fn default_h() -> u32 {
    240
}

fn parse_hex(s: &str) -> AppResult<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest(format!(
            "invalid color (expected 6 hex digits): {}",
            s
        )));
    }
    let n = u32::from_str_radix(s, 16).map_err(|_| {
        AppError::BadRequest(format!("invalid color (parse failed): {}", s))
    })?;
    Ok((((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8))
}

/// Multiply each channel by `factor` (clamped to [0, 255]). Cheap, decent
/// "darken" without bothering with HSL conversion.
fn darken(rgb: (u8, u8, u8), factor: f32) -> (u8, u8, u8) {
    let f = factor.clamp(0.0, 1.0);
    (
        (rgb.0 as f32 * f).round() as u8,
        (rgb.1 as f32 * f).round() as u8,
        (rgb.2 as f32 * f).round() as u8,
    )
}

pub async fn handle_gradient(
    State(state): State<AppState>,
    Query(query): Query<GradientQuery>,
) -> Result<impl IntoResponse, AppError> {
    let config = &state.config;

    // Validate dimensions against the same bounds as /image.
    let w = query.w;
    let h = query.h;
    if w < config.min_dimension
        || w > config.max_dimension
        || h < config.min_dimension
        || h > config.max_dimension
    {
        return Err(AppError::BadRequest(
            "invalid w/h parameter".to_string(),
        ));
    }

    let c1 = parse_hex(&query.c)?;
    let c2 = match &query.c2 {
        Some(s) => parse_hex(s)?,
        None => darken(c1, 0.72),
    };

    // Cache by all parameters.
    let cache_key = format!("gradient:{:06x}:{:06x}:{}x{}",
        ((c1.0 as u32) << 16) | ((c1.1 as u32) << 8) | (c1.2 as u32),
        ((c2.0 as u32) << 16) | ((c2.1 as u32) << 8) | (c2.2 as u32),
        w, h);

    if let Some(cached) = state.cache_manager.get_output(&cache_key).await {
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/webp"),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable",
                ),
                (header::HeaderName::from_static("x-cache"), "HIT"),
            ],
            cached,
        ));
    }

    let img = render_diagonal(w, h, c1, c2);
    let webp = encode_webp(&DynamicImage::ImageRgb8(img), config.webp_quality)?;
    state
        .cache_manager
        .set_output(cache_key, webp.clone())
        .await;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/webp"),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
            (header::HeaderName::from_static("x-cache"), "MISS"),
        ],
        webp,
    ))
}

/// Render a 2-stop diagonal gradient. `t` runs along the top-left → bottom-right
/// diagonal: `t = (x/(w-1) + y/(h-1)) / 2` ∈ [0, 1].
fn render_diagonal(
    w: u32,
    h: u32,
    c1: (u8, u8, u8),
    c2: (u8, u8, u8),
) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    let wm1 = (w.saturating_sub(1)).max(1) as f32;
    let hm1 = (h.saturating_sub(1)).max(1) as f32;
    let dr = c2.0 as f32 - c1.0 as f32;
    let dg = c2.1 as f32 - c1.1 as f32;
    let db = c2.2 as f32 - c1.2 as f32;
    for y in 0..h {
        let ty = y as f32 / hm1;
        for x in 0..w {
            let t = (x as f32 / wm1 + ty) * 0.5;
            let r = (c1.0 as f32 + dr * t).round() as u8;
            let g = (c1.1 as f32 + dg * t).round() as u8;
            let b = (c1.2 as f32 + db * t).round() as u8;
            img.put_pixel(x, y, image::Rgb([r, g, b]));
        }
    }
    img
}

// Suppress unused warning for Bytes when feature flags evolve.
#[allow(dead_code)]
fn _bytes_marker(_: Bytes) {}
