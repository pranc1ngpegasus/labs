use crate::App;
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};

pub const MAX_CANDIDATES: usize = 5;

/// Built-in slash commands: `(name, description)`.
///
/// `/exit` and `/quit` both shut down the application. `/model` switches
/// among named models loaded from `[[model.<name>]]` config sections.
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("exit", "quit the application"),
    ("quit", "quit the application"),
    ("model", "switch configured models"),
];

/// A pluggable slash command.
///
/// Implement this trait and register with [`App::register_command`] to add
/// custom slash commands.
///
/// # Example
///
/// ```
/// use sui_app::{App, SlashCommand};
///
/// struct Hello;
///
/// impl SlashCommand for Hello {
///     fn name(&self) -> &'static str { "hello" }
///     fn description(&self) -> &'static str { "print a greeting" }
///     fn execute(&self, app: &mut App) {
///         app.add_message("hello, world!");
///     }
/// }
///
/// let mut app = App::new();
/// app.register_command(Hello);
/// ```
pub trait SlashCommand {
    /// The command name as typed after `/` (e.g. `"skill"` for `/skill`).
    fn name(&self) -> &'static str;

    /// A short description shown in the suggestion panel.
    fn description(&self) -> &'static str;

    /// Called when the user selects and executes this command.
    fn execute(
        &self,
        app: &mut App,
    );
}

/// Internal representation of a single suggestion entry.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SlashCandidate {
    /// Index into [`BUILTIN_COMMANDS`].
    Builtin { index: usize },
    /// Index into [`App::plugins`].
    Plugin { index: usize },
    /// Index into [`App::models`].
    Model { index: usize },
}

/// No-op command used as a placeholder during plugin execution.
pub(crate) struct NoopCommand;

impl SlashCommand for NoopCommand {
    fn name(&self) -> &'static str {
        ""
    }

    fn description(&self) -> &'static str {
        ""
    }

    fn execute(
        &self,
        _app: &mut App,
    ) {
    }
}

