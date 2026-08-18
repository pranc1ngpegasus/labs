//! FLAC lossless encoder (48 kHz stereo, 24-bit PCM).

use super::{AudioEncoder, CodecError, OggComments};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, MetadataBlockData, Stream};
use flacenc::config;
use flacenc::encode_fixed_size_frame;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: usize = 24;
const BLOCK_SIZE: usize = 4096;
const CHANNEL_COUNT: usize = 2;
/// Padding block size so downstream tools can append tags without rewriting frames.
const PADDING_LEN: usize = 8_192;

/// Encodes interleaved stereo `f32` PCM into a FLAC bitstream.
///
/// PCM is compressed incrementally during [`AudioEncoder::encode`]; the container
/// header, metadata, and frames are emitted from [`AudioEncoder::finalize`] so
/// `STREAMINFO` can record the final sample count and MD5 digest (same buffering
/// model as [`super::WavEncoder`]).
pub struct FlacEncoder {
    config: Verified<config::Encoder>,
    stream: Stream,
    context: Context,
    frame_buf: FrameBuf,
    pending_pcm: Vec<f32>,
    scratch_i32: Vec<i32>,
    frame_number: usize,
    finished: bool,
}

impl FlacEncoder {
    /// Creates a FLAC encoder with Vorbis Comment tags matching the OGG encoder.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when compression level or encoder setup fails.
    pub fn with_comments(
        compression_level: u8,
        comments: &OggComments,
    ) -> Result<Self, CodecError> {
        let config = build_config(compression_level)?;
        let mut stream = Stream::new(
            usize::try_from(SAMPLE_RATE).map_err(|_| {
                CodecError::Encoder("sample rate exceeds usize on this platform".to_owned())
            })?,
            usize::from(CHANNELS),
            BITS_PER_SAMPLE,
        )
        .map_err(|err| CodecError::Encoder(err.to_string()))?;
        stream
            .stream_info_mut()
            .set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
            .map_err(|err| CodecError::Encoder(err.to_string()))?;
        stream.add_metadata_block(padding_block(PADDING_LEN)?);
        stream.add_metadata_block(vorbis_comment_block(comments)?);

        Ok(Self {
            config,
            stream,
            context: Context::new(BITS_PER_SAMPLE, CHANNEL_COUNT),
            frame_buf: FrameBuf::with_size(CHANNEL_COUNT, BLOCK_SIZE)
                .map_err(|err| CodecError::Encoder(err.to_string()))?,
            pending_pcm: Vec::new(),
            scratch_i32: Vec::new(),
            frame_number: 0,
            finished: false,
        })
    }

    fn encode_pending_blocks(&mut self) -> Result<(), CodecError> {
        let block_samples = BLOCK_SIZE * CHANNEL_COUNT;
        while self.pending_pcm.len() >= block_samples {
            let chunk: Vec<f32> = self.pending_pcm.drain(..block_samples).collect();
            self.encode_one_block(&chunk)?;
        }
        Ok(())
    }

    fn encode_one_block(
        &mut self,
        pcm: &[f32],
    ) -> Result<(), CodecError> {
        pcm_to_i32(pcm, &mut self.scratch_i32);
        Fill::fill_interleaved(
            &mut (&mut self.frame_buf, &mut self.context),
            &self.scratch_i32,
        )
        .map_err(|err| CodecError::Encoder(err.to_string()))?;

        let frame = encode_fixed_size_frame(
            &self.config,
            &self.frame_buf,
            self.frame_number,
            self.stream.stream_info(),
        )
        .map_err(|err| CodecError::Encoder(err.to_string()))?;
        self.stream.add_frame(frame);
        self.frame_number = self.frame_number.saturating_add(1);
        Ok(())
    }

    fn write_stream_to_vec(&self) -> Result<Vec<u8>, CodecError> {
        let mut sink = ByteSink::new();
        self.stream
            .write(&mut sink)
            .map_err(|err| CodecError::Encoder(err.to_string()))?;
        Ok(sink.as_slice().to_vec())
    }
}

impl AudioEncoder for FlacEncoder {
    fn encode(
        &mut self,
        pcm: &[f32],
    ) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Err(CodecError::Encoder(
                "FLAC encoder already finalized".to_owned(),
            ));
        }
        if pcm.is_empty() {
            return Ok(Vec::new());
        }
        if !pcm.len().is_multiple_of(CHANNEL_COUNT) {
            return Err(CodecError::Encoder(format!(
                "PCM length {} is not a multiple of {CHANNEL_COUNT} channels",
                pcm.len()
            )));
        }

        self.pending_pcm.extend_from_slice(pcm);
        self.encode_pending_blocks()?;
        Ok(Vec::new())
    }

    fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        if !self.pending_pcm.is_empty() {
            let tail = std::mem::take(&mut self.pending_pcm);
            self.encode_one_block(&tail)?;
        }

        self.stream
            .stream_info_mut()
            .set_total_samples(self.context.total_samples());
        self.stream
            .stream_info_mut()
            .set_md5_digest(&self.context.md5_digest());

        self.write_stream_to_vec()
    }
}

