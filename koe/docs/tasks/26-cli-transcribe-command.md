---
title: 26 — CLI `koe transcribe` Command
status: draft
depends: [10-speech-analyzer-bridge, 20-transcript-formatter]
spec_refs: [08-cli-interface]
---

# 26 — CLI `koe transcribe` Command

Transcribe an existing audio file without recording.

## Location

`koe-cli/src/commands/transcribe.rs`

## Command Definition

```
koe transcribe [OPTIONS] <INPUT_FILE>

Options:
  --locale <LOCALE>           Speech recognition locale (default: en-US)
  --output, -o <PATH>         Output transcript path (default: <input>.<format>)
  --format <FORMAT>           Transcript format: txt, srt, vtt, json (default: txt)
  --start-at <TIMESTAMP>      Start transcribing from offset (e.g., 1m30s)
  --end-at <TIMESTAMP>        Stop transcribing at offset
```

## Supported Input Formats

WAV (PCM, Float32), FLAC, OGG, MP3, AAC, AIFF.

## Implementation

1. **Open and decode input file**
   - Use `Symphonia` or similar Rust audio decoding crate
   - Resample/decode to canonical format: 48 kHz, Float32, mono/stereo
   - Handle `--start-at` / `--end-at` — seek and limit

2. **Feed to SFSpeechAnalyzer**
   - Initialize `SpeechAnalyzerBridge` with locale
   - Feed decoded audio in chunks
   - Collect `TranscriptionSegment` results

3. **Format and write output**
   - Use `TranscriptFormatter` for specified output format
   - Write to `--output` path (or default)

4. **Progress reporting**
   - Show elapsed time / total duration
   - Show partial segments as they appear

## Verification

- `koe transcribe test.wav` → outputs test.txt
- `koe transcribe --format srt --locale ja-JP meeting.ogg`
- `koe transcribe --start-at 30s --end-at 2m recording.flac`
- Test with unsupported format → graceful error
