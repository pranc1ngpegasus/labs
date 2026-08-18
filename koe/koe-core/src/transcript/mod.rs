//! Transcript formatters (TXT / SRT / VTT / JSON).

mod cues;
mod json;
mod txt;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use koe_ffi::{AudioSourceConfig, TranscriptFormat, TranscriptionSegment};

use cues::CueFormatter;
use json::JsonFormatter;
use txt::TxtFormatter;

/// Session metadata required by the JSON transcript schema.
///
/// TXT / SRT / VTT ignore this; JSON embeds `locale`, `created_at`, and `source`.
#[derive(Debug, Clone)]
pub struct TranscriptMeta {
    /// BCP-47 locale used for recognition (e.g. `en-US`).
    pub locale: String,
    /// ISO-8601 timestamp for the `created_at` field.
    pub created_at: String,
    /// Capture source written into the JSON `source` object.
    pub source: AudioSourceConfig,
}

impl TranscriptMeta {
    /// Builds metadata for a live recording session.
    #[must_use]
    pub fn for_session(
        source: &AudioSourceConfig,
        locale: &str,
    ) -> Self {
        Self {
            locale: locale.to_owned(),
            created_at: utc_now_iso8601(),
            source: source.clone(),
        }
    }
}

/// Formats transcription segments for file output and live preview.
///
/// [`Self::write_segment`] accepts both partial and final segments.
/// [`Self::current_output`] includes the latest partial (TXT/SRT/VTT) for live
/// preview. [`Self::committed_output`] / [`Self::finalize`] exclude partials so
/// only finalized segments reach disk.
pub trait TranscriptFormatter: Send {
    /// Records a segment (partial or final).
    fn write_segment(
        &mut self,
        segment: &TranscriptionSegment,
    );

    /// Returns the in-progress transcript (for live preview).
    fn current_output(&self) -> String;

    /// Returns finalized segments only (safe for file output).
    fn committed_output(&self) -> String;

    /// Consuming finalize; defaults to [`Self::committed_output`].
    fn finalize(self) -> String
    where
        Self: Sized,
    {
        self.committed_output()
    }
}

/// File extension for a transcript format (without the leading dot).
#[must_use]
pub const fn transcript_extension(format: TranscriptFormat) -> &'static str {
    match format {
        TranscriptFormat::Txt => "txt",
        TranscriptFormat::Srt => "srt",
        TranscriptFormat::Vtt => "vtt",
        TranscriptFormat::Json => "json",
    }
}

/// Default transcript path beside an audio file: `{stem}.{transcript_ext}`.
#[must_use]
pub fn default_transcript_path(
    audio_output: &Path,
    format: TranscriptFormat,
) -> PathBuf {
    audio_output.with_extension(transcript_extension(format))
}

/// Creates a formatter for the requested transcript format.
///
/// `meta` is required for JSON session fields; TXT / SRT / VTT ignore it.
#[must_use]
pub fn create_formatter(
    format: TranscriptFormat,
    meta: &TranscriptMeta,
) -> Box<dyn TranscriptFormatter> {
    match format {
        TranscriptFormat::Txt => Box::new(TxtFormatter::new()),
        TranscriptFormat::Srt => Box::new(CueFormatter::srt()),
        TranscriptFormat::Vtt => Box::new(CueFormatter::vtt()),
        TranscriptFormat::Json => Box::new(JsonFormatter::new(meta.clone())),
    }
}

/// Shared final/partial segment storage used by every formatter.
#[derive(Debug, Default)]
struct SegmentBuffer {
    finals: Vec<TranscriptionSegment>,
    partial: Option<TranscriptionSegment>,
}

impl SegmentBuffer {
    const fn new() -> Self {
        Self {
            finals: Vec::new(),
            partial: None,
        }
    }

    fn write(
        &mut self,
        segment: &TranscriptionSegment,
    ) {
        if segment.is_final {
            self.partial = None;
            if !segment.text.is_empty() {
                self.finals.push(segment.clone());
            }
            return;
        }
        if segment.text.is_empty() {
            self.partial = None;
        } else {
            self.partial = Some(segment.clone());
        }
    }
}

/// Formats a recording-relative timestamp as `HH:MM:SS{sep}mmm`.
fn format_timestamp(
    ms: i64,
    decimal_sep: char,
) -> String {
    let ms = u64::try_from(ms.max(0)).unwrap_or(0);
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1_000;
    let millis = ms % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}{decimal_sep}{millis:03}")
}

fn utc_now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i32::try_from(secs / 86_400).unwrap_or(i32::MAX);
    let tod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
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
    use super::*;

    fn final_seg(
        text: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> TranscriptionSegment {
        TranscriptionSegment {
            text: text.to_owned(),
            start_ms,
            end_ms,
            is_final: true,
            confidence: 0.95,
        }
    }

    fn partial_seg(
        text: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> TranscriptionSegment {
        TranscriptionSegment {
            text: text.to_owned(),
            start_ms,
            end_ms,
            is_final: false,
            confidence: 0.5,
        }
    }

    #[test]
    fn transcript_extension_matches_format() {
        assert_eq!(transcript_extension(TranscriptFormat::Txt), "txt");
        assert_eq!(transcript_extension(TranscriptFormat::Srt), "srt");
        assert_eq!(transcript_extension(TranscriptFormat::Vtt), "vtt");
        assert_eq!(transcript_extension(TranscriptFormat::Json), "json");
    }

    #[test]
    fn default_transcript_path_replaces_extension() {
        let path = default_transcript_path(Path::new("/tmp/rec.ogg"), TranscriptFormat::Srt);
        assert_eq!(path, PathBuf::from("/tmp/rec.srt"));
    }

    #[test]
    fn format_timestamp_srt_and_vtt_separators() {
        assert_eq!(format_timestamp(1_250, ','), "00:00:01,250");
        assert_eq!(format_timestamp(1_250, '.'), "00:00:01.250");
        assert_eq!(format_timestamp(3_725_456, ','), "01:02:05,456");
        assert_eq!(format_timestamp(-10, '.'), "00:00:00.000");
    }

    #[test]
    fn create_formatter_dispatches_all_formats() {
        let meta = TranscriptMeta {
            locale: "en-US".into(),
            created_at: "2026-08-10T15:30:00Z".into(),
            source: AudioSourceConfig::Microphone,
        };
        for format in [
            TranscriptFormat::Txt,
            TranscriptFormat::Srt,
            TranscriptFormat::Vtt,
            TranscriptFormat::Json,
        ] {
            let mut fmt = create_formatter(format, &meta);
            fmt.write_segment(&final_seg("hi", 0, 500));
            assert!(fmt.committed_output().contains("hi"));
        }
    }

    #[test]
    fn segment_buffer_partial_cleared_by_final() {
        let mut buf = SegmentBuffer::new();
        buf.write(&partial_seg("draft", 0, 100));
        assert!(buf.partial.is_some());
        buf.write(&final_seg("done", 0, 200));
        assert_eq!(buf.finals.len(), 1);
        assert_eq!(buf.finals[0].text, "done");
        assert!(buf.partial.is_none());
    }

    #[test]
    fn segment_buffer_empty_final_clears_partial() {
        let mut buf = SegmentBuffer::new();
        buf.write(&partial_seg("draft", 0, 100));
        buf.write(&final_seg("", 0, 100));
        assert!(buf.partial.is_none());
        assert!(buf.finals.is_empty());
    }
}
