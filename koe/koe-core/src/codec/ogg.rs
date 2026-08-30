//! OGG Opus encoder and container writer (48 kHz stereo, RFC 7845).
//!
//! The header builders emit fixed-width binary fields mandated by RFC 7845
//! (u16 pre-skip, u32 comment lengths, u8 channels), so narrowing casts from
//! wider Rust types and `f32`→`i16` sample scaling are the file's normal idiom.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use getrandom::fill;
use koe_ffi::AudioSourceConfig;
use ogg::{PacketWriteEndInfo, PacketWriter};
use shiguredo_opus::Encoder;

use super::{AudioEncoder, CodecError};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// Opus granule positions are 48 kHz samples per RFC 7845; a 20 ms frame is
/// always 960 such samples regardless of the encoder input rate.
const FRAME_SAMPLES_48K: u64 = 960;

/// Opus comment tags written into the identification header.
#[derive(Debug, Clone)]
pub struct OggComments {
    /// `TITLE` tag.
    pub title: String,
    /// `ARTIST` tag.
    pub artist: String,
    /// `DATE` tag (ISO 8601).
    pub date: String,
    /// `DESCRIPTION` tag.
    pub description: String,
    /// `ENCODER` tag.
    pub encoder: String,
    /// `KOE_SOURCE` tag (JSON of the capture source).
    pub koe_source: String,
}

impl OggComments {
    /// Minimal tags when session metadata is unavailable.
    #[must_use]
    pub fn basic() -> Self {
        Self {
            title: "Koe recording".to_owned(),
            artist: "Koe".to_owned(),
            date: String::new(),
            description: String::new(),
            encoder: format!("koe v{}", env!("CARGO_PKG_VERSION")),
            koe_source: r#"{"type":"unknown"}"#.to_owned(),
        }
    }

    /// Builds tags for a recording session from capture source and locale.
    #[must_use]
    pub fn for_session(
        source: &AudioSourceConfig,
        locale: &str,
    ) -> Self {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (date, time, iso) = unix_to_civil(now_secs);
        let app_name = source_label(source);
        Self {
            title: format!("{app_name} recording — {date} {time}"),
            artist: "Koe".to_owned(),
            date: iso,
            description: format!("Source: {app_name}, Locale: {locale}"),
            encoder: format!("koe v{}", env!("CARGO_PKG_VERSION")),
            koe_source: source_json(source),
        }
    }

    const fn as_pairs(&self) -> [(&str, &str); 6] {
        [
            ("TITLE", self.title.as_str()),
            ("ARTIST", self.artist.as_str()),
            ("DATE", self.date.as_str()),
            ("DESCRIPTION", self.description.as_str()),
            ("ENCODER", self.encoder.as_str()),
            ("KOE_SOURCE", self.koe_source.as_str()),
        ]
    }
}

/// Encodes interleaved stereo `f32` PCM into an OGG Opus bitstream.
///
/// The [`PacketWriter`] routes every completed page into a shared sink buffer;
/// [`AudioEncoder::encode`] returns whatever pages accumulated since the last
/// call, so the pipeline can write progress incrementally without a streaming
/// writer.
pub struct OggEncoder {
    encoder: Encoder,
    writer: PacketWriter<'static, SharedSink>,
    sink_buf: Arc<Mutex<Vec<u8>>>,
    /// Interleaved `f32` input buffered toward one 20 ms Opus frame.
    buf: Vec<f32>,
    /// Encoder frame size (samples per channel).
    frame_samples: usize,
    serial: u32,
    /// Pre-skip in 48 kHz samples (from the encoder's lookahead).
    pre_skip_48k: u64,
    /// Data packets encoded so far (for granule position math).
    packets_encoded: u64,
    /// Most recent packet, deferred so the final page can carry EOS.
    pending: Option<EncodedPacket>,
    finished: bool,
}

/// A single Opus packet with its 48 kHz granule position.
struct EncodedPacket {
    data: Vec<u8>,
    granule: u64,
}

