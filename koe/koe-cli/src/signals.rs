//! Signal handling for CLI recording sessions.
//!
//! | Signal | Behavior |
//! |--------|----------|
//! | SIGINT (1st) | Graceful stop (exit code 5 after finalize). |
//! | SIGINT (2nd within 2s) | Force stop / hard exit during in-flight stop. |
//! | SIGTERM | Same as first SIGINT. |
//! | SIGUSR1 | Toggle pause / resume. |
//!
//! Double-tap escalation while `stop().await` holds `&mut RecordingPipeline`
//! cannot call `force_stop` concurrently — the watchdog uses `process::exit(5)`
//! as the only safe escape hatch (see `koe_core` shutdown docs). When the second
//! SIGINT is observed *before* `stop` starts, the CLI prefers `force_stop`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How recording should end after an interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// Finalize transcription, flush outputs, then exit 5.
    Graceful,
    /// Skip ASR finalize; still finalize the audio container; exit 5.
    Force,
}

/// Events delivered while a recording is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalEvent {
    /// SIGINT / SIGTERM requesting session end.
    Stop(StopSignal),
    /// SIGUSR1 — toggle pause/resume on the pipeline.
    TogglePause,
}

/// Double-tap window for second SIGINT → force.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_secs(2);

/// Process exit code for interrupt (matches CLI spec / `MainError::Interrupted`).
const EXIT_INTERRUPTED: i32 = 5;

/// Tracks whether a first interrupt has armed the force window.
///
/// Pure state — no runtime. [`SignalListener`] owns the 2s reset timer.
#[derive(Debug, Default)]
pub struct InterruptGate {
    armed: Arc<AtomicBool>,
}

impl InterruptGate {
    /// Creates a gate with a shared arm flag.
    #[must_use]
    pub fn new() -> Self {
        Self {
            armed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Shared arm flag for the in-flight-stop force-exit watchdog.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.armed)
    }

    /// Records SIGINT. Second press while armed → [`StopSignal::Force`].
    #[must_use]
    pub fn on_sigint(&self) -> StopSignal {
        if self.armed.swap(true, Ordering::Relaxed) {
            StopSignal::Force
        } else {
            StopSignal::Graceful
        }
    }

    /// Records SIGTERM (always graceful; still arms the force window for SIGINT).
    #[must_use]
    pub fn on_sigterm(&self) -> StopSignal {
        self.armed.store(true, Ordering::Relaxed);
        StopSignal::Graceful
    }
}

/// Async listener for recording-related Unix signals (and Ctrl-C elsewhere).
pub struct SignalListener {
    gate: InterruptGate,
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigusr1: tokio::signal::unix::Signal,
    #[cfg(not(unix))]
    _non_unix: (),
}

