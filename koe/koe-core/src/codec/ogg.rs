//! OGG Vorbis encoder (48 kHz stereo, quality-based VBR).

use std::fmt::Write as _;
use std::io::{self, Write};
use std::num::{NonZeroU8, NonZeroU32};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use koe_ffi::AudioSourceConfig;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

use super::{AudioEncoder, CodecError};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// libvorbis VBR quality range used by [`VorbisBitrateManagementStrategy::QualityVbr`].
const QUALITY_MIN: f32 = -0.1;
const QUALITY_MAX: f32 = 1.0;

/// Vorbis Comment tags written into the OGG identification block.
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

    /// Vorbis Comment tag pairs shared with the FLAC encoder.
    #[must_use]
    pub const fn tag_pairs(&self) -> [(&str, &str); 6] {
        self.as_pairs()
    }
}

/// Encodes interleaved stereo `f32` PCM into an OGG Vorbis bitstream.
pub struct OggEncoder {
    /// Held behind `Option` so [`AudioEncoder::finalize`] can consume it.
    encoder: Option<VorbisEncoder<SharedSink>>,
    sink_buf: Arc<Mutex<Vec<u8>>>,
    /// Reused planar scratch buffers (avoid per-chunk allocation).
    left: Vec<f32>,
    right: Vec<f32>,
    finished: bool,
}

// SAFETY: `VorbisEncoder` embeds raw pointers to libvorbis state and is therefore
// `!Send`. We only ever access this encoder through `Mutex` (exclusive lock), so
// moving the wrapper between threads is safe as long as concurrent use is prevented.
#[allow(unsafe_code, clippy::non_send_fields_in_send_ty)]
unsafe impl Send for OggEncoder {}

