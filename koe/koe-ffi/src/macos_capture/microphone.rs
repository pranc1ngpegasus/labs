//! Microphone capture via [`oto_capture`] (Shiguredo `AudioCapture`).
//!
//! The Shiguredo backend delivers raw S16 frames with the device's actual
//! channel count / sample rate. This module converts to the koe canonical
//! format (48 kHz, stereo, interleaved `f32`) and applies soft AGC before
//! forwarding to the pipeline [`crate::handles::CaptureHandle`].
//!
//! A dedicated converter thread drains the bounded capture channel so the
//! audio callback only copies frames (conversion, upmix, and AGC are off the
//! hot path).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use crate::error::CaptureError;
use crate::handles::CaptureHandle;
use oto_capture::AudioFrameOwned;

use super::{CaptureSession, monotonic_ms};

/// Bounded frames buffered between the capture callback and the converter.
const CHANNEL_CAPACITY: usize = 32;

/// Soft AGC target / limits (moved from the prior AudioQueue path).
const AGC_TARGET_PEAK: f32 = 0.45;
const AGC_MAX_GAIN: f32 = 30.0;
const AGC_MIN_GAIN: f32 = 1.0;

pub(super) struct MicrophoneSession {
    capture: Option<oto_capture::CaptureSession>,
    worker: Option<JoinHandle<()>>,
    /// Frames dropped by the capture channel under backpressure.
    dropped: Arc<AtomicUsize>,
}

impl MicrophoneSession {
    fn stop_inner(&mut self) {
        // Stop the hardware, then drop the session so the capture callback's
        // sender is released and the converter thread's `recv` unblocks.
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            // koe-ffi has no logging dependency; warn on the diagnostic stream
            // only when frames were actually lost (rare).
            eprintln!("koe mic capture: dropped {dropped} frame(s) under backpressure");
        }
    }
}

impl CaptureSession for MicrophoneSession {
    fn stop(&mut self) {
        self.stop_inner();
    }
}

impl Drop for MicrophoneSession {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

pub(super) fn start(handle: Arc<CaptureHandle>) -> Result<Box<dyn CaptureSession>, CaptureError> {
    let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicUsize::new(0));
    let capture = oto_capture::CaptureSession::start(None, 2, tx, Arc::clone(&dropped))
        .map_err(|e| CaptureError::StreamError { msg: e.to_string() })?;

    let deliver = Arc::clone(&handle);
    let worker = thread::Builder::new()
        .name("koe-mic-converter".to_owned())
        .spawn(move || {
            let mut agc = SoftAgc::new();
            while let Ok(frame) = rx.recv() {
                let pcm = frame_to_stereo_f32(&frame);
                if pcm.is_empty() {
                    continue;
                }
                deliver.deliver_audio(agc.apply(&pcm), monotonic_ms());
            }
        })
        .map_err(|e| CaptureError::Internal {
            msg: format!("spawn mic converter thread: {e}"),
        })?;

    Ok(Box::new(MicrophoneSession {
        capture: Some(capture),
        worker: Some(worker),
        dropped,
    }))
}

/// Converts an S16 capture frame to interleaved stereo `f32`, upmixing mono
/// and truncating unexpected excess channels to the first two.
#[must_use]
fn frame_to_stereo_f32(frame: &AudioFrameOwned) -> Vec<f32> {
    let Some(samples) = frame.as_s16() else {
        return Vec::new();
    };
    let channels = frame.channels.max(1) as usize;
    match channels {
        2 => samples.iter().map(|&s| f32::from(s) / 32768.0).collect(),
        1 => samples
            .iter()
            .flat_map(|&s| {
                let f = f32::from(s) / 32768.0;
                [f, f]
            })
            .collect(),
        n => {
            // Truncate interleaved multi-channel frames to the left+right pair.
            let frames = samples.len() / n;
            let mut out = Vec::with_capacity(frames * 2);
            for frame in 0..frames {
                let base = frame * n;
                out.push(f32::from(samples[base]) / 32768.0);
                out.push(f32::from(samples[base + 1]) / 32768.0);
            }
            out
        },
    }
}

