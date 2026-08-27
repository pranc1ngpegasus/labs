//! Sample conversion: format (S16/F32 → i16), channel downmix, and resampling.
//!
//! The capture layer delivers chunks at the device's *actual* sample rate,
//! channel count, and format (which may differ from what we asked for on some
//! backends, design 02/04). A [`Converter`] turns each chunk into interleaved
//! i16 PCM matching the encoder's fixed target rate/channels.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use thiserror::Error;

use crate::{AudioChunk, PcmFormat};

/// Sample rates the Opus codec supports (RFC 6716). Any other input rate is
/// resampled to [`OPUS_FALLBACK_RATE`] before encoding.
pub const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// Fallback target rate for inputs outside [`OPUS_RATES`].
pub const OPUS_FALLBACK_RATE: u32 = 48_000;

/// Returns whether `rate` is directly encodable by Opus.
#[must_use]
pub const fn is_opus_rate(rate: u32) -> bool {
    matches!(rate, 8_000 | 12_000 | 16_000 | 24_000 | 48_000)
}

/// The Opus encoder rate for a device rate: the device rate if Opus-supported,
/// otherwise [`OPUS_FALLBACK_RATE`].
#[must_use]
pub const fn opus_target_rate(rate: u32) -> u32 {
    if is_opus_rate(rate) {
        rate
    } else {
        OPUS_FALLBACK_RATE
    }
}

/// Effective Opus channel count: never more than the device actually delivers,
/// and capped at the user's requested count (`requested.min(actual)`).
#[must_use]
pub const fn opus_target_channels(
    actual: u8,
    requested: u8,
) -> u8 {
    if actual < requested {
        actual
    } else {
        requested
    }
}

/// Converts interleaved f32 PCM into interleaved i16 (clamped, NaN→0).
fn f32_to_i16(
    pcm: &[f32],
    out: &mut Vec<i16>,
) {
    out.reserve(pcm.len());
    for &sample in pcm {
        let clamped = if sample.is_nan() {
            0.0
        } else {
            sample.clamp(-1.0, 1.0)
        };
        out.push((clamped * i16::MAX as f32) as i16);
    }
}

/// Downmix to `target_channels`, averaging the source channels that map to
/// each output channel.
///
/// `target` must be <= `source_channels`. Source channels are distributed
/// evenly across the target channels and each group averaged, so any source
/// layout (stereo, 3ch, 5.1, ...) reduces safely without out-of-bounds access.
fn downmix(
    pcm: &[f32],
    source_channels: usize,
    target_channels: usize,
    out: &mut Vec<f32>,
) {
    debug_assert!(target_channels <= source_channels && target_channels > 0);
    let frames = pcm.len() / source_channels;
    out.reserve(frames * target_channels);
    for frame in 0..frames {
        let base = frame * source_channels;
        for target in 0..target_channels {
            let start = target * source_channels / target_channels;
            let end = (target + 1) * source_channels / target_channels;
            let group = &pcm[base + start..base + end];
            let sum: f32 = group.iter().sum();
            out.push(sum / group.len() as f32);
        }
    }
}

/// Stateful resampler holding the FFT resampler and a partial-frame buffer.
struct ResamplerState {
    resampler: Fft<f32>,
    channels: usize,
    /// Interleaved f32 samples (downmixed) not yet consumed by a full chunk.
    pending: Vec<f32>,
}

/// Converts captured chunks into interleaved i16 PCM at a fixed target rate.
pub struct Converter {
    /// Target sample rate of the encoder.
    target_rate: u32,
    /// Target channel count of the encoder.
    target_channels: u8,
    /// Resampler when the device rate differs from `target_rate`.
    resampler: Option<ResamplerState>,
    /// Reused output buffer for i16 samples.
    i16_buf: Vec<i16>,
    /// Reused output buffer for f32 samples.
    f32_buf: Vec<f32>,
}

impl Converter {
    /// Creates a converter from the device's actual rate to the encoder's
    /// target rate/channels.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] when a resampler can't be constructed.
    pub fn new(
        input_rate: u32,
        target_rate: u32,
        target_channels: u8,
    ) -> Result<Self, ConvertError> {
        let resampler = if input_rate == target_rate {
            None
        } else {
            let resampler = Fft::<f32>::new(
                input_rate as usize,
                target_rate as usize,
                1_024,
                target_channels as usize,
                FixedSync::Input,
            )
            .map_err(|e| ConvertError::Resample(e.to_string()))?;
            Some(ResamplerState {
                resampler,
                channels: target_channels as usize,
                pending: Vec::new(),
            })
        };
        Ok(Self {
            target_rate,
            target_channels,
            resampler,
            i16_buf: Vec::new(),
            f32_buf: Vec::new(),
        })
    }

