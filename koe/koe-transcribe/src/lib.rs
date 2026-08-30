//! Zero-runtime-dependency macOS on-device speech recognition.
//!
//! A thin safe wrapper over the `Speech` framework (Apple's on-device
//! `SpeechAnalyzer` API family), built the way
//! [shiguredo/audio-device-rs](https://github.com/shiguredo/audio-device-rs)
//! builds its audio capture: an Objective-C shim compiled by `build.rs`,
//! system frameworks linked directly, and bindgen-generated C bindings.
//!
//! The crate has **no runtime dependency crates** — only the system
//! `Speech`/`Foundation`/`CoreFoundation`/`AVFoundation` frameworks.
//!
//! ## Platform
//!
//! The recognizer bridge is macOS-only. On other targets the API compiles but
//! every function degrades to `Engine::Unavailable` / `Vec::new()` /
//! `Err(Error::NotAvailable)`, keeping workspace CI (Linux) green.
//!
//! ## Engine selection
//!
//! [`RequestedEngine::OnDevice`] never sends audio off-device. `Auto` probes
//! once per process and falls back to network recognition when the host cannot
//! run on-device models (for example Siri & Dictation are disabled). See
//! [`probe`] for the verdict.

#![allow(unsafe_code)]

mod error;
mod ffi;

pub use error::Error;

#[cfg(target_os = "macos")]
use std::ffi::{CStr, CString, c_char, c_int, c_void};
#[cfg(target_os = "macos")]
use std::ptr::{self, NonNull};
#[cfg(target_os = "macos")]
use std::sync::{Mutex, OnceLock, PoisonError};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

/// Interleaved L/R channels.
#[cfg(target_os = "macos")]
const CHANNELS: usize = 2;
/// How long a probe waits for the recognizer's verdict before assuming
/// on-device works (healthy hosts report instantly).
#[cfg(target_os = "macos")]
const PROBE_WAIT: Duration = Duration::from_millis(1_200);
/// How long [`SpeechAnalyzer::finish`] waits for the final result.
#[cfg(target_os = "macos")]
const FINALIZE_WAIT: Duration = Duration::from_secs(20);
/// Run-loop pump quantum used while polling recognizer callbacks.
#[cfg(target_os = "macos")]
const RUNLOOP_STEP: f64 = 0.05;

/// Help text for [`Error::OnDeviceUnavailable`] (used only on macOS where a
/// session can actually be requested).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ON_DEVICE_HELP: &str = "enable Dictation / on-device speech models in System Settings → Apple Intelligence & \
     Siri → Dictation, or use engine=Network";

// ---------------------------------------------------------------------------
// Public value types
// ---------------------------------------------------------------------------

/// Current speech-recognition authorization status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationStatus {
    NotDetermined,
    Denied,
    Restricted,
    Authorized,
}

/// Which recognition engine a session should use.
///
/// `Auto` prefers on-device and falls back to network only when the host
/// cannot run on-device models. `OnDevice` never sends audio off-device and
/// errors instead of falling back. `Network` always uses server-side
/// recognition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedEngine {
    Auto,
    OnDevice,
    Network,
}

/// The engine actually in use (the resolved result, distinct from the
/// [`RequestedEngine`] a caller asked for). `Unavailable` also reports a
/// failed probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    OnDevice,
    Network,
    Unavailable,
}

/// One transcription segment (utterance-level timing, like koe's
/// `TranscriptionSegment`).
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub text: String,
    /// Offset from stream start in milliseconds.
    pub start_ms: i64,
    pub end_ms: i64,
    /// `false` = partial result (may be updated), `true` = final.
    pub is_final: bool,
    /// 0.0–1.0.
    pub confidence: f32,
}

// ---------------------------------------------------------------------------
// System queries
// ---------------------------------------------------------------------------

