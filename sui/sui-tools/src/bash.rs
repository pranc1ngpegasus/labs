//! Async bash session primitives for agent tool use.
//!
//! # Threat model
//!
//! This is an **unsandboxed** local shell: any command the caller writes runs with
//! the host process credentials. There is no filesystem jail, no capability drop,
//! and no full environment scrubbing (only `BASH_ENV` / `ENV` are stripped at
//! spawn so `--noprofile --norc` cannot be bypassed). Callers (agents, CLIs) must
//! isolate the process (container, VM, dedicated user) if untrusted input can
//! reach this API. Full sandboxing is deferred.
//!
//! # I/O model
//!
//! Uses **pipes**, not a PTY: suitable for non-interactive command I/O and polling.
//! Interactive programs that require a TTY (e.g. password prompts, full-screen TUI)
//! will misbehave.
//!
//! # One-shot vs session
//!
//! Prefer [`run_line`] for a single command that should run to completion (e.g. TUI
//! bang `! cmd`). Use [`BashSession`] / [`crate::BashTool`] when the caller needs a
//! persistent shell across multiple writes.
//!
//! # Runtime
//!
//! [`BashSession::spawn`] starts Tokio tasks to read stdout/stderr. It must be
//! called from a process that has a Tokio runtime (or will enter one before the
//! readers need to run). Prefer constructing sessions inside `async` contexts.
//!
//! # Output buffers
//!
//! Each stream is capped at [`MAX_BUFFER_BYTES`]. Further bytes are dropped and
//! the next [`BashSession::drain`] / [`BashSession::read`] reports
//! [`SessionOutput::truncated`]. Callers should drain regularly.

use std::{process::Stdio, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
    time::timeout,
};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

use crate::ToolsError;

/// Soft cap on buffered stdout or stderr (per stream).
pub const MAX_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Max time to wait for reader tasks after the child exits / group is signalled.
const READER_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Process liveness from a non-blocking poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Child is still running.
    Running,
    /// Child has exited; `code` is `None` when killed by signal.
    Exited {
        /// Exit code when available.
        code: Option<i32>,
    },
}

impl ProcessState {
    /// Exit code when [`Self::Exited`], otherwise `None`.
    #[must_use]
    pub const fn exit_code(self) -> Option<i32> {
        match self {
            Self::Running => None,
            Self::Exited { code } => code,
        }
    }
}

/// Snapshot of buffered session output and process liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutput {
    /// Bytes read from stdout since the last drain (or since start if never drained).
    pub stdout: Vec<u8>,
    /// Bytes read from stderr since the last drain (or since start if never drained).
    pub stderr: Vec<u8>,
    /// Current process state.
    pub state: ProcessState,
    /// True when a stream hit [`MAX_BUFFER_BYTES`] and further bytes were dropped.
    pub truncated: bool,
}

/// Result of [`BashSession::wait_timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct WaitOutcome {
    /// Exit code when available (`None` if killed by signal or still unknown).
    pub code: Option<i32>,
    /// True when the wait deadline elapsed and the session was killed.
    pub timed_out: bool,
}

struct StreamBuffer {
    data: Vec<u8>,
    truncated: bool,
}

impl StreamBuffer {
    const fn new() -> Self {
        Self {
            data: Vec::new(),
            truncated: false,
        }
    }

    fn append(
        &mut self,
        chunk: &[u8],
    ) {
        if self.data.len() >= MAX_BUFFER_BYTES {
            self.truncated = true;
            return;
        }
        let room = MAX_BUFFER_BYTES - self.data.len();
        if chunk.len() > room {
            self.data.extend_from_slice(&chunk[..room]);
            self.truncated = true;
        } else {
            self.data.extend_from_slice(chunk);
        }
    }

    fn snapshot(&self) -> (Vec<u8>, bool) {
        (self.data.clone(), self.truncated)
    }

    fn take(&mut self) -> (Vec<u8>, bool) {
        let truncated = self.truncated;
        self.truncated = false;
        (std::mem::take(&mut self.data), truncated)
    }
}

