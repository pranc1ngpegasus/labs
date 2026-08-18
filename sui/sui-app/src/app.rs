use crate::markdown::{StreamingMarkdown, render_markdown};
use crate::mode::Mode;
use crate::slash::{MAX_CANDIDATES, SlashCandidate, SlashCommand};
use ratatui::{
    DefaultTerminal, Frame, Terminal, TerminalOptions, Viewport,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Layout, Position},
    style::{Modifier, Style},
    text::{Line, Text},
    widgets::{Paragraph, Widget},
};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;
use sui_llm::{ChatMessage, ChatResponse, LlmClient, LlmModel};
use sui_theme::Theme;
use sui_widget::{PROMPT_MIN_HEIGHT, PromptWidget, wrap_prefixed, wrap_text};

/// In-flight LLM chat: worker stream channel + spinner clock.
pub(crate) struct PendingLlm {
    rx: Receiver<crate::llm::LlmStreamMsg>,
    started: Instant,
}

impl PendingLlm {
    pub(crate) fn new(rx: Receiver<crate::llm::LlmStreamMsg>) -> Self {
        Self {
            rx,
            started: Instant::now(),
        }
    }
}

/// Minimum rows occupied by the bordered prompt widget (single content line).
pub const PROMPT_HEIGHT: u16 = PROMPT_MIN_HEIGHT;

/// Number of blank rows padded above and below each flushed prompt line.
const PROMPT_FLUSH_PADDING: usize = 1;

/// Extra inline rows reserved while the slash-suggestion panel is open.
///
/// Kept as a `u16` literal (not `MAX_CANDIDATES as u16`) to satisfy pedantic
/// cast lints; the assert below locks it to [`MAX_CANDIDATES`].
const SUGGESTION_PANEL_HEIGHT: u16 = 5;
const _: () = assert!(SUGGESTION_PANEL_HEIGHT as usize == MAX_CANDIDATES);

/// A single scrollback line pending flush above the inline viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScrollbackLine {
    /// User / status line rendered with the prompt prefix.
    Prompt(String),
    /// Dim ghost text (shell stdout/stderr) without the prompt prefix.
    Ghost(String),
    /// Assistant reply (Markdown source, no prompt prefix).
    Reply(String),
}

