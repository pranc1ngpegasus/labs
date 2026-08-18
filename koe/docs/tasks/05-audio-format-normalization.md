---
title: 05 — Audio Format Normalization
status: draft
depends: [02-koe-native-package]
spec_refs: [02-core-audio-process-tap, 10-recording-pipeline]
---

# 05 — Audio Format Normalization

Resample and convert captured audio to the canonical format using `AudioConverter`.

## Location

`koe-native/Sources/AudioUtils/FormatNormalizer.swift`

## Canonical Format

| Property | Canonical Value |
|----------|-----------------|
| Sample rate | 48,000 Hz |
| Channels | 2 (stereo), interleaved |
| Sample format | Float32 |
| Buffer size | Power-of-two multiple of 20 ms (~960 frames at 48 kHz) |

## Implementation

1. **`FormatNormalizer` class**
   ```swift
   public final class FormatNormalizer {
       public init(sourceDesc: AudioStreamBasicDescription) throws
       public func convert(
           input: UnsafePointer<Float>,
           frameCount: Int,
           output: UnsafeMutablePointer<Float>,
           maxOutputFrames: Int
       ) -> Int  // Returns actual frames written
   }
   ```

2. **Use `AudioConverterRef`**
   - Create with `AudioConverterNew(sourceASBD, targetASBD, &converter)`
   - Configure target format: 48 kHz, 2ch interleaved, Float32
   - Handle complex input formats (different sample rates, channel counts, bit depths)

3. **Efficiency**
   - Reuse `AudioConverterRef` for the lifetime of a capture session
   - Pre-compute `AudioConverterGetProperty(kAudioConverterPropertyMaximumOutputPacketSize)` for buffer sizing

## Note

SCK can be configured to deliver canonical format natively via `SCStreamConfiguration`
(sampleRate, channelCount). The normalizer exists primarily for Process Tap which
delivers audio in the source process's native format.

## Verification

- Test with common source formats: 44.1 kHz mono, 48 kHz stereo, 96 kHz stereo
- Verify output is always 48 kHz Float32 stereo interleaved
- Measure conversion latency
