use super::{App, Mode, PROMPT_HEIGHT, char_index_to_byte};
use crate::app::{PendingLlm, ScrollbackLine};
use crate::llm::LlmStreamMsg;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Position;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::sync::mpsc;
use sui_theme::Theme;

fn message_texts(app: &App) -> Vec<&str> {
    app.messages
        .iter()
        .map(|line| match line {
            ScrollbackLine::Prompt(text)
            | ScrollbackLine::Ghost(text)
            | ScrollbackLine::Reply(text) => text.as_str(),
        })
        .collect()
}

#[test]
fn char_index_to_byte_ascii() {
    assert_eq!(char_index_to_byte("hello", 0), Some(0));
    assert_eq!(char_index_to_byte("hello", 4), Some(4));
}

#[test]
fn char_index_to_byte_multibyte() {
    // "あいう" — each char is 3 bytes
    assert_eq!(char_index_to_byte("あいう", 0), Some(0));
    assert_eq!(char_index_to_byte("あいう", 1), Some(3));
    assert_eq!(char_index_to_byte("あいう", 2), Some(6));
}

#[test]
fn char_index_to_byte_past_end() {
    assert_eq!(char_index_to_byte("hi", 2), None);
    assert_eq!(char_index_to_byte("", 0), None);
}

#[test]
fn with_prompt_prefix_changes_prefix() {
    let app = App::new().with_prompt_prefix("$ ");
    assert_eq!(app.prompt_prefix, "$ ");
}

// ── key-handling tests ──────────────────────────────────────────────

fn key_char(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn typing_appends_to_input() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    assert_eq!(app.input, "hi");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn enter_submits_and_clears() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.input.is_empty());
    assert_eq!(app.cursor_position, 0);
    assert_eq!(
        message_texts(&app),
        vec![
            "a",
            "llm not configured: set [llm] in config.toml or SUI_LLM_BASE_URL and SUI_LLM_MODEL (optional SUI_LLM_API_KEY)"
        ]
    );
}

#[test]
fn enter_on_empty_does_nothing() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.messages.is_empty());
}

#[test]
fn backspace_removes_char_before_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('x'));
    app.handle_key(key_char('y'));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.input, "x");
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn backspace_at_start_does_nothing() {
    let mut app = App::new();
    app.handle_key(key_char('x'));
    // Move cursor to start, then backspace
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.input, "x");
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn delete_removes_char_at_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    app.handle_key(key(KeyCode::Left)); // cursor now before 'b'
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.input, "a");
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn left_right_move_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor_position, 1);
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn home_end_move_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    app.handle_key(key(KeyCode::Home));
    assert_eq!(app.cursor_position, 0);
    app.handle_key(key(KeyCode::End));
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn esc_sets_should_quit() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.should_quit);
}

#[test]
fn ctrl_c_sets_should_quit() {
    let mut app = App::new();
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.should_quit);
}

#[test]
fn insert_middle_of_input() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('c'));
    app.handle_key(key(KeyCode::Left)); // cursor between 'a' and 'c'
    app.handle_key(key_char('b'));
    assert_eq!(app.input, "abc");
    assert_eq!(app.cursor_position, 2);
}

// ── slash-command tests ────────────────────────────────────────────

fn type_text(
    app: &mut App,
    text: &str,
) {
    for c in text.chars() {
        app.handle_key(key_char(c));
    }
}

fn type_and_enter(
    app: &mut App,
    text: &str,
) {
    type_text(app, text);
    app.handle_key(key(KeyCode::Enter));
    // LLM chat is async (spinner path); settle so assertions see the reply.
    app.settle_pending_llm();
}

fn named_models() -> Vec<sui_llm::LlmModel> {
    sui_llm::LlmModel::from_toml(
        r#"
        [[model."gemma4"]]
        base_url = "http://localhost:11434"
        model = "gemma4:latest"

        [[model."gpt4o"]]
        base_url = "https://api.openai.com/v1"
        api_key = "test-key"
        model = "gpt-4o"
        "#,
    )
    .expect("models")
}

/// Enter shell mode (`!` on empty prompt) then type/run a command.
fn shell_and_enter(
    app: &mut App,
    command: &str,
) {
    app.handle_key(key_char('!'));
    assert_eq!(app.mode(), Mode::Shell);
    type_and_enter(app, command);
}

#[test]
fn slash_exit_quits() {
    let mut app = App::new();
    type_and_enter(&mut app, "/exit");
    assert!(app.should_quit);
    assert!(app.input.is_empty());
    assert!(app.messages.is_empty());
}

#[test]
fn slash_quit_quits() {
    let mut app = App::new();
    type_and_enter(&mut app, "/quit");
    assert!(app.should_quit);
    assert!(app.input.is_empty());
    assert!(app.messages.is_empty());
}

#[test]
fn slash_unknown_shows_error() {
    let mut app = App::new();
    type_and_enter(&mut app, "/foo");
    assert!(!app.should_quit);
    assert!(app.input.is_empty());
    assert_eq!(message_texts(&app), vec!["unknown command: /foo"]);
}

#[test]
fn slash_model_candidates_switch_active_model() {
    let mut app = App::new().with_models(named_models());
    assert_eq!(app.active_model, Some(0));

    type_text(&mut app, "/model");
    assert_eq!(app.slash_candidates.len(), 2);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.active_model, Some(1));
    assert_eq!(
        app.llm.as_ref().expect("active client").default_model(),
        "gpt-4o"
    );
    assert_eq!(message_texts(&app), vec!["model: gpt4o (gpt-4o)"]);
}

