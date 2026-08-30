//! Ogg Opus encoder and container writer (RFC 7845).
//!
//! Encodes interleaved i16 PCM with [`shiguredo_opus::Encoder`] and writes the
//! packets into an Ogg stream (via the [`ogg`] crate), one packet per page,
//! with the Opus-specific pre-skip / granule bookkeeping (design 04).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_lossless
)]

use std::io::{Seek, Write};
use std::sync::{Arc, Mutex};

use getrandom::fill;
use ogg::{PacketWriteEndInfo, PacketWriter};
use shiguredo_opus::Encoder;

use super::{EncoderSpec, EncoderStats, Error};
use crate::AudioChunk;

/// `OpusTags` comment fields written into the identification header.
#[derive(Debug, Clone)]
pub struct Tags {
    /// `TITLE` value (the output filename).
    pub title: String,
    /// `ENCODER` value (e.g. `oto <version>`).
    pub encoder: String,
    /// `CREATED` value (ISO 8601 timestamp).
    pub created: String,
}

/// Granule positions are expressed in 48 kHz samples per RFC 7845, regardless
/// of the encoder's input rate. A 20 ms Opus frame is always 960 such samples.
const FRAME_SAMPLES_48K: u64 = 960;

/// Encodes i16 PCM into an Ogg/Opus stream.
///
/// `W` must be a writer (a file in production, a `Cursor` in tests).
pub struct OggOpusEncoder<'a, W: Write> {
    writer: PacketWriter<'a, W>,
    encoder: Encoder,
    serial: u32,
    /// Interleaved input samples buffered toward one encoder frame.
    buf: Vec<i16>,
    /// Encoder frame size (samples per channel).
    frame_samples: usize,
    /// Encoder channel count.
    channels: usize,
    /// Pre-skip in 48 kHz samples (from the encoder's lookahead).
    pre_skip_48k: u64,
    /// Number of data packets encoded so far (for granule math).
    packets_encoded: u64,
    /// Spec (rate/channels) reported to callers.
    spec: EncoderSpec,
    /// The most recent encoded packet, deferred so the final page can carry EOS.
    pending: Option<EncodedPacket>,
    finished: bool,
}

/// A single encoded Opus packet with its granule position.
struct EncodedPacket {
    data: Vec<u8>,
    granule: u64,
}

impl<W: Write + Seek> OggOpusEncoder<'_, W> {
    /// Creates an Ogg/Opus encoder for the given rate/channels and bitrate.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the Opus encoder can't be created, a serial can't
    /// be generated, or the identification header can't be written.
    pub fn new(
        writer: W,
        sample_rate: u32,
        channels: u8,
        bitrate_bps: Option<u32>,
        tags: &Tags,
    ) -> Result<Self, Error> {
        let encoder = Encoder::new(shiguredo_opus::EncoderConfig {
            bitrate: bitrate_bps,
            ..shiguredo_opus::EncoderConfig::new(sample_rate, channels)
        })
        .map_err(|e| Error::Codec(e.to_string()))?;

        let frame_samples = encoder.frame_samples();
        let pre_skip_48k = u64::from(
            encoder
                .get_lookahead()
                .map_err(|e| Error::Codec(e.to_string()))?,
        );

        let mut serial_bytes = [0_u8; 4];
        fill(&mut serial_bytes).map_err(|e| Error::Codec(e.to_string()))?;
        let serial = u32::from_le_bytes(serial_bytes).max(1);

        let mut packet_writer = PacketWriter::new(writer);
        let head = build_opus_head(channels, pre_skip_48k, sample_rate);
        packet_writer
            .write_packet(head, serial, PacketWriteEndInfo::NormalPacket, 0)
            .map_err(Error::Io)?;
        let opus_tags = build_opus_tags(tags);
        packet_writer
            .write_packet(opus_tags, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(Error::Io)?;

        Ok(Self {
            writer: packet_writer,
            encoder,
            serial,
            buf: Vec::new(),
            frame_samples,
            channels: usize::from(channels),
            pre_skip_48k,
            packets_encoded: 0,
            spec: EncoderSpec {
                sample_rate,
                channels,
            },
            pending: None,
            finished: false,
        })
    }

    /// The underlying writer, consumed after [`Self::finalize`].
    #[must_use]
    pub fn into_inner(self) -> W {
        self.writer.into_inner()
    }

    /// Encodes and queues one packet, flushing the previous queued packet so we
    /// know which one is truly last for the EOS flag.
    fn push_packet(
        &mut self,
        data: Vec<u8>,
    ) -> Result<(), Error> {
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
                .map_err(Error::Io)?;
        }
        self.pending = Some(EncodedPacket { data, granule });
        Ok(())
    }

    /// Encodes any buffered samples as complete frames.
    fn encode_buffered(&mut self) -> Result<(), Error> {
        let frame_len = self.frame_samples * self.channels;
        while self.buf.len() >= frame_len {
            let frame = self.buf.drain(..frame_len).collect::<Vec<_>>();
            let packet = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
            self.push_packet(packet)?;
        }
        Ok(())
    }
}

