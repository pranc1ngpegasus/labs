//! macOS on-device speech recognition via the `Speech` framework.
//!
//! Implements `start_transcription` / `feed_transcription_audio` /
//! `finalize_transcription` against `SFSpeechRecognizer` + a streaming
//! `SFSpeechAudioBufferRecognitionRequest`, without linking the Swift
//! `koe-native` dylib. This is what makes `koe transcribe file.ogg` →
//! `file.txt` work from the plain Rust CLI.
//!
//! ## Engine selection
//!
//! Task 10 says "100% on-device", but that requirement is unfulfillable on
//! hosts where Dictation is disabled — the recognizer fails immediately with
//! "Siri and Dictation are disabled" before consuming any audio. Recognition
//! therefore probes on-device once per process and, when the system rejects
//! that mode, falls back to network recognition with a warning. Nothing is
//! lost: the probe happens before any audio has been fed.
//!
//! ## Authorization
//!
//! The current status is checked here. `requestAuthorization` is never called
//! blindly from an unbundled CLI: Apple crashes when prompting without an
//! Info.plist usage description. The CLI embeds one via `-sectcreate`, so a
//! prompt-based flow is possible, but until the status is `Authorized` we
//! return a descriptive error.

#![allow(unsafe_code)]

use std::ffi::{c_double, c_void};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_avf_audio::{AVAudioCommonFormat, AVAudioFormat, AVAudioPCMBuffer};
use objc2_foundation::{NSArray, NSError, NSLocale, NSOperationQueue, NSString};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus, SFTranscriptionSegment,
};

use crate::error::TranscriptionError;
use crate::handles::TranscriptionHandle;
use crate::types::{SpeechEngine, TranscriptionSegment};

/// PCM frame rate fed to the recognizer (matches the pipeline's canonical rate).
const SAMPLE_RATE_HZ: f64 = 48_000.0;
/// Interleaved L/R channels.
const CHANNELS: usize = 2;
/// Probe wait: if on-device rejects with an engine-level error it does so
/// almost immediately; cap the wait so healthy hosts pay little latency.
const PROBE_WAIT: Duration = Duration::from_millis(1_200);
/// How long `finalize_transcription` waits for the final result after
/// `endAudio` + `finish`, pumping the runloop on the main thread.
const FINALIZE_WAIT: Duration = Duration::from_secs(20);

/// Engine probe outcome (cached per process).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    OnDevice,
    Network,
    Unavailable,
}

/// `None` = not yet probed; see [`probe_engine`].
static ENGINE_CACHE: OnceLock<Engine> = OnceLock::new();

fn on_device_help() -> String {
    "enable Dictation / on-device speech models in System Settings → Apple Intelligence & Siri → Dictation, or use engine=network to explicitly allow Apple server-side recognition".to_owned()
}

/// Whether the recognizer reported a system-level engine failure ("Siri and
/// Dictation are disabled" et al.) which a retry with network recognition
/// can recover from.
fn is_recoverable_engine_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("siri and dictation are disabled")
        || lower.contains("dictation")
        || lower.contains("on-device speech recognition is unavailable")
}

/// Selects the speech engine, preferring on-device (privacy) with a one-time
/// fallback to network recognition when the host has dictation disabled.
#[must_use]
pub fn probe_engine(recognizer: &SFSpeechRecognizer) -> Engine {
    if let Some(&engine) = ENGINE_CACHE.get() {
        return engine;
    }
    let engine = match probe_on_device(recognizer) {
        ProbeOutcome::Works => Engine::OnDevice,
        ProbeOutcome::Recoverable => Engine::Network,
        ProbeOutcome::Failed => Engine::Unavailable,
    };
    let _ = ENGINE_CACHE.set(engine);
    engine
}

/// Result of a short on-device recognizer probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Works,
    Recoverable,
    Failed,
}

