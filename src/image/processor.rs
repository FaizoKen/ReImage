use bytes::Bytes;
use image::{DynamicImage, GenericImageView, RgbaImage};
use rayon::prelude::*;
use std::io::Cursor;

use crate::server::error::{AppError, AppResult};

/// Maximum megapixels allowed (decompression bomb protection)
const MAX_MEGAPIXELS: u32 = 100_000_000;

/// Decode image from bytes
pub fn decode_image(data: &[u8]) -> AppResult<DynamicImage> {
    let cursor = Cursor::new(data);
    image::io::Reader::new(cursor)
        .with_guessed_format()
        .map_err(|e| AppError::ImageProcessing(format!("Failed to detect format: {}", e)))?
        .decode()
        .map_err(|e| AppError::ImageProcessing(format!("Failed to decode image: {}", e)))
}

/// Get image dimensions and validate
pub fn get_metadata(img: &DynamicImage) -> AppResult<(u32, u32)> {
    let (width, height) = img.dimensions();

    // Decompression bomb check
    let pixels = width as u64 * height as u64;
    if pixels > MAX_MEGAPIXELS as u64 {
        return Err(AppError::BadRequest(
            "Image dimensions exceed maximum allowed".to_string(),
        ));
    }

    Ok((width, height))
}

/// Calculate final dimensions maintaining aspect ratio
/// Allows upscaling beyond original dimensions (matching Node.js behavior)
pub fn calculate_dimensions(
    orig_width: u32,
    orig_height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> (u32, u32) {
    match (max_width, max_height) {
        (None, None) => (orig_width, orig_height),
        (Some(mw), None) => {
            let aspect = orig_width as f64 / orig_height as f64;
            let h = (mw as f64 / aspect).round() as u32;
            (mw, h.max(1))
        }
        (None, Some(mh)) => {
            let aspect = orig_width as f64 / orig_height as f64;
            let w = (mh as f64 * aspect).round() as u32;
            (w.max(1), mh)
        }
        (Some(mw), Some(mh)) => {
            let aspect = orig_width as f64 / orig_height as f64;
            let target_aspect = mw as f64 / mh as f64;

            if target_aspect > aspect {
                // Target is wider, fit to height
                let w = (mh as f64 * aspect).round() as u32;
                (w.max(1), mh)
            } else {
                // Target is taller, fit to width
                let h = (mw as f64 / aspect).round() as u32;
                (mw, h.max(1))
            }
        }
    }
}

/// Resize image maintaining aspect ratio.
///
/// Uses `fast_image_resize` (SIMD: SSE4.1/AVX2/NEON), 3-10× faster than
/// `image::resize_exact` at the same visual quality. Default filter is
/// CatmullRom (bicubic); override via `REIMAGE_RESIZE_FILTER` env var.
/// Always returns an RGBA8 image — the downstream composite path needs
/// RGBA anyway, so this saves one conversion when the source was non-RGBA.
pub fn resize_image(
    img: &DynamicImage,
    width: u32,
    height: u32,
) -> DynamicImage {
    use fast_image_resize as fr;
    use fr::images::Image as FrImage;

    // Convert source to RGBA8 (fast_image_resize works on raw typed buffers).
    let rgba_src = img.to_rgba8();
    let (sw, sh) = rgba_src.dimensions();

    let src = match FrImage::from_vec_u8(sw, sh, rgba_src.into_raw(), fr::PixelType::U8x4) {
        Ok(s) => s,
        // Fallback to the pure-Rust path on the (impossible) construction
        // failure — keeps the function infallible to match the old signature.
        Err(_) => {
            return img.resize_exact(width, height, image::imageops::FilterType::CatmullRom);
        }
    };

    let mut dst = FrImage::new(width, height, fr::PixelType::U8x4);
    let mut resizer = fr::Resizer::new();
    let opts = fr::ResizeOptions::new().resize_alg(resize_alg());

    if resizer.resize(&src, &mut dst, &opts).is_err() {
        return img.resize_exact(width, height, image::imageops::FilterType::CatmullRom);
    }

    let raw = dst.into_vec();
    match RgbaImage::from_raw(width, height, raw) {
        Some(out) => DynamicImage::ImageRgba8(out),
        None => img.resize_exact(width, height, image::imageops::FilterType::CatmullRom),
    }
}

/// Resize-and-crop an image to exactly `target_w` × `target_h` (cover fit).
///
/// The image is scaled — preserving aspect ratio — to fully cover the target
/// box, then the overflow is cropped away. `focus_y` (0.0 = top .. 1.0 =
/// bottom) chooses which vertical slice survives; the horizontal axis is
/// always centred. Used by `/image` when both `maxw` and `maxh` are supplied.
pub fn resize_crop_cover(
    img: &DynamicImage,
    target_w: u32,
    target_h: u32,
    focus_y: f32,
) -> DynamicImage {
    let (sw, sh) = img.dimensions();
    if sw == 0 || sh == 0 || target_w == 0 || target_h == 0 {
        return img.clone();
    }

    // Cover scale: the larger of the two ratios, so the scaled image fully
    // covers the box with overflow on at most one axis.
    let scale = (target_w as f64 / sw as f64).max(target_h as f64 / sh as f64);
    let scaled_w = ((sw as f64 * scale).round() as u32).max(target_w);
    let scaled_h = ((sh as f64 * scale).round() as u32).max(target_h);

    let scaled = resize_image(img, scaled_w, scaled_h);

    // Centre horizontally; place the vertical window by the focus point.
    let crop_x = (scaled_w - target_w) / 2;
    let max_y = scaled_h - target_h;
    let crop_y = ((max_y as f32) * focus_y.clamp(0.0, 1.0)).round() as u32;

    scaled.crop_imm(crop_x, crop_y.min(max_y), target_w, target_h)
}

/// Cached resize-algorithm choice. Read once at first use.
fn resize_alg() -> fast_image_resize::ResizeAlg {
    use fast_image_resize::{FilterType, ResizeAlg};
    use once_cell::sync::Lazy;
    static ALG: Lazy<ResizeAlg> = Lazy::new(|| {
        match std::env::var("REIMAGE_RESIZE_FILTER")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("nearest") => ResizeAlg::Nearest,
            Some("triangle") | Some("bilinear") => {
                ResizeAlg::Convolution(FilterType::Bilinear)
            }
            Some("gaussian") => ResizeAlg::Convolution(FilterType::Gaussian),
            Some("lanczos3") => ResizeAlg::Convolution(FilterType::Lanczos3),
            Some("hamming") => ResizeAlg::Convolution(FilterType::Hamming),
            Some("mitchell") => ResizeAlg::Convolution(FilterType::Mitchell),
            _ => ResizeAlg::Convolution(FilterType::CatmullRom),
        }
    });
    *ALG
}