    /// The converter's target rate.
    #[must_use]
    pub const fn target_rate(&self) -> u32 {
        self.target_rate
    }

    /// The converter's target channel count.
    #[must_use]
    pub const fn target_channels(&self) -> u8 {
        self.target_channels
    }

    /// Converts a captured chunk into interleaved i16 PCM.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] when the chunk's channel count is below the
    /// target (upmix isn't supported) or conversion fails.
    pub fn convert_chunk(
        &mut self,
        chunk: &AudioChunk<'_>,
    ) -> Result<Vec<i16>, ConvertError> {
        if chunk.channels < self.target_channels {
            return Err(ConvertError::Upmix(chunk.channels, self.target_channels));
        }
        let target_channels = self.target_channels as usize;

        // Fast path: S16 at the target rate/channels needs no conversion, so
        // copy it verbatim (preserves exact values including i16::MIN).
        if self.resampler.is_none()
            && chunk.format == PcmFormat::S16
            && chunk.channels == self.target_channels
        {
            let mut out = Vec::with_capacity(chunk.data.len() / 2);
            for pair in chunk.data.as_chunks::<2>().0 {
                out.push(i16::from_le_bytes([pair[0], pair[1]]));
            }
            return Ok(out);
        }

        // Decode interleaved samples into f32.
        self.f32_buf.clear();
        match chunk.format {
            PcmFormat::S16 => {
                if !chunk.data.len().is_multiple_of(2) {
                    return Err(ConvertError::Format(PcmFormat::S16));
                }
                for pair in chunk.data.as_chunks::<2>().0 {
                    let s = i16::from_le_bytes([pair[0], pair[1]]);
                    self.f32_buf.push(s as f32 / i16::MAX as f32);
                }
            },
            PcmFormat::F32 => {
                if !chunk.data.len().is_multiple_of(4) {
                    return Err(ConvertError::Format(PcmFormat::F32));
                }
                for quad in chunk.data.as_chunks::<4>().0 {
                    let s = f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]);
                    self.f32_buf.push(s);
                }
            },
        }

        // Downmix to target channels if the device delivers more.
        if chunk.channels > self.target_channels {
            let downmixed = std::mem::take(&mut self.f32_buf);
            let mut out = Vec::new();
            downmix(
                &downmixed,
                chunk.channels as usize,
                target_channels,
                &mut out,
            );
            self.f32_buf = out;
        }

        let Some(state) = &mut self.resampler else {
            self.i16_buf.clear();
            f32_to_i16(&self.f32_buf, &mut self.i16_buf);
            return Ok(std::mem::take(&mut self.i16_buf));
        };

        state.pending.extend_from_slice(&self.f32_buf);
        let mut result = Vec::new();
        drain_resampler(state, false, &mut result)?;
        Ok(result)
    }

    /// Flushes any buffered audio and drains the resampler, returning the
    /// final interleaved i16 samples.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] when the resampler fails to process the tail.
    pub fn flush(&mut self) -> Result<Vec<i16>, ConvertError> {
        let Some(state) = &mut self.resampler else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        drain_resampler(state, true, &mut result)?;
        Ok(result)
    }
}

/// Drains full resampler chunks from `pending`; when `partial` is set,
/// processes any remaining short tail as a final partial chunk.
fn drain_resampler(
    state: &mut ResamplerState,
    partial: bool,
    out: &mut Vec<i16>,
) -> Result<(), ConvertError> {
    let input_frames = state.resampler.input_frames_next();
    let channels = state.channels;

    while state.pending.len() >= input_frames * channels {
        let tail = state.pending.split_off(input_frames * channels);
        let buf = std::mem::take(&mut state.pending);
        let input = InterleavedOwned::new_from(buf, channels, input_frames)
            .map_err(|e| ConvertError::Resample(e.to_string()))?;
        let output = state
            .resampler
            .process(&input, None)
            .map_err(|e| ConvertError::Resample(e.to_string()))?;
        let out_f32 = output.take_data();
        f32_to_i16(&out_f32, out);
        state.pending = tail;
    }

    if partial && !state.pending.is_empty() {
        let available = state.pending.len() / channels;
        // Pad to a full input chunk; `partial_len` tells the resampler how
        // many leading frames are valid.
        let mut buf = std::mem::take(&mut state.pending);
        buf.resize(input_frames * channels, 0.0);
        let input = InterleavedOwned::new_from(buf, channels, input_frames)
            .map_err(|e| ConvertError::Resample(e.to_string()))?;
        let indexing = Indexing::new().partial_len(available);
        let output = state
            .resampler
            .process(&input, Some(&indexing))
            .map_err(|e| ConvertError::Resample(e.to_string()))?;
        let out_f32 = output.take_data();
        f32_to_i16(&out_f32, out);
        state.pending.clear();
    }

    Ok(())
}

