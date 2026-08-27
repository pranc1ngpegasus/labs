//! Decode arbitrary audio files to canonical PCM for offline transcription.
//!
//! Canonical format matches the live pipeline: 48 kHz, Float32, interleaved
//! stereo. Supported containers/codecs are whatever Symphonia is built with
//! (WAV/AIFF/FLAC/OGG/MP3/AAC).

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::Channels;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::MainError;

/// Target sample rate for ASR / pipeline compatibility.
pub const CANONICAL_SAMPLE_RATE_HZ: u32 = 48_000;
/// Target channel count (interleaved L/R).
pub const CANONICAL_CHANNELS: usize = 2;
/// Frames per feed chunk (~100 ms).
pub const CHUNK_FRAMES: usize = CANONICAL_SAMPLE_RATE_HZ as usize / 10;

/// Metadata about a decoded (and optionally windowed) audio stream.
#[derive(Debug, Clone, Copy)]
pub struct DecodedAudioInfo {
    /// Total duration of the source file, when the container reports it.
    pub source_duration_ms: Option<u64>,
    /// Duration of the PCM window that will be fed to ASR.
    pub window_duration_ms: u64,
    /// Sample rate of the source before resampling.
    pub source_sample_rate_hz: u32,
    /// Channel count of the source before up/down-mix.
    pub source_channels: usize,
}

/// Decodes `path` to canonical PCM, applying an optional `[start_ms, end_ms)` window.
///
/// Returns interleaved stereo f32 at 48 kHz plus stream info for progress UI.
pub fn decode_to_canonical(
    path: &Path,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
) -> Result<(Vec<f32>, DecodedAudioInfo), MainError> {
    let (mut format, track) = open_format(path)?;
    let track_id = track.id;
    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(CodecParameters::audio)
        .ok_or_else(|| MainError::InvalidArgs(format!("no audio codec in '{}'", path.display())))?;
    let source_rate = audio_params.sample_rate.ok_or_else(|| {
        MainError::InvalidArgs(format!("sample rate unknown for '{}'", path.display()))
    })?;
    let source_channels = audio_params
        .channels
        .as_ref()
        .map_or(1, Channels::count)
        .max(1);

    let source_duration_ms = track_duration_ms(&track);
    let (start_ms, end_ms) = normalize_window(start_ms, end_ms, source_duration_ms)?;

    let seeked = maybe_seek_to_start(&mut *format, track_id, start_ms);
    // After a successful seek the decoder timeline restarts near `start_ms`, so
    // window bounds must be relative to that point. If seek failed (or was a
    // no-op), keep absolute frame indices and skip via `append_windowed`.
    let (start_frame, end_frame) = if seeked {
        let relative_end = match (start_ms, end_ms) {
            (Some(start), Some(end)) => Some(ms_to_frames(end.saturating_sub(start), source_rate)),
            (None, Some(end)) => Some(ms_to_frames(end, source_rate)),
            (_, None) => None,
        };
        (0, relative_end)
    } else {
        (
            start_ms.map_or(0, |ms| ms_to_frames(ms, source_rate)),
            end_ms.map(|ms| ms_to_frames(ms, source_rate)),
        )
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
        .map_err(|err| {
            MainError::InvalidArgs(format!("unsupported codec in '{}': {err}", path.display()))
        })?;

    let raw = decode_window(
        path,
        &mut *format,
        &mut *decoder,
        track_id,
        source_channels,
        start_frame,
        end_frame,
    )?;

    if raw.is_empty() {
        return Err(MainError::InvalidArgs(format!(
            "no audio samples decoded from '{}' (check --start-at / --end-at)",
            path.display()
        )));
    }

    let stereo = to_stereo(&raw, source_channels);
    let pcm = if source_rate == CANONICAL_SAMPLE_RATE_HZ {
        stereo
    } else {
        resample_linear(&stereo, source_rate, CANONICAL_SAMPLE_RATE_HZ)
    };

    let window_frames = pcm.len() / CANONICAL_CHANNELS;
    #[allow(clippy::cast_precision_loss)]
    let window_duration_ms =
        ((window_frames as f64) * 1000.0 / f64::from(CANONICAL_SAMPLE_RATE_HZ)).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let window_duration_ms = window_duration_ms as u64;

    Ok((
        pcm,
        DecodedAudioInfo {
            source_duration_ms,
            window_duration_ms,
            source_sample_rate_hz: source_rate,
            source_channels,
        },
    ))
}

fn open_format(path: &Path) -> Result<(Box<dyn FormatReader>, Track), MainError> {
    let file = File::open(path)
        .map_err(|err| MainError::Io(format!("failed to open '{}': {err}", path.display())))?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|err| {
            MainError::InvalidArgs(format!(
                "unsupported or unreadable audio '{}': {err}",
                path.display()
            ))
        })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| MainError::InvalidArgs(format!("no audio track in '{}'", path.display())))?
        .clone();
    Ok((format, track))
}