impl OggEncoder {
    /// Creates an OGG encoder with the given VBR quality and Vorbis comments.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when quality is out of range or libvorbis setup fails.
    pub fn with_comments(
        quality: f32,
        comments: &OggComments,
    ) -> Result<Self, CodecError> {
        if !(QUALITY_MIN..=QUALITY_MAX).contains(&quality) {
            return Err(CodecError::Encoder(format!(
                "OGG quality {quality} out of range [{QUALITY_MIN}, {QUALITY_MAX}]"
            )));
        }

        let sample_rate = NonZeroU32::new(SAMPLE_RATE)
            .ok_or_else(|| CodecError::Encoder("sample rate must be non-zero".to_owned()))?;
        let channels = NonZeroU8::new(
            u8::try_from(CHANNELS)
                .map_err(|_| CodecError::Encoder("channel count exceeds u8".to_owned()))?,
        )
        .ok_or_else(|| CodecError::Encoder("channel count must be non-zero".to_owned()))?;

        let sink_buf = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink {
            buf: Arc::clone(&sink_buf),
        };

        let mut builder = VorbisEncoderBuilder::new(sample_rate, channels, sink)
            .map_err(|err| CodecError::Encoder(err.to_string()))?;
        builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
            target_quality: quality,
        });
        for (tag, value) in comments.as_pairs() {
            builder
                .comment_tag(tag, value)
                .map_err(|err| CodecError::Encoder(err.to_string()))?;
        }

        let encoder = builder
            .build()
            .map_err(|err| CodecError::Encoder(err.to_string()))?;

        Ok(Self {
            encoder: Some(encoder),
            sink_buf,
            left: Vec::new(),
            right: Vec::new(),
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

    fn fill_planar(
        &mut self,
        pcm: &[f32],
    ) {
        let frames = pcm.len() / 2;
        self.left.clear();
        self.right.clear();
        self.left.reserve(frames);
        self.right.reserve(frames);
        for pair in pcm.chunks_exact(2) {
            self.left.push(pair[0]);
            self.right.push(pair[1]);
        }
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
        if !pcm.len().is_multiple_of(usize::from(CHANNELS)) {
            return Err(CodecError::Encoder(format!(
                "PCM length {} is not a multiple of {CHANNELS} channels",
                pcm.len()
            )));
        }

        self.fill_planar(pcm);
        // Temporarily move planar buffers out so we can mutably borrow the encoder.
        let left = std::mem::take(&mut self.left);
        let right = std::mem::take(&mut self.right);
        let encode_result = {
            let encoder = self
                .encoder
                .as_mut()
                .ok_or_else(|| CodecError::Encoder("OGG encoder already finalized".to_owned()))?;
            encoder
                .encode_audio_block([&left[..], &right[..]])
                .map_err(|err| CodecError::Encoder(err.to_string()))
        };
        self.left = left;
        self.right = right;
        encode_result?;
        self.take_encoded()
    }

    fn finalize(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        if let Some(encoder) = self.encoder.take() {
            let _sink = encoder
                .finish()
                .map_err(|err| CodecError::Encoder(err.to_string()))?;
        }
        self.take_encoded()
    }
}

/// Shared byte sink so [`VorbisEncoder`] can write while we drain pages per chunk.
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
    use std::time::Instant;

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

    #[test]
    fn encodes_ogg_with_valid_header_and_comments() {
        let comments = OggComments {
            title: "Test recording — 2026-08-11 12:00:00".to_owned(),
            artist: "Koe".to_owned(),
            date: "2026-08-11T12:00:00Z".to_owned(),
            description: "Source: Microphone, Locale: en-US".to_owned(),
            encoder: "koe v0.0.0".to_owned(),
            koe_source: r#"{"type":"microphone"}"#.to_owned(),
        };
        let mut encoder = OggEncoder::with_comments(0.4, &comments).expect("encoder");

        // ~1 second of audio in 960-frame blocks (20 ms @ 48 kHz).
        let block = sine_stereo(960, 440.0);
        let mut out = Vec::new();
        for _ in 0..50 {
            out.extend(encoder.encode(&block).expect("encode"));
        }
        out.extend(encoder.finalize().expect("finalize"));

        assert!(out.len() > 100, "expected non-trivial OGG payload");
        assert_eq!(&out[..4], b"OggS", "OGG capture pattern");
        let as_str = String::from_utf8_lossy(&out);
        assert!(as_str.contains("ARTIST=Koe") || as_str.contains("Koe"));
        assert!(as_str.contains("vorbis"));
        assert!(as_str.contains("koe v0.0.0") || as_str.contains("ENCODER"));
        assert!(as_str.contains("microphone") || as_str.contains("KOE_SOURCE"));
    }

    #[test]
    fn encode_latency_under_one_ms_per_960_frames() {
        let comments = OggComments::for_session(&AudioSourceConfig::Microphone, "en-US");
        let mut encoder = OggEncoder::with_comments(0.4, &comments).expect("encoder");
        let block = sine_stereo(960, 440.0);

        // Warm up (headers + codebook).
        let _ = encoder.encode(&block).expect("warmup");

        let iterations = 100_u32;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = encoder.encode(&block).expect("encode");
        }
        let elapsed = start.elapsed();
        let per_block_us = elapsed.as_micros() / u128::from(iterations);
        assert!(
            per_block_us < 1_000,
            "encode latency {per_block_us} µs/block exceeds 1 ms budget"
        );
        let _ = encoder.finalize().expect("finalize");
    }

    #[test]
    fn rejects_out_of_range_quality() {
        let comments = OggComments::for_session(&AudioSourceConfig::Microphone, "ja-JP");
        match OggEncoder::with_comments(1.5, &comments) {
            Ok(_) => panic!("expected quality error"),
            Err(err) => assert!(err.to_string().contains("quality")),
        }
    }

    #[test]
    fn rejects_odd_pcm_length() {
        let comments = OggComments::for_session(&AudioSourceConfig::Microphone, "en-US");
        let mut encoder = OggEncoder::with_comments(0.4, &comments).expect("encoder");
        let err = encoder.encode(&[0.0]).expect_err("odd");
        assert!(err.to_string().contains("multiple"));
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