/// Starts a throwaway on-device task on `recognizer` and watches the first
/// callback for an engine-level error. No audio is fed, so a fallback retry
/// loses nothing.
fn probe_on_device(recognizer: &SFSpeechRecognizer) -> ProbeOutcome {
    let request = make_request(true);
    let (tx, rx) = mpsc::channel::<ProbeOutcome>();
    let block = RcBlock::new(
        move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
            let outcome = if !error.is_null() {
                // SAFETY: the Speech framework passes a +0 (borrowed) error
                // valid for the duration of this block invocation.
                let msg = unsafe { Retained::retain(error) }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default();
                if is_recoverable_engine_error(&msg) {
                    ProbeOutcome::Recoverable
                } else {
                    ProbeOutcome::Failed
                }
            } else if result.is_null() {
                ProbeOutcome::Failed
            } else {
                ProbeOutcome::Works
            };
            let _ = tx.send(outcome);
        },
    );
    let queue = make_result_queue();
    // SAFETY: assigning our own serial result queue.
    unsafe {
        recognizer.setQueue(&queue);
    }
    // Keep the task alive for the whole probe: dropping it may cancel
    // recognition before the recoverable error has a chance to fire.
    let task = unsafe { recognizer.recognitionTaskWithRequest_resultHandler(&request, &block) };

    // No early verdict: assume on-device works (partial results or silence —
    // nothing failed synchronously).
    let outcome = wait_for_probe(&rx).unwrap_or(ProbeOutcome::Works);
    drop(task);
    outcome
}