/// A non-blocking bash process with piped stdin/stdout/stderr.
///
/// Background tasks continuously append child output into internal buffers.
/// Call [`Self::drain`] / [`Self::read`] to observe them.
pub struct BashSession {
    child: Child,
    /// Process group id captured at spawn (`==` leader pid with `process_group(0)`).
    /// Retained after reap so [`Self::kill`] / [`Drop`] can still signal orphans.
    #[cfg(unix)]
    pgid: Option<u32>,
    stdin: Option<ChildStdin>,
    stdout_buf: Arc<Mutex<StreamBuffer>>,
    stderr_buf: Arc<Mutex<StreamBuffer>>,
    reader_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl BashSession {
    /// Spawns a non-interactive `bash` session in `cwd` (defaults to process cwd).
    ///
    /// Invokes `bash --noprofile --norc` so user/system rc files are not sourced.
    /// `BASH_ENV` and `ENV` are removed from the child environment so those hooks
    /// cannot bypass `--norc`. On Unix, the child is placed in its own process
    /// group; [`Self::kill`] / [`Drop`] signal that group via `kill(2)`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] when the process cannot be spawned or pipes
    /// cannot be taken.
    pub fn spawn(cwd: Option<&std::path::Path>) -> Result<Self, ToolsError> {
        let mut command = Command::new("bash");
        command
            .args(["--noprofile", "--norc"])
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        {
            // Own process group: avoids sharing the agent's foreground group.
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|error| ToolsError::Bash(format!("failed to spawn bash: {error}")))?;

        #[cfg(unix)]
        let pgid = child.id();

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ToolsError::Bash("missing stdin pipe".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolsError::Bash("missing stdout pipe".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolsError::Bash("missing stderr pipe".into()))?;

        let stdout_buf = Arc::new(Mutex::new(StreamBuffer::new()));
        let stderr_buf = Arc::new(Mutex::new(StreamBuffer::new()));

        let stdout_target = Arc::clone(&stdout_buf);
        let stderr_target = Arc::clone(&stderr_buf);

        let stdout_handle = tokio::spawn(async move {
            let mut buf = [0_u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = stdout_target.lock().await;
                        guard.append(&buf[..n]);
                    },
                }
            }
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = [0_u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = stderr_target.lock().await;
                        guard.append(&buf[..n]);
                    },
                }
            }
        });

        Ok(Self {
            child,
            #[cfg(unix)]
            pgid,
            stdin: Some(stdin),
            stdout_buf,
            stderr_buf,
            reader_handles: vec![stdout_handle, stderr_handle],
        })
    }

    /// Writes raw bytes to the session stdin.
    ///
    /// **Trusted / raw:** this does **not** validate or sanitize `data`. Embedded
    /// newlines, NULs, and shell metacharacters are forwarded verbatim. Prefer
    /// [`Self::write_line`] for untrusted single-line commands.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] when stdin is closed or the write fails.
    pub async fn write(
        &mut self,
        data: &[u8],
    ) -> Result<(), ToolsError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| ToolsError::Bash("stdin is closed".into()))?;
        stdin
            .write_all(data)
            .await
            .map_err(|error| ToolsError::Bash(format!("stdin write failed: {error}")))?;
        stdin
            .flush()
            .await
            .map_err(|error| ToolsError::Bash(format!("stdin flush failed: {error}")))?;
        Ok(())
    }

    /// Writes `line` followed by a newline.
    ///
    /// Rejects embedded NUL, CR, or LF so a single logical line cannot smuggle
    /// additional shell statements through [`Self::write_line`].
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::InvalidArgs`] when `line` contains `\0`, `\n`, or
    /// `\r`, otherwise same as [`Self::write`].
    pub async fn write_line(
        &mut self,
        line: &str,
    ) -> Result<(), ToolsError> {
        validate_single_line(line)?;
        let mut payload = line.as_bytes().to_vec();
        payload.push(b'\n');
        self.write(&payload).await
    }

    /// Returns buffered output without clearing it, plus a non-blocking exit poll.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if polling the child fails.
    pub async fn read(&mut self) -> Result<SessionOutput, ToolsError> {
        let (stdout, stdout_trunc) = self.stdout_buf.lock().await.snapshot();
        let (stderr, stderr_trunc) = self.stderr_buf.lock().await.snapshot();
        let state = self.poll()?;
        Ok(SessionOutput {
            stdout,
            stderr,
            state,
            truncated: stdout_trunc || stderr_trunc,
        })
    }

    /// Takes and clears buffered stdout/stderr, plus a non-blocking exit poll.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if polling the child fails.
    pub async fn drain(&mut self) -> Result<SessionOutput, ToolsError> {
        let (stdout, stdout_trunc) = self.stdout_buf.lock().await.take();
        let (stderr, stderr_trunc) = self.stderr_buf.lock().await.take();
        let state = self.poll()?;
        Ok(SessionOutput {
            stdout,
            stderr,
            state,
            truncated: stdout_trunc || stderr_trunc,
        })
    }

    /// Non-blocking process poll.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if `try_wait` fails.
    pub fn poll(&mut self) -> Result<ProcessState, ToolsError> {
        self.child
            .try_wait()
            .map_err(|error| ToolsError::Bash(format!("try_wait failed: {error}")))?
            .map_or(Ok(ProcessState::Running), |status| {
                Ok(ProcessState::Exited {
                    code: status.code(),
                })
            })
    }

    /// Waits for the process to exit (no timeout), then finishes reader tasks.
    ///
    /// After exit, any leftover process-group members are signalled so inherited
    /// pipes close; readers join with a short timeout (then abort) to avoid hangs.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if waiting fails.
    pub async fn wait(&mut self) -> Result<Option<i32>, ToolsError> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|error| ToolsError::Bash(format!("wait failed: {error}")))?;
        self.cleanup_group_and_readers().await;
        Ok(status.code())
    }

    /// Waits up to `duration` for the process to exit.
    ///
    /// On timeout the process group is signalled and readers are allowed to
    /// finish draining the pipes (they see EOF once the group is dead), so any
    /// partial output survives. [`WaitOutcome::timed_out`] is set. Callers
    /// should [`Self::drain`] afterward to collect that output (this method does
    /// not clear buffers).
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] on I/O or kill failure (not on timeout alone).
    pub async fn wait_timeout(
        &mut self,
        duration: Duration,
    ) -> Result<WaitOutcome, ToolsError> {
        match timeout(duration, self.child.wait()).await {
            Ok(Ok(status)) => {
                self.cleanup_group_and_readers().await;
                Ok(WaitOutcome {
                    code: status.code(),
                    timed_out: false,
                })
            },
            Ok(Err(error)) => Err(ToolsError::Bash(format!("wait failed: {error}"))),
            Err(_) => {
                self.kill_session().await?;
                let code = match self.poll()? {
                    ProcessState::Exited { code } => code,
                    ProcessState::Running => None,
                };
                Ok(WaitOutcome {
                    code,
                    timed_out: true,
                })
            },
        }
    }

    /// Closes stdin so the shell can see EOF.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if closing/flushing fails.
    pub async fn close_stdin(&mut self) -> Result<(), ToolsError> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .shutdown()
                .await
                .map_err(|error| ToolsError::Bash(format!("stdin shutdown failed: {error}")))?;
        }
        Ok(())
    }

    /// Kills the bash session (process group on Unix) and lets reader tasks
    /// drain any remaining pipe output before stopping.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::Bash`] if the kill syscall fails.
    pub async fn kill(&mut self) -> Result<(), ToolsError> {
        self.kill_session().await
    }

    async fn kill_session(&mut self) -> Result<(), ToolsError> {
        // Group first so orphan grandchildren release inherited pipes.
        self.kill_process_group()?;
        // Let readers consume anything still in the pipes: after the group is
        // dead the write ends close, readers see EOF, and partial output that
        // was written but not yet read survives. Aborting the readers here
        // would discard it (a timeout `wait` would then report empty stdout).
        self.finish_readers().await;
        match self.child.kill().await {
            Ok(()) => Ok(()),
            // Already reaped after group kill / wait.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(ToolsError::Bash(format!("kill failed: {error}"))),
        }
    }

    async fn cleanup_group_and_readers(&mut self) {
        // Best-effort: group may already be empty after a clean exit.
        let _ = self.kill_process_group();
        self.finish_readers().await;
    }

    fn kill_process_group(&self) -> Result<(), ToolsError> {
        #[cfg(unix)]
        {
            let Some(pgid) = self.pgid else {
                return Ok(());
            };
            // Signal the whole group directly via the kill(2) syscall instead of
            // shelling out to `/bin/kill`, which is absent in hermetic sandboxes
            // (e.g. Nix builds) and minimal containers.
            let group = Pid::from_raw(pgid.cast_signed())
                .ok_or_else(|| ToolsError::Bash(format!("invalid process group id: {pgid}")))?;
            // ESRCH means the group already exited — nothing left to kill.
            match kill_process_group(group, Signal::KILL) {
                Ok(()) | Err(Errno::SRCH) => Ok(()),
                Err(error) => Err(ToolsError::Bash(format!(
                    "kill process group {pgid} failed: {error}"
                ))),
            }
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }

    async fn finish_readers(&mut self) {
        let handles = std::mem::take(&mut self.reader_handles);
        for mut handle in handles {
            tokio::select! {
                _ = &mut handle => {},
                () = tokio::time::sleep(READER_JOIN_TIMEOUT) => {
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
    }
}

impl Drop for BashSession {
    fn drop(&mut self) {
        for handle in self.reader_handles.drain(..) {
            handle.abort();
        }
        // kill_on_drop only signals the leader; orphans in the group need this.
        let _ = self.kill_process_group();
    }
}

/// Captured result of [`run_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct CommandOutput {
    /// stdout bytes decoded lossily as UTF-8.
    pub stdout: String,
    /// stderr bytes decoded lossily as UTF-8.
    pub stderr: String,
    /// Exit code when available (`None` if killed by signal or still unknown).
    pub code: Option<i32>,
    /// True when the wait deadline elapsed and the session was killed.
    pub timed_out: bool,
    /// True when a stream hit [`MAX_BUFFER_BYTES`] and further bytes were dropped.
    pub truncated: bool,
}

/// Default wall-clock budget for [`run_line`] when callers omit an explicit timeout.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs a single shell command to completion in a fresh bash session.
///
/// Spawns [`BashSession`], writes `command` as one line, closes stdin (EOF),
/// waits up to `timeout`, then drains buffered output. Intended for bang-style
/// (`! cmd`) one-shots — not a substitute for a long-lived [`BashSession`].
///
/// `command` must be a single line (no NUL, CR, or LF).
///
/// # Errors
///
/// Returns [`ToolsError::InvalidArgs`] when `command` contains NUL/CR/LF,
/// otherwise any spawn / I/O / wait error.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use sui_tools::run_line;
///
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
/// # runtime.block_on(async {
/// let out = run_line("echo hi", None, Duration::from_secs(3))
///     .await
///     .expect("run");
/// assert!(out.stdout.contains("hi"));
/// assert_eq!(out.code, Some(0));
/// # });
/// ```
pub async fn run_line(
    command: &str,
    cwd: Option<&std::path::Path>,
    timeout: Duration,
) -> Result<CommandOutput, ToolsError> {
    validate_single_line(command)?;
    let mut session = BashSession::spawn(cwd)?;
    session.write_line(command).await?;
    session.close_stdin().await?;
    let outcome = session.wait_timeout(timeout).await?;
    let drained = session.drain().await?;
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&drained.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&drained.stderr).into_owned(),
        code: outcome.code,
        timed_out: outcome.timed_out,
        truncated: drained.truncated,
    })
}

