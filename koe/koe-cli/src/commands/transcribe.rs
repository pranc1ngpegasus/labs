//! `koe transcribe` — offline transcription of an existing audio file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koe_core::{
    AudioSourceConfig, TranscriptFormat, TranscriptMeta, TranscriptionCallback,
    TranscriptionSegment, create_formatter, default_transcript_path, feed_transcription_audio,
    finalize_transcription, start_transcription,
};
use usage::Args;

use super::Run;
use super::decode::{CANONICAL_SAMPLE_RATE_HZ, DecodedAudioInfo, decode_to_canonical};
use super::duration::parse_duration;
use super::{parse_speech_engine, parse_transcript_format};
use crate::MainError;
use crate::config::KoeConfig;

/// Speech framework recognition tasks are bounded; keep each request short
/// enough that finalization can reliably produce a final result.
const RECOGNITION_WINDOW_MS: u64 = 50_000;

/// Feed granularity: `SFSpeechAudioBufferRecognitionRequest` consumes audio at
/// (near) real-time, so appending the whole window in one burst then calling
/// endAudio finalizes before the recognizer has ingested anything ("No speech
/// detected"). Feed ~1s chunks and pause briefly between them so the streaming
/// recognizer keeps up; the pause is ~2% of the audio duration.
const FEED_CHUNK_FRAMES: usize = CANONICAL_SAMPLE_RATE_HZ as usize;
/// Per-1s-chunk settle time before the recognizer ingests the next chunk.
const FEED_PACE: Duration = Duration::from_millis(20);

/// Transcribe an existing audio file without recording.
#[derive(Debug, Args)]
pub struct TranscribeArgs {
    /// Input audio file (WAV / FLAC / OGG / MP3 / AAC / AIFF).
    pub input: PathBuf,

    /// Speech recognition locale (BCP-47).
    #[usage(long)]
    pub locale: Option<String>,

    /// Transcript output path (default: `<input>.<format>`).
    #[usage(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Transcript format: `txt`, `srt`, `vtt`, or `json`.
    #[usage(long)]
    pub format: Option<String>,

    /// Speech engine: `auto` (default), `on-device`, or `network`.
    ///
    /// `on-device` never sends audio to Apple; it errors if unavailable.
    /// `network` always uses Apple's servers. `auto` prefers on-device and
    /// falls back to network with a warning.
    #[usage(long)]
    pub engine: Option<String>,

    /// Start transcribing from this offset (e.g. `30s`, `1m30s`).
    #[usage(long)]
    pub start_at: Option<String>,

    /// Stop transcribing at this offset (same syntax as `--start-at`).
    #[usage(long)]
    pub end_at: Option<String>,
}

impl Run for TranscribeArgs {
    fn run(
        self,
        config: &KoeConfig,
    ) -> Result<(), MainError> {
        let prepared = prepare(&self, config)?;
        run_transcription(&prepared)
    }
}

#[derive(Debug)]
struct Prepared {
    input: PathBuf,
    output: PathBuf,
    locale: String,
    speech_engine: koe_core::SpeechEngine,
    format: TranscriptFormat,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
}

fn prepare(
    args: &TranscribeArgs,
    config: &KoeConfig,
) -> Result<Prepared, MainError> {
    if !args.input.exists() {
        return Err(MainError::Io(format!(
            "input file not found: {}",
            args.input.display()
        )));
    }
    if !args.input.is_file() {
        return Err(MainError::InvalidArgs(format!(
            "input is not a file: {}",
            args.input.display()
        )));
    }

    let locale = crate::config::transcribe_locale(args.locale.clone(), config);
    let engine_name = crate::config::transcribe_engine(args.engine.clone(), config);
    let speech_engine = parse_speech_engine(&engine_name)?;
    let format_name = crate::config::transcribe_format(args.format.clone(), config);
    let format = parse_transcript_format(&format_name)?;
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_transcript_path(&args.input, format));
    if output.exists() {
        return Err(MainError::Io(format!(
            "output already exists: {}",
            output.display()
        )));
    }

    let start_ms = parse_offset_ms(args.start_at.as_deref())?;
    let end_ms = parse_offset_ms(args.end_at.as_deref())?;

    if let (Some(start), Some(end)) = (start_ms, end_ms)
        && start >= end
    {
        return Err(MainError::InvalidArgs(
            "--start-at must be less than --end-at".into(),
        ));
    }

    if locale.trim().is_empty() {
        return Err(MainError::InvalidArgs(
            "--locale must be a non-empty BCP-47 tag".into(),
        ));
    }

    Ok(Prepared {
        input: args.input.clone(),
        output,
        locale,
        speech_engine,
        format,
        start_ms,
        end_ms,
    })
}

