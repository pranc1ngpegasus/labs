//! Oto core — recording pipeline and session control.
//!
//! Wires [`oto_capture`] and [`oto_encode`] together: a bounded channel with
//! drop-oldest backpressure, a consumer thread, and a recording session that
//! owns the capture-to-file lifecycle and statistics (design 02).
//!
//! The CLI talks to this crate only — never to the leaf crates directly.

pub use oto_capture::DeviceInfo;

pub use oto_capture::Error as CaptureError;

/// Enumerates the system's input devices.
///
/// Pass-through of [`oto_capture::enumerate_input_devices`], exposed so the
/// CLI keeps a single dependency on this crate.
///
/// # Errors
///
/// Returns [`CaptureError`] when device enumeration fails.
pub fn list_input_devices() -> Result<Vec<DeviceInfo>, CaptureError> {
    oto_capture::enumerate_input_devices()
}