/// Rejects strings that would become multi-line or NUL-terminated shell input.
pub(crate) fn validate_single_line(line: &str) -> Result<(), ToolsError> {
    if line.as_bytes().contains(&0) {
        return Err(ToolsError::InvalidArgs(
            "bash command must not contain NUL bytes".into(),
        ));
    }
    if line.contains('\n') || line.contains('\r') {
        return Err(ToolsError::InvalidArgs(
            "bash command must be a single line (no CR/LF)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::process::test_kill_process;

    #[tokio::test]
    async fn echo_roundtrip() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("echo hello-sui-tools").await?;
        session.write_line("exit").await?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut stdout = String::new();
        loop {
            let out = session.drain().await?;
            stdout.push_str(&String::from_utf8_lossy(&out.stdout));
            if !matches!(out.state, ProcessState::Running) || stdout.contains("hello-sui-tools") {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert!(stdout.contains("hello-sui-tools"), "stdout was: {stdout:?}");
        let outcome = session.wait_timeout(Duration::from_secs(2)).await?;
        assert!(!outcome.timed_out);
        assert_eq!(outcome.code, Some(0));
        Ok(())
    }

    #[tokio::test]
    async fn sleep_stays_running_until_wait() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("sleep 0.2; echo done; exit").await?;

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(matches!(session.poll()?, ProcessState::Running));

        let outcome = session.wait_timeout(Duration::from_secs(3)).await?;
        assert!(!outcome.timed_out);
        assert_eq!(outcome.code, Some(0));

        let out = session.drain().await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("done"), "stdout was: {stdout:?}");
        Ok(())
    }

    #[tokio::test]
    async fn stderr_is_captured() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session
            .write_line("echo err-msg 1>&2; echo out-msg; exit")
            .await?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut stdout = String::new();
        let mut stderr = String::new();
        loop {
            let out = session.drain().await?;
            stdout.push_str(&String::from_utf8_lossy(&out.stdout));
            stderr.push_str(&String::from_utf8_lossy(&out.stderr));
            if !matches!(out.state, ProcessState::Running)
                || (stdout.contains("out-msg") && stderr.contains("err-msg"))
            {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(stdout.contains("out-msg"), "stdout={stdout:?}");
        assert!(stderr.contains("err-msg"), "stderr={stderr:?}");
        let outcome = session.wait_timeout(Duration::from_secs(2)).await?;
        assert_eq!(outcome.code, Some(0));
        Ok(())
    }

    #[tokio::test]
    async fn close_stdin_lets_shell_exit() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.close_stdin().await?;
        let outcome = session.wait_timeout(Duration::from_secs(3)).await?;
        assert_eq!(outcome.code, Some(0));
        Ok(())
    }

    #[tokio::test]
    async fn kill_terminates_running_session() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("sleep 30").await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(session.poll()?, ProcessState::Running));
        session.kill().await?;
        assert!(matches!(session.poll()?, ProcessState::Exited { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn wait_timeout_kills_hung_child() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("sleep 30").await?;
        let outcome = session.wait_timeout(Duration::from_millis(100)).await?;
        assert!(outcome.timed_out, "expected timed_out");
        assert!(matches!(session.poll()?, ProcessState::Exited { .. }));
        // Partial drain after timeout should succeed (buffers may be empty).
        let _ = session.drain().await?;
        Ok(())
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_reported() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("exit 7").await?;
        let outcome = session.wait_timeout(Duration::from_secs(3)).await?;
        assert_eq!(outcome.code, Some(7));
        Ok(())
    }

    #[tokio::test]
    async fn write_line_rejects_newline_nul_and_cr() {
        let mut session = BashSession::spawn(None).expect("spawn");
        let err = session
            .write_line("echo a\necho b")
            .await
            .expect_err("newline");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));

        let err = session.write_line("echo\0oops").await.expect_err("nul");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));

        let err = session.write_line("echo a\recho b").await.expect_err("cr");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn spawn_with_cwd() -> Result<(), ToolsError> {
        let dir = std::env::temp_dir().join(format!(
            "sui-tools-bash-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).map_err(|e| ToolsError::io(&dir, e))?;
        let mut session = BashSession::spawn(Some(&dir))?;
        session.write_line("pwd; exit").await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut stdout = String::new();
        loop {
            let out = session.drain().await?;
            stdout.push_str(&String::from_utf8_lossy(&out.stdout));
            if !matches!(out.state, ProcessState::Running)
                || stdout.contains(dir.to_string_lossy().as_ref())
            {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            stdout.contains(dir.to_string_lossy().as_ref()),
            "stdout={stdout:?}"
        );
        let _ = session.wait_timeout(Duration::from_secs(2)).await;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn drop_kills_process_group_orphans() -> Result<(), ToolsError> {
        // Capture the background PID explicitly — nix sandboxes often lack `pgrep`.
        let mut session = BashSession::spawn(None)?;
        session
            .write_line("sleep 999999 & echo PID:$!; exit")
            .await?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut stdout = String::new();
        loop {
            let out = session.drain().await?;
            stdout.push_str(&String::from_utf8_lossy(&out.stdout));
            if stdout.contains("PID:") || !matches!(out.state, ProcessState::Running) {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let raw: u32 = stdout
            .lines()
            .find_map(|line| line.strip_prefix("PID:"))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolsError::Bash(format!("missing PID in stdout: {stdout:?}")))?
            .parse()
            .map_err(|e| ToolsError::Bash(format!("invalid PID in stdout: {e}")))?;

        // Do not call wait() (it also cleans the group); Drop must do the work.
        drop(session);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let pid = Pid::from_raw(raw.cast_signed())
            .ok_or_else(|| ToolsError::Bash(format!("invalid PID: {raw}")))?;
        let alive = test_kill_process(pid).is_ok();
        assert!(!alive, "orphan sleep pid {raw} still alive after Drop");
        Ok(())
    }

    #[tokio::test]
    async fn wait_with_background_job_does_not_hang() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        session.write_line("sleep 30 & echo BG_OK; exit").await?;
        let outcome = timeout(Duration::from_secs(3), session.wait())
            .await
            .map_err(|_| ToolsError::Bash("wait hung on background job".into()))??;
        assert_eq!(outcome, Some(0));
        let out = session.drain().await?;
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("BG_OK"),
            "stdout={:?}",
            out.stdout
        );
        Ok(())
    }

    #[tokio::test]
    async fn buffer_truncation_is_reported() -> Result<(), ToolsError> {
        let mut session = BashSession::spawn(None)?;
        // Emit slightly more than the cap; `yes` is fast enough for a unit test.
        session
            .write_line(&format!(
                "dd if=/dev/zero bs=4096 count={} 2>/dev/null | tr '\\0' 'x'; exit",
                (MAX_BUFFER_BYTES / 4096) + 2
            ))
            .await?;
        let outcome = session.wait_timeout(Duration::from_secs(30)).await?;
        assert!(!outcome.timed_out, "dd should finish");
        let out = session.drain().await?;
        assert!(out.truncated, "expected truncated flag");
        assert!(out.stdout.len() <= MAX_BUFFER_BYTES);
        Ok(())
    }

    #[test]
    fn validate_single_line_ok() {
        assert!(validate_single_line("echo hi").is_ok());
    }

    #[test]
    fn validate_single_line_rejects_cr() {
        assert!(matches!(
            validate_single_line("echo a\rb"),
            Err(ToolsError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn run_line_echo() -> Result<(), ToolsError> {
        let out = run_line("echo run-line-ok", None, Duration::from_secs(3)).await?;
        assert!(!out.timed_out);
        assert_eq!(out.code, Some(0));
        assert!(
            out.stdout.contains("run-line-ok"),
            "stdout={:?}",
            out.stdout
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_line_captures_stderr_and_nonzero() -> Result<(), ToolsError> {
        let out = run_line("echo err-line 1>&2; exit 3", None, Duration::from_secs(3)).await?;
        assert_eq!(out.code, Some(3));
        assert!(out.stderr.contains("err-line"), "stderr={:?}", out.stderr);
        Ok(())
    }

    #[tokio::test]
    async fn run_line_timeout() -> Result<(), ToolsError> {
        let out = run_line("sleep 30", None, Duration::from_millis(100)).await?;
        assert!(out.timed_out);
        Ok(())
    }

    #[tokio::test]
    async fn run_line_rejects_newline() {
        let err = run_line("echo a\necho b", None, Duration::from_secs(1))
            .await
            .expect_err("newline");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));
    }
}
