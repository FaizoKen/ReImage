use once_cell::sync::Lazy;
use std::collections::HashSet;

/// Allowed image content types
static ALLOWED_CONTENT_TYPES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("image/jpeg");
    set.insert("image/jpg");
    set.insert("image/png");
    set.insert("image/gif");
    set.insert("image/webp");
    set.insert("image/avif");
    set.insert("image/svg+xml");
    set.insert("image/bmp");
    set.insert("image/tiff");
    set
});

/// Check if content type is a valid image type
pub fn is_valid_content_type(content_type: &str) -> bool {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_lowercase();

    // Allow if in whitelist
    if ALLOWED_CONTENT_TYPES.contains(ct.as_str()) {
        return true;
    }

    // Allow any image/* type as fallback
    if ct.starts_with("image/") {
        return true;
    }

    false
}

/// Validate image buffer by checking magic bytes
/// Matches the Node.js implementation exactly
pub fn is_valid_image_buffer(buf: &[u8]) -> bool {
    if buf.len() < 4 {
        return false;
    }

    // JPEG: FF D8 FF
    if buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF {
        return true;
    }

    // PNG: 89 50 4E 47
    if buf[0] == 0x89 && buf[1] == 0x50 && buf[2] == 0x4E && buf[3] == 0x47 {
        return true;
    }

    // GIF: 47 49 46 38
    if buf[0] == 0x47 && buf[1] == 0x49 && buf[2] == 0x46 && buf[3] == 0x38 {
        return true;
    }

    // WebP: RIFF....WEBP
    if buf.len() > 11
        && buf[0] == 0x52
        && buf[1] == 0x49
        && buf[2] == 0x46
        && buf[3] == 0x46
        && buf[8] == 0x57
        && buf[9] == 0x45
        && buf[10] == 0x42
        && buf[11] == 0x50
    {
        return true;
    }

    // BMP: 42 4D
    if buf[0] == 0x42 && buf[1] == 0x4D {
        return true;
    }

    // TIFF: II or MM (49 49 or 4D 4D)
    if (buf[0] == 0x49 && buf[1] == 0x49) || (buf[0] == 0x4D && buf[1] == 0x4D) {
        return true;
    }

    // SVG: starts with < (XML)
    if buf[0] == 0x3C {
        return true;
    }

    // AVIF/HEIC: check for ftyp box
    if buf.len() > 11
        && buf[4] == 0x66
        && buf[5] == 0x74
        && buf[6] == 0x79
        && buf[7] == 0x70
    {
        return true;
    }

    false
}

/// Get image format from magic bytes
pub fn detect_format(buf: &[u8]) -> Option<&'static str> {
    if buf.len() < 4 {
        return None;
    }

    // JPEG
    if buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF {
        return Some("jpeg");
    }

    // PNG
    if buf[0] == 0x89 && buf[1] == 0x50 && buf[2] == 0x4E && buf[3] == 0x47 {
        return Some("png");
    }

    // GIF
    if buf[0] == 0x47 && buf[1] == 0x49 && buf[2] == 0x46 && buf[3] == 0x38 {
        return Some("gif");
    }

    // WebP
    if buf.len() > 11
        && buf[0] == 0x52
        && buf[1] == 0x49
        && buf[2] == 0x46
        && buf[3] == 0x46
        && buf[8] == 0x57
        && buf[9] == 0x45
        && buf[10] == 0x42
        && buf[11] == 0x50
    {
        return Some("webp");
    }

    // BMP
    if buf[0] == 0x42 && buf[1] == 0x4D {
        return Some("bmp");
    }

    // TIFF
    if (buf[0] == 0x49 && buf[1] == 0x49) || (buf[0] == 0x4D && buf[1] == 0x4D) {
        return Some("tiff");
    }

    // SVG
    if buf[0] == 0x3C {
        return Some("svg");
    }

    // AVIF
    if buf.len() > 11
        && buf[4] == 0x66
        && buf[5] == 0x74
        && buf[6] == 0x79
        && buf[7] == 0x70
    {
        return Some("avif");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_content_types() {
        assert!(is_valid_content_type("image/jpeg"));
        assert!(is_valid_content_type("image/png"));
        assert!(is_valid_content_type("image/webp"));
        assert!(is_valid_content_type("image/gif"));
        assert!(is_valid_content_type("image/jpeg; charset=utf-8"));
        assert!(is_valid_content_type("image/unknown")); // Any image/* allowed
        assert!(!is_valid_content_type("text/html"));
        assert!(!is_valid_content_type("application/json"));
    }

    #[test]
    fn test_magic_bytes_jpeg() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0];
        assert!(is_valid_image_buffer(&jpeg));
        assert_eq!(detect_format(&jpeg), Some("jpeg"));
    }

    #[test]
    fn test_magic_bytes_png() {
        let png = [0x89, 0x50, 0x4E, 0x47];
        assert!(is_valid_image_buffer(&png));
        assert_eq!(detect_format(&png), Some("png"));
    }

    #[test]
    fn test_magic_bytes_gif() {
        let gif = [0x47, 0x49, 0x46, 0x38];
        assert!(is_valid_image_buffer(&gif));
        assert_eq!(detect_format(&gif), Some("gif"));
    }

    #[test]
    fn test_invalid_buffer() {
        let invalid = [0x00, 0x00, 0x00, 0x00];
        assert!(!is_valid_image_buffer(&invalid));
        assert_eq!(detect_format(&invalid), None);
    }
}