/// Current speech-recognition authorization status.
///
/// This only reads the status; requesting authorization is the caller's job
/// (an unbundled CLI cannot prompt without an Info.plist usage description).
#[must_use]
#[allow(clippy::missing_const_for_fn)] // macOS reads the framework; stub targets are const-free
pub fn authorization_status() -> AuthorizationStatus {
    #[cfg(target_os = "macos")]
    {
        let raw = unsafe { ffi::koe_speech_authorization_status() };
        match raw {
            ffi::koe::AUTHORIZATION_AUTHORIZED => AuthorizationStatus::Authorized,
            ffi::koe::AUTHORIZATION_RESTRICTED => AuthorizationStatus::Restricted,
            ffi::koe::AUTHORIZATION_DENIED => AuthorizationStatus::Denied,
            _ => AuthorizationStatus::NotDetermined,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        AuthorizationStatus::NotDetermined
    }
}

/// BCP-47 locale identifiers supported by the Speech framework, sorted and
/// deduplicated. Raw identifiers from the framework (`en_US`) are normalized
/// to BCP-47 hyphen form (`en-US`).
#[must_use]
#[allow(clippy::missing_const_for_fn)] // macOS walks NSLocale; stub targets are const-free
pub fn supported_locales() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        let ctx = Box::into_raw(Box::new(LocaleCtx {
            locales: Mutex::new(Vec::new()),
        }));
        unsafe {
            ffi::koe_speech_supported_locales(Some(locale_trampoline), ctx.cast::<c_void>());
        }
        let ctx = unsafe { Box::from_raw(ctx) };
        let locales = ctx.locales.into_inner().unwrap_or_default();
        let mut out: Vec<String> = locales.into_iter().map(|l| to_bcp47(&l)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Tags the Speech framework's locale identifiers into BCP-47 hyphen form.
///
/// Only used by the macOS locale walk; kept on other targets so the
/// normalization tests can run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
fn to_bcp47(identifier: &str) -> String {
    identifier.replace('_', "-")
}

/// Whether the recognizer reported a system-level engine failure (for
/// example "Siri and Dictation are disabled") that a retry with network
/// recognition can recover from.
///
/// Only used by the macOS probe path; on other targets it stays (dead) code
/// so the pure-logic tests can still exercise it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
fn is_recoverable_engine_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("siri and dictation are disabled")
        || lower.contains("dictation")
        || lower.contains("on-device speech recognition is unavailable")
}

// ---------------------------------------------------------------------------
// Engine probing
// ---------------------------------------------------------------------------

/// Verdict of a short on-device recognizer probe.
///
/// Produced by the macOS probe path; kept alive on other targets so the
/// classification tests below can run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeVerdict {
    /// The recognizer produced a result (on-device works).
    Works,
    /// The recognizer was rejected; the error message decides recoverability.
    Recoverable(String),
    /// An unrecoverable failure.
    Failed,
    /// No verdict arrived.
    Unknown,
}

/// Maps a probe verdict to an engine choice (pure, testable).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
fn classify_probe(verdict: ProbeVerdict) -> Engine {
    match verdict {
        ProbeVerdict::Works => Engine::OnDevice,
        ProbeVerdict::Recoverable(msg) if is_recoverable_engine_error(&msg) => Engine::Network,
        ProbeVerdict::Recoverable(_) | ProbeVerdict::Failed | ProbeVerdict::Unknown => {
            Engine::Unavailable
        },
    }
}

/// Probes whether on-device recognition works for `locale`.
///
/// Network-engine hosts (Dictation disabled) resolve to [`Engine::Network`];
/// truly broken hosts resolve to [`Engine::Unavailable`]. Each call probes
/// fresh; [`SpeechAnalyzer::start`] with `Auto`/`OnDevice` caches the verdict
/// per process so a capture loop pays the probe cost only once.
#[must_use]
#[allow(clippy::missing_const_for_fn)] // macOS probes the recognizer; stub targets are const-free
pub fn probe(locale: &str) -> Engine {
    probe_native(locale)
}

/// Non-macOS: no recognizer to probe.
#[cfg(not(target_os = "macos"))]
const fn probe_native(_locale: &str) -> Engine {
    Engine::Unavailable
}

