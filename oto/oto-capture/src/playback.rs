//! Playback session wrapping [`shiguredo_audio_device::AudioPlayback`].
//!
//! Bridges the push model (producers call [`PlaybackSession::write`] with
//! interleaved stereo Float32) onto the backend's pull model (the render
//! callback requests frames on demand). A mutex-protected pending buffer holds
//! incoming PCM; the pull callback drains it and pads underruns with silence so
//! the device keeps running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use shiguredo_audio_device::{AudioPlayback, AudioPlaybackConfig, PlaybackFrame};

use crate::Error;

/// Bounds the requested sample rate / channel count: the canonical format.
const REQUESTED_SAMPLE_RATE: i32 = 48_000;
const REQUESTED_CHANNELS: i32 = 2;
/// Cap pending samples to ~200 ms so a stalled device cannot grow memory
/// without bound (48000 Hz × 2 ch × 0.2 s).
const PENDING_CAP: usize = REQUESTED_SAMPLE_RATE as usize * REQUESTED_CHANNELS as usize / 5;

/// A started output session routing PCM to the default output device.
pub struct PlaybackSession {
    playback: AudioPlayback,
    pending: Arc<Mutex<Vec<f32>>>,
    running: Arc<AtomicBool>,
}

impl PlaybackSession {
    /// Creates and starts playback to the default output device.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the output device can't be opened or started.
    pub fn start() -> Result<Self, Error> {
        let pending = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(false));
        let callback_pending = Arc::clone(&pending);

        let config = AudioPlaybackConfig {
            device_id: None,
            sample_rate: REQUESTED_SAMPLE_RATE,
            channels: REQUESTED_CHANNELS,
        };
        let callback = move |frames: i32, channels: i32, sample_rate: i32| {
            let need = usize::try_from(frames)
                .ok()
                .and_then(|f| usize::try_from(channels).ok().map(|c| f.saturating_mul(c)))
                .filter(|&n| n > 0)
                .filter(|_| sample_rate > 0)?;
            let filled = take_up_to(&callback_pending, need);
            PlaybackFrame::from_f32(&filled, channels, sample_rate).ok()
        };

        let mut playback =
            AudioPlayback::new(config, callback).map_err(|e| Error::Device(e.to_string()))?;
        playback.start().map_err(|e| Error::Device(e.to_string()))?;
        running.store(true, Ordering::Release);

        Ok(Self {
            playback,
            pending,
            running,
        })
    }

    /// Enqueues interleaved stereo [`f32`] samples for playback.
    ///
    /// Non-blocking: samples are copied into the pending buffer and drained by
    /// the render callback. Excess samples beyond ~200 ms are dropped (oldest
    /// first) to bound memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when monitoring has already been stopped.
    pub fn write(
        &self,
        pcm: &[f32],
    ) -> Result<(), crate::Error> {
        if !self.running.load(Ordering::Acquire) {
            return Err(Error::Stopped);
        }
        let mut pending = lock(&self.pending);
        pending.extend_from_slice(pcm);
        if pending.len() > PENDING_CAP {
            let overflow = pending.len() - PENDING_CAP;
            pending.drain(..overflow);
        }
        drop(pending);
        Ok(())
    }

    /// Stops and releases the output session.
    pub fn stop(&mut self) {
        if self.running.swap(false, Ordering::AcqRel) {
            self.playback.stop();
        }
        let mut pending = lock(&self.pending);
        pending.clear();
    }
}

/// Drains up to `need` samples from the pending buffer, padding with silence
/// so the buffer holds exactly `need` samples (a full playback frame).
fn take_up_to(
    buffer: &Mutex<Vec<f32>>,
    need: usize,
) -> Vec<f32> {
    let mut pending = lock(buffer);
    let take = need.min(pending.len());
    let mut out = Vec::with_capacity(need);
    out.extend(pending.drain(..take));
    while out.len() < need {
        out.push(0.0);
    }
    drop(pending);
    out
}

fn lock(buffer: &Mutex<Vec<f32>>) -> std::sync::MutexGuard<'_, Vec<f32>> {
    buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