/// Smoothed-envelope soft AGC (identical behavior to the prior
/// AudioQueue-only path).
#[derive(Debug)]
struct SoftAgc {
    /// Smoothed absolute-peak envelope.
    envelope: f32,
}

impl SoftAgc {
    const fn new() -> Self {
        Self { envelope: 1e-3_f32 }
    }

    fn apply(
        &mut self,
        samples: &[f32],
    ) -> Vec<f32> {
        let mut block_peak = 1e-6_f32;
        for &s in samples {
            block_peak = block_peak.max(s.abs());
        }

        // Fast attack, slow release so speech onsets aren't clipped and quiet
        // passages still get makeup.
        let prev = self.envelope;
        self.envelope = if block_peak > prev {
            block_peak
        } else {
            0.02f32.mul_add(block_peak, 0.98 * prev)
        };

        let gain = (AGC_TARGET_PEAK / self.envelope.max(1e-4)).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
        samples
            .iter()
            .map(|&s| (s * gain).clamp(-1.0, 1.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oto_capture::AudioFormat;

    fn s16_frame(
        data: Vec<i16>,
        channels: i32,
    ) -> AudioFrameOwned {
        let bytes: Vec<u8> = data.iter().flat_map(|s| s.to_ne_bytes()).collect();
        AudioFrameOwned {
            data: bytes,
            frames: data.len() as i32 / channels,
            channels,
            sample_rate: 48_000,
            format: AudioFormat::S16,
            timestamp_us: 0,
        }
    }

    fn assert_close(
        actual: &[f32],
        expected: &[f32],
    ) {
        assert_eq!(actual.len(), expected.len());
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
        }
    }

    #[test]
    fn stereo_frame_passes_through_as_f32() {
        let frame = s16_frame(vec![16384, -16384, 32767, -32767], 2);
        let pcm = frame_to_stereo_f32(&frame);
        assert_close(&pcm, &[0.5, -0.5, 32767.0 / 32768.0, -32767.0 / 32768.0]);
    }

    #[test]
    fn mono_frame_upmixes_to_stereo() {
        let frame = s16_frame(vec![16384, 0, -32767], 1);
        let pcm = frame_to_stereo_f32(&frame);
        assert_close(
            &pcm,
            &[0.5, 0.5, 0.0, 0.0, -32767.0 / 32768.0, -32767.0 / 32768.0],
        );
    }

    #[test]
    fn excess_channels_truncate_to_left_right() {
        // Interleaved 3-channel frames: L R X L R X.
        let frame = s16_frame(vec![16384, -16384, 9999, 8192, -8192, 9999], 3);
        let pcm = frame_to_stereo_f32(&frame);
        assert_eq!(pcm.len(), 4);
        assert!((pcm[0] - 0.5).abs() < 1e-6);
        assert!((pcm[1] + 0.5).abs() < 1e-6);
        assert!((pcm[2] - 0.25).abs() < 1e-6);
        assert!((pcm[3] + 0.25).abs() < 1e-6);
    }

    #[test]
    fn agc_lifts_quiet_input_without_exceeding_limits() {
        let mut agc = SoftAgc::new();
        let quiet = vec![0.01, -0.01, 0.005, -0.005];
        let out = agc.apply(&quiet);
        // AGC boosts toward ~0.45 target but stays within 1..=30 gain.
        for &s in &out {
            assert!(s.abs() <= 1.0);
        }
        assert!(out.iter().any(|&s| s.abs() > 0.01));
    }

    #[test]
    fn agc_fast_attack_does_not_clip_loud_onsets() {
        let mut agc = SoftAgc::new();
        // Gain starts high from silence, but a large onset must be clamped.
        let quiet = vec![0.001; 1920];
        let _ = agc.apply(&quiet);
        let onset = vec![1.0; 10];
        let out = agc.apply(&onset);
        assert!(out.iter().all(|&s| s.abs() <= 1.0));
    }
}