/// Errors from sample conversion.
#[derive(Debug, Error)]
pub enum ConvertError {
    /// The chunk's channel count is below the target and upmixing isn't
    /// supported.
    #[error("cannot upmix {0} channels to {1}")]
    Upmix(u8, u8),
    /// The chunk uses an unexpected or mis-sized sample format.
    #[error("unsupported sample format: {0:?}")]
    Format(PcmFormat),
    /// A resampler couldn't be built or failed to process.
    #[error("resampler error: {0}")]
    Resample(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16_chunk(
        data: Vec<i16>,
        channels: u8,
        sample_rate: u32,
    ) -> AudioChunk<'static> {
        let mut bytes = Vec::with_capacity(data.len() * 2);
        for s in data {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        AudioChunk {
            data: Box::leak(bytes.into_boxed_slice()),
            format: PcmFormat::S16,
            sample_rate,
            channels,
        }
    }

    fn f32_chunk(
        data: Vec<f32>,
        channels: u8,
        sample_rate: u32,
    ) -> AudioChunk<'static> {
        let mut bytes = Vec::with_capacity(data.len() * 4);
        for s in data {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        AudioChunk {
            data: Box::leak(bytes.into_boxed_slice()),
            format: PcmFormat::F32,
            sample_rate,
            channels,
        }
    }

    #[test]
    fn s16_passthrough_same_rate_and_channels() {
        let mut conv = Converter::new(48_000, 48_000, 1).expect("converter");
        let input = vec![0_i16, 32767, -32768, 100];
        let out = conv
            .convert_chunk(&s16_chunk(input.clone(), 1, 48_000))
            .expect("convert");
        assert_eq!(out, input);
    }

    #[test]
    fn f32_converts_to_i16_with_clamp_and_nan() {
        let mut conv = Converter::new(48_000, 48_000, 1).expect("converter");
        let chunk = f32_chunk(vec![0.0, 1.0, -1.0, 2.0, f32::NAN], 1, 48_000);
        let out = conv.convert_chunk(&chunk).expect("convert");
        assert_eq!(out, vec![0, 32767, -32767, 32767, 0]);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        let mut conv = Converter::new(48_000, 48_000, 1).expect("converter");
        let chunk = f32_chunk(vec![0.0, 0.0, 0.5, 0.5, 1.0, -1.0], 2, 48_000);
        let out = conv.convert_chunk(&chunk).expect("convert");
        assert_eq!(out, vec![0, 16383, 0]);
    }

    #[test]
    fn downmixes_three_channels_to_stereo() {
        let mut conv = Converter::new(48_000, 48_000, 2).expect("converter");
        // 3ch frame: L=0.5, C=-0.5, R=0.5.
        let chunk = f32_chunk(vec![0.5, -0.5, 0.5], 3, 48_000);
        let out = conv.convert_chunk(&chunk).expect("convert");
        // Even distribution: out[0] = ch0, out[1] = avg(ch1, ch2) = 0.0.
        assert_eq!(out, vec![16383, 0]);
    }

    #[test]
    fn rejects_upmix() {
        let mut conv = Converter::new(48_000, 48_000, 2).expect("converter");
        let err = conv
            .convert_chunk(&s16_chunk(vec![0, 1], 1, 48_000))
            .unwrap_err();
        assert!(matches!(err, ConvertError::Upmix(1, 2)));
    }

    #[test]
    fn resamples_44100_to_48000() {
        let mut conv = Converter::new(44_100, 48_000, 1).expect("converter");
        // One second of a 440 Hz sine at 44.1 kHz.
        let mut pcm = Vec::with_capacity(44_100);
        for i in 0..44_100 {
            let t = i as f32 / 44_100.0;
            pcm.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
        }
        let mut out = Vec::new();
        for chunk in pcm.chunks(4_410) {
            out.extend(
                conv.convert_chunk(&f32_chunk(chunk.to_vec(), 1, 44_100))
                    .expect("convert"),
            );
        }
        out.extend(conv.flush().expect("flush"));
        // Roughly one second of output.
        assert!(
            (48_000.0 - out.len() as f32).abs() < 2_000.0,
            "len {}",
            out.len()
        );
        assert!(out.iter().any(|&s| s.abs() > 10_000), "non-silent output");
    }

    #[test]
    fn opus_rate_helpers() {
        assert!(is_opus_rate(48_000));
        assert!(!is_opus_rate(44_100));
        assert_eq!(opus_target_rate(44_100), 48_000);
        assert_eq!(opus_target_rate(48_000), 48_000);
        assert_eq!(opus_target_channels(2, 1), 1);
        assert_eq!(opus_target_channels(2, 2), 2);
        assert_eq!(opus_target_channels(1, 2), 1);
    }
}
