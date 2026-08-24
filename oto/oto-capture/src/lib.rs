//! Oto capture — device enumeration and microphone capture.
//!
//! Wraps [`shiguredo_audio_device`] so that platform-specific backend code and
//! feature selection stay confined to this crate. Device listing and capture
//! sessions land here (design 02).

mod capture;

use serde::Serialize;
use shiguredo_audio_device::AudioDeviceList;
use thiserror::Error;

pub use capture::CaptureSession;
pub use shiguredo_audio_device::{AudioCaptureConfig, AudioFormat, AudioFrameOwned};

/// A single enumerable input device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    /// Human-readable device name.
    pub name: String,
    /// Stable device identifier (backend-specific).
    pub unique_id: String,
    /// Number of channels reported by the device.
    pub channels: i32,
    /// Device sample rate in hertz.
    pub sample_rate: i32,
}

/// Errors from device enumeration and capture.
#[derive(Debug, Error)]
pub enum Error {
    /// Device enumeration or device property lookup failed.
    #[error("device enumeration failed: {0}")]
    Device(String),
}

/// Enumerates the system's input devices (microphones).
///
/// Uses the backend selected for the current platform (`CoreAudio` /
/// `PulseAudio` / `WASAPI`). The returned order is backend-defined.
///
/// # Errors
///
/// Returns [`Error::Device`] when enumeration or a device property lookup fails.
pub fn enumerate_input_devices() -> Result<Vec<DeviceInfo>, Error> {
    let devices = AudioDeviceList::enumerate_input().map_err(|e| Error::Device(e.to_string()))?;
    devices
        .into_iter()
        .map(|device| {
            Ok(DeviceInfo {
                name: device.name().map_err(|e| Error::Device(e.to_string()))?,
                unique_id: device
                    .unique_id()
                    .map_err(|e| Error::Device(e.to_string()))?,
                channels: device.channels(),
                sample_rate: device.sample_rate(),
            })
        })
        .collect()
}
