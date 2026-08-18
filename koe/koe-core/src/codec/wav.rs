//! WAV (RIFF/WAVE) encoder — lossless PCM / IEEE float fallback.

use super::{AudioEncoder, CodecError};

const SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;

/// Bytes before the PCM payload: RIFF/WAVE + `fmt ` + `fact` + `data` headers.
const HEADER_LEN: usize = 56;
/// Value stored in the RIFF chunk size field for an empty `data` chunk (`HEADER_LEN - 8`).
const RIFF_SIZE_OVERHEAD: u32 = 48;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// Writes interleaved `f32` PCM into a RIFF/WAVE container.
///
/// Canonical pipeline input is 48 kHz stereo. Use [`WavEncoder::with_channels`] for
/// mono test fixtures; [`crate::codec::create_encoder`] always builds stereo because
/// [`koe_ffi::OutputFormat::Wav`] carries bit depth only.
///
/// Bit depth `32` means IEEE float (`WAVE_FORMAT_IEEE_FLOAT`); `16` / `24` mean
/// little-endian integer PCM. Integer paths clamp samples to `[-1.0, 1.0]` (NaN/Inf →
/// silence). Float samples are stored as-is except NaN/Inf → `0.0`.
///
/// # Memory
///
/// All PCM is buffered until [`AudioEncoder::finalize`]; `encode` always returns an
/// empty `Vec` for WAV. Prefer OGG/FLAC for long sessions (also avoids the 4 GiB
/// RIFF limit).
pub struct WavEncoder {
    bits_per_sample: u16,
    channel_count: u16,
    pcm_bytes: Vec<u8>,
    /// Sample frames written so far (one frame = `channel_count` samples).
    frame_count: u32,
    finished: bool,
}

