use bytes::Bytes;
use resvg::usvg::fontdb;
use std::fmt::Write as _;

use crate::image::fonts::FONTS;
use crate::security::sanitize::{sanitize_color, sanitize_font, sanitize_text};

/// Text overlay configuration.
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
    /// CSS font-weight (100..=900). Defaults to 700 (bold) for backwards
    /// compatibility — the feature historically forced bold.
    pub weight: u16,
    /// Optional outline/halo color drawn behind the fill for legibility over
    /// busy images. `None` disables the outline.
    pub outline_color: Option<String>,
    /// Outline stroke width in px. `None` (with an outline color set) auto-scales
    /// from the font size.
    pub outline_width: Option<u32>,
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
            weight: 700,
            outline_color: None,
            outline_width: None,
        }
    }
}

/// Line advance for a given font size. 1.2× is the conventional default leading.
fn line_height(font_size: u32) -> f64 {
    font_size as f64 * 1.2
}

// ---------------------------------------------------------------------------
// Text measurement
// ---------------------------------------------------------------------------

/// Abstracts "how wide is this text". Backed by real font metrics when a face
/// is available, and a proportional per-character heuristic otherwise.
trait Measure {
    /// Advance width of a single character at `font_size`, in pixels.
    fn char_width(&self, ch: char, font_size: f64) -> f64;

    /// Width of a whole string (sum of advances). Kerning/ligatures are ignored;
    /// for Latin text these only ever *reduce* the rendered width, so a layout
    /// that fits by this measure will not overflow once shaped.
    fn text_width(&self, s: &str, font_size: f64) -> f64 {
        s.chars().map(|c| self.char_width(c, font_size)).sum()
    }
}

/// Metric-accurate measurer reading glyph advances straight from the font.
struct FontMeasure<'f, 'd> {
    face: &'f ttf_parser::Face<'d>,
    units_per_em: f64,
}

impl Measure for FontMeasure<'_, '_> {
    fn char_width(&self, ch: char, font_size: f64) -> f64 {
        let advance = self
            .face
            .glyph_index(ch)
            .and_then(|g| self.face.glyph_hor_advance(g))
            .map(|a| a as f64)
            // Missing glyph: assume a half-em box (typical fallback width).
            .unwrap_or(self.units_per_em * 0.5);
        advance / self.units_per_em * font_size
    }
}

/// Fallback measurer used when no font face can be resolved. Approximates
/// proportional widths far better than a flat factor, and treats East-Asian /
/// emoji codepoints as full-width.
struct HeuristicMeasure;

impl Measure for HeuristicMeasure {
    fn char_width(&self, ch: char, font_size: f64) -> f64 {
        font_size * heuristic_ratio(ch)
    }
}

fn heuristic_ratio(ch: char) -> f64 {
    match ch {
        ' ' | 'i' | 'j' | 'l' | '!' | '.' | ',' | '\'' | '|' | '`' | ';' | ':' => 0.28,
        'I' | 'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '{' | '}' | '/' | '\\' => 0.34,
        'm' | 'M' | 'W' | 'w' => 0.85,
        'A'..='Z' => 0.68,
        '0'..='9' => 0.55,
        c if is_wide(c) => 1.0,
        _ => 0.5,
    }
}

/// Rough East-Asian-wide / emoji detection for the heuristic fallback.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E  // CJK radicals, Kangxi, punctuation
        | 0x3041..=0x33FF  // Hiragana, Katakana, CJK symbols
        | 0x3400..=0x4DBF  // CJK Extension A
        | 0x4E00..=0x9FFF  // CJK Unified Ideographs
        | 0xA000..=0xA4CF  // Yi
        | 0xAC00..=0xD7A3  // Hangul Syllables
        | 0xF900..=0xFAFF  // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F  // CJK Compatibility Forms
        | 0xFF00..=0xFF60  // Fullwidth Forms
        | 0xFFE0..=0xFFE6  // Fullwidth signs
        | 0x1F300..=0x1FAFF // Emoji & pictographs
        | 0x20000..=0x3FFFD // CJK Extension B+
    )
}

// ---------------------------------------------------------------------------
// Wrapping
// ---------------------------------------------------------------------------