#[test]
fn slash_model_unknown_name_shows_error() {
    let mut app = App::new().with_models(named_models());
    type_and_enter(&mut app, "/model missing");
    assert_eq!(message_texts(&app), vec!["unknown model: missing"]);
}

#[test]
fn slash_model_without_named_models_shows_hint() {
    let mut app = App::new();
    type_and_enter(&mut app, "/model");
    assert_eq!(
        message_texts(&app),
        vec!["no models configured: add [[model.\"name\"]] entries to config.toml"]
    );
}

#[test]
fn with_llm_clears_switchable_model_state() {
    let config = sui_llm::LlmConfig::new("http://localhost:4000", "", "single").expect("config");
    let app = App::new()
        .with_models(named_models())
        .with_llm(sui_llm::LlmClient::new(&config));

    assert!(app.models.is_empty());
    assert_eq!(app.active_model, None);
    assert_eq!(
        app.llm.as_ref().expect("active client").default_model(),
        "single"
    );
}

#[test]
fn tab_on_model_candidates_cycles_without_replacing_command() {
    let mut app = App::new().with_models(named_models());
    type_text(&mut app, "/model");
    assert_eq!(app.input, "/model");
    assert_eq!(app.slash_selected, 0);

    app.handle_key(key(KeyCode::Tab));

    assert_eq!(app.input, "/model");
    assert_eq!(app.slash_selected, 1);
    assert_eq!(app.slash_candidates.len(), 2);
}

#[test]
fn normal_text_still_adds_to_messages() {
    let mut app = App::new();
    type_and_enter(&mut app, "hello");
    assert_eq!(
        message_texts(&app),
        vec![
            "hello",
            "llm not configured: set [llm] in config.toml or SUI_LLM_BASE_URL and SUI_LLM_MODEL (optional SUI_LLM_API_KEY)"
        ]
    );
}

#[test]
fn prompt_without_llm_does_not_grow_chat_history() {
    let mut app = App::new();
    type_and_enter(&mut app, "hello");
    assert!(app.chat_history.is_empty());
}

#[test]
fn bang_echo_adds_command_and_stdout() {
    let mut app = App::new();
    shell_and_enter(&mut app, "echo bang-app-ok");
    assert!(app.input.is_empty());
    assert_eq!(app.mode(), Mode::Prompt);
    assert!(
        app.messages
            .iter()
            .any(|m| m == &ScrollbackLine::Prompt("! echo bang-app-ok".into())),
        "messages={:?}",
        app.messages
    );
    assert!(
        app.messages.iter().any(|m| {
            matches!(
                m,
                ScrollbackLine::Ghost(text)
                    if text.contains("bang-app-ok") && text.as_str() != "! echo bang-app-ok"
            )
        }),
        "expected ghost stdout, messages={:?}",
        app.messages
    );
}

#[test]
fn bang_empty_shows_usage() {
    let mut app = App::new();
    app.handle_key(key_char('!'));
    app.handle_key(key(KeyCode::Enter));
    // Empty Enter does not run bash — stay in shell so the user can type.
    assert_eq!(app.mode(), Mode::Shell);
    assert_eq!(
        app.messages,
        vec![ScrollbackLine::Prompt("usage: <command>".into())]
    );
}

#[test]
fn bang_nonzero_exit_is_reported() {
    let mut app = App::new();
    shell_and_enter(&mut app, "exit 9");
    assert!(
        app.messages
            .iter()
            .any(|m| matches!(m, ScrollbackLine::Ghost(text) if text == "exit 9")),
        "messages={:?}",
        app.messages
    );
}

#[test]
fn shell_mode_entered_with_bang_on_empty_prompt() {
    let mut app = App::new();
    assert_eq!(app.mode(), Mode::Prompt);
    assert_eq!(app.prompt_title(), " prompt ");

    app.handle_key(key_char('!'));
    assert_eq!(app.mode(), Mode::Shell);
    assert!(app.input.is_empty());
    assert_eq!(app.prompt_title(), " shell ");

    type_text(&mut app, "echo");
    assert_eq!(app.input, "echo");
    assert_eq!(app.mode(), Mode::Shell);
}

#[test]
fn esc_leaves_shell_mode_without_quitting() {
    let mut app = App::new();
    app.handle_key(key_char('!'));
    type_text(&mut app, "pwd");
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.mode(), Mode::Prompt);
    assert!(app.input.is_empty());
    assert!(!app.should_quit);
    assert_eq!(app.prompt_title(), " prompt ");
}

#[test]
fn bang_mid_prompt_inserts_literally() {
    let mut app = App::new();
    type_text(&mut app, "hi");
    app.handle_key(key_char('!'));
    assert_eq!(app.mode(), Mode::Prompt);
    assert_eq!(app.input, "hi!");
}

#[test]
fn slash_in_shell_mode_is_literal_not_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('!'));
    type_text(&mut app, "/exit");
    assert_eq!(app.mode(), Mode::Shell);
    assert!(app.slash_candidates.is_empty());
    assert_eq!(app.input, "/exit");
}

#[test]
fn shell_mode_returns_to_prompt_after_command() {
    let mut app = App::new();
    shell_and_enter(&mut app, "echo done");
    assert_eq!(app.mode(), Mode::Prompt);
    assert!(app.input.is_empty());
    assert_eq!(app.prompt_title(), " prompt ");
}