/// Seeks near `start_ms`. Returns `true` when seek ran and succeeded so the
/// caller can switch to relative frame windowing.
fn maybe_seek_to_start(
    format: &mut dyn FormatReader,
    track_id: u32,
    start_ms: Option<u64>,
) -> bool {
    let Some(start) = start_ms.filter(|ms| *ms > 0) else {
        return false;
    };
    format
        .seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_millis_u64(start),
                track_id: Some(track_id),
            },
        )
        .is_ok()
}

fn decode_window(
    path: &Path,
    format: &mut dyn FormatReader,
    decoder: &mut dyn AudioDecoder,
    track_id: u32,
    source_channels: usize,
    start_frame: u64,
    end_frame: Option<u64>,
) -> Result<Vec<f32>, MainError> {
    let mut raw_interleaved = Vec::new();
    let mut frames_seen: u64 = 0;
    let channels = source_channels.max(1);

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            },
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            },
            Err(SymphoniaError::IoError(err)) => {
                return Err(MainError::Io(format!(
                    "read error in '{}': {err}",
                    path.display()
                )));
            },
            Err(err) => {
                return Err(MainError::InvalidArgs(format!(
                    "failed decoding '{}': {err}",
                    path.display()
                )));
            },
        };

        if packet.track_id != track_id {
            continue;
        }

        let audio_buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(err) => {
                return Err(MainError::InvalidArgs(format!(
                    "decode error in '{}': {err}",
                    path.display()
                )));
            },
        };

        // Copy the decoded packet into interleaved f32, converting from whatever
        // sample format the codec produced.
        let total_samples = audio_buf.samples_interleaved();
        let mut samples = vec![0.0f32; total_samples];
        audio_buf.copy_to_slice_interleaved(&mut samples);

        let frames_in_packet = u64::try_from(total_samples / channels).unwrap_or(u64::MAX);

        append_windowed(
            &mut raw_interleaved,
            &samples,
            channels,
            frames_seen,
            frames_in_packet,
            start_frame,
            end_frame,
        );
        frames_seen = frames_seen.saturating_add(frames_in_packet);

        if end_frame.is_some_and(|end| frames_seen >= end) {
            break;
        }
    }

    Ok(raw_interleaved)
}

fn track_duration_ms(track: &Track) -> Option<u64> {
    let n_frames = track.num_frames?;
    let tb = track.time_base.or_else(|| {
        track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .and_then(|p| p.sample_rate)
            .and_then(|rate| TimeBase::try_new(1, rate))
    })?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let time = tb.calc_time(Timestamp::new(i64::try_from(n_frames).ok()?))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((time.as_secs_f64() * 1000.0).round() as u64)
}

