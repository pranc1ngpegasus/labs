//! Errors surfaced by the Speech wrapper.
//!
//! Zero-dependency (no `thiserror`): implementing [`std::error::Error`] by hand
//! keeps the crate's runtime dependency tree empty.

use std::fmt;

/// Errors returned by the safe [`crate::SpeechAnalyzer`] API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Speech recognition authorization is not granted.
    PermissionDenied(String),
    /// The locale is unknown to the Speech framework.
    UnsupportedLocale(String),
    /// Speech services are unavailable on this host/build.
    NotAvailable,
    /// On-device recognition was requested but the host cannot run it.
    OnDeviceUnavailable { msg: String },
    /// Any other failure (engine error, timeout, invalid input).
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::PermissionDenied(msg) => write!(f, "Permission denied: {msg}"),
            Self::UnsupportedLocale(locale) => write!(f, "Unsupported locale: {locale}"),
            Self::NotAvailable => write!(f, "Speech recognition not available"),
            Self::OnDeviceUnavailable { msg } => {
                write!(f, "On-device recognition unavailable: {msg}")
            },
            Self::Internal(msg) => write!(f, "Transcription internal error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn display_messages_are_distinct() {
        assert_eq!(
            Error::PermissionDenied("speech".into()).to_string(),
            "Permission denied: speech"
        );
        assert_eq!(
            Error::UnsupportedLocale("xx".into()).to_string(),
            "Unsupported locale: xx"
        );
        assert_eq!(
            Error::NotAvailable.to_string(),
            "Speech recognition not available"
        );
        assert_eq!(
            Error::OnDeviceUnavailable {
                msg: "enable dictation".into()
            }
            .to_string(),
            "On-device recognition unavailable: enable dictation"
        );
        assert_eq!(
            Error::Internal("asr".into()).to_string(),
            "Transcription internal error: asr"
        );
    }
}
