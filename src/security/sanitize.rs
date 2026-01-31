use once_cell::sync::Lazy;
use std::collections::HashSet;

/// Safe named colors whitelist
static SAFE_COLORS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("black");
    set.insert("white");
    set.insert("red");
    set.insert("green");
    set.insert("blue");
    set.insert("yellow");
    set.insert("orange");
    set.insert("purple");
    set.insert("pink");
    set.insert("gray");
    set.insert("grey");
    set.insert("brown");
    set.insert("cyan");
    set.insert("magenta");
    set.insert("lime");
    set.insert("navy");
    set.insert("teal");
    set.insert("maroon");
    set.insert("olive");
    set.insert("silver");
    set.insert("aqua");
    set.insert("fuchsia");
    set
});

/// Sanitize color input (hex colors or named colors only)
pub fn sanitize_color(color: Option<&str>) -> String {
    let color = match color {
        Some(c) if !c.is_empty() => c,
        _ => return "#000000".to_string(),
    };

    // Check for valid hex color
    let hex_pattern = if color.starts_with('#') {
        &color[1..]
    } else {
        color
    };

    // Valid hex formats: RGB (3), RRGGBB (6), RRGGBBAA (8)
    if matches!(hex_pattern.len(), 3 | 6 | 8)
        && hex_pattern.chars().all(|c| c.is_ascii_hexdigit())
    {
        if color.starts_with('#') {
            return color.to_string();
        } else {
            return format!("#{}", color);
        }
    }

    // Check for named color
    let lower = color.to_lowercase();
    if SAFE_COLORS.contains(lower.as_str()) {
        return lower;
    }

    "#000000".to_string()
}

/// Sanitize font family (alphanumeric, spaces, and hyphens only)
pub fn sanitize_font(font: Option<&str>) -> String {
    let font = match font {
        Some(f) if !f.is_empty() => f,
        _ => return "Arial".to_string(),
    };

    let sanitized: String = font
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .take(50)
        .collect();

    if sanitized.is_empty() {
        "Arial".to_string()
    } else {
        sanitized
    }
}

/// Sanitize text content (prevent XSS in SVG)
/// Encodes HTML entities for safe SVG embedding
pub fn sanitize_text(text: &str, max_length: usize) -> String {
    text.chars()
        .take(max_length)
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#x27;".to_string(),
            '/' => "&#x2F;".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// Safe integer parsing with bounds checking
pub fn safe_int(value: Option<i64>, min: i64, max: i64, default: Option<i64>) -> Option<i64> {
    match value {
        Some(v) => Some(v.clamp(min, max)),
        None => default,
    }
}

/// Safe unsigned integer parsing with bounds checking
pub fn safe_uint(value: Option<u32>, min: u32, max: u32, default: Option<u32>) -> Option<u32> {
    match value {
        Some(v) => Some(v.clamp(min, max)),
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_color_hex() {
        assert_eq!(sanitize_color(Some("#FF0000")), "#FF0000");
        assert_eq!(sanitize_color(Some("FF0000")), "#FF0000");
        assert_eq!(sanitize_color(Some("#F00")), "#F00");
        assert_eq!(sanitize_color(Some("#FF000080")), "#FF000080");
    }

    #[test]
    fn test_sanitize_color_named() {
        assert_eq!(sanitize_color(Some("red")), "red");
        assert_eq!(sanitize_color(Some("RED")), "red");
        assert_eq!(sanitize_color(Some("blue")), "blue");
    }

    #[test]
    fn test_sanitize_color_invalid() {
        assert_eq!(sanitize_color(Some("invalid")), "#000000");
        assert_eq!(sanitize_color(Some("")), "#000000");
        assert_eq!(sanitize_color(None), "#000000");
        assert_eq!(sanitize_color(Some("#GGGGGG")), "#000000");
    }

    #[test]
    fn test_sanitize_font() {
        assert_eq!(sanitize_font(Some("Arial")), "Arial");
        assert_eq!(sanitize_font(Some("Times New Roman")), "Times New Roman");
        assert_eq!(sanitize_font(Some("font-family")), "font-family");
        assert_eq!(sanitize_font(Some("Bad<Script>")), "BadScript");
        assert_eq!(sanitize_font(None), "Arial");
    }

    #[test]
    fn test_sanitize_text() {
        assert_eq!(sanitize_text("Hello World", 500), "Hello World");
        assert_eq!(sanitize_text("<script>", 500), "&lt;script&gt;");
        assert_eq!(sanitize_text("a&b", 500), "a&amp;b");
        assert_eq!(sanitize_text("\"quoted\"", 500), "&quot;quoted&quot;");
    }
}
