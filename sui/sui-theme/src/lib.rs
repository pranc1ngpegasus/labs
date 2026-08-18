//! Central colour palette and derived styles for the sui TUI.
//!
//! Colours that appear on prompt / shell widgets and suggestion panels are
//! defined here as consts so they read as one cohesive palette. The active
//! palette can be selected from `config.toml` via [`config`]; call sites
//! consume a [`Theme`] value so swapping the palette stays localised.

pub mod config;

use ratatui::style::{Color, Style};

/// Named colours shared across the sui TUI widgets.
///
/// The [`DEFAULT`](Self::DEFAULT) value is applied throughout the app. Call
/// sites should consume the derived style methods rather than destructuring
/// the colour fields directly, so swapping the palette later stays localised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Border colour of the interactive prompt widget.
    pub prompt_border: Color,
    /// Border colour of the one-shot shell widget.
    pub shell_border: Color,
    /// Background of a flushed prompt line in the scrollback.
    pub prompt_background: Color,
    /// Foreground of the highlighted suggestion row.
    pub selection_fg: Color,
    /// Background of the highlighted suggestion row.
    pub selection_bg: Color,
}

impl Theme {
    /// The default palette, modelled on the dark variant of the
    /// [iceberg](https://github.com/cocopon/iceberg.vim) colour scheme.
    pub const DEFAULT: Self = Self {
        prompt_border: Color::Rgb(0x84, 0xA0, 0xC6),
        shell_border: Color::Rgb(0xA0, 0x93, 0xC7),
        prompt_background: Color::Rgb(0x3D, 0x42, 0x5B),
        selection_fg: Color::Rgb(0xEF, 0xF0, 0xF4),
        selection_bg: Color::Rgb(0x5B, 0x63, 0x89),
    };

    /// Foreground-only border style for the interactive prompt widget.
    #[must_use]
    pub fn prompt_style(self) -> Style {
        Style::default().fg(self.prompt_border)
    }

    /// Foreground-only border style for the one-shot shell widget.
    #[must_use]
    pub fn shell_style(self) -> Style {
        Style::default().fg(self.shell_border)
    }

    /// Background style for flushed prompt lines in the scrollback.
    #[must_use]
    pub fn prompt_flush_style(self) -> Style {
        Style::default().bg(self.prompt_background)
    }

    /// Filled style for the highlighted suggestion row.
    #[must_use]
    pub fn selected_style(self) -> Style {
        Style::default().fg(self.selection_fg).bg(self.selection_bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_styles_carry_the_expected_colors() {
        assert_eq!(
            Theme::DEFAULT.prompt_style().fg,
            Some(Color::Rgb(0x84, 0xA0, 0xC6))
        );
        assert_eq!(
            Theme::DEFAULT.shell_style().fg,
            Some(Color::Rgb(0xA0, 0x93, 0xC7))
        );
        assert_eq!(
            Theme::DEFAULT.prompt_flush_style().bg,
            Some(Color::Rgb(0x3D, 0x42, 0x5B))
        );
        assert_eq!(
            Theme::DEFAULT.selected_style(),
            Style::default()
                .fg(Color::Rgb(0xEF, 0xF0, 0xF4))
                .bg(Color::Rgb(0x5B, 0x63, 0x89))
        );
    }
}
