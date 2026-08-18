//! `@`-mention file picker for [`crate::Mode::Prompt`].
//!
//! Typing `@` anywhere in the prompt opens a suggestion panel (mirroring the
//! slash-command panel) listing workspace files whose relative path contains
//! the text between `@` and the cursor. Accepting a candidate (Enter or Tab)
//! replaces that `@…` token with the plain relative path so the composed prompt
//! reads naturally (e.g. `explain src/main.rs`).
//!
//! Suggestions never fire while the input is a slash command — `/` owns the
//! panel then. The file list is walked once per session and cached; it reuses
//! the [`sui_tools::list_files`] skip policy (no `target`, dot-dirs, secrets).

use crate::App;
use crate::char_index_to_byte;
use crate::slash::MAX_CANDIDATES;
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};

/// Upper bound on files cached for the picker (bounds the one-time walk cost).
const FILE_CACHE_LIMIT: usize = 10_000;

/// Locates the active `@`-mention token at the cursor.
///
/// Scans left from `cursor` (a char index into `input`). A token is an `@`
/// that sits at the start of the input or right after whitespace, with no
/// whitespace between it and the cursor. Returns `(at_char_index, query)`
/// where `query` is the text between `@` and the cursor (possibly empty).
///
/// Returns `None` when the cursor is not inside such a token — including
/// email-like `foo@bar`, where `@` is preceded by a non-space character.
#[must_use]
pub fn active_at_token(
    input: &str,
    cursor: usize,
) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    for i in (0..cursor).rev() {
        let c = chars[i];
        if c.is_whitespace() {
            return None;
        }
        if c == '@' {
            let boundary = i == 0 || chars[i - 1].is_whitespace();
            if !boundary {
                return None;
            }
            let query: String = chars[i + 1..cursor].iter().collect();
            return Some((i, query));
        }
    }
    None
}

/// Filters `files` to those whose path contains `query`, preserving order and
/// keeping at most `limit` matches. An empty query keeps the first `limit`
/// files.
///
/// Matching is **ASCII case-insensitive** (non-ASCII bytes compare exactly),
/// and allocation-free per path so it stays cheap when called on every
/// keystroke over a large cached file list.
#[must_use]
pub fn filter_files(
    files: &[String],
    query: &str,
    limit: usize,
) -> Vec<String> {
    let needle = query.to_ascii_lowercase();
    files
        .iter()
        .filter(|path| contains_ascii_case_insensitive(path, needle.as_bytes()))
        .take(limit)
        .cloned()
        .collect()
}

/// ASCII-case-insensitive substring test that allocates nothing.
///
/// `needle` must already be ASCII-lowercased. An empty needle always matches.
fn contains_ascii_case_insensitive(
    haystack: &str,
    needle: &[u8],
) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = haystack.as_bytes();
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

impl App {
    /// Populates the workspace file cache once, on first use.
    fn ensure_file_cache(&mut self) {
        if self.file_cache.is_none() {
            let files = std::env::current_dir()
                .ok()
                .and_then(|root| sui_tools::list_workspace_files(&root, FILE_CACHE_LIMIT).ok())
                .unwrap_or_default();
            self.file_cache = Some(files);
        }
    }

    /// Rebuilds `at_candidates` from the current input and cursor.
    ///
    /// Only applies in [`crate::Mode::Prompt`] and never while the input is a
    /// slash command (slash owns the suggestion panel then).
    pub(crate) fn update_at_candidates(&mut self) {
        if self.mode != crate::Mode::Prompt || self.input.starts_with('/') {
            self.at_candidates.clear();
            self.at_selected = 0;
            return;
        }
        let Some((_, query)) = active_at_token(&self.input, self.cursor_position) else {
            self.at_candidates.clear();
            self.at_selected = 0;
            return;
        };
        self.ensure_file_cache();
        let files = self.file_cache.as_deref().unwrap_or_default();
        self.at_candidates = filter_files(files, &query, MAX_CANDIDATES);
        if self.at_selected >= self.at_candidates.len() {
            self.at_selected = 0;
        }
    }

    /// Replaces the active `@…` token with the highlighted file path.
    ///
    /// No-op when no token or candidate is available. The whole contiguous
    /// token is replaced (from `@` through the next whitespace), even when the
    /// cursor sits mid-token, so no stray suffix is left behind. Leaves the
    /// cursor after the inserted path and a trailing space, then refreshes
    /// suggestions so the panel closes.
    pub(crate) fn accept_selected_at(&mut self) {
        let Some((at_idx, _)) = active_at_token(&self.input, self.cursor_position) else {
            return;
        };
        let Some(path) = self.at_candidates.get(self.at_selected).cloned() else {
            return;
        };
        let chars: Vec<char> = self.input.chars().collect();
        let token_end = chars[self.cursor_position.min(chars.len())..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| self.cursor_position + offset);

        let start = char_index_to_byte(&self.input, at_idx).unwrap_or(self.input.len());
        let end = char_index_to_byte(&self.input, token_end).unwrap_or(self.input.len());
        let replacement = format!("{path} ");
        self.input.replace_range(start..end, &replacement);
        self.cursor_position = at_idx + replacement.chars().count();
        self.refresh_suggestions();
    }

    /// Renders the `@`-mention suggestion lines directly under the prompt.
    pub(crate) fn render_at_suggestions(
        &self,
        frame: &mut Frame,
        area: Rect,
    ) {
        let selected_style = self.theme.selected_style();
        let normal_style = Style::default();

        for (i, path) in self.at_candidates.iter().enumerate() {
            let text = format!(" @{path}");
            let style = if i == self.at_selected {
                selected_style
            } else {
                normal_style
            };
            let line_area = Rect::new(
                area.x,
                area.y + u16::try_from(i).unwrap_or(u16::MAX),
                area.width,
                1,
            );
            frame.render_widget(Paragraph::new(text).style(style), line_area);
        }
    }
}