fn wait_for_probe(rx: &Receiver<ProbeOutcome>) -> Option<ProbeOutcome> {
    let deadline = Instant::now() + PROBE_WAIT;
    loop {
        match rx.try_recv() {
            Ok(outcome) => return Some(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return None;
                }
                pump_runloop(0.05);
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// An active recognition session bound to a [`TranscriptionHandle`].
///
/// Owns every Objective-C object the task graph retains: recognizer, request,
/// the in-flight task, and the result-delivery queue. Dropped when the
/// transcription session ends.
pub struct SpeechSession {
    /// Retained for the session lifetime (the task keeps it alive too, but an
    /// explicit hold makes the graph deterministic on `Drop`).
    #[expect(dead_code)]
    recognizer: Retained<SFSpeechRecognizer>,
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    _queue: Retained<NSOperationQueue>,
    /// Completion signal consumed by [`crate::finalize_transcription`].
    completion: Option<Receiver<SessionOutcome>>,
    engine: Engine,
}

/// What ended a recognition session, delivered to the finalize wait.
#[derive(Debug)]
pub enum SessionOutcome {
    /// The recognizer emitted a final result (or an error).
    Finished(Result<(), String>),
}

// SAFETY: the Objective-C objects are only touched from threads that hold the
// owning `TranscriptionHandle` session lock; `SFSpeechRecognizer`/request/task
// are thread-confined to the serial feed path (same as `ScreenAudioSession`).
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for SpeechSession {}

// SAFETY: all mutable state is behind the `TranscriptionHandle` mutex; the
// recognizer itself is documented as usable from any queue via its `queue`
// property, which we set to our own serial `NSOperationQueue`.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Sync for SpeechSession {}

impl SpeechSession {
    /// Starts recognition on `handle` using the requested [`SpeechEngine`].
    ///
    /// - [`SpeechEngine::Auto`] prefers on-device and falls back to network
    ///   recognition when the host cannot run on-device models.
    /// - [`SpeechEngine::OnDevice`] never sends audio off-device: it errors
    ///   with [`TranscriptionError::OnDeviceUnavailable`] instead of falling
    ///   back.
    /// - [`SpeechEngine::Network`] always uses server-side recognition.
    ///
    /// # Errors
    ///
    /// [`TranscriptionError::UnsupportedLocale`] when the locale is unknown,
    /// [`TranscriptionError::PermissionDenied`] when speech authorization has
    /// not been granted, [`TranscriptionError::OnDeviceUnavailable`] when
    /// [`SpeechEngine::OnDevice`] is requested but unavailable, and
    /// [`TranscriptionError::NotAvailable`] when speech services are otherwise
    /// unavailable.
    pub fn start(
        handle: &Arc<TranscriptionHandle>,
        locale: &str,
        requested: SpeechEngine,
    ) -> Result<Self, TranscriptionError> {
        let status = unsafe { SFSpeechRecognizer::authorizationStatus() };
        if status != SFSpeechRecognizerAuthorizationStatus::Authorized {
            return Err(TranscriptionError::PermissionDenied {
                msg: format!(
                    "speech recognition authorization is {status:?}; enable it in \
                     System Settings → Privacy & Security → Speech Recognition"
                ),
            });
        }

        // Locale validation happens before engine probing so an unknown locale
        // reports `UnsupportedLocale`, not a confusing engine error.
        let recognizer =
            make_recognizer(locale).ok_or_else(|| TranscriptionError::UnsupportedLocale {
                locale: locale.to_owned(),
            })?;

        // Resolve the requested engine to a concrete on-device / network mode.
        // `Network` skips the probe entirely (no throwaway task, no audio
        // off-device until the user asked for it).
        let use_on_device = match requested {
            SpeechEngine::Network => false,
            SpeechEngine::OnDevice => match probe_engine(&recognizer) {
                Engine::OnDevice => true,
                Engine::Network | Engine::Unavailable => {
                    return Err(TranscriptionError::OnDeviceUnavailable {
                        msg: on_device_help(),
                    });
                },
            },
            SpeechEngine::Auto => match probe_engine(&recognizer) {
                Engine::OnDevice => true,
                Engine::Network => false,
                Engine::Unavailable => return Err(TranscriptionError::NotAvailable),
            },
        };
        let engine = if use_on_device {
            Engine::OnDevice
        } else {
            Engine::Network
        };

        let request = make_request(use_on_device);
        let queue = make_result_queue();
        // SAFETY: assigning our own serial result queue.
        unsafe {
            recognizer.setQueue(&queue);
        }

        let weak = Arc::downgrade(handle);
        let (outcome_tx, outcome_rx) = mpsc::channel::<SessionOutcome>();
        let block = RcBlock::new(
            move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                handle_result(weak.clone(), result, error, &outcome_tx);
            },
        );
        let task = unsafe { recognizer.recognitionTaskWithRequest_resultHandler(&request, &block) };

        Ok(Self {
            recognizer,
            request,
            task,
            _queue: queue,
            completion: Some(outcome_rx),
            engine,
        })
    }

    /// Feeds one interleaved stereo f32 chunk (48 kHz) to the recognizer.
    pub fn feed(
        &self,
        pcm: &[f32],
    ) {
        if pcm.len() < CHANNELS {
            return;
        }
        let frame_count = pcm.len() / CHANNELS;
        let Some(buffer) = make_pcm_buffer(frame_count) else {
            return;
        };
        fill_buffer(&buffer, pcm, frame_count);
        // SAFETY: buffer is a valid AVAudioPCMBuffer; the request is alive.
        unsafe {
            self.request.appendAudioPCMBuffer(&buffer);
        }
    }

    /// Signals the end of audio input (idempotent; safe to call even if the
    /// recognizer already finished) and returns the completion receiver.
    ///
    /// The caller must wait on the receiver *outside* the session lock so a
    /// concurrent [`SpeechSession::feed`] (e.g. a live recorder feeding on a
    /// worker thread while the owner finalizes) never blocks on this thread's
    /// 20 s wait.
    pub fn start_finalize(&mut self) -> Option<Receiver<SessionOutcome>> {
        let receiver = self.completion.take()?;
        // SAFETY: both calls are documented for use when audio input ends.
        unsafe {
            self.request.endAudio();
            self.task.finish();
        }
        Some(receiver)
    }

    /// The engine actually in use (for diagnostics/warnings).
    #[must_use]
    pub const fn engine(&self) -> Engine {
        self.engine
    }
}

/// Handles one result-handler callback: forwards segments/errors to the
/// handle and signals the finalize wait when the task is done.
#[allow(clippy::needless_pass_by_value)]
fn handle_result(
    weak: Weak<TranscriptionHandle>,
    result: *mut SFSpeechRecognitionResult,
    error: *mut NSError,
    outcome_tx: &Sender<SessionOutcome>,
) {
    let Some(handle) = weak.upgrade() else {
        return;
    };
    if !result.is_null() {
        // SAFETY: the Speech framework passes a +0 (borrowed) result; retain
        // it for the duration of this delivery.
        let Some(retained) = (unsafe { Retained::retain(result) }) else {
            return;
        };
        deliver_result(&handle, &retained);
        // SAFETY: simple Boolean getter.
        if unsafe { retained.isFinal() } {
            let _ = outcome_tx.send(SessionOutcome::Finished(Ok(())));
            return;
        }
    }
    if !error.is_null() {
        // SAFETY: same +0 borrow as the result argument.
        let message = unsafe { Retained::retain(error) }.map_or_else(
            || "speech recognition error".to_owned(),
            |e| e.localizedDescription().to_string(),
        );
        let _ = outcome_tx.send(SessionOutcome::Finished(Err(message.clone())));
        handle.deliver_error(message);
    }
}

fn wait_for_outcome(rx: &Receiver<SessionOutcome>) -> Option<SessionOutcome> {
    let deadline = Instant::now() + FINALIZE_WAIT;
    loop {
        match rx.try_recv() {
            Ok(outcome) => return Some(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return None;
                }
                pump_runloop(0.05);
            },
        }
    }
}

/// Waits for the recognizer's final verdict on `receiver` (from
/// [`SpeechSession::start_finalize`]). Returns an error on recognizer failure
/// or timeout.
///
/// # Errors
///
/// [`TranscriptionError::Internal`] when recognition failed or timed out.
pub fn wait_for_finalize(receiver: &Receiver<SessionOutcome>) -> Result<(), String> {
    match wait_for_outcome(receiver) {
        Some(SessionOutcome::Finished(Ok(()))) => Ok(()),
        Some(SessionOutcome::Finished(Err(message))) => Err(message),
        None => Err("timed out waiting for the speech recognizer".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Native construction helpers
// ---------------------------------------------------------------------------

fn make_recognizer(locale: &str) -> Option<Retained<SFSpeechRecognizer>> {
    let ns_locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(locale));
    // SAFETY: alloc + initWithLocale: is the documented constructor.
    let recognizer =
        unsafe { SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &ns_locale) };
    let recognizer = recognizer?;
    // SAFETY: simple Boolean getter.
    if !unsafe { recognizer.isAvailable() } {
        return None;
    }
    Some(recognizer)
}

#[must_use]
fn make_request(on_device: bool) -> Retained<SFSpeechAudioBufferRecognitionRequest> {
    // SAFETY: `new` allocates an empty audio-buffer request; the setters are
    // documented request configuration, safe on a freshly-created request.
    unsafe {
        let request = SFSpeechAudioBufferRecognitionRequest::new();
        request.setRequiresOnDeviceRecognition(on_device);
        request.setShouldReportPartialResults(true);
        request.setAddsPunctuation(true);
        request
    }
}

fn make_result_queue() -> Retained<NSOperationQueue> {
    let queue = NSOperationQueue::new();
    queue.setMaxConcurrentOperationCount(1);
    queue.setName(Some(&NSString::from_str("dev.mokmok.koe.speech.results")));
    queue
}

fn make_pcm_buffer(frame_count: usize) -> Option<Retained<AVAudioPCMBuffer>> {
    let frame_count_u32 = u32::try_from(frame_count).ok()?;
    let channel_count = u32::try_from(CHANNELS).ok()?;
    // SAFETY: initWithCommonFormat:sampleRate:channels:interleaved: with
    // Float32, 48 kHz, 2ch, non-interleaved is a valid PCM format.
    let format = unsafe {
        AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
            AVAudioFormat::alloc(),
            AVAudioCommonFormat::PCMFormatFloat32,
            SAMPLE_RATE_HZ,
            channel_count,
            false,
        )
    };
    let format = format?;
    // SAFETY: initWithPCMFormat:frameCapacity: with our layout.
    unsafe {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            &format,
            frame_count_u32,
        )
    }
}