impl App {
    /// Renders the slash-command suggestion lines directly under the prompt.
    pub(crate) fn render_slash_suggestions(
        &self,
        frame: &mut Frame,
        area: Rect,
    ) {
        let selected_style = self.theme.selected_style();
        let normal_style = Style::default();

        for (i, candidate) in self.slash_candidates.iter().enumerate() {
            let text = match candidate {
                SlashCandidate::Builtin { index } => {
                    let (name, desc) = BUILTIN_COMMANDS[*index];
                    format!(" /{name} — {desc}")
                },
                SlashCandidate::Plugin { index } => {
                    let cmd = &self.plugins[*index];
                    format!(" /{} — {}", cmd.name(), cmd.description())
                },
                SlashCandidate::Model { index } => {
                    let model = &self.models[*index];
                    let active = if self.active_model == Some(*index) {
                        " (active)"
                    } else {
                        ""
                    };
                    format!(" /{} — {}{}", model.name(), model.config().model(), active)
                },
            };
            let style = if i == self.slash_selected {
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

    /// Rebuilds `slash_candidates` based on the current input.
    ///
    /// Slash suggestions only apply in [`crate::Mode::Prompt`].
    pub(crate) fn update_slash_candidates(&mut self) {
        if self.mode != crate::Mode::Prompt {
            self.slash_candidates.clear();
            self.slash_selected = 0;
            return;
        }
        if let Some(partial) = self.input.strip_prefix('/') {
            let partial = partial.to_owned();
            self.slash_candidates.clear();

            if let Some(query) = model_query(&partial) {
                self.push_model_candidates(query);
            } else {
                // Built-ins are always checked first.
                for (i, (name, _)) in BUILTIN_COMMANDS.iter().enumerate() {
                    if self.slash_candidates.len() >= MAX_CANDIDATES {
                        break;
                    }
                    if name.starts_with(&partial) {
                        self.slash_candidates
                            .push(SlashCandidate::Builtin { index: i });
                    }
                }

                // Plugin commands.
                for (i, cmd) in self.plugins.iter().enumerate() {
                    if self.slash_candidates.len() >= MAX_CANDIDATES {
                        break;
                    }
                    if cmd.name().starts_with(&partial) {
                        self.slash_candidates
                            .push(SlashCandidate::Plugin { index: i });
                    }
                }
            }

            let len = self.slash_candidates.len().max(1);
            if self.slash_selected >= len {
                self.slash_selected = 0;
            }
        } else {
            self.slash_candidates.clear();
            self.slash_selected = 0;
        }
    }

    fn push_model_candidates(
        &mut self,
        query: &str,
    ) {
        for (i, model) in self.models.iter().enumerate() {
            if self.slash_candidates.len() >= MAX_CANDIDATES {
                break;
            }
            if model.name().starts_with(query) {
                self.slash_candidates
                    .push(SlashCandidate::Model { index: i });
            }
        }
    }

    /// Executes the currently highlighted slash candidate.
    pub(crate) fn execute_selected_slash_command(&mut self) {
        enum Action {
            Builtin(&'static str),
            Plugin(usize),
            Model(usize),
        }

        // Extract the action outside the borrow scope.
        let action = {
            let candidate = &self.slash_candidates[self.slash_selected];
            match candidate {
                SlashCandidate::Builtin { index } => Action::Builtin(BUILTIN_COMMANDS[*index].0),
                SlashCandidate::Plugin { index } => Action::Plugin(*index),
                SlashCandidate::Model { index } => Action::Model(*index),
            }
        };
        match action {
            Action::Builtin(name) => self.execute_builtin_command(name),
            Action::Plugin(index) => {
                // Temporarily swap out the command so we can call
                // execute(&mut self) without conflicting borrows.
                let cmd = std::mem::replace(&mut self.plugins[index], Box::new(NoopCommand));
                cmd.execute(self);
                self.plugins[index] = cmd;
            },
            Action::Model(index) => self.switch_to_model(index),
        }
    }

    /// Returns the command name of the currently selected slash candidate.
    pub(crate) fn selected_candidate_name(&self) -> String {
        match &self.slash_candidates[self.slash_selected] {
            SlashCandidate::Builtin { index } => BUILTIN_COMMANDS[*index].0.to_owned(),
            SlashCandidate::Plugin { index } => self.plugins[*index].name().to_owned(),
            SlashCandidate::Model { index } => self.models[*index].name().to_owned(),
        }
    }

    pub(crate) fn handle_slash_command(
        &mut self,
        cmd: &str,
    ) {
        if let Some(name) = cmd.strip_prefix("model ") {
            self.switch_to_model_name(name.trim());
        } else if BUILTIN_COMMANDS.iter().any(|(name, _)| *name == cmd) {
            self.execute_builtin_command(cmd);
        } else {
            self.add_message(format!("unknown command: /{cmd}"));
        }
    }

    fn execute_builtin_command(
        &mut self,
        name: &str,
    ) {
        match name {
            "exit" | "quit" => self.quit(),
            "model" => self.cycle_model(),
            _ => self.add_message(format!("unknown command: /{name}")),
        }
    }

    fn cycle_model(&mut self) {
        if self.models.is_empty() {
            self.add_message("no models configured: add [[model.\"name\"]] entries to config.toml");
            return;
        }
        let next = self
            .active_model
            .map_or(0, |index| (index + 1) % self.models.len());
        self.switch_to_model(next);
    }

    fn switch_to_model_name(
        &mut self,
        name: &str,
    ) {
        if name.is_empty() {
            self.cycle_model();
            return;
        }
        let Some(index) = self.models.iter().position(|model| model.name() == name) else {
            self.add_message(format!("unknown model: {name}"));
            return;
        };
        self.switch_to_model(index);
    }

    fn switch_to_model(
        &mut self,
        index: usize,
    ) {
        let Some(model) = self.models.get(index) else {
            self.add_message("unknown model index");
            return;
        };
        self.llm = Some(sui_llm::LlmClient::new(model.config()));
        self.active_model = Some(index);
        self.add_message(format!(
            "model: {} ({})",
            model.name(),
            model.config().model()
        ));
    }
}

fn model_query(partial: &str) -> Option<&str> {
    match partial {
        "model" => Some(""),
        value if value.starts_with("model ") => Some(&value[6..]),
        _ => None,
    }
}