#[test]
fn shell_mode_returns_to_prompt_after_bash_error() {
    let mut app = App::new();
    // Embedded newline is rejected by validate_single_line before spawn.
    app.handle_key(key_char('!'));
    // Insert via handle_key would type chars; inject invalid command directly.
    app.input = "echo a\necho b".into();
    app.cursor_position = app.input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.mode(), Mode::Prompt);
    assert!(
        message_texts(&app)
            .iter()
            .any(|m| m.starts_with("bash error:")),
        "messages={:?}",
        app.messages
    );
}

#[test]
fn empty_enter_does_nothing_with_slash() {
    // Already tested but making sure /-prefix doesn't interfere with empty handling
    let mut app = App::new();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.messages.is_empty());
    assert!(!app.should_quit);
}

#[test]
fn bare_slash_has_candidate_and_enter_executes_it() {
    // "/" alone shows all commands as candidates; Enter runs the first one.
    let mut app = App::new();
    type_and_enter(&mut app, "/");
    assert!(app.should_quit); // exit is the first candidate
    assert!(app.messages.is_empty());
}

#[test]
fn slash_candidates_populated_after_typing_slash() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    assert_eq!(app.slash_candidates.len(), 3); // exit + quit + model match ""

    app.handle_key(key_char('e'));
    assert_eq!(app.slash_candidates.len(), 1); // "exit" starts with "e"

    app.handle_key(key_char('x'));
    assert_eq!(app.slash_candidates.len(), 1); // "exit" starts with "ex"

    app.handle_key(key_char('z'));
    assert!(app.slash_candidates.is_empty()); // no command starts with "exz"
}

#[test]
fn slash_candidates_match_quit_prefix() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key_char('q'));
    assert_eq!(app.slash_candidates.len(), 1); // "quit" starts with "q"
}

#[test]
fn down_up_cycle_candidates_and_wrap() {
    let mut app = App::new();
    // Type "/" to populate candidates (exit, quit, model)
    app.handle_key(key_char('/'));
    assert_eq!(app.slash_selected, 0);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.slash_selected, 1);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.slash_selected, 2);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.slash_selected, 0);

    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.slash_selected, 2);
    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.slash_selected, 1);
}

#[test]
fn tab_on_bare_slash_autocompletes_first_candidate() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key(KeyCode::Tab));
    // Completes the first candidate; candidates then narrow to that name.
    assert_eq!(app.input, "/exit");
    assert_eq!(app.slash_candidates.len(), 1);
    assert_eq!(app.slash_selected, 0);
}

#[test]
fn backspace_narrows_candidates_again() {
    let mut app = App::new();
    // Type "/exz" — "exz" matches nothing
    app.handle_key(key_char('/'));
    app.handle_key(key_char('e'));
    app.handle_key(key_char('x'));
    app.handle_key(key_char('z'));
    assert!(app.slash_candidates.is_empty());

    // Backspace to "ex" — "exit" matches again
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.slash_candidates.len(), 1);
}

#[test]
fn delete_narrows_candidates() {
    let mut app = App::new();
    // Type "/xexit" then move cursor back and delete 'x'
    app.handle_key(key_char('/'));
    app.handle_key(key_char('x'));
    app.handle_key(key_char('e'));
    app.handle_key(key_char('x'));
    app.handle_key(key_char('i'));
    app.handle_key(key_char('t'));
    // Input is "/xexit", cursor at end. Candidates: empty ("xexit" doesn't match)
    assert!(app.slash_candidates.is_empty());

    // Delete the leading 'x' after '/': move left 5 times, then delete
    for _ in 0..5 {
        app.handle_key(key(KeyCode::Left));
    }
    // cursor is now after '/'
    app.handle_key(key(KeyCode::Delete));
    // Input is now "/exit"
    assert_eq!(app.slash_candidates.len(), 1);
}

#[test]
fn slash_candidates_cleared_on_enter() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    assert!(!app.slash_candidates.is_empty());
    app.handle_key(key(KeyCode::Enter));
    assert!(app.slash_candidates.is_empty());
    assert_eq!(app.slash_selected, 0);
}

#[test]
fn slash_candidates_cleared_when_not_starting_with_slash() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    assert!(!app.slash_candidates.is_empty());
    // Backspace to remove the '/'
    app.handle_key(key(KeyCode::Backspace));
    assert!(app.slash_candidates.is_empty());
}

#[test]
fn tab_does_nothing_without_candidates() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Tab));
    // No slash typed, candidates empty, Tab is a no-op
    assert_eq!(app.slash_candidates.len(), 0);
    assert_eq!(app.slash_selected, 0);
}

#[test]
fn slash_selected_resets_when_candidates_shrink() {
    let mut app = App::new();
    // Populate candidates with "/"
    app.handle_key(key_char('/'));
    assert_eq!(app.slash_selected, 0);
}

// ── Emacs-style key tests ────────────────────────────────────────

#[test]
fn ctrl_f_moves_cursor_right() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(ctrl_key('b'));
    assert_eq!(app.cursor_position, 0);
    app.handle_key(ctrl_key('f'));
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn ctrl_b_moves_cursor_left() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    assert_eq!(app.cursor_position, 2);
    app.handle_key(ctrl_key('b'));
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn ctrl_a_moves_to_start() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    app.handle_key(ctrl_key('a'));
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn ctrl_e_moves_to_end() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    app.handle_key(ctrl_key('a'));
    assert_eq!(app.cursor_position, 0);
    app.handle_key(ctrl_key('e'));
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn ctrl_h_deletes_before_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    app.handle_key(ctrl_key('h'));
    assert_eq!(app.input, "a");
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn ctrl_h_at_start_does_nothing() {
    let mut app = App::new();
    app.handle_key(key_char('x'));
    app.handle_key(key(KeyCode::Home));
    app.handle_key(ctrl_key('h'));
    assert_eq!(app.input, "x");
    assert_eq!(app.cursor_position, 0);
}

