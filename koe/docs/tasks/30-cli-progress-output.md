---
title: 30 — CLI Progress Output
status: draft
depends: [24-cli-record-command]
spec_refs: [08-cli-interface, 04-speech-recognition]
---

# 30 — CLI Progress Rendering

Implement live-updating progress output for `koe record`.

## Location

`koe-cli/src/progress.rs`

## TTY Mode

When stderr is a TTY, render a live-updating status block:

```
Recording | ⣾ 00:02:34 | OGG 48kHz stereo | App: Google Chrome (PID 4201)
[SYS] [00:02:30] "This is what I heard..."
```

- Status line: spinner + elapsed time + format + source info
- Transcript lines carry the capture source tag: `[SYS]` (system/app audio), `[MIC]` (microphone), `[SYS+MIC]` (mixed both)
- Partial segments overwrite in-place (no newline)
- Final segments: new line each

## Non-TTY Mode

When stderr is not a TTY (piped, redirected), emit newline-delimited JSON:

```json
{"type":"status","elapsed_ms":154000,"bytes_written":1843200}
{"type":"segment","start_ms":150000,"end_ms":152400,"text":"This is what I heard","is_final":false}
```

## Implementation

```rust
pub trait ProgressRenderer {
    fn render_status(&mut self, status: &RecordingStatus);
    fn render_segment(&mut self, segment: &TranscriptionSegment);
    fn finish(&mut self, summary: &RecordingSummary);
}

pub struct TtyRenderer { /* ANSI escape codes, spinner frames */ }
pub struct JsonRenderer { /* stdout lines */ }

pub fn create_renderer() -> Box<dyn ProgressRenderer> {
    if std::io::stderr().is_terminal() {
        Box::new(TtyRenderer::new())
    } else {
        Box::new(JsonRenderer::new())
    }
}
```

## Spinner

Use a set of Braille spinner frames: `⣾⣽⣻⢿⡿⣟⣯⣷`
Rotate each status update (~10 Hz).

## Verification

- Run recording in terminal → TTY progress renders correctly
- Pipe stderr: `koe record ... 2>&1 | cat` → JSON lines output
- Verify partial segments appear and update in TTY mode
- Verify final segments appear on new lines
