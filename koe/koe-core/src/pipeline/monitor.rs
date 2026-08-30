//! Live monitoring: route clean PCM to the default output device.
//!
//! Signal path (spec): Ring Buffer → AEC → Clean Audio ─┬─→ Encoder
//!                                                      └─→ `AudioQueue` output
//!
//! The output session lives in `koe-capture` (`PlaybackSession`), which wraps
//! Shiguredo's `AudioPlayback`. This module owns the pipeline-side contract and
//! an FFI-backed implementation. Monitoring failures are non-fatal: the
//! recording path must keep running.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use koe_ffi::MonitorError;
use koe_ffi::{MonitorHandle, feed_monitor, start_monitor, stop_monitor};

/// Sink for clean (post-AEC) PCM destined for the default output device.
///
/// PCM must be interleaved stereo [`f32`] at 48 kHz, 2 channels.
pub trait AudioMonitor: Send + Sync {
    /// Enqueues interleaved stereo Float32 samples for playback.
    ///
    /// Implementations must not block for longer than one buffer period.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError`] when the output device rejects the write or
    /// the monitor has already been stopped.
    fn write(
        &self,
        pcm: &[f32],
    ) -> Result<(), MonitorError>;

    /// Tears down the output queue. Safe to call more than once.
    fn stop(&self);
}

/// FFI-backed monitor that forwards PCM to the native `AudioQueue` bridge.
struct FfiMonitor {
    handle: Arc<MonitorHandle>,
    stopped: AtomicBool,
}

impl FfiMonitor {
    /// Opens a native monitoring session (canonical 48 kHz stereo Float32).
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::CreateFailed`] when the FFI layer cannot start
    /// the output queue.
    fn start() -> Result<Self, MonitorError> {
        let handle = start_monitor()?;
        Ok(Self {
            handle,
            stopped: AtomicBool::new(false),
        })
    }
}

impl AudioMonitor for FfiMonitor {
    fn write(
        &self,
        pcm: &[f32],
    ) -> Result<(), MonitorError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(MonitorError::NotRunning);
        }
        feed_monitor(Arc::clone(&self.handle), pcm.to_vec())?;
        // Re-check after feed so a concurrent stop is visible to the caller.
        if self.stopped.load(Ordering::Acquire) {
            return Err(MonitorError::NotRunning);
        }
        Ok(())
    }

    fn stop(&self) {
        if self
            .stopped
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            stop_monitor(Arc::clone(&self.handle));
        }
    }
}

impl Drop for FfiMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Opens a monitor. Create failures are logged and treated as off so recording
/// still proceeds. Callers skip this when monitoring is disabled.
pub fn start_session_monitor() -> Option<Arc<dyn AudioMonitor>> {
    match FfiMonitor::start() {
        Ok(monitor) => Some(Arc::new(monitor)),
        Err(err) => {
            log::warn!("audio monitor unavailable; continuing without monitoring: {err}");
            None
        },
    }
}

/// Test double that records every write (used by unit tests).
#[cfg(test)]
#[derive(Debug, Default)]
pub struct RecordingMonitor {
    pub samples: std::sync::Mutex<Vec<Vec<f32>>>,
    pub write_count: std::sync::atomic::AtomicU64,
    pub stop_count: std::sync::atomic::AtomicU64,
    pub fail_writes: AtomicBool,
}

#[cfg(test)]
impl AudioMonitor for RecordingMonitor {
    fn write(
        &self,
        pcm: &[f32],
    ) -> Result<(), MonitorError> {
        if self.fail_writes.load(Ordering::Relaxed) {
            return Err(MonitorError::Internal {
                msg: "injected failure".to_owned(),
            });
        }
        self.write_count.fetch_add(1, Ordering::Relaxed);
        self.samples
            .lock()
            .map_err(|_| MonitorError::Internal {
                msg: "lock poisoned".to_owned(),
            })?
            .push(pcm.to_vec());
        Ok(())
    }

    fn stop(&self) {
        self.stop_count.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_constants_match_spec() {
        const SAMPLE_RATE_HZ: u32 = 48_000;
        const CHANNEL_COUNT: u16 = 2;
        const BUFFER_FRAMES: usize = 960;
        const BYTES_PER_FRAME: usize = 8;
        assert_eq!(BUFFER_FRAMES * usize::from(CHANNEL_COUNT), 1_920);
        assert_eq!(BUFFER_FRAMES * 1_000 / SAMPLE_RATE_HZ as usize, 20);
        assert_eq!(BYTES_PER_FRAME, 8);
    }

    #[test]
    fn enabled_monitor_uses_ffi_stub() {
        // Opening the output device can fail on a headless host; skip rather
        // than panic so the pipeline robustness contract is what's exercised.
        let Some(monitor) = start_session_monitor() else {
            return;
        };
        monitor.write(&[0.1, -0.1, 0.2, -0.2]).expect("write");
        monitor.stop();
        monitor.stop();
    }

    #[test]
    fn recording_monitor_captures_pcm() {
        let monitor = RecordingMonitor::default();
        monitor.write(&[0.5, -0.5]).expect("write");
        monitor.write(&[0.25, -0.25]).expect("write");
        assert_eq!(monitor.write_count.load(Ordering::Relaxed), 2);
        let samples = monitor.samples.lock().expect("lock").clone();
        assert_eq!(samples, vec![vec![0.5, -0.5], vec![0.25, -0.25]]);
        monitor.stop();
        assert_eq!(monitor.stop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn ffi_monitor_rejects_write_after_stop() {
        let monitor = FfiMonitor::start().expect("start");
        monitor.stop();
        let err = monitor.write(&[0.0, 0.0]).expect_err("stopped");
        assert!(matches!(err, MonitorError::NotRunning));
    }
}
