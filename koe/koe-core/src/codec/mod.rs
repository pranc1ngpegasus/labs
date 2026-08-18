//! Audio encoding abstractions.

mod flac;
mod wav;

#[cfg(feature = "ogg")]
mod ogg;

use koe_ffi::OutputFormat;
use thiserror::Error;

pub use flac::FlacEncoder;
pub use wav::WavEncoder;

#[cfg(feature = "ogg")]
pub use ogg::{OggComments, OggEncoder};

/// Errors raised while encoding audio.
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("Encoder error: {0}")]
    Encoder(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Encodes canonical PCM into a container.
///
/// Input is 48 kHz interleaved `f32`, typically stereo. WAV may be constructed
/// mono via [`WavEncoder::with_channels`]; the pipeline / [`create_encoder`] path
/// stays stereo.
pub trait AudioEncoder: Send {
    /// Encode a chunk of PCM audio.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when encoding fails.
    fn encode(
        &mut self,
        pcm: &[f32],
    ) -> Result<Vec<u8>, CodecError>;

    /// Flush buffered frames and write any container trailer.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when finalization fails.
    fn finalize(&mut self) -> Result<Vec<u8>, CodecError>;
}

/// Creates an encoder for the requested output format.
///
/// When `comments` is `None`, a minimal default comment set is used for OGG and
/// FLAC. WAV ignores comments.
///
/// # Errors
///
/// Returns [`CodecError`] when the format is unsupported or encoder setup fails.
pub fn create_encoder(
    format: &OutputFormat,
    comments: Option<&OggComments>,
) -> Result<Box<dyn AudioEncoder>, CodecError> {
    match format {
        OutputFormat::Wav { bits_per_sample } => Ok(Box::new(WavEncoder::new(*bits_per_sample)?)),
        OutputFormat::Ogg { quality } => {
            let comments = comments.cloned().unwrap_or_else(OggComments::basic);
            create_ogg_encoder(*quality, &comments)
        },
        OutputFormat::Flac { compression_level } => {
            let comments = comments.cloned().unwrap_or_else(OggComments::basic);
            Ok(Box::new(FlacEncoder::with_comments(
                *compression_level,
                &comments,
            )?))
        },
    }
}

#[cfg(feature = "ogg")]
fn create_ogg_encoder(
    quality: f32,
    comments: &OggComments,
) -> Result<Box<dyn AudioEncoder>, CodecError> {
    Ok(Box::new(OggEncoder::with_comments(quality, comments)?))
}

#[cfg(not(feature = "ogg"))]
fn create_ogg_encoder(
    _quality: f32,
    _comments: &OggComments,
) -> Result<Box<dyn AudioEncoder>, CodecError> {
    Err(CodecError::Encoder(
        "OGG support requires the `ogg` feature".to_owned(),
    ))
}

#[cfg(not(feature = "ogg"))]
/// Vorbis Comment tags (no-op stub when the `ogg` feature is disabled).
#[derive(Debug, Clone, Default)]
pub struct OggComments;

#[cfg(not(feature = "ogg"))]
impl OggComments {
    /// Minimal tags used when no session metadata is available.
    #[must_use]
    pub fn basic() -> Self {
        Self
    }

    /// Builds session tags; ignored without the `ogg` feature.
    #[must_use]
    pub fn for_session(
        _source: &koe_ffi::AudioSourceConfig,
        _locale: &str,
    ) -> Self {
        Self
    }

    /// Vorbis Comment tag pairs shared with the FLAC encoder.
    #[must_use]
    pub const fn tag_pairs(&self) -> [(&'static str, &'static str); 6] {
        [
            ("TITLE", "Koe recording"),
            ("ARTIST", "Koe"),
            ("DATE", ""),
            ("DESCRIPTION", ""),
            ("ENCODER", "koe"),
            ("KOE_SOURCE", r#"{"type":"unknown"}"#),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_encoder_writes_header_on_finalize() {
        let mut encoder = WavEncoder::new(16).expect("wav encoder");
        let _ = encoder.encode(&[0.0, 0.0]).expect("encode");
        let trailer = encoder.finalize().expect("finalize");
        assert!(trailer.len() >= 44);
    }

    #[test]
    fn create_encoder_flac_emits_flac_magic() {
        let mut encoder = create_encoder(
            &OutputFormat::Flac {
                compression_level: 5,
            },
            None,
        )
        .expect("flac");
        let pcm = vec![0.0_f32; 4096 * 2];
        let _ = encoder.encode(&pcm).expect("encode");
        let out = encoder.finalize().expect("finalize");
        assert_eq!(&out[..4], b"fLaC");
    }

    #[cfg(feature = "ogg")]
    #[test]
    fn create_encoder_ogg_emits_ogg_capture_pattern() {
        let mut encoder = create_encoder(&OutputFormat::Ogg { quality: 0.4 }, None).expect("ogg");
        let pcm = vec![0.0_f32; 960 * 2];
        let mut out = encoder.encode(&pcm).expect("encode");
        out.extend(encoder.finalize().expect("finalize"));
        assert_eq!(&out[..4], b"OggS");
    }
}
