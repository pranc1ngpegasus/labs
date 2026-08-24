//! Oto encode — audio conversion, encoders, and containers.
//!
//! Platform-agnostic layer (design 02/04): turns captured PCM into the encoder
//! input format and writes WAV / Ogg+Opus output. Kept independent of device
//! and CLI concerns so it can be tested headlessly.

use std::fmt::Display;
use std::io;

use thiserror::Error;

pub mod convert;
pub mod ogg_opus;
pub mod wav;

pub use convert::{ConvertError, Converter};
pub use ogg_opus::{OggOpusEncoder, Tags};
pub use wav::WavEncoder;

/// PCM sample format delivered by the capture device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmFormat {
    /// Signed 16-bit little-endian samples.
    S16,
    /// IEEE 32-bit float samples in `[-1, 1]`.
    F32,
}

/// A captured audio chunk, decoupled from the capture crate so the encoder
/// layer stays platform-agnostic (design 02).
#[derive(Debug, Clone, Copy)]
pub struct AudioChunk<'a> {
    /// Raw interleaved PCM bytes in `format`.
    pub data: &'a [u8],
    /// Sample format of `data`.
    pub format: PcmFormat,
    /// Sample rate in hertz (the device's actual rate).
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u8,
}

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
    /// Sample conversion failed before reaching the encoder.
    #[error("conversion error: {0}")]
    Convert(#[from] ConvertError),
}

/// Encodes captured audio into an output container.
///
/// Implementations are owned by a single consumer thread. [`Self::write`]
/// accepts one captured chunk; the WAV writer appends the raw (format-preserving)
/// bytes as-is, while the Opus writer consumes the caller-converted interleaved
/// i16 samples passed in `converted` and buffers to exactly one encoder frame
/// per packet. `converted` is `None` for the WAV path, which does no conversion.
pub trait AudioEncoder: Send {
    /// Returns the encoder's output constraints.
    fn spec(&self) -> EncoderSpec;

    /// Writes one captured chunk.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when encoding or writing fails. The writer is left in
    /// an unspecified state after an error.
    fn write(
        &mut self,
        chunk: &AudioChunk<'_>,
        converted: Option<&[i16]>,
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
