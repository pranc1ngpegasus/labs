//! WAV container writer (RIFF / WAVE).
//!
//! Preserves the captured source format losslessly: `S16` frames are written as
//! PCM 16-bit (format 1) and `F32` frames as IEEE float 32-bit (format 3), at
//! the device's actual rate and channel count (design 04). No conversion or
//! resampling happens on this path.
#![allow(clippy::cast_possible_truncation)]

use std::io::{Seek, SeekFrom, Write};

use crate::{AudioChunk, PcmFormat};

use super::{EncoderSpec, EncoderStats, Error};

/// Audio format codes for the WAV `fmt` chunk.
const FORMAT_PCM: u16 = 1;
const FORMAT_IEEE_FLOAT: u16 = 3;
/// Bytes of the standard 44-byte header (RIFF + fmt + data headers).
const DATA_OFFSET: u64 = 44;

/// Writes captured frames to a WAV file, preserving the source format.
///
/// `W` must be a seekable writer so [`Self::finalize`] can rewrite the RIFF and
/// data chunk sizes; in tests this is a `Cursor<Vec<u8>>`.
pub struct WavEncoder<W> {
    writer: W,
    /// Actual rate/channels of the source (from the first frame).
    spec: EncoderSpec,
    /// Source sample format (set on the first written chunk).
    format: Option<PcmFormat>,
    /// Whether the header has been written yet.
    header_written: bool,
    /// Number of audio data bytes written.
    data_bytes: u64,
    finished: bool,
}

impl<W: Write + Seek> WavEncoder<W> {
    /// Creates a WAV encoder writing to `writer`.
    ///
    /// `spec` is the source's actual rate/channels (read from the device after
    /// capture start). The header is deferred until the first chunk so the
    /// sample format can be detected from the chunk itself.
    pub const fn new(
        writer: W,
        spec: EncoderSpec,
    ) -> Self {
        Self {
            writer,
            spec,
            format: None,
            header_written: false,
            data_bytes: 0,
            finished: false,
        }
    }

    /// The writer, consumed after [`Self::finalize`].
    #[must_use]
    pub fn into_inner(self) -> W {
        self.writer
    }

    fn write_header(
        &mut self,
        format: PcmFormat,
        sample_rate: u32,
        channels: u8,
    ) -> Result<(), Error> {
        let (audio_format, bits) = match format {
            PcmFormat::S16 => (FORMAT_PCM, 16_u16),
            PcmFormat::F32 => (FORMAT_IEEE_FLOAT, 32_u16),
        };
        let channels_u16 = u16::from(channels);
        let block_align = channels_u16 * (bits / 8);
        let byte_rate = sample_rate * u32::from(block_align);

        let mut header = Vec::with_capacity(DATA_OFFSET as usize);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&0_u32.to_le_bytes()); // chunk size (patched)
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&audio_format.to_le_bytes());
        header.extend_from_slice(&channels_u16.to_le_bytes());
        header.extend_from_slice(&sample_rate.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&block_align.to_le_bytes());
        header.extend_from_slice(&bits.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0_u32.to_le_bytes()); // data size (patched)

        self.writer.write_all(&header)?;
        self.spec = EncoderSpec {
            sample_rate,
            channels,
        };
        self.format = Some(format);
        self.header_written = true;
        Ok(())
    }
}

impl<W: Write + Seek + Send> super::AudioEncoder for WavEncoder<W> {
    fn spec(&self) -> EncoderSpec {
        self.spec
    }