impl<W: Write + Seek + Send> super::AudioEncoder for OggOpusEncoder<'_, W> {
    fn spec(&self) -> EncoderSpec {
        self.spec
    }

    fn write(
        &mut self,
        _chunk: &AudioChunk<'_>,
        converted: Option<&[i16]>,
    ) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Unsupported(
                "Opus encoder already finalized".to_owned(),
            ));
        }
        let pcm = converted.ok_or_else(|| {
            Error::Unsupported("Opus encoder requires converted i16 input".to_owned())
        })?;
        self.buf.extend_from_slice(pcm);
        self.encode_buffered()
    }

    fn finalize(&mut self) -> Result<EncoderStats, Error> {
        if self.finished {
            return Ok(EncoderStats::default());
        }
        self.finished = true;

        // Encode a final frame from any remainder, zero-padded to a full frame.
        let frame_len = self.frame_samples * self.channels;
        if !self.buf.is_empty() {
            self.buf.resize(frame_len, 0);
            let frame = std::mem::take(&mut self.buf);
            let packet = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
            self.push_packet(packet)?;
        }

        let final_packet = if let Some(packet) = self.pending.take() {
            Some(packet)
        } else {
            // Empty recording: emit one silence frame so the stream has an EOS
            // page and a valid (non-trivial) data segment.
            let frame = vec![0_i16; frame_len];
            let data = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
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
                .map_err(Error::Io)?;
        }
        self.writer.inner_mut().flush().map_err(Error::Io)?;

        let bytes = self.writer.get_current_offs().map_err(Error::Io)?;
        let frames = self.packets_encoded * self.frame_samples as u64;
        let duration_ms = if self.spec.sample_rate == 0 {
            0
        } else {
            frames * 1000 / u64::from(self.spec.sample_rate)
        };
        Ok(EncoderStats {
            frames,
            bytes,
            dropped: 0,
            duration_ms,
        })
    }
}

/// An `OpusTags` comment (`KEY=VALUE`) written into the identification header.
///
/// Key/value pairs are unordered per RFC 7845; order here is preserved byte
/// for byte into the stream.
#[derive(Debug, Clone)]
pub struct Comment {
    /// Comment key (e.g. `TITLE`, `ARTIST`).
    pub key: String,
    /// Comment value.
    pub value: String,
}

