use crate::segment_ranges;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Widget},
};
use sui_theme::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Minimum block height (top + bottom borders + one content row).
pub const PROMPT_MIN_HEIGHT: u16 = 3;

/// A text-input prompt widget with a prefix, wrapped input, and cursor tracking.
///
/// The widget renders a bordered box containing the prompt prefix followed by
/// the current input text.  When the input text is wider than the available
/// space it wraps onto additional rows; continuation lines are indented to
/// align with the text after the prefix on the first row.
///
/// Display widths are computed via [`unicode_width`] so that full-width
/// characters (CJK, emoji, etc.) contribute 2 columns each.  This keeps the
/// cursor correctly positioned even when the input contains Japanese, Chinese,
/// or Korean text.
///
/// After rendering the caller should position the terminal cursor by calling
/// [`screen_cursor`](PromptWidget::screen_cursor) with the same [`Rect`] that
/// was used for rendering.
///
/// # Example
///
/// ```ignore
/// let prompt = PromptWidget::new(input, cursor_pos, "❯ ");
/// let cursor_pos = prompt.screen_cursor(area);
/// frame.render_widget(prompt, area);
/// frame.set_cursor_position(cursor_pos);
/// ```
///
/// Note: call `screen_cursor` *before* `render_widget` because [`Widget::render`]
/// consumes `self`.
pub struct PromptWidget<'a> {
    block: Block<'a>,
    input: &'a str,
    /// Char-based position of the cursor within `input`.
    cursor_position: usize,
    prompt_prefix: &'a str,
    /// Border title (e.g. `" prompt "` or `" shell "`).
    title: &'a str,
}

impl<'a> PromptWidget<'a> {
    /// Creates a new prompt widget.
    ///
    /// * `input` — the current input text.
    /// * `cursor_position` — char-based cursor index into `input`.
    /// * `prompt_prefix` — prefix displayed before the input (e.g. `"❯ "`).
    ///
    /// The border title defaults to `" prompt "`. Override with [`Self::with_title`].
    #[must_use]
    pub fn new(
        input: &'a str,
        cursor_position: usize,
        prompt_prefix: &'a str,
    ) -> Self {
        Self {
            block: Block::default()
                .borders(Borders::ALL)
                .style(Theme::DEFAULT.prompt_style()),
            input,
            cursor_position,
            prompt_prefix,
            title: " prompt ",
        }
    }

    /// Sets the border title (e.g. `" shell "` for shell mode).
    #[must_use]
    pub const fn with_title(
        mut self,
        title: &'a str,
    ) -> Self {
        self.title = title;
        self
    }

    /// Sets the border style (e.g. foreground color per interaction mode).
    #[must_use]
    pub fn with_style(
        mut self,
        style: Style,
    ) -> Self {
        self.block = self.block.style(style);
        self
    }

    /// Block height (borders + wrapped content rows) for the given terminal width.
    #[must_use]
    pub fn block_height(
        input: &str,
        prefix: &str,
        area_width: u16,
    ) -> u16 {
        let inner_width = area_width.saturating_sub(2) as usize;
        let content_lines = wrapped_lines(input, prefix, inner_width).len().max(1);
        u16::try_from(content_lines)
            .map_or(u16::MAX, |lines| lines.saturating_add(2))
            .max(PROMPT_MIN_HEIGHT)
    }

    /// Returns the (x, y) terminal coordinates where the cursor should be
    /// placed so that it sits immediately after the visible portion of input up
    /// to `cursor_position`.
    ///
    /// `area` must be the same [`Rect`] that will be (or was) passed to
    /// [`Widget::render`].
    #[must_use]
    pub fn screen_cursor(
        &self,
        area: Rect,
    ) -> (u16, u16) {
        let inner_width = area.width.saturating_sub(2) as usize;
        let lines = wrapped_lines(self.input, self.prompt_prefix, inner_width);
        let cursor_pos = self.cursor_position.min(self.input.chars().count());
        let prefix_width = self.prompt_prefix.width();

        let (line_idx, line_start) = lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (start, end))| cursor_pos >= *start && cursor_pos <= *end)
            .map_or((0, 0), |(idx, (start, _))| (idx, *start));

        let indent = prefix_width;

        let input_chars: Vec<char> = self.input.chars().collect();
        let cursor_display_offset: usize = input_chars[line_start..cursor_pos]
            .iter()
            .map(|c| c.width().unwrap_or(0))
            .sum();

        let inner_x = 1usize + indent + cursor_display_offset;
        let x = area.x + u16::try_from(inner_x).unwrap_or(u16::MAX);
        let max_x = area.x + area.width.saturating_sub(2);
        let y = area.y + 1 + u16::try_from(line_idx).unwrap_or(u16::MAX);
        (x.min(max_x), y)
    }
}

