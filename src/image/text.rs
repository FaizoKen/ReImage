use bytes::Bytes;

use crate::security::sanitize::{sanitize_color, sanitize_font, sanitize_text};

/// Text overlay configuration
#[derive(Debug, Clone)]
pub struct TextConfig {
    pub text: String,
    pub x: i64,
    pub y: i64,
    pub font_size: u32,
    pub color: String,
    pub font_family: String,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub align: String,
}

impl Default for TextConfig {
    fn default() -> Self {
        Self {
            text: String::new(),
            x: 0,
            y: 0,
            font_size: 24,
            color: "#000000".to_string(),
            font_family: "Arial".to_string(),
            max_width: None,
            max_height: None,
            align: "left".to_string(),
        }
    }
}

/// Estimate character width based on font size
/// Uses the same estimation as Node.js: fontSize * 0.6
fn estimate_char_width(font_size: u32) -> f64 {
    font_size as f64 * 0.6
}

/// Calculate line height
/// Uses the same calculation as Node.js: fontSize * 1.2
fn line_height(font_size: u32) -> f64 {
    font_size as f64 * 1.2
}

/// Wrap text to fit within max width/height constraints
/// Matches the Node.js implementation exactly
pub fn wrap_text(
    text: &str,
    font_size: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Vec<String> {
    if max_width.is_none() && max_height.is_none() {
        return vec![text.to_string()];
    }

    let char_width = estimate_char_width(font_size);
    let lh = line_height(font_size);

    let max_chars_per_line = max_width
        .map(|w| (w as f64 / char_width).floor() as usize)
        .unwrap_or(usize::MAX);

    let max_lines = max_height
        .map(|h| (h as f64 / lh).floor() as usize)
        .unwrap_or(usize::MAX);

    if max_chars_per_line == 0 || max_lines == 0 {
        return vec!["..".to_string()];
    }

    let mut lines = Vec::new();
    let mut remaining = text.to_string();

    while !remaining.is_empty() && lines.len() < max_lines {
        let is_last_allowed_line = lines.len() == max_lines - 1;

        if remaining.len() <= max_chars_per_line {
            lines.push(remaining);
            remaining = String::new();
        } else if is_last_allowed_line {
            // Last line - truncate with ellipsis
            let truncate_at = max_chars_per_line.saturating_sub(2);
            let truncated: String = remaining.chars().take(truncate_at).collect();
            lines.push(format!("{}..", truncated));
            remaining = String::new();
        } else {
            // Find a good break point (prefer word boundary)
            let mut break_at = max_chars_per_line;

            // Look for last space within allowed range
            let chars: Vec<char> = remaining.chars().take(max_chars_per_line).collect();
            let search_str: String = chars.iter().collect();

            if let Some(last_space) = search_str.rfind(' ') {
                // Use word boundary if it's at least 40% through the line
                if last_space > (max_chars_per_line * 4) / 10 {
                    break_at = last_space;
                }
            }

            let (line, rest) = remaining.split_at(
                remaining
                    .char_indices()
                    .nth(break_at)
                    .map(|(i, _)| i)
                    .unwrap_or(remaining.len()),
            );

            lines.push(line.trim_end().to_string());
            remaining = rest.trim_start().to_string();
        }
    }

    // If there's still remaining text after max lines, ensure ellipsis on last line
    if !remaining.is_empty() && !lines.is_empty() {
        let last_idx = lines.len() - 1;
        if !lines[last_idx].ends_with("..") {
            let truncate_at = max_chars_per_line.saturating_sub(2);
            let truncated: String = lines[last_idx].chars().take(truncate_at).collect();
            lines[last_idx] = format!("{}..", truncated);
        }
    }

    if lines.is_empty() {
        vec!["".to_string()]
    } else {
        lines
    }
}

/// Map text alignment to SVG text-anchor
fn alignment_to_text_anchor(align: &str) -> &'static str {
    match align {
        "center" => "middle",
        "right" => "end",
        _ => "start", // "left" or default
    }
}

/// Generate SVG for text overlay (optimized with capacity hints)
pub fn generate_text_svg(texts: &[TextConfig], width: u32, height: u32, max_text_length: usize) -> Bytes {
    // Pre-allocate with estimated capacity (reduces reallocations)
    // Average text element is ~200 bytes
    let mut elements = String::with_capacity(texts.len() * 200);

    for config in texts {
        if config.text.is_empty() {
            continue;
        }

        // Decode + to space (matching Node.js behavior)
        let decoded_text = config.text.replace('+', " ");

        // Sanitize inputs
        let color = sanitize_color(Some(&config.color));
        let font_family = sanitize_font(Some(&config.font_family));
        let text_anchor = alignment_to_text_anchor(&config.align);

        // Wrap text
        let lines = wrap_text(
            &decoded_text,
            config.font_size,
            config.max_width,
            config.max_height,
        );

        let lh = line_height(config.font_size);

        for (line_index, line) in lines.iter().enumerate() {
            // Sanitize text for SVG (XSS prevention)
            let safe_text = sanitize_text(line, max_text_length);
            let line_y = config.y + config.font_size as i64 + (line_index as f64 * lh) as i64;

            // Use write! macro with fmt::Write trait for zero-allocation formatting
            use std::fmt::Write;
            let _ = write!(
                elements,
                r#"<text x="{}" y="{}" font-family="'{}'" font-size="{}" fill="{}" text-anchor="{}" xml:space="preserve" style="font-weight: bold;">{}</text>"#,
                config.x, line_y, font_family, config.font_size, color, text_anchor, safe_text
            );
        }
    }

    // Pre-calculate SVG wrapper size
    let svg_capacity = 80 + elements.len(); // SVG wrapper is ~80 bytes
    let mut svg = String::with_capacity(svg_capacity);
    use std::fmt::Write;
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}">{}</svg>"#,
        width, height, elements
    );

    Bytes::from(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text_no_constraints() {
        let lines = wrap_text("Hello World", 24, None, None);
        assert_eq!(lines, vec!["Hello World"]);
    }

    #[test]
    fn test_wrap_text_with_width() {
        // Font size 10, char width = 6, max width 60 = 10 chars per line
        let lines = wrap_text("Hello World Test", 10, Some(60), None);
        assert!(lines.len() > 1);
    }

    #[test]
    fn test_wrap_text_with_height() {
        // Font size 10, line height = 12, max height 12 = 1 line
        let lines = wrap_text("Hello World This is a long text", 10, Some(60), Some(12));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with(".."));
    }

    #[test]
    fn test_alignment_mapping() {
        assert_eq!(alignment_to_text_anchor("left"), "start");
        assert_eq!(alignment_to_text_anchor("center"), "middle");
        assert_eq!(alignment_to_text_anchor("right"), "end");
    }
}
