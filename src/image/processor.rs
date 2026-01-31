use bytes::Bytes;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage};
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

/// Encode image to WebP format
pub fn encode_webp(img: &DynamicImage, quality: u8) -> AppResult<Bytes> {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    // Use webp crate for encoding
    let encoder = webp::Encoder::from_rgba(&rgba, width, height);
    let webp_data = encoder.encode(quality as f32);

    Ok(Bytes::from(webp_data.to_vec()))
}

/// Create a rounded corner mask as SVG
pub fn create_rounded_mask_svg(width: u32, height: u32, radius: u32) -> String {
    format!(
        r#"<svg width="{}" height="{}"><rect x="0" y="0" width="{}" height="{}" rx="{}" ry="{}" fill="black"/></svg>"#,
        width, height, width, height, radius, radius
    )
}

/// Apply rounded corners to an image using alpha masking
pub fn apply_rounded_corners(img: &DynamicImage, radius: u32) -> AppResult<DynamicImage> {
    let (width, height) = img.dimensions();
    let mut rgba = img.to_rgba8();

    let radius = radius.min(width / 2).min(height / 2);

    // Apply rounded corners by modifying alpha channel
    for y in 0..height {
        for x in 0..width {
            let pixel = rgba.get_pixel_mut(x, y);

            // Check each corner
            let in_corner = |cx: u32, cy: u32| -> bool {
                let dx = if x < cx { cx - x } else { x - cx };
                let dy = if y < cy { cy - y } else { y - cy };
                dx * dx + dy * dy > radius * radius
            };

            let should_clip = {
                // Top-left corner
                (x < radius && y < radius && in_corner(radius, radius))
                    // Top-right corner
                    || (x >= width - radius && y < radius && in_corner(width - radius - 1, radius))
                    // Bottom-left corner
                    || (x < radius && y >= height - radius && in_corner(radius, height - radius - 1))
                    // Bottom-right corner
                    || (x >= width - radius && y >= height - radius && in_corner(width - radius - 1, height - radius - 1))
            };

            if should_clip {
                pixel[3] = 0; // Set alpha to 0
            }
        }
    }

    Ok(DynamicImage::ImageRgba8(rgba))
}

/// Composite overlay image onto base at given position
pub fn composite_overlay(
    base: &mut RgbaImage,
    overlay: &DynamicImage,
    x: i64,
    y: i64,
) {
    let overlay_rgba = overlay.to_rgba8();
    let (overlay_width, overlay_height) = overlay_rgba.dimensions();
    let (base_width, base_height) = base.dimensions();

    for oy in 0..overlay_height {
        for ox in 0..overlay_width {
            let bx = x + ox as i64;
            let by = y + oy as i64;

            // Skip if outside base image bounds
            if bx < 0 || by < 0 || bx >= base_width as i64 || by >= base_height as i64 {
                continue;
            }

            let overlay_pixel = overlay_rgba.get_pixel(ox, oy);
            let base_pixel = base.get_pixel_mut(bx as u32, by as u32);

            // Alpha blending
            let alpha = overlay_pixel[3] as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;

            base_pixel[0] = (overlay_pixel[0] as f32 * alpha + base_pixel[0] as f32 * inv_alpha) as u8;
            base_pixel[1] = (overlay_pixel[1] as f32 * alpha + base_pixel[1] as f32 * inv_alpha) as u8;
            base_pixel[2] = (overlay_pixel[2] as f32 * alpha + base_pixel[2] as f32 * inv_alpha) as u8;
            base_pixel[3] = (alpha * 255.0 + base_pixel[3] as f32 * inv_alpha).min(255.0) as u8;
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
