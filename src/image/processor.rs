use bytes::Bytes;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage};
use rayon::prelude::*;
use std::io::Cursor;

use crate::config::Config;
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

/// Resize image maintaining aspect ratio
pub fn resize_image(
    img: &DynamicImage,
    width: u32,
    height: u32,
) -> DynamicImage {
    img.resize_exact(width, height, image::imageops::FilterType::Lanczos3)
}

/// Encode image to WebP format (optimized to avoid copies)
pub fn encode_webp(img: &DynamicImage, quality: u8) -> AppResult<Bytes> {
    // Try to get RGBA8 directly if already in that format, avoiding conversion
    let rgba = match img {
        DynamicImage::ImageRgba8(rgba) => std::borrow::Cow::Borrowed(rgba),
        _ => std::borrow::Cow::Owned(img.to_rgba8()),
    };
    let (width, height) = rgba.dimensions();

    // Use webp crate for encoding - encode returns WebPMemory which owns the data
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), width, height);
    let webp_data = encoder.encode(quality as f32);

    // WebPMemory derefs to &[u8], copy into Bytes
    Ok(Bytes::copy_from_slice(&webp_data))
}

/// Create a rounded corner mask as SVG
pub fn create_rounded_mask_svg(width: u32, height: u32, radius: u32) -> String {
    format!(
        r#"<svg width="{}" height="{}"><rect x="0" y="0" width="{}" height="{}" rx="{}" ry="{}" fill="black"/></svg>"#,
        width, height, width, height, radius, radius
    )
}

/// Apply rounded corners to an image using alpha masking (parallelized)
pub fn apply_rounded_corners(img: &DynamicImage, radius: u32) -> AppResult<DynamicImage> {
    let (width, height) = img.dimensions();
    let mut rgba = img.to_rgba8();
    let radius = radius.min(width / 2).min(height / 2);

    if radius == 0 {
        return Ok(DynamicImage::ImageRgba8(rgba));
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

    Ok(DynamicImage::ImageRgba8(rgba))
}

/// Composite overlay image onto base at given position (optimized with integer math)
pub fn composite_overlay(
    base: &mut RgbaImage,
    overlay: &DynamicImage,
    x: i64,
    y: i64,
) {
    let overlay_rgba = overlay.to_rgba8();
    let (overlay_width, overlay_height) = overlay_rgba.dimensions();
    let (base_width, base_height) = base.dimensions();

    // Pre-compute bounds to avoid repeated checks
    let start_ox = if x < 0 { (-x) as u32 } else { 0 };
    let start_oy = if y < 0 { (-y) as u32 } else { 0 };
    let end_ox = overlay_width.min(if x < 0 { base_width } else { (base_width as i64 - x) as u32 });
    let end_oy = overlay_height.min(if y < 0 { base_height } else { (base_height as i64 - y) as u32 });

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

/// Sizing parameters for overlay ring + shadow, scaled proportionally to overlay size.
/// Approximates the look of Tailwind `ring-2 ring-indigo-400/70 shadow-lg` from the
/// ImageComposerModal preview, expressed as fractions of the overlay's shorter side.
#[derive(Debug, Clone, Copy)]
pub struct DecorationParams {
    pub ring_width: u32,
    pub shadow_offset_y: i32,
    pub shadow_blur_sigma: f32,
}

impl DecorationParams {
    pub fn from_overlay_size(width: u32, height: u32) -> Self {
        let size = width.min(height).max(1);
        let ring_width = ((size + 32) / 64).max(2);
        let shadow_offset_y = ((size / 10) as i32).max(4);
        let shadow_blur_sigma = (size as f32 / 14.0).max(4.0);
        Self {
            ring_width,
            shadow_offset_y,
            shadow_blur_sigma,
        }
    }

    /// Symmetric padding around the overlay needed to fit shadow + ring without clipping.
    pub fn padding(&self) -> u32 {
        let blur_radius = (self.shadow_blur_sigma * 2.5).ceil() as u32;
        blur_radius + self.shadow_offset_y.unsigned_abs() + self.ring_width + 2
    }
}

/// Indigo-400 at 70% opacity — matches `ring-indigo-400/70` from Tailwind.
const RING_COLOR: [u8; 3] = [129, 140, 248];
const RING_ALPHA: u8 = 178;
/// Approximation of `shadow-lg`'s composite alpha (≈15% black with soft falloff).
const SHADOW_TINT_ALPHA: u32 = 38;

/// Apply a Tailwind-style ring + drop shadow to an overlay image. Returns the
/// decorated image and the (x, y) padding added — callers should subtract this
/// from the overlay's render position so the visible image stays in place.
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

    // 2. Ring: pixels outside the overlay shape that are within ring_width of any
    //    shape pixel. Uses the overlay's alpha (threshold) as the shape mask.
    let mask: Vec<bool> = overlay_rgba
        .as_raw()
        .par_chunks(4)
        .map(|px| px[3] > 128)
        .collect();

    let rw = params.ring_width as i32;
    let rw_sq = rw * rw;
    let ow_i = ow as i32;
    let oh_i = oh as i32;
    let pad_i = pad as i32;

    let mut ring_layer = RgbaImage::new(new_w, new_h);
    let stride = new_w as usize * 4;
    let ring_dst: &mut [u8] = ring_layer.as_mut();
    ring_dst
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(cy, row)| {
            let my = cy as i32 - pad_i;
            for cx in 0..new_w {
                let mx = cx as i32 - pad_i;
                let inside = mx >= 0
                    && mx < ow_i
                    && my >= 0
                    && my < oh_i
                    && mask[(my as usize) * ow as usize + mx as usize];
                if inside {
                    continue;
                }
                let mut hit = false;
                'search: for dy in -rw..=rw {
                    let ny = my + dy;
                    if ny < 0 || ny >= oh_i {
                        continue;
                    }
                    let dy_sq = dy * dy;
                    if dy_sq > rw_sq {
                        continue;
                    }
                    let max_dx_sq = rw_sq - dy_sq;
                    for dx in -rw..=rw {
                        if dx * dx > max_dx_sq {
                            continue;
                        }
                        let nx = mx + dx;
                        if nx < 0 || nx >= ow_i {
                            continue;
                        }
                        if mask[(ny as usize) * ow as usize + nx as usize] {
                            hit = true;
                            break 'search;
                        }
                    }
                }
                if hit {
                    let i = cx as usize * 4;
                    row[i] = RING_COLOR[0];
                    row[i + 1] = RING_COLOR[1];
                    row[i + 2] = RING_COLOR[2];
                    row[i + 3] = RING_ALPHA;
                }
            }
        });

    // 3. Composite: shadow → ring → overlay. The overlay's own alpha covers the
    //    inner part of the ring, leaving only the outer 2-ish px visible — matching
    //    Tailwind's `ring-2` which sits outside the rounded-corner box.
    let mut canvas = RgbaImage::new(new_w, new_h);
    composite_rgba(&mut canvas, &shadow_blurred);
    composite_rgba(&mut canvas, &ring_layer);
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
}