impl OggEncoder {
    /// Creates an Opus encoder with the given bitrate and comment tags.
    ///
    /// `bitrate_bps` of `None` lets libopus pick a default for 48 kHz stereo.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when the Opus encoder can't be created or the
    /// identification header can't be written.
    pub fn with_comments(
        bitrate_bps: Option<u32>,
        comments: &OggComments,
    ) -> Result<Self, CodecError> {
        let encoder = Encoder::new(shiguredo_opus::EncoderConfig {
            bitrate: bitrate_bps,
            ..shiguredo_opus::EncoderConfig::new(SAMPLE_RATE, CHANNELS as u8)
        })
        .map_err(|e| CodecError::Encoder(e.to_string()))?;

        let frame_samples = encoder.frame_samples();
        let pre_skip_48k = u64::from(
            encoder
                .get_lookahead()
                .map_err(|e| CodecError::Encoder(e.to_string()))?,
        );

        let mut serial_bytes = [0_u8; 4];
        fill(&mut serial_bytes).map_err(|e| CodecError::Encoder(e.to_string()))?;
        let serial = u32::from_le_bytes(serial_bytes).max(1);

        let sink_buf = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink {
            buf: Arc::clone(&sink_buf),
        };
        let mut writer = PacketWriter::new(sink);
        let head = build_opus_head(CHANNELS as u8, pre_skip_48k, SAMPLE_RATE);
        writer
            .write_packet(head, serial, PacketWriteEndInfo::NormalPacket, 0)
            .map_err(CodecError::Io)?;
        let opus_tags = build_opus_tags(comments);
        writer
            .write_packet(opus_tags, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(CodecError::Io)?;

        Ok(Self {
            encoder,
            writer,
            sink_buf,
            buf: Vec::new(),
            frame_samples,
            serial,
            pre_skip_48k,
            packets_encoded: 0,
            pending: None,
            finished: false,
        })
    }

    fn take_encoded(&self) -> Result<Vec<u8>, CodecError> {
        let mut guard = self
            .sink_buf
            .lock()
            .map_err(|_| CodecError::Encoder("ogg sink lock poisoned".to_owned()))?;
        Ok(std::mem::take(&mut *guard))
    }

    fn encode_buffered(&mut self) -> Result<(), CodecError> {
        let frame_len = self.frame_samples * CHANNELS as usize;
        while self.buf.len() >= frame_len {
            let i16_frame = self
                .buf
                .drain(..frame_len)
                .map(f32_to_i16)
                .collect::<Vec<_>>();
            let packet = self
                .encoder
                .encode(&i16_frame)
                .map_err(|e| CodecError::Encoder(e.to_string()))?;
            self.push_packet(packet)?;
        }
        Ok(())
    }

    /// Queues a packet, flushing the previously queued one so we know which is
    /// truly last for the EOS flag on [`AudioEncoder::finalize`].
    fn push_packet(
        &mut self,
        data: Vec<u8>,
    ) -> Result<(), CodecError> {
        let granule = self.pre_skip_48k + (self.packets_encoded + 1) * FRAME_SAMPLES_48K;
        self.packets_encoded += 1;
        if let Some(prev) = self.pending.take() {
            self.writer
                .write_packet(
                    prev.data,
                    self.serial,
                    PacketWriteEndInfo::EndPage,
                    prev.granule,
                )
                .map_err(CodecError::Io)?;
        }
        self.pending = Some(EncodedPacket { data, granule });
        Ok(())
    }
}

impl AudioEncoder for OggEncoder {
    fn encode(
        &mut self,
        pcm: &[f32],
    ) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Err(CodecError::Encoder(
                "OGG encoder already finalized".to_owned(),
            ));
        }
        if pcm.is_empty() {
            return self.take_encoded();
        }
        if !pcm.len().is_multiple_of(CHANNELS as usize) {
            return Err(CodecError::Encoder(format!(
                "PCM length {} is not a multiple of {CHANNELS} channels",
                pcm.len()
            )));
        }