fn normalize_window(
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    source_duration_ms: Option<u64>,
) -> Result<(Option<u64>, Option<u64>), MainError> {
    if let (Some(start), Some(end)) = (start_ms, end_ms)
        && start >= end
    {
        return Err(MainError::InvalidArgs(
            "--start-at must be less than --end-at".into(),
        ));
    }
    if let (Some(start), Some(total)) = (start_ms, source_duration_ms)
        && start >= total
    {
        return Err(MainError::InvalidArgs(format!(
            "--start-at ({start}ms) is past the end of the file ({total}ms)"
        )));
    }
    let end_ms = match (end_ms, source_duration_ms) {
        (Some(end), Some(total)) if end > total => {
            eprintln!("warning: --end-at ({end}ms) exceeds file duration ({total}ms); clamping");
            Some(total)
        },
        (end, _) => end,
    };
    Ok((start_ms, end_ms))
}

fn ms_to_frames(
    ms: u64,
    sample_rate: u32,
) -> u64 {
    ms.saturating_mul(u64::from(sample_rate)) / 1000
}

fn append_windowed(
    out: &mut Vec<f32>,
    samples: &[f32],
    channels: usize,
    frames_seen: u64,
    frames_in_packet: u64,
    start_frame: u64,
    end_frame: Option<u64>,
) {
    let packet_start = frames_seen;
    let packet_end = frames_seen.saturating_add(frames_in_packet);
    let window_start = start_frame.max(packet_start);
    let window_end = end_frame.unwrap_or(u64::MAX).min(packet_end);
    if window_start >= window_end {
        return;
    }
    let Ok(local_start) = usize::try_from(window_start - packet_start) else {
        return;
    };
    let Ok(local_end) = usize::try_from(window_end - packet_start) else {
        return;
    };
    let sample_start = local_start.saturating_mul(channels);
    let sample_end = local_end.saturating_mul(channels).min(samples.len());
    if sample_start < sample_end {
        out.extend_from_slice(&samples[sample_start..sample_end]);
    }
}

fn to_stereo(
    interleaved: &[f32],
    channels: usize,
) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames * CANONICAL_CHANNELS);
    match channels {
        1 => {
            for &sample in interleaved.iter().take(frames) {
                out.push(sample);
                out.push(sample);
            }
        },
        2 => out.extend_from_slice(&interleaved[..frames * 2]),
        _ => {
            for frame in 0..frames {
                let base = frame * channels;
                out.push(interleaved[base]);
                out.push(interleaved[base + 1]);
            }
        },
    }
    out
}

/// Linear resampler for speech (quality is secondary to simplicity here).
fn resample_linear(
    interleaved_stereo: &[f32],
    from_hz: u32,
    to_hz: u32,
) -> Vec<f32> {
    if from_hz == 0 || to_hz == 0 || interleaved_stereo.is_empty() {
        return Vec::new();
    }
    let in_frames = interleaved_stereo.len() / CANONICAL_CHANNELS;
    if in_frames == 0 {
        return Vec::new();
    }
    let out_frames =
        usize::try_from((in_frames as u64).saturating_mul(u64::from(to_hz)) / u64::from(from_hz))
            .unwrap_or(0);
    if out_frames == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(out_frames * CANONICAL_CHANNELS);
    let ratio = f64::from(from_hz) / f64::from(to_hz);
    for out_frame in 0..out_frames {
        #[allow(clippy::cast_precision_loss)]
        let src = out_frame as f64 * ratio;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i0 = src.floor() as usize;
        let i1 = (i0 + 1).min(in_frames - 1);
        #[allow(clippy::cast_possible_truncation)]
        let frac = (src - src.floor()) as f32;
        for ch in 0..CANONICAL_CHANNELS {
            let s0 = interleaved_stereo[i0 * CANONICAL_CHANNELS + ch];
            let s1 = interleaved_stereo[i1 * CANONICAL_CHANNELS + ch];
            out.push(s0.mul_add(1.0 - frac, s1 * frac));
        }
    }
    out
}