#[cfg(target_os = "macos")]
fn probe_native(locale: &str) -> Engine {
    if locale.trim().is_empty() {
        return Engine::Unavailable;
    }
    let Ok(locale_c) = CString::new(locale) else {
        return Engine::Unavailable;
    };
    let ctx = Box::into_raw(Box::new(ProbeCtx {
        verdict: Mutex::new(None),
    }));
    let handle = unsafe {
        // SAFETY: `ctx` stays alive until cancel; the trampoline only writes
        // to it from the recognizer's queue, which cancel joins out.
        ffi::koe_speech_probe_start(
            locale_c.as_ptr(),
            Some(probe_trampoline),
            ctx.cast::<c_void>(),
        )
    };
    if handle.is_null() {
        // The locale is unsupported or the recognizer is unavailable.
        // SAFETY: we own `ctx` and never passed it to the recognizer.
        unsafe { drop(Box::from_raw(ctx)) };
        return Engine::Unavailable;
    }

    let deadline = Instant::now() + PROBE_WAIT;
    let result = loop {
        let verdict = unsafe {
            // SAFETY: `ctx` is ours and alive until cancel below, which joins
            // out the trampoline that could be writing this field.
            &*ctx
        }
        .verdict
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
        if let Some(verdict) = verdict {
            break classify_probe(verdict);
        }
        if Instant::now() >= deadline {
            break Engine::OnDevice;
        }
        // SAFETY: pumps this thread's run loop so pending dispatch/queue
        // sources can drain; touches no recognizer state.
        unsafe { ffi::koe_speech_runloop_step(RUNLOOP_STEP) };
    };

    // SAFETY: cancel joins any in-flight callback, so `ctx` is no longer
    // dereferenced afterwards and can be freed.
    unsafe { ffi::koe_speech_probe_cancel(handle) };
    // SAFETY: the probe is ours; cancel stopped all deliveries.
    unsafe { drop(Box::from_raw(ctx)) };
    result
}

/// The concrete engine a session will run with.
///
/// Resolved by [`resolve_engine`]; only constructed on macOS, but kept on
/// other targets so the resolution tests can run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedEngine {
    OnDevice,
    Network,
}

/// Resolves a requested engine against a probe verdict (pure, testable).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn resolve_engine(
    requested: RequestedEngine,
    probed: Engine,
) -> Result<ResolvedEngine, Error> {
    match requested {
        RequestedEngine::Network => Ok(ResolvedEngine::Network),
        RequestedEngine::OnDevice => match probed {
            Engine::OnDevice => Ok(ResolvedEngine::OnDevice),
            _ => Err(Error::OnDeviceUnavailable {
                msg: ON_DEVICE_HELP.to_owned(),
            }),
        },
        RequestedEngine::Auto => match probed {
            Engine::OnDevice => Ok(ResolvedEngine::OnDevice),
            Engine::Network => Ok(ResolvedEngine::Network),
            Engine::Unavailable => Err(Error::NotAvailable),
        },
    }
}

#[cfg(target_os = "macos")]
fn map_create_error(
    code: c_int,
    locale: &str,
) -> Error {
    match code {
        ffi::koe::ERROR_PERMISSION_DENIED => Error::PermissionDenied(
            "speech recognition authorization is not granted; enable it in \
             System Settings → Privacy & Security → Speech Recognition"
                .to_owned(),
        ),
        ffi::koe::ERROR_UNSUPPORTED_LOCALE => Error::UnsupportedLocale(locale.to_owned()),
        ffi::koe::ERROR_NOT_AVAILABLE | ffi::koe::ERROR_ENGINE => Error::NotAvailable,
        ffi::koe::ERROR_INVALID_ARGUMENT => {
            Error::Internal("invalid argument to speech session create".to_owned())
        },
        _ => Error::Internal(format!("speech session create failed (code {code})")),
    }
}

// ---------------------------------------------------------------------------
// Streaming session
// ---------------------------------------------------------------------------

/// Receives transcription results from a [`SpeechAnalyzer`] session.
///
/// Feeds interleaved Float32 stereo PCM at 48 kHz; partial and final
/// [`Segment`]s are delivered to the `on_segment` closure registered in
/// [`SpeechAnalyzer::start`], recognizer warnings to `on_error`.
#[cfg(target_os = "macos")]
pub struct SpeechAnalyzer {
    handle: NonNull<ffi::KoeSpeechSession>,
    ctx: Box<SessionCtx>,
}

/// Non-macOS placeholder so the crate compiles with the same public API.
#[cfg(not(target_os = "macos"))]
pub struct SpeechAnalyzer;

#[cfg(target_os = "macos")]
struct SessionCtx {
    on_segment: Box<dyn Fn(&Segment) + Send + Sync>,
    on_error: Box<dyn Fn(&str) + Send + Sync>,
    /// Set when the recognizer delivers a terminal FINISHED result.
    finished: Mutex<Option<Result<(), String>>>,
}

// SAFETY: the C session serializes appends with a lock and joins any in-flight
// callback (and in-progress feed) on destroy, so the raw handle can be shared;
// the per-session `SessionCtx` is `Send + Sync` (boxed `Send + Sync` closures
// and a `Mutex`). Mirrors the bound koe's `SpeechSession` relied on for its
// UniFFI `TranscriptionHandle`.
#[cfg(target_os = "macos")]
unsafe impl Send for SpeechAnalyzer {}
#[cfg(target_os = "macos")]
unsafe impl Sync for SpeechAnalyzer {}