        self.buf.extend_from_slice(pcm);
        self.encode_buffered()?;
        self.take_encoded()
    }

    fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        // Encode a final frame from any remainder, zero-padded to a full frame.
        let frame_len = self.frame_samples * CHANNELS as usize;
        if !self.buf.is_empty() {
            self.buf.resize(frame_len, 0.0);
            let buffer: Vec<f32> = std::mem::take(&mut self.buf);
            let i16_frame = buffer.into_iter().map(f32_to_i16).collect::<Vec<_>>();
            let packet = self
                .encoder
                .encode(&i16_frame)
                .map_err(|e| CodecError::Encoder(e.to_string()))?;
            self.push_packet(packet)?;
        }

        let final_packet = if let Some(packet) = self.pending.take() {
            Some(packet)
        } else {
            // Empty recording: emit one silence frame so the stream has an
            // EOS page and a valid data segment.
            let i16_frame = vec![0_i16; frame_len];
            let data = self
                .encoder
                .encode(&i16_frame)
                .map_err(|e| CodecError::Encoder(e.to_string()))?;
            let granule = self.pre_skip_48k + (self.packets_encoded + 1) * FRAME_SAMPLES_48K;
            Some(EncodedPacket { data, granule })
        };

        if let Some(packet) = final_packet {
            self.writer
                .write_packet(
                    packet.data,
                    self.serial,
                    PacketWriteEndInfo::EndStream,
                    packet.granule,
                )
                .map_err(CodecError::Io)?;
        }
        self.take_encoded()
    }
}

/// Appends a complete page to the shared sink so the encoder can hand back
/// accumulated bytes per chunk (`take_encoded`).
struct SharedSink {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedSink {
    fn write(
        &mut self,
        data: &[u8],
    ) -> io::Result<usize> {
        self.buf
            .lock()
            .map_err(|_| io::Error::other("ogg sink lock poisoned"))?
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Builds the 19-byte `OpusHead` identification header.
fn build_opus_head(
    channels: u8,
    pre_skip_48k: u64,
    input_sample_rate: u32,
) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(channels);
    head.extend_from_slice(&(pre_skip_48k as u16).to_le_bytes());
    head.extend_from_slice(&input_sample_rate.to_le_bytes());
    head.extend_from_slice(&0_i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping (family 0)
    head
}

/// Builds the `OpusTags` comment packet (vendor string + the 6 koe tags).
fn build_opus_tags(comments: &OggComments) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OpusTags");
    let vendor = comments.encoder.as_bytes();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    let pairs = comments.as_pairs();
    out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for (tag, value) in pairs {
        let comment = format!("{tag}={value}");
        out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        out.extend_from_slice(comment.as_bytes());
    }
    out
}

fn source_label(source: &AudioSourceConfig) -> String {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } | AudioSourceConfig::Both { bundle_id } => {
            bundle_id.clone()
        },
        AudioSourceConfig::PidAudio { pid } => format!("pid:{pid}"),
        AudioSourceConfig::Microphone => "Microphone".to_owned(),
    }
}

fn source_json(source: &AudioSourceConfig) -> String {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } => {
            format!(
                r#"{{"type":"app_audio","bundle_id":"{}"}}"#,
                escape_json(bundle_id)
            )
        },
        AudioSourceConfig::PidAudio { pid } => {
            format!(r#"{{"type":"pid_audio","pid":{pid}}}"#)
        },
        AudioSourceConfig::Microphone => r#"{"type":"microphone"}"#.to_owned(),
        AudioSourceConfig::Both { bundle_id } => {
            format!(
                r#"{{"type":"both","bundle_id":"{}"}}"#,
                escape_json(bundle_id)
            )
        },
    }
}

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            },
            c => out.push(c),
        }
    }
    out
}

/// Converts a normalized `f32` sample ([-1, 1]) to 16-bit PCM, scaling to the
/// full i16 range and clamping. Quantization truncates toward zero (matches
/// the capture-side `s16 → f32 / 32768` mapping, so the round-trip is exact
/// for that path).
fn f32_to_i16(sample: f32) -> i16 {
    (sample * 32768.0).clamp(-32768.0, 32767.0) as i16
}

