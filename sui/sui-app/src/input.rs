use crate::app::PendingLlm;
use crate::mode::Mode;
use crate::{App, char_index_to_byte};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub fn handle_event(
        &mut self,
        event: &Event,
    ) {
        if let Event::Key(key) = event {
            self.handle_key(*key);
        }
    }

    pub(crate) fn handle_enter(&mut self) {
        match self.mode {
            Mode::Shell => {
                let command = self.input.clone();
                if self.handle_shell_command(&command) {
                    // One-shot: leave shell once the bash command finishes.
                    self.set_mode(Mode::Prompt);
                }
            },
            Mode::Prompt => {
                // An open @-mention panel takes Enter to accept the file and
                // keep composing, rather than submitting the prompt.
                if !self.at_candidates.is_empty() {
                    self.accept_selected_at();
                    return;
                }
                if self.input.is_empty() {
                    return;
                }
                if !self.slash_candidates.is_empty() {
                    self.execute_selected_slash_command();
                } else if self.input.starts_with('/') {
                    let cmd = self.input[1..].to_owned();
                    self.handle_slash_command(&cmd);
                } else {
                    let prompt = self.input.clone();
                    self.handle_chat_prompt(&prompt);
                }
            },
        }
        self.input.clear();
        self.cursor_position = 0;
        self.slash_candidates.clear();
        self.slash_selected = 0;
        self.at_candidates.clear();
        self.at_selected = 0;
    }

    /// Queues a user turn to the configured LLM.
    ///
    /// Returns immediately after spawning the worker; the event loop polls
    /// [`crate::App::poll_pending_llm`] and renders Markdown deltas above the
    /// prompt until the stream completes (or [`crate::llm::DEFAULT_CHAT_TIMEOUT`]
    /// fires).
    pub(crate) fn handle_chat_prompt(
        &mut self,
        prompt: &str,
    ) {
        self.add_message(prompt);
        let Some(client) = self.llm.clone() else {
            self.add_message(
                "llm not configured: set [llm] in config.toml or SUI_LLM_BASE_URL and SUI_LLM_MODEL (optional SUI_LLM_API_KEY)",
            );
            return;
        };

        let rx = if let Some(tools) = self.tools.clone() {
            if self.chat_history.is_empty() {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                self.chat_history
                    .push(sui_llm::ChatMessage::system(sui_agent::system_prompt(&cwd)));
            }
            crate::llm::agent_spawn(&client, tools, &self.chat_history, prompt)
        } else {
            let mut outgoing = self.chat_history.clone();
            outgoing.push(sui_llm::ChatMessage::user(prompt.to_owned()));
            crate::llm::chat_stream_spawn(&client, &outgoing)
        };
        self.pending_llm = Some(PendingLlm::new(rx));
    }

    /// Runs a one-shot shell command via [`crate::bang`].
    ///
    /// Returns `true` when a non-empty command was submitted (run attempted).
    /// Empty input shows usage and returns `false` so the caller can stay in
    /// [`Mode::Shell`].
    ///
    /// Blocks the event loop until the command finishes or hits the default
    /// timeout ([`sui_tools::DEFAULT_RUN_TIMEOUT`]). Long-running commands will
    /// freeze the TUI until they exit or are killed by that deadline.
    pub(crate) fn handle_shell_command(
        &mut self,
        command: &str,
    ) -> bool {
        let command = command.trim();
        if command.is_empty() {
            self.add_message("usage: <command>");
            return false;
        }
        self.add_message(format!("! {command}"));
        match crate::bang::run_blocking(command) {
            Ok(output) => {
                for line in crate::bang::format_output(&output) {
                    self.add_ghost(line);
                }
            },
            Err(error) => self.add_message(format!("bash error: {error}")),
        }
        true
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
    ) {
        // Quit shortcuts still abandon an in-flight request so history stays paired.
        if self.pending_llm.is_some() {
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.abandon_pending_llm();
                    self.should_quit = true;
                    return;
                },
                KeyCode::Esc => {
                    self.abandon_pending_llm();
                    self.should_quit = true;
                    return;
                },
                KeyCode::Enter => return,
                _ => {},
            }
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            },
            KeyCode::Esc => {
                if matches!(self.mode, Mode::Prompt) {
                    self.should_quit = true;
                } else {
                    // Shell (and future modes): leave back to Prompt.
                    self.set_mode(Mode::Prompt);
                }
            },
            KeyCode::Enter => self.handle_enter(),
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    let new_pos = self.cursor_position.saturating_sub(1);
                    if let Some(byte_idx) = char_index_to_byte(&self.input, new_pos) {
                        self.input.remove(byte_idx);
                        self.cursor_position = new_pos;
                    }
                }
                self.refresh_suggestions();
            },
            KeyCode::Delete => {
                if self.cursor_position < self.input.chars().count()
                    && let Some(byte_idx) = char_index_to_byte(&self.input, self.cursor_position)
                {
                    self.input.remove(byte_idx);
                }
                self.refresh_suggestions();
            },
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position = self.cursor_position.saturating_sub(1);
                }
                self.refresh_suggestions();
            },
            KeyCode::Right => {
                if self.cursor_position < self.input.chars().count() {
                    self.cursor_position = self.cursor_position.saturating_add(1);
                }
                self.refresh_suggestions();
            },
            KeyCode::Home => {
                self.cursor_position = 0;
                self.refresh_suggestions();
            },
            KeyCode::End => {
                self.cursor_position = self.input.chars().count();
                self.refresh_suggestions();
            },
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = 0;
                self.refresh_suggestions();
            },
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_position = self.input.chars().count();
                self.refresh_suggestions();
            },
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position < self.input.chars().count() {
                    self.cursor_position = self.cursor_position.saturating_add(1);
                }
                self.refresh_suggestions();
            },
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position > 0 {
                    self.cursor_position = self.cursor_position.saturating_sub(1);
                }
                self.refresh_suggestions();
            },
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position > 0 {
                    let new_pos = self.cursor_position.saturating_sub(1);
                    if let Some(byte_idx) = char_index_to_byte(&self.input, new_pos) {
                        self.input.remove(byte_idx);
                        self.cursor_position = new_pos;
                    }
                }
                self.refresh_suggestions();
            },
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position < self.input.chars().count()
                    && let Some(byte_idx) = char_index_to_byte(&self.input, self.cursor_position)
                {
                    self.input.remove(byte_idx);
                }
                self.refresh_suggestions();
            },
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.cursor_position < self.input.chars().count() {
                    let start_byte = char_index_to_byte(&self.input, self.cursor_position)
                        .unwrap_or(self.input.len());
                    self.input.truncate(start_byte);
                }
                self.refresh_suggestions();
            },
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cycle_suggestion(true);
            },
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cycle_suggestion(false);
            },
            // Empty prompt + `!` enters one-shot shell mode (Enter/Esc → Prompt).
            KeyCode::Char('!')
                if self.mode == Mode::Prompt
                    && self.input.is_empty()
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.set_mode(Mode::Shell);
            },
            KeyCode::Char(c) => {
                let byte_pos = char_index_to_byte(&self.input, self.cursor_position)
                    .unwrap_or(self.input.len());
                self.input.insert(byte_pos, c);
                self.cursor_position = self.cursor_position.saturating_add(1);
                self.refresh_suggestions();
            },
            // Tab accepts the highlighted @-mention (insert path, keep editing).
            KeyCode::Tab if !self.at_candidates.is_empty() => {
                self.accept_selected_at();
            },
            KeyCode::Tab if !self.slash_candidates.is_empty() => {
                if matches!(
                    self.slash_candidates[self.slash_selected],
                    crate::slash::SlashCandidate::Model { .. }
                ) {
                    "/model".clone_into(&mut self.input);
                } else {
                    let name = self.selected_candidate_name();
                    self.input = format!("/{name}");
                }
                self.cursor_position = self.input.chars().count();
                self.slash_selected = (self.slash_selected + 1) % self.slash_candidates.len();
                self.refresh_suggestions();
            },
            KeyCode::Down if self.suggestion_count() > 0 => {
                self.cycle_suggestion(true);
            },
            KeyCode::BackTab | KeyCode::Up if self.suggestion_count() > 0 => {
                self.cycle_suggestion(false);
            },
            _ => {},
        }
    }
}