/// Encode image to WebP format.
pub fn encode_webp(img: &DynamicImage, quality: u8) -> AppResult<Bytes> {
    // Borrow the RGBA buffer when already in that format; else convert.
    let rgba = match img {
        DynamicImage::ImageRgba8(rgba) => std::borrow::Cow::Borrowed(rgba),
        _ => std::borrow::Cow::Owned(img.to_rgba8()),
    };
    let (width, height) = rgba.dimensions();

    // The `webp` crate's `WebPMemory` is neither `Send` nor `AsRef<[u8]>`, so
    // we cannot hand ownership to `Bytes` directly — copy out once. This is a
    // single sequential copy of an already-compressed buffer (<1 MB for the
    // sizes this service serves), dominated by the encode itself.
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), width, height);
    let webp_data = encoder.encode(quality as f32);
    Ok(Bytes::copy_from_slice(&webp_data))
}

/// Create a rounded corner mask as SVG
pub fn create_rounded_mask_svg(width: u32, height: u32, radius: u32) -> String {
    format!(
        r#"<svg width="{}" height="{}"><rect x="0" y="0" width="{}" height="{}" rx="{}" ry="{}" fill="black"/></svg>"#,
        width, height, width, height, radius, radius
    )
}

/// Apply rounded corners to an image using alpha masking (parallelized).
/// Skips conversion + clone when the input is already RGBA8 by mutating in
/// place via the owned-by-value variant below.
pub fn apply_rounded_corners(img: &DynamicImage, radius: u32) -> AppResult<DynamicImage> {
    let mut rgba = img.to_rgba8();
    apply_rounded_corners_inplace(&mut rgba, radius);
    Ok(DynamicImage::ImageRgba8(rgba))
}