/// Split canonical PCM into ~100 ms feed chunks.
pub fn chunk_pcm(pcm: &[f32]) -> impl Iterator<Item = &[f32]> {
    let chunk_samples = CHUNK_FRAMES * CANONICAL_CHANNELS;
    pcm.chunks(chunk_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_wav_i16(
        path: &Path,
        sample_rate: u32,
        channels: u16,
        samples: &[i16],
    ) {
        let data_bytes = u32::try_from(samples.len() * 2).expect("fixture too large");
        let mut f = File::create(path).expect("create wav");
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_bytes).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        let byte_rate = sample_rate * u32::from(channels) * 2;
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        let block_align = channels * 2;
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_bytes.to_le_bytes()).unwrap();
        for s in samples {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }

    fn temp_wav(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "koe-transcribe-test-{name}-{}.wav",
            std::process::id()
        ));
        path
    }

    #[test]
    fn decodes_wav_to_48k_stereo() {
        let path = temp_wav("basic");
        let samples = vec![0i16; 1600];
        write_wav_i16(&path, 16_000, 1, &samples);
        let (pcm, info) = decode_to_canonical(&path, None, None).expect("decode");
        let _ = std::fs::remove_file(&path);
        assert_eq!(info.source_sample_rate_hz, 16_000);
        assert_eq!(info.source_channels, 1);
        assert_eq!(pcm.len() % 2, 0);
        let frames = pcm.len() / 2;
        assert!((4790..=4810).contains(&frames), "frames={frames}");
    }

    #[test]
    fn start_end_window_shortens_output() {
        let path = temp_wav("window");
        let samples = vec![1000i16; 48_000];
        write_wav_i16(&path, 48_000, 1, &samples);
        let (pcm, info) = decode_to_canonical(&path, Some(200), Some(500)).expect("decode window");
        let _ = std::fs::remove_file(&path);
        assert!((290..=310).contains(&info.window_duration_ms));
        let frames = pcm.len() / 2;
        assert!((14_000..=15_000).contains(&frames), "frames={frames}");
    }

    #[test]
    fn start_end_window_keeps_marker_samples() {
        // 1s @ 48 kHz mono: silence, then a loud marker only in [200ms, 500ms).
        let mut samples = vec![0i16; 48_000];
        for sample in &mut samples[9_600..24_000] {
            *sample = 16_000;
        }
        let path = temp_wav("marker");
        write_wav_i16(&path, 48_000, 1, &samples);
        let (pcm, _) = decode_to_canonical(&path, Some(200), Some(500)).expect("decode");
        let _ = std::fs::remove_file(&path);

        let peak = pcm
            .iter()
            .copied()
            .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
        assert!(
            peak > 0.4,
            "expected marker energy inside the window, peak={peak}"
        );

        // Decode only the silent head [0ms, 100ms) — marker begins at 200ms.
        let path = temp_wav("silent-head");
        write_wav_i16(&path, 48_000, 1, &samples);
        let (head, _) = decode_to_canonical(&path, Some(0), Some(100)).expect("decode head");
        let _ = std::fs::remove_file(&path);
        let head_peak = head
            .iter()
            .copied()
            .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
        assert!(
            head_peak < 0.05,
            "expected silence before marker, peak={head_peak}"
        );
    }

    #[test]
    fn rejects_start_after_end() {
        let path = temp_wav("bad-window");
        write_wav_i16(&path, 48_000, 1, &[0; 4800]);
        let err = decode_to_canonical(&path, Some(500), Some(100)).expect_err("bad window");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, MainError::InvalidArgs(_)));
    }

    #[test]
    fn unsupported_extension_errors_gracefully() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "koe-transcribe-not-audio-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, b"not audio").unwrap();
        let err = decode_to_canonical(&path, None, None).expect_err("not audio");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, MainError::InvalidArgs(_)));
    }

    #[test]
    fn resample_and_stereo_helpers() {
        let mono = [0.0f32, 1.0, 0.0, -1.0];
        let stereo = to_stereo(&mono, 1);
        assert_eq!(stereo, vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, -1.0, -1.0]);
        let up = resample_linear(&[0.0, 0.0, 1.0, 1.0], 24_000, 48_000);
        assert_eq!(up.len() / 2, 4);
    }
}
