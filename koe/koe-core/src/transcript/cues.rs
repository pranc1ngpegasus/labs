//! Timed cue formatters (SRT / `WebVTT`).

use std::fmt::Write as _;

use koe_ffi::TranscriptionSegment;

use super::{SegmentBuffer, TranscriptFormatter, format_timestamp};

/// Shared SRT / `WebVTT` renderer. Differs only by timestamp separator and header.
pub(super) struct CueFormatter {
    buffer: SegmentBuffer,
    decimal_sep: char,
    header: &'static str,
}

impl CueFormatter {
    pub(super) const fn srt() -> Self {
        Self {
            buffer: SegmentBuffer::new(),
            decimal_sep: ',',
            header: "",
        }
    }

    pub(super) const fn vtt() -> Self {
        Self {
            buffer: SegmentBuffer::new(),
            decimal_sep: '.',
            header: "WEBVTT\n",
        }
    }

    fn render<'a>(
        &self,
        segments: impl IntoIterator<Item = &'a TranscriptionSegment>,
    ) -> String {
        let mut out = String::from(self.header);
        for (index, segment) in segments.into_iter().enumerate() {
            if !self.header.is_empty() || index > 0 {
                out.push('\n');
            }
            let _ = writeln!(out, "{}", index + 1);
            let _ = writeln!(
                out,
                "{} --> {}",
                format_timestamp(segment.start_ms, self.decimal_sep),
                format_timestamp(segment.end_ms, self.decimal_sep)
            );
            let _ = writeln!(out, "{}", segment.text);
        }
        out
    }
}

impl TranscriptFormatter for CueFormatter {
    fn write_segment(
        &mut self,
        segment: &TranscriptionSegment,
    ) {
        self.buffer.write(segment);
    }

    fn current_output(&self) -> String {
        self.render(
            self.buffer
                .finals
                .iter()
                .chain(self.buffer.partial.as_ref()),
        )
    }

    fn committed_output(&self) -> String {
        self.render(&self.buffer.finals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(
        text: &str,
        start_ms: i64,
        end_ms: i64,
        is_final: bool,
    ) -> TranscriptionSegment {
        TranscriptionSegment {
            text: text.to_owned(),
            start_ms,
            end_ms,
            is_final,
            confidence: 0.95,
        }
    }

    #[test]
    fn srt_matches_spec_example() {
        let mut fmt = CueFormatter::srt();
        fmt.write_segment(&seg(
            "This is what was spoken in the first utterance.",
            1_250,
            4_800,
            true,
        ));
        fmt.write_segment(&seg(
            "This is the second utterance, which is longer.",
            5_100,
            9_200,
            true,
        ));
        let expected = "\
1
00:00:01,250 --> 00:00:04,800
This is what was spoken in the first utterance.

2
00:00:05,100 --> 00:00:09,200
This is the second utterance, which is longer.
";
        assert_eq!(fmt.finalize(), expected);
    }

    #[test]
    fn srt_partial_in_preview_only() {
        let mut fmt = CueFormatter::srt();
        fmt.write_segment(&seg("done", 0, 1_000, true));
        fmt.write_segment(&seg("draft", 1_000, 1_500, false));
        let preview = fmt.current_output();
        assert!(preview.contains("draft"));
        assert!(preview.contains("2\n"));
        assert!(!fmt.committed_output().contains("draft"));
        assert!(!fmt.finalize().contains("draft"));
    }

    #[test]
    fn vtt_matches_spec_example() {
        let mut fmt = CueFormatter::vtt();
        fmt.write_segment(&seg(
            "This is what was spoken in the first utterance.",
            1_250,
            4_800,
            true,
        ));
        fmt.write_segment(&seg(
            "This is the second utterance, which is longer.",
            5_100,
            9_200,
            true,
        ));
        let expected = "\
WEBVTT

1
00:00:01.250 --> 00:00:04.800
This is what was spoken in the first utterance.

2
00:00:05.100 --> 00:00:09.200
This is the second utterance, which is longer.
";
        assert_eq!(fmt.finalize(), expected);
    }

    #[test]
    fn vtt_empty_still_has_header() {
        let fmt = CueFormatter::vtt();
        assert_eq!(fmt.finalize(), "WEBVTT\n");
    }

    #[test]
    fn vtt_partial_in_preview_only() {
        let mut fmt = CueFormatter::vtt();
        fmt.write_segment(&seg("done", 0, 500, true));
        fmt.write_segment(&seg("draft", 500, 800, false));
        assert!(fmt.current_output().contains("draft"));
        assert!(!fmt.committed_output().contains("draft"));
        assert!(!fmt.finalize().contains("draft"));
    }
}
