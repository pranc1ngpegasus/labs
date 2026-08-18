---
title: 08 — ScreenCaptureKit Audio Capture
status: draft
depends: [04-ring-buffer, 06-process-enumeration]
spec_refs: [03-screen-capture-kit]
---

# 08 — ScreenCaptureKit Audio Capture

Implement per-app audio capture via `SCStream` (ScreenCaptureKit).

## Location

`koe-native/Sources/ScreenAudio/ScreenAudioCapture.swift`

## API

```swift
public final class ScreenAudioCapture: NSObject, SCStreamOutput {
    public static func enumerateContent() async throws -> SCShareableContent

    public func start(
        target: SCRunningApplication,
        config: SCStreamConfiguration,
        callback: @escaping (AudioBuffer) -> Void
    ) async throws

    public func stop() async
}
```

## Stream Configuration

```swift
let config = SCStreamConfiguration()
config.width = 1                     // Minimal video (required by SCK)
config.height = 1
config.pixelFormat = .bgra8888
config.minimumFrameInterval = CMTime(value: 1, timescale: 1)  // 1 fps
config.queueDepth = 3
config.capturesAudio = true          // Audio is what we want
config.excludesCurrentProcessAudio = true
config.channelCount = 2
config.sampleRate = 48_000
```

## Callback Behavior

- `SCStreamOutput.stream(_:didOutputSampleBuffer:of:)` fires on a serial dispatch queue (NOT real-time audio thread)
- More flexibility than Process Tap callback, but still minimize work
- Discard video frames immediately (`guard type == .audio`)
- Extract `CMSampleBuffer.audioBufferList`, copy PCM to ring buffer
- Ring buffer write, return quickly

## Error Handling

| Error | Recovery |
|-------|----------|
| `SCStreamError.userDeclined` | Show Settings link, exit gracefully |
| `SCStreamError.noAudioSamples` | Pause, notify UI; resume when samples return |
| `SCStreamError.applicationUnavailable` | End capture for target; notify user |

## Content Picker

For macOS 14+: use `SCContentSharingPicker` (system-standard UI).
For macOS 12–13: implement custom NSWindow with NSOutlineView.

## Verification

- Start capture on Chrome playing audio
- Verify audio samples arrive in ring buffer
- Verify video frames (~1 fps) are discarded
- Stop capture, verify clean teardown
- Test target app quit during capture → verify error handling
