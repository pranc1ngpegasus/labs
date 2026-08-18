---
title: 09 — Microphone Capture
status: draft
depends: [04-ring-buffer, 03-permission-checker]
spec_refs: [10-recording-pipeline]
---

# 09 — Microphone Capture

Implement microphone input capture via AudioQueue or HAL input.

## Location

`koe-native/Sources/AudioTap/MicrophoneCapture.swift` (or similar)

## API

```swift
public final class MicrophoneCapture {
    public init() throws
    public func start(callback: @escaping (UnsafePointer<Float>, Int) -> Void) throws
    public func stop()
    public var currentLevel: Float { get }  // RMS level, 0.0–1.0
}
```

## Implementation Options

1. **AudioQueue** (higher-level, recommended for v1)
   - `AudioQueueNewInput()` with canonical format
   - Set `kAudioQueueProperty_EnableLevelMetering` for level metering
   - Callback delivers `AudioQueueBuffer`; copy to ring buffer

2. **HAL Input** (lower-level, alternative)
   - `AudioDeviceCreateIOProcID()` on default input device
   - Real-time callback, stricter constraints
   - Only needed if AudioQueue latency is insufficient

## Canonical Format

Same as Process Tap: 48 kHz, Float32, 2ch interleaved.
If source device is mono, upmix (copy to both channels).

## Level Metering

- Compute RMS from the most recent buffer
- Expose as `currentLevel: Float` (0.0–1.0, linear)
- Polled by Rust side for GUI level meters at ~60 Hz

## Verification

- Start mic capture, verify audio data arrives in ring buffer
- Verify level metering responds to input
- Test with built-in mic and external USB mic
- Verify stop/cleanup