/// Wrap text to fit within optional width/height constraints using a proportional
/// heuristic. Kept public for tests and as the metric-free fallback; the rendered
/// path uses real font metrics via [`layout_lines`].
///
/// `\n` characters force hard line breaks. When `max_width` is set, lines are
/// greedily word-wrapped (long words are broken across lines). When `max_height`
/// is set, the result is capped to the number of fitting lines and the last line
/// is truncated with an ellipsis.
pub fn wrap_text(
    text: &str,
    font_size: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Vec<String> {
    wrap_lines(
        text,
        font_size,
        max_width,
        max_height,
        line_height(font_size),
        &HeuristicMeasure,
    )
}

/// Lay out `text` into lines using the real metrics of the resolved font face,
/// falling back to the heuristic when no face is available.
fn layout_lines(
    text: &str,
    font_size: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
    line_height: f64,
    weight: u16,
    family: &str,
) -> Vec<String> {
    let from_font = FONTS
        .resolve_face(family, fontdb::Weight(weight))
        .and_then(|id| {
            FONTS
                .db
                .with_face_data(id, |data, index| {
                    ttf_parser::Face::parse(data, index).ok().map(|face| {
                        let upem = face.units_per_em();
                        let measure = FontMeasure {
                            face: &face,
                            units_per_em: if upem == 0 { 1000.0 } else { upem as f64 },
                        };
                        wrap_lines(
                            text,
                            font_size,
                            max_width,
                            max_height,
                            line_height,
                            &measure,
                        )
                    })
                })
                .flatten()
        });

    // No usable face (empty font db): fall back to the proportional heuristic.
    // `wrap_text` recomputes the same `line_height(font_size)` passed above.
    from_font.unwrap_or_else(|| wrap_text(text, font_size, max_width, max_height))
}

fn wrap_lines<M: Measure>(
    text: &str,
    font_size: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
    line_height: f64,
    m: &M,
) -> Vec<String> {
    let fs = font_size as f64;
    let max_w = max_width.filter(|&w| w > 0).map(|w| w as f64);
    let max_lines = max_height
        .filter(|&h| h > 0)
        .map(|h| ((h as f64 / line_height).floor() as usize).max(1))
        .unwrap_or(usize::MAX);

    let mut lines: Vec<String> = Vec::new();

    for paragraph in text.split('\n') {
        match max_w {
            // No width constraint: each explicit paragraph is a single line.
            None => lines.push(paragraph.to_string()),
            Some(mw) => wrap_paragraph(paragraph, mw, fs, m, &mut lines),
        }

        // Stop early once we clearly have more than will fit vertically; avoids
        // building enormous line vectors for tall/narrow boxes.
        if max_lines != usize::MAX && lines.len() > max_lines {
            break;
        }
    }

    // Vertical fit: cap to the number of lines that fit and ellipsize the last.
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(last, max_w, fs, m);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Greedy word-wrap of a single paragraph, appending resulting lines to `out`.
fn wrap_paragraph<M: Measure>(paragraph: &str, max_w: f64, fs: f64, m: &M, out: &mut Vec<String>) {
    let space_w = m.char_width(' ', fs);
    let mut cur = String::new();
    let mut cur_w = 0.0;
    let mut saw_word = false;

    for word in paragraph.split_whitespace() {
        saw_word = true;
        let word_w = m.text_width(word, fs);

        if word_w > max_w && word.chars().count() > 1 {
            // Word can't fit on a line by itself — flush, then hard-break it.
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let mut chunks = break_word(word, max_w, fs, m);
            let last = chunks.pop().unwrap_or_default();
            out.extend(chunks);
            cur = last;
            cur_w = m.text_width(&cur, fs);
        } else if cur.is_empty() {
            cur = word.to_string();
            cur_w = word_w;
        } else if cur_w + space_w + word_w <= max_w {
            cur.push(' ');
            cur.push_str(word);
            cur_w += space_w + word_w;
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
            cur_w = word_w;
        }
    }

    // Push the trailing line, or a blank line for an empty paragraph so that
    // intentional blank lines (e.g. "a\n\nb") preserve their vertical gap.
    if !cur.is_empty() || !saw_word {
        out.push(cur);
    }
}

/// Break a single over-long word into width-bounded chunks (at least one char
/// each, so progress is always made even when a glyph alone exceeds the width).
fn break_word<M: Measure>(word: &str, max_w: f64, fs: f64, m: &M) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0.0;

    for ch in word.chars() {
        let cw = m.char_width(ch, fs);
        if !cur.is_empty() && cur_w + cw > max_w {
            chunks.push(std::mem::take(&mut cur));
            cur_w = 0.0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Truncate `line` so that `line + …` fits within `max_w` (or just append the
/// ellipsis when no width constraint applies).
fn ellipsize<M: Measure>(line: &str, max_w: Option<f64>, fs: f64, m: &M) -> String {
    const ELLIPSIS: char = '…';

    let mut chars: Vec<char> = line.chars().collect();

    if let Some(mw) = max_w {
        let ew = m.char_width(ELLIPSIS, fs);
        while !chars.is_empty() {
            let w: f64 = chars.iter().map(|&c| m.char_width(c, fs)).sum::<f64>() + ew;
            if w <= mw {
                break;
            }
            chars.pop();
        }
    }

    // Trim trailing whitespace so the ellipsis sits flush against the text.
    while chars.last().is_some_and(|c| c.is_whitespace()) {
        chars.pop();
    }

    let mut s: String = chars.into_iter().collect();
    s.push(ELLIPSIS);
    s
}

// ---------------------------------------------------------------------------
// SVG generation
// ---------------------------------------------------------------------------

/// Map text alignment to SVG `text-anchor`.
fn alignment_to_text_anchor(align: &str) -> &'static str {
    match align {
        "center" => "middle",
        "right" => "end",
        _ => "start", // "left" or default
    }
}

/// Generate the SVG document for a set of text overlays. The document matches
/// the target image dimensions so it composites 1:1 over the base raster.
pub fn generate_text_svg(
    texts: &[TextConfig],
    width: u32,
    height: u32,
    max_text_length: usize,
) -> Bytes {
    // ~256 bytes per text element (two <text> tags when outlined).
    let mut elements = String::with_capacity(texts.len() * 256);
    let default_family = FONTS.default_family.as_str();

    for config in texts {
        if config.text.is_empty() {
            continue;
        }

        // Decode `+` to space (form-encoding convention) and cap total length.
        let decoded: String = config
            .text
            .replace('+', " ")
            .chars()
            .take(max_text_length)
            .collect();

        let color = sanitize_color(Some(&config.color));
        let family = sanitize_font(Some(&config.font_family));
        let anchor = alignment_to_text_anchor(&config.align);
        let lh = line_height(config.font_size);
        let weight = config.weight;

        // Resolve the optional outline once per text block.
        let outline = config.outline_color.as_deref().map(|c| {
            let oc = sanitize_color(Some(c));
            let ow = config
                .outline_width
                .map(|w| w as f64)
                .unwrap_or_else(|| (config.font_size as f64 * 0.08).max(1.0));
            (oc, ow)
        });

        let lines = layout_lines(
            &decoded,
            config.font_size,
            config.max_width,
            config.max_height,
            lh,
            weight,
            &family,
        );

        for (index, line) in lines.iter().enumerate() {
            let safe_text = sanitize_text(line, max_text_length);
            if safe_text.is_empty() {
                continue; // Blank line: nothing to draw, but it still advanced y.
            }

            let line_y = config.y + config.font_size as i64 + (index as f64 * lh) as i64;

            // Outline first (drawn behind the fill via a separate stroke-only
            // element — robust across renderers regardless of paint-order
            // support). Rounded joins/caps keep the halo smooth.
            if let Some((oc, ow)) = &outline {
                let _ = write!(
                    elements,
                    r#"<text x="{x}" y="{y}" font-family="'{fam}','{def}',sans-serif" font-size="{fs}" font-weight="{w}" text-anchor="{a}" fill="none" stroke="{oc}" stroke-width="{ow}" stroke-linejoin="round" stroke-linecap="round" xml:space="preserve">{t}</text>"#,
                    x = config.x,
                    y = line_y,
                    fam = family,
                    def = default_family,
                    fs = config.font_size,
                    w = weight,
                    a = anchor,
                    oc = oc,
                    ow = ow,
                    t = safe_text,
                );
            }

            let _ = write!(
                elements,
                r#"<text x="{x}" y="{y}" font-family="'{fam}','{def}',sans-serif" font-size="{fs}" font-weight="{w}" fill="{fill}" text-anchor="{a}" xml:space="preserve">{t}</text>"#,
                x = config.x,
                y = line_y,
                fam = family,
                def = default_family,
                fs = config.font_size,
                w = weight,
                fill = color,
                a = anchor,
                t = safe_text,
            );
        }
    }

    let mut svg = String::with_capacity(96 + elements.len());
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
    fn no_constraints_keeps_single_line() {
        assert_eq!(
            wrap_text("Hello World", 24, None, None),
            vec!["Hello World"]
        );
    }

    #[test]
    fn explicit_newlines_force_breaks() {
        assert_eq!(wrap_text("a\nb\nc", 24, None, None), vec!["a", "b", "c"]);
    }

    #[test]
    fn blank_paragraph_preserved() {
        // "a\n\nb" keeps the empty middle line so the vertical gap survives.
        assert_eq!(wrap_text("a\n\nb", 24, None, None), vec!["a", "", "b"]);
    }

    #[test]
    fn width_constraint_wraps_to_multiple_lines() {
        let lines = wrap_text("Hello World Test", 10, Some(60), None);
        assert!(lines.len() > 1, "expected wrapping, got {lines:?}");
    }

    #[test]
    fn height_constraint_caps_lines_with_ellipsis() {
        // line_height = 12, max_height = 12 → exactly one line, ellipsized.
        let lines = wrap_text(
            "Hello World This is a long sentence",
            10,
            Some(60),
            Some(12),
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with('…'), "got {:?}", lines[0]);
    }

    #[test]
    fn long_unbroken_word_is_hard_broken() {
        // A single word longer than the line must still be split.
        let lines = wrap_text("supercalifragilisticexpialidocious", 12, Some(40), None);
        assert!(lines.len() > 1, "expected hard break, got {lines:?}");
    }

    #[test]
    fn heuristic_widths_are_proportional() {
        let m = HeuristicMeasure;
        // 'W' is wider than 'i' at the same size.
        assert!(m.char_width('W', 100.0) > m.char_width('i', 100.0));
    }

    #[test]
    fn ellipsize_without_width_just_appends() {
        let m = HeuristicMeasure;
        assert_eq!(ellipsize("hello ", None, 12.0, &m), "hello…");
    }

    #[test]
    fn alignment_mapping() {
        assert_eq!(alignment_to_text_anchor("left"), "start");
        assert_eq!(alignment_to_text_anchor("center"), "middle");
        assert_eq!(alignment_to_text_anchor("right"), "end");
        assert_eq!(alignment_to_text_anchor("bogus"), "start");
    }

    #[test]
    fn outline_emits_stroke_behind_fill() {
        let cfg = TextConfig {
            text: "Hi".into(),
            font_size: 20,
            color: "#ffffff".into(),
            outline_color: Some("#000000".into()),
            ..Default::default()
        };
        let svg = String::from_utf8(generate_text_svg(&[cfg], 100, 50, 500).to_vec()).unwrap();
        // Two <text> elements: the stroke-only outline then the fill.
        assert_eq!(svg.matches("<text").count(), 2);
        let stroke_at = svg.find("stroke=\"#000000\"").expect("stroke present");
        let fill_at = svg.find("fill=\"#ffffff\"").expect("fill present");
        assert!(stroke_at < fill_at, "outline must be drawn before the fill");
    }

    #[test]
    fn no_outline_emits_single_element() {
        let cfg = TextConfig {
            text: "Hi".into(),
            font_size: 20,
            color: "#ffffff".into(),
            ..Default::default()
        };
        let svg = String::from_utf8(generate_text_svg(&[cfg], 100, 50, 500).to_vec()).unwrap();
        assert_eq!(svg.matches("<text").count(), 1);
        assert!(!svg.contains("stroke="));
    }

    #[test]
    fn plus_is_decoded_to_space() {
        let cfg = TextConfig {
            text: "hello+world".into(),
            font_size: 20,
            ..Default::default()
        };
        let svg = String::from_utf8(generate_text_svg(&[cfg], 200, 50, 500).to_vec()).unwrap();
        assert!(svg.contains("hello world"));
    }
}