/// In-place variant — used after `result_image.to_rgba8()` was already
/// allocated for composite work, so the corner-clipping step doesn't
/// allocate another buffer.
pub fn apply_rounded_corners_inplace(rgba: &mut RgbaImage, radius: u32) {
    let (width, height) = rgba.dimensions();
    let radius = radius.min(width / 2).min(height / 2);

    if radius == 0 {
        return;
    }

    let radius_sq = radius * radius;
    let right_edge = width - radius;
    let bottom_edge = height - radius;

    // Pre-compute corner centers
    let top_left = (radius, radius);
    let top_right = (width - radius - 1, radius);
    let bottom_left = (radius, height - radius - 1);
    let bottom_right = (width - radius - 1, height - radius - 1);

    // Process rows in parallel using rayon
    rgba.par_chunks_mut((width * 4) as usize).enumerate().for_each(|(y, row)| {
        let y = y as u32;

        // Only process rows that could have corners
        if y >= radius && y < bottom_edge {
            return;
        }

        for x in 0..width {
            // Only check pixels that could be in corner regions
            if x >= radius && x < right_edge {
                continue;
            }

            let should_clip = {
                // Inline corner check for each region
                if x < radius && y < radius {
                    // Top-left corner
                    let dx = top_left.0 - x;
                    let dy = top_left.1 - y;
                    dx * dx + dy * dy > radius_sq
                } else if x >= right_edge && y < radius {
                    // Top-right corner
                    let dx = x - top_right.0;
                    let dy = top_right.1 - y;
                    dx * dx + dy * dy > radius_sq
                } else if x < radius && y >= bottom_edge {
                    // Bottom-left corner
                    let dx = bottom_left.0 - x;
                    let dy = y - bottom_left.1;
                    dx * dx + dy * dy > radius_sq
                } else if x >= right_edge && y >= bottom_edge {
                    // Bottom-right corner
                    let dx = x - bottom_right.0;
                    let dy = y - bottom_right.1;
                    dx * dx + dy * dy > radius_sq
                } else {
                    false
                }
            };

            if should_clip {
                row[(x * 4 + 3) as usize] = 0; // Set alpha to 0
            }
        }
    });
}

