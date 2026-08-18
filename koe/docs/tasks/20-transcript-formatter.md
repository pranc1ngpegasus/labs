---
title: 20 — Transcript Formatter
status: draft
depends: [15-pipeline-core]
spec_refs: [11-data-formats, 04-speech-recognition]
---

# 20 — Transcript Formatter

Implement the `TranscriptFormatter` trait and all four output formats.

## Location

`koe-core/src/transcript/`

```
koe-core/src/transcript/
  mod.rs    — TranscriptFormatter trait + TranscriptFormat enum
  txt.rs    — Plain text formatter
  srt.rs    — SubRip formatter
  vtt.rs    — WebVTT formatter
  json.rs   — JSON formatter
```

## TranscriptFormatter Trait

```rust
pub trait TranscriptFormatter: Send {
    /// Write a finalized segment.
    fn write_segment(&mut self, segment: &TranscriptionSegment);

    /// Get in-progress output (for CLI preview / GUI live view).
    fn current_output(&self) -> String;

    /// Finalize and return complete output.
    fn finalize(self) -> String;
}
```

## Format Implementations

### TXT
- One line per finalized segment
- No timestamps, no speaker labels
- Partial segments excluded from file output

### SRT
- Segment index (1-based), timestamp range, text
- Timestamps in `HH:MM:SS,mmm` format, recording-relative
- Empty line between segments

### VTT
- `WEBVTT` header
- Same timestamp model as SRT but `.` separator for milliseconds (`HH:MM:SS.mmm`)
- Optional `<i>` styling cues (no partial segments in final output)

### JSON
```json
{
  "format": "koe-transcript",
  "version": 1,
  "locale": "en-US",
  "created_at": "ISO8601",
  "source": { ... },
  "segments": [
    { "index": 0, "start_ms": 1250, "end_ms": 4800, "text": "...", "confidence": 0.95 }
  ]
}
```

## File Output Naming

- Default: `{audio_output_path_stem}.{transcript_ext}`
- Explicit: `--transcript-output <PATH>`

## Verification

- Format test segments in each format, verify output against spec
- Verify SRT/VTT timestamp formatting
- Verify JSON is valid and contains all fields
- Verify partial segments are excluded from TXT/SRT/VTT but present in `current_output()`