impl Widget for PromptWidget<'_> {
    fn render(
        self,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let inner_width = area.width.saturating_sub(2) as usize;
        let lines = wrapped_lines(self.input, self.prompt_prefix, inner_width);
        let prefix_width = self.prompt_prefix.width();
        let indent = " ".repeat(prefix_width);

        let input_chars: Vec<char> = self.input.chars().collect();
        let display_lines: Vec<Line<'_>> = lines
            .iter()
            .enumerate()
            .map(|(idx, (start, end))| {
                let segment: String = input_chars[*start..*end].iter().collect();
                if idx == 0 {
                    Line::from(format!("{}{segment}", self.prompt_prefix))
                } else {
                    Line::from(format!("{indent}{segment}"))
                }
            })
            .collect();

        let paragraph =
            Paragraph::new(Text::from(display_lines)).block(self.block.title(self.title));

        paragraph.render(area, buf);
    }
}

// ── wrap helpers ────────────────────────────────────────────────────────────

/// Returns `(start, end)` char ranges into `input`, one per wrapped row.
fn wrapped_lines(
    input: &str,
    prefix: &str,
    inner_width: usize,
) -> Vec<(usize, usize)> {
    segment_ranges(input, inner_width.saturating_sub(prefix.width()))
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    #[test]
    fn wrapped_lines_empty_input() {
        let lines = wrapped_lines("", "❯ ", 18);
        assert_eq!(lines, vec![(0, 0)]);
    }

    #[test]
    fn wrapped_lines_single_row() {
        let lines = wrapped_lines("abc", "❯ ", 18);
        assert_eq!(lines, vec![(0, 3)]);
    }

    #[test]
    fn wrapped_lines_word_wrap() {
        let lines = wrapped_lines("hello world", "❯ ", 8);
        // inner 8, prefix 2 → line width 6: "hello " fits (6), "world" next
        assert_eq!(lines, vec![(0, 6), (6, 11)]);
    }

    #[test]
    fn wrapped_lines_hard_break_without_spaces() {
        let lines = wrapped_lines("abcdefgh", "❯ ", 8);
        assert_eq!(lines, vec![(0, 6), (6, 8)]);
    }

    #[test]
    fn wrapped_lines_cjk() {
        let lines = wrapped_lines("あいう", "❯ ", 8);
        // line width 6 → あ(2)い(2)う(2)
        assert_eq!(lines, vec![(0, 3)]);
    }

    #[test]
    fn wrapped_lines_cjk_wraps() {
        let lines = wrapped_lines("あいうえ", "❯ ", 8);
        assert_eq!(lines, vec![(0, 3), (3, 4)]);
    }

    #[test]
    fn block_height_grows_with_wrap() {
        assert_eq!(
            PromptWidget::block_height("abc", "❯ ", 30),
            PROMPT_MIN_HEIGHT
        );
        assert!(PromptWidget::block_height("hello world", "❯ ", 10) > PROMPT_MIN_HEIGHT);
    }

    #[test]
    fn screen_cursor_at_start() {
        let widget = PromptWidget::new("hello", 0, "❯ ");
        let area = Rect::new(0, 0, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 3);
        assert_eq!(y, 1);
    }

    #[test]
    fn screen_cursor_after_text() {
        let widget = PromptWidget::new("hi", 2, "> ");
        let area = Rect::new(5, 2, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 10);
        assert_eq!(y, 3);
    }

    #[test]
    fn screen_cursor_on_second_wrapped_line() {
        let widget = PromptWidget::new("hello world", 6, "❯ ");
        let area = Rect::new(0, 0, 10, 5);
        let (x, y) = widget.screen_cursor(area);
        // "hello " on line 0, cursor at start of "world" on line 1
        assert_eq!(x, 3);
        assert_eq!(y, 2);
    }

    #[test]
    fn screen_cursor_clamps_to_border() {
        let widget = PromptWidget::new("hello", 5, "❯ ");
        let area = Rect::new(0, 0, 5, 3);
        let (x, _y) = widget.screen_cursor(area);
        assert!(x <= 3);
    }

    #[test]
    fn screen_cursor_after_cjk() {
        let widget = PromptWidget::new("あいう", 3, "❯ ");
        let area = Rect::new(0, 0, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 9);
        assert_eq!(y, 1);
    }

    #[test]
    fn screen_cursor_mid_cjk() {
        let widget = PromptWidget::new("あいう", 1, "❯ ");
        let area = Rect::new(0, 0, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 5);
        assert_eq!(y, 1);
    }

    #[test]
    fn screen_cursor_mixed_ascii_cjk() {
        let widget = PromptWidget::new("abあcd", 3, "❯ ");
        let area = Rect::new(0, 0, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 7);
        assert_eq!(y, 1);
    }

    #[test]
    fn with_title_preserves_cursor_math() {
        let widget = PromptWidget::new("hi", 2, "> ").with_title(" shell ");
        let area = Rect::new(5, 2, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 10);
        assert_eq!(y, 3);
    }

    #[test]
    fn with_style_does_not_affect_cursor_math() {
        let widget = PromptWidget::new("hi", 2, "> ")
            .with_title(" shell ")
            .with_style(Style::default().fg(Color::Magenta));
        let area = Rect::new(5, 2, 30, 3);
        let (x, y) = widget.screen_cursor(area);
        assert_eq!(x, 10);
        assert_eq!(y, 3);
    }
}
