//! Reusable widgets.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Sparkline;
use unicode_width::UnicodeWidthStr;

use crate::adapter::types::SessionStatus;
use crate::tui::theme;

/// Left-pad `s` with spaces to reach at least `target_cols` terminal columns.
///
/// Rust's built-in `format!("{:<N}", s)` pads by char count, which produces
/// visually ragged columns whenever CJK characters are in play — `语言`
/// (2 chars, 4 cols) lines up with `English` (7 chars, 7 cols) as if they had
/// the same width when they don't. This helper uses `unicode-width` for the
/// correct East-Asian-Width-aware measurement.
///
/// If the string is already at or past the target, it's returned unchanged;
/// truncation is out of scope because deciding where to cut a grapheme
/// cluster belongs to the caller.
pub fn pad_display_width(s: &str, target_cols: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width >= target_cols {
        s.to_string()
    } else {
        let mut out = String::with_capacity(s.len() + (target_cols - width));
        out.push_str(s);
        for _ in 0..(target_cols - width) {
            out.push(' ');
        }
        out
    }
}

pub fn status_span(status: SessionStatus) -> Span<'static> {
    let (label, style) = match status {
        SessionStatus::Active => ("● active", theme::status_active()),
        SessionStatus::Idle => ("◐ idle", theme::status_idle()),
        SessionStatus::Completed => ("○ done", theme::status_done()),
        SessionStatus::Unknown => ("○ ?", theme::status_done()),
    };
    Span::styled(label, style)
}

pub fn rss_spark(history: &[u64], style: Style) -> Sparkline<'_> {
    Sparkline::default().data(history).style(style)
}

pub fn human_bytes(bytes_kb: u64) -> String {
    if bytes_kb < 1024 {
        format!("{bytes_kb} KB")
    } else if bytes_kb < 1024 * 1024 {
        format!("{:.1} MB", bytes_kb as f64 / 1024.0)
    } else {
        format!("{:.2} GB", bytes_kb as f64 / (1024.0 * 1024.0))
    }
}

pub fn token_bar_line(
    input: u64,
    cache_read: u64,
    cache_create: u64,
    output: u64,
) -> Line<'static> {
    let total = input + cache_read + cache_create + output;
    let label = format!(
        "in {input}  cache_r {cache_read}  cache_w {cache_create}  out {output}  Σ {total}"
    );
    Line::from(Span::styled(label, theme::muted()))
}

/// Inline braille-dot sparkline. Each character packs two samples (left / right
/// column) × four vertical levels, giving 2×4 the resolution of a classic
/// single-cell ASCII block sparkline in the same terminal cell count. Output
/// length is exactly `char_width` characters when there are ≥ `char_width * 2`
/// samples.
///
/// Uses a 5 %-of-peak flatness guard so sampling jitter isn't rendered as a
/// dramatic ridge.
pub fn braille_spark(values: &[u64], char_width: usize) -> String {
    if values.is_empty() || char_width == 0 {
        return String::new();
    }
    // Braille dot bit layout (Unicode U+2800-U+28FF):
    //   dot1 0x01  dot4 0x08   <- top row
    //   dot2 0x02  dot5 0x10
    //   dot3 0x04  dot6 0x20
    //   dot7 0x40  dot8 0x80   <- bottom row
    // Fills bottom-up by height (0..=4).
    const LEFT_FILL: [u32; 5] = [0x00, 0x40, 0x44, 0x46, 0x47];
    const RIGHT_FILL: [u32; 5] = [0x00, 0x80, 0xA0, 0xB0, 0xB8];
    const BRAILLE_BASE: u32 = 0x2800;

    let sample_count = char_width * 2;
    let start = values.len().saturating_sub(sample_count);
    let slice = &values[start..];
    let max = *slice.iter().max().unwrap_or(&0);
    let min = *slice.iter().min().unwrap_or(&0);
    let spread = max.saturating_sub(min);
    let flat = max == 0 || spread.saturating_mul(20) < max;

    let height = |v: u64| -> usize {
        if flat {
            return 1;
        }
        let norm = v.saturating_sub(min) as f64 / spread as f64;
        (norm * 4.0).round().clamp(0.0, 4.0) as usize
    };

    let mut out = String::with_capacity(char_width * 3);
    for i in 0..char_width {
        let left = slice.get(i * 2).copied().map(height).unwrap_or(0);
        let right = slice.get(i * 2 + 1).copied().map(height).unwrap_or(0);
        let code = BRAILLE_BASE | LEFT_FILL[left] | RIGHT_FILL[right];
        out.push(char::from_u32(code).unwrap_or(' '));
    }
    out
}