/// Composite overlay image onto base at given position (optimized with integer math).
/// Borrows the overlay's RGBA buffer when it's already RGBA8 to avoid a full
/// per-call clone — every cached overlay reuse used to pay that copy.
pub fn composite_overlay(
    base: &mut RgbaImage,
    overlay: &DynamicImage,
    x: i64,
    y: i64,
) {
    let overlay_rgba: std::borrow::Cow<RgbaImage> = match overlay {
        DynamicImage::ImageRgba8(rgba) => std::borrow::Cow::Borrowed(rgba),
        other => std::borrow::Cow::Owned(other.to_rgba8()),
    };
    let (overlay_width, overlay_height) = overlay_rgba.dimensions();
    let (base_width, base_height) = base.dimensions();

    // Pre-compute bounds to avoid repeated checks.
    // For negative offsets the overlay starts above/left of the canvas, so the
    // visible window extends `base_dim + |offset|` into the overlay — not just
    // `base_dim`. Use the unified formula `base_dim - offset` which handles
    // both positive and negative `offset` correctly (clamped to non-negative
    // for safety against far-right/far-down placements).
    let start_ox = if x < 0 { (-x) as u32 } else { 0 };
    let start_oy = if y < 0 { (-y) as u32 } else { 0 };
    let end_ox = overlay_width.min((base_width as i64 - x).max(0) as u32);
    let end_oy = overlay_height.min((base_height as i64 - y).max(0) as u32);

    // Early exit if overlay is completely outside
    if start_ox >= end_ox || start_oy >= end_oy {
        return;
    }

    let base_start_x = (x.max(0)) as u32;
    let base_start_y = (y.max(0)) as u32;

    // Get raw pixel slices for direct manipulation
    let overlay_raw = overlay_rgba.as_raw();
    let base_raw: &mut [u8] = base.as_mut();

    // Process rows - use parallel processing for larger overlays
    let rows_to_process = end_oy - start_oy;
    let cols_to_process = end_ox - start_ox;

    // Use parallel processing for larger images (threshold: 64x64 = 4096 pixels)
    let total_pixels = rows_to_process * cols_to_process;
    if total_pixels > 4096 {
        let overlay_width_usize = overlay_width as usize;
        let base_width_usize = base_width as usize;
        let row_stride = base_width_usize * 4;

        // Split base image into rows for parallel processing
        // Each row is independent so we can safely process them in parallel
        base_raw
            .par_chunks_mut(row_stride)
            .enumerate()
            .skip(base_start_y as usize)
            .take(rows_to_process as usize)
            .for_each(|(by, base_row)| {
                let row_offset = (by - base_start_y as usize) as u32;
                let oy = start_oy + row_offset;

                for col_offset in 0..cols_to_process {
                    let ox = start_ox + col_offset;
                    let bx = base_start_x + col_offset;

                    let oi = ((oy as usize * overlay_width_usize) + ox as usize) * 4;
                    let bi = (bx as usize) * 4;

                    // Fast integer alpha blending (avoids floating point)
                    let alpha = overlay_raw[oi + 3] as u32;
                    if alpha == 0 {
                        continue; // Fully transparent, skip
                    }

                    if alpha == 255 {
                        // Fully opaque, direct copy
                        base_row[bi] = overlay_raw[oi];
                        base_row[bi + 1] = overlay_raw[oi + 1];
                        base_row[bi + 2] = overlay_raw[oi + 2];
                        base_row[bi + 3] = 255;
                    } else {
                        // Integer alpha blending: (src * alpha + dst * (255 - alpha) + 127) / 255
                        let inv_alpha = 255 - alpha;
                        base_row[bi] = ((overlay_raw[oi] as u32 * alpha + base_row[bi] as u32 * inv_alpha + 127) / 255) as u8;
                        base_row[bi + 1] = ((overlay_raw[oi + 1] as u32 * alpha + base_row[bi + 1] as u32 * inv_alpha + 127) / 255) as u8;
                        base_row[bi + 2] = ((overlay_raw[oi + 2] as u32 * alpha + base_row[bi + 2] as u32 * inv_alpha + 127) / 255) as u8;
                        base_row[bi + 3] = ((alpha * 255 + base_row[bi + 3] as u32 * inv_alpha + 127) / 255).min(255) as u8;
                    }
                }
            });
    } else {
        // Sequential processing for smaller images (avoid parallel overhead)
        let overlay_width_usize = overlay_width as usize;
        let base_width_usize = base_width as usize;

        for row_offset in 0..rows_to_process {
            let oy = start_oy + row_offset;
            let by = base_start_y + row_offset;

            for col_offset in 0..cols_to_process {
                let ox = start_ox + col_offset;
                let bx = base_start_x + col_offset;

                let oi = ((oy as usize * overlay_width_usize) + ox as usize) * 4;
                let bi = ((by as usize * base_width_usize) + bx as usize) * 4;

                let alpha = overlay_raw[oi + 3] as u32;
                if alpha == 0 {
                    continue;
                }

                if alpha == 255 {
                    base_raw[bi] = overlay_raw[oi];
                    base_raw[bi + 1] = overlay_raw[oi + 1];
                    base_raw[bi + 2] = overlay_raw[oi + 2];
                    base_raw[bi + 3] = 255;
                } else {
                    let inv_alpha = 255 - alpha;
                    base_raw[bi] = ((overlay_raw[oi] as u32 * alpha + base_raw[bi] as u32 * inv_alpha + 127) / 255) as u8;
                    base_raw[bi + 1] = ((overlay_raw[oi + 1] as u32 * alpha + base_raw[bi + 1] as u32 * inv_alpha + 127) / 255) as u8;
                    base_raw[bi + 2] = ((overlay_raw[oi + 2] as u32 * alpha + base_raw[bi + 2] as u32 * inv_alpha + 127) / 255) as u8;
                    base_raw[bi + 3] = ((alpha * 255 + base_raw[bi + 3] as u32 * inv_alpha + 127) / 255).min(255) as u8;
                }
            }
        }
    }
}