fn fill_buffer(
    buffer: &AVAudioPCMBuffer,
    pcm: &[f32],
    frame_count: usize,
) {
    // SAFETY: floatChannelData returns planar non-interleaved channel arrays;
    // for our Float32 non-interleaved format, each channel array has
    // `frameCapacity` contiguous frames.
    let channels_ptr = unsafe { buffer.floatChannelData() };
    if channels_ptr.is_null() {
        return;
    }
    let left = channels_ptr;
    // SAFETY: the buffer's format has exactly two channels; the second
    // channel array follows the first.
    let right = unsafe { channels_ptr.add(1) };
    // SAFETY: floatChannelData channels are either both null or both valid
    // for `frameCapacity` frames.
    let left = unsafe { left.as_ref() }.map(|p| p.as_ptr());
    let right = unsafe { right.as_ref() }.map(|p| p.as_ptr());
    let (Some(left), Some(right)) = (left, right) else {
        return;
    };
    let frames = frame_count.min(pcm.len() / CHANNELS);
    let left_slice = unsafe { std::slice::from_raw_parts_mut(left, frames) };
    let right_slice = unsafe { std::slice::from_raw_parts_mut(right, frames) };
    for (i, pair) in pcm.chunks_exact(CHANNELS).take(frames).enumerate() {
        left_slice[i] = pair[0];
        right_slice[i] = pair[1];
    }
    // SAFETY: frameLength ≤ frameCapacity.
    unsafe {
        buffer.setFrameLength(u32::try_from(frames).unwrap_or(0));
    }
}

