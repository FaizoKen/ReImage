//! Gradient generator endpoint.
//!
//! `GET /gradient?c=AABBCC&w=600&h=240` returns a Discord-style radial
//! gradient WebP: a lighter tint of `c` glows up from the bottom-centre to
//! the base colour `c` at the top and edges. Mirrors the CSS Discord emits
//! for icon-derived server banners:
//!
//!   radial-gradient(105.43% 127.05% at 50.1% 127.05%,
//!                   <lighter> 20.65%, <c> 85.16%)
//!
//! Callers supply only `c` (the icon-derived base colour); the lighter inner
//! stop is derived by blending `c` toward white. `c2` can override it.
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
    /// Optional inner (bottom-centre) colour. Defaults to a lighter tint of
    /// `c` blended toward white.
    #[serde(default)]
    pub c2: Option<String>,
    /// When omitted, falls back to `GRADIENT_MAX_WIDTH`.
    #[serde(default)]
    pub w: Option<u32>,
    /// When omitted, falls back to `GRADIENT_MAX_HEIGHT`.
    #[serde(default)]
    pub h: Option<u32>,
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

/// Factor used to derive the lighter inner stop from the base colour `c`.
/// Blending `c=040404` toward white by this amount yields `rgb(77,77,77)`,
/// matching Discord's gradient for a pure-black server icon.
const LIGHTEN_FACTOR: f32 = 0.29;

/// Blend each channel toward white by `factor` (clamped to [0, 1]). Used to
/// derive the bright inner gradient stop from the base colour.
fn lighten(rgb: (u8, u8, u8), factor: f32) -> (u8, u8, u8) {
    let f = factor.clamp(0.0, 1.0);
    let mix = |c: u8| (c as f32 + (255.0 - c as f32) * f).round() as u8;
    (mix(rgb.0), mix(rgb.1), mix(rgb.2))
}

pub async fn handle_gradient(
    State(state): State<AppState>,
    Query(query): Query<GradientQuery>,
) -> Result<impl IntoResponse, AppError> {
    let config = &state.config;

    // Validate dimensions. `/gradient` uses its own caps (configurable via
    // GRADIENT_MAX_WIDTH / GRADIENT_MAX_HEIGHT) — separate from the much
    // larger limits the `/image` endpoint allows for real photos. When the
    // caller omits w/h the configured cap doubles as the default so bumping
    // GRADIENT_MAX_HEIGHT enlarges the rendered banner without forcing every
    // caller to pass an explicit h=.
    let w = query.w.unwrap_or(config.gradient_max_width);
    let h = query.h.unwrap_or(config.gradient_max_height);
    if w < config.min_dimension
        || w > config.gradient_max_width
        || h < config.min_dimension
        || h > config.gradient_max_height
    {
        return Err(AppError::BadRequest(format!(
            "w/h out of range (w<={}, h<={})",
            config.gradient_max_width, config.gradient_max_height
        )));
    }

    // `c` is the icon-derived base colour — it sits at the outer edge of the
    // radial gradient. The inner (bottom-centre) stop is a lighter tint.
    let outer = parse_hex(&query.c)?;
    let inner = match &query.c2 {
        Some(s) => parse_hex(s)?,
        None => lighten(outer, LIGHTEN_FACTOR),
    };

    // Cache by all parameters.
    let cache_key = format!("gradient:{:06x}:{:06x}:{}x{}",
        ((inner.0 as u32) << 16) | ((inner.1 as u32) << 8) | (inner.2 as u32),
        ((outer.0 as u32) << 16) | ((outer.1 as u32) << 8) | (outer.2 as u32),
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

    let img = render_radial(w, h, inner, outer);
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

/// Render a Discord-style radial gradient.
///
/// Mirrors the CSS Discord emits for icon-derived server banners:
///
///   radial-gradient(105.43% 127.05% at 50.1% 127.05%,
///                   <inner> 20.65%, <outer> 85.16%)
///
/// An ellipse is centred just below the banner (50.1% across, 127.05% down),
/// with radii 105.43%/127.05% of the banner's width/height. The lighter
/// `inner` colour sits at the ellipse centre and the darker `outer` colour at
/// its edge; the colour stops sit at 20.65% and 85.16% of the radius, so the
/// banner shows a soft glow rising from the bottom-centre.
///
/// Unlike the old vertical gradient the colour varies per pixel (the centre
/// is off-axis), so each pixel is written individually rather than memcpy'd
/// across a row.
fn render_radial(
    w: u32,
    h: u32,
    inner: (u8, u8, u8),
    outer: (u8, u8, u8),
) -> RgbImage {
    // Ellipse geometry, in pixels, straight from the Discord CSS.
    let wf = w as f32;
    let hf = h as f32;
    let cx = 0.501 * wf;
    let cy = 1.2705 * hf;
    let rx = 1.0543 * wf;
    let ry = 1.2705 * hf;
    // Colour stops as fractions of the gradient ray (0 = centre, 1 = edge).
    const STOP0: f32 = 0.2065;
    const STOP1: f32 = 0.8516;
    let span = STOP1 - STOP0;

    let dr = outer.0 as f32 - inner.0 as f32;
    let dg = outer.1 as f32 - inner.1 as f32;
    let db = outer.2 as f32 - inner.2 as f32;

    let w_usize = w as usize;
    let row_bytes = w_usize * 3;
    let mut buf = vec![0u8; row_bytes * h as usize];

    for y in 0..h {
        let ny = (y as f32 + 0.5 - cy) / ry;
        let row_start = y as usize * row_bytes;
        let row = &mut buf[row_start..row_start + row_bytes];
        for (x, chunk) in row.chunks_exact_mut(3).enumerate() {
            let nx = (x as f32 + 0.5 - cx) / rx;
            let dist = (nx * nx + ny * ny).sqrt();
            // Position within the inner↔outer ramp, flat outside the stops.
            let t = ((dist - STOP0) / span).clamp(0.0, 1.0);
            chunk[0] = (inner.0 as f32 + dr * t).round() as u8;
            chunk[1] = (inner.1 as f32 + dg * t).round() as u8;
            chunk[2] = (inner.2 as f32 + db * t).round() as u8;
        }
    }

    RgbImage::from_raw(w, h, buf).expect("buffer sized correctly")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lighten_matches_discord_black_banner() {
        // Discord renders rgb(4,4,4) → rgb(77,77,77) for a pure-black icon.
        assert_eq!(lighten((4, 4, 4), LIGHTEN_FACTOR), (77, 77, 77));
    }

    #[test]
    fn radial_is_light_at_bottom_centre_dark_at_top() {
        let img = render_radial(480, 160, (77, 77, 77), (4, 4, 4));
        let bottom_centre = img.get_pixel(240, 159).0[0];
        let top_centre = img.get_pixel(240, 0).0[0];
        // Bottom-centre sits near the inner stop; top-centre near the outer.
        assert!(
            bottom_centre > top_centre,
            "bottom {bottom_centre} should be lighter than top {top_centre}"
        );
        assert!(bottom_centre >= 70, "bottom too dark: {bottom_centre}");
        assert!(top_centre <= 10, "top too light: {top_centre}");
    }
}

// Suppress unused warning for Bytes when feature flags evolve.
#[allow(dead_code)]
fn _bytes_marker(_: Bytes) {}