/// Sizing parameters for the overlay's drop shadow, scaled mildly to overlay size.
/// Matches the modal's CSS `drop-shadow(0 4px 8px rgba(0,0,0,0.5))` at typical
/// avatar sizes (~128 px) and grows gently from there. No ring — the modal
/// preview is shadow-only.
#[derive(Debug, Clone, Copy)]
pub struct DecorationParams {
    pub shadow_offset_y: i32,
    pub shadow_blur_sigma: f32,
}

impl DecorationParams {
    pub fn from_overlay_size(width: u32, height: u32) -> Self {
        let size = width.min(height).max(1);
        // Modal preview uses fixed 4px offset / 8px blur (sigma ≈ 4). Scale
        // very gently so larger overlays still get a visible shadow without
        // looking heavy.
        let shadow_offset_y = ((size / 32) as i32).max(4);
        let shadow_blur_sigma = (size as f32 / 24.0).max(4.0);
        Self {
            shadow_offset_y,
            shadow_blur_sigma,
        }
    }

    /// Symmetric padding around the overlay needed to fit shadow without clipping.
    pub fn padding(&self) -> u32 {
        let blur_radius = (self.shadow_blur_sigma * 2.5).ceil() as u32;
        blur_radius + self.shadow_offset_y.unsigned_abs() + 2
    }
}

/// Drop-shadow tint alpha before Gaussian spread (matches modal's 50%-black
/// drop-shadow once the blur disperses it).
const SHADOW_TINT_ALPHA: u32 = 128;

/// Apply a drop shadow to an overlay image. Returns the decorated image and
/// the (x, y) padding added — callers should subtract this from the overlay's
/// render position so the visible image stays in place.
///
/// Matches the modal's CSS `drop-shadow(0 4px 8px rgba(0,0,0,0.5))`. The
/// indigo ring that previously wrapped the overlay was removed because the
/// modal preview no longer shows it (button label is now just "Shadow"); the
/// ring produced a visible halo in the rendered output that didn't appear in
/// the preview.
pub fn apply_overlay_decorations(
    overlay: &DynamicImage,
    params: &DecorationParams,
) -> AppResult<(DynamicImage, u32, u32)> {
    let (ow, oh) = overlay.dimensions();
    if ow == 0 || oh == 0 {
        return Ok((overlay.clone(), 0, 0));
    }

    let pad = params.padding();
    let new_w = ow + pad * 2;
    let new_h = oh + pad * 2;

    let overlay_rgba = match overlay {
        DynamicImage::ImageRgba8(rgba) => rgba.clone(),
        other => other.to_rgba8(),
    };

    // 1. Shadow silhouette: copy alpha into a black layer, offset down by shadow_offset_y.
    let mut shadow_layer = RgbaImage::new(new_w, new_h);
    {
        let shadow_off_x = pad as i32;
        let shadow_off_y = pad as i32 + params.shadow_offset_y;
        let stride = new_w as usize * 4;
        let src = overlay_rgba.as_raw();
        let dst: &mut [u8] = shadow_layer.as_mut();

        dst.par_chunks_mut(stride)
            .enumerate()
            .for_each(|(cy, row)| {
                let oy = cy as i32 - shadow_off_y;
                if oy < 0 || oy >= oh as i32 {
                    return;
                }
                let src_row_start = (oy as usize) * ow as usize * 4;
                for ox in 0..ow {
                    let cx = ox as i32 + shadow_off_x;
                    if cx < 0 || cx >= new_w as i32 {
                        continue;
                    }
                    let src_a = src[src_row_start + ox as usize * 4 + 3] as u32;
                    if src_a == 0 {
                        continue;
                    }
                    let a = (src_a * SHADOW_TINT_ALPHA / 255) as u8;
                    let i = cx as usize * 4;
                    row[i] = 0;
                    row[i + 1] = 0;
                    row[i + 2] = 0;
                    row[i + 3] = a;
                }
            });
    }
    let shadow_blurred = image::imageops::blur(&shadow_layer, params.shadow_blur_sigma);

    // Composite: shadow → overlay. The overlay's own pixels cover the shadow
    // silhouette beneath it, leaving only the offset/blurred tail visible.
    let mut canvas = RgbaImage::new(new_w, new_h);
    composite_rgba(&mut canvas, &shadow_blurred);
    composite_overlay(&mut canvas, overlay, pad as i64, pad as i64);

    Ok((DynamicImage::ImageRgba8(canvas), pad, pad))
}

