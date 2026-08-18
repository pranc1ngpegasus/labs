//! Markdown rendering via [`codewandler-markdown`] for assistant replies.
//!
//! The incremental parser feeds a ratatui [`Text`] renderer so streaming LLM
//! deltas can be painted on every frame without re-parsing the full document.

use markdown::{Event, Parser, StreamParser};
use markdown_ratatui::{Theme, render_with};
use ratatui::text::{Line, Text};

/// Incremental Markdown state for an in-flight assistant reply.
pub struct StreamingMarkdown {
    parser: StreamParser,
    events: Vec<Event>,
    buffer: String,
}

impl StreamingMarkdown {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            parser: StreamParser::new(),
            events: Vec::new(),
            buffer: String::new(),
        }
    }

    pub(crate) fn push_delta(
        &mut self,
        delta: &str,
    ) {
        if delta.is_empty() {
            return;
        }
        self.buffer.push_str(delta);
        self.events.extend(self.parser.write(delta.as_bytes()));
    }

    pub(crate) fn finish(&mut self) {
        self.events.extend(self.parser.flush());
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn render_buffer_lines(
        &self,
        width: usize,
    ) -> Vec<Line<'static>> {
        render_markdown(&self.buffer, width).lines
    }

    #[must_use]
    pub(crate) fn line_count(
        &self,
        width: usize,
    ) -> u16 {
        let lines = self.render_lines(width);
        u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX)
    }

    #[must_use]
    pub(crate) fn render_lines(
        &self,
        width: usize,
    ) -> Vec<Line<'static>> {
        render_markdown_events(&self.events, width).lines
    }
}

/// Render a complete Markdown document to ratatui lines at `width` columns.
#[must_use]
pub fn render_markdown(
    text: &str,
    width: usize,
) -> Text<'static> {
    let events = markdown::parse(text);
    render_markdown_events(&events, width)
}

fn render_markdown_events(
    events: &[Event],
    width: usize,
) -> Text<'static> {
    render_with(events, &Theme::default(), width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn streaming_bold_across_chunks() {
        let mut stream = StreamingMarkdown::new();
        stream.push_delta("I am **fugu");
        stream.push_delta("**, done");
        stream.finish();
        let lines = stream.render_lines(80);
        let has_bold = lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.style.add_modifier(Modifier::BOLD) == s.style && s.content.contains("fugu"));
        assert!(has_bold, "expected bold fugu, lines={lines:?}");
    }

    #[test]
    fn render_markdown_strips_bold_markers() {
        let text = render_markdown("**hello**", 40);
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert_eq!(rendered, "hello");
    }
}