/// Converts Unix seconds to `(YYYY-MM-DD, HH:MM:SS, ISO-8601Z)` in UTC.
fn unix_to_civil(secs: u64) -> (String, String, String) {
    let days = i32::try_from(secs / 86_400).unwrap_or(i32::MAX);
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = format!("{hour:02}:{minute:02}:{second:02}");
    let iso = format!("{date}T{time}Z");
    (date, time, iso)
}

/// Howard Hinnant's `civil_from_days` (days since 1970-01-01 → y/m/d).
fn civil_from_days(days_since_epoch: i32) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = u32::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i32::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Instant;

    use ogg::PacketReader;
    use shiguredo_opus::Decoder;

    use super::*;

    fn sine_stereo(
        frames: usize,
        freq_hz: f32,
    ) -> Vec<f32> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / SAMPLE_RATE as f32;
            let sample = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.25;
            pcm.push(sample);
            pcm.push(sample);
        }
        pcm
    }

    fn sample_comments() -> OggComments {
        OggComments {
            title: "Test recording — 2026-08-11 12:00:00".to_owned(),
            artist: "Koe".to_owned(),
            date: "2026-08-11T12:00:00Z".to_owned(),
            description: "Source: Microphone, Locale: en-US".to_owned(),
            encoder: "koe v0.0.0".to_owned(),
            koe_source: r#"{"type":"microphone"}"#.to_owned(),
        }
    }

    /// Encodes the given interleaved stereo blocks and returns the full stream.
    fn encode_blocks(
        blocks: &[Vec<f32>],
        bitrate: Option<u32>,
    ) -> Vec<u8> {
        let mut encoder = OggEncoder::with_comments(bitrate, &sample_comments()).expect("encoder");
        let mut out = Vec::new();
        for block in blocks {
            out.extend(encoder.encode(block).expect("encode"));
        }
        out.extend(encoder.finalize().expect("finalize"));
        out
    }

    fn decode_opus(bytes: &[u8]) -> Vec<i16> {
        let mut reader = PacketReader::new(Cursor::new(bytes));
        let mut decoder =
            Decoder::new(shiguredo_opus::DecoderConfig::new(SAMPLE_RATE, 2)).expect("decoder");
        let mut samples = Vec::new();
        while let Some(packet) = reader.read_packet().expect("read packet") {
            if packet.data.starts_with(b"Opus") {
                continue; // OpusHead / OpusTags
            }
            samples.extend(decoder.decode(&packet.data).expect("decode"));
        }
        samples
    }

    fn mean_abs(samples: &[i16]) -> f64 {
        samples.iter().map(|&s| f64::from(s).abs()).sum::<f64>() / samples.len() as f64
    }

    /// Parses each page's `(granule position, header-type byte)`. The
    /// header-type byte is at byte offset 5 (`0x02` BOS, `0x04` EOS).
    fn page_headers(bytes: &[u8]) -> Vec<(u64, u8)> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 27 <= bytes.len() {
            if bytes[pos..pos + 4] != *b"OggS" {
                break;
            }
            let seg_count = bytes[pos + 26] as usize;
            let page_len = 27
                + seg_count
                + bytes[pos + 27..pos + 27 + seg_count]
                    .iter()
                    .map(|&n| u32::from(n))
                    .sum::<u32>() as usize;
            if pos + 27 + seg_count > bytes.len() {
                break;
            }
            let granule = u64::from_le_bytes(bytes[pos + 6..pos + 14].try_into().unwrap());
            out.push((granule, bytes[pos + 5]));
            pos += page_len;
        }
        out
    }

    /// Reads the `pre_skip` field from the `OpusHead` in the stream.
    fn head_pre_skip(bytes: &[u8]) -> u16 {
        let head_pos = bytes
            .windows(8)
            .position(|w| w == b"OpusHead")
            .expect("opus head");
        u16::from_le_bytes([bytes[head_pos + 10], bytes[head_pos + 11]])
    }

    #[test]
    fn encodes_ogg_with_valid_header_and_comments() {
        let block = sine_stereo(960, 440.0);
        // ~1 second of audio in 960-frame blocks (20 ms @ 48 kHz).
        let out = encode_blocks(&vec![block; 50], Some(64_000));

        assert!(out.len() > 100, "expected non-trivial OGG payload");
        assert_eq!(&out[..4], b"OggS", "OGG capture pattern");
        let as_str = String::from_utf8_lossy(&out);
        assert!(as_str.contains("OpusHead"));
        assert!(as_str.contains("OpusTags"));
        assert!(as_str.contains("ARTIST=Koe") || as_str.contains("Koe"));
        assert!(as_str.contains("ENCODER=koe v0.0.0"));
        assert!(as_str.contains("KOE_SOURCE="));

        // OpusHead fields: version 1, stereo, non-zero pre-skip, rate.
        let head_pos = out
            .windows(8)
            .position(|w| w == b"OpusHead")
            .expect("opus head");
        let head = &out[head_pos..head_pos + 19];
        assert_eq!(head[8], 1);
        assert_eq!(head[9], 2);
        let pre_skip = head_pre_skip(&out);
        assert!(pre_skip > 0, "pre-skip should be non-zero");
        assert_eq!(
            u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
            SAMPLE_RATE
        );
    }

    #[test]
    fn round_trip_decodes_to_audible_audio() {
        let block = sine_stereo(960, 440.0);
        let out = encode_blocks(&vec![block; 50], Some(64_000));
        let decoded = decode_opus(&out);
        assert!(
            decoded.len() >= SAMPLE_RATE as usize,
            "expected ~1s of samples"
        );
        assert!(
            mean_abs(&decoded) > 500.0,
            "expected audible output, mean_abs={:.0}",
            mean_abs(&decoded)
        );
    }

    #[test]
    fn default_bitrate_and_explicit_bitrate_both_decode() {
        for bitrate in [None, Some(24_000), Some(128_000)] {
            let out = encode_blocks(&vec![sine_stereo(960, 440.0); 10], bitrate);
            let decoded = decode_opus(&out);
            assert!(decoded.len() >= 960 * 10, "expected ~10 frames of samples");
            // Same audible bar as the 1-second round-trip: proves the chosen
            // bitrate actually carries the tone rather than near-silence.
            assert!(
                mean_abs(&decoded) > 500.0,
                "bitrate {bitrate:?} decoded to near-silence, mean_abs={:.0}",
                mean_abs(&decoded)
            );
        }
    }

    #[test]
    fn encode_latency_under_one_ms_per_960_frames() {
        // 960 frames @ 48 kHz is a 20 ms block; the 1 ms budget keeps encoding
        // far below real time even on modest hardware.
        const MAX_ENCODE_BLOCK_US: u128 = 1_000;

        let comments = sample_comments();
        let mut encoder = OggEncoder::with_comments(None, &comments).expect("encoder");
        let block = sine_stereo(960, 440.0);

        // Warm up (headers + initial frame).
        let _ = encoder.encode(&block).expect("warmup");

        // Measure per-block latency and assert on the median (odd sample count
        // so the middle element is the true median) so one or two scheduler
        // hiccups (parallel test compilation, loaded CI host) don't flake the
        // real-time budget. A genuine encoder regression pushes the median
        // well past the budget, so the central tendency still catches it.
        let iterations = 101_usize;
        let mut per_block_us = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            let _ = encoder.encode(&block).expect("encode");
            per_block_us.push(start.elapsed().as_micros());
        }
        per_block_us.sort_unstable();
        let median_us = per_block_us[iterations / 2];
        assert!(
            median_us < MAX_ENCODE_BLOCK_US,
            "median encode latency {median_us} µs/block exceeds 1 ms budget"
        );
        let _ = encoder.finalize().expect("finalize");
    }

    #[test]
    fn rejects_odd_pcm_length() {
        let comments = sample_comments();
        let mut encoder = OggEncoder::with_comments(None, &comments).expect("encoder");
        let err = encoder.encode(&[0.0]).expect_err("odd");
        assert!(err.to_string().contains("multiple"));
    }

    #[test]
    fn f32_to_i16_scales_and_clamps() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(0.5), 16_384);
        assert_eq!(f32_to_i16(-0.5), -16_384);
        // Out-of-range input clamps rather than wrapping.
        assert_eq!(f32_to_i16(1.0), 32_767);
        assert_eq!(f32_to_i16(-1.0), -32_768);
        assert_eq!(f32_to_i16(2.0), 32_767);
        assert_eq!(f32_to_i16(-2.0), -32_768);
    }

    #[test]
    fn encode_after_finalize_is_rejected() {
        let comments = sample_comments();
        let mut encoder = OggEncoder::with_comments(None, &comments).expect("encoder");
        let _ = encoder.finalize().expect("finalize");
        let err = encoder
            .encode(&[0.0, 0.0])
            .expect_err("encode after finalize must fail");
        assert!(err.to_string().contains("already finalized"));
    }

    #[test]
    fn double_finalize_is_idempotent() {
        let comments = sample_comments();
        let mut encoder = OggEncoder::with_comments(None, &comments).expect("encoder");
        let _ = encoder.encode(&sine_stereo(960, 440.0)).expect("encode");
        let first = encoder.finalize().expect("first finalize");
        assert!(!first.is_empty(), "first finalize emits the stream trailer");
        // A regression that wrote an extra trailer would surface here as a
        // non-empty second finalize.
        let second = encoder.finalize().expect("second finalize");
        assert!(second.is_empty(), "double-finalize must emit no bytes");
    }

    #[test]
    fn empty_frame_encode_is_a_noop() {
        let comments = sample_comments();
        let mut encoder = OggEncoder::with_comments(None, &comments).expect("encoder");
        let _ = encoder.encode(&sine_stereo(960, 440.0)).expect("encode");
        // After a draining encode the sink is empty; an empty chunk adds nothing.
        assert!(encoder.encode(&[]).expect("empty encode").is_empty());
    }

    #[test]
    fn empty_recording_still_yields_valid_stream() {
        // No data chunks: finalize must emit header + EOS silence page.
        let out = encode_blocks(&[], None);
        assert_eq!(&out[..4], b"OggS");
        let decoded = decode_opus(&out);
        assert!(!decoded.is_empty());
        // The single frame must be (near-)silence, not arbitrary PCM.
        let level = mean_abs(&decoded);
        assert!(
            level < 1.0,
            "empty recording must decode to silence, mean_abs={level}"
        );
    }

    #[test]
    fn granule_positions_increase_by_960() {
        let block = sine_stereo(960, 440.0);
        let out = encode_blocks(&vec![block; 50], Some(64_000));
        let pages = page_headers(&out);
        // Header page (granule 0) + 50 data pages for one second @ 20 ms.
        assert!(
            pages.len() >= 51,
            "expected >=51 pages, got {}",
            pages.len()
        );
        let granules = pages.iter().map(|&(g, _)| g).collect::<Vec<_>>();
        // First page is the identification header (BOS); last carries EOS.
        assert_ne!(pages[0].1 & 0x02, 0, "header page must set BOS");
        assert_ne!(
            pages.last().expect("pages").1 & 0x04,
            0,
            "last page must set EOS"
        );
        // First data granule = pre_skip + one 20 ms frame in 48 kHz units.
        let pre_skip = u64::from(head_pre_skip(&out));
        assert_eq!(granules[1], pre_skip + FRAME_SAMPLES_48K);
        for pair in granules[1..].windows(2) {
            assert_eq!(pair[1] - pair[0], 960, "granule delta should be 960");
        }
    }

    #[test]
    fn civil_from_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2024-02-29 (leap day): 19782 days after epoch.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_161), (2025, 3, 14));
        assert_eq!(
            unix_to_civil(0),
            (
                "1970-01-01".to_owned(),
                "00:00:00".to_owned(),
                "1970-01-01T00:00:00Z".to_owned(),
            )
        );
    }
}
