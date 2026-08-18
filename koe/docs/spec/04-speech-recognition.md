---
title: Speech Recognition
topic: speech-recognition
status: draft
date: 2026-08-10
depends: [01-architecture, 07-native-bridge]
---

# 04 — Speech Recognition

## Overview

Koe uses **`SFSpeechAnalyzer`** (macOS 14+) for on-device speech-to-text.
This API supersedes the older `SFSpeechRecognizer` and runs entirely
on-device — no network round-trips, no Apple servers.

## Why SFSpeechAnalyzer

| Property | SFSpeechAnalyzer | SFSpeechRecognizer (legacy) | whisper.cpp |
|----------|-----------------|---------------------------|-------------|
| On-device | Yes (required) | Optional (requires `requiresOnDeviceRecognition`) | Yes |
| Network | Never | Defaults to server; opt-in to on-device | Never |
| Accuracy | High (Apple models) | High (server), lower (on-device) | Model-dependent |
| Latency | Streaming (incremental results) | Streaming | Batch / chunked |
| Language support | Same as iOS keyboard dictation (~60) | Same | Model-dependent |
| Punctuation / formatting | Automatic | Automatic | Model-dependent |
| Speaker diarization | No (v1 limitation) | No | Possible |
| CPU/GPU | Apple Neural Engine | Apple Neural Engine | CPU/GPU (Metal) |
| macOS minimum | 14.0 | 10.15 | 10.15+ |
| Bundle size | None (OS built-in) | None (OS built-in) | 50 MB – 4 GB model |

**Decision:** `SFSpeechAnalyzer` for v1. Add whisper.cpp as an optional
alternative backend post-v1 for users who need diarization or models with
domain-specific vocabularies.

## API Surface (Swift → Rust FFI)

```swift
// koe-native/Sources/SpeechAnalyzer/SpeechAnalyzerBridge.swift

public final class SpeechAnalyzerBridge {
    /// Initialize the analyzer for a given locale.
    /// - Parameter locale: e.g. "en-US", "ja-JP"
    public init(locale: Locale) throws

    /// Feed an audio chunk to the analyzer.
    /// - Parameter pcm: Float32, 48 kHz, mono or stereo interleaved
    /// - Parameter frameCount: number of frames per channel
    public func feedAudio(pcm: UnsafePointer<Float>, frameCount: Int)

    /// Signal end-of-utterance (e.g., on silence detection or stream end).
    public func finalize()

    /// The analyzer's result callback.
    /// Set from Rust via function pointer.
    public var onResult: ((TranscriptionSegment) -> Void)?

    /// The analyzer's error callback.
    public var onError: ((Error) -> Void)?
}

public struct TranscriptionSegment {
    public let text: String
    public let startMs: Int64     // Offset from stream start in milliseconds
    public let endMs: Int64
    public let isFinal: Bool      // false = partial result, true = final
    public let confidence: Float  // 0.0–1.0
}
```

## Rust-Side Consumer (koe-ffi)

```rust
// koe-ffi/src/speech.rs (sketch)

pub struct SpeechAnalyzer {
    inner: ffi::SpeechAnalyzerBridge, // uniffi-generated handle
    segment_tx: tokio::sync::mpsc::UnboundedSender<TranscriptionSegment>,
}

impl SpeechAnalyzer {
    pub fn new(locale: &str) -> Result<Self, SpeechError> { /* ... */ }

    pub fn feed(&self, pcm: &[f32], frame_count: usize) -> Result<(), SpeechError> {
        self.inner.feed_audio(pcm.as_ptr(), frame_count as i32);
        Ok(())
    }

    pub fn finalize(&self) { self.inner.finalize(); }

    /// Returns a stream of transcription segments.
    pub fn segments(&self) -> tokio::sync::mpsc::UnboundedReceiver<TranscriptionSegment> {
        self.segment_tx.subscribe()
    }
}
```

## Audio Preprocessing Before Analyzer

`SFSpeechAnalyzer` accepts 48 kHz mono or stereo Float32 PCM. Koe's pipeline
feeds it directly from the AEC output (see
[05-echo-cancellation](./05-echo-cancellation.md)):

```
Ring Buffer → AEC → [clean stereo f32] → SpeechAnalyzerBridge.feedAudio()
```

If the source is **mono** (e.g., a single-mic input), it is passed as mono —
the analyzer handles it natively. If the source is **multi-channel beyond
stereo**, only channels 1–2 (front L/R) are extracted.

No additional VAD (voice activity detection) is applied — `SFSpeechAnalyzer`
performs its own endpoint detection internally. We use `finalize()` to signal
end of stream or a user-initiated stop.

## Incremental Results

`SFSpeechAnalyzer` produces **partial results** (`isFinal: false`) as speech is
recognized, and **final results** when it detects an utterance boundary. The
pipeline handles this as follows:

```
SpeechAnalyzerBridge.onResult
  → ffi callback (C function pointer)
    → tokio channel send (non-blocking)
      → TranscriptFormatter (on tokio task)
        → for partial: buffer in memory, emit as "[partial] ..."
        → for final:   commit to output stream, write to file
```

### CLI Output Behavior

Lines are prefixed with the capture source — `[SYS]` for system/app audio,
`[MIC]` for the microphone, and `[SYS+MIC]` for the mixed (AEC) stream. The
analyzer's `isFinal` value controls whether the TTY line is updated in place
(partial) or committed as a new line (final); it is not printed as a status
label.

```
$ koe record --source system --app-id com.google.Chrome
Recording | ⣾ 00:00:05 | FLAC 48kHz stereo | App: Google Chrome (PID 4201)
[SYS] [00:00:05] "This is what I heard so far..."   # partial, overwrites in-place
[SYS] [00:00:08] "This is the final text."            # final, new line
[SYS] [00:00:08] "And now a new utterance..."       # next partial
```

### GUI Output Behavior

Partial results render in a **gray, italicized** font in the live transcript
view. On finalization, the text snaps to the standard font. Each finalized
segment is timestamped and scrolls into view.

## Language Configuration

```bash
# CLI
koe record --locale ja-JP --source system --app-id com.google.Chrome
koe record --locale en-US --source mic

# GUI: picker in Preferences / per-session dropdown
```

Supported locales are those available for on-device dictation:
`SFSpeechAnalyzer.supportedLocales()`. The GUI exposes this as a searchable
dropdown.

## Limitations (v1)

1. **macOS 14+ only.** No fallback to `SFSpeechRecognizer` in v1.
2. **No speaker diarization.** The analyzer does not identify speakers.
   Multi-speaker transcripts will be a continuous text block.
3. **No custom vocabulary.** Cannot inject domain-specific terms or
   pronunciations.
4. **No word-level timestamps.** Segments are utterance-level only. Word-level
   timing requires a different backend (e.g., whisper.cpp).
5. **Language detection is not automatic.** The user must specify the locale.

## Future: whisper.cpp Backend

Post-v1, `koe-core` should define a `SpeechBackend` trait:

```rust
pub trait SpeechBackend: Send {
    fn feed(&mut self, pcm: &[f32]) -> Result<(), SpeechError>;
    fn flush(&mut self) -> Result<Vec<TranscriptionSegment>, SpeechError>;
    fn set_locale(&mut self, locale: &str) -> Result<(), SpeechError>;
}
```

With implementations for `SFSpeechAnalyzer` (via FFI) and `WhisperCpp`
(native Rust binding or `whisper-rs`). The pipeline selects the backend at
startup based on configuration.