impl WavEncoder {
    /// Creates a stereo WAV encoder for the given bit depth (`16`, `24`, or `32`).
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Encoder`] for unsupported bit depths.
    pub fn new(bits_per_sample: u16) -> Result<Self, CodecError> {
        Self::with_channels(bits_per_sample, DEFAULT_CHANNELS)
    }

    /// Creates a WAV encoder with an explicit channel count (`1` or `2`).
    ///
    /// Prefer [`Self::new`] for production paths; mono is mainly for fixtures.
    /// [`koe_ffi::OutputFormat::Wav`] does not encode channel count, so
    /// [`crate::codec::create_encoder`] cannot reconstruct a mono instance.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Encoder`] for unsupported bit depths or channel counts.
    pub fn with_channels(
        bits_per_sample: u16,
        channel_count: u16,
    ) -> Result<Self, CodecError> {
        if !matches!(bits_per_sample, 16 | 24 | 32) {
            return Err(CodecError::Encoder(format!(
                "unsupported WAV bit depth: {bits_per_sample} (expected 16, 24, or 32)"
            )));
        }
        if !matches!(channel_count, 1 | 2) {
            return Err(CodecError::Encoder(format!(
                "unsupported WAV channel count: {channel_count} (expected 1 or 2)"
            )));
        }
        Ok(Self {
            bits_per_sample,
            channel_count,
            pcm_bytes: Vec::new(),
            frame_count: 0,
            finished: false,
        })
    }

    fn bytes_per_sample(&self) -> usize {
        usize::from(self.bits_per_sample / 8)
    }

    const fn format_tag(&self) -> u16 {
        if self.bits_per_sample == 32 {
            WAVE_FORMAT_IEEE_FLOAT
        } else {
            WAVE_FORMAT_PCM
        }
    }

    fn append_sample(
        bits_per_sample: u16,
        sample: f32,
        out: &mut Vec<u8>,
    ) {
        match bits_per_sample {
            16 => {
                let finite = sanitize_for_pcm(sample);
                #[allow(clippy::cast_possible_truncation)]
                let value = (finite * f32::from(i16::MAX)) as i16;
                out.extend_from_slice(&value.to_le_bytes());
            },
            24 => {
                let finite = sanitize_for_pcm(sample);
                #[allow(clippy::cast_possible_truncation)]
                let value = (finite * 8_388_607.0) as i32;
                let bytes = value.to_le_bytes();
                out.extend_from_slice(&bytes[..3]);
            },
            32 => {
                // IEEE float WAV allows peaks outside [-1, 1]; only scrub non-finite.
                let value = if sample.is_finite() { sample } else { 0.0 };
                out.extend_from_slice(&value.to_le_bytes());
            },
            _ => unreachable!("bit depth validated in constructor"),
        }
    }

    fn build_header(
        &self,
        data_size: u32,
    ) -> Result<Vec<u8>, CodecError> {
        let bytes_per_sample = u32::from(self.bits_per_sample / 8);
        let channels = u32::from(self.channel_count);
        let byte_rate = SAMPLE_RATE * channels * bytes_per_sample;
        let block_align = self.channel_count * (self.bits_per_sample / 8);
        let riff_size = RIFF_SIZE_OVERHEAD
            .checked_add(data_size)
            .ok_or_else(|| CodecError::Encoder("WAV RIFF size exceeds u32::MAX".to_owned()))?;

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&riff_size.to_le_bytes());
        header.extend_from_slice(b"WAVE");

        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&self.format_tag().to_le_bytes());
        header.extend_from_slice(&self.channel_count.to_le_bytes());
        header.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&block_align.to_le_bytes());
        header.extend_from_slice(&self.bits_per_sample.to_le_bytes());

        // Required by the task (and by WAVE for non-PCM / IEEE float).
        header.extend_from_slice(b"fact");
        header.extend_from_slice(&4u32.to_le_bytes());
        header.extend_from_slice(&self.frame_count.to_le_bytes());

        header.extend_from_slice(b"data");
        header.extend_from_slice(&data_size.to_le_bytes());
        debug_assert_eq!(header.len(), HEADER_LEN);
        Ok(header)
    }

    fn ensure_riff_capacity(
        &self,
        additional_pcm_bytes: usize,
    ) -> Result<(), CodecError> {
        let total = HEADER_LEN
            .checked_add(self.pcm_bytes.len())
            .and_then(|n| n.checked_add(additional_pcm_bytes))
            .ok_or_else(|| CodecError::Encoder("WAV size overflow".to_owned()))?;
        // RIFF chunk size is a u32 counting all bytes after the size field.
        let riff_size = total
            .checked_sub(8)
            .ok_or_else(|| CodecError::Encoder("WAV size underflow".to_owned()))?;
        if riff_size > u32::MAX as usize {
            return Err(CodecError::Encoder(
                "WAV payload would exceed the 4 GiB RIFF limit; use OGG or FLAC instead".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Clamp finite samples to `[-1, 1]` for integer PCM; non-finite → silence.
const fn sanitize_for_pcm(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

impl AudioEncoder for WavEncoder {
    fn encode(
        &mut self,
        pcm: &[f32],
    ) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Err(CodecError::Encoder(
                "WAV encoder already finalized".to_owned(),
            ));
        }
        if pcm.is_empty() {
            return Ok(Vec::new());
        }
        let channels = usize::from(self.channel_count);
        if !pcm.len().is_multiple_of(channels) {
            return Err(CodecError::Encoder(format!(
                "PCM length {} is not a multiple of {channels} channels",
                pcm.len()
            )));
        }

        let add_bytes = pcm
            .len()
            .checked_mul(self.bytes_per_sample())
            .ok_or_else(|| CodecError::Encoder("WAV size overflow".to_owned()))?;
        self.ensure_riff_capacity(add_bytes)?;

        let frames = pcm.len() / channels;
        let new_frames = u32::try_from(frames)
            .map_err(|_| CodecError::Encoder("WAV frame count exceeds u32::MAX".to_owned()))?;
        self.frame_count = self
            .frame_count
            .checked_add(new_frames)
            .ok_or_else(|| CodecError::Encoder("WAV frame count exceeds u32::MAX".to_owned()))?;

        self.pcm_bytes.reserve(add_bytes);
        let bits = self.bits_per_sample;
        for sample in pcm {
            Self::append_sample(bits, *sample, &mut self.pcm_bytes);
        }
        // Buffer until finalize — streaming pages are not emitted for WAV.
        Ok(Vec::new())
    }

    fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            // Idempotent flush (matches OGG): second call yields an empty trailer.
            return Ok(Vec::new());
        }

        let data_size = u32::try_from(self.pcm_bytes.len())
            .map_err(|_| CodecError::Encoder("WAV payload exceeds u32::MAX".to_owned()))?;
        self.ensure_riff_capacity(0)?;

        let mut out = self.build_header(data_size)?;
        out.append(&mut self.pcm_bytes);
        self.finished = true;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn read_u16(
        buf: &[u8],
        at: usize,
    ) -> u16 {
        u16::from_le_bytes(buf[at..at + 2].try_into().expect("u16"))
    }

    fn read_u32(
        buf: &[u8],
        at: usize,
    ) -> u32 {
        u32::from_le_bytes(buf[at..at + 4].try_into().expect("u32"))
    }

    fn find_chunk(
        wav: &[u8],
        id: [u8; 4],
    ) -> &[u8] {
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let mut offset = 12usize;
        while offset + 8 <= wav.len() {
            let chunk_id = &wav[offset..offset + 4];
            let size = read_u32(wav, offset + 4) as usize;
            let data_start = offset + 8;
            let data_end = data_start + size;
            assert!(data_end <= wav.len(), "chunk overruns file");
            if chunk_id == id {
                return &wav[data_start..data_end];
            }
            // Chunks are word-aligned.
            offset = data_end + (size % 2);
        }
        panic!("chunk {} not found", String::from_utf8_lossy(&id));
    }

    fn sine(
        frames: usize,
        channels: u16,
        freq_hz: f32,
    ) -> Vec<f32> {
        let ch = usize::from(channels);
        let mut pcm = Vec::with_capacity(frames * ch);
        for i in 0..frames {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / SAMPLE_RATE as f32;
            let sample = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.5;
            for _ in 0..ch {
                pcm.push(sample);
            }
        }
        pcm
    }

    fn encode_all(
        bits: u16,
        channels: u16,
        pcm: &[f32],
    ) -> Vec<u8> {
        let mut encoder = WavEncoder::with_channels(bits, channels).expect("encoder");
        assert!(encoder.encode(pcm).expect("encode").is_empty());
        encoder.finalize().expect("finalize")
    }

    #[test]
    fn writes_fmt_fact_data_chunks_f32_stereo() {
        let wav = encode_all(32, 2, &sine(480, 2, 440.0));
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), HEADER_LEN + 480 * 2 * 4);
        assert_eq!(read_u32(&wav, 4), u32::try_from(wav.len() - 8).unwrap());

        let fmt = find_chunk(&wav, *b"fmt ");
        assert_eq!(fmt.len(), 16);
        assert_eq!(read_u16(fmt, 0), WAVE_FORMAT_IEEE_FLOAT);
        assert_eq!(read_u16(fmt, 2), 2);
        assert_eq!(read_u32(fmt, 4), SAMPLE_RATE);
        assert_eq!(read_u32(fmt, 8), SAMPLE_RATE * 2 * 4); // byte rate
        assert_eq!(read_u16(fmt, 12), 8); // block align: 2 ch * 4 bytes
        assert_eq!(read_u16(fmt, 14), 32);

        let fact = find_chunk(&wav, *b"fact");
        assert_eq!(fact.len(), 4);
        assert_eq!(read_u32(fact, 0), 480);

        let data = find_chunk(&wav, *b"data");
        assert_eq!(data.len(), 480 * 2 * 4);
    }

    #[test]
    fn writes_pcm_i16_and_i24() {
        let i16_wav = encode_all(16, 2, &[0.0, 0.0, 1.0, -1.0]);
        let fmt16 = find_chunk(&i16_wav, *b"fmt ");
        assert_eq!(read_u16(fmt16, 0), WAVE_FORMAT_PCM);
        assert_eq!(read_u16(fmt16, 14), 16);
        let data16 = find_chunk(&i16_wav, *b"data");
        assert_eq!(data16.len(), 8);
        assert_eq!(
            i16::from_le_bytes(data16[4..6].try_into().unwrap()),
            i16::MAX
        );
        assert_eq!(
            i16::from_le_bytes(data16[6..8].try_into().unwrap()),
            -i16::MAX
        );

        let i24_wav = encode_all(24, 2, &[0.0, 0.0]);
        let fmt24 = find_chunk(&i24_wav, *b"fmt ");
        assert_eq!(read_u16(fmt24, 0), WAVE_FORMAT_PCM);
        assert_eq!(read_u16(fmt24, 14), 24);
        assert_eq!(find_chunk(&i24_wav, *b"data").len(), 6);
        assert_eq!(read_u32(find_chunk(&i24_wav, *b"fact"), 0), 1);
    }

    #[test]
    fn float_preserves_peaks_outside_unit_range() {
        let wav = encode_all(32, 1, &[1.5]);
        let data = find_chunk(&wav, *b"data");
        let value = f32::from_le_bytes(data[0..4].try_into().unwrap());
        assert!((value - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn mono_input_writes_one_channel_fmt() {
        let wav = encode_all(32, 1, &sine(100, 1, 220.0));
        let fmt = find_chunk(&wav, *b"fmt ");
        assert_eq!(read_u16(fmt, 2), 1);
        assert_eq!(read_u32(find_chunk(&wav, *b"fact"), 0), 100);
        assert_eq!(find_chunk(&wav, *b"data").len(), 100 * 4);
    }

    #[test]
    fn rejects_bad_config_and_odd_pcm() {
        assert!(WavEncoder::new(8).is_err());
        assert!(WavEncoder::with_channels(16, 3).is_err());
        let mut encoder = WavEncoder::new(16).expect("encoder");
        let err = encoder.encode(&[0.0]).expect_err("odd");
        assert!(err.to_string().contains("multiple"));
    }

    #[test]
    fn encode_returns_empty_until_finalize() {
        let mut encoder = WavEncoder::new(32).expect("encoder");
        assert!(encoder.encode(&sine(960, 2, 440.0)).unwrap().is_empty());
        assert!(encoder.encode(&sine(960, 2, 440.0)).unwrap().is_empty());
        let out = encoder.finalize().unwrap();
        assert!(out.len() > HEADER_LEN);
        assert!(encoder.finalize().unwrap().is_empty());
        let err = encoder.encode(&[0.0, 0.0]).expect_err("after finalize");
        assert!(err.to_string().contains("finalized"));
    }

    #[test]
    fn empty_recording_still_writes_valid_header() {
        let mut encoder = WavEncoder::new(16).expect("encoder");
        let wav = encoder.finalize().expect("finalize");
        assert_eq!(wav.len(), HEADER_LEN);
        assert_eq!(read_u32(find_chunk(&wav, *b"fact"), 0), 0);
        assert!(find_chunk(&wav, *b"data").is_empty());
    }

    #[test]
    fn ffprobe_accepts_generated_wav() {
        let wav = encode_all(32, 2, &sine(4_800, 2, 440.0));
        let path = std::env::temp_dir().join(format!(
            "koe-wav-encoder-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&path, &wav).expect("write wav");

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,sample_fmt,sample_rate,channels",
                "-of",
                "default=noprint_wrappers=1",
                path.to_str().expect("utf8 path"),
            ])
            .output();

        let _ = std::fs::remove_file(&path);

        let output = match probe {
            Ok(o) => o,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // ffprobe is optional outside CI images that ship ffmpeg.
                return;
            },
            Err(err) => panic!("ffprobe spawn failed: {err}"),
        };
        assert!(
            output.status.success(),
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("codec_name=pcm_f32le"), "{stdout}");
        assert!(stdout.contains("sample_rate=48000"), "{stdout}");
        assert!(stdout.contains("channels=2"), "{stdout}");
    }
}