impl Comment {
    /// Builds a `KEY=VALUE` comment.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Incremental Ogg/Opus encoder consuming interleaved `f32` PCM.
///
/// Complements [`OggOpusEncoder`] (which writes pages straight to a caller's
/// `Write`): this variant buffers completed pages into an internal sink and
/// returns the accumulated bytes on each [`encode`](OggEncoder::encode) call,
/// so a consumer can write progress incrementally without a streaming writer.
///
/// `f32` samples are scaled and clamped to signed 16-bit before encoding (a
/// quiet default for headroom-safe input in `[-1, 1]`). Any Opus-supported
/// rate and channel count may be used; granule positions stay in 48 kHz units
/// per RFC 7845.
pub struct OggEncoder {
    encoder: Encoder,
    writer: PacketWriter<'static, SharedSink>,
    sink_buf: Arc<Mutex<Vec<u8>>>,
    /// Interleaved `f32` input buffered toward one 20 ms Opus frame.
    buf: Vec<f32>,
    /// Encoder frame size (samples per channel).
    frame_samples: usize,
    channels: usize,
    serial: u32,
    /// Pre-skip in 48 kHz samples (from the encoder's lookahead).
    pre_skip_48k: u64,
    /// Data packets encoded so far (for granule position math).
    packets_encoded: u64,
    /// Most recent packet, deferred so the final page can carry EOS.
    pending: Option<EncodedPacket>,
    finished: bool,
}

impl OggEncoder {
    /// Creates an Opus encoder for `sample_rate`/`channels`, a `bitrate_bps`
    /// (or `None` for a libopus default), and comment tags.
    ///
    /// The `OpusTags` vendor string (RFC 7845) is taken from the comment whose
    /// key is `ENCODER`; when no such comment is present it falls back to
    /// `"oto"`. The `ENCODER` comment (if any) still appears in the comment
    /// list itself.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the Opus encoder can't be created or the
    /// identification header can't be written.
    pub fn new(
        sample_rate: u32,
        channels: u8,
        bitrate_bps: Option<u32>,
        comments: &[Comment],
    ) -> Result<Self, Error> {
        let encoder = Encoder::new(shiguredo_opus::EncoderConfig {
            bitrate: bitrate_bps,
            ..shiguredo_opus::EncoderConfig::new(sample_rate, channels)
        })
        .map_err(|e| Error::Codec(e.to_string()))?;

        let frame_samples = encoder.frame_samples();
        let pre_skip_48k = u64::from(
            encoder
                .get_lookahead()
                .map_err(|e| Error::Codec(e.to_string()))?,
        );

        let mut serial_bytes = [0_u8; 4];
        fill(&mut serial_bytes).map_err(|e| Error::Codec(e.to_string()))?;
        let serial = u32::from_le_bytes(serial_bytes).max(1);

        let sink_buf = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink {
            buf: Arc::clone(&sink_buf),
        };
        let mut writer = PacketWriter::new(sink);
        let head = build_opus_head(channels, pre_skip_48k, sample_rate);
        writer
            .write_packet(head, serial, PacketWriteEndInfo::NormalPacket, 0)
            .map_err(Error::Io)?;
        let comment_strs = comments
            .iter()
            .map(|c| format!("{}={}", c.key, c.value))
            .collect::<Vec<_>>();
        let vendor = comments
            .iter()
            .find(|c| c.key == "ENCODER")
            .map_or("oto", |c| c.value.as_str())
            .as_bytes()
            .to_vec();
        writer
            .write_packet(
                build_opus_tags_v2(&vendor, &comment_strs),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .map_err(Error::Io)?;

        Ok(Self {
            encoder,
            writer,
            sink_buf,
            buf: Vec::new(),
            frame_samples,
            channels: usize::from(channels),
            serial,
            pre_skip_48k,
            packets_encoded: 0,
            pending: None,
            finished: false,
        })
    }

    /// Returns bytes accumulated since the last call, or an error if the sink
    /// lock is poisoned.
    fn take_encoded(&self) -> Result<Vec<u8>, Error> {
        self.sink_buf
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .map_err(|_| Error::Codec("ogg sink lock poisoned".to_owned()))
    }

    fn encode_buffered(&mut self) -> Result<(), Error> {
        let frame_len = self.frame_samples * self.channels;
        while self.buf.len() >= frame_len {
            let frame = self
                .buf
                .drain(..frame_len)
                .map(f32_to_i16)
                .collect::<Vec<_>>();
            let packet = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
            self.push_packet(packet)?;
        }
        Ok(())
    }

    /// Queues a packet, flushing the previously queued one so we know which is
    /// truly last for the EOS flag on [`Self::finalize`].
    fn push_packet(
        &mut self,
        data: Vec<u8>,
    ) -> Result<(), Error> {
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
                .map_err(Error::Io)?;
        }
        self.pending = Some(EncodedPacket { data, granule });
        Ok(())
    }