/// Alpha-blend one RGBA image onto another of the same dimensions. Source-over.
fn composite_rgba(base: &mut RgbaImage, top: &RgbaImage) {
    let (bw, bh) = base.dimensions();
    let (tw, th) = top.dimensions();
    debug_assert_eq!((bw, bh), (tw, th));
    let stride = bw as usize * 4;
    let top_raw = top.as_raw();
    let base_raw: &mut [u8] = base.as_mut();

    base_raw
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row_start = y * stride;
            for x in 0..bw as usize {
                let si = src_row_start + x * 4;
                let alpha = top_raw[si + 3] as u32;
                if alpha == 0 {
                    continue;
                }
                let bi = x * 4;
                if alpha == 255 {
                    row[bi] = top_raw[si];
                    row[bi + 1] = top_raw[si + 1];
                    row[bi + 2] = top_raw[si + 2];
                    row[bi + 3] = 255;
                } else {
                    let inv = 255 - alpha;
                    row[bi] = ((top_raw[si] as u32 * alpha + row[bi] as u32 * inv + 127) / 255) as u8;
                    row[bi + 1] =
                        ((top_raw[si + 1] as u32 * alpha + row[bi + 1] as u32 * inv + 127) / 255) as u8;
                    row[bi + 2] =
                        ((top_raw[si + 2] as u32 * alpha + row[bi + 2] as u32 * inv + 127) / 255) as u8;
                    row[bi + 3] =
                        ((alpha * 255 + row[bi + 3] as u32 * inv + 127) / 255).min(255) as u8;
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimension_calculation() {
        // Original 1000x500
        assert_eq!(calculate_dimensions(1000, 500, Some(500), None), (500, 250));
        assert_eq!(calculate_dimensions(1000, 500, None, Some(250)), (500, 250));
        assert_eq!(calculate_dimensions(1000, 500, Some(400), Some(300)), (400, 200));

        // No resize
        assert_eq!(calculate_dimensions(1000, 500, None, None), (1000, 500));

        // Upscaling allowed
        assert_eq!(calculate_dimensions(100, 50, Some(200), None), (200, 100));
    }

    #[test]
    fn resize_crop_cover_produces_exact_dimensions() {
        let src = DynamicImage::ImageRgba8(RgbaImage::new(600, 240));

        // A wide source cropped to a shorter box keeps the exact target size.
        assert_eq!(
            resize_crop_cover(&src, 480, 192, 0.5).dimensions(),
            (480, 192)
        );

        // Focus extremes stay in bounds and still yield the exact size.
        assert_eq!(resize_crop_cover(&src, 480, 100, 0.0).dimensions(), (480, 100));
        assert_eq!(resize_crop_cover(&src, 480, 100, 1.0).dimensions(), (480, 100));

        // Degenerate inputs return the source untouched rather than panicking.
        assert_eq!(resize_crop_cover(&src, 0, 100, 0.5).dimensions(), (600, 240));
    }

    #[test]
    fn composite_overlay_negative_offset_does_not_clip_visible_area() {
        // Regression: when an overlay is placed at negative coordinates (as the
        // `odeco` decoration does to keep the visible image at the caller's
        // (x, y)), the renderer previously capped the read window at the base
        // canvas size, clipping the bottom/right of the visible overlay.
        let mut base = RgbaImage::from_pixel(40, 40, image::Rgba([0, 0, 0, 255]));
        let overlay = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            60,
            60,
            image::Rgba([255, 0, 0, 255]),
        ));
        // Place overlay so its centre 40x40 covers the canvas: padding of 10 on each side.
        composite_overlay(&mut base, &overlay, -10, -10);
        // Every canvas pixel must now be red — none of the visible window is lost.
        for px in base.pixels() {
            assert_eq!(px.0, [255, 0, 0, 255]);
        }
    }
}