/// Holds the full application state: prompt input, message history, and the
/// run-loop flag.
///
/// # Example
///
/// ```no_run
/// use sui_app::App;
/// use sui_llm::LlmClient;
///
/// let mut app = App::new();
/// if let Ok(client) = LlmClient::from_config_or_env() {
///     app = app.with_llm(client);
/// }
/// app.run_inline()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct App {
    pub(crate) input: String,
    /// Char-based cursor position within `input`.
    pub(crate) cursor_position: usize,
    pub(crate) should_quit: bool,
    pub(crate) prompt_prefix: String,
    /// Sticky interaction mode (see [`Mode`]).
    pub(crate) mode: Mode,
    /// Active colour palette for the UI.
    pub(crate) theme: Theme,
    /// History of submitted prompts / status / ghost lines.
    ///
    /// New entries are committed above the inline viewport via
    /// [`Terminal::insert_before`] so they scroll into the terminal scrollback
    /// while the prompt stays pinned once it reaches the bottom of the screen.
    pub(crate) messages: Vec<ScrollbackLine>,
    /// How many messages have already been written above the viewport.
    pub(crate) flushed_messages: usize,
    /// Current [`Viewport::Inline`] height managed by [`App::run`].
    pub(crate) viewport_height: u16,
    /// Registered pluggable slash commands.
    pub(crate) plugins: Vec<Box<dyn SlashCommand>>,
    /// Candidates that match the current slash partial input.
    pub(crate) slash_candidates: Vec<SlashCandidate>,
    /// Currently highlighted index within `slash_candidates`.
    pub(crate) slash_selected: usize,
    /// Workspace file paths matching the active `@`-mention token.
    pub(crate) at_candidates: Vec<String>,
    /// Currently highlighted index within `at_candidates`.
    pub(crate) at_selected: usize,
    /// Lazily-walked workspace file list backing the `@`-mention picker.
    pub(crate) file_cache: Option<Vec<String>>,
    /// Optional OpenAI-compatible client for [`Mode::Prompt`] chat.
    pub(crate) llm: Option<LlmClient>,
    /// Switchable models loaded from `[[model.<name>]]` config sections.
    pub(crate) models: Vec<LlmModel>,
    /// Active index into [`Self::models`], when named models are configured.
    pub(crate) active_model: Option<usize>,
    /// Tools advertised to the model when set ([`App::with_tools`]).
    pub(crate) tools: Option<sui_tools::ToolRegistry>,
    /// Session chat turns sent to the OpenAI-compatible API (user + assistant + tool results).
    pub(crate) chat_history: Vec<ChatMessage>,
    /// Active LLM request (event loop polls; spinner animates until it lands).
    pub(crate) pending_llm: Option<PendingLlm>,
    /// Incremental Markdown for the in-flight assistant reply.
    pub(crate) streaming_reply: Option<StreamingMarkdown>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub(crate) const POLL_DRAIN_BUDGET: usize = 256;

    /// Creates a new `App` with the default prompt prefix (`"❯ "`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_position: 0,
            should_quit: false,
            prompt_prefix: "❯ ".to_string(),
            mode: Mode::Prompt,
            theme: Theme::DEFAULT,
            messages: Vec::new(),
            flushed_messages: 0,
            viewport_height: PROMPT_HEIGHT,
            plugins: Vec::new(),
            slash_candidates: Vec::new(),
            slash_selected: 0,
            at_candidates: Vec::new(),
            at_selected: 0,
            file_cache: None,
            llm: None,
            models: Vec::new(),
            active_model: None,
            tools: None,
            chat_history: Vec::new(),
            pending_llm: None,
            streaming_reply: None,
        }
    }

    /// Attach an LLM client for prompt-mode chat.
    ///
    /// Without this, prompt submits surface a configuration hint instead of
    /// calling the configured API. This single-client mode clears any named
    /// models previously attached with [`Self::with_models`]. Typical wiring:
    /// `App::new().with_llm(LlmClient::from_config_or_env()?)`.
    #[must_use]
    pub fn with_llm(
        mut self,
        client: LlmClient,
    ) -> Self {
        self.llm = Some(client);
        self.models.clear();
        self.active_model = None;
        self
    }

    /// Attach named models for `/model` switching.
    ///
    /// The first model becomes active immediately. Passing an empty list leaves
    /// the current client unchanged and unregisters switchable models.
    #[must_use]
    pub fn with_models(
        mut self,
        models: Vec<LlmModel>,
    ) -> Self {
        let Some(first) = models.first() else {
            self.models.clear();
            self.active_model = None;
            return self;
        };
        self.llm = Some(LlmClient::new(first.config()));
        self.models = models;
        self.active_model = Some(0);
        self
    }

    /// Attach a tool registry so prompt-mode chat runs the agent loop.
    ///
    /// Without this, prompt submits are plain chat (no function calling).
    /// Typical wiring: [`sui_tools::coding_registry`] over the workspace.
    #[must_use]
    pub fn with_tools(
        mut self,
        tools: sui_tools::ToolRegistry,
    ) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Request application shutdown.
    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Current interaction mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Switch mode, clearing the input buffer and slash suggestions.
    pub(crate) fn set_mode(
        &mut self,
        mode: Mode,
    ) {
        self.mode = mode;
        self.input.clear();
        self.cursor_position = 0;
        self.slash_candidates.clear();
        self.slash_selected = 0;
        self.at_candidates.clear();
        self.at_selected = 0;
    }

    /// Rebuilds every suggestion panel (slash commands and `@`-mentions) from
    /// the current input. The two are mutually exclusive, so at most one panel
    /// is populated afterward.
    pub(crate) fn refresh_suggestions(&mut self) {
        self.update_slash_candidates();
        self.update_at_candidates();
    }

    /// Number of rows the active suggestion panel occupies (0 when closed).
    ///
    /// Slash and `@`-mention panels never open together, so the larger of the
    /// two counts is the active one.
    #[must_use]
    pub(crate) fn suggestion_count(&self) -> usize {
        self.slash_candidates.len().max(self.at_candidates.len())
    }

    /// Moves the highlight within whichever suggestion panel is open, wrapping.
    pub(crate) fn cycle_suggestion(
        &mut self,
        forward: bool,
    ) {
        let step = |selected: usize, len: usize| {
            if forward {
                (selected + 1) % len
            } else {
                (selected + len - 1) % len
            }
        };
        if !self.slash_candidates.is_empty() {
            self.slash_selected = step(self.slash_selected, self.slash_candidates.len());
        } else if !self.at_candidates.is_empty() {
            self.at_selected = step(self.at_selected, self.at_candidates.len());
        }
    }

    /// Append a normal (prompt-prefixed) message to the scrollback history.
    ///
    /// The next [`App::run`] iteration (or [`App::flush_messages`]) writes it
    /// above the inline prompt into the terminal scrollback.
    pub fn add_message(
        &mut self,
        msg: impl Into<String>,
    ) {
        self.messages.push(ScrollbackLine::Prompt(msg.into()));
    }

    /// Append dim ghost text (e.g. shell command output) to the scrollback.
    pub fn add_ghost(
        &mut self,
        msg: impl Into<String>,
    ) {
        self.messages.push(ScrollbackLine::Ghost(msg.into()));
    }

    /// Append an assistant reply (Markdown source, no prompt prefix).
    pub fn add_reply(
        &mut self,
        msg: impl Into<String>,
    ) {
        self.messages.push(ScrollbackLine::Reply(msg.into()));
    }

    /// Border title for the current mode.
    #[must_use]
    pub(crate) const fn prompt_title(&self) -> &'static str {
        self.mode.title()
    }

    /// Register a pluggable slash command.
    ///
    /// Built-in `/exit` and `/quit` are always available. Commands registered
    /// here appear alongside them in the suggestion panel.
    pub fn register_command(
        &mut self,
        cmd: impl SlashCommand + 'static,
    ) {
        self.plugins.push(Box::new(cmd));
    }

    /// Sets the prompt prefix.
    ///
    /// The default is `"❯ "`.
    ///
    /// # Example
    ///
    /// ```
    /// use sui_app::App;
    /// let app = App::new().with_prompt_prefix("$ ");
    /// ```
    /// Sets the active colour palette for the UI.
    ///
    /// Defaults to [`sui_theme::Theme::DEFAULT`].
    #[must_use]
    pub const fn with_theme(
        mut self,
        theme: Theme,
    ) -> Self {
        self.theme = theme;
        self
    }

    #[must_use]
    pub fn with_prompt_prefix(
        mut self,
        prefix: impl Into<String>,
    ) -> Self {
        self.prompt_prefix = prefix.into();
        self
    }

    /// Inline viewport height for the current UI at the given terminal width.
    ///
    /// Grows with wrapped prompt input, the streaming reply, and expands by a
    /// fixed suggestion-panel budget when any slash candidates are visible.
    #[must_use]
    pub fn inline_height(
        &self,
        width: u16,
    ) -> u16 {
        let prompt_height = PromptWidget::block_height(&self.input, &self.prompt_prefix, width);
        let streaming_height = self
            .streaming_reply
            .as_ref()
            .map_or(0, |reply| reply.line_count(width as usize));
        let suggestions_height = if self.suggestion_count() == 0 {
            0
        } else {
            SUGGESTION_PANEL_HEIGHT
        };
        prompt_height
            .saturating_add(streaming_height)
            .saturating_add(suggestions_height)
    }

    /// Initialize an inline terminal, run until quit, then restore the terminal.
    ///
    /// This is the preferred entry point: no alternate screen, prompt-only
    /// viewport, scrollback via [`App::flush_messages`].
    ///
    /// On exit only raw mode is disabled. [`ratatui::restore`] is intentionally
    /// not used: it always emits `LeaveAlternateScreen` (`CSI ?1049l`), and many
    /// terminals treat that as “restore cursor” even when the app never entered
    /// the alternate buffer — yanking the cursor above `insert_before`
    /// scrollback (including shell ghost lines).
    ///
    /// # Errors
    /// Returns an I/O error if terminal setup or the run loop fails. Raw-mode
    /// cleanup is best-effort and does not override a successful run result.
    pub fn run_inline(&mut self) -> std::io::Result<()> {
        let mut terminal = ratatui::try_init_with_options(TerminalOptions {
            viewport: Viewport::Inline(PROMPT_HEIGHT),
        })?;
        let result = self.run(&mut terminal);
        let _ = crossterm::terminal::disable_raw_mode();
        result
    }

    /// Blocking run loop: flush scrollback → sync viewport → draw → read event.
    ///
    /// Prefer [`App::run_inline`] unless you already own an inline
    /// [`ratatui::Viewport`] terminal whose height starts at [`PROMPT_HEIGHT`].
    ///
    /// On exit any pending scrollback (including ghost lines) is flushed, the
    /// inline viewport is cleared, and the cursor is moved to the viewport
    /// origin so the host shell prompt sits just below that output.
    ///
    /// Callers that set up the terminal themselves should disable raw mode
    /// after this returns and **must not** call [`ratatui::restore`] (it emits
    /// `LeaveAlternateScreen`, which can reset the cursor above the scrollback).
    ///
    /// # Errors
    /// Returns an I/O error if terminal operations or event reading fail.
    pub fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
    ) -> std::io::Result<()> {
        let result = self.event_loop(terminal);
        // Commit any lines queued by the final event (e.g. bang ghosts) before
        // parking the cursor — otherwise teardown uses a stale viewport origin.
        let _ = self.flush_messages(terminal);
        let _ = Self::teardown_inline(terminal);
        result
    }

    fn event_loop(
        &mut self,
        terminal: &mut DefaultTerminal,
    ) -> std::io::Result<()> {
        while !self.should_quit {
            self.poll_pending_llm();
            self.flush_messages(terminal)?;
            self.sync_viewport_height(terminal)?;
            terminal.draw(|frame| self.render(frame))?;
            if self.pending_llm.is_some() {
                // Short poll so the spinner advances while the worker runs.
                if crossterm::event::poll(crate::llm::SPINNER_TICK)? {
                    self.handle_event(&crossterm::event::read()?);
                }
            } else {
                self.handle_event(&crossterm::event::read()?);
            }
        }
        Ok(())
    }

    fn ensure_streaming_reply(&mut self) -> &mut StreamingMarkdown {
        self.streaming_reply
            .get_or_insert_with(StreamingMarkdown::new)
    }

    fn clear_streaming_reply(&mut self) {
        self.streaming_reply = None;
    }

    fn finish_streaming_reply(
        &mut self,
        response: ChatResponse,
        history: Vec<ChatMessage>,
    ) {
        self.chat_history = history;
        if let Some(mut streaming) = self.streaming_reply.take() {
            streaming.finish();
        }
        self.add_reply(response.content);
    }

    /// Returns `true` when the stream is finished and [`Self::pending_llm`]
    /// should be cleared.
    fn handle_stream_msg(
        &mut self,
        msg: crate::llm::LlmStreamMsg,
    ) -> bool {
        use crate::llm::LlmStreamMsg;
        match msg {
            LlmStreamMsg::Chunk(delta) => {
                self.ensure_streaming_reply().push_delta(&delta);
                false
            },
            LlmStreamMsg::Tool(line) => {
                self.add_ghost(line);
                false
            },
            LlmStreamMsg::Done { response, history } => {
                self.finish_streaming_reply(response, history);
                true
            },
            LlmStreamMsg::Failed(error) => {
                self.clear_streaming_reply();
                self.add_message(format!("llm error: {error}"));
                true
            },
        }
    }

    /// Non-blocking check for an in-flight LLM response.
    pub(crate) fn poll_pending_llm(&mut self) {
        for _ in 0..Self::POLL_DRAIN_BUDGET {
            let msg = {
                let Some(pending) = &self.pending_llm else {
                    return;
                };
                match pending.rx.try_recv() {
                    Ok(msg) => msg,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        crate::llm::LlmStreamMsg::Failed("llm worker disconnected".into())
                    },
                }
            };
            if self.handle_stream_msg(msg) {
                self.pending_llm = None;
                return;
            }
        }
    }

    /// Drop an in-flight request and roll back the optimistic user turn.
    ///
    /// Used when quitting mid-request so [`Self::chat_history`] stays paired.
    pub(crate) fn abandon_pending_llm(&mut self) {
        if self.pending_llm.take().is_some() {
            self.clear_streaming_reply();
        }
    }

    /// Blocks until any in-flight LLM request finishes (tests / sync callers).
    #[cfg(test)]
    pub(crate) fn settle_pending_llm(&mut self) {
        loop {
            let msg = {
                let Some(pending) = &self.pending_llm else {
                    return;
                };
                pending.rx.recv().unwrap_or_else(|_| {
                    crate::llm::LlmStreamMsg::Failed("llm worker disconnected".into())
                })
            };
            if self.handle_stream_msg(msg) {
                self.pending_llm = None;
                return;
            }
        }
    }

    /// Border title: mode title normally, animated working spinner while waiting.
    pub(crate) fn prompt_title_for_render(&self) -> String {
        self.pending_llm.as_ref().map_or_else(
            || self.prompt_title().to_owned(),
            |pending| {
                let glyph = crate::llm::spinner_glyph(pending.started.elapsed());
                format!(" working {glyph} ")
            },
        )
    }

    /// Clears the inline viewport and parks the cursor at its top-left origin.
    ///
    /// [`Terminal::clear`] restores the previous cursor position afterward, so
    /// callers must move back to the viewport origin explicitly (same pattern
    /// as Atuin’s inline-search teardown).
    pub(crate) fn teardown_inline<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        let origin = terminal.get_frame().area().as_position();
        terminal.clear()?;
        terminal.set_cursor_position(origin)?;
        Ok(())
    }

    /// Writes any unflushed messages above the inline viewport.
    ///
    /// Prompt lines include [`Self::prompt_prefix`]. Ghost lines are dim and
    /// unprefixed.
    ///
    /// Once the viewport reaches the bottom of the terminal, further inserts
    /// scroll prior output into the scrollback buffer and keep the prompt pinned.
    ///
    /// # Errors
    /// Returns a backend error if inserting lines fails.
    pub fn flush_messages<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<(), B::Error> {
        let width = terminal.get_frame().area().width;
        while self.flushed_messages < self.messages.len() {
            let line = &self.messages[self.flushed_messages];
            match line {
                ScrollbackLine::Prompt(text) => {
                    let rows = Self::padded_prompt_rows(text, &self.prompt_prefix, width as usize);
                    Self::insert_wrapped_rows(terminal, &rows, self.theme.prompt_flush_style())?;
                },
                ScrollbackLine::Ghost(text) => {
                    let rows = wrap_text(text, width as usize);
                    Self::insert_wrapped_rows(
                        terminal,
                        &rows,
                        Style::default().add_modifier(Modifier::DIM),
                    )?;
                },
                ScrollbackLine::Reply(text) => {
                    let rendered = render_markdown(text, width as usize);
                    Self::insert_wrapped_lines(terminal, rendered.lines)?;
                },
            }
            self.flushed_messages += 1;
        }
        Ok(())
    }

    /// Wraps a prompt to `max_width` and pads `PROMPT_FLUSH_PADDING` blank rows
    /// above and below so it reads as a distinct block in the scrollback.
    fn padded_prompt_rows(
        text: &str,
        prefix: &str,
        max_width: usize,
    ) -> Vec<String> {
        let mut rows = wrap_prefixed(text, prefix, max_width);
        for _ in 0..PROMPT_FLUSH_PADDING {
            rows.insert(0, String::new());
            rows.push(String::new());
        }
        rows
    }

    fn insert_wrapped_rows<B: Backend>(
        terminal: &mut Terminal<B>,
        rows: &[String],
        style: Style,
    ) -> Result<(), B::Error> {
        let lines: Vec<Line<'_>> = rows.iter().map(|row| Line::from(row.as_str())).collect();
        Self::insert_wrapped_lines_styled(terminal, lines, style)
    }

    fn insert_wrapped_lines<B: Backend>(
        terminal: &mut Terminal<B>,
        rows: Vec<Line<'static>>,
    ) -> Result<(), B::Error> {
        Self::insert_wrapped_lines_styled(terminal, rows, Style::default())
    }

    fn insert_wrapped_lines_styled<B: Backend>(
        terminal: &mut Terminal<B>,
        rows: Vec<Line<'_>>,
        style: Style,
    ) -> Result<(), B::Error> {
        let height = u16::try_from(rows.len().max(1)).unwrap_or(u16::MAX);
        terminal.insert_before(height, move |buf| {
            Paragraph::new(Text::from(rows))
                .style(style)
                .render(buf.area, buf);
        })
    }

    /// Grows or shrinks the inline viewport to match [`App::inline_height`].
    ///
    /// Ratatui fixes [`Viewport::Inline`] height at construction time, so a
    /// height change rebuilds the terminal on the normal screen (raw mode stays
    /// enabled; only the viewport is replaced).
    fn sync_viewport_height(
        &mut self,
        terminal: &mut DefaultTerminal,
    ) -> std::io::Result<()> {
        let width = terminal.get_frame().area().width;
        let height = self.inline_height(width);
        if height == self.viewport_height {
            return Ok(());
        }

        let area = terminal.get_frame().area();
        terminal.clear()?;
        terminal.set_cursor_position(Position::new(area.x, area.y))?;

        let backend = CrosstermBackend::new(std::io::stdout());
        *terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        self.viewport_height = height;
        Ok(())
    }

    fn render(
        &self,
        frame: &mut Frame,
    ) {
        let area = frame.area();
        let width = area.width;
        let prompt_height = PromptWidget::block_height(&self.input, &self.prompt_prefix, width);
        let streaming_height = self
            .streaming_reply
            .as_ref()
            .map_or(0, |reply| reply.line_count(width as usize));
        let suggestions_height = if self.suggestion_count() == 0 {
            0
        } else {
            u16::try_from(self.suggestion_count().min(MAX_CANDIDATES)).unwrap_or(u16::MAX)
        };

        let [streaming_area, prompt_area, suggestions_area, _reserved] = Layout::vertical([
            Constraint::Length(streaming_height),
            Constraint::Length(prompt_height),
            Constraint::Length(suggestions_height),
            Constraint::Min(0),
        ])
        .areas(area);

        if streaming_height > 0
            && let Some(streaming) = &self.streaming_reply
        {
            let text = Text::from(streaming.render_lines(width as usize));
            Paragraph::new(text).render(streaming_area, frame.buffer_mut());
        }

        let title = self.prompt_title_for_render();
        let prompt = PromptWidget::new(&self.input, self.cursor_position, &self.prompt_prefix)
            .with_title(&title)
            .with_style(self.mode.border_style(self.theme));
        let cursor_pos = prompt.screen_cursor(prompt_area);
        frame.render_widget(prompt, prompt_area);
        frame.set_cursor_position((cursor_pos.0, cursor_pos.1));

        if !self.slash_candidates.is_empty() {
            self.render_slash_suggestions(frame, suggestions_area);
        } else if !self.at_candidates.is_empty() {
            self.render_at_suggestions(frame, suggestions_area);
        }
    }
}