    fn write(
        &mut self,
        chunk: &AudioChunk<'_>,
        _converted: Option<&[i16]>,
    ) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Unsupported(
                "WAV encoder already finalized".to_owned(),
            ));
        }
        if chunk.data.is_empty() {
            return Ok(());
        }
        let rate = chunk.sample_rate;
        let channels = chunk.channels;
        let format = chunk.format;

        if !self.header_written {
            self.write_header(format, rate, channels)?;
        } else if self.format != Some(format)
            || self.spec.sample_rate != rate
            || self.spec.channels != channels
        {
            // A WAV file can only hold one format; guard against a mid-stream
            // change (shouldn't happen in practice).
            return Err(Error::Unsupported(format!(
                "source format changed mid-stream ({format:?}, {rate} Hz, {channels} ch)"
            )));
        }

        self.writer.write_all(chunk.data)?;
        self.data_bytes += chunk.data.len() as u64;
        Ok(())
    }

    fn finalize(&mut self) -> Result<EncoderStats, Error> {
        if self.finished {
            return Ok(EncoderStats::default());
        }
        self.finished = true;
        // An empty recording must still produce a playable (silent) WAV: write a
        // PCM16 header so the file isn't 0 bytes and unparseable.
        if !self.header_written {
            self.write_header(PcmFormat::S16, self.spec.sample_rate, self.spec.channels)?;
        }
        if self.data_bytes > u64::from(u32::MAX) {
            return Err(Error::Unsupported(format!(
                "WAV data exceeds 4 GiB limit ({} bytes); split or use Ogg",
                self.data_bytes
            )));
        }
        let riff_size = 36 + self.data_bytes;
        let data_size = self.data_bytes as u32;

        self.writer.seek(SeekFrom::Start(4))?;
        self.writer.write_all(&(riff_size as u32).to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(40))?;
        self.writer.write_all(&data_size.to_le_bytes())?;
        self.writer.seek(SeekFrom::End(0))?;
        self.writer.flush()?;

        let block_align = u16::from(self.spec.channels)
            * match self.format {
                Some(PcmFormat::S16) | None => 2,
                Some(PcmFormat::F32) => 4,
            };
        let frames = if block_align == 0 {
            0
        } else {
            self.data_bytes / u64::from(block_align)
        };
        let duration_ms = if self.spec.sample_rate == 0 {
            0
        } else {
            frames * 1000 / u64::from(self.spec.sample_rate)
        };
        Ok(EncoderStats {
            frames,
            bytes: DATA_OFFSET + self.data_bytes,
            dropped: 0,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::AudioEncoder;

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

    fn encode_wav(chunk: &AudioChunk<'_>) -> Vec<u8> {
        let spec = EncoderSpec {
            sample_rate: chunk.sample_rate,
            channels: chunk.channels,
        };
        let mut enc = WavEncoder::new(Cursor::new(Vec::new()), spec);
        enc.write(chunk, None).expect("write");
        enc.finalize().expect("finalize");
        enc.into_inner().into_inner()
    }

    #[test]
    fn writes_pcm16_header_and_sizes() {
        let chunk = s16_chunk(vec![1, -1, 0, 32767], 1, 48_000);
        let bytes = encode_wav(&chunk);
        assert_eq!(bytes.len(), DATA_OFFSET as usize + chunk.data.len());

        // RIFF header.
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16);
        // audio_format = 1 (PCM), channels = 1, rate = 48000.
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        // byte_rate = 48000 * 2 = 96000.
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            96_000
        );
        // block_align = 2, bits = 16.
        assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(&bytes[36..40], b"data");
        // data size = 8 bytes (4 samples × 2).
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        // RIFF size = 36 + 8 = 44.
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 44);
    }

    #[test]
    fn writes_ieee_float32_format() {
        let chunk = f32_chunk(vec![0.5, -0.5], 1, 44_100);
        let bytes = encode_wav(&chunk);
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 3);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
    }

    #[test]
    fn stereo_channels_and_stats() {
        let chunk = s16_chunk(vec![1, 2, 3, 4], 2, 48_000);
        let bytes = encode_wav(&chunk);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            192_000
        );

        let spec = EncoderSpec {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut enc = WavEncoder::new(Cursor::new(Vec::new()), spec);
        enc.write(&chunk, None).expect("write");
        let stats = enc.finalize().expect("finalize");
        assert_eq!(stats.frames, 2);
        assert_eq!(stats.bytes, DATA_OFFSET + 8);
        assert_eq!(stats.duration_ms, 0); // 2 frames @ 48k ≈ 0 ms
    }

    #[test]
    fn empty_recording_still_writes_valid_header() {
        let spec = EncoderSpec {
            sample_rate: 48_000,
            channels: 1,
        };
        let mut enc = WavEncoder::new(Cursor::new(Vec::new()), spec);
        let stats = enc.finalize().expect("finalize");
        let bytes = enc.into_inner().into_inner();
        assert_eq!(bytes.len(), DATA_OFFSET as usize);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1); // PCM16
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(stats.bytes, DATA_OFFSET);
    }
}
