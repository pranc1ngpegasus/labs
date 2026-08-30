//! Raw C FFI surface generated from `speech.h` (macOS only).
//!
//! What `audioCoreaudio.m` is to `FfiCaptureImpl` in shiguredo/audio-device-rs,
//! this module is to the safe wrapper in [`crate`]. The generated bindings are
//! lint-suppressed wholesale; the safe API in `lib.rs` is what callers see.

#[cfg(target_os = "macos")]
mod bindings {
    #![allow(
        clippy::all,
        clippy::nursery,
        clippy::pedantic,
        dead_code,
        non_camel_case_types,
        non_snake_case,
        non_upper_case_globals
    )]
    include!(concat!(env!("OUT_DIR"), "/speech_bindings.rs"));
}

#[cfg(target_os = "macos")]
pub use bindings::{
    KoeSpeechSegment, KoeSpeechSession, koe_speech_authorization_status, koe_speech_create,
    koe_speech_destroy, koe_speech_engine, koe_speech_feed, koe_speech_finish,
    koe_speech_probe_cancel, koe_speech_probe_start, koe_speech_runloop_step,
    koe_speech_supported_locales,
};

/// Callback/engine/error codes mirrored from the header.
///
/// bindgen also emits these as enum modules; the ints are duplicated here so
/// the safe API does not depend on bindgen's exact naming/layout.
#[cfg(target_os = "macos")]
pub mod koe {
    use std::ffi::c_int;

    pub const CALLBACK_SEGMENT: c_int = 0;
    pub const CALLBACK_FINISHED: c_int = 1;
    pub const CALLBACK_ERROR: c_int = 2;

    pub const PROBE_WORKS: c_int = 1;
    pub const PROBE_RECOVERABLE: c_int = 2;
    pub const PROBE_FAILED: c_int = 3;

    pub const ENGINE_ON_DEVICE: c_int = 0;
    pub const ENGINE_NETWORK: c_int = 1;

    pub const ERROR_NONE: c_int = 0;
    pub const ERROR_PERMISSION_DENIED: c_int = 1;
    pub const ERROR_UNSUPPORTED_LOCALE: c_int = 2;
    pub const ERROR_NOT_AVAILABLE: c_int = 3;
    pub const ERROR_ENGINE: c_int = 4;
    pub const ERROR_INVALID_ARGUMENT: c_int = 5;

    pub const AUTHORIZATION_DENIED: c_int = 1;
    pub const AUTHORIZATION_RESTRICTED: c_int = 2;
    pub const AUTHORIZATION_AUTHORIZED: c_int = 3;
}