impl SpeechAnalyzer {
    /// Starts a streaming recognition session for `locale`.
    ///
    /// `engine` selects on-device / network behavior (with an `Auto` probe
    /// cache); `on_segment` receives each partial/final [`Segment`] and
    /// `on_error` receives non-fatal recognizer errors. Both are called on the
    /// recognizer's serial queue and must stay non-blocking.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedLocale`] when the locale is unknown,
    /// [`Error::PermissionDenied`] when speech authorization is not granted,
    /// [`Error::OnDeviceUnavailable`] when `OnDevice` is requested but the
    /// host cannot run it, and [`Error::NotAvailable`] when `Auto` cannot
    /// resolve an engine.
    pub fn start(
        locale: &str,
        engine: RequestedEngine,
        on_segment: impl Fn(&Segment) + Send + Sync + 'static,
        on_error: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        start_impl(locale, engine, on_segment, on_error)
    }

    /// Appends interleaved stereo Float32 PCM (48 kHz) to the recognizer.
    ///
    /// A no-op when `pcm` has fewer than 2 samples or the session is not
    /// accepting audio (after [`SpeechAnalyzer::finish`]).
    #[allow(clippy::missing_const_for_fn)] // macOS forwards to the C session; stubs are const-free
    pub fn feed(
        &self,
        pcm: &[f32],
    ) {
        feed_impl(self, pcm);
    }

    /// Signals end-of-audio and blocks until the final result, delivering any
    /// remaining segments to `on_segment` first.
    ///
    /// # Errors
    ///
    /// [`Error::Internal`] when the recognizer fails or does not finalize
    /// within 20 seconds.
    #[allow(clippy::missing_const_for_fn)] // macOS waits on the C session; stubs are const-free
    pub fn finish(&mut self) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        {
            finish_impl(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(Error::NotAvailable)
        }
    }

    /// The engine this session actually uses.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // macOS reads the C session; non-macOS has no state
    pub fn engine(&self) -> Engine {
        engine_impl(self)
    }
}

#[cfg(target_os = "macos")]
fn engine_impl(analyzer: &SpeechAnalyzer) -> Engine {
    let raw = unsafe {
        // SAFETY: the handle is valid for the analyzer's lifetime; C returns
        // the stored engine or -1 for an invalid handle.
        ffi::koe_speech_engine(analyzer.handle.as_ptr())
    };
    match raw {
        ffi::koe::ENGINE_ON_DEVICE => Engine::OnDevice,
        ffi::koe::ENGINE_NETWORK => Engine::Network,
        _ => Engine::Unavailable,
    }
}

#[cfg(not(target_os = "macos"))]
const fn engine_impl(_analyzer: &SpeechAnalyzer) -> Engine {
    Engine::Unavailable
}

#[cfg(target_os = "macos")]
impl Drop for SpeechAnalyzer {
    fn drop(&mut self) {
        // SAFETY: `self` uniquely owns the handle; destroy cancels the task
        // and joins any in-flight callback before freeing the session.
        unsafe { ffi::koe_speech_destroy(self.handle.as_ptr()) };
    }
}

#[cfg(target_os = "macos")]
fn start_impl(
    locale: &str,
    engine: RequestedEngine,
    on_segment: impl Fn(&Segment) + Send + Sync + 'static,
    on_error: impl Fn(&str) + Send + Sync + 'static,
) -> Result<SpeechAnalyzer, Error> {
    if locale.trim().is_empty() {
        return Err(Error::UnsupportedLocale(locale.to_owned()));
    }
    let Ok(locale_c) = CString::new(locale) else {
        // Interior NUL: the locale is not a valid C string.
        return Err(Error::Internal("locale contains a NUL byte".to_owned()));
    };

    let resolved = match engine {
        RequestedEngine::Network => resolve_engine(engine, Engine::Unavailable)?,
        RequestedEngine::Auto | RequestedEngine::OnDevice => {
            resolve_engine(engine, cached_engine(locale))?
        },
    };
    let engine_raw = match resolved {
        ResolvedEngine::OnDevice => ffi::koe::ENGINE_ON_DEVICE,
        ResolvedEngine::Network => ffi::koe::ENGINE_NETWORK,
    };

    let mut ctx = Box::new(SessionCtx {
        on_segment: Box::new(on_segment),
        on_error: Box::new(on_error),
        finished: Mutex::new(None),
    });
    let ctx_ptr: *mut c_void = (&raw mut *ctx).cast::<c_void>();
    let mut handle: *mut ffi::KoeSpeechSession = ptr::null_mut();
    let code = unsafe {
        // SAFETY: `ctx` outlives the handle (stored in `SpeechAnalyzer`); the
        // trampoline only reads it, and destroy joins in-flight deliveries.
        ffi::koe_speech_create(
            locale_c.as_ptr(),
            engine_raw,
            Some(session_trampoline),
            ctx_ptr,
            (&raw mut handle).cast(),
        )
    };
    if code != ffi::koe::ERROR_NONE {
        return Err(map_create_error(code, locale));
    }
    // SAFETY: a successful create always writes a valid non-null handle.
    let handle = NonNull::new(handle)
        .ok_or_else(|| Error::Internal("speech session create returned null".to_owned()))?;

    Ok(SpeechAnalyzer { handle, ctx })
}