#[test]
fn ctrl_h_updates_slash_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    assert!(!app.slash_candidates.is_empty());
    app.handle_key(ctrl_key('h'));
    assert!(app.slash_candidates.is_empty());
    assert_eq!(app.input, "");
}

#[test]
fn ctrl_k_deletes_to_end() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('e'));
    app.handle_key(key_char('l'));
    app.handle_key(key_char('l'));
    app.handle_key(key_char('o'));
    // Move cursor to position 2 (between 'l' and 'l')
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor_position, 2);
    app.handle_key(ctrl_key('k'));
    assert_eq!(app.input, "he");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn ctrl_k_at_end_does_nothing() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    app.handle_key(ctrl_key('k'));
    assert_eq!(app.input, "hi");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn ctrl_d_deletes_char_at_cursor() {
    let mut app = App::new();
    app.handle_key(key_char('a'));
    app.handle_key(key_char('b'));
    app.handle_key(key_char('c'));
    // Move cursor left twice: position 1 (between 'a' and 'b')
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor_position, 1);
    app.handle_key(ctrl_key('d'));
    assert_eq!(app.input, "ac");
    assert_eq!(app.cursor_position, 1);
}

#[test]
fn ctrl_d_at_end_does_nothing() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    app.handle_key(ctrl_key('d'));
    assert_eq!(app.input, "hi");
    assert_eq!(app.cursor_position, 2);
}

#[test]
fn ctrl_d_updates_slash_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key_char('e'));
    app.handle_key(key_char('x'));
    app.handle_key(key_char('i'));
    app.handle_key(key_char('t'));
    // Input is "/exit", cursor at end. Move to start.
    app.handle_key(key(KeyCode::Home));
    assert_eq!(app.cursor_position, 0);
    app.handle_key(ctrl_key('d'));
    // Deletes the '/' — input becomes "exit", candidates cleared
    assert_eq!(app.input, "exit");
    assert!(app.slash_candidates.is_empty());
}

#[test]
fn ctrl_k_updates_slash_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key_char('e'));
    app.handle_key(key_char('x'));
    app.handle_key(key_char('i'));
    app.handle_key(key_char('t'));
    // Move cursor to after "/e"
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.input, "/exit");
    assert_eq!(app.cursor_position, 2);
    app.handle_key(ctrl_key('k'));
    assert_eq!(app.input, "/e");
    // Candidates should still include "exit" since "/e" matches
    assert_eq!(app.slash_candidates.len(), 1);
}

#[test]
fn ctrl_n_p_cycle_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    // With 3 candidates (exit, quit, model), cycling wraps
    assert_eq!(app.slash_selected, 0);
    app.handle_key(ctrl_key('n'));
    assert_eq!(app.slash_selected, 1);
    app.handle_key(ctrl_key('n'));
    assert_eq!(app.slash_selected, 2);
    app.handle_key(ctrl_key('n'));
    assert_eq!(app.slash_selected, 0);
    app.handle_key(ctrl_key('p'));
    assert_eq!(app.slash_selected, 2);
}

// ── Tab autocomplete tests ───────────────────────────────────────

#[test]
fn tab_autocompletes_to_exit() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key_char('e'));
    // candidates: [exit], selected: 0
    assert_eq!(app.input, "/e");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "/exit");
    assert_eq!(app.cursor_position, 5);
}

#[test]
fn tab_autocompletes_to_quit() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    app.handle_key(key_char('q'));
    assert_eq!(app.input, "/q");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "/quit");
    assert_eq!(app.cursor_position, 5);
}

#[test]
fn tab_no_candidates_does_nothing() {
    let mut app = App::new();
    app.handle_key(key_char('h'));
    app.handle_key(key_char('i'));
    app.handle_key(key(KeyCode::Tab));
    // No candidates, input unchanged
    assert_eq!(app.input, "hi");
}

// ── inline viewport tests ────────────────────────────────────────

#[test]
fn inline_height_is_prompt_only_by_default() {
    let app = App::new();
    assert_eq!(app.inline_height(80), PROMPT_HEIGHT);
    assert_eq!(app.viewport_height, PROMPT_HEIGHT);
}

#[test]
fn inline_height_grows_with_wrapped_input() {
    let mut app = App::new();
    app.input = "hello world".into();
    assert!(app.inline_height(10) > PROMPT_HEIGHT);
}

#[test]
fn inline_height_grows_with_slash_candidates() {
    let mut app = App::new();
    app.handle_key(key_char('/'));
    // "/" matches exit + quit + model — panel opens at a fixed budget, not per-row.
    assert_eq!(app.slash_candidates.len(), 3);
    let open_height = app.inline_height(80);
    assert!(open_height > PROMPT_HEIGHT);

    // Narrowing candidates must not resize on every keystroke.
    app.handle_key(key_char('e'));
    assert_eq!(app.slash_candidates.len(), 1);
    assert_eq!(app.inline_height(80), open_height);

    // Leaving slash mode collapses back to the prompt-only height.
    app.handle_key(key(KeyCode::Backspace));
    app.handle_key(key(KeyCode::Backspace));
    assert!(app.slash_candidates.is_empty());
    assert_eq!(app.inline_height(80), PROMPT_HEIGHT);
}

