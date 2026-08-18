//! Shell command execution for [`crate::Mode::Shell`].
//!
//! The TUI event loop is synchronous, so each command runs on a short-lived
//! worker thread with its own current-thread Tokio runtime. That avoids
//! `block_in_place` (which panics on current-thread runtimes used by many tests).
//!
//! Enter shell mode with `!` on an empty prompt; this module runs the command
//! once submitted. The app returns to [`crate::Mode::Prompt`] afterward.

use sui_tools::{CommandOutput, DEFAULT_RUN_TIMEOUT, ToolsError, run_line};

/// Formats [`CommandOutput`] into scrollback lines (no prompt prefix; [`crate::App`]
/// adds that when flushing).
pub fn format_output(out: &CommandOutput) -> Vec<String> {
    let mut lines = Vec::new();
    push_stream_lines(&mut lines, &out.stdout, None);
    push_stream_lines(&mut lines, &out.stderr, Some("[stderr] "));
    if out.timed_out {
        lines.push("command timed out".to_owned());
    } else if let Some(code) = out.code
        && code != 0
    {
        lines.push(format!("exit {code}"));
    }
    if out.truncated {
        lines.push("[output truncated]".to_owned());
    }
    lines
}

fn push_stream_lines(
    lines: &mut Vec<String>,
    text: &str,
    prefix: Option<&str>,
) {
    if text.is_empty() {
        return;
    }
    for line in text.lines() {
        match prefix {
            Some(prefix) => lines.push(format!("{prefix}{line}")),
            None => lines.push(line.to_owned()),
        }
    }
}

/// Runs `command` to completion (timeout [`DEFAULT_RUN_TIMEOUT`]).
///
/// # Errors
///
/// Propagates [`ToolsError`] from validation / spawn / wait, or a bash error if
/// the worker thread cannot build a runtime or panics.
pub fn run_blocking(command: &str) -> Result<CommandOutput, ToolsError> {
    let command = command.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                ToolsError::Bash(format!("failed to create tokio runtime: {error}"))
            })?;
        runtime.block_on(run_line(&command, None, DEFAULT_RUN_TIMEOUT))
    })
    .join()
    .unwrap_or_else(|_| Err(ToolsError::Bash("bash worker thread panicked".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stdout_stderr_and_exit() {
        let out = CommandOutput {
            stdout: "hello\n".into(),
            stderr: "warn\n".into(),
            code: Some(2),
            timed_out: false,
            truncated: false,
        };
        assert_eq!(
            format_output(&out),
            vec![
                "hello".to_owned(),
                "[stderr] warn".to_owned(),
                "exit 2".to_owned()
            ]
        );
    }

    #[test]
    fn format_timeout_and_truncation() {
        let out = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            code: None,
            timed_out: true,
            truncated: true,
        };
        assert_eq!(
            format_output(&out),
            vec![
                "command timed out".to_owned(),
                "[output truncated]".to_owned()
            ]
        );
    }

    #[test]
    fn format_success_is_silent_aside_from_stdout() {
        let out = CommandOutput {
            stdout: "ok".into(),
            stderr: String::new(),
            code: Some(0),
            timed_out: false,
            truncated: false,
        };
        assert_eq!(format_output(&out), vec!["ok".to_owned()]);
    }

    #[test]
    fn run_blocking_echo() {
        let out = run_blocking("echo bang-ok").expect("run");
        assert_eq!(out.code, Some(0));
        assert!(out.stdout.contains("bang-ok"), "stdout={:?}", out.stdout);
    }
}