fn build_config(compression_level: u8) -> Result<Verified<config::Encoder>, CodecError> {
    if compression_level > 8 {
        return Err(CodecError::Encoder(format!(
            "FLAC compression level {compression_level} out of range [0, 8]"
        )));
    }

    let mut encoder = config::Encoder::default();
    encoder.block_size = BLOCK_SIZE;
    encoder.multithread = false;
    encoder.workers = None;

    match compression_level {
        0..=2 => {
            encoder.subframe_coding.use_lpc = false;
            encoder.subframe_coding.fixed.max_order = 2;
        },
        6..=8 => {
            encoder.subframe_coding.fixed.max_order = 4;
            encoder.subframe_coding.qlpc.lpc_order = 12;
        },
        _ => {},
    }

    encoder
        .into_verified()
        .map_err(|(_, err)| CodecError::Encoder(err.to_string()))
}

fn padding_block(size: usize) -> Result<MetadataBlockData, CodecError> {
    MetadataBlockData::new_unknown(1, &vec![0_u8; size])
        .map_err(|err| CodecError::Encoder(err.to_string()))
}

fn vorbis_comment_block(comments: &OggComments) -> Result<MetadataBlockData, CodecError> {
    let mut payload = Vec::new();
    let vendor = format!("koe v{}", env!("CARGO_PKG_VERSION"));
    write_le_u32(
        &mut payload,
        u32::try_from(vendor.len()).map_err(|_| {
            CodecError::Encoder("FLAC vendor string length exceeds u32::MAX".to_owned())
        })?,
    );
    payload.extend_from_slice(vendor.as_bytes());

    let pairs = comments.tag_pairs();
    write_le_u32(
        &mut payload,
        u32::try_from(pairs.len()).map_err(|_| {
            CodecError::Encoder("FLAC comment field count exceeds u32::MAX".to_owned())
        })?,
    );
    for (name, value) in pairs {
        let field = format!("{name}={value}");
        write_le_u32(
            &mut payload,
            u32::try_from(field.len()).map_err(|_| {
                CodecError::Encoder("FLAC comment field length exceeds u32::MAX".to_owned())
            })?,
        );
        payload.extend_from_slice(field.as_bytes());
    }

    MetadataBlockData::new_unknown(4, &payload).map_err(|err| CodecError::Encoder(err.to_string()))
}

fn write_le_u32(
    out: &mut Vec<u8>,
    value: u32,
) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn pcm_to_i32(
    pcm: &[f32],
    out: &mut Vec<i32>,
) {
    out.clear();
    out.reserve(pcm.len());
    for sample in pcm {
        let finite = sanitize_for_pcm(*sample);
        #[allow(clippy::cast_possible_truncation)]
        let value = (finite * 8_388_607.0) as i32;
        out.push(value);
    }
}

