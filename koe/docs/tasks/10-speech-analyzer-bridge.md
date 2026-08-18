---
title: 10 — Speech Analyzer Bridge
status: draft
depends: [02-koe-native-package]
spec_refs: [04-speech-recognition, 07-native-bridge]
---

# 10 — SFSpeechAnalyzer Bridge

Wrap `SFSpeechAnalyzer` (macOS 14+) for on-device speech-to-text.

## Location

`koe-native/Sources/SpeechAnalyzer/SpeechAnalyzerBridge.swift`

## API

```swift
public final class SpeechAnalyzerBridge {
    public init(locale: Locale) throws
    public func feedAudio(pcm: UnsafePointer<Float>, frameCount: Int)
    public func finalize()
    public var onResult: ((TranscriptionSegment) -> Void)?
    public var onError: ((Error) -> Void)?
}

public struct TranscriptionSegment {
    public let text: String
    public let startMs: Int64       // Offset from stream start
    public let endMs: Int64
    public let isFinal: Bool        // false = partial, true = final
    public let confidence: Float    // 0.0–1.0
}
```

## Implementation

1. **Initialize `SFSpeechAnalyzer`**
   - Create with `SFSpeechAnalyzer(locale:)` targeting the provided locale
   - Configure `SFSpeechAnalyzer.analysisContext` if needed

2. **`feedAudio(pcm:frameCount:)`**
   - Convert `[Float]` to `AVAudioPCMBuffer` (48 kHz, Float32)
   - Call `SFSpeechAnalyzer.addAudio()` or equivalent streaming API

3. **Incremental results**
   - `SFSpeechAnalyzer` produces partial results (`isFinal: false`) as recognized
   - On utterance boundary, it produces final results
   - Forward both via `onResult` callback

4. **`finalize()`**
   - Signal end of utterance (silence timeout or user stop)
   - Force final result emission

## Supported Locales

- `SFSpeechAnalyzer.supportedLocales()` — expose via `static var supportedLocales: [Locale]`
- Typically ~60 locales matching iOS keyboard dictation support

## Limitations (v1)

- macOS 14+ only (no fallback to legacy `SFSpeechRecognizer`)
- No speaker diarization
- No custom vocabulary
- No word-level timestamps (utterance-level only)
- Language detection not automatic

## Verification

- Initialize with `en-US`, feed known audio, verify segment callback fires
- Test partial → final transition
- Test error callback (unsupported locale, corrupted audio)
- Test `finalize()` flushes final partial segments