fn parse_offset_ms(raw: Option<&str>) -> Result<Option<u64>, MainError> {
    raw.map(parse_duration)
        .transpose()
        .map_err(MainError::InvalidArgs)?
        .map(|d| {
            u64::try_from(d.as_millis())
                .map_err(|_| MainError::InvalidArgs("offset is too large".into()))
        })
        .transpose()
}

fn run_transcription(prepared: &Prepared) -> Result<(), MainError> {
    eprintln!("Decoding {}…", prepared.input.display());
    let (pcm, info) = decode_to_canonical(&prepared.input, prepared.start_ms, prepared.end_ms)?;
    report_decode(&info);

    let started = Instant::now();
    let total_ms = info.window_duration_ms.max(1);
    let samples_per_ms = 2 * u64::from(CANONICAL_SAMPLE_RATE_HZ) / 1000;

    let window_samples = usize::try_from(RECOGNITION_WINDOW_MS.saturating_mul(samples_per_ms))
        .map_err(|_| MainError::Internal("recognition window is too large".into()))?;
    let mut finals = Vec::new();
    let mut asr_error = None;
    let mut fed_ms: u64 = 0;
    for (window_index, window) in pcm.chunks(window_samples).enumerate() {
        let window_offset_ms = prepared.start_ms.unwrap_or(0).saturating_add(
            RECOGNITION_WINDOW_MS.saturating_mul(u64::try_from(window_index).unwrap_or(u64::MAX)),
        );
        let time_offset_ms = i64::try_from(window_offset_ms).unwrap_or(i64::MAX);
        let collector = SegmentCollector::new(time_offset_ms);
        let partial = Arc::clone(&collector.partial);
        let segments = Arc::clone(&collector.segments);
        let errors = Arc::clone(&collector.errors);
        let handle = start_transcription(
            prepared.locale.clone(),
            prepared.speech_engine,
            Box::new(collector),
        )
        .map_err(|err| match err {
            koe_core::TranscriptionError::PermissionDenied { .. } => {
                MainError::PermissionDenied(err.to_string())
            },
            other => MainError::Internal(format!("start transcription: {other}")),
        })?;

        // Feed in ~1s chunks with a small pause so the streaming recognizer
        // ingests each chunk (a burst-then-endAudio finalizes too early).
        for chunk in window.chunks(FEED_CHUNK_FRAMES * 2) {
            feed_transcription_audio(Arc::clone(&handle), chunk.to_vec());
            fed_ms = fed_ms
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(0) / samples_per_ms.max(1));
            report_progress(fed_ms.min(total_ms), total_ms, &partial);
            std::thread::sleep(FEED_PACE);
        }
        // Let the recognizer ingest the final tail before endAudio.
        std::thread::sleep(FEED_PACE);
        finalize_transcription(handle);

        if let Some(err) = errors.lock().ok().and_then(|guard| guard.clone()) {
            eprintln!("warning: transcription error in window {window_index}: {err}");
            asr_error = Some(err);
        }
        finals.extend(
            segments
                .lock()
                .map_err(|_| MainError::Internal("segment lock poisoned".into()))?
                .iter()
                .cloned(),
        );
    }

    if finals.is_empty() {
        if let Some(err) = asr_error {
            return Err(MainError::Internal(format!(
                "transcription failed with no segments: {err}"
            )));
        }
        eprintln!(
            "note: no final segments produced (native speech analyzer may be unavailable in this build)"
        );
    }

    let meta = TranscriptMeta::for_session(&AudioSourceConfig::Microphone, &prepared.locale);
    // JSON `source` still uses AudioSourceConfig (no file variant yet). Offline
    // transcription records Microphone as a stand-in until FFI gains a file source.
    let mut formatter = create_formatter(prepared.format, &meta);
    for segment in &finals {
        formatter.write_segment(segment);
    }
    let body = formatter.committed_output();
    write_output(&prepared.output, &body)?;

    eprintln!(
        "Wrote {} ({} final segment{}, wall {}, audio {})",
        prepared.output.display(),
        finals.len(),
        if finals.len() == 1 { "" } else { "s" },
        format_secs_display(started.elapsed().as_millis()),
        format_secs_display(u128::from(info.window_duration_ms)),
    );
    Ok(())
}