const fn sanitize_for_pcm(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use koe_ffi::AudioSourceConfig;

    use super::*;

    fn encode_all(
        level: u8,
        comments: &OggComments,
        pcm: &[f32],
    ) -> Result<Vec<u8>, CodecError> {
        let mut encoder = FlacEncoder::with_comments(level, comments)?;
        encoder.encode(pcm)?;
        encoder.finalize()
    }

    fn sine_stereo(
        frames: usize,
        freq_hz: f32,
    ) -> Vec<f32> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / SAMPLE_RATE as f32;
            let sample = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.5;
            pcm.push(sample);
            pcm.push(sample);
        }
        pcm
    }

    fn session_comments() -> OggComments {
        OggComments::for_session(&AudioSourceConfig::Microphone, "en-US")
    }

    #[test]
    fn flac_magic_and_metadata_blocks() {
        let pcm = sine_stereo(BLOCK_SIZE, 440.0);
        let flac = encode_all(5, &session_comments(), &pcm).expect("encode");
        assert_eq!(&flac[0..4], b"fLaC");
        assert!(flac.len() > 4_096);
        let body = String::from_utf8_lossy(&flac);
        assert!(body.contains("ARTIST=Koe") || body.contains("Koe"));
        assert!(body.contains("microphone") || body.contains("KOE_SOURCE"));
    }

    #[test]
    fn encode_returns_empty_until_finalize() {
        let mut encoder = FlacEncoder::with_comments(5, &OggComments::basic()).expect("encoder");
        assert!(
            encoder
                .encode(&sine_stereo(BLOCK_SIZE, 440.0))
                .expect("encode")
                .is_empty()
        );
        let out = encoder.finalize().expect("finalize");
        assert!(out.len() > 4);
        assert!(encoder.finalize().expect("idempotent").is_empty());
    }

    #[test]
    fn rejects_bad_compression_level_and_odd_pcm() {
        assert!(FlacEncoder::with_comments(9, &OggComments::basic()).is_err());
        let mut encoder = FlacEncoder::with_comments(5, &OggComments::basic()).expect("encoder");
        let err = encoder.encode(&[0.0]).expect_err("odd");
        assert!(err.to_string().contains("multiple"));
    }

    #[test]
    fn compression_ratio_beats_raw_pcm_for_speech_like_signal() {
        let mut pcm = sine_stereo(usize::try_from(SAMPLE_RATE).expect("sample rate"), 200.0);
        for (idx, sample) in pcm.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let noise = ((idx * 17) % 997) as f32 / 997.0 - 0.5;
            *sample = sample.mul_add(0.7, noise * 0.15);
        }
        let raw_bytes = pcm.len() * 4;
        let flac = encode_all(5, &session_comments(), &pcm).expect("encode");
        #[allow(clippy::cast_precision_loss)]
        let ratio = flac.len() as f64 / raw_bytes as f64;
        assert!(
            ratio < 0.75,
            "FLAC should be smaller than raw PCM; ratio={ratio}"
        );
        assert!(
            ratio > 0.05,
            "FLAC payload should not be trivially empty; ratio={ratio}"
        );
    }

    #[test]
    fn ffprobe_accepts_generated_flac() {
        let flac = encode_all(5, &session_comments(), &sine_stereo(4_800, 440.0)).expect("encode");
        let path = std::env::temp_dir().join(format!(
            "koe-flac-encoder-{}-{}.flac",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&path, &flac).expect("write flac");

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,sample_rate,channels,bits_per_raw_sample",
                "-of",
                "default=noprint_wrappers=1",
                path.to_str().expect("utf8 path"),
            ])
            .output();

        let _ = std::fs::remove_file(&path);

        let output = match probe {
            Ok(o) => o,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) => panic!("ffprobe spawn failed: {err}"),
        };
        assert!(
            output.status.success(),
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("codec_name=flac"), "{stdout}");
        assert!(stdout.contains("sample_rate=48000"), "{stdout}");
        assert!(stdout.contains("channels=2"), "{stdout}");
    }

    #[test]
    fn encode_keeps_up_with_realtime_48khz_blocks() {
        use std::time::Instant;

        let mut encoder = FlacEncoder::with_comments(5, &OggComments::basic()).expect("encoder");
        let block = sine_stereo(BLOCK_SIZE, 440.0);
        let _ = encoder.encode(&block).expect("warmup");

        let iterations = 50_u32;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = encoder.encode(&block).expect("encode");
        }
        let elapsed = start.elapsed();
        #[allow(clippy::cast_precision_loss)]
        let block_duration = BLOCK_SIZE as f64 / f64::from(SAMPLE_RATE);
        let encoded_duration = block_duration * f64::from(iterations);
        assert!(
            elapsed.as_secs_f64() < encoded_duration,
            "encoding {:.2}s of audio took {:.2}s",
            encoded_duration,
            elapsed.as_secs_f64()
        );
        let _ = encoder.finalize().expect("finalize");
    }

    #[test]
    fn lossless_round_trip_via_ffmpeg() {
        let pcm = sine_stereo(BLOCK_SIZE * 2 + 17, 330.0);
        let mut expected = Vec::with_capacity(pcm.len());
        pcm_to_i32(&pcm, &mut expected);
        let flac = encode_all(5, &session_comments(), &pcm).expect("encode");
        let path = std::env::temp_dir().join(format!(
            "koe-flac-roundtrip-{}-{}.flac",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&path, &flac).expect("write flac");

        let decode = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-i",
                path.to_str().expect("utf8 path"),
                "-f",
                "s32le",
                "-acodec",
                "pcm_s32le",
                "-",
            ])
            .output();

        let _ = std::fs::remove_file(&path);

        let output = match decode {
            Ok(o) => o,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) => panic!("ffmpeg spawn failed: {err}"),
        };
        assert!(
            output.status.success(),
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout.len(), expected.len() * 4);
        for (idx, chunk) in output.stdout.chunks_exact(4).enumerate() {
            let decoded = i32::from_le_bytes(chunk.try_into().expect("i32"));
            let original = expected[idx];
            let decoded_i24 = decoded >> 8;
            assert_eq!(original, decoded_i24, "sample {idx} mismatch");
        }
    }
}