#[cfg(target_os = "macos")]
fn cached_engine(locale: &str) -> Engine {
    static ENGINE_CACHE: OnceLock<Engine> = OnceLock::new();
    *ENGINE_CACHE.get_or_init(|| probe(locale))
}

#[cfg(target_os = "macos")]
fn feed_impl(
    analyzer: &SpeechAnalyzer,
    pcm: &[f32],
) {
    if pcm.len() < CHANNELS {
        return;
    }
    let frames = pcm.len() / CHANNELS;
    // SAFETY: the session handle is valid for the analyzer's lifetime; the C
    // side caps `frames` to UINT32 and copies synchronously.
    unsafe {
        ffi::koe_speech_feed(analyzer.handle.as_ptr(), pcm.as_ptr(), frames);
    }
}

#[cfg(target_os = "macos")]
fn finish_impl(analyzer: &mut SpeechAnalyzer) -> Result<(), Error> {
    // SAFETY: only the owning thread calls this; the C side guards against
    // concurrent double-finish.
    unsafe { ffi::koe_speech_finish(analyzer.handle.as_ptr()) };

    let deadline = Instant::now() + FINALIZE_WAIT;
    loop {
        let outcome = analyzer
            .ctx
            .finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(outcome) = outcome {
            return outcome.map_err(Error::Internal);
        }
        if Instant::now() >= deadline {
            return Err(Error::Internal(
                "timed out waiting for the speech recognizer".to_owned(),
            ));
        }
        // SAFETY: pumps this thread's run loop; the FINISHED event is
        // delivered on the recognizer's own serial queue.
        unsafe { ffi::koe_speech_runloop_step(RUNLOOP_STEP) };
    }
}

#[cfg(not(target_os = "macos"))]
fn start_impl(
    locale: &str,
    engine: RequestedEngine,
    on_segment: impl Fn(&Segment) + Send + Sync + 'static,
    on_error: impl Fn(&str) + Send + Sync + 'static,
) -> Result<SpeechAnalyzer, Error> {
    let _ = (locale, engine, on_segment, on_error);
    Err(Error::NotAvailable)
}

#[cfg(not(target_os = "macos"))]
const fn feed_impl(
    _analyzer: &SpeechAnalyzer,
    _pcm: &[f32],
) {
}

// ---------------------------------------------------------------------------
// FFI callback contexts and trampolines (macOS)
// ---------------------------------------------------------------------------

/// Accumulates raw locale identifiers during [`crate::supported_locales`].
#[cfg(target_os = "macos")]
struct LocaleCtx {
    locales: Mutex<Vec<String>>,
}

/// Receives a probe verdict.
#[cfg(target_os = "macos")]
struct ProbeCtx {
    verdict: Mutex<Option<ProbeVerdict>>,
}

/// Copies a NUL-terminated UTF-8 string, tolerating lossy sequences and NULL.
#[cfg(target_os = "macos")]
fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let bytes = unsafe {
        // SAFETY: the C side guarantees `ptr` is a valid NUL-terminated C
        // string for the duration of the callback.
        CStr::from_ptr(ptr)
    }
    .to_bytes();
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(target_os = "macos")]
fn segment_from_ffi(src: &ffi::KoeSpeechSegment) -> Segment {
    Segment {
        text: cstr_to_string(src.text),
        start_ms: src.start_ms,
        end_ms: src.end_ms,
        is_final: src.is_final != 0,
        confidence: src.confidence,
    }
}