fn write_output(
    path: &Path,
    body: &str,
) -> Result<(), MainError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|err| {
            MainError::Io(format!(
                "failed to create output directory '{}': {err}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, body.as_bytes())
        .map_err(|err| MainError::Io(format!("failed to write '{}': {err}", path.display())))
}

fn report_decode(info: &DecodedAudioInfo) {
    let source_dur = info
        .source_duration_ms
        .map_or_else(|| "unknown".to_owned(), format_secs_display_u64);
    eprintln!(
        "Source: {} Hz, {} ch, duration {source_dur}; window {} @ {} Hz stereo",
        info.source_sample_rate_hz,
        info.source_channels,
        format_secs_display_u64(info.window_duration_ms),
        CANONICAL_SAMPLE_RATE_HZ,
    );
}

fn report_progress(
    fed_ms: u64,
    total_ms: u64,
    partial: &Arc<Mutex<Option<String>>>,
) {
    // Throttle: print about once per second of audio.
    if fed_ms % 1000 > 100 && fed_ms + 100 < total_ms {
        return;
    }
    let pct = (fed_ms.saturating_mul(100) / total_ms).min(100);
    let partial_text = partial
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default();
    if partial_text.is_empty() {
        eprint!(
            "\rTranscribing… {pct}% ({} / {})   ",
            format_secs_display_u64(fed_ms),
            format_secs_display_u64(total_ms)
        );
    } else {
        let preview: String = partial_text.chars().take(60).collect();
        let ellipsis = if partial_text.chars().count() > 60 {
            "…"
        } else {
            ""
        };
        eprint!("\rTranscribing… {pct}% | \"{preview}{ellipsis}\"   ");
    }
    let _ = std::io::Write::flush(&mut std::io::stderr());
    if fed_ms >= total_ms {
        eprintln!();
    }
}

fn format_secs_display(ms: u128) -> String {
    let whole = ms / 1000;
    let frac = (ms % 1000) / 100;
    format!("{whole}.{frac}s")
}

fn format_secs_display_u64(ms: u64) -> String {
    format_secs_display(u128::from(ms))
}

/// Collects ASR segments, shifting timestamps by the `--start-at` offset so
/// they align with the original file timeline.
struct SegmentCollector {
    offset_ms: i64,
    segments: Arc<Mutex<Vec<TranscriptionSegment>>>,
    partial: Arc<Mutex<Option<String>>>,
    errors: Arc<Mutex<Option<String>>>,
}

impl SegmentCollector {
    fn new(offset_ms: i64) -> Self {
        Self {
            offset_ms,
            segments: Arc::new(Mutex::new(Vec::new())),
            partial: Arc::new(Mutex::new(None)),
            errors: Arc::new(Mutex::new(None)),
        }
    }
}

impl TranscriptionCallback for SegmentCollector {
    fn on_segment(
        &self,
        mut segment: TranscriptionSegment,
    ) {
        segment.start_ms = segment.start_ms.saturating_add(self.offset_ms);
        segment.end_ms = segment.end_ms.saturating_add(self.offset_ms);
        if segment.is_final {
            if let Ok(mut partial) = self.partial.lock() {
                *partial = None;
            }
            if !segment.text.is_empty()
                && let Ok(mut segments) = self.segments.lock()
            {
                segments.push(segment);
            }
        } else if let Ok(mut partial) = self.partial.lock() {
            *partial = if segment.text.is_empty() {
                None
            } else {
                Some(segment.text)
            };
        }
    }

    fn on_error(
        &self,
        error: String,
    ) {
        if let Ok(mut slot) = self.errors.lock() {
            *slot = Some(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    use usage::{Cli, Subcommands};

    #[derive(Cli)]
    #[usage(bin = "koe")]
    struct TestCli {
        #[usage(subcommand)]
        command: TestCommand,
    }

    #[derive(Subcommands)]
    enum TestCommand {
        Transcribe(TranscribeArgs),
    }

    fn parse_transcribe(args: &[&str]) -> TranscribeArgs {
        let cli_argv: Vec<&OsStr> = ["koe", "transcribe"]
            .into_iter()
            .chain(args.iter().copied())
            .map(OsStr::new)
            .collect();
        match TestCli::try_parse_from(&cli_argv).expect("parse").command {
            TestCommand::Transcribe(transcribe) => transcribe,
        }
    }

    #[test]
    fn parses_required_input() {
        let args = parse_transcribe(&["meeting.wav"]);
        assert_eq!(args.input, PathBuf::from("meeting.wav"));
        assert!(args.locale.is_none());
        assert!(args.format.is_none());
        assert!(args.output.is_none());
    }

    #[test]
    fn parses_all_flags() {
        let args = parse_transcribe(&[
            "--locale",
            "ja-JP",
            "--format",
            "srt",
            "--engine",
            "on-device",
            "-o",
            "out.srt",
            "--start-at",
            "30s",
            "--end-at",
            "2m",
            "in.flac",
        ]);
        assert_eq!(args.input, PathBuf::from("in.flac"));
        assert_eq!(args.locale.as_deref(), Some("ja-JP"));
        assert_eq!(args.format.as_deref(), Some("srt"));
        assert_eq!(args.engine.as_deref(), Some("on-device"));
        assert_eq!(args.output.as_deref(), Some(Path::new("out.srt")));
        assert_eq!(args.start_at.as_deref(), Some("30s"));
        assert_eq!(args.end_at.as_deref(), Some("2m"));
    }

    #[test]
    fn default_output_replaces_extension() {
        let path = default_transcript_path(Path::new("/tmp/rec.ogg"), TranscriptFormat::Vtt);
        assert_eq!(path, PathBuf::from("/tmp/rec.vtt"));
    }

    #[test]
    fn prepare_rejects_missing_input() {
        let args = parse_transcribe(&["/tmp/koe-definitely-missing-input-xyz.wav"]);
        let err = prepare(&args, &KoeConfig::default()).expect_err("missing");
        assert!(matches!(err, MainError::Io(_)));
    }

    #[test]
    fn prepare_rejects_inverted_window() {
        let mut path = std::env::temp_dir();
        path.push(format!("koe-transcribe-prepare-{}.wav", std::process::id()));
        std::fs::write(&path, b"RIFF").unwrap();
        let args = parse_transcribe(&[
            "--start-at",
            "2m",
            "--end-at",
            "30s",
            path.to_str().unwrap(),
        ]);
        let err = prepare(&args, &KoeConfig::default()).expect_err("inverted");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, MainError::InvalidArgs(_)));
    }

    #[test]
    fn prepare_uses_transcription_section() {
        let mut path = std::env::temp_dir();
        path.push(format!("koe-transcribe-config-{}.wav", std::process::id()));
        std::fs::write(&path, b"RIFF").unwrap();
        let args = parse_transcribe(&[path.to_str().unwrap()]);
        let config = crate::config::parse_toml(
            r#"
[transcription]
locale = "ja-JP"
transcript-format = "srt"
engine = "network"
"#,
        )
        .expect("config");
        let prepared = prepare(&args, &config).expect("prepare");
        let _ = std::fs::remove_file(&path);
        assert_eq!(prepared.locale, "ja-JP");
        assert_eq!(prepared.speech_engine, koe_core::SpeechEngine::Network);
        assert_eq!(prepared.format, TranscriptFormat::Srt);
    }

    #[test]
    fn collector_offsets_final_segments() {
        let collector = SegmentCollector::new(30_000);
        collector.on_segment(TranscriptionSegment {
            text: "hello".into(),
            start_ms: 100,
            end_ms: 500,
            is_final: true,
            confidence: 0.9,
        });
        let (len, start, end) = {
            let segs = collector.segments.lock().unwrap();
            (segs.len(), segs[0].start_ms, segs[0].end_ms)
        };
        assert_eq!(len, 1);
        assert_eq!(start, 30_100);
        assert_eq!(end, 30_500);
    }
}
