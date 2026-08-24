//! Signal handling for recording sessions.
//!
//! | Signal | Behavior |
//! |--------|----------|
//! | SIGINT / SIGTERM (1st) | Graceful stop (finalize, exit 0). |
//! | SIGINT (2nd) | Force stop via `process::exit(5)`. |
//!
//! The double-tap escalation is a hard `process::exit` because the consumer
//! thread holds the encoder while finalizing and cannot be interrupted safely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Exit code for a forced interrupt (design 03).
const EXIT_INTERRUPTED: i32 = 5;

/// Tracks whether an interrupt has armed the force-exit window.
#[derive(Debug, Default)]
pub struct InterruptGate {
    armed: Arc<AtomicBool>,
}

impl InterruptGate {
    /// Creates an un-armed gate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            armed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Shared arm flag for the force-exit watchdog.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.armed)
    }

    /// Arms the force window so a subsequent SIGINT escalates.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Relaxed);
    }
}

/// Spawns a watchdog that hard-exits on a SIGINT while the force window is armed.
///
/// The consumer thread owns the encoder while finalizing, so a second tap
/// cannot be forwarded to it; a hard exit is the only safe escape hatch.
pub fn spawn_force_exit_watchdog(armed: Arc<AtomicBool>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
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
                    std::process::exit(EXIT_INTERRUPTED);
                }
            }
        }
        #[cfg(not(unix))]
        {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                if armed.load(Ordering::Relaxed) {
                    eprintln!("Force exit — second interrupt during shutdown");
                    std::process::exit(EXIT_INTERRUPTED);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_sets_the_force_flag() {
        let gate = InterruptGate::new();
        assert!(!gate.flag().load(Ordering::Relaxed));
        gate.arm();
        assert!(gate.flag().load(Ordering::Relaxed));
    }

    #[test]
    fn arm_is_idempotent() {
        let gate = InterruptGate::new();
        gate.arm();
        gate.arm();
        assert!(gate.flag().load(Ordering::Relaxed));
    }
}