/// Directional trend arrow based on comparing the tail window's mean to the
/// head window's mean. Uses a 5 % deadband so micro-jitter renders as a
/// horizontal arrow rather than flipping between up/down each sample.
pub fn trend_arrow(values: &[u64]) -> &'static str {
    if values.len() < 4 {
        return "→";
    }
    let window = (values.len() / 4).max(1);
    let head: f64 = values[..window].iter().map(|&v| v as f64).sum::<f64>() / window as f64;
    let tail: f64 = values[values.len() - window..]
        .iter()
        .map(|&v| v as f64)
        .sum::<f64>()
        / window as f64;
    if head == 0.0 {
        return if tail > 0.0 { "↗" } else { "→" };
    }
    let delta = (tail - head) / head;
    if delta > 0.05 {
        "↗"
    } else if delta < -0.05 {
        "↘"
    } else {
        "→"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_spark_empty_and_zero_width() {
        assert_eq!(braille_spark(&[], 10), "");
        assert_eq!(braille_spark(&[1, 2, 3], 0), "");
    }

    #[test]
    fn braille_spark_all_zero_renders_blank_cells() {
        // All zeros ⇒ flat guard ⇒ height=1 per column ⇒ bottom row only.
        let s = braille_spark(&[0, 0, 0, 0], 2);
        assert_eq!(s.chars().count(), 2);
        // Bottom-row-only pattern is U+28C0 (⣀).
        for ch in s.chars() {
            assert_eq!(ch, '\u{28C0}');
        }
    }

    #[test]
    fn braille_spark_flat_within_noise_band() {
        // spread / max = 1 / 100 < 5 %, so the flatness guard fires.
        let v = vec![100u64; 4];
        let s = braille_spark(&v, 2);
        assert!(s.chars().all(|c| c == '\u{28C0}'));
    }

    #[test]
    fn braille_spark_full_range_hits_extremes() {
        // min maps to 0 dots, max maps to a fully-filled column.
        let s = braille_spark(&[0, 100], 1);
        let ch = s.chars().next().unwrap();
        // Left column h=0 (empty), right column h=4 (all four dots: 0x88+0x20+0x10+0x08 = 0xB8).
        assert_eq!(ch as u32, 0x2800 | 0xB8);
    }

    #[test]
    fn braille_spark_length_respects_char_width() {
        let v: Vec<u64> = (0..40).collect();
        assert_eq!(braille_spark(&v, 10).chars().count(), 10);
        assert_eq!(braille_spark(&v, 20).chars().count(), 20);
    }

    #[test]
    fn trend_arrow_flat_and_rising_and_falling() {
        let flat = vec![100u64; 16];
        assert_eq!(trend_arrow(&flat), "→");

        let rising: Vec<u64> = (0..16).map(|i| 100 + i * 10).collect();
        assert_eq!(trend_arrow(&rising), "↗");

        let falling: Vec<u64> = (0..16).map(|i| 500 - i * 10).collect();
        assert_eq!(trend_arrow(&falling), "↘");
    }

    #[test]
    fn trend_arrow_short_series_defaults_to_flat() {
        assert_eq!(trend_arrow(&[]), "→");
        assert_eq!(trend_arrow(&[1, 2, 3]), "→");
    }

    #[test]
    fn trend_arrow_zero_head_with_growth_is_rising() {
        let v = vec![0, 0, 0, 0, 50, 100, 150, 200];
        assert_eq!(trend_arrow(&v), "↗");
    }

    #[test]
    fn pad_display_width_handles_cjk_and_ascii() {
        // ASCII: straightforward, padding equals column count.
        assert_eq!(pad_display_width("Language", 12), "Language    ");
        // CJK: each char is 2 cols, so "语言" is 4 cols wide.
        let padded = pad_display_width("语言", 12);
        assert_eq!(UnicodeWidthStr::width(padded.as_str()), 12);
        // Mixed: "Token 显示" = 5 + 1 + 4 = 10 cols; pad to 16.
        let padded = pad_display_width("Token 显示", 16);
        assert_eq!(UnicodeWidthStr::width(padded.as_str()), 16);
    }

    #[test]
    fn pad_display_width_already_wide_enough_returns_unchanged() {
        // Over-width input: we refuse to truncate, just hand it back.
        let s = "very long label that exceeds target";
        assert_eq!(pad_display_width(s, 5), s);
    }
}
