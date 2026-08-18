use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Wraps `text` to fit within `max_width` display columns.
///
/// Explicit newlines in `text` are preserved as hard line breaks.
#[must_use]
pub fn wrap_text(
    text: &str,
    max_width: usize,
) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_owned()];
    }

    let mut lines = Vec::new();
    for segment in text.split('\n') {
        lines.extend(wrap_segment(segment, max_width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Like [`wrap_text`], but the first row begins with `prefix` and continuation
/// rows are indented to the prefix display width.
#[must_use]
pub fn wrap_prefixed(
    text: &str,
    prefix: &str,
    max_width: usize,
) -> Vec<String> {
    if max_width == 0 {
        return vec![format!("{prefix}{text}")];
    }

    let line_width = max_width.saturating_sub(prefix.width());
    let indent = " ".repeat(prefix.width());
    let mut display = Vec::new();

    for (segment_idx, segment) in text.split('\n').enumerate() {
        let ranges = segment_ranges(segment, line_width);
        let chars: Vec<char> = segment.chars().collect();
        if ranges.is_empty() {
            if segment_idx == 0 && display.is_empty() {
                display.push(prefix.to_string());
            } else {
                display.push(String::new());
            }
            continue;
        }
        for (line_idx, (start, end)) in ranges.into_iter().enumerate() {
            let body: String = chars[start..end].iter().collect();
            if segment_idx == 0 && line_idx == 0 {
                display.push(format!("{prefix}{body}"));
            } else {
                display.push(format!("{indent}{body}"));
            }
        }
    }

    if display.is_empty() {
        display.push(prefix.to_string());
    }
    display
}

/// Char-index ranges `(start, end)` for one segment without embedded newlines.
#[must_use]
pub fn segment_ranges(
    segment: &str,
    max_width: usize,
) -> Vec<(usize, usize)> {
    if max_width == 0 {
        return vec![(0, segment.chars().count())];
    }

    let chars: Vec<char> = segment.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = line_end(&chars, start, max_width);
        let next = end.max(start + 1).min(chars.len());
        ranges.push((start, next));
        start = next;
    }
    ranges
}

fn wrap_segment(
    segment: &str,
    max_width: usize,
) -> Vec<String> {
    let chars: Vec<char> = segment.chars().collect();
    segment_ranges(segment, max_width)
        .into_iter()
        .map(|(start, end)| chars[start..end].iter().collect())
        .collect()
}

fn line_end(
    chars: &[char],
    start: usize,
    max_width: usize,
) -> usize {
    if max_width == 0 {
        return start;
    }

    let mut width = 0usize;
    let mut last_space = None;
    let mut i = start;

    while i < chars.len() {
        let ch_width = chars[i].width().unwrap_or(0);
        if ch_width > max_width {
            return if width == 0 { i + 1 } else { i };
        }
        if width + ch_width > max_width {
            if let Some(sp) = last_space.filter(|&sp| sp >= start) {
                return sp + 1;
            }
            return if width == 0 { i + 1 } else { i };
        }
        width += ch_width;
        if chars[i] == ' ' {
            last_space = Some(i);
        }
        i += 1;
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_empty() {
        assert_eq!(wrap_text("", 20), vec![""]);
    }

    #[test]
    fn wrap_text_word_wrap() {
        assert_eq!(
            wrap_text("hello world", 6),
            vec!["hello ".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn wrap_text_preserves_newlines() {
        assert_eq!(
            wrap_text("a\nbcde", 3),
            vec!["a".to_string(), "bcd".to_string(), "e".to_string()]
        );
    }

    #[test]
    fn wrap_prefixed_first_line_only() {
        assert_eq!(
            wrap_prefixed("hello world", "> ", 8),
            vec!["> hello ".to_string(), "  world".to_string()]
        );
    }
}