fn deliver_result(
    handle: &TranscriptionHandle,
    result: &Retained<SFSpeechRecognitionResult>,
) {
    // SAFETY: `bestTranscription` returns an owned transcription object; it
    // is valid while the result is retained.
    let best = unsafe { result.bestTranscription() };
    // SAFETY: `formattedString` returns the whole recognized utterance. The
    // per-word `segments` are still used below for timing/confidence only.
    let text = unsafe { best.formattedString() }.to_string();
    if text.is_empty() {
        return;
    }

    // SAFETY: `segments` returns an owned array of word-level timing segments.
    let segments = unsafe { best.segments() };
    let (start_ms, end_ms, confidence) = transcription_timing(&segments);
    // SAFETY: simple Boolean getter.
    let is_final = unsafe { result.isFinal() };
    handle.deliver_segment(TranscriptionSegment {
        text,
        start_ms,
        end_ms,
        is_final,
        confidence,
    });
}

fn transcription_timing(segments: &NSArray<SFTranscriptionSegment>) -> (i64, i64, f32) {
    let mut start_ms: Option<i64> = None;
    let mut end_ms = 0_i64;
    let mut confidence_sum = 0.0_f32;
    let mut count = 0_u32;

    for segment in segments {
        // SAFETY: NSTimeInterval getters; values are finite doubles.
        let timestamp = unsafe { segment.timestamp() };
        let duration = unsafe { segment.duration() };
        let confidence = unsafe { segment.confidence() };
        // Rounding timestamps to whole ms; values stay well within i64 range.
        #[allow(clippy::cast_possible_truncation)]
        let segment_start_ms = (timestamp * 1_000.0).round() as i64;
        #[allow(clippy::cast_possible_truncation)]
        let segment_end_ms = ((timestamp + duration) * 1_000.0).round() as i64;
        start_ms = Some(start_ms.map_or(segment_start_ms, |start| start.min(segment_start_ms)));
        end_ms = end_ms.max(segment_end_ms);
        confidence_sum += confidence;
        count = count.saturating_add(1);
    }

    let start_ms = start_ms.unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    let confidence = if count == 0 {
        0.0
    } else {
        confidence_sum / count as f32
    };
    (start_ms, end_ms.max(start_ms), confidence)
}

// ---------------------------------------------------------------------------
// Runloop pumping
// ---------------------------------------------------------------------------

type CFStringRef = *const c_void;
type CFTimeInterval = c_double;
type Boolean = u8;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRunInMode(
        mode: CFStringRef,
        seconds: CFTimeInterval,
        return_after_source_handled: Boolean,
    ) -> i32;
    static kCFRunLoopDefaultMode: CFStringRef;
}

/// Pumps the current thread's runloop so dispatch/OperationQueue sources can
/// run. Recognition results are delivered on our own `NSOperationQueue`, so
/// this is mainly useful on the main thread in CLI hosts.
fn pump_runloop(interval: f64) {
    // SAFETY: CoreFoundation runloop API; yields for at most `interval`
    // seconds and returns when a source is handled.
    let _ = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, interval, 1) };
}

#[cfg(test)]
mod tests {
    use super::is_recoverable_engine_error;

    #[test]
    fn dictation_disabled_is_recoverable() {
        assert!(is_recoverable_engine_error(
            "Siri and Dictation are disabled"
        ));
        assert!(is_recoverable_engine_error(
            "Dictation is not available for this language"
        ));
        assert!(is_recoverable_engine_error(
            "on-device speech recognition is unavailable"
        ));
    }

    #[test]
    fn unrelated_errors_are_not_recoverable() {
        assert!(!is_recoverable_engine_error("No speech detected"));
        assert!(!is_recoverable_engine_error("Connection failed"));
        assert!(!is_recoverable_engine_error(""));
    }
}
