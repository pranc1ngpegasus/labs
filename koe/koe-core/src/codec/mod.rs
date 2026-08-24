//! Audio encoding abstractions.

mod ogg;

use koe_ffi::OutputFormat;
use thiserror::Error;

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
/// Input is 48 kHz interleaved `f32`, typically stereo.
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
/// When `comments` is `None`, a minimal default comment set is used.
///
/// # Errors
///
/// Returns [`CodecError`] when encoder setup fails.
pub fn create_encoder(
    format: &OutputFormat,
    comments: Option<&OggComments>,
) -> Result<Box<dyn AudioEncoder>, CodecError> {
    match format {
        OutputFormat::Ogg { quality } => {
            let comments = comments.cloned().unwrap_or_else(OggComments::basic);
            Ok(Box::new(OggEncoder::with_comments(*quality, &comments)?))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_encoder_ogg_emits_ogg_capture_pattern() {
        let mut encoder = create_encoder(&OutputFormat::Ogg { quality: 0.4 }, None).expect("ogg");
        let pcm = vec![0.0_f32; 960 * 2];
        let mut out = encoder.encode(&pcm).expect("encode");
        out.extend(encoder.finalize().expect("finalize"));
        assert_eq!(&out[..4], b"OggS");
    }
}