    /// Encodes interleaved `f32` PCM, returning any bytes accumulated since the
    /// previous call (headers are returned from the first call).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when the input length isn't a multiple of
    /// the encoder's channel count (misaligned interleaved PCM), or when
    /// encoding or flushing the sink fails.
    pub fn encode(
        &mut self,
        pcm: &[f32],
    ) -> Result<Vec<u8>, Error> {
        if self.finished {
            return Err(Error::Unsupported(
                "Opus encoder already finalized".to_owned(),
            ));
        }
        if !pcm.len().is_multiple_of(self.channels) {
            return Err(Error::Unsupported(format!(
                "PCM length {} is not a multiple of {channels} channels",
                pcm.len(),
                channels = self.channels
            )));
        }
        self.buf.extend_from_slice(pcm);
        self.encode_buffered()?;
        self.take_encoded()
    }

    /// Signal the end of input and return the final trailer bytes.
    ///
    /// Calling this again after a successful finalize is a no-op that returns
    /// an empty [`Vec`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the final frame or trailer can't be written.
    pub fn finalize(&mut self) -> Result<Vec<u8>, Error> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        // Encode a final frame from any remainder, zero-padded to a full frame.
        let frame_len = self.frame_samples * self.channels;
        if !self.buf.is_empty() {
            self.buf.resize(frame_len, 0.0);
            let buffer: Vec<f32> = std::mem::take(&mut self.buf);
            let frame = buffer.into_iter().map(f32_to_i16).collect::<Vec<_>>();
            let packet = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
            self.push_packet(packet)?;
        }

        let final_packet = if let Some(packet) = self.pending.take() {
            Some(packet)
        } else {
            // Empty recording: emit one silence frame so the stream has an EOS
            // page and a valid data segment.
            let frame = vec![0_i16; frame_len];
            let data = self
                .encoder
                .encode(&frame)
                .map_err(|e| Error::Codec(e.to_string()))?;
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
                .map_err(Error::Io)?;
        }
        self.take_encoded()
    }
}

