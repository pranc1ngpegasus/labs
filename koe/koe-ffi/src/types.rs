//! Shared FFI value types exported to Swift.

#[derive(Debug, Clone, uniffi::Enum)]
pub enum AudioSourceConfig {
    /// Capture system audio from a specific app via `ScreenCaptureKit`.
    AppAudio { bundle_id: String },
    /// Capture system audio from a specific process via Process Tap.
    PidAudio { pid: i32 },
    /// Capture microphone input.
    Microphone,
    /// Capture both system audio and microphone (AEC active).
    Both { bundle_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Permission {
    Microphone,
    ScreenRecording,
    Accessibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PermissionStatus {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub is_final: bool,
    pub confidence: f32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AppInfo {
    pub pid: i32,
    pub name: String,
    pub bundle_id: Option<String>,
    pub has_audio: bool,
}

/// Which speech-recognition engine a session should use.
///
/// `Auto` prefers on-device recognition and only falls back to network
/// recognition when the host cannot run on-device models (e.g. Dictation is
/// disabled). `OnDevice` refuses to send audio off-device and errors instead
/// of falling back. `Network` always uses server-side recognition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SpeechEngine {
    Auto,
    OnDevice,
    Network,
}

/// Default Core Audio device identity (name + persistent UID).
///
/// Rust hosts only (not a `UniFFI` record). Consumed by `koe info` and any
/// future Rust GUI that needs the same diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub uid: String,
}

impl std::fmt::Display for AudioDeviceInfo {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.uid)
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum OutputFormat {
    /// OGG Vorbis, quality-based VBR.
    ///
    /// `quality` is the libvorbis VBR quality in `[-0.1, 1.0]` (the value is
    /// validated when the encoder is created). `0.4` is Koe's speech-optimized
    /// default (~128 kbps nominal at 48 kHz stereo).
    Ogg { quality: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TranscriptFormat {
    Txt,
    Srt,
    Vtt,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecordingState {
    Recording,
    Paused,
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RecordingStatus {
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub level_left: f32,
    pub level_right: f32,
    pub state: RecordingState,
}