fn visible_row(
    backend: &TestBackend,
    y: u16,
) -> String {
    use ratatui::buffer::CellWidth;
    let area = backend.buffer().area;
    let mut out = String::new();
    let mut x = 0u16;
    while x < area.width {
        let cell = &backend.buffer()[(x, y)];
        let w = cell.cell_width().max(1);
        let sym = cell.symbol();
        if sym != " " {
            out.push_str(sym);
        }
        x = x.saturating_add(w);
    }
    out.trim_end().to_string()
}

fn infallible<T>(result: Result<T, core::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

fn inline_test_terminal(
    width: u16,
    height: u16,
    cursor_y: u16,
) -> Terminal<TestBackend> {
    let mut backend = TestBackend::new(width, height);
    infallible(backend.set_cursor_position(Position::new(0, cursor_y)));
    infallible(Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(PROMPT_HEIGHT),
        },
    ))
}

#[test]
fn flush_reply_wraps_long_line() {
    let mut terminal = inline_test_terminal(20, 10, 4);

    let mut app = App::new();
    app.add_reply("Great to see you! What can I help you with today?");
    infallible(app.flush_messages(&mut terminal));

    let row = |backend: &TestBackend, y: u16| -> String {
        let area = backend.buffer().area;
        (0..area.width)
            .map(|x| backend.buffer()[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    let backend = terminal.backend();
    assert_eq!(row(backend, 4), "Great to see you!");
    assert_eq!(row(backend, 5), "What can I help you");
    assert_eq!(row(backend, 6), "with today?");
    assert_eq!(terminal.get_frame().area().y, 7);
}

#[test]
fn flush_messages_writes_above_inline_viewport() {
    // height = 4 (cursor_y) + 6 (inserted rows) + 3 (viewport)
    let mut terminal = inline_test_terminal(40, 13, 4);

    let mut app = App::new().with_prompt_prefix("> ");
    app.add_message("hello");
    app.add_message("world");
    infallible(app.flush_messages(&mut terminal));

    assert_eq!(app.flushed_messages, 2);
    // Each prompt is padded with one blank row above and below → 3 rows each.
    // Messages were inserted above the viewport; viewport shifts down by 6.
    assert_eq!(terminal.get_frame().area().y, 10);

    let row = |backend: &TestBackend, y: u16| -> String {
        let area = backend.buffer().area;
        (0..area.width)
            .map(|x| backend.buffer()[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    let backend = terminal.backend();
    assert_eq!(row(backend, 5), "> hello");
    assert_eq!(row(backend, 8), "> world");
}

#[test]
fn flush_prompt_lines_are_padded_and_gray_highlighted() {
    // height = 4 (cursor_y) + 3 (padded prompt rows) + 3 (viewport)
    let mut terminal = inline_test_terminal(30, 10, 4);

    let mut app = App::new().with_prompt_prefix("> ");
    app.add_message("hello");
    infallible(app.flush_messages(&mut terminal));

    assert_eq!(terminal.get_frame().area().y, 7);
    let buf = terminal.backend().buffer();

    let bg = |y: u16| {
        buf[(0, y)]
            .style()
            .bg
            .expect("flushed prompt rows must carry a background color")
    };
    // Pad row above, prompt row, pad row below all share the same highlighted band.
    let back = Theme::DEFAULT.prompt_background;
    assert_eq!(bg(4), back);
    assert_eq!(bg(5), back);
    assert_eq!(bg(6), back);
    let symbol = |y: u16| buf[(0, y)].symbol();
    assert_eq!(symbol(5), ">");
}

#[test]
fn flush_ghost_messages_omit_prompt_prefix() {
    // height = 4 (cursor_y) + 4 (inserted rows) + 3 (viewport)
    let mut terminal = inline_test_terminal(40, 11, 4);

    let mut app = App::new().with_prompt_prefix("> ");
    app.add_message("! echo hi");
    app.add_ghost("hi");
    infallible(app.flush_messages(&mut terminal));

    // Padded prompt (3 rows) + ghost (1 row) = 4 inserted rows above the viewport.
    assert_eq!(terminal.get_frame().area().y, 8);

    let row = |backend: &TestBackend, y: u16| -> String {
        let area = backend.buffer().area;
        (0..area.width)
            .map(|x| backend.buffer()[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    let backend = terminal.backend();
    assert_eq!(row(backend, 5), "> ! echo hi");
    assert_eq!(row(backend, 7), "hi");
}

#[test]
fn teardown_after_pending_ghost_flush_parks_cursor_below_output() {
    // Mirrors App::run exit path: flush queued bang ghosts, then tear down.
    let mut terminal = inline_test_terminal(40, 12, 4);
    let mut app = App::new().with_prompt_prefix("> ");
    app.add_message("! echo hi");
    app.add_ghost("hi");
    app.add_ghost("bye");
    assert_eq!(app.flushed_messages, 0);

    infallible(app.flush_messages(&mut terminal));
    infallible(App::teardown_inline(&mut terminal));

    assert_eq!(app.flushed_messages, 3);
    // Padded prompt (3 rows) + 2 ghosts = 5 inserted rows below cursor_y (4).
    assert_eq!(
        infallible(terminal.get_cursor_position()),
        Position::new(0, 9),
        "cursor must sit just below flushed ghost lines"
    );

    let row = |backend: &TestBackend, y: u16| -> String {
        let area = backend.buffer().area;
        (0..area.width)
            .map(|x| backend.buffer()[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    let backend = terminal.backend();
    assert_eq!(row(backend, 5), "> ! echo hi");
    assert_eq!(row(backend, 7), "hi");
    assert_eq!(row(backend, 8), "bye");
}

#[test]
fn flush_messages_is_idempotent() {
    let mut terminal = inline_test_terminal(20, 8, 1);

    let mut app = App::new();
    app.add_message("once");
    infallible(app.flush_messages(&mut terminal));
    let y_after_first = terminal.get_frame().area().y;
    infallible(app.flush_messages(&mut terminal));
    assert_eq!(terminal.get_frame().area().y, y_after_first);
    assert_eq!(app.flushed_messages, 1);
}

#[test]
fn teardown_inline_clears_viewport_and_resets_cursor() {
    let mut terminal = inline_test_terminal(20, 10, 4);
    let viewport = terminal.get_frame().area();
    assert_eq!(viewport.y, 4);
    assert_eq!(viewport.height, PROMPT_HEIGHT);

    // Draw something into the viewport so we can tell clear worked.
    infallible(terminal.draw(|frame| {
        frame.render_widget(ratatui::widgets::Paragraph::new("leftover"), frame.area());
        frame.set_cursor_position(Position::new(frame.area().x + 3, frame.area().y + 1));
    }));

    infallible(App::teardown_inline(&mut terminal));

    let origin = Position::new(viewport.x, viewport.y);
    assert_eq!(infallible(terminal.get_cursor_position()), origin);

    // Cells in the former viewport should be empty after clear.
    let backend = terminal.backend();
    for y in viewport.top()..viewport.bottom() {
        for x in viewport.left()..viewport.right() {
            assert_eq!(
                backend.buffer()[(x, y)].symbol(),
                " ",
                "expected empty cell at ({x},{y})"
            );
        }
    }
}

fn pending_app(
    messages: impl IntoIterator<Item = LlmStreamMsg>
) -> (App, mpsc::Sender<LlmStreamMsg>) {
    let (tx, rx) = mpsc::channel();
    for msg in messages {
        assert!(tx.send(msg).is_ok());
    }
    let mut app = App::new();
    app.pending_llm = Some(PendingLlm::new(rx));
    (app, tx)
}

fn rendered_streaming_text(app: &App) -> String {
    app.streaming_reply
        .as_ref()
        .map(|reply| {
            reply
                .render_buffer_lines(80)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .unwrap_or_default()
}

#[test]
fn poll_pending_llm_drains_queued_chunks() {
    use sui_llm::ChatResponse;

    let (mut app, _tx) = pending_app([
        LlmStreamMsg::Chunk("foo".into()),
        LlmStreamMsg::Chunk("bar".into()),
        LlmStreamMsg::Done {
            response: ChatResponse::new("final-reply", "test-model"),
            history: Vec::new(),
        },
    ]);
    app.poll_pending_llm();

    assert!(app.pending_llm.is_none());
    assert_eq!(
        app.messages,
        vec![ScrollbackLine::Reply("final-reply".into())]
    );
}

#[test]
fn poll_pending_llm_stops_draining_after_terminal_message() {
    use sui_llm::ChatResponse;

    let (mut app, _tx) = pending_app([
        LlmStreamMsg::Chunk("before".into()),
        LlmStreamMsg::Done {
            response: ChatResponse::new("final-reply", "test-model"),
            history: Vec::new(),
        },
        LlmStreamMsg::Chunk("after-terminal".into()),
    ]);
    app.poll_pending_llm();

    assert!(app.pending_llm.is_none());
    assert_eq!(
        app.messages,
        vec![ScrollbackLine::Reply("final-reply".into())]
    );
}

#[test]
fn poll_pending_llm_drains_chunks_without_done_and_keeps_pending() {
    let (mut app, _tx) = pending_app([
        LlmStreamMsg::Chunk("foo".into()),
        LlmStreamMsg::Chunk("bar".into()),
    ]);
    app.poll_pending_llm();

    assert!(app.pending_llm.is_some());
    let rendered = rendered_streaming_text(&app);
    assert!(rendered.contains("foobar"), "rendered={rendered:?}");
}

#[test]
fn poll_pending_llm_budget_bounds_drain_and_preserves_order() {
    let over_budget = App::POLL_DRAIN_BUDGET + 3;
    let token = |i: usize| format!("chunk{i:03};");
    let (mut app, _tx) = pending_app((0..over_budget).map(|i| LlmStreamMsg::Chunk(token(i))));

    app.poll_pending_llm();
    assert!(
        app.pending_llm.is_some(),
        "budget exhaustion must leave the request pending"
    );

    let after_first = rendered_streaming_text(&app);
    for i in 0..App::POLL_DRAIN_BUDGET {
        let chunk = token(i);
        assert!(
            after_first.contains(&chunk),
            "expected budgeted chunk {chunk} after first poll: {after_first:?}"
        );
    }
    for i in App::POLL_DRAIN_BUDGET..over_budget {
        let chunk = token(i);
        assert!(
            !after_first.contains(&chunk),
            "chunk {chunk} must not be drained until the next poll: {after_first:?}"
        );
    }

    app.poll_pending_llm();
    assert!(app.pending_llm.is_some());
    let after_second = rendered_streaming_text(&app);
    let mut last = 0usize;
    for i in 0..over_budget {
        let chunk = token(i);
        let at = after_second
            .find(&chunk)
            .unwrap_or_else(|| panic!("missing chunk {chunk} after second poll: {after_second:?}"));
        assert!(at >= last, "chunk {chunk} out of order: {after_second:?}");
        last = at;
    }
}

#[test]
fn poll_pending_llm_handles_disconnect_after_draining_chunk() {
    let (tx, rx) = mpsc::channel();
    assert!(tx.send(LlmStreamMsg::Chunk("partial".into())).is_ok());
    drop(tx);

    let mut app = App::new();
    app.pending_llm = Some(PendingLlm::new(rx));
    app.poll_pending_llm();

    assert!(app.pending_llm.is_none());
    assert!(app.streaming_reply.is_none());
    assert_eq!(app.messages.len(), 1);
    assert!(
        matches!(
            &app.messages[0],
            ScrollbackLine::Prompt(text)
                if text.starts_with("llm error:") && text.contains("llm worker disconnected")
        ),
        "messages={:?}",
        app.messages
    );
}

#[test]
fn poll_pending_llm_drains_tool_messages_before_done() {
    use sui_llm::ChatResponse;

    let (mut app, _tx) = pending_app([
        LlmStreamMsg::Tool("tool bash: ok".into()),
        LlmStreamMsg::Chunk("done".into()),
        LlmStreamMsg::Done {
            response: ChatResponse::new("done", "test-model"),
            history: Vec::new(),
        },
    ]);
    app.poll_pending_llm();

    assert!(app.pending_llm.is_none());
    assert_eq!(
        app.messages,
        vec![
            ScrollbackLine::Ghost("tool bash: ok".into()),
            ScrollbackLine::Reply("done".into()),
        ]
    );
}

#[tokio::test]
async fn prompt_with_llm_appends_assistant_reply_and_history() {
    use serde_json::json;
    use sui_llm::{LlmClient, LlmConfig};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path},
    };

    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"proxy-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\\nworld\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({ "stream": true })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse.as_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let config = LlmConfig::new(server.uri(), "test-key", "proxy-model").expect("config");
    let mut app = App::new().with_llm(LlmClient::new(&config));
    type_and_enter(&mut app, "hi");

    assert_eq!(message_texts(&app), vec!["hi", "hello\nworld"]);
    assert!(
        matches!(
            &app.messages[1..],
            [ScrollbackLine::Reply(content)] if content == "hello\nworld"
        ),
        "expected reply markdown, messages={:?}",
        app.messages
    );
    assert_eq!(app.chat_history.len(), 2);
    assert_eq!(app.chat_history[0].content, "hi");
    assert_eq!(app.chat_history[1].content, "hello\nworld");
}

#[tokio::test]
async fn prompt_with_tools_runs_agent_loop() {
    use serde_json::json;
    use sui_llm::{LlmClient, LlmConfig};
    use sui_tools::ToolRegistry;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "proxy-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "agent-ok"
                },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = LlmConfig::new(server.uri(), "test-key", "proxy-model").expect("config");
    let mut app = App::new()
        .with_llm(LlmClient::new(&config))
        .with_tools(ToolRegistry::new());
    type_and_enter(&mut app, "hi");

    assert!(
        message_texts(&app).contains(&"agent-ok"),
        "messages={:?}",
        message_texts(&app)
    );
    assert!(
        app.chat_history.iter().any(|msg| msg.content == "agent-ok"),
        "history={:?}",
        app.chat_history
    );
}

#[tokio::test]
async fn prompt_llm_error_pops_failed_user_turn() {
    use serde_json::json;
    use sui_llm::{LlmClient, LlmConfig};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "message": "invalid key",
                "type": "auth_error",
                "param": null,
                "code": null
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = LlmConfig::new(server.uri(), "bad-key", "proxy-model").expect("config");
    let mut app = App::new().with_llm(LlmClient::new(&config));
    type_and_enter(&mut app, "hi");

    assert!(app.chat_history.is_empty());
    assert_eq!(message_texts(&app)[0], "hi");
    assert!(
        message_texts(&app)[1].starts_with("llm error:"),
        "messages={:?}",
        message_texts(&app)
    );
}

#[tokio::test]
async fn prompt_llm_waiting_shows_spinner_and_allows_typing() {
    use serde_json::json;
    use std::time::Duration;
    use sui_llm::{LlmClient, LlmConfig};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path},
    };

    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"proxy-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({ "stream": true })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_raw(sse.as_bytes(), "text/event-stream"),
        )
        // Quit abandons before the worker may have hit the server.
        .mount(&server)
        .await;

    let config = LlmConfig::new(server.uri(), "test-key", "proxy-model").expect("config");
    let mut app = App::new().with_llm(LlmClient::new(&config));
    type_text(&mut app, "hi");
    app.handle_key(key(KeyCode::Enter));

    assert!(app.pending_llm.is_some());
    let title = app.prompt_title_for_render();
    assert!(
        title.starts_with(" working ") && title.ends_with(' '),
        "title={title:?}"
    );

    app.handle_key(key_char('x'));
    assert_eq!(app.input, "x", "typing must work while waiting");
    assert!(!app.should_quit);

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input, "x", "enter must not submit while waiting");
    assert!(app.pending_llm.is_some());

    app.handle_key(ctrl_key('c'));
    assert!(app.should_quit);
    assert!(app.pending_llm.is_none());
    assert!(
        app.chat_history.is_empty(),
        "quit mid-request must roll back the user turn"
    );
    // Worker may still finish; we abandoned the receiver so no reply is applied.
    assert_eq!(message_texts(&app), vec!["hi"]);
}

#[test]
fn flush_japanese_prompt_renders_without_spurious_gaps() {
    let mut terminal = inline_test_terminal(40, 10, 4);
    let mut app = App::new();
    app.add_message("こんにちは！");
    infallible(app.flush_messages(&mut terminal));

    let backend = terminal.backend();
    let line = visible_row(backend, 5);
    assert!(
        !line.contains("こ ん"),
        "CJK must not have spaces between chars: {line:?}"
    );
    assert!(
        line.contains("こんにちは"),
        "expected intact Japanese: {line:?}"
    );
}

#[test]
fn render_japanese_streaming_without_spurious_gaps() {
    use crate::markdown::StreamingMarkdown;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        widgets::{Paragraph, Widget},
    };

    let mut stream = StreamingMarkdown::new();
    stream.push_delta("こんにちは！");
    stream.finish();

    let terminal = TestBackend::new(40, 5);
    let mut term = Terminal::new(terminal).unwrap();
    term.draw(|frame| {
        let text = ratatui::text::Text::from(stream.render_lines(40));
        Paragraph::new(text).render(frame.area(), frame.buffer_mut());
    })
    .unwrap();

    let backend = term.backend();
    let row = visible_row(backend, 0);
    assert!(
        !row.contains("こ ん"),
        "streaming CJK must not have gaps: {row:?}"
    );
}

// ── @-mention file picker tests ──────────────────────────────────

use crate::mention::{active_at_token, filter_files};

/// Seeds the workspace file cache so `@` tests avoid touching the filesystem.
fn seed_files(
    app: &mut App,
    files: &[&str],
) {
    app.file_cache = Some(files.iter().map(|s| (*s).to_owned()).collect());
}

#[test]
fn active_at_token_detects_bare_at() {
    assert_eq!(active_at_token("@", 1), Some((0, String::new())));
}

#[test]
fn active_at_token_reads_query_up_to_cursor() {
    assert_eq!(active_at_token("@src", 4), Some((0, "src".into())));
    // Cursor mid-token only captures text to its left.
    assert_eq!(active_at_token("@src", 2), Some((0, "s".into())));
}

#[test]
fn active_at_token_works_mid_prompt_after_space() {
    assert_eq!(
        active_at_token("explain @main", 13),
        Some((8, "main".into()))
    );
}

#[test]
fn active_at_token_ignores_email_like_at() {
    assert_eq!(active_at_token("foo@bar", 7), None);
}

#[test]
fn active_at_token_none_after_whitespace() {
    assert_eq!(active_at_token("@src file", 9), None);
}

#[test]
fn filter_files_substring_case_insensitive_and_capped() {
    let files = vec![
        "src/app.rs".to_owned(),
        "src/MAIN.rs".to_owned(),
        "README.md".to_owned(),
    ];
    assert_eq!(filter_files(&files, "main", 5), vec!["src/MAIN.rs"]);
    // Empty query keeps the first `limit` entries.
    assert_eq!(filter_files(&files, "", 2).len(), 2);
}

#[test]
fn typing_at_opens_file_candidates() {
    let mut app = App::new();
    seed_files(&mut app, &["src/main.rs", "src/app.rs", "README.md"]);
    app.handle_key(key_char('@'));
    assert_eq!(app.at_candidates.len(), 3);
    assert!(app.slash_candidates.is_empty());

    type_text(&mut app, "app");
    assert_eq!(app.at_candidates, vec!["src/app.rs".to_owned()]);
}

#[test]
fn tab_accepts_selected_file_mention() {
    let mut app = App::new();
    seed_files(&mut app, &["src/app.rs"]);
    type_text(&mut app, "@app");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "src/app.rs ");
    assert_eq!(app.cursor_position, app.input.chars().count());
    assert!(app.at_candidates.is_empty());
}

#[test]
fn enter_accepts_mention_without_submitting() {
    let mut app = App::new();
    seed_files(&mut app, &["src/app.rs"]);
    type_text(&mut app, "explain @app");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input, "explain src/app.rs ");
    assert!(
        app.messages.is_empty(),
        "Enter must not submit while accepting"
    );
}

#[test]
fn mention_accept_preserves_trailing_text() {
    let mut app = App::new();
    seed_files(&mut app, &["src/app.rs"]);
    type_text(&mut app, "a @app b");
    // Move cursor to just after "@app" (before " b").
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "a src/app.rs  b");
}

#[test]
fn mention_accept_replaces_whole_token_from_mid_cursor() {
    let mut app = App::new();
    seed_files(&mut app, &["src/app.rs"]);
    type_text(&mut app, "@app");
    // Cursor between "@a" and "pp"; accepting must still drop the "pp" suffix.
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor_position, 2);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "src/app.rs ");
}

#[test]
fn at_candidates_cleared_for_slash_input() {
    let mut app = App::new();
    seed_files(&mut app, &["src/app.rs"]);
    // "/" owns the panel: an @ later in a slash line must not open files.
    type_text(&mut app, "/x @app");
    assert!(app.at_candidates.is_empty());
}

#[test]
fn at_navigation_cycles_file_candidates() {
    let mut app = App::new();
    seed_files(&mut app, &["a.rs", "b.rs"]);
    app.handle_key(key_char('@'));
    assert_eq!(app.at_selected, 0);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.at_selected, 1);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.at_selected, 0);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.at_selected, 1);
}

#[test]
fn at_panel_grows_inline_height() {
    let mut app = App::new();
    seed_files(&mut app, &["a.rs"]);
    app.handle_key(key_char('@'));
    assert!(app.inline_height(80) > PROMPT_HEIGHT);
}

#[test]
fn at_is_literal_in_shell_mode() {
    let mut app = App::new();
    seed_files(&mut app, &["a.rs"]);
    app.handle_key(key_char('!'));
    type_text(&mut app, "@a");
    assert_eq!(app.mode(), Mode::Shell);
    assert!(app.at_candidates.is_empty());
    assert_eq!(app.input, "@a");
}