/// Fires once per locale identifier during [`crate::supported_locales`].
#[cfg(target_os = "macos")]
unsafe extern "C" fn locale_trampoline(
    user_data: *mut c_void,
    locale_identifier: *const c_char,
) {
    let ctx = unsafe { &*user_data.cast::<LocaleCtx>() };
    ctx.locales
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(cstr_to_string(locale_identifier));
}

/// Fires at most once with a probe verdict.
#[cfg(target_os = "macos")]
unsafe extern "C" fn probe_trampoline(
    user_data: *mut c_void,
    probe_result: c_int,
    error_message: *const c_char,
) {
    let ctx = unsafe { &*user_data.cast::<ProbeCtx>() };
    let verdict = match probe_result {
        ffi::koe::PROBE_WORKS => ProbeVerdict::Works,
        ffi::koe::PROBE_RECOVERABLE => ProbeVerdict::Recoverable(cstr_to_string(error_message)),
        ffi::koe::PROBE_FAILED => ProbeVerdict::Failed,
        _ => ProbeVerdict::Unknown,
    };
    *ctx.verdict.lock().unwrap_or_else(PoisonError::into_inner) = Some(verdict);
}

/// Receives segments / terminal / error events from a session.
#[cfg(target_os = "macos")]
unsafe extern "C" fn session_trampoline(
    user_data: *mut c_void,
    callback_type: c_int,
    segment: *const ffi::KoeSpeechSegment,
    error_message: *const c_char,
    done_ok: c_int,
) {
    let ctx = unsafe { &*user_data.cast::<SessionCtx>() };
    match callback_type {
        ffi::koe::CALLBACK_SEGMENT => {
            if !segment.is_null() {
                let value = segment_from_ffi(unsafe {
                    // SAFETY: the C side passes a pointer to a stack segment
                    // valid for this call. `value` copies the string inside.
                    &*segment
                });
                (ctx.on_segment)(&value);
            }
        },
        ffi::koe::CALLBACK_FINISHED => {
            let outcome = if done_ok != 0 {
                Ok(())
            } else {
                Err(cstr_to_string(error_message))
            };
            *ctx.finished.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        },
        ffi::koe::CALLBACK_ERROR => {
            let message = cstr_to_string(error_message);
            (ctx.on_error)(&message);
        },
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_bcp47_normalizes_underscore() {
        assert_eq!(to_bcp47("en_US"), "en-US");
        assert_eq!(to_bcp47("zh_Hans_CN"), "zh-Hans-CN");
        assert_eq!(to_bcp47("ja-JP"), "ja-JP");
    }

    #[test]
    fn recoverable_engine_errors_are_classified() {
        assert!(is_recoverable_engine_error(
            "Siri and Dictation are disabled"
        ));
        assert!(is_recoverable_engine_error(
            "Dictation is not available for this language"
        ));
        assert!(is_recoverable_engine_error(
            "on-device speech recognition is unavailable"
        ));
        assert!(!is_recoverable_engine_error("No speech detected"));
        assert!(!is_recoverable_engine_error("Connection failed"));
        assert!(!is_recoverable_engine_error(""));
    }

    #[test]
    fn probe_verdicts_map_to_engines() {
        assert_eq!(classify_probe(ProbeVerdict::Works), Engine::OnDevice);
        assert_eq!(
            classify_probe(ProbeVerdict::Recoverable(
                "Siri and Dictation are disabled".into()
            )),
            Engine::Network
        );
        assert_eq!(
            classify_probe(ProbeVerdict::Recoverable("boom".into())),
            Engine::Unavailable
        );
        assert_eq!(classify_probe(ProbeVerdict::Failed), Engine::Unavailable);
        assert_eq!(classify_probe(ProbeVerdict::Unknown), Engine::Unavailable);
    }

    #[test]
    fn requested_engines_resolve_against_probes() {
        assert_eq!(
            resolve_engine(RequestedEngine::Network, Engine::Unavailable).unwrap(),
            ResolvedEngine::Network
        );
        assert_eq!(
            resolve_engine(RequestedEngine::Auto, Engine::OnDevice).unwrap(),
            ResolvedEngine::OnDevice
        );
        assert_eq!(
            resolve_engine(RequestedEngine::Auto, Engine::Network).unwrap(),
            ResolvedEngine::Network
        );
        assert!(matches!(
            resolve_engine(RequestedEngine::Auto, Engine::Unavailable),
            Err(Error::NotAvailable)
        ));
        assert!(matches!(
            resolve_engine(RequestedEngine::OnDevice, Engine::Network),
            Err(Error::OnDeviceUnavailable { .. })
        ));
    }
}