impl SignalListener {
    /// Installs handlers. Fails if the OS rejects registration.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when signal registration fails.
    pub fn install() -> Result<Self, String> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let sigint =
                signal(SignalKind::interrupt()).map_err(|err| format!("SIGINT handler: {err}"))?;
            let sigterm =
                signal(SignalKind::terminate()).map_err(|err| format!("SIGTERM handler: {err}"))?;
            let sigusr1 = signal(SignalKind::from_raw(sigusr1_raw()))
                .map_err(|err| format!("SIGUSR1 handler: {err}"))?;
            Ok(Self {
                gate: InterruptGate::new(),
                sigint,
                sigterm,
                sigusr1,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                gate: InterruptGate::new(),
                _non_unix: (),
            })
        }
    }

    /// Shared arm flag — set after the first interrupt.
    #[must_use]
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.gate.flag()
    }

    /// Waits for the next recording-related signal.
    ///
    /// Closed signal streams (`recv` → `None`) are ignored so a driver teardown
    /// cannot be mistaken for Ctrl-C.
    pub async fn recv(&mut self) -> SignalEvent {
        #[cfg(unix)]
        {
            loop {
                tokio::select! {
                    Some(()) = self.sigint.recv() => {
                        let kind = self.gate.on_sigint();
                        if kind == StopSignal::Graceful {
                            self.spawn_double_tap_reset();
                        }
                        return SignalEvent::Stop(kind);
                    }
                    Some(()) = self.sigterm.recv() => {
                        let kind = self.gate.on_sigterm();
                        self.spawn_double_tap_reset();
                        return SignalEvent::Stop(kind);
                    }
                    Some(()) = self.sigusr1.recv() => {
                        return SignalEvent::TogglePause;
                    }
                    else => {
                        // All signal drivers closed — park forever.
                        std::future::pending::<()>().await;
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {
                    let kind = self.gate.on_sigint();
                    if kind == StopSignal::Graceful {
                        self.spawn_double_tap_reset();
                    }
                    SignalEvent::Stop(kind)
                },
                // ctrl_c install/listen failure: treat as graceful stop request
                // so the session still finalizes rather than hanging forever.
                Err(_) => SignalEvent::Stop(StopSignal::Graceful),
            }
        }
    }

    /// Waits until an interrupt that should abort an in-flight graceful stop.
    ///
    /// Ignores SIGUSR1. Used for a non-blocking pending-interrupt poll.
    pub async fn recv_force_during_stop(&mut self) {
        loop {
            match self.recv().await {
                SignalEvent::Stop(_) => return,
                SignalEvent::TogglePause => {},
            }
        }
    }

    fn spawn_double_tap_reset(&self) {
        let flag = self.gate.flag();
        tokio::spawn(async move {
            tokio::time::sleep(DOUBLE_TAP_WINDOW).await;
            flag.store(false, Ordering::Relaxed);
        });
    }
}

/// Spawns a watchdog that hard-exits on a second SIGINT while graceful stop runs.
///
/// Needed because `pipeline.stop().await` holds `&mut RecordingPipeline`, so an
/// in-flight escalate to `force_stop` is impossible. Tokio allows a second
/// `signal(SIGINT)` registration alongside [`SignalListener`].
pub fn spawn_force_exit_watchdog(armed: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        tokio::spawn(async move {
            let Ok(mut sigint) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            else {
                return;
            };
            loop {
                if sigint.recv().await.is_none() {
                    return;
                }
                if armed.load(Ordering::Relaxed) {
                    eprintln!("Force exit — second interrupt during shutdown");
                    // In-flight `stop().await` holds `&mut self`; cannot escalate
                    // to `force_stop`. Hard exit matches the CLI signal contract.
                    std::process::exit(EXIT_INTERRUPTED);
                }
            }
        });
    }
    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                if armed.load(Ordering::Relaxed) {
                    eprintln!("Force exit — second interrupt during shutdown");
                    std::process::exit(EXIT_INTERRUPTED);
                }
            }
        });
    }
}

/// Raw `SIGUSR1` number (avoids a `libc` dependency).
#[cfg(unix)]
const fn sigusr1_raw() -> std::os::raw::c_int {
    // Darwin / BSD: 30, Linux and most others: 10.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        30
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sigint_is_graceful_second_is_force() {
        let gate = InterruptGate::new();
        assert_eq!(gate.on_sigint(), StopSignal::Graceful);
        assert!(gate.flag().load(Ordering::Relaxed));
        assert_eq!(gate.on_sigint(), StopSignal::Force);
    }

    #[test]
    fn sigterm_is_always_graceful_but_arms_force_window() {
        let gate = InterruptGate::new();
        assert_eq!(gate.on_sigterm(), StopSignal::Graceful);
        assert_eq!(gate.on_sigint(), StopSignal::Force);
    }

    #[test]
    fn clearing_flag_reopens_graceful_window() {
        let gate = InterruptGate::new();
        assert_eq!(gate.on_sigint(), StopSignal::Graceful);
        gate.flag().store(false, Ordering::Relaxed);
        assert_eq!(gate.on_sigint(), StopSignal::Graceful);
    }
}
