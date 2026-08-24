//! Oto encode — audio conversion, encoders, and containers.
//!
//! Platform-agnostic layer (design 02/04): turns captured PCM into the encoder
//! input format and writes WAV / Ogg+Opus output. Kept independent of device
//! and CLI concerns so it can be tested headlessly.

use std::fmt::Display;
use std::io;

use thiserror::Error;

/// Encoder input constraints (the actual capture rate and channel count,
/// which may differ from the configured values on some backends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSpec {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count (1 = mono, 2 = stereo).
    pub channels: u8,
}

/// Recording statistics reported by [`AudioEncoder::finalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncoderStats {
    /// Total number of audio frames written.
    pub frames: u64,
    /// Output file size in bytes.
    pub bytes: u64,
    /// Frames dropped by the capture pipeline due to backpressure.
    pub dropped: u64,
    /// Recording duration in milliseconds (`frames / rate * 1000`).
    pub duration_ms: u64,
}

/// Error type for encoding and container writing.
#[derive(Debug, Error)]
pub enum Error {
    /// File or container I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Encoder input outside the supported sample rates or channel counts.
    #[error("unsupported encoder input: {0}")]
    Unsupported(String),
    /// The underlying codec rejected a frame.
    #[error("codec error: {0}")]
    Codec(String),
}

/// Encodes interleaved i16 PCM into an audio container.
///
/// Implementations are owned by a single consumer thread. [`Self::write_pcm`]
/// accepts arbitrary chunk boundaries; the WAV writer appends as-is, while the
/// Opus writer buffers to exactly one encoder frame per packet.
pub trait AudioEncoder: Send {
    /// Returns the encoder's input constraints.
    fn spec(&self) -> EncoderSpec;

    /// Writes interleaved i16 PCM samples.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when encoding or writing fails. The writer is left in
    /// an unspecified state after an error.
    fn write_pcm(
        &mut self,
        pcm: &[i16],
    ) -> Result<(), Error>;

    /// Flushes remaining buffered audio and finalizes the container
    /// (header rewrite / final Ogg page), returning recording statistics.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when finalization fails.
    fn finalize(&mut self) -> Result<EncoderStats, Error>;
}

impl Display for EncoderSpec {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let channel_label = match self.channels {
            1 => "mono",
            2 => "stereo",
            other => return write!(f, "{other}ch"),
        };
        write!(f, "{channel_label} {} Hz", self.sample_rate)
    }
}