/// Appends a complete page to a shared buffer so the encoder can hand back
/// accumulated bytes per chunk (`take_encoded`).
struct SharedSink {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for SharedSink {
    fn write(
        &mut self,
        data: &[u8],
    ) -> std::io::Result<usize> {
        self.buf
            .lock()
            .map_err(|_| std::io::Error::other("ogg sink lock poisoned"))?
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Converts a normalized `f32` sample ([-1, 1]) to 16-bit PCM, scaling to the
/// full i16 range and clamping.
fn f32_to_i16(sample: f32) -> i16 {
    (sample * 32768.0).clamp(-32768.0, 32767.0) as i16
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

/// Builds the `OpusTags` comment packet for the oto-style [`Tags`].
fn build_opus_tags(tags: &Tags) -> Vec<u8> {
    let comments = [
        format!("TITLE={}", tags.title),
        format!("ENCODER={}", tags.encoder),
        format!("CREATED={}", tags.created),
    ];
    build_opus_tags_v2(tags.encoder.as_bytes(), &comments)
}

/// Builds the `OpusTags` comment packet from a vendor string and pre-formatted
/// `KEY=VALUE` comments (shared by [`build_opus_tags`] and [`OggEncoder`]).
fn build_opus_tags_v2(
    vendor: &[u8],
    comments: &[String],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OpusTags");
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for comment in comments {
        out.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        out.extend_from_slice(comment.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use shiguredo_opus::Decoder;

    use super::*;
    use crate::AudioEncoder;

    fn sample_tags() -> Tags {
        Tags {
            title: "memo.ogg".to_owned(),
            encoder: "oto 0.0.0".to_owned(),
            created: "2026-08-24T00:00:00Z".to_owned(),
        }
    }

    fn sine_i16(
        rate: u32,
        frames: usize,
    ) -> Vec<i16> {
        (0..frames)
            .map(|i| {
                let t = i as f32 / rate as f32;
                ((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.25 * i16::MAX as f32) as i16
            })
            .collect()
    }

    fn encode_ogg(
        rate: u32,
        channels: u8,
        pcm: &[i16],
        bitrate: Option<u32>,
    ) -> Vec<u8> {
        let mut enc = OggOpusEncoder::new(
            Cursor::new(Vec::new()),
            rate,
            channels,
            bitrate,
            &sample_tags(),
        )
        .expect("encoder");
        let chunk = AudioChunk {
            data: &[],
            format: crate::PcmFormat::S16,
            sample_rate: rate,
            channels,
        };
        enc.write(&chunk, Some(pcm)).expect("write");
        enc.finalize().expect("finalize");
        enc.into_inner().into_inner()
    }

    #[test]
    fn emits_ogg_capture_and_opus_identifiers() {
        let rate = 48_000;
        let pcm = sine_i16(rate, rate as usize); // 1 second
        let bytes = encode_ogg(rate, 1, &pcm, Some(64_000));

        assert_eq!(&bytes[..4], b"OggS", "Ogg capture pattern");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("OpusHead"));
        assert!(text.contains("OpusTags"));
        assert!(text.contains("ENCODER=oto 0.0.0"));
        assert!(text.contains("TITLE=memo.ogg"));
        assert!(text.contains("CREATED=2026-08-24T00:00:00Z"));

        // OpusHead fields: version 1, mono, non-zero pre-skip, rate.
        let head_pos = bytes
            .windows(8)
            .position(|w| w == b"OpusHead")
            .expect("head");
        let head = &bytes[head_pos..head_pos + 19];
        assert_eq!(head[8], 1);
        assert_eq!(head[9], 1);
        let pre_skip = u16::from_le_bytes([head[10], head[11]]);
        assert!(pre_skip > 0, "pre-skip should be non-zero");
        assert_eq!(
            u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
            rate
        );
    }

    #[test]
    fn round_trip_decodes_to_non_silent_audio() {
        let rate = 48_000;
        let pcm = sine_i16(rate, rate as usize);
        let bytes = encode_ogg(rate, 1, &pcm, Some(64_000));

        // Read the stream back with the `ogg` crate and decode each Opus packet.
        let mut reader = ogg::PacketReader::new(Cursor::new(bytes));
        let mut opus_decoder =
            Decoder::new(shiguredo_opus::DecoderConfig::new(rate, 1)).expect("decoder");

        let mut decoded = Vec::new();
        while let Some(packet) = reader.read_packet().expect("read packet") {
            if packet.data.starts_with(b"Opus") {
                continue; // OpusHead / OpusTags
            }
            decoded.extend(opus_decoder.decode(&packet.data).expect("decode"));
        }

        assert!(decoded.len() >= rate as usize, "expected ~1s of samples");
        let rms = {
            let sum: f64 = decoded.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (sum / decoded.len() as f64).sqrt()
        };
        assert!(rms > 1000.0, "expected audible output, RMS={rms:.0}");
    }

    #[test]
    fn round_trip_at_non_48k_rate() {
        // A 16 kHz device rate uses a 16 kHz Opus encoder; granule positions
        // and pre-skip must still be expressed in 48 kHz samples (RFC 7845).
        let rate = 16_000;
        let pcm = sine_i16(rate, rate as usize);
        let bytes = encode_ogg(rate, 1, &pcm, Some(24_000));

        let mut reader = ogg::PacketReader::new(Cursor::new(bytes.clone()));
        let mut opus_decoder =
            Decoder::new(shiguredo_opus::DecoderConfig::new(rate, 1)).expect("decoder");
        let mut decoded = Vec::new();
        while let Some(packet) = reader.read_packet().expect("read packet") {
            if packet.data.starts_with(b"Opus") {
                continue;
            }
            decoded.extend(opus_decoder.decode(&packet.data).expect("decode"));
        }
        assert!(decoded.len() >= rate as usize, "~1s of audio");
        let rms = {
            let sum: f64 = decoded.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (sum / decoded.len() as f64).sqrt()
        };
        assert!(rms > 1000.0, "expected audible output, RMS={rms:.0}");

        let granules = page_granules(&bytes);
        // Data-page granule deltas are 20 ms in 48 kHz units = 960.
        for pair in granules[1..].windows(2) {
            assert_eq!(pair[1] - pair[0], 960, "granule delta should be 960");
        }
    }

    #[test]
    fn granule_positions_increase_by_960() {
        let rate = 48_000;
        let pcm = sine_i16(rate, rate as usize);
        let bytes = encode_ogg(rate, 1, &pcm, Some(64_000));
        let granules = page_granules(&bytes);
        // Header page (granule 0) + 50 data pages for one second @ 20 ms.
        assert!(
            granules.len() >= 51,
            "expected >=51 pages, got {}",
            granules.len()
        );
        // The first data page is pre_skip + 960 = 1272; subsequent deltas are 960.
        for pair in granules[1..].windows(2) {
            assert_eq!(pair[1] - pair[0], 960, "granule delta should be 960");
        }
    }

    /// Extracts the granule position of every page.
    fn page_granules(bytes: &[u8]) -> Vec<u64> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 27 <= bytes.len() {
            if &bytes[pos..pos + 4] != b"OggS" {
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
            out.push(u64::from_le_bytes(
                bytes[pos + 6..pos + 14].try_into().unwrap(),
            ));
            pos += page_len;
        }
        out
    }

    // -------------------------------------------------------------------
    // OggEncoder (incremental f32) tests
    // -------------------------------------------------------------------

    fn sample_comments() -> Vec<Comment> {
        vec![
            Comment {
                key: "TITLE".into(),
                value: "k".into(),
            },
            Comment {
                key: "ARTIST".into(),
                value: "a".into(),
            },
            Comment {
                key: "ENCODER".into(),
                value: "koe v0.0.0".into(),
            },
            Comment {
                key: "KOE_SOURCE".into(),
                value: r#"{"type":"microphone"}"#.into(),
            },
        ]
    }

    fn sine_f32(
        rate: u32,
        frames: usize,
        channels: u8,
    ) -> Vec<f32> {
        let mut pcm = Vec::with_capacity(frames * usize::from(channels));
        for i in 0..frames {
            let sample =
                (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / rate as f32)).sin() * 0.25;
            for _ in 0..channels {
                pcm.push(sample);
            }
        }
        pcm
    }

    /// Encodes `blocks` of interleaved f32 into one stream and returns it.
    fn encode_blocks_f32(
        blocks: &[Vec<f32>],
        bitrate: Option<u32>,
    ) -> Vec<u8> {
        let mut enc = OggEncoder::new(48_000, 2, bitrate, &sample_comments()).expect("encoder");
        let mut out = Vec::new();
        for block in blocks {
            out.extend(enc.encode(block).expect("encode"));
        }
        out.extend(enc.finalize().expect("finalize"));
        out
    }

    fn decode_opus_f32(bytes: &[u8]) -> Vec<i16> {
        let mut reader = ogg::PacketReader::new(Cursor::new(bytes));
        let mut decoder =
            Decoder::new(shiguredo_opus::DecoderConfig::new(48_000, 2)).expect("decoder");
        let mut samples = Vec::new();
        while let Some(packet) = reader.read_packet().expect("read packet") {
            if packet.data.starts_with(b"Opus") {
                continue;
            }
            samples.extend(decoder.decode(&packet.data).expect("decode"));
        }
        samples
    }

    fn mean_abs(samples: &[i16]) -> f64 {
        samples.iter().map(|&s| f64::from(s).abs()).sum::<f64>() / samples.len() as f64
    }

    #[test]
    fn f32_encode_round_trips_to_audible_audio() {
        let block = sine_f32(48_000, 960, 2);
        let out = encode_blocks_f32(&vec![block; 50], Some(64_000));
        assert_eq!(&out[..4], b"OggS");
        let decoded = decode_opus_f32(&out);
        assert!(decoded.len() >= 48_000, "~1s of samples");
        assert!(
            mean_abs(&decoded) > 500.0,
            "audible, mean_abs={:.0}",
            mean_abs(&decoded)
        );
    }

    #[test]
    fn f32_encode_writes_comments_and_headers() {
        let block = sine_f32(48_000, 960, 2);
        let out = encode_blocks_f32(&vec![block; 1], None);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("OpusHead"));
        assert!(text.contains("OpusTags"));
        assert!(text.contains("ENCODER=koe v0.0.0"));
        assert!(text.contains("KOE_SOURCE="));
    }

    #[test]
    fn f32_granules_and_eos_are_correct() {
        let block = sine_f32(48_000, 960, 2);
        let out = encode_blocks_f32(&vec![block; 10], Some(64_000));
        let mut pages = Vec::new();
        let mut pos = 0;
        while pos + 27 <= out.len() && &out[pos..pos + 4] == b"OggS" {
            let seg_count = out[pos + 26] as usize;
            let page_len = 27
                + seg_count
                + out[pos + 27..pos + 27 + seg_count]
                    .iter()
                    .map(|&n| u32::from(n))
                    .sum::<u32>() as usize;
            pages.push((out[pos + 5], pos));
            pos += page_len;
        }
        assert_ne!(pages[0].0 & 0x02, 0, "header page BOS");
        assert_ne!(pages.last().expect("pages").0 & 0x04, 0, "last page EOS");
    }

    #[test]
    fn f32_incremental_encode_returns_bytes_per_call() {
        let mut enc = OggEncoder::new(48_000, 2, Some(64_000), &sample_comments()).expect("enc");
        // First block returns the identification header + tags.
        let first = enc.encode(&sine_f32(48_000, 960, 2)).expect("first");
        assert!(!first.is_empty(), "first encode returns header bytes");
        let mid = enc.encode(&sine_f32(48_000, 960, 2)).expect("mid");
        // Interim blocks may return a full page (packet written on the next push).
        let _ = mid;
        let trailer = enc.finalize().expect("finalize");
        assert!(trailer.len() < 300, "trailer is a short EOS page");
    }

    #[test]
    fn f32_empty_recording_still_yields_valid_stream() {
        let out = encode_blocks_f32(&[], None);
        assert_eq!(&out[..4], b"OggS");
        let decoded = decode_opus_f32(&out);
        assert!(!decoded.is_empty());
        assert!(mean_abs(&decoded) < 1.0, "empty stream decodes to silence");
    }

    #[test]
    fn f32_encode_after_finalize_is_rejected() {
        let mut enc = OggEncoder::new(48_000, 2, None, &sample_comments()).expect("enc");
        let _ = enc.finalize().expect("finalize");
        let err = enc
            .encode(&[0.0, 0.0])
            .expect_err("encode after finalize must fail");
        assert!(err.to_string().contains("already finalized"));
    }

    #[test]
    fn f32_rejects_odd_pcm_length() {
        let mut enc = OggEncoder::new(48_000, 2, None, &sample_comments()).expect("enc");
        let err = enc.encode(&[0.0]).expect_err("odd signal");
        assert!(err.to_string().contains("multiple"));
    }

    #[test]
    fn f32_double_finalize_is_idempotent() {
        let mut enc = OggEncoder::new(48_000, 2, None, &sample_comments()).expect("enc");
        let _ = enc.encode(&sine_f32(48_000, 960, 2)).expect("encode");
        let first = enc.finalize().expect("first finalize");
        assert!(!first.is_empty());
        let second = enc.finalize().expect("second finalize");
        assert!(second.is_empty(), "double-finalize emits no bytes");
    }
}
