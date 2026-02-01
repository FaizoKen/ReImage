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
